//! One libghostty terminal per pane, and the plain frame data drawn from it.

pub mod copy_mode;
mod emulator;
mod frame;
mod select;
mod user_vars;

pub use emulator::{Geometry, KeyInput, MouseInput, Terminal};
pub use frame::{Cell, CellStyle, Cursor, CursorShape, Frame, Grapheme, Row};
pub use select::{Highlight, Pointer, Release, SearchStatus};
