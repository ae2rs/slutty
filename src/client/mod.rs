//! The clients' side of the mux: reaching the server, the UI's line to it
//! and its copy of what the server shows. No platform types.

mod connection;
mod dial;
mod mirror;
mod mode_keys;

pub use connection::{Connection, log_failure};
pub use dial::{ConnectError, connect_or_start, stop_server};
pub use mirror::{Changes, Mirror};
pub use mode_keys::{ModeKey, mode_key};
