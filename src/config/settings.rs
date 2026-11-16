//! Typed settings with their defaults. Loading TOML comes in a later
//! milestone (see docs/design.md); until then the defaults are the config.

#[derive(Clone, Debug)]
pub struct Config {
    pub font: Font,
    pub theme: Theme,
    pub window: Window,
    pub terminal: Terminal,
    pub mouse: Mouse,
}

#[derive(Clone, Debug)]
pub struct Font {
    /// Size in points.
    pub size: f64,
}

#[derive(Clone, Debug)]
pub struct Window {
    /// Initial size in cells.
    pub cols: u16,
    pub rows: u16,
    /// Horizontal padding, in cells, on each side.
    pub padding_cells: u16,
}

#[derive(Clone, Debug)]
pub struct Terminal {
    pub scrollback_lines: usize,
    pub cursor: CursorShape,
    pub option_as_alt: OptionAsAlt,
}

#[derive(Clone, Debug)]
pub struct Mouse {
    /// Lines scrolled per notch of a wheel without precise deltas.
    pub wheel_lines: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(dead_code, reason = "chosen in the config file, which comes in M2")]
pub enum CursorShape {
    Block,
    Bar,
    Underline,
}

/// Which Option keys send Alt. The others compose characters (`∂`, `é`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(dead_code, reason = "chosen in the config file, which comes in M2")]
pub enum OptionAsAlt {
    None,
    Left,
    Right,
    Both,
}

pub type Rgb = [u8; 3];

#[derive(Clone, Debug)]
pub struct Theme {
    pub background: Rgb,
    pub foreground: Rgb,
    pub cursor: Rgb,
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

impl Default for Config {
    fn default() -> Self {
        Self {
            font: Font { size: 16.0 },
            theme: Theme::tokyo_night(),
            window: Window { cols: 100, rows: 30, padding_cells: 1 },
            terminal: Terminal {
                scrollback_lines: 100_000,
                cursor: CursorShape::Bar,
                option_as_alt: OptionAsAlt::Left,
            },
            mouse: Mouse { wheel_lines: 5 },
        }
    }
}

const fn hex(value: u32) -> Rgb {
    let [_, r, g, b] = value.to_be_bytes();
    [r, g, b]
}
