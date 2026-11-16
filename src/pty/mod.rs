//! Child processes on pseudo-terminals. Knows nothing about terminals or
//! windows.

mod process;
mod session;

pub use process::Process;
pub use session::{Pty, ReadStatus, Spawn, WindowSize};
