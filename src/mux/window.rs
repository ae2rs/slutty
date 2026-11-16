//! A window's tabs and which one is active. Pure bookkeeping.

use super::PaneId;
use crate::terminal::Geometry;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Tab {
    pub pane: PaneId,
}

#[derive(Debug)]
pub(super) struct Window {
    tabs: Vec<Tab>,
    active: usize,
    /// The space for panes, below the tab bar and inside the padding.
    pub size: Geometry,
    pub focused: bool,
}

impl Window {
    pub fn new(tab: Tab, size: Geometry) -> Self {
        Self { tabs: vec![tab], active: 0, size, focused: false }
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub fn active(&self) -> usize {
        self.active
    }

    pub fn active_tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    /// Adds a tab at the end and makes it active.
    pub fn push(&mut self, tab: Tab) {
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
    }

    /// Makes tab `index` active. Returns whether that changed anything.
    pub fn activate(&mut self, index: usize) -> bool {
        if index >= self.tabs.len() || index == self.active {
            return false;
        }
        self.active = index;
        true
    }

    /// Removes tab `index`. The tab that takes its place becomes active,
    /// or the one before it when it was last. Returns the tab, and whether
    /// the window is now empty.
    pub fn remove(&mut self, index: usize) -> (Tab, bool) {
        let tab = self.tabs.remove(index);
        if index < self.active || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        (tab, self.tabs.is_empty())
    }

    pub fn position(&self, pane: PaneId) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.pane == pane)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: Geometry = Geometry { cols: 80, rows: 24, cell_width: 8, cell_height: 16 };

    fn window(n: u64) -> Window {
        let mut w = Window::new(Tab { pane: PaneId(0) }, SIZE);
        for i in 1..n {
            w.push(Tab { pane: PaneId(i) });
        }
        w
    }

    #[test]
    fn new_tabs_go_last_and_become_active() {
        let w = window(3);
        assert_eq!(w.active(), 2);
        assert_eq!(w.position(PaneId(1)), Some(1));
    }

    #[test]
    fn activate_ignores_missing_tabs() {
        let mut w = window(3);
        assert!(w.activate(0));
        assert!(!w.activate(0));
        assert!(!w.activate(7));
        assert_eq!(w.active(), 0);
    }

    #[test]
    fn removing_keeps_a_neighbour_active() {
        let mut w = window(4);
        w.activate(1);
        assert_eq!(w.remove(1).0.pane, PaneId(1));
        assert_eq!(w.active_tab().pane, PaneId(2), "the next tab takes its place");
        w.activate(2);
        w.remove(2);
        assert_eq!(w.active_tab().pane, PaneId(2), "the last tab falls back to the one before");
        w.remove(0);
        assert_eq!(w.active_tab().pane, PaneId(2), "removing before the active tab keeps it");
        assert!(w.remove(0).1, "the window is empty");
    }
}
