//! Windows, their tabs, and the panes in them. No platform types, so it can
//! later move to its own thread or process (docs/design.md).

use std::cell::RefMut;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use super::pane::{Pane, PaneInfo, Wake};
use super::window::{Tab, Window};
use super::{PaneId, WindowId};
use crate::config::Config;
use crate::terminal::{Frame, Geometry};

/// A pane's place in its window, in cells from the top-left of the pane
/// area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub cols: u16,
    pub rows: u16,
}

/// What changed since the last pump, for the windows to show.
#[derive(Debug, Default)]
pub struct Pump {
    /// Windows whose visible panes have new output.
    pub redraw: BTreeSet<WindowId>,
    /// Windows whose tab bar or title may read differently.
    pub tabs: BTreeSet<WindowId>,
    /// Windows that lost their last pane and are gone.
    pub closed: Vec<WindowId>,
    /// Output is still queued; pump again soon.
    pub more: bool,
}

/// One tab as the tab bar shows it.
pub struct TabView<'a> {
    pub active: bool,
    pub info: &'a PaneInfo,
}

pub struct Mux {
    panes: BTreeMap<PaneId, Pane>,
    windows: BTreeMap<WindowId, Window>,
    next_id: u64,
    wake: Wake,
    buf: Vec<u8>,
}

impl Mux {
    /// `wake` runs on another thread when output is waiting; the owner must
    /// then call [`Mux::pump`] on this thread.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self { panes: BTreeMap::new(), windows: BTreeMap::new(), next_id: 0, wake: Arc::new(wake), buf: Vec::new() }
    }

    /// Opens a window with one tab running the login shell in `cwd`.
    pub fn open_window(&mut self, size: Geometry, config: &Config, cwd: Option<&Path>) -> io::Result<WindowId> {
        let pane = self.spawn(size, config, cwd)?;
        let id = WindowId(self.next_id());
        self.windows.insert(id, Window::new(Tab { pane }, size));
        Ok(id)
    }

    /// Closes a window and everything running in it.
    pub fn close_window(&mut self, window: WindowId) {
        if let Some(w) = self.windows.remove(&window) {
            for tab in w.tabs() {
                self.panes.remove(&tab.pane);
            }
        }
    }

    /// Opens a tab after the others, in the active pane's directory.
    pub fn new_tab(&mut self, window: WindowId, config: &Config) -> io::Result<()> {
        let Some(w) = self.windows.get(&window) else { return Ok(()) };
        let size = w.size;
        let cwd = self.panes.get(&w.active_tab().pane).and_then(|p| p.info().cwd.clone());
        let pane = self.spawn(size, config, cwd.as_deref())?;
        let focused = self.blur(window);
        if let Some(w) = self.windows.get_mut(&window) {
            w.push(Tab { pane });
        }
        self.refocus(window, focused);
        Ok(())
    }

    /// Closes the active tab. Returns whether the window is now empty and
    /// gone.
    pub fn close_tab(&mut self, window: WindowId) -> bool {
        let Some(w) = self.windows.get(&window) else { return false };
        let index = w.active();
        self.remove_tab(window, index)
    }

    pub fn activate_tab(&mut self, window: WindowId, index: usize) {
        let focused = self.blur(window);
        if let Some(w) = self.windows.get_mut(&window) {
            w.activate(index);
        }
        self.refocus(window, focused);
    }

    pub fn tabs(&self, window: WindowId) -> Vec<TabView<'_>> {
        let Some(w) = self.windows.get(&window) else { return Vec::new() };
        w.tabs()
            .iter()
            .enumerate()
            .filter_map(|(i, tab)| Some(TabView { active: i == w.active(), info: self.panes.get(&tab.pane)?.info() }))
            .collect()
    }

    /// The pane keys and pastes go to.
    pub fn active_pane(&self, window: WindowId) -> Option<PaneId> {
        Some(self.windows.get(&window)?.active_tab().pane)
    }

    /// Whether closing the active tab would stop something other than a
    /// shell.
    pub fn active_tab_busy(&self, window: WindowId) -> bool {
        self.active_pane(window).and_then(|id| self.panes.get(&id)).is_some_and(Pane::is_busy)
    }

    pub fn pane_mut(&mut self, id: PaneId) -> Option<&mut Pane> {
        self.panes.get_mut(&id)
    }

    /// The panes on screen in `window`, with their place and what to draw.
    pub fn visible(&mut self, window: WindowId) -> Vec<(PaneId, Rect, RefMut<'_, Frame>)> {
        let Some(w) = self.windows.get(&window) else { return Vec::new() };
        let rect = Rect { x: 0, y: 0, cols: w.size.cols, rows: w.size.rows };
        let pane = w.active_tab().pane;
        self.panes.iter_mut().filter(|(id, _)| **id == pane).map(|(id, p)| (*id, rect, p.frame())).collect()
    }

    /// Fits every tab of `window` to `size`.
    pub fn resize(&mut self, window: WindowId, size: Geometry) {
        let Some(w) = self.windows.get_mut(&window) else { return };
        w.size = size;
        for tab in w.tabs() {
            if let Some(pane) = self.panes.get_mut(&tab.pane) {
                pane.resize(size);
            }
        }
    }

    /// Tells the window's active pane whether it has the keyboard.
    pub fn focus(&mut self, window: WindowId, focused: bool) {
        let Some(w) = self.windows.get_mut(&window) else { return };
        w.focused = focused;
        let pane = w.active_tab().pane;
        if let Some(pane) = self.panes.get_mut(&pane) {
            pane.focus(focused);
        }
    }

    /// Applies a reloaded config to every terminal.
    pub fn configure(&mut self, config: &Config) {
        for pane in self.panes.values_mut() {
            pane.configure(config);
        }
    }

    /// The soonest a synchronized-output hold expires.
    pub fn hold_deadline(&self) -> Option<Instant> {
        self.panes.values().filter_map(Pane::hold_deadline).min()
    }

    /// Parses waiting output, and closes the panes whose program exited.
    pub fn pump(&mut self) -> Pump {
        let mut pump = Pump::default();
        let mut closed = Vec::new();
        for (&id, pane) in &mut self.panes {
            let pumped = pane.pump(&mut self.buf);
            pump.more |= pumped.more;
            let Some((window, w)) = self.windows.iter().find(|(_, w)| w.position(id).is_some()) else { continue };
            if pumped.output && w.active_tab().pane == id {
                pump.redraw.insert(*window);
            }
            if pumped.info_changed {
                pump.tabs.insert(*window);
            }
            if pumped.closed {
                closed.push((*window, id));
            }
        }
        for (window, pane) in closed {
            let Some(index) = self.windows.get(&window).and_then(|w| w.position(pane)) else { continue };
            pump.redraw.insert(window);
            pump.tabs.insert(window);
            if self.remove_tab(window, index) {
                pump.closed.push(window);
            }
        }
        pump
    }

    fn remove_tab(&mut self, window: WindowId, index: usize) -> bool {
        let focused = self.blur(window);
        let Some(w) = self.windows.get_mut(&window) else { return false };
        let (tab, empty) = w.remove(index);
        self.panes.remove(&tab.pane);
        if empty {
            self.windows.remove(&window);
        } else {
            self.refocus(window, focused);
        }
        empty
    }

    /// Takes focus from the active pane before it changes. Returns whether
    /// the window had it.
    fn blur(&mut self, window: WindowId) -> bool {
        let focused = self.windows.get(&window).is_some_and(|w| w.focused);
        if focused {
            self.focus(window, false);
        }
        focused
    }

    fn refocus(&mut self, window: WindowId, focused: bool) {
        if focused {
            self.focus(window, true);
        }
    }

    fn spawn(&mut self, size: Geometry, config: &Config, cwd: Option<&Path>) -> io::Result<PaneId> {
        let pane = Pane::spawn(size, config, cwd, &self.wake)?;
        let id = PaneId(self.next_id());
        self.panes.insert(id, pane);
        Ok(id)
    }

    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
}
