//! Windows, their tabs, and the panes in them. No platform types, so it can
//! run in the mux server (docs/design.md).

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use super::pane::{Pane, PaneInfo, Wake};
use super::tree::{Axis, Direction, Layout};
use super::window::{Tab, Window};
use super::{PaneId, WindowId};
use crate::config::Config;
use crate::pty::Spawn;
use crate::terminal::{Geometry, Notification};

/// What a pump found that the mux does not keep.
#[derive(Debug, Default)]
pub struct Pump {
    /// Output is still queued; pump again soon.
    pub more: bool,
    /// Text a program put on the clipboard (OSC 52); the last one wins.
    pub clipboard: Option<String>,
    pub notifications: Vec<(PaneId, Notification)>,
}

/// One tab as the tab bar shows it.
pub struct TabView<'a> {
    /// The tab's active pane.
    pub info: &'a PaneInfo,
    pub panes: Vec<PaneId>,
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

    pub fn window_ids(&self) -> Vec<WindowId> {
        self.windows.keys().copied().collect()
    }

    /// Opens a window with one tab running `spawn`.
    pub fn open_window(&mut self, size: Geometry, config: &Config, spawn: Spawn) -> io::Result<(WindowId, PaneId)> {
        let pane = self.spawn(size, config, spawn)?;
        let id = WindowId(self.next_id());
        self.windows.insert(id, Window::new(Tab::new(pane), size));
        Ok((id, pane))
    }

    /// Closes a window and everything running in it.
    pub fn close_window(&mut self, window: WindowId) {
        if let Some(w) = self.windows.remove(&window) {
            for pane in w.tabs().iter().flat_map(|tab| tab.tree.panes()) {
                self.panes.remove(&pane);
            }
        }
    }

    /// Opens a tab after the others running `spawn`.
    pub fn new_tab(&mut self, window: WindowId, config: &Config, spawn: Spawn) -> io::Result<PaneId> {
        let Some(w) = self.windows.get(&window) else { return Err(missing("window", window.0)) };
        let pane = self.spawn(w.size, config, spawn)?;
        self.change_focus(window, |w| w.push(Tab::new(pane)));
        Ok(pane)
    }

    /// Closes the active tab.
    pub fn close_tab(&mut self, window: WindowId) {
        if let Some(index) = self.windows.get(&window).map(Window::active) {
            self.remove_tab(window, index);
        }
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
            .filter_map(|tab| Some(TabView { info: self.panes.get(&tab.active)?.info(), panes: tab.tree.panes() }))
            .collect()
    }

    pub fn active_tab(&self, window: WindowId) -> Option<usize> {
        self.windows.get(&window).map(Window::active)
    }

    /// The pane area.
    pub fn size(&self, window: WindowId) -> Option<Geometry> {
        self.windows.get(&window).map(|w| w.size)
    }

    /// Splits `pane`, starting `spawn` in the new part, which takes `share`
    /// (0 to 1) of the space and becomes active.
    pub fn split(&mut self, pane: PaneId, axis: Axis, share: f32, config: &Config, spawn: Spawn) -> io::Result<PaneId> {
        let (window, index) = self.find(pane).ok_or_else(|| missing("pane", pane.0))?;
        let size = self.windows.get(&window).ok_or_else(|| missing("window", window.0))?.size;
        let new = self.spawn(size, config, spawn)?;
        self.change_focus(window, |w| {
            w.activate(index);
            let tab = w.active_tab_mut();
            tab.zoomed = false;
            tab.tree.split(pane, new, axis, share);
            tab.active = new;
        });
        self.fit_tab(window, None);
        Ok(new)
    }

    /// Closes `pane`, and its tab with it when it was the last.
    pub fn close_pane(&mut self, pane: PaneId) {
        if let Some((window, _)) = self.find(pane) {
            self.remove_pane(window, pane);
        }
    }

    /// Moves `pane` into a new window of the same size, or its tab when it
    /// is the tab's only pane. Returns the new window; `None` when the pane
    /// is alone in its window.
    pub fn pop_pane(&mut self, pane: PaneId) -> Option<WindowId> {
        let (window, index) = self.find(pane)?;
        let size = self.windows.get(&window)?.size;
        let mut detached = false;
        self.change_focus(window, |w| {
            let tab = w.tab_mut(index);
            if let Some(next) = tab.tree.remove(pane) {
                detached = true;
                tab.zoomed = false;
                if tab.active == pane {
                    tab.active = next;
                }
            }
        });
        if !detached {
            return self.pop_tab_at(window, index);
        }
        self.fit_tab(window, Some(index));
        Some(self.adopt(size, Tab::new(pane)))
    }

    /// Moves the active tab into a new window of the same size. Returns the
    /// new window; `None` when the tab is alone in its window.
    pub fn pop_tab(&mut self, window: WindowId) -> Option<WindowId> {
        let index = self.windows.get(&window)?.active();
        self.pop_tab_at(window, index)
    }

    fn pop_tab_at(&mut self, window: WindowId, index: usize) -> Option<WindowId> {
        let w = self.windows.get(&window)?;
        if w.tabs().len() < 2 {
            return None;
        }
        let size = w.size;
        let mut removed = None;
        self.change_focus(window, |w| removed = Some(w.remove(index).0));
        Some(self.adopt(size, removed?))
    }

    /// Shows `pane`: its tab becomes active, and it the active pane, out of
    /// a zoom that hid it. Returns its window.
    pub fn reveal(&mut self, pane: PaneId) -> Option<WindowId> {
        let (window, index) = self.find(pane)?;
        self.change_focus(window, |w| {
            w.activate(index);
            let tab = w.active_tab_mut();
            tab.zoomed &= tab.active == pane;
            tab.active = pane;
        });
        self.fit_tab(window, None);
        Some(window)
    }

    /// The window and tab holding `pane`.
    pub fn find(&self, pane: PaneId) -> Option<(WindowId, usize)> {
        self.windows.iter().find_map(|(&id, w)| Some((id, w.position(pane)?)))
    }

    /// The directory of the window's active pane.
    pub fn active_cwd(&self, window: WindowId) -> Option<PathBuf> {
        self.cwd(self.active_pane(window)?)
    }

    pub fn cwd(&self, pane: PaneId) -> Option<PathBuf> {
        self.panes.get(&pane).and_then(|p| p.info().cwd.clone())
    }

    /// Makes the pane next to `pane` in `direction` active.
    pub fn focus_direction(&mut self, pane: PaneId, direction: Direction) {
        let Some((window, index)) = self.find(pane) else { return };
        let Some(w) = self.windows.get(&window) else { return };
        if let Some(next) = w.tabs()[index].tree.neighbour(w.area(), pane, direction) {
            self.reveal(next);
        }
    }

    /// Moves the divider nearest `pane` by `cells`.
    pub fn resize_pane(&mut self, pane: PaneId, direction: Direction, cells: u16) {
        let Some((window, index)) = self.find(pane) else { return };
        let Some(w) = self.windows.get_mut(&window) else { return };
        let area = w.area();
        let tab = w.tab_mut(index);
        if tab.zoomed {
            return;
        }
        tab.tree.resize(area, pane, direction, cells);
        self.fit_tab(window, Some(index));
    }

    /// Zooms `pane`, or ends the zoom of its tab.
    pub fn toggle_zoom(&mut self, pane: PaneId) {
        let Some((window, index)) = self.find(pane) else { return };
        self.change_focus(window, |w| {
            let tab = w.tab_mut(index);
            tab.zoomed = !tab.zoomed && tab.tree.panes().len() > 1;
            tab.active = pane;
        });
        self.fit_tab(window, Some(index));
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

    /// Whether closing `pane` would stop something other than a shell.
    pub fn pane_busy(&self, pane: PaneId) -> bool {
        self.panes.get(&pane).is_some_and(Pane::is_busy)
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
            if pumped.closed {
                closed.push(id);
            }
        }
        for pane in closed {
            self.close_pane(pane);
        }
        pump
    }

    fn remove_pane(&mut self, window: WindowId, pane: PaneId) {
        let Some(index) = self.windows.get(&window).and_then(|w| w.position(pane)) else { return };
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
    }

    /// Removes tab `index`, and the window with it when it was the last.
    fn remove_tab(&mut self, window: WindowId, index: usize) {
        let mut removed = None;
        self.change_focus(window, |w| removed = Some(w.remove(index)));
        let Some((tab, empty)) = removed else { return };
        for pane in tab.tree.panes() {
            self.panes.remove(&pane);
        }
        if empty {
            self.windows.remove(&window);
        }
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
        let Some(tab) = w.tabs().get(index.unwrap_or(w.active())) else { return };
        let size = w.size;
        // A zoomed tab's hidden panes keep their size.
        for (id, rect) in tab.layout(w.area()).panes {
            if let Some(pane) = self.panes.get_mut(&id) {
                pane.resize(Geometry { cols: rect.cols.max(1), rows: rect.rows.max(1), ..size });
            }
        }
    }

    /// A new window holding `tab`.
    fn adopt(&mut self, size: Geometry, tab: Tab) -> WindowId {
        let id = WindowId(self.next_id());
        self.windows.insert(id, Window::new(tab, size));
        self.fit_tab(id, None);
        id
    }

    fn spawn(&mut self, size: Geometry, config: &Config, spawn: Spawn) -> io::Result<PaneId> {
        let id = PaneId(self.next_id());
        let pane = Pane::spawn(id, size, config, spawn, &self.wake)?;
        self.panes.insert(id, pane);
        Ok(id)
    }

    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
}

fn missing(what: &str, id: u64) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("no {what} {id}"))
}
