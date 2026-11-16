//! What animates and when it next changes. Pure timing with no AppKit, so
//! the platform layer can keep one timer for every window.

use std::time::{Duration, Instant};

/// rho's working spinner: braille cells using dots 1 to 6 only, so it keeps
/// to the cell's upper three rows of dots like the other marks.
pub const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// How long the spinner shows each frame.
const SPINNER_FRAME: Duration = Duration::from_millis(100);

/// How long a blinking cursor stays on, then off.
const CURSOR_BLINK: Duration = Duration::from_millis(800);

/// How long after input a cursor keeps blinking before it holds still.
const CURSOR_CUTOFF: Duration = Duration::from_secs(5);

/// How long blinking text stays on, then off.
const TEXT_BLINK: Duration = Duration::from_millis(500);

/// What animates in one window now: its live zones.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Live {
    /// rho's spinner shows in the tab bar of a window on screen.
    pub spinner: bool,
    /// The cursor of a key window on screen blinks, its phase starting at
    /// this input.
    pub cursor: Option<Instant>,
    /// A key window on screen shows text that blinks.
    pub text: bool,
}

/// What the zones show at one moment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Phase {
    /// The spinner's frame, an index into [`SPINNER`].
    pub spinner: usize,
    /// A blinking cursor is in its visible half; a steady one always is.
    pub cursor: bool,
    /// Blinking text is in its visible half.
    pub text: bool,
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

    pub fn phase(&self, live: Live, now: Instant) -> Phase {
        Phase {
            spinner: (steps(self.epoch, now, SPINNER_FRAME) % SPINNER.len() as u128) as usize,
            cursor: live.cursor.is_none_or(|since| cursor_on(since, now)),
            text: !live.text || steps(self.epoch, now, TEXT_BLINK).is_multiple_of(2),
        }
    }

    /// When the next live zone changes, or `None` when none will.
    pub fn next_change(&self, live: Live, now: Instant) -> Option<Instant> {
        let spinner = live.spinner.then(|| next_step(self.epoch, now, SPINNER_FRAME));
        let cursor = live.cursor.and_then(|since| next_cursor_change(since, now));
        let text = live.text.then(|| next_step(self.epoch, now, TEXT_BLINK));
        [spinner, cursor, text].into_iter().flatten().min()
    }
}

/// Whether a cursor blinking since `since` shows at `now`: on first, then
/// off, and on for good once the cut-off passes.
fn cursor_on(since: Instant, now: Instant) -> bool {
    now.saturating_duration_since(since) >= CURSOR_CUTOFF || steps(since, now, CURSOR_BLINK).is_multiple_of(2)
}

fn next_cursor_change(since: Instant, now: Instant) -> Option<Instant> {
    let end = since + CURSOR_CUTOFF;
    if now >= end {
        return None;
    }
    let next = next_step(since, now, CURSOR_BLINK);
    if next < end {
        Some(next)
    } else {
        // The last half before the cut-off: only an off cursor still changes.
        (!cursor_on(since, now)).then_some(end)
    }
}

/// Whole periods of `period` from `epoch` to `now`.
fn steps(epoch: Instant, now: Instant, period: Duration) -> u128 {
    now.saturating_duration_since(epoch).as_nanos() / period.as_nanos()
}

/// The start of the next period of `period` from `epoch` after `now`.
fn next_step(epoch: Instant, now: Instant, period: Duration) -> Instant {
    let steps = u32::try_from(steps(epoch, now, period) + 1).unwrap_or(u32::MAX);
    epoch + period * steps
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
        let at = |ms: u32| clock.phase(Live::default(), epoch + ms * MS).spinner;
        assert_eq!((at(0), at(99), at(100)), (0, 0, 1));
        assert_eq!(at(1_050), 0, "ten frames wrap");
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
        let live = Live { spinner: true, ..Live::default() };
        assert_eq!(clock.next_change(live, epoch + 130 * MS), Some(epoch + 200 * MS));
        assert_eq!(clock.next_change(live, epoch + 200 * MS), Some(epoch + 300 * MS));
    }

    #[test]
    fn the_cursor_blinks_from_the_last_input_then_holds_on() {
        let epoch = Instant::now();
        let clock = Clock::new(epoch);
        let input = epoch + 333 * MS;
        let live = Live { cursor: Some(input), ..Live::default() };
        let on = |ms: u32| clock.phase(live, input + ms * MS).cursor;
        assert!(on(0) && on(799) && !on(800) && !on(1_599) && on(1_600));
        assert!(!on(4_000) && on(4_800) && on(5_200) && on(60_000), "on for good after the cut-off");
        assert!(clock.phase(Live::default(), input).cursor, "a steady cursor shows");

        let next = |ms: u32| clock.next_change(live, input + ms * MS);
        assert_eq!(next(10), Some(input + 800 * MS), "in phase with the input, not the epoch");
        assert_eq!(next(4_100), Some(input + 4_800 * MS));
        assert_eq!(next(4_800), None, "no wake-ups once it holds");
    }

    #[test]
    fn text_blinks_every_half_second_in_step_across_windows() {
        let epoch = Instant::now();
        let clock = Clock::new(epoch);
        let live = Live { text: true, ..Live::default() };
        let on = |ms: u32| clock.phase(live, epoch + ms * MS).text;
        assert!(on(0) && on(499) && !on(500) && on(1_000));
        assert!(clock.phase(Live::default(), epoch + 700 * MS).text, "shown while not live");
        assert_eq!(clock.next_change(live, epoch + 620 * MS), Some(epoch + 1_000 * MS));
    }

    #[test]
    fn the_timer_wakes_for_the_soonest_zone() {
        let epoch = Instant::now();
        let clock = Clock::new(epoch);
        let live = Live { spinner: true, cursor: Some(epoch + 50 * MS), text: false };
        assert_eq!(clock.next_change(live, epoch + 820 * MS), Some(epoch + 850 * MS));
        assert_eq!(clock.next_change(live, epoch + 860 * MS), Some(epoch + 900 * MS));
    }
}
