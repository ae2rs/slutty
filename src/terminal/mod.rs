//! One libghostty terminal per pane, and the plain frame data drawn from it.

mod emulator;
mod frame;

pub use emulator::{Geometry, KeyInput, MouseInput, Terminal};
pub use frame::{Cell, CellStyle, CursorShape, Frame, Grapheme};
