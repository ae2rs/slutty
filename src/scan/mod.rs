//! Finding things in the text on screen: links and quick-select matches.
//! Pure: rows in, cell spans out.

mod links;
mod quick_select;
mod row;

pub use links::link_at;
pub use quick_select::{Labelled, default_patterns, find, url_pattern};
pub use row::RowText;
