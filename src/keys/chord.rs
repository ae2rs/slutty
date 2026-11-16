//! Chords as the config writes them (`cmd+shift+t`) and keys as pressed.

use std::fmt;
use std::str::FromStr;

use libghostty_vt::key::{Key, Mods};
use serde::{Deserialize, Deserializer, de};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    pub cmd: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Modifiers {
    fn mods(self) -> Mods {
        let mut mods = Mods::empty();
        for (on, flag) in [(self.cmd, Mods::SUPER), (self.ctrl, Mods::CTRL), (self.alt, Mods::ALT), (self.shift, Mods::SHIFT)] {
            if on {
                mods |= flag;
            }
        }
        mods
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyName {
    /// A key that types this character with no modifiers.
    Char(char),
    Enter,
    Escape,
    Tab,
    Backspace,
    Delete,
    Insert,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    /// F1 to F12.
    F(u8),
}

const NAMES: [(&str, KeyName); 17] = [
    ("enter", KeyName::Enter),
    ("return", KeyName::Enter),
    ("escape", KeyName::Escape),
    ("esc", KeyName::Escape),
    ("tab", KeyName::Tab),
    ("backspace", KeyName::Backspace),
    ("delete", KeyName::Delete),
    ("insert", KeyName::Insert),
    ("up", KeyName::Up),
    ("down", KeyName::Down),
    ("left", KeyName::Left),
    ("right", KeyName::Right),
    ("home", KeyName::Home),
    ("end", KeyName::End),
    ("pageup", KeyName::PageUp),
    ("pagedown", KeyName::PageDown),
    ("space", KeyName::Char(' ')),
];

/// One key with its modifiers, as bound in the config.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    pub mods: Modifiers,
    pub key: KeyName,
}

/// A key as pressed, with both characters a binding may name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyPress {
    pub mods: Modifiers,
    /// The key without modifiers: `v` for cmd+shift+v.
    pub key: KeyName,
    /// What shift alone makes of the key: `V`, or `?` for `/`.
    pub shifted: Option<char>,
}

impl Chord {
    pub fn matches(&self, press: &KeyPress) -> bool {
        if self.mods == press.mods && self.key == press.key {
            return true;
        }
        // `R` and `?` name the shifted character; shift is then implied.
        press.mods.shift
            && self.mods == Modifiers { shift: false, ..press.mods }
            && press.shifted.is_some_and(|c| self.key == KeyName::Char(c))
    }

    /// The physical key, modifiers and character libghostty encodes for this
    /// chord, as if typed on a US layout.
    pub fn encoding(&self) -> (Key, Mods, Option<char>) {
        let (key, unshifted) = match self.key {
            KeyName::Char(c) if c.is_ascii_uppercase() => {
                return (char_key(c.to_ascii_lowercase()), self.mods.mods() | Mods::SHIFT, Some(c.to_ascii_lowercase()));
            }
            KeyName::Char(c) => (char_key(c), Some(c)),
            KeyName::Enter => (Key::Enter, None),
            KeyName::Escape => (Key::Escape, None),
            KeyName::Tab => (Key::Tab, None),
            KeyName::Backspace => (Key::Backspace, None),
            KeyName::Delete => (Key::Delete, None),
            KeyName::Insert => (Key::Insert, None),
            KeyName::Up => (Key::ArrowUp, None),
            KeyName::Down => (Key::ArrowDown, None),
            KeyName::Left => (Key::ArrowLeft, None),
            KeyName::Right => (Key::ArrowRight, None),
            KeyName::Home => (Key::Home, None),
            KeyName::End => (Key::End, None),
            KeyName::PageUp => (Key::PageUp, None),
            KeyName::PageDown => (Key::PageDown, None),
            KeyName::F(n) => (function_key(n), None),
        };
        (key, self.mods.mods(), unshifted)
    }
}

fn char_key(c: char) -> Key {
    const LETTERS: [Key; 26] = [
        Key::A,
        Key::B,
        Key::C,
        Key::D,
        Key::E,
        Key::F,
        Key::G,
        Key::H,
        Key::I,
        Key::J,
        Key::K,
        Key::L,
        Key::M,
        Key::N,
        Key::O,
        Key::P,
        Key::Q,
        Key::R,
        Key::S,
        Key::T,
        Key::U,
        Key::V,
        Key::W,
        Key::X,
        Key::Y,
        Key::Z,
    ];
    const DIGITS: [Key; 10] = [
        Key::Digit0,
        Key::Digit1,
        Key::Digit2,
        Key::Digit3,
        Key::Digit4,
        Key::Digit5,
        Key::Digit6,
        Key::Digit7,
        Key::Digit8,
        Key::Digit9,
    ];
    match c {
        'a'..='z' => LETTERS[c as usize - 'a' as usize],
        '0'..='9' => DIGITS[c as usize - '0' as usize],
        '-' => Key::Minus,
        '=' => Key::Equal,
        '[' => Key::BracketLeft,
        ']' => Key::BracketRight,
        '\\' => Key::Backslash,
        ';' => Key::Semicolon,
        '\'' => Key::Quote,
        ',' => Key::Comma,
        '.' => Key::Period,
        '/' => Key::Slash,
        '`' => Key::Backquote,
        ' ' => Key::Space,
        _ => Key::Unidentified,
    }
}

fn function_key(n: u8) -> Key {
    const KEYS: [Key; 12] = [
        Key::F1,
        Key::F2,
        Key::F3,
        Key::F4,
        Key::F5,
        Key::F6,
        Key::F7,
        Key::F8,
        Key::F9,
        Key::F10,
        Key::F11,
        Key::F12,
    ];
    KEYS.get(usize::from(n).wrapping_sub(1)).copied().unwrap_or(Key::Unidentified)
}

impl FromStr for KeyName {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let lower = s.to_ascii_lowercase();
        if let Some(&(_, key)) = NAMES.iter().find(|(name, _)| *name == lower) {
            return Ok(key);
        }
        let mut chars = s.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            return Ok(Self::Char(c));
        }
        match lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
            Some(n @ 1..=12) => Ok(Self::F(n)),
            _ => Err(format!("unknown key `{s}`")),
        }
    }
}

impl FromStr for Chord {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let (mods, key) = match s.strip_suffix("++") {
            Some(mods) => (mods, "+"),
            None if s == "+" => ("", "+"),
            None => s.rsplit_once('+').unwrap_or(("", s)),
        };
        let mut chord = Chord { mods: Modifiers::default(), key: key.parse()? };
        for name in mods.split('+').filter(|_| !mods.is_empty()) {
            let flag = match name.to_ascii_lowercase().as_str() {
                "cmd" | "command" | "super" => &mut chord.mods.cmd,
                "ctrl" | "control" => &mut chord.mods.ctrl,
                "alt" | "opt" | "option" => &mut chord.mods.alt,
                "shift" => &mut chord.mods.shift,
                _ => return Err(format!("unknown modifier `{name}` in `{s}`")),
            };
            *flag = true;
        }
        Ok(chord)
    }
}

impl<'de> Deserialize<'de> for Chord {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?.parse().map_err(de::Error::custom)
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = self.mods;
        for (on, name) in [(m.cmd, "cmd"), (m.ctrl, "ctrl"), (m.alt, "alt"), (m.shift, "shift")] {
            if on {
                write!(f, "{name}+")?;
            }
        }
        match self.key {
            KeyName::Char(' ') => f.write_str("space"),
            KeyName::Char(c) => write!(f, "{c}"),
            KeyName::F(n) => write!(f, "f{n}"),
            key => {
                let name = NAMES.iter().find(|(_, k)| *k == key).map_or("?", |(name, _)| name);
                f.write_str(name)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(s: &str) -> Chord {
        s.parse().expect(s)
    }

    fn press(mods: Modifiers, key: KeyName, shifted: Option<char>) -> KeyPress {
        KeyPress { mods, key, shifted }
    }

    const CMD: Modifiers = Modifiers { cmd: true, ctrl: false, alt: false, shift: false };
    const CMD_SHIFT: Modifiers = Modifiers { cmd: true, ctrl: false, alt: false, shift: true };

    #[test]
    fn parses_modifiers_names_and_plus() {
        let c = chord("Cmd+Shift+Left");
        assert_eq!(c, Chord { mods: CMD_SHIFT, key: KeyName::Left });
        assert_eq!(chord("cmd++").key, KeyName::Char('+'));
        assert_eq!(chord("+").key, KeyName::Char('+'));
        assert_eq!(chord("ctrl+-").key, KeyName::Char('-'));
        assert_eq!(chord("f5").key, KeyName::F(5));
        assert_eq!(chord("space").key, KeyName::Char(' '));
        assert!("hyper+a".parse::<Chord>().is_err());
        assert!("cmd+nope".parse::<Chord>().is_err());
        assert_eq!(chord("opt+right").to_string(), "alt+right");
    }

    #[test]
    fn shift_matches_by_key_or_by_shifted_character() {
        let p = press(CMD_SHIFT, KeyName::Char('r'), Some('R'));
        assert!(chord("cmd+shift+r").matches(&p));
        assert!(chord("cmd+R").matches(&p));
        assert!(!chord("cmd+r").matches(&p));
        let question = press(Modifiers { shift: true, ..Modifiers::default() }, KeyName::Char('/'), Some('?'));
        assert!(chord("?").matches(&question));
        assert!(!chord("cmd+R").matches(&press(CMD, KeyName::Char('r'), Some('R'))));
    }

    #[test]
    fn encodes_like_a_us_keyboard() {
        assert_eq!(chord("ctrl+u").encoding(), (Key::U, Mods::CTRL, Some('u')));
        assert_eq!(chord("U").encoding(), (Key::U, Mods::SHIFT, Some('u')));
        assert_eq!(chord("alt+left").encoding(), (Key::ArrowLeft, Mods::ALT, None));
    }
}
