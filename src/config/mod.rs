//! Typed settings, their defaults, and loading them from the TOML file.
//! Other modules take the typed structs and never see TOML.

mod load;
mod settings;
mod theme;

pub use load::{load, path};
pub use settings::{Config, CursorShape, Font, FontWeight, OptionAsAlt, Padding};
pub use theme::Rgb;
