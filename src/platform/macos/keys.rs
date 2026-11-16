//! macOS key codes and modifier flags in libghostty's terms.

use libghostty_vt::key::{Key, Mods};
use objc2_app_kit::NSEventModifierFlags;

// Device-dependent bits of NSEventModifierFlags (IOKit's NX_DEVICE*KEYMASK)
// that tell the right-hand modifier keys apart.
const RIGHT_SHIFT: usize = 0x0004;
const RIGHT_CONTROL: usize = 0x2000;
const LEFT_OPTION: usize = 0x0020;
const RIGHT_OPTION: usize = 0x0040;
const RIGHT_COMMAND: usize = 0x0010;

pub fn mods(flags: NSEventModifierFlags) -> Mods {
    let raw = flags.0;
    let mut mods = Mods::empty();
    for (flag, side, mod_, side_mod) in [
        (NSEventModifierFlags::Shift, RIGHT_SHIFT, Mods::SHIFT, Mods::SHIFT_SIDE),
        (NSEventModifierFlags::Control, RIGHT_CONTROL, Mods::CTRL, Mods::CTRL_SIDE),
        (NSEventModifierFlags::Option, RIGHT_OPTION, Mods::ALT, Mods::ALT_SIDE),
        (NSEventModifierFlags::Command, RIGHT_COMMAND, Mods::SUPER, Mods::SUPER_SIDE),
    ] {
        if flags.contains(flag) {
            mods |= mod_;
            if raw & side != 0 {
                mods |= side_mod;
            }
        }
    }
    if flags.contains(NSEventModifierFlags::CapsLock) {
        mods |= Mods::CAPS_LOCK;
    }
    mods
}

/// Which Option keys are down, as (left, right).
pub fn option_sides(flags: NSEventModifierFlags) -> (bool, bool) {
    if !flags.contains(NSEventModifierFlags::Option) {
        return (false, false);
    }
    let (left, right) = (flags.0 & LEFT_OPTION != 0, flags.0 & RIGHT_OPTION != 0);
    // Synthesised events may carry no side bits; treat them as left.
    if left || right { (left, right) } else { (true, false) }
}

/// The physical key for a macOS virtual key code (`kVK_*`).
pub fn key(code: u16) -> Key {
    match code {
        0x00 => Key::A,
        0x01 => Key::S,
        0x02 => Key::D,
        0x03 => Key::F,
        0x04 => Key::H,
        0x05 => Key::G,
        0x06 => Key::Z,
        0x07 => Key::X,
        0x08 => Key::C,
        0x09 => Key::V,
        0x0A => Key::IntlBackslash,
        0x0B => Key::B,
        0x0C => Key::Q,
        0x0D => Key::W,
        0x0E => Key::E,
        0x0F => Key::R,
        0x10 => Key::Y,
        0x11 => Key::T,
        0x12 => Key::Digit1,
        0x13 => Key::Digit2,
        0x14 => Key::Digit3,
        0x15 => Key::Digit4,
        0x16 => Key::Digit6,
        0x17 => Key::Digit5,
        0x18 => Key::Equal,
        0x19 => Key::Digit9,
        0x1A => Key::Digit7,
        0x1B => Key::Minus,
        0x1C => Key::Digit8,
        0x1D => Key::Digit0,
        0x1E => Key::BracketRight,
        0x1F => Key::O,
        0x20 => Key::U,
        0x21 => Key::BracketLeft,
        0x22 => Key::I,
        0x23 => Key::P,
        0x24 => Key::Enter,
        0x25 => Key::L,
        0x26 => Key::J,
        0x27 => Key::Quote,
        0x28 => Key::K,
        0x29 => Key::Semicolon,
        0x2A => Key::Backslash,
        0x2B => Key::Comma,
        0x2C => Key::Slash,
        0x2D => Key::N,
        0x2E => Key::M,
        0x2F => Key::Period,
        0x30 => Key::Tab,
        0x31 => Key::Space,
        0x32 => Key::Backquote,
        0x33 => Key::Backspace,
        0x35 => Key::Escape,
        0x36 => Key::MetaRight,
        0x37 => Key::MetaLeft,
        0x38 => Key::ShiftLeft,
        0x39 => Key::CapsLock,
        0x3A => Key::AltLeft,
        0x3B => Key::ControlLeft,
        0x3C => Key::ShiftRight,
        0x3D => Key::AltRight,
        0x3E => Key::ControlRight,
        0x3F => Key::Fn,
        0x40 => Key::F17,
        0x41 => Key::NumpadDecimal,
        0x43 => Key::NumpadMultiply,
        0x45 => Key::NumpadAdd,
        0x47 => Key::NumLock,
        0x48 => Key::AudioVolumeUp,
        0x49 => Key::AudioVolumeDown,
        0x4A => Key::AudioVolumeMute,
        0x4B => Key::NumpadDivide,
        0x4C => Key::NumpadEnter,
        0x4E => Key::NumpadSubtract,
        0x4F => Key::F18,
        0x50 => Key::F19,
        0x51 => Key::NumpadEqual,
        0x52 => Key::Numpad0,
        0x53 => Key::Numpad1,
        0x54 => Key::Numpad2,
        0x55 => Key::Numpad3,
        0x56 => Key::Numpad4,
        0x57 => Key::Numpad5,
        0x58 => Key::Numpad6,
        0x59 => Key::Numpad7,
        0x5A => Key::F20,
        0x5B => Key::Numpad8,
        0x5C => Key::Numpad9,
        0x5D => Key::IntlYen,
        0x5E => Key::IntlRo,
        0x5F => Key::NumpadComma,
        0x60 => Key::F5,
        0x61 => Key::F6,
        0x62 => Key::F7,
        0x63 => Key::F3,
        0x64 => Key::F8,
        0x65 => Key::F9,
        0x67 => Key::F11,
        0x69 => Key::F13,
        0x6A => Key::F16,
        0x6B => Key::F14,
        0x6D => Key::F10,
        0x6E => Key::ContextMenu,
        0x6F => Key::F12,
        0x71 => Key::F15,
        0x72 => Key::Insert,
        0x73 => Key::Home,
        0x74 => Key::PageUp,
        0x75 => Key::Delete,
        0x76 => Key::F4,
        0x77 => Key::End,
        0x78 => Key::F2,
        0x79 => Key::PageDown,
        0x7A => Key::F1,
        0x7B => Key::ArrowLeft,
        0x7C => Key::ArrowRight,
        0x7D => Key::ArrowDown,
        0x7E => Key::ArrowUp,
        _ => Key::Unidentified,
    }
}

/// Text AppKit reports for keys that type nothing: control characters and
/// the private-use codes it gives function keys.
pub fn is_text(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| !c.is_control() && !('\u{F700}'..='\u{F8FF}').contains(&c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn right_option_sets_the_side() {
        let flags = NSEventModifierFlags(NSEventModifierFlags::Option.0 | RIGHT_OPTION);
        assert_eq!(mods(flags), Mods::ALT | Mods::ALT_SIDE);
        assert_eq!(option_sides(flags), (false, true));
        let left = NSEventModifierFlags(NSEventModifierFlags::Option.0 | LEFT_OPTION);
        assert_eq!(mods(left), Mods::ALT);
        assert_eq!(option_sides(left), (true, false));
    }

    #[test]
    fn function_key_codes_are_not_text() {
        assert!(!is_text("\u{F700}"));
        assert!(!is_text("\u{1b}"));
        assert!(is_text("é"));
    }
}
