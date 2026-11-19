//! The UI's line to the mux server. Requests and input go out through a
//! writer thread, so a slow server never blocks AppKit; a reader thread
//! queues what comes back and wakes the main thread.

use std::io::{BufReader, BufWriter};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use std::{io, mem};

use crate::proto::{self, ClientMessage, Command, Input, Reply, ServerMessage};

/// How long a write may wait on a server that does not read.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Connection {
    stream: UnixStream,
    outgoing: Option<Sender<ClientMessage>>,
    incoming: Arc<Mutex<Incoming>>,
    reader: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
    next_id: u32,
}

#[derive(Default)]
struct Incoming {
    messages: Vec<ServerMessage>,
    /// Why the server is gone, once it is.
    gone: Option<String>,
}

impl Connection {
    /// Talks over `stream`, connected as the UI. `wake` runs on the reader
    /// thread when there is something to [`Connection::receive`].
    pub fn new(stream: UnixStream, wake: impl Fn() + Send + 'static) -> io::Result<Self> {
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        let incoming = Arc::new(Mutex::new(Incoming::default()));
        let reader = {
            let (stream, incoming) = (stream.try_clone()?, Arc::clone(&incoming));
            thread::Builder::new().name("server-reader".into()).spawn(move || read_loop(&stream, &incoming, wake))?
        };
        let (outgoing, rx) = mpsc::channel();
        let writer = {
            let stream = stream.try_clone()?;
            thread::Builder::new().name("server-writer".into()).spawn(move || write_loop(&stream, &rx))?
        };
        Ok(Self { stream, outgoing: Some(outgoing), incoming, reader: Some(reader), writer: Some(writer), next_id: 0 })
    }

    /// Sends `command`. Returns the id its response comes back with.
    pub fn request(&mut self, command: Command) -> u32 {
        self.next_id = self.next_id.wrapping_add(1);
        let id = self.next_id;
        self.send(ClientMessage::Request { id, command });
        id
    }

    pub fn input(&mut self, input: Input) {
        self.send(ClientMessage::Input(input));
    }

    /// What the server said since the last call.
    pub fn receive(&mut self) -> Vec<ServerMessage> {
        mem::take(&mut self.incoming.lock().unwrap_or_else(PoisonError::into_inner).messages)
    }

    /// Why the server is gone, once it is.
    pub fn gone(&self) -> Option<String> {
        self.incoming.lock().unwrap_or_else(PoisonError::into_inner).gone.clone()
    }

    /// Sends what is queued, then hangs up.
    pub fn close(&mut self) {
        self.outgoing = None;
        join(self.writer.take());
        let _ = self.stream.shutdown(Shutdown::Both);
        join(self.reader.take());
    }

    fn send(&self, message: ClientMessage) {
        if let Some(outgoing) = &self.outgoing {
            // A closed writer means the server is gone, which the reader reports.
            let _ = outgoing.send(message);
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.close();
    }
}

fn join(thread: Option<JoinHandle<()>>) {
    if let Some(Err(err)) = thread.map(JoinHandle::join) {
        log::error!("a connection thread panicked: {err:?}");
    }
}

fn read_loop(stream: &UnixStream, incoming: &Mutex<Incoming>, wake: impl Fn()) {
    let mut reader = BufReader::new(stream);
    let gone = loop {
        match proto::receive(&mut reader) {
            Ok(Some(message)) => incoming.lock().unwrap_or_else(PoisonError::into_inner).messages.push(message),
            Ok(None) => break "the server hung up".to_owned(),
            Err(err) => break format!("reading from the server failed: {err}"),
        }
        wake();
    };
    incoming.lock().unwrap_or_else(PoisonError::into_inner).gone = Some(gone);
    wake();
}

fn write_loop(stream: &UnixStream, outgoing: &Receiver<ClientMessage>) {
    let mut w = BufWriter::new(stream);
    for message in outgoing {
        if let Err(err) = proto::send(&mut w, &message) {
            log::error!("writing to the server failed: {err}");
            return;
        }
    }
}

/// A response that only needs logging when it failed.
pub fn log_failure(result: &Result<Reply, String>) {
    if let Err(err) = result {
        log::error!("the server refused: {err}");
    }
}
