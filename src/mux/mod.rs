//! The model: windows, tabs and panes, with their terminals and child
//! processes. No platform types.

mod ids;
mod model;
mod pane;
mod tree;
mod window;

pub use ids::{PaneId, WindowId};
pub use model::{Mux, TabView, Visible};
pub use pane::Pane;
#[cfg(test)]
pub use pane::PaneInfo;
pub use tree::{Axis, Direction, Rect};
