//! The protocol between the mux server and its clients: the UI and
//! `slutty cli`.

mod message;
mod transport;
pub(crate) mod wire;

pub use message::{
    Command, Event, Gesture, Hover, Input, KeyEvent, Mode, ModeOp, MouseEvent, PaneOverlay, PaneUpdate, Program, Reply,
    ServerMessage, TabState, WindowState,
};
pub use transport::{Header, VERSION, read_header, receive, send, socket_path, write_header};
