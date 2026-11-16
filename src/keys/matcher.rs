//! Turns key presses into actions: multi-key bindings such as `leader v`,
//! and the stack of active key tables.

use super::action::Action;
use super::bindings::{Binding, Keys};
use super::chord::KeyPress;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A binding fired.
    Run(Action),
    /// The key was used: it started a binding, changed tables, or cancelled
    /// a binding it did not continue.
    Handled,
    /// No binding wants the key; it goes to the program.
    Unbound,
}

struct Table {
    name: String,
    sticky: bool,
}

/// Where the user is in a binding. Lives as long as the window, and is reset
/// when the bindings change.
#[derive(Default)]
pub struct Matcher {
    pending: Vec<KeyPress>,
    tables: Vec<Table>,
}

impl Matcher {
    pub fn press(&mut self, keys: &Keys, press: KeyPress) -> Outcome {
        self.pending.push(press);
        let layers = self.tables.iter().rev().filter_map(|t| keys.tables.get(&t.name)).chain([&keys.base]);
        // The first layer, from the top table down, where these keys start a
        // binding decides: run it if complete, else wait for the next key.
        let mut started = None;
        for layer in layers {
            let mut bindings = layer.iter().filter(|b| b.starts_with(&self.pending)).peekable();
            if bindings.peek().is_some() {
                started =
                    Some(bindings.find(|b| b.chords.len() == self.pending.len()).map(|b: &Binding| b.action.clone()));
                break;
            }
        }
        if started == Some(None) {
            return Outcome::Handled;
        }

        let broken = self.pending.len() > 1;
        self.pending.clear();
        // A one-shot table lasts one key.
        if self.tables.last().is_some_and(|t| !t.sticky) {
            self.tables.pop();
        }
        match started.flatten() {
            Some(Action::KeyTable { name, sticky }) => {
                self.tables.push(Table { name, sticky });
                Outcome::Handled
            }
            Some(Action::PopTable) => {
                self.tables.pop();
                Outcome::Handled
            }
            Some(action) => Outcome::Run(action),
            // The key broke a binding it did not continue.
            None if broken => Outcome::Handled,
            None => Outcome::Unbound,
        }
    }

    /// No binding is half typed and no table is active.
    pub fn is_idle(&self) -> bool {
        self.pending.is_empty() && self.tables.is_empty()
    }

    pub fn reset(&mut self) {
        self.pending.clear();
        self.tables.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::bindings::KeysFile;
    use crate::keys::chord::{KeyName, Modifiers};

    fn keys(toml: &str) -> Keys {
        Keys::try_from(toml::from_str::<KeysFile>(toml).expect("toml")).expect("keys")
    }

    fn press(chord: &str) -> KeyPress {
        let c: crate::keys::chord::Chord = chord.parse().expect(chord);
        let shifted = match c.key {
            KeyName::Char(ch) if c.mods.shift => Some(ch.to_ascii_uppercase()),
            _ => None,
        };
        KeyPress { mods: c.mods, key: c.key, shifted }
    }

    const KEYS: &str = r#"
        defaults = false
        [bind]
        "leader v" = "paste"
        "cmd+q" = "quit"
        "leader r" = { action = "key-table", name = "resize", sticky = true }
        "leader o" = { action = "key-table", name = "once" }
        [tables.resize]
        "h" = { action = "send", text = "left" }
        "escape" = "pop-table"
        [tables.once]
        "h" = { action = "send", text = "once" }
    "#;

    fn send(text: &str) -> Outcome {
        Outcome::Run(Action::Send { text: text.into() })
    }

    #[test]
    fn leader_waits_for_the_next_key() {
        let k = keys(KEYS);
        let mut m = Matcher::default();
        assert_eq!(m.press(&k, press("ctrl+s")), Outcome::Handled);
        assert_eq!(m.press(&k, press("v")), Outcome::Run(Action::Paste));
        assert_eq!(m.press(&k, press("v")), Outcome::Unbound);
        assert_eq!(m.press(&k, press("cmd+q")), Outcome::Run(Action::Quit));
    }

    #[test]
    fn a_key_that_breaks_a_binding_is_swallowed() {
        let k = keys(KEYS);
        let mut m = Matcher::default();
        m.press(&k, press("ctrl+s"));
        assert_eq!(m.press(&k, press("z")), Outcome::Handled);
        assert_eq!(m.press(&k, press("z")), Outcome::Unbound);
    }

    #[test]
    fn sticky_tables_stay_until_popped() {
        let k = keys(KEYS);
        let mut m = Matcher::default();
        m.press(&k, press("ctrl+s"));
        m.press(&k, press("r"));
        assert_eq!(m.press(&k, press("h")), send("left"));
        assert_eq!(m.press(&k, press("h")), send("left"));
        assert_eq!(m.press(&k, press("x")), Outcome::Unbound, "other keys pass through");
        assert_eq!(m.press(&k, press("cmd+q")), Outcome::Run(Action::Quit), "the base layer still works");
        assert_eq!(m.press(&k, press("escape")), Outcome::Handled);
        assert_eq!(m.press(&k, press("h")), Outcome::Unbound);
    }

    #[test]
    fn one_shot_tables_last_one_binding() {
        let k = keys(KEYS);
        let mut m = Matcher::default();
        m.press(&k, press("ctrl+s"));
        m.press(&k, press("o"));
        assert_eq!(m.press(&k, press("h")), send("once"));
        assert_eq!(m.press(&k, press("h")), Outcome::Unbound);
    }

    #[test]
    fn shifted_letters_bind_apart_from_plain_ones() {
        let k = keys("defaults = false\n[bind]\n\"leader M\" = \"quit\"\n\"leader m\" = \"paste\"");
        let mut m = Matcher::default();
        m.press(&k, press("ctrl+s"));
        let shift_m = KeyPress {
            mods: Modifiers { shift: true, ..Modifiers::default() },
            key: KeyName::Char('m'),
            shifted: Some('M'),
        };
        assert_eq!(m.press(&k, shift_m), Outcome::Run(Action::Quit));
        m.press(&k, press("ctrl+s"));
        assert_eq!(m.press(&k, press("m")), Outcome::Run(Action::Paste));
    }
}
