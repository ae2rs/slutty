//! The mux server: owns every window, pane and terminal, and answers its
//! clients over a Unix socket.

mod process;
mod run;
mod session;
mod socket;

pub use process::main;
#[cfg(test)]
pub use run::run;
#[cfg(test)]
pub use socket::Listener;
