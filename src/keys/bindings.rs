//! The `[keys]` section: the leader, bindings and named key tables, merged
//! over the built-in defaults.

use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};

use super::action::{Action, Spec};
use super::chord::{Chord, KeyName, KeyPress, Modifiers};

/// slutty's own bindings, which a config file extends.
const DEFAULTS: &str = include_str!("defaults.toml");

/// One step of a binding as written: a chord, or the leader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Leader,
    Chord(Chord),
}

/// Bindings in the order the file lists them.
#[derive(Clone, Debug, Default)]
struct BindList(Vec<(Vec<Step>, Spec)>);

/// `[keys]` as written in the config file.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct KeysFile {
    leader: Chord,
    /// Start from slutty's bindings; false starts from none.
    defaults: bool,
    bind: BindList,
    tables: BTreeMap<String, BindList>,
}

impl Default for KeysFile {
    fn default() -> Self {
        Self {
            leader: Chord { mods: Modifiers { ctrl: true, ..Modifiers::default() }, key: KeyName::Char('s') },
            defaults: true,
            bind: BindList::default(),
            tables: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub chords: Vec<Chord>,
    pub action: Action,
}

impl Binding {
    /// Whether `presses` start this binding.
    pub(super) fn starts_with(&self, presses: &[KeyPress]) -> bool {
        presses.len() <= self.chords.len() && self.chords.iter().zip(presses).all(|(c, p)| c.matches(p))
    }
}

/// The bindings in effect: the base layer and the named key tables.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "KeysFile")]
pub struct Keys {
    pub(super) base: Vec<Binding>,
    pub(super) tables: BTreeMap<String, Vec<Binding>>,
}

impl Default for Keys {
    fn default() -> Self {
        Self::try_from(KeysFile::default()).expect("the built-in bindings are valid")
    }
}

impl TryFrom<KeysFile> for Keys {
    type Error = String;

    fn try_from(file: KeysFile) -> Result<Self, String> {
        let mut base = Vec::new();
        let mut tables = BTreeMap::<String, Vec<Binding>>::new();
        let defaults = if file.defaults {
            toml::from_str::<KeysFile>(DEFAULTS).map_err(|e| format!("built-in bindings: {e}"))?
        } else {
            KeysFile::default()
        };
        let leader = file.leader;
        for layer in [defaults, file] {
            merge(&mut base, layer.bind, leader);
            for (name, list) in layer.tables {
                merge(tables.entry(name).or_default(), list, leader);
            }
        }
        for binding in base.iter().chain(tables.values().flatten()) {
            if let Action::KeyTable { name, .. } = &binding.action
                && !tables.contains_key(name)
            {
                return Err(format!("no key table named `{name}`"));
            }
        }
        Ok(Self { base, tables })
    }
}

/// Adds `list` over `bindings`: the same chords replace an earlier binding,
/// and `"none"` removes it.
fn merge(bindings: &mut Vec<Binding>, list: BindList, leader: Chord) {
    for (steps, spec) in list.0 {
        let chords: Vec<Chord> = steps
            .iter()
            .map(|step| match *step {
                Step::Leader => leader,
                Step::Chord(chord) => chord,
            })
            .collect();
        bindings.retain(|b| b.chords != chords);
        if let Spec::Bind(action) = spec {
            bindings.push(Binding { chords, action });
        }
    }
}

fn parse_steps(s: &str) -> Result<Vec<Step>, String> {
    let steps = s
        .split_whitespace()
        .map(|word| if word.eq_ignore_ascii_case("leader") { Ok(Step::Leader) } else { word.parse().map(Step::Chord) })
        .collect::<Result<Vec<_>, _>>()?;
    if steps.is_empty() {
        return Err("empty binding".into());
    }
    Ok(steps)
}

impl<'de> Deserialize<'de> for BindList {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ListVisitor;

        impl<'de> Visitor<'de> for ListVisitor {
            type Value = BindList;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a table of bindings")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<BindList, A::Error> {
                let mut list = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    let steps = parse_steps(&key).map_err(de::Error::custom)?;
                    list.push((steps, map.next_value()?));
                }
                Ok(BindList(list))
            }
        }

        deserializer.deserialize_map(ListVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(toml: &str) -> Result<Keys, String> {
        let file: KeysFile = toml::from_str(toml).map_err(|e| e.to_string())?;
        Keys::try_from(file)
    }

    fn chords(s: &str) -> Vec<Chord> {
        s.split_whitespace().map(|c| c.parse().expect(c)).collect()
    }

    fn action(keys: &Keys, s: &str) -> Option<Action> {
        let c = chords(s);
        keys.base.iter().find(|b| b.chords == c).map(|b| b.action.clone())
    }

    #[test]
    fn defaults_parse_and_resolve_the_leader() {
        let k = Keys::default();
        assert_eq!(action(&k, "cmd+q"), Some(Action::Quit));
        assert_eq!(action(&k, "ctrl+s R"), Some(Action::ReloadConfig));
    }

    #[test]
    fn the_file_overrides_removes_and_adds() {
        let k = keys(
            r#"
            leader = "ctrl+a"
            [bind]
            "cmd+q" = "none"
            "cmd+v" = { action = "send", text = "x" }
            "leader x" = "paste"
            "#,
        )
        .expect("valid");
        assert_eq!(action(&k, "cmd+q"), None);
        assert_eq!(action(&k, "cmd+v"), Some(Action::Send { text: "x".into() }));
        assert_eq!(action(&k, "ctrl+a x"), Some(Action::Paste));
        assert_eq!(action(&k, "ctrl+a R"), Some(Action::ReloadConfig), "defaults follow the leader");
    }

    #[test]
    fn defaults_can_be_left_out() {
        let k = keys("defaults = false\n[bind]\n\"cmd+p\" = \"paste\"").expect("valid");
        assert_eq!(k.base.len(), 1);
    }

    #[test]
    fn errors_name_the_problem() {
        let err = keys("[bind]\n\"cmd+q\" = \"explode\"").expect_err("unknown action");
        assert!(err.contains("explode") && err.contains("line 2"), "{err}");
        let err = keys("[bind]\n\"hyper+q\" = \"quit\"").expect_err("bad chord");
        assert!(err.contains("hyper"), "{err}");
        let err = keys("[bind]\n\"cmd+q\" = { action = \"send\" }").expect_err("missing text");
        assert!(err.contains("text"), "{err}");
        let err = keys("[bind]\n\"cmd+k\" = { action = \"key-table\", name = \"nope\" }").expect_err("no table");
        assert!(err.contains("nope"), "{err}");
    }
}
