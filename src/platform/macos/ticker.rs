//! The one timer behind every animation. It is armed only while some window
//! has a live zone, for the moment the next one changes, so nothing runs
//! while nothing animates.

use std::cell::RefCell;
use std::time::Instant;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2_foundation::{NSRunLoop, NSRunLoopCommonModes, NSTimer};

use super::state;
use crate::animation::{Clock, Phase};

struct Ticker {
    clock: Clock,
    /// One-shot; fired or invalidated before another is made.
    timer: Option<Retained<NSTimer>>,
    due: Option<Instant>,
}

thread_local! {
    static TICKER: RefCell<Ticker> =
        RefCell::new(Ticker { clock: Clock::new(Instant::now()), timer: None, due: None });
}

/// What every zone shows at `now`.
pub fn phase(now: Instant) -> Phase {
    TICKER.with(|t| t.borrow().clock.phase(now))
}

/// Arms the timer for the next change of any window's live zones, or stops
/// it when none is live. Call after a window's zones may have changed.
pub fn reschedule() {
    let now = Instant::now();
    let clock = TICKER.with(|t| t.borrow().clock);
    let due = state::views().iter().filter_map(|view| clock.next_change(view.live(), now)).min();
    TICKER.with(|t| t.borrow_mut().arm(due, now));
}

impl Ticker {
    fn arm(&mut self, due: Option<Instant>, now: Instant) {
        if self.due == due && self.timer.is_some() {
            return;
        }
        if let Some(timer) = self.timer.take() {
            timer.invalidate();
        }
        self.due = due;
        let Some(due) = due else {
            log::trace!("animations stopped");
            return;
        };
        let interval = due.saturating_duration_since(now).as_secs_f64();
        let block = RcBlock::new(|_| tick());
        // SAFETY: the block captures nothing, and the timer fires on the main
        // run loop it is added to.
        let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(interval, false, &block) };
        // Lets macOS fire it with other wake-ups; a frame a little late is
        // never seen.
        timer.setTolerance(interval * 0.1);
        // SAFETY: the main run loop and a valid mode, so it fires during
        // resizes and menus too.
        unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
        self.timer = Some(timer);
    }
}

fn tick() {
    TICKER.with(|t| {
        let mut t = t.borrow_mut();
        t.timer = None;
        t.due = None;
    });
    log::trace!("tick");
    let now = Instant::now();
    for view in state::views() {
        view.animate(now);
    }
    reschedule();
}
