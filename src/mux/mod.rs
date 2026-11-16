//! The model: windows, tabs and panes, with their terminals and child
//! processes. No platform types.

mod ids;
mod model;
mod modes;
mod pane;
mod tree;
mod window;

pub use ids::{PaneId, WindowId};
pub use model::{Mux, TabView, Visible};
pub use modes::ModeKey;
#[cfg(test)]
pub use pane::PaneInfo;
pub use pane::{Pane, RhoActivity};
pub use tree::{Axis, Direction, Rect};
