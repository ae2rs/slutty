//! The keys of a pane's copy and search modes: WezTerm's default
//! `copy_mode` and `search_mode` tables, with the closing keys of Lucas's
//! rho setup. Read on the UI side, so a key press never waits on the server.

use crate::keys::{KeyName, KeyPress, Modifiers};
use crate::proto::{Mode, ModeOp};
use crate::terminal::copy_mode::{Command, Jump, Kind, Motion};

/// What a key does in a pane's mode.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ModeKey {
    Op(ModeOp),
    /// The mode takes the key and does nothing with it.
    Swallow,
    /// The mode does not know the key: it goes to the bindings, and to the
    /// program only when no mode is open.
    Unused,
}

/// The key `press` in `mode`. `awaits_jump` is set after an `f` or `t` in
/// copy mode, until a character comes.
pub fn mode_key(mode: &Mode, awaits_jump: bool, press: KeyPress) -> ModeKey {
    match mode {
        Mode::None => ModeKey::Unused,
        Mode::Copy => copy_mode_key(awaits_jump, press),
        Mode::Search(_) => search_key(press),
    }
}

fn copy_mode_key(awaits_jump: bool, press: KeyPress) -> ModeKey {
    if press.mods.cmd {
        return ModeKey::Unused;
    }
    if awaits_jump {
        return typed(press).map_or(ModeKey::Swallow, |c| ModeKey::Op(ModeOp::CopyJump(c)));
    }
    copy_key(press).map_or(ModeKey::Unused, ModeKey::Op)
}

fn search_key(press: KeyPress) -> ModeKey {
    let Modifiers { cmd, ctrl, alt, .. } = press.mods;
    if cmd {
        return ModeKey::Unused;
    }
    let op = match (ctrl, press.key) {
        (false, KeyName::Escape) => ModeOp::SearchClose,
        // Enter goes to older output, as in WezTerm.
        (false, KeyName::Enter | KeyName::Up | KeyName::PageUp) | (true, KeyName::Char('p')) => {
            ModeOp::SearchStep { older: true }
        }
        (false, KeyName::Down | KeyName::PageDown) | (true, KeyName::Char('n')) => ModeOp::SearchStep { older: false },
        (true, KeyName::Char('u')) => ModeOp::SearchClear,
        // WezTerm's CycleMatchType; libghostty's text search ignores case.
        (true, KeyName::Char('r')) => ModeOp::SearchToggleRegex,
        (false, KeyName::Backspace) => ModeOp::SearchBackspace,
        (false, _) if !alt => match typed(press) {
            Some(c) => ModeOp::SearchType(c),
            None => return ModeKey::Unused,
        },
        _ => return ModeKey::Unused,
    };
    ModeKey::Op(op)
}

/// The character a key types with shift alone.
fn typed(press: KeyPress) -> Option<char> {
    match press.key {
        KeyName::Char(c) if press.mods.shift => press.shifted.or(Some(c)),
        KeyName::Char(c) => Some(c),
        _ => None,
    }
}

fn copy_key(press: KeyPress) -> Option<ModeOp> {
    use Motion::*;
    let Modifiers { ctrl, alt, shift, .. } = press.mods;
    let go = |motion| Some(ModeOp::Copy(Command::Move(motion)));
    let select = |kind| Some(ModeOp::Copy(Command::Select(kind)));
    let jump = |forward, till| Some(ModeOp::Copy(Command::Jump(Jump { forward, till })));
    let close = |copy| Some(ModeOp::CopyClose { copy });
    if ctrl {
        return match press.key {
            KeyName::Char('b') => go(Halves(-2)),
            KeyName::Char('f') => go(Halves(2)),
            KeyName::Char('u') => go(Halves(-1)),
            KeyName::Char('d') => go(Halves(1)),
            KeyName::Char('v') => select(Kind::Block),
            KeyName::Char('c' | 'g') => close(false),
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
        KeyName::Escape => return close(false),
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
        'o' => Some(ModeOp::Copy(Command::OtherEnd)),
        'O' => Some(ModeOp::Copy(Command::OtherEndHoriz)),
        'w' => go(WordForward),
        'b' => go(WordBackward),
        'e' => go(WordEnd),
        'h' => go(Left),
        'j' => go(Down),
        'k' => go(Up),
        'l' => go(Right),
        'y' => close(true),
        'q' => close(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(key: KeyName, mods: Modifiers) -> KeyPress {
        KeyPress { key, mods, shifted: None }
    }

    fn plain(c: char) -> KeyPress {
        press(KeyName::Char(c), Modifiers::default())
    }

    #[test]
    fn copy_mode_reads_vim_keys_and_leaves_command_chords() {
        let go = |m| ModeKey::Op(ModeOp::Copy(Command::Move(m)));
        assert_eq!(mode_key(&Mode::Copy, false, plain('j')), go(Motion::Down));
        assert_eq!(mode_key(&Mode::Copy, false, plain('y')), ModeKey::Op(ModeOp::CopyClose { copy: true }));
        assert_eq!(mode_key(&Mode::Copy, false, plain('z')), ModeKey::Unused);
        let cmd = Modifiers { cmd: true, ..Modifiers::default() };
        assert_eq!(mode_key(&Mode::Copy, false, press(KeyName::Char('c'), cmd)), ModeKey::Unused);
    }

    #[test]
    fn a_pending_jump_takes_the_next_character() {
        assert_eq!(mode_key(&Mode::Copy, true, plain('x')), ModeKey::Op(ModeOp::CopyJump('x')));
        assert_eq!(mode_key(&Mode::Copy, true, press(KeyName::Left, Modifiers::default())), ModeKey::Swallow);
    }

    #[test]
    fn search_types_into_the_needle_and_leaves_the_leader() {
        let search = Mode::Search(crate::terminal::SearchStatus::default());
        assert_eq!(mode_key(&search, false, plain('a')), ModeKey::Op(ModeOp::SearchType('a')));
        let ctrl = Modifiers { ctrl: true, ..Modifiers::default() };
        assert_eq!(mode_key(&search, false, press(KeyName::Char('s'), ctrl)), ModeKey::Unused);
        assert_eq!(mode_key(&Mode::None, false, plain('a')), ModeKey::Unused);
    }
}
