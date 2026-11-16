//! What animates and when it next changes. Pure timing with no AppKit, so
//! the platform layer can keep one timer for every window.

use std::time::{Duration, Instant};

/// rho's working spinner: braille cells using dots 1 to 6 only, so it keeps
/// to the cell's upper three rows of dots like the other marks.
pub const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// How long the spinner shows each frame.
const SPINNER_FRAME: Duration = Duration::from_millis(100);

/// What animates in one window now: its live zones.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Live {
    /// rho's spinner shows in the tab bar of a window on screen.
    pub spinner: bool,
}

/// What the zones show at one moment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Phase {
    /// The spinner's frame, an index into [`SPINNER`].
    pub spinner: usize,
}

/// Time for every animation, counted from one epoch so that zones in
/// different windows change on the same ticks.
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    epoch: Instant,
}

impl Clock {
    pub fn new(epoch: Instant) -> Self {
        Self { epoch }
    }

    pub fn phase(&self, now: Instant) -> Phase {
        Phase { spinner: (self.steps(now, SPINNER_FRAME) % SPINNER.len() as u128) as usize }
    }

    /// When the next live zone changes, or `None` when none is live.
    pub fn next_change(&self, live: Live, now: Instant) -> Option<Instant> {
        live.spinner.then(|| self.next_step(now, SPINNER_FRAME))
    }

    /// Whole periods of `period` since the epoch.
    fn steps(&self, now: Instant, period: Duration) -> u128 {
        now.saturating_duration_since(self.epoch).as_nanos() / period.as_nanos()
    }

    /// The start of the next period of `period` after `now`.
    fn next_step(&self, now: Instant, period: Duration) -> Instant {
        let steps = u32::try_from(self.steps(now, period) + 1).unwrap_or(u32::MAX);
        self.epoch + period * steps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn spinner_frames_use_six_dots_and_differ() {
        for (i, c) in SPINNER.iter().enumerate() {
            // Dots 7 and 8 are the pattern's two high bits.
            let dots = u32::from(*c) - 0x2800;
            assert!(dots > 0 && dots < 0x40, "{c} uses dots 1 to 6 only");
            assert!(!SPINNER[..i].contains(c), "{c} appears once");
        }
    }

    #[test]
    fn the_spinner_turns_every_hundred_milliseconds_and_wraps() {
        let epoch = Instant::now();
        let clock = Clock::new(epoch);
        assert_eq!(clock.phase(epoch).spinner, 0);
        assert_eq!(clock.phase(epoch + 99 * MS).spinner, 0);
        assert_eq!(clock.phase(epoch + 100 * MS).spinner, 1);
        assert_eq!(clock.phase(epoch + 1_050 * MS).spinner, 0, "ten frames wrap");
    }

    #[test]
    fn nothing_live_needs_no_timer() {
        let epoch = Instant::now();
        assert_eq!(Clock::new(epoch).next_change(Live::default(), epoch + 5 * MS), None);
    }

    #[test]
    fn a_live_spinner_wakes_on_its_next_frame() {
        let epoch = Instant::now();
        let clock = Clock::new(epoch);
        let live = Live { spinner: true };
        assert_eq!(clock.next_change(live, epoch + 130 * MS), Some(epoch + 200 * MS));
        assert_eq!(clock.next_change(live, epoch + 200 * MS), Some(epoch + 300 * MS));
    }
}
