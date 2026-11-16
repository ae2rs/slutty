//! A pane's copy and search modes, and the keys they take: WezTerm's
//! default `copy_mode` and `search_mode` tables, with the closing keys of
//! Lucas's rho setup.

use super::pane::Pane;
use crate::keys::{KeyName, KeyPress, Modifiers};
use crate::terminal::copy_mode::{Command, Jump, Kind, Motion};
use crate::terminal::{Highlight, SearchStatus};

/// What a key did in a pane's mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModeKey {
    Used,
    /// The mode does not know the key: it goes to the bindings, and to the
    /// program only when no mode is open.
    Unused,
    /// Copy mode closed; copy this text, if any.
    Closed(Option<String>),
}

/// What a key does in copy mode.
enum CopyKey {
    Command(Command),
    Copy,
    Close,
}

impl Pane {
    pub fn start_copy_mode(&mut self) {
        self.terminal.close_search();
        self.terminal.start_copy_mode();
    }

    /// Whether copy mode or search is open.
    pub fn in_mode(&self) -> bool {
        self.terminal.in_copy_mode() || self.terminal.search_status().is_some()
    }

    pub fn copy_cursor(&self) -> Option<(u16, u16)> {
        self.terminal.copy_cursor()
    }

    /// Opens search, looking for `needle`.
    pub fn start_search(&mut self, needle: &str) {
        self.terminal.close_copy_mode(false);
        self.terminal.search(needle);
    }

    pub fn search_status(&self) -> Option<SearchStatus> {
        self.terminal.search_status()
    }

    pub fn search_highlights(&mut self) -> Vec<Highlight> {
        self.terminal.search_highlights()
    }

    /// Adds pasted text to the search, if it is open. Returns whether it was.
    pub fn paste_into_search(&mut self, text: &str) -> bool {
        let Some(status) = self.terminal.search_status() else { return false };
        self.terminal.search(&format!("{}{}", status.needle, text.replace(['\n', '\r'], " ")));
        true
    }

    /// Gives a key to the open mode first.
    pub fn mode_key(&mut self, press: KeyPress) -> ModeKey {
        if self.terminal.in_copy_mode() {
            self.copy_mode_key(press)
        } else if let Some(status) = self.terminal.search_status() {
            self.search_key(press, status.needle)
        } else {
            ModeKey::Unused
        }
    }

    fn copy_mode_key(&mut self, press: KeyPress) -> ModeKey {
        if press.mods.cmd {
            return ModeKey::Unused;
        }
        if self.terminal.copy_mode_awaits_jump() {
            if let Some(c) = typed(press) {
                self.terminal.copy_mode_jump(c);
            }
            return ModeKey::Used;
        }
        match copy_key(press) {
            Some(CopyKey::Command(command)) => self.terminal.copy_mode(command),
            Some(CopyKey::Copy) => return ModeKey::Closed(self.terminal.close_copy_mode(true)),
            Some(CopyKey::Close) => return ModeKey::Closed(self.terminal.close_copy_mode(false)),
            None => return ModeKey::Unused,
        }
        ModeKey::Used
    }

    fn search_key(&mut self, press: KeyPress, needle: String) -> ModeKey {
        let Modifiers { cmd, ctrl, alt, .. } = press.mods;
        if cmd {
            return ModeKey::Unused;
        }
        match (ctrl, press.key) {
            (false, KeyName::Escape) => {
                self.terminal.close_search();
                return ModeKey::Closed(None);
            }
            // Enter goes to older output, as in WezTerm.
            (false, KeyName::Enter | KeyName::Up | KeyName::PageUp) | (true, KeyName::Char('p')) => {
                self.terminal.search_step(true)
            }
            (false, KeyName::Down | KeyName::PageDown) | (true, KeyName::Char('n')) => self.terminal.search_step(false),
            (true, KeyName::Char('u')) => self.terminal.search(""),
            (false, KeyName::Backspace) => {
                let mut needle = needle;
                needle.pop();
                self.terminal.search(&needle);
            }
            (false, _) if !alt => match typed(press) {
                Some(c) => self.terminal.search(&format!("{needle}{c}")),
                None => return ModeKey::Unused,
            },
            _ => return ModeKey::Unused,
        }
        ModeKey::Used
    }
}

/// The character a key types with shift alone.
fn typed(press: KeyPress) -> Option<char> {
    match press.key {
        KeyName::Char(c) if press.mods.shift => press.shifted.or(Some(c)),
        KeyName::Char(c) => Some(c),
        _ => None,
    }
}

fn copy_key(press: KeyPress) -> Option<CopyKey> {
    use CopyKey::{Close, Copy};
    use Motion::*;
    let Modifiers { ctrl, alt, shift, .. } = press.mods;
    let go = |motion| Some(CopyKey::Command(Command::Move(motion)));
    let select = |kind| Some(CopyKey::Command(Command::Select(kind)));
    let jump = |forward, till| Some(CopyKey::Command(Command::Jump(Jump { forward, till })));
    if ctrl {
        return match press.key {
            KeyName::Char('b') => go(Halves(-2)),
            KeyName::Char('f') => go(Halves(2)),
            KeyName::Char('u') => go(Halves(-1)),
            KeyName::Char('d') => go(Halves(1)),
            KeyName::Char('v') => select(Kind::Block),
            KeyName::Char('c' | 'g') => Some(Close),
            _ => None,
        };
    }
    if alt {
        return match press.key {
            KeyName::Char('b') | KeyName::Left => go(WordBackward),
            KeyName::Char('f') | KeyName::Right => go(WordForward),
            KeyName::Char('m') => go(LineContentStart),
            _ => None,
        };
    }
    match press.key {
        KeyName::Tab if shift => return go(WordBackward),
        KeyName::Tab => return go(WordForward),
        KeyName::Enter => return go(NextLineStart),
        KeyName::Escape => return Some(Close),
        KeyName::Left => return go(Left),
        KeyName::Right => return go(Right),
        KeyName::Up => return go(Up),
        KeyName::Down => return go(Down),
        KeyName::PageUp => return go(Halves(-2)),
        KeyName::PageDown => return go(Halves(2)),
        KeyName::Home => return go(LineStart),
        KeyName::End => return go(LineContentEnd),
        _ => {}
    }
    match typed(press)? {
        ' ' | 'v' => select(Kind::Cell),
        'V' => select(Kind::Line),
        '$' => go(LineContentEnd),
        '^' => go(LineContentStart),
        '0' => go(LineStart),
        ',' => go(JumpReverse),
        ';' => go(JumpAgain),
        'f' => jump(true, false),
        't' => jump(true, true),
        'F' => jump(false, false),
        'T' => jump(false, true),
        'g' => go(Top),
        'G' => go(Bottom),
        'H' => go(ViewportTop),
        'M' => go(ViewportMiddle),
        'L' => go(ViewportBottom),
        'o' => Some(CopyKey::Command(Command::OtherEnd)),
        'O' => Some(CopyKey::Command(Command::OtherEndHoriz)),
        'w' => go(WordForward),
        'b' => go(WordBackward),
        'e' => go(WordEnd),
        'h' => go(Left),
        'j' => go(Down),
        'k' => go(Up),
        'l' => go(Right),
        'y' => Some(Copy),
        'q' => Some(Close),
        _ => None,
    }
}
