//! Draws frames with Metal. Only damaged rows are rebuilt; nothing runs
//! between frames.

mod atlas;
mod metal;
mod scene;

pub use metal::{PaneDraw, Renderer, Surface, Tint};
pub use scene::{Line, Overlay, RectInstance};
