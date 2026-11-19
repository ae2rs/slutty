//! The model: windows, tabs and panes, with their terminals and child
//! processes. No platform types.

mod float;
mod ids;
mod model;
mod modes;
mod pane;
mod tree;
mod window;

pub use float::{Anchor, Extent, Place, PlaceChange};
pub use ids::{PaneId, WindowId};
pub use model::{Mux, Placement};
pub use pane::{Pane, PaneInfo, RhoActivity};
pub use tree::{Axis, Direction, Layout, Rect};
pub use window::Float;
