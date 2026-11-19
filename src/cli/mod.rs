//! `slutty cli`: one command to the server, like `wezterm cli`. It never
//! starts a server.

mod args;
mod run;

pub use args::Cli;
pub use run::run;
