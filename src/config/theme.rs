//! Colors: a built-in theme with single colors overridden by the config.

use serde::{Deserialize, Deserializer, de};

pub type Rgb = [u8; 3];

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(from = "ThemeFile")]
pub struct Theme {
    pub background: Rgb,
    pub foreground: Rgb,
    pub cursor: Rgb,
    /// The line between split panes.
    pub split: Rgb,
    pub selection_fg: Rgb,
    pub selection_bg: Rgb,
    /// The 16 ANSI colors: normal then bright.
    pub ansi: [Rgb; 16],
}

impl Theme {
    /// WezTerm's built-in "Tokyo Night".
    pub fn tokyo_night() -> Self {
        Self {
            background: hex(0x1a1b26),
            foreground: hex(0xc0caf5),
            cursor: hex(0xc0caf5),
            // WezTerm's default; the scheme sets none.
            split: hex(0x444444),
            selection_fg: hex(0xc0caf5),
            selection_bg: hex(0x283457),
            ansi: [
                hex(0x15161e),
                hex(0xf7768e),
                hex(0x9ece6a),
                hex(0xe0af68),
                hex(0x7aa2f7),
                hex(0xbb9af7),
                hex(0x7dcfff),
                hex(0xa9b1d6),
                hex(0x414868),
                hex(0xf7768e),
                hex(0x9ece6a),
                hex(0xe0af68),
                hex(0x7aa2f7),
                hex(0xbb9af7),
                hex(0x7dcfff),
                hex(0xc0caf5),
            ],
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::tokyo_night()
    }
}

const fn hex(value: u32) -> Rgb {
    let [_, r, g, b] = value.to_be_bytes();
    [r, g, b]
}

/// `[theme]` as written in the config file.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct ThemeFile {
    name: ThemeName,
    colors: Colors,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ThemeName {
    #[default]
    TokyoNight,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct Colors {
    background: Option<Hex>,
    foreground: Option<Hex>,
    cursor: Option<Hex>,
    split: Option<Hex>,
    selection_fg: Option<Hex>,
    selection_bg: Option<Hex>,
    ansi: Option<[Hex; 8]>,
    brights: Option<[Hex; 8]>,
}

impl From<ThemeFile> for Theme {
    fn from(file: ThemeFile) -> Self {
        let mut theme = match file.name {
            ThemeName::TokyoNight => Self::tokyo_night(),
        };
        let c = file.colors;
        for (slot, color) in [
            (&mut theme.background, c.background),
            (&mut theme.foreground, c.foreground),
            (&mut theme.cursor, c.cursor),
            (&mut theme.split, c.split),
            (&mut theme.selection_fg, c.selection_fg),
            (&mut theme.selection_bg, c.selection_bg),
        ] {
            if let Some(Hex(rgb)) = color {
                *slot = rgb;
            }
        }
        let (normal, bright) = theme.ansi.split_at_mut(8);
        for (slots, colors) in [(normal, c.ansi), (bright, c.brights)] {
            if let Some(colors) = colors {
                for (slot, Hex(rgb)) in slots.iter_mut().zip(colors) {
                    *slot = rgb;
                }
            }
        }
        theme
    }
}

/// A color written as `"#rrggbb"`.
struct Hex(Rgb);

impl<'de> Deserialize<'de> for Hex {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.strip_prefix('#')
            .filter(|digits| digits.len() == 6)
            .and_then(|digits| u32::from_str_radix(digits, 16).ok())
            .map(|value| Hex(hex(value)))
            .ok_or_else(|| de::Error::custom(format!("`{s}` is not a #rrggbb color")))
    }
}
