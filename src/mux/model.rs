//! Windows, their tabs, and the panes in them. No platform types, so it can
//! later move to its own thread or process (docs/design.md).

use std::cell::RefMut;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use super::pane::{Pane, PaneInfo, Wake};
use super::tree::{Axis, Direction, Layout, Rect};
use super::window::{Tab, Window};
use super::{PaneId, WindowId};
use crate::config::Config;
use crate::terminal::{Frame, Geometry, Notification};

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
    /// Text a program put on the clipboard (OSC 52); the last one wins.
    pub clipboard: Option<String>,
    pub notifications: Vec<(PaneId, Notification)>,
}

/// One tab as the tab bar shows it.
pub struct TabView<'a> {
    pub active: bool,
    pub info: &'a PaneInfo,
}

/// The panes on screen in a window, with their place in cells and what to
/// draw, and the dividers between them.
pub struct Visible<'a> {
    pub panes: Vec<(PaneId, Rect, RefMut<'a, Frame>)>,
    pub dividers: Vec<(Axis, Rect)>,
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
        self.windows.insert(id, Window::new(Tab::new(pane), size));
        Ok(id)
    }

    /// Closes a window and everything running in it.
    pub fn close_window(&mut self, window: WindowId) {
        if let Some(w) = self.windows.remove(&window) {
            for pane in w.tabs().iter().flat_map(|tab| tab.tree.panes()) {
                self.panes.remove(&pane);
            }
        }
    }

    /// Opens a tab after the others, in the active pane's directory.
    pub fn new_tab(&mut self, window: WindowId, config: &Config) -> io::Result<()> {
        let Some(w) = self.windows.get(&window) else { return Ok(()) };
        let size = w.size;
        let cwd = self.cwd(w.active_tab().active);
        let pane = self.spawn(size, config, cwd.as_deref())?;
        self.change_focus(window, |w| w.push(Tab::new(pane)));
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
        self.change_focus(window, |w| {
            w.activate(index);
        });
    }

    pub fn tabs(&self, window: WindowId) -> Vec<TabView<'_>> {
        let Some(w) = self.windows.get(&window) else { return Vec::new() };
        w.tabs()
            .iter()
            .enumerate()
            .filter_map(|(i, tab)| Some(TabView { active: i == w.active(), info: self.panes.get(&tab.active)?.info() }))
            .collect()
    }

    /// Splits the active pane, starting a shell in its directory in the new
    /// half, which becomes active.
    pub fn split(&mut self, window: WindowId, axis: Axis, config: &Config) -> io::Result<()> {
        let Some(w) = self.windows.get(&window) else { return Ok(()) };
        let active = w.active_tab().active;
        let cwd = self.cwd(active);
        let pane = self.spawn(w.size, config, cwd.as_deref())?;
        self.change_focus(window, |w| {
            let tab = w.active_tab_mut();
            tab.zoomed = false;
            tab.tree.split(active, pane, axis);
            tab.active = pane;
        });
        self.fit_tab(window, None);
        Ok(())
    }

    /// Closes the active pane, and its tab with it when it was the last.
    /// Returns whether the window is now empty and gone.
    pub fn close_pane(&mut self, window: WindowId) -> bool {
        match self.active_pane(window) {
            Some(pane) => self.remove_pane(window, pane),
            None => false,
        }
    }

    /// Moves the active pane into a new window of the same size, or its tab
    /// when it is the tab's only pane. Returns the new window; `None` when
    /// the pane is alone in its window.
    pub fn pop_pane(&mut self, window: WindowId) -> Option<WindowId> {
        let w = self.windows.get(&window)?;
        let (index, pane, size) = (w.active(), w.active_tab().active, w.size);
        let mut detached = false;
        self.change_focus(window, |w| {
            let tab = w.tab_mut(index);
            if let Some(next) = tab.tree.remove(pane) {
                detached = true;
                tab.zoomed = false;
                tab.active = next;
            }
        });
        if !detached {
            return self.pop_tab(window);
        }
        self.fit_tab(window, Some(index));
        Some(self.adopt(size, Tab::new(pane)))
    }

    /// Moves the active tab into a new window of the same size. Returns the
    /// new window; `None` when the tab is alone in its window.
    pub fn pop_tab(&mut self, window: WindowId) -> Option<WindowId> {
        let w = self.windows.get(&window)?;
        if w.tabs().len() < 2 {
            return None;
        }
        let (index, size) = (w.active(), w.size);
        let mut removed = None;
        self.change_focus(window, |w| removed = Some(w.remove(index).0));
        Some(self.adopt(size, removed?))
    }

    /// Shows `pane`: its tab becomes active, and it the active pane, out of
    /// a zoom that hid it. Returns its window.
    pub fn reveal(&mut self, pane: PaneId) -> Option<WindowId> {
        let (&window, w) = self.windows.iter().find(|(_, w)| w.position(pane).is_some())?;
        let index = w.position(pane)?;
        self.change_focus(window, |w| {
            w.activate(index);
            let tab = w.active_tab_mut();
            tab.zoomed &= tab.active == pane;
            tab.active = pane;
        });
        self.fit_tab(window, None);
        Some(window)
    }

    /// The directory of the window's active pane.
    pub fn active_cwd(&self, window: WindowId) -> Option<std::path::PathBuf> {
        self.cwd(self.active_pane(window)?)
    }

    /// Makes `pane` active, if it is in the window's active tab.
    pub fn activate_pane(&mut self, window: WindowId, pane: PaneId) {
        self.change_focus(window, |w| {
            let tab = w.active_tab_mut();
            if tab.tree.contains(pane) && tab.active != pane {
                tab.active = pane;
                tab.zoomed = false;
            }
        });
        self.fit_tab(window, None);
    }

    /// Makes the pane next to the active one in `direction` active.
    pub fn focus_direction(&mut self, window: WindowId, direction: Direction) {
        let Some(w) = self.windows.get(&window) else { return };
        let tab = w.active_tab();
        if let Some(pane) = tab.tree.neighbour(w.area(), tab.active, direction) {
            self.activate_pane(window, pane);
        }
    }

    /// Moves the divider nearest the active pane by `cells`.
    pub fn resize_pane(&mut self, window: WindowId, direction: Direction, cells: u16) {
        let Some(w) = self.windows.get_mut(&window) else { return };
        let area = w.area();
        let tab = w.active_tab_mut();
        if tab.zoomed {
            return;
        }
        let active = tab.active;
        tab.tree.resize(area, active, direction, cells);
        self.fit_tab(window, None);
    }

    pub fn toggle_zoom(&mut self, window: WindowId) {
        let Some(w) = self.windows.get_mut(&window) else { return };
        let tab = w.active_tab_mut();
        tab.zoomed = !tab.zoomed && tab.tree.panes().len() > 1;
        self.fit_tab(window, None);
    }

    pub fn rotate_panes(&mut self, window: WindowId) {
        let Some(w) = self.windows.get_mut(&window) else { return };
        w.active_tab_mut().tree.rotate_clockwise();
        self.fit_tab(window, None);
    }

    /// The pane keys and pastes go to.
    pub fn active_pane(&self, window: WindowId) -> Option<PaneId> {
        Some(self.windows.get(&window)?.active_tab().active)
    }

    /// Whether closing the active pane would stop something other than a
    /// shell.
    pub fn active_pane_busy(&self, window: WindowId) -> bool {
        self.active_pane(window).and_then(|id| self.panes.get(&id)).is_some_and(Pane::is_busy)
    }

    /// Whether closing the active tab would stop something other than a
    /// shell in any of its panes.
    pub fn active_tab_busy(&self, window: WindowId) -> bool {
        let Some(w) = self.windows.get(&window) else { return false };
        w.active_tab().tree.panes().iter().filter_map(|id| self.panes.get(id)).any(Pane::is_busy)
    }

    pub fn pane_mut(&mut self, id: PaneId) -> Option<&mut Pane> {
        self.panes.get_mut(&id)
    }

    /// Where the panes of the window's active tab are, in cells.
    pub fn layout(&self, window: WindowId) -> Layout {
        self.windows.get(&window).map(|w| w.active_tab().layout(w.area())).unwrap_or_default()
    }

    pub fn visible(&mut self, window: WindowId) -> Visible<'_> {
        let Layout { panes, dividers } = self.layout(window);
        let mut frames: BTreeMap<PaneId, RefMut<'_, Frame>> = self
            .panes
            .iter_mut()
            .filter(|(id, _)| panes.iter().any(|(p, _)| p == *id))
            .map(|(id, pane)| (*id, pane.frame()))
            .collect();
        let panes = panes.into_iter().filter_map(|(id, rect)| Some((id, rect, frames.remove(&id)?))).collect();
        Visible { panes, dividers }
    }

    /// Fits every tab of `window` to `size`.
    pub fn resize(&mut self, window: WindowId, size: Geometry) {
        let Some(w) = self.windows.get_mut(&window) else { return };
        w.size = size;
        for index in 0..w.tabs().len() {
            self.fit_tab(window, Some(index));
        }
    }

    /// Tells the window's active pane whether it has the keyboard.
    pub fn focus(&mut self, window: WindowId, focused: bool) {
        let Some(w) = self.windows.get_mut(&window) else { return };
        w.focused = focused;
        let pane = w.active_tab().active;
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
            if pumped.clipboard.is_some() {
                pump.clipboard = pumped.clipboard;
            }
            pump.notifications.extend(pumped.notifications.into_iter().map(|n| (id, n)));
            let Some((window, w)) = self.windows.iter().find(|(_, w)| w.position(id).is_some()) else { continue };
            if pumped.output && w.active_tab().tree.contains(id) {
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
            pump.redraw.insert(window);
            pump.tabs.insert(window);
            if self.remove_pane(window, pane) {
                pump.closed.push(window);
            }
        }
        pump
    }

    /// Returns whether the window is now empty and gone.
    fn remove_pane(&mut self, window: WindowId, pane: PaneId) -> bool {
        let Some(index) = self.windows.get(&window).and_then(|w| w.position(pane)) else { return false };
        let mut removed = false;
        self.change_focus(window, |w| {
            let tab = w.tab_mut(index);
            if let Some(next) = tab.tree.remove(pane) {
                removed = true;
                tab.zoomed = false;
                if tab.active == pane {
                    tab.active = next;
                }
            }
        });
        if !removed {
            return self.remove_tab(window, index);
        }
        self.panes.remove(&pane);
        self.fit_tab(window, Some(index));
        false
    }

    fn remove_tab(&mut self, window: WindowId, index: usize) -> bool {
        let mut removed = None;
        self.change_focus(window, |w| removed = Some(w.remove(index)));
        let Some((tab, empty)) = removed else { return false };
        for pane in tab.tree.panes() {
            self.panes.remove(&pane);
        }
        if empty {
            self.windows.remove(&window);
        }
        empty
    }

    /// Changes which pane is active through `f`, moving the keyboard focus
    /// along when the window has it.
    fn change_focus(&mut self, window: WindowId, f: impl FnOnce(&mut Window)) {
        let Some(w) = self.windows.get(&window) else { return };
        let focused = w.focused;
        if focused {
            self.focus(window, false);
        }
        let Some(w) = self.windows.get_mut(&window) else { return };
        f(w);
        if focused && !w.tabs().is_empty() {
            self.focus(window, true);
        }
    }

    /// Fits the panes of tab `index`, or of the active tab, to their place.
    fn fit_tab(&mut self, window: WindowId, index: Option<usize>) {
        let Some(w) = self.windows.get(&window) else { return };
        let tab = &w.tabs()[index.unwrap_or(w.active())];
        let size = w.size;
        // A zoomed tab's hidden panes keep their size.
        for (id, rect) in tab.layout(w.area()).panes {
            if let Some(pane) = self.panes.get_mut(&id) {
                pane.resize(Geometry { cols: rect.cols.max(1), rows: rect.rows.max(1), ..size });
            }
        }
    }

    fn cwd(&self, pane: PaneId) -> Option<std::path::PathBuf> {
        self.panes.get(&pane).and_then(|p| p.info().cwd.clone())
    }

    /// A new window holding `tab`.
    fn adopt(&mut self, size: Geometry, tab: Tab) -> WindowId {
        let id = WindowId(self.next_id());
        self.windows.insert(id, Window::new(tab, size));
        self.fit_tab(id, None);
        id
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
