//! The UI's side of the mux: its line to the server and its copy of what
//! the server shows. No platform types.

mod connection;
mod mirror;
mod mode_keys;

pub use connection::{Connection, log_failure};
pub use mirror::{Changes, Mirror};
pub use mode_keys::{ModeKey, mode_key};
