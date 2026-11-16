//! What animates and when it next changes. Pure timing with no AppKit, so
//! the platform layer can keep one timer for every window.

/// rho's working spinner: braille cells using dots 1 to 6 only, so it keeps
/// to the cell's upper three rows of dots like the other marks.
pub const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spinner_frames_use_six_dots_and_differ() {
        for (i, c) in SPINNER.iter().enumerate() {
            // Dots 7 and 8 are the pattern's two high bits.
            let dots = u32::from(*c) - 0x2800;
            assert!(dots > 0 && dots < 0x40, "{c} uses dots 1 to 6 only");
            assert!(!SPINNER[..i].contains(c), "{c} appears once");
        }
    }
}
