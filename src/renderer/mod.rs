//! Draws frames with Metal. Only damaged rows are rebuilt; nothing runs
//! between frames.

mod atlas;
mod metal;
mod scene;

pub use metal::{PIXEL_FORMAT, PaneDraw, Renderer, Tint};
pub use scene::{Line, Overlay, RectInstance};
