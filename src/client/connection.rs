//! The UI's line to the mux server. Requests and input go out at once and
//! never wait; what comes back is read when the main thread is woken.

use std::sync::Arc;
use std::time::Instant;

use crate::proto::{Command, Input, Reply, ServerMessage};
use crate::server::Session;

pub struct Connection {
    session: Session,
    wake: Arc<dyn Fn() + Send + Sync>,
    next_id: u32,
    responses: Vec<ServerMessage>,
}

impl Connection {
    /// A server in this process. `wake` runs, on any thread, when there is
    /// something to [`Connection::receive`].
    pub fn local(config_path: Option<std::path::PathBuf>, wake: impl Fn() + Send + Sync + 'static) -> Self {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        let mut session = Session::new(
            {
                let wake = Arc::clone(&wake);
                move || wake()
            },
            config_path,
        );
        session.attach();
        Self { session, wake, next_id: 0, responses: Vec::new() }
    }

    /// Sends `command`. Returns the id its response comes back with.
    pub fn request(&mut self, command: Command) -> u32 {
        self.next_id = self.next_id.wrapping_add(1);
        let id = self.next_id;
        let result = self.session.request(command);
        self.responses.push(ServerMessage::Response { id, result });
        (self.wake)();
        id
    }

    pub fn input(&mut self, input: Input) {
        self.session.input(input);
        (self.wake)();
    }

    /// What the server said since the last call.
    pub fn receive(&mut self) -> Vec<ServerMessage> {
        if self.session.pump() {
            (self.wake)();
        }
        let mut messages = std::mem::take(&mut self.responses);
        messages.extend(self.session.events().into_iter().map(ServerMessage::Event));
        messages
    }

    /// When to call [`Connection::receive`] again with no wake, for a
    /// program that holds its screen too long.
    pub fn deadline(&self) -> Option<Instant> {
        self.session.hold_deadline()
    }
}

/// A response that only needs logging when it failed.
pub fn log_failure(result: &Result<Reply, String>) {
    if let Err(err) = result {
        log::error!("the server refused: {err}");
    }
}
