//! Names for panes and windows that stay valid while they live.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PaneId(pub(super) u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WindowId(pub(super) u64);

/// As text, for naming an id outside the process, such as in a
/// notification or `slutty cli`. One read back may have closed since.
macro_rules! as_text {
    ($id:ident) => {
        impl std::fmt::Display for $id {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl std::str::FromStr for $id {
            type Err = std::num::ParseIntError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                s.parse().map(Self)
            }
        }
    };
}

as_text!(PaneId);
as_text!(WindowId);

#[cfg(test)]
impl PaneId {
    pub fn for_test(id: u64) -> Self {
        Self(id)
    }
}

#[cfg(test)]
impl WindowId {
    pub fn for_test(id: u64) -> Self {
        Self(id)
    }
}
