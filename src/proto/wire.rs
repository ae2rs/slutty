//! Serde for the libghostty types that cross the socket, for use with
//! `#[serde(with = "...")]`. C enums travel as their integer value; one the
//! other side does not know is an error, not a guess.

use libghostty_vt::color::Rgb;
use libghostty_vt::key::Mods;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub mod rgb {
    use super::*;

    pub fn serialize<S: Serializer>(c: &Rgb, s: S) -> Result<S::Ok, S::Error> {
        [c.r, c.g, c.b].serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Rgb, D::Error> {
        let [r, g, b] = <[u8; 3]>::deserialize(d)?;
        Ok(Rgb { r, g, b })
    }
}

pub mod opt_rgb {
    use super::*;

    pub fn serialize<S: Serializer>(c: &Option<Rgb>, s: S) -> Result<S::Ok, S::Error> {
        c.map(|c| [c.r, c.g, c.b]).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Rgb>, D::Error> {
        Ok(Option::<[u8; 3]>::deserialize(d)?.map(|[r, g, b]| Rgb { r, g, b }))
    }
}

pub mod mods {
    use super::*;

    pub fn serialize<S: Serializer>(m: &Mods, s: S) -> Result<S::Ok, S::Error> {
        m.bits().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Mods, D::Error> {
        Ok(Mods::from_bits_truncate(u16::deserialize(d)?))
    }
}

/// A module of serde functions for a libghostty C enum.
macro_rules! c_enum {
    ($name:ident, $ty:ty) => {
        pub mod $name {
            use super::*;

            pub fn serialize<S: Serializer>(v: &$ty, s: S) -> Result<S::Ok, S::Error> {
                i32::from(*v).serialize(s)
            }

            pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<$ty, D::Error> {
                let raw = i32::deserialize(d)?;
                <$ty>::try_from(raw).map_err(|_| D::Error::custom(format!("unknown {} {raw}", stringify!($name))))
            }
        }
    };
}

c_enum!(key, libghostty_vt::key::Key);
c_enum!(key_action, libghostty_vt::key::Action);
c_enum!(mouse_action, libghostty_vt::mouse::Action);

pub mod opt_mouse_button {
    use super::*;
    use libghostty_vt::mouse::Button;

    pub fn serialize<S: Serializer>(b: &Option<Button>, s: S) -> Result<S::Ok, S::Error> {
        b.map(i32::from).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Button>, D::Error> {
        Option::<i32>::deserialize(d)?
            .map(|raw| Button::try_from(raw).map_err(|_| D::Error::custom(format!("unknown mouse button {raw}"))))
            .transpose()
    }
}
