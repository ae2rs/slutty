//! The mux server: owns every window, pane and terminal, and answers its
//! clients.

mod session;

pub use session::Session;
