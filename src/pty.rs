//! A child process on a pseudo-terminal, with one thread blocked on reads and
//! one blocked on writes. Knows nothing about terminals or windows.

use std::ffi::{CStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Chunks the reader may queue before it blocks, which in turn blocks the
/// child: backpressure instead of unbounded memory.
const QUEUED_CHUNKS: usize = 64;
const CHUNK_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowSize {
    pub cols: u16,
    pub rows: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}

impl WindowSize {
    fn winsize(self) -> libc::winsize {
        libc::winsize {
            ws_row: self.rows,
            ws_col: self.cols,
            ws_xpixel: self.cols.saturating_mul(self.cell_width),
            ws_ypixel: self.rows.saturating_mul(self.cell_height),
        }
    }
}

/// What to run. `login` starts the program as a login shell (`argv[0]` is
/// `-name`).
#[derive(Clone, Debug)]
pub struct Spawn {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub login: bool,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(OsString, OsString)>,
}

impl Spawn {
    /// The user's login shell from the password database, in their home.
    pub fn login_shell() -> Self {
        let (shell, home) = passwd_entry();
        let program = shell
            .or_else(|| std::env::var_os("SHELL").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("/bin/zsh"));
        let cwd = home.or_else(|| std::env::var_os("HOME").map(PathBuf::from));
        Self { program, args: Vec::new(), login: true, cwd, env: Vec::new() }
    }
}

/// How a read from [`Pty::read`] went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadStatus {
    /// Everything queued was read.
    Drained,
    /// The budget ran out with output still queued.
    More,
    /// The child closed the terminal; nothing more will come.
    Closed,
}

pub struct Pty {
    master: OwnedFd,
    output: Receiver<Vec<u8>>,
    wake_pending: Arc<AtomicBool>,
    input: Option<Sender<Vec<u8>>>,
    shutdown: Option<OwnedFd>,
    reader: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
}

impl Pty {
    /// Starts `spawn` on a new PTY. `wake` runs on the reader thread when
    /// output arrives while no earlier wake is pending, and once when the
    /// child closes the terminal.
    pub fn spawn(
        spawn: &Spawn,
        size: WindowSize,
        wake: impl Fn() + Send + 'static,
    ) -> io::Result<Self> {
        let (master, slave) = open_pty(size)?;
        let child = spawn_child(spawn, &slave)?;
        drop(slave);

        let (shutdown_rx, shutdown_tx) = pipe()?;
        let wake_pending = Arc::new(AtomicBool::new(false));
        let (out_tx, output) = mpsc::sync_channel(QUEUED_CHUNKS);
        let reader = {
            let master = File::from(master.try_clone()?);
            let pending = Arc::clone(&wake_pending);
            thread::Builder::new()
                .name("pty-reader".into())
                .spawn(move || read_loop(master, shutdown_rx, child, out_tx, &pending, wake))?
        };
        let (in_tx, in_rx) = mpsc::channel();
        let writer = {
            let master = File::from(master.try_clone()?);
            thread::Builder::new()
                .name("pty-writer".into())
                .spawn(move || write_loop(master, &in_rx))?
        };

        Ok(Self {
            master,
            output,
            wake_pending,
            input: Some(in_tx),
            shutdown: Some(shutdown_tx),
            reader: Some(reader),
            writer: Some(writer),
        })
    }

    /// Queues `data` for the child's input.
    pub fn write(&self, data: &[u8]) {
        if let Some(input) = &self.input
            && !data.is_empty()
        {
            // A closed input means the child is gone; its input no longer matters.
            let _ = input.send(data.to_vec());
        }
    }

    /// Appends queued output to `out`, stopping once it has grown by about
    /// `budget` bytes.
    pub fn read(&self, out: &mut Vec<u8>, budget: usize) -> ReadStatus {
        // Clear before draining: output queued after this point wakes again.
        self.wake_pending.store(false, Ordering::Release);
        let start = out.len();
        while out.len() - start < budget {
            match self.output.try_recv() {
                Ok(chunk) => out.extend_from_slice(&chunk),
                Err(TryRecvError::Empty) => return ReadStatus::Drained,
                Err(TryRecvError::Disconnected) => return ReadStatus::Closed,
            }
        }
        ReadStatus::More
    }

    pub fn resize(&self, size: WindowSize) -> io::Result<()> {
        let winsize = size.winsize();
        // SAFETY: TIOCSWINSZ reads one `winsize` from a valid pointer.
        let rc = unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &raw const winsize) };
        if rc == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // The reader hangs up the child, waits for it and exits.
        drop(self.shutdown.take());
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        // The child is gone, so a write still in progress fails at once.
        drop(self.input.take());
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

fn read_loop(
    mut master: File,
    shutdown: OwnedFd,
    mut child: Child,
    output: SyncSender<Vec<u8>>,
    wake_pending: &AtomicBool,
    wake: impl Fn(),
) {
    let mut buf = vec![0u8; CHUNK_BYTES];
    let hung_up = loop {
        match wait_readable(&master, &shutdown) {
            Ok(Readable::Data) => {}
            Ok(Readable::Shutdown) => break true,
            Err(err) => {
                log::warn!("pty poll failed: {err}");
                break false;
            }
        }
        match master.read(&mut buf) {
            // EIO is how macOS reports that every slave descriptor is closed.
            Ok(0) => break false,
            Err(err) if err.raw_os_error() == Some(libc::EIO) => break false,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => {
                log::warn!("pty read failed: {err}");
                break false;
            }
            Ok(n) => {
                if output.send(buf[..n].to_vec()).is_err() {
                    break true;
                }
                if !wake_pending.swap(true, Ordering::AcqRel) {
                    wake();
                }
            }
        }
    };
    drop(output);
    if !hung_up {
        wake_pending.store(true, Ordering::Release);
        wake();
    }
    reap(&mut child, hung_up);
}

fn write_loop(mut master: File, input: &Receiver<Vec<u8>>) {
    while let Ok(data) = input.recv() {
        if let Err(err) = master.write_all(&data) {
            log::debug!("pty write failed: {err}");
            return;
        }
    }
}

/// Waits for the child to exit. When we hung up on it, it gets a moment to
/// leave on SIGHUP before SIGKILL.
fn reap(child: &mut Child, hang_up: bool) {
    if hang_up {
        let pid = child.id().cast_signed();
        // SAFETY: plain signal to our own child; a stale pid is not possible
        // because the child is not reaped yet.
        unsafe {
            libc::kill(pid, libc::SIGHUP);
        }
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = child.kill();
    }
    match child.wait() {
        Ok(status) => log::info!("child exited: {status}"),
        Err(err) => log::warn!("waiting for child failed: {err}"),
    }
}

enum Readable {
    Data,
    Shutdown,
}

fn wait_readable(master: &File, shutdown: &OwnedFd) -> io::Result<Readable> {
    let mut fds = [
        libc::pollfd { fd: master.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: shutdown.as_raw_fd(), events: libc::POLLIN, revents: 0 },
    ];
    loop {
        // SAFETY: `fds` is a valid array of two pollfd.
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if rc >= 0 {
            break;
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
    // The shutdown pipe only ever hangs up, when its write end is dropped.
    if fds[1].revents != 0 {
        Ok(Readable::Shutdown)
    } else {
        Ok(Readable::Data)
    }
}

fn open_pty(size: WindowSize) -> io::Result<(OwnedFd, OwnedFd)> {
    let mut master = -1;
    let mut slave = -1;
    let mut winsize = size.winsize();
    // SAFETY: out pointers are valid; name and termios may be null.
    let rc = unsafe {
        libc::openpty(&raw mut master, &raw mut slave, std::ptr::null_mut(), std::ptr::null_mut(), &raw mut winsize)
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openpty returned two fresh descriptors we now own.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    set_cloexec(&master)?;
    set_cloexec(&slave)?;
    Ok((master, slave))
}

fn spawn_child(spawn: &Spawn, slave: &OwnedFd) -> io::Result<Child> {
    let mut command = Command::new(&spawn.program);
    command.args(&spawn.args);
    if spawn.login {
        let name = spawn.program.file_name().unwrap_or(spawn.program.as_os_str());
        let mut arg0 = OsString::from("-");
        arg0.push(name);
        command.arg0(arg0);
    }
    if let Some(cwd) = &spawn.cwd {
        command.current_dir(cwd);
    }
    command.envs(spawn.env.iter().map(|(k, v)| (k, v)));
    command
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave.try_clone()?));
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            // stdin is the slave: make it the controlling terminal.
            if libc::ioctl(0, libc::TIOCSCTTY.into(), 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn()
}

fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: `fds` has room for two descriptors.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: pipe returned two fresh descriptors we now own.
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    set_cloexec(&read)?;
    set_cloexec(&write)?;
    Ok((read, write))
}

fn set_cloexec(fd: &OwnedFd) -> io::Result<()> {
    // SAFETY: plain fcntl on a descriptor we own.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The shell and home directory of the current user.
fn passwd_entry() -> (Option<PathBuf>, Option<PathBuf>) {
    let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut buf = vec![0 as libc::c_char; 4096];
    let mut result = std::ptr::null_mut();
    // SAFETY: all pointers are valid for the sizes given.
    let rc = unsafe {
        libc::getpwuid_r(libc::getuid(), entry.as_mut_ptr(), buf.as_mut_ptr(), buf.len(), &raw mut result)
    };
    if rc != 0 || result.is_null() {
        return (None, None);
    }
    // SAFETY: getpwuid_r filled `entry`, and its strings point into `buf`.
    let entry = unsafe { entry.assume_init() };
    let path = |ptr: *const libc::c_char| {
        // SAFETY: non-null passwd strings are NUL-terminated within `buf`.
        (!ptr.is_null())
            .then(|| unsafe { CStr::from_ptr(ptr) }.to_bytes().to_vec())
            .filter(|bytes| !bytes.is_empty())
            .map(|bytes| PathBuf::from(OsString::from_vec(bytes)))
    };
    (path(entry.pw_shell), path(entry.pw_dir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::RecvTimeoutError;

    fn run(program: &str, args: &[&str]) -> (Pty, Receiver<()>) {
        let spawn = Spawn {
            program: program.into(),
            args: args.iter().map(OsString::from).collect(),
            login: false,
            cwd: None,
            env: Vec::new(),
        };
        let size = WindowSize { cols: 80, rows: 24, cell_width: 8, cell_height: 16 };
        let (tx, rx) = mpsc::channel();
        let pty = Pty::spawn(&spawn, size, move || {
            let _ = tx.send(());
        })
        .expect("spawn");
        (pty, rx)
    }

    /// Reads until `done` holds for the output or the child closes.
    fn read_until(pty: &Pty, wakes: &Receiver<()>, done: impl Fn(&[u8]) -> bool) -> (Vec<u8>, ReadStatus) {
        let mut out = Vec::new();
        loop {
            let status = pty.read(&mut out, usize::MAX);
            if status == ReadStatus::Closed || done(&out) {
                return (out, status);
            }
            match wakes.recv_timeout(Duration::from_secs(5)) {
                Ok(()) => {}
                Err(RecvTimeoutError::Timeout) => panic!("no output: {}", String::from_utf8_lossy(&out)),
                Err(RecvTimeoutError::Disconnected) => return (out, pty.read(&mut Vec::new(), 0)),
            }
        }
    }

    #[test]
    fn echoes_input_and_reports_close() {
        let (pty, wakes) = run("/bin/cat", &[]);
        pty.write(b"hello\n");
        let (out, _) = read_until(&pty, &wakes, |out| out.windows(5).filter(|w| w == b"hello").count() >= 2);
        assert!(String::from_utf8_lossy(&out).contains("hello"));
        // ctrl+d ends cat, which closes the terminal.
        pty.write(b"\x04");
        let (_, status) = read_until(&pty, &wakes, |_| false);
        assert_eq!(status, ReadStatus::Closed);
    }

    #[test]
    fn child_sees_the_window_size() {
        let (pty, wakes) = run("/bin/sh", &["-c", "sleep 0.2; stty size"]);
        pty.resize(WindowSize { cols: 100, rows: 30, cell_width: 8, cell_height: 16 }).expect("resize");
        let (out, _) = read_until(&pty, &wakes, |out| out.ends_with(b"\r\n"));
        assert_eq!(String::from_utf8_lossy(&out).trim(), "30 100");
    }

    #[test]
    fn drop_hangs_up_a_running_child() {
        let (pty, _wakes) = run("/bin/sleep", &["30"]);
        let start = Instant::now();
        drop(pty);
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
