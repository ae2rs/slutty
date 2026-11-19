//! The server's threads. The calling thread owns the session, since
//! terminals cannot leave the thread that made them, and waits on one
//! channel. An acceptor thread, and a reader and a writer thread per client,
//! feed it. All run in one scope, so none outlives [`run`].

use std::collections::BTreeMap;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::net::Shutdown;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, Scope};
use std::time::{Duration, Instant};

use super::session::Session;
use super::socket::Listener;
use crate::proto::{self, ClientMessage, Event, Role, ServerMessage};

/// How long a new client has to say who it is.
const HANDSHAKE: Duration = Duration::from_secs(5);

enum Message {
    /// A pane has output.
    Wake,
    Connected {
        client: u64,
        role: Role,
        out: Sender<ServerMessage>,
    },
    Client {
        client: u64,
        message: ClientMessage,
    },
    Gone {
        client: u64,
    },
    /// SIGTERM, SIGINT or SIGHUP.
    Stop,
}

/// Serves on `listener` until the last window closes, a client asks it to
/// stop, or, with `signals`, until SIGTERM, SIGINT or SIGHUP. `ready` runs once
/// clients can connect.
pub fn run(listener: Listener, config_path: Option<PathBuf>, signals: bool, ready: impl FnOnce()) -> io::Result<()> {
    let (tx, rx) = mpsc::channel();
    let stopping = AtomicBool::new(false);
    let signal = signals.then(catch_signals).transpose()?;
    thread::scope(|s| {
        let (listener, stopping) = (&listener, &stopping);
        let acceptor = tx.clone();
        thread::Builder::new()
            .name("accept".into())
            .spawn_scoped(s, move || accept(s, listener, stopping, acceptor))?;
        if let Some((read, _)) = &signal {
            let tx = tx.clone();
            thread::Builder::new().name("signals".into()).spawn_scoped(s, move || {
                // A byte comes from the handler, or from below at the end.
                let _ = (&*read).read(&mut [0]);
                let _ = tx.send(Message::Stop);
            })?;
        }
        ready();
        serve(rx, tx, config_path, listener.path());
        stopping.store(true, Ordering::Release);
        listener.wake();
        if let Some((_, write)) = &signal {
            let _ = (&*write).write_all(&[0]);
        }
        Ok(())
    })
}

/// The mux thread: applies what clients send and tells the UI what changed.
/// Takes `rx` so that, once it returns, nothing more can queue there.
fn serve(rx: Receiver<Message>, tx: Sender<Message>, config_path: Option<PathBuf>, socket: &Path) {
    let woken = Arc::new(AtomicBool::new(false));
    let wake = {
        let woken = Arc::clone(&woken);
        move || {
            if !woken.swap(true, Ordering::AcqRel) {
                let _ = tx.send(Message::Wake);
            }
        }
    };
    let mut session = Session::new(wake, config_path, socket);
    let mut clients: BTreeMap<u64, Sender<ServerMessage>> = BTreeMap::new();
    let mut ui = None;
    let mut more = false;
    loop {
        let first = match (more, session.hold_deadline()) {
            (true, _) => rx.try_recv().ok(),
            (false, Some(deadline)) => rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).ok(),
            (false, None) => rx.recv().ok(),
        };
        let mut stop = false;
        for message in first.into_iter().chain(rx.try_iter()) {
            match message {
                Message::Wake => {}
                Message::Stop => stop = true,
                Message::Connected { client, role, out } => {
                    if role == Role::Ui {
                        // The newest UI wins, so a hung one never locks the user out.
                        if let Some(old) = ui.replace(client).and_then(|old| clients.remove(&old)) {
                            let _ = old.send(ServerMessage::Event(Event::Detached));
                        }
                        session.attach();
                    }
                    clients.insert(client, out);
                }
                Message::Client { client, message } => match message {
                    ClientMessage::Request { id, command } => {
                        let result = session.request(command);
                        if let Some(out) = clients.get(&client) {
                            let _ = out.send(ServerMessage::Response { id, result });
                        }
                    }
                    ClientMessage::Input(input) if ui == Some(client) => session.input(input),
                    ClientMessage::Input(_) | ClientMessage::Hello { .. } => {}
                },
                Message::Gone { client } => {
                    clients.remove(&client);
                    if ui == Some(client) {
                        ui = None;
                        session.detach();
                    }
                }
            }
        }
        woken.store(false, Ordering::Release);
        more = session.pump();
        if let Some(out) = ui.and_then(|client| clients.get(&client)) {
            for event in session.events() {
                let _ = out.send(ServerMessage::Event(event));
            }
        }
        if stop || session.is_over() {
            log::info!("stopping");
            return;
        }
    }
}

fn accept<'scope>(s: &'scope Scope<'scope, '_>, listener: &Listener, stopping: &AtomicBool, tx: Sender<Message>) {
    for client in 1.. {
        let stream = listener.accept();
        if stopping.load(Ordering::Acquire) {
            return;
        }
        let stream = match stream {
            Ok(stream) => stream,
            Err(err) => {
                // Out of descriptors, say: let some close.
                log::warn!("accepting a client: {err}");
                thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        let tx = tx.clone();
        let spawned = thread::Builder::new().name("client-reader".into()).spawn_scoped(s, move || {
            if let Err(err) = read_client(s, &stream, client, &tx) {
                log::debug!("client {client}: {err}");
            }
            let _ = tx.send(Message::Gone { client });
        });
        if let Err(err) = spawned {
            log::error!("starting a client thread: {err}");
        }
    }
}

/// Greets a client, then passes on what it sends until it hangs up.
fn read_client<'scope>(
    s: &'scope Scope<'scope, '_>,
    stream: &UnixStream,
    client: u64,
    tx: &Sender<Message>,
) -> io::Result<()> {
    stream.set_read_timeout(Some(HANDSHAKE))?;
    proto::write_header(&mut &*stream)?;
    let header = proto::read_header(&mut &*stream)?;
    if header.version != proto::VERSION {
        return Err(io::Error::other(format!("pid {} speaks version {}", header.pid, header.version)));
    }
    let mut reader = BufReader::new(stream);
    let Some(ClientMessage::Hello { role }) = proto::receive(&mut reader)? else {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "no hello"));
    };
    stream.set_read_timeout(None)?;
    let (out, outgoing) = mpsc::channel();
    tx.send(Message::Connected { client, role, out }).map_err(|_| io::Error::other("the server is stopping"))?;
    let writer = stream.try_clone()?;
    thread::Builder::new().name("client-writer".into()).spawn_scoped(s, move || write_client(&writer, &outgoing))?;
    while let Some(message) = proto::receive(&mut reader)? {
        if tx.send(Message::Client { client, message }).is_err() {
            break;
        }
    }
    Ok(())
}

/// Sends what the mux thread queues, until it lets go of the client.
fn write_client(stream: &UnixStream, outgoing: &Receiver<ServerMessage>) {
    let mut w = BufWriter::new(stream);
    for message in outgoing {
        if let Err(err) = proto::send(&mut w, &message) {
            log::debug!("writing to a client: {err}");
            break;
        }
    }
    // Ends the reader's wait on the same socket.
    let _ = stream.shutdown(Shutdown::Both);
}

/// Where the signal handler writes; -1 before [`catch_signals`].
static SIGNAL_FD: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_signal(_: libc::c_int) {
    let fd = SIGNAL_FD.load(Ordering::Relaxed);
    // SAFETY: write is async-signal-safe, and the buffer lives on the stack.
    unsafe { libc::write(fd, [0u8].as_ptr().cast(), 1) };
}

/// Turns SIGTERM, SIGINT and SIGHUP into a byte on the first socket.
/// Writing to the second wakes the reader too. They are caught, not
/// ignored: an ignored signal stays ignored in the programs panes run, and
/// closing a pane hangs its program up.
fn catch_signals() -> io::Result<(UnixStream, UnixStream)> {
    let (read, write) = UnixStream::pair()?;
    SIGNAL_FD.store(write.as_raw_fd(), Ordering::Relaxed);
    let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: the handler only makes an async-signal-safe call.
        unsafe { libc::signal(signal, handler) };
    }
    Ok((read, write))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::mux::PaneId;
    use crate::proto::{Command, PaneUpdate, Program, Reply};
    use crate::terminal::{Geometry, Grapheme};

    const SIZE: Geometry = Geometry { cols: 40, rows: 5, cell_width: 8, cell_height: 16 };

    struct Client {
        stream: UnixStream,
        reader: BufReader<UnixStream>,
    }

    impl Client {
        fn connect(socket: &Path, role: Role) -> Self {
            let stream = UnixStream::connect(socket).expect("connects");
            stream.set_read_timeout(Some(Duration::from_secs(10))).expect("timeout");
            proto::write_header(&mut &stream).expect("header sent");
            assert_eq!(proto::read_header(&mut &stream).expect("header read").version, proto::VERSION);
            proto::send(&mut &stream, &ClientMessage::Hello { role }).expect("hello");
            let reader = BufReader::new(stream.try_clone().expect("clone"));
            Self { stream, reader }
        }

        fn send(&self, message: &ClientMessage) {
            proto::send(&mut &self.stream, message).expect("sent");
        }

        fn next(&mut self) -> Option<ServerMessage> {
            proto::receive(&mut self.reader).expect("read")
        }

        fn request(&mut self, command: Command) -> Result<Reply, String> {
            self.send(&ClientMessage::Request { id: 9, command });
            loop {
                match self.next().expect("a response") {
                    ServerMessage::Response { id: 9, result } => return result,
                    _ => continue,
                }
            }
        }

        /// Reads until an update for `pane` shows `needle`.
        fn wait_for(&mut self, pane: PaneId, needle: &str) -> PaneUpdate {
            loop {
                if let Some(ServerMessage::Event(Event::Pane(update))) = self.next()
                    && update.pane == pane
                {
                    self.send(&ClientMessage::Input(proto::Input::Drawn { panes: vec![pane] }));
                    if text(&update).contains(needle) {
                        return update;
                    }
                }
            }
        }
    }

    fn text(update: &PaneUpdate) -> String {
        let cell = |c: &crate::terminal::Cell| match &c.text {
            Grapheme::Char(c) => c.to_string(),
            Grapheme::Cluster(s) => s.to_string(),
            Grapheme::Empty => " ".into(),
        };
        update.changed.iter().map(|(_, row)| row.cells.iter().map(cell).collect::<String>()).collect()
    }

    #[test]
    fn panes_outlive_their_ui_and_the_newest_ui_wins() {
        let dir = std::env::temp_dir().join(format!("slutty-run-{}", std::process::id()));
        let socket = dir.join("mux.sock");
        let listener = Listener::bind(&socket).expect("binds").expect("free");
        let (ready, started) = mpsc::channel();
        let server = thread::spawn(move || run(listener, None, false, move || ready.send(()).expect("told")));
        started.recv().expect("ready");

        let mut first = Client::connect(&socket, Role::Ui);
        let script = "printf marker; exec sleep 30";
        let program = Program { command: Some(("/bin/sh".into(), vec!["-c".into(), script.into()])), cwd: None };
        let pane = match first.request(Command::OpenWindow { program, size: SIZE }) {
            Ok(Reply::Spawned { pane, .. }) => pane,
            other => panic!("opening a window: {other:?}"),
        };
        first.wait_for(pane, "marker");
        // A crash looks the same: the socket closes mid-stream.
        drop(first);

        let mut second = Client::connect(&socket, Role::Ui);
        let update = second.wait_for(pane, "marker");
        assert_eq!(update.changed.len(), usize::from(SIZE.rows), "a full update on attach");

        let mut third = Client::connect(&socket, Role::Ui);
        third.wait_for(pane, "marker");
        let mut detached = false;
        while let Some(message) = second.next() {
            detached |= message == ServerMessage::Event(Event::Detached);
        }
        assert!(detached, "the older UI is told, then hung up on");

        let mut cli = Client::connect(&socket, Role::Cli);
        assert_eq!(cli.request(Command::ActivatePane { pane }), Ok(Reply::Done));
        assert_eq!(cli.request(Command::KillServer), Ok(Reply::Done));
        server.join().expect("no panic").expect("served");
        while third.next().is_some() {}
        assert!(!socket.exists(), "the socket is removed");
        std::fs::remove_dir_all(dir).expect("cleaned up");
    }
}
