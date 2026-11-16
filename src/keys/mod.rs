//! Key bindings: chords, actions, the bindings from the config and the
//! matcher that runs them. No platform types.

mod action;
mod bindings;
mod chord;
mod matcher;

pub use action::Action;
pub use bindings::Keys;
pub use chord::{KeyName, KeyPress, Modifiers};
pub use matcher::{Matcher, Outcome};
