//! AppKit: the app, its windows and their terminal views.

mod actions;
mod app;
mod keys;
mod layout;
mod pointer;
mod quick_select;
mod state;
mod tab_bar;
mod view;
mod watch;

pub use app::run;
