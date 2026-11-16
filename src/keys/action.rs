//! What a binding does. Written in the config as a name (`"paste"`) or as a
//! table with `action` and its arguments.

use std::fmt;

use serde::de::value::{MapAccessDeserializer, MapDeserializer};
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};

use super::chord::Chord;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Action {
    /// Sends text to the program as if typed.
    Send {
        text: String,
    },
    /// Sends a key to the program, encoded like a key press.
    SendKey {
        key: Chord,
    },
    Paste,
    Quit,
    IncreaseFontSize,
    DecreaseFontSize,
    ResetFontSize,
    ReloadConfig,
    ToggleFullscreen,
    /// Makes a key table active. A sticky table stays until `pop-table`;
    /// otherwise it lasts one key.
    KeyTable {
        name: String,
        #[serde(default)]
        sticky: bool,
    },
    PopTable,
    NewTab,
    CloseTab {
        /// Ask first when something other than a shell runs in it.
        #[serde(default)]
        confirm: bool,
    },
    ActivateTab {
        index: TabIndex,
    },
    ToggleTabBar,
}

/// A tab by position: 1 is the first, -1 the last.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "i32")]
pub struct TabIndex(i32);

impl TabIndex {
    /// The index into `count` tabs, if there is such a tab.
    pub fn resolve(self, count: usize) -> Option<usize> {
        let n = usize::try_from(self.0.unsigned_abs()).ok()?;
        if self.0 > 0 { Some(n - 1).filter(|&i| i < count) } else { count.checked_sub(n) }
    }
}

impl TryFrom<i32> for TabIndex {
    type Error = &'static str;

    fn try_from(index: i32) -> Result<Self, Self::Error> {
        if index == 0 { Err("tabs count from 1, or from -1 for the last") } else { Ok(Self(index)) }
    }
}

/// A binding's value in the config: an action, or `"none"` to remove a
/// default binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Spec {
    Bind(Action),
    Unbind,
}

impl<'de> Deserialize<'de> for Spec {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SpecVisitor;

        impl<'de> Visitor<'de> for SpecVisitor {
            type Value = Spec;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an action name or a table with `action`")
            }

            fn visit_str<E: de::Error>(self, name: &str) -> Result<Spec, E> {
                if name == "none" {
                    return Ok(Spec::Unbind);
                }
                let fields = MapDeserializer::new(std::iter::once(("action", name)));
                Action::deserialize(fields).map(Spec::Bind)
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Spec, A::Error> {
                Action::deserialize(MapAccessDeserializer::new(map)).map(Spec::Bind)
            }
        }

        deserializer.deserialize_any(SpecVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_indexes_count_from_either_end() {
        assert_eq!(TabIndex(1).resolve(3), Some(0));
        assert_eq!(TabIndex(3).resolve(3), Some(2));
        assert_eq!(TabIndex(4).resolve(3), None);
        assert_eq!(TabIndex(-1).resolve(3), Some(2));
        assert_eq!(TabIndex(-3).resolve(3), Some(0));
        assert_eq!(TabIndex(-4).resolve(3), None);
        assert!(TabIndex::try_from(0).is_err());
    }
}
