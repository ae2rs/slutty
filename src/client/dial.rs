//! Reaching the server: connecting to it, and starting one for the UI.

use std::fmt;
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::proto::{self, ClientMessage, Header, Role};

/// How long a server has to answer, and a new one to start listening.
const PATIENCE: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub enum ConnectError {
    /// Nothing listens on the socket.
    NoServer,
    /// The server speaks another version of the protocol.
    Mismatch(Header),
    Io(io::Error),
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoServer => write!(f, "no slutty server running"),
            Self::Mismatch(header) => write!(
                f,
                "the server (pid {}) speaks protocol {}, this slutty speaks {}",
                header.pid,
                header.version,
                proto::VERSION
            ),
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl From<io::Error> for ConnectError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

/// Connects to the server on `socket` as `role`.
pub fn connect(socket: &Path, role: Role) -> Result<UnixStream, ConnectError> {
    let stream = UnixStream::connect(socket).map_err(|err| match err.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => ConnectError::NoServer,
        _ => ConnectError::Io(err),
    })?;
    stream.set_read_timeout(Some(PATIENCE))?;
    proto::write_header(&mut &stream)?;
    let header = proto::read_header(&mut &stream)?;
    if header.version != proto::VERSION {
        return Err(ConnectError::Mismatch(header));
    }
    proto::send(&mut &stream, &ClientMessage::Hello { role })?;
    stream.set_read_timeout(None)?;
    Ok(stream)
}

/// Connects as the UI, starting a server first when none runs.
pub fn connect_or_start(socket: &Path) -> Result<UnixStream, ConnectError> {
    match connect(socket, Role::Ui) {
        Err(ConnectError::NoServer) => {}
        other => return other,
    }
    start_server(socket)?;
    // A server that lost a race to start says it is ready while the winner
    // may still be binding.
    let start = Instant::now();
    loop {
        match connect(socket, Role::Ui) {
            Err(ConnectError::NoServer) if start.elapsed() < PATIENCE => thread::sleep(Duration::from_millis(20)),
            other => return other,
        }
    }
}

/// Stops the server with `pid` and waits until nothing listens on `socket`.
pub fn stop_server(socket: &Path, pid: u32) -> io::Result<()> {
    let pid = libc::pid_t::try_from(pid).map_err(io::Error::other)?;
    // SAFETY: a plain signal. The pid came from the server on `socket` just
    // now, so it is that server's.
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let start = Instant::now();
    while start.elapsed() < PATIENCE {
        if let Err(ConnectError::NoServer) = connect(socket, Role::Cli) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Err(io::Error::new(io::ErrorKind::TimedOut, "the old server did not stop"))
}

/// Starts `slutty server --daemon` on `socket` and waits until it listens.
fn start_server(socket: &Path) -> io::Result<()> {
    let mut child = Command::new(std::env::current_exe()?)
        .args(["server", "--daemon", "--socket"])
        .arg(socket)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut stdout = child.stdout.take().ok_or_else(|| io::Error::other("no pipe to the server"))?;
    // The first process exits as soon as it has forked the server.
    child.wait()?;
    let mut poll = libc::pollfd { fd: stdout.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    let timeout = libc::c_int::try_from(PATIENCE.as_millis()).unwrap_or(libc::c_int::MAX);
    // SAFETY: one pollfd, for a descriptor `stdout` owns.
    let polled = unsafe { libc::poll(&raw mut poll, 1, timeout) };
    if polled < 0 {
        return Err(io::Error::last_os_error());
    }
    let started = polled > 0 && stdout.read(&mut [0])? == 1;
    if started {
        Ok(())
    } else {
        Err(io::Error::other("the server did not start; see ~/Library/Logs/slutty/server.log"))
    }
}
