//! The settings with their defaults, as the config file writes them. Every
//! key is optional.

use std::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer};

use super::theme::Theme;
use crate::keys::Keys;

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub font: Font,
    pub theme: Theme,
    pub window: Window,
    pub terminal: Terminal,
    pub mouse: Mouse,
    pub tab_bar: TabBar,
    pub keys: Keys,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TabBar {
    /// Shown when a window opens; a key toggles it.
    pub enabled: bool,
    /// The widest a tab grows, in cells.
    pub max_width: u16,
    /// System stats on the right, in this order. They refresh on a timer
    /// while a window has focus, so none show by default.
    pub status: Vec<Stat>,
}

impl Default for TabBar {
    fn default() -> Self {
        Self { enabled: true, max_width: 32, status: Vec::new() }
    }
}

/// A system stat the tab bar can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stat {
    /// Memory apps use, as Activity Monitor's "Memory Used".
    Ram,
    /// Busy share of every core.
    Cpu,
    /// Bytes a second in and out of the Ethernet and Wi-Fi interfaces.
    Net,
    /// Share of the data volume in use.
    Disk,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Font {
    pub family: String,
    /// Size in points.
    pub size: f64,
    /// Cell height as a multiple of the font's line height.
    pub line_height: f64,
    pub bold_weight: FontWeight,
    pub faint_weight: FontWeight,
    /// Join character sequences such as `->` into one symbol, where the font
    /// has one.
    pub ligatures: bool,
}

impl Default for Font {
    fn default() -> Self {
        Self {
            family: "JetBrains Mono".into(),
            size: 16.0,
            line_height: 1.0,
            bold_weight: FontWeight::Demibold,
            faint_weight: FontWeight::Extralight,
            ligatures: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FontWeight {
    Thin,
    Extralight,
    Light,
    Regular,
    Medium,
    #[serde(alias = "semibold")]
    Demibold,
    Bold,
    Extrabold,
    Black,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Window {
    /// Initial size in cells.
    pub cols: u16,
    pub rows: u16,
    pub padding: Padding,
    /// Ask before quitting.
    pub confirm_quit: bool,
}

impl Default for Window {
    fn default() -> Self {
        Self { cols: 100, rows: 30, padding: Padding::default(), confirm_quit: true }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Padding {
    pub left: Length,
    pub right: Length,
    pub top: Length,
    pub bottom: Length,
}

impl Default for Padding {
    fn default() -> Self {
        Self { left: Length::Cells(1), right: Length::Cells(1), top: Length::Points(0.0), bottom: Length::Points(0.0) }
    }
}

/// A distance written as points (`4`, `"4pt"`) or cells (`"1cell"`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Length {
    Points(f64),
    Cells(u16),
}

impl Length {
    /// In device pixels, for a cell of `cell` pixels at `scale` pixels per
    /// point.
    pub fn pixels(self, cell: u32, scale: f64) -> u32 {
        match self {
            Self::Points(points) => (points * scale).round() as u32,
            Self::Cells(cells) => u32::from(cells) * cell,
        }
    }
}

impl<'de> Deserialize<'de> for Length {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct LengthVisitor;

        impl Visitor<'_> for LengthVisitor {
            type Value = Length;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("points, such as 4 or \"4pt\", or cells, such as \"1cell\"")
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Length, E> {
                u16::try_from(v)
                    .map(|v| Length::Points(f64::from(v)))
                    .map_err(|_| E::custom("a length cannot be negative"))
            }

            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Length, E> {
                if v >= 0.0 { Ok(Length::Points(v)) } else { Err(E::custom("a length cannot be negative")) }
            }

            fn visit_str<E: de::Error>(self, s: &str) -> Result<Length, E> {
                let invalid = || E::invalid_value(de::Unexpected::Str(s), &self);
                if let Some(n) = s.strip_suffix("cells").or_else(|| s.strip_suffix("cell")) {
                    n.trim().parse().map(Length::Cells).map_err(|_| invalid())
                } else if let Some(n) = s.strip_suffix("pt") {
                    n.trim().parse::<f64>().ok().filter(|v| *v >= 0.0).map(Length::Points).ok_or_else(invalid)
                } else {
                    Err(invalid())
                }
            }
        }

        deserializer.deserialize_any(LengthVisitor)
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Terminal {
    pub scrollback_lines: usize,
    pub cursor: CursorShape,
    /// Whether the cursor blinks until a program says otherwise, as
    /// WezTerm's `default_cursor_style`.
    pub cursor_blink: bool,
    pub option_as_alt: OptionAsAlt,
}

impl Default for Terminal {
    fn default() -> Self {
        Self {
            scrollback_lines: 100_000,
            cursor: CursorShape::Bar,
            cursor_blink: false,
            option_as_alt: OptionAsAlt::Left,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Mouse {
    /// Lines scrolled per notch of a wheel without precise deltas.
    pub wheel_lines: u16,
}

impl Default for Mouse {
    fn default() -> Self {
        Self { wheel_lines: 5 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CursorShape {
    Block,
    Bar,
    Underline,
}

/// Which Option keys send Alt. The others compose characters (`∂`, `é`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OptionAsAlt {
    None,
    Left,
    Right,
    Both,
}
