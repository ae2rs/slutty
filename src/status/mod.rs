//! System stats for the tab bar: counters read from the kernel, turned into
//! rates and shares, and laid out at fixed widths so the bar never shifts.

mod counters;
mod display;

pub use display::{Ink, Monitor, Stats, segments, width};
