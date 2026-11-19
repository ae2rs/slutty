//! A window's tabs and which one is active, and each tab's panes. Pure
//! bookkeeping.

use super::PaneId;
use super::float::Place;
use super::tree::{Layout, Rect, Tree};
use crate::terminal::Geometry;

/// A pane over the tiled ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Float {
    pub pane: PaneId,
    pub place: Place,
    pub hidden: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Tab {
    pub tree: Tree,
    /// The tiled pane keys go to while no float has them. Always in `tree`.
    pub active: PaneId,
    /// Whether the active pane fills the tab, hiding the other tiled ones.
    pub zoomed: bool,
    /// The last is on top.
    floats: Vec<Float>,
    /// Keys go to the top shown float. Only set while one shows.
    floating: bool,
}

impl Tab {
    pub fn new(pane: PaneId) -> Self {
        Self { tree: Tree::new(pane), active: pane, zoomed: false, floats: Vec::new(), floating: false }
    }

    /// The pane keys go to.
    pub fn focused(&self) -> PaneId {
        match self.floats.iter().rev().find(|f| !f.hidden) {
            Some(top) if self.floating => top.pane,
            _ => self.active,
        }
    }

    /// Every pane: the tiled ones in reading order, then the floats from
    /// the bottom.
    pub fn panes(&self) -> Vec<PaneId> {
        let mut panes = self.tree.panes();
        panes.extend(self.floats.iter().map(|f| f.pane));
        panes
    }

    pub fn contains(&self, pane: PaneId) -> bool {
        self.tree.contains(pane) || self.float(pane).is_some()
    }

    pub fn floats(&self) -> &[Float] {
        &self.floats
    }

    pub fn float(&self, pane: PaneId) -> Option<&Float> {
        self.floats.iter().find(|f| f.pane == pane)
    }

    pub fn float_mut(&mut self, pane: PaneId) -> Option<&mut Float> {
        self.floats.iter_mut().find(|f| f.pane == pane)
    }

    /// Adds a float on top, with the keys unless `hidden`.
    pub fn add_float(&mut self, pane: PaneId, place: Place, hidden: bool) {
        self.floats.push(Float { pane, place, hidden });
        self.floating |= !hidden;
    }

    /// Shows the float `pane` on top and gives it the keys.
    pub fn raise(&mut self, pane: PaneId) {
        let Some(at) = self.floats.iter().position(|f| f.pane == pane) else { return };
        let mut float = self.floats.remove(at);
        float.hidden = false;
        self.floats.push(float);
        self.floating = true;
    }

    /// Hides the float `pane`; the keys go to the next float shown, else to
    /// the tiled panes.
    pub fn hide(&mut self, pane: PaneId) {
        if let Some(float) = self.float_mut(pane) {
            float.hidden = true;
        }
        self.settle();
    }

    /// Takes the float `pane` out. Returns whether it was one.
    pub fn remove_float(&mut self, pane: PaneId) -> bool {
        let before = self.floats.len();
        self.floats.retain(|f| f.pane != pane);
        self.settle();
        self.floats.len() != before
    }

    /// Gives the keys to the tiled pane `pane`.
    pub fn focus_tiled(&mut self, pane: PaneId) {
        self.active = pane;
        self.floating = false;
    }

    /// Hides every float when one shows, else shows them all. Returns
    /// whether there were any.
    pub fn toggle_floats(&mut self) -> bool {
        let show = self.floats.iter().all(|f| f.hidden);
        for float in &mut self.floats {
            float.hidden = !show;
        }
        self.floating = show;
        self.settle();
        !self.floats.is_empty()
    }

    /// Moves the keys on: from the tiled panes to the first float shown,
    /// from a float to the next one shown, in the order they were made,
    /// and from the last back to the tiled panes.
    pub fn cycle_floats(&mut self) {
        let mut shown: Vec<PaneId> = self.floats.iter().filter(|f| !f.hidden).map(|f| f.pane).collect();
        shown.sort();
        let next = if self.floating {
            let focused = self.focused();
            shown.into_iter().find(|&pane| pane > focused)
        } else {
            shown.first().copied()
        };
        match next {
            Some(pane) => self.raise(pane),
            None => self.floating = false,
        }
    }

    /// Where each shown pane goes in `area`.
    pub fn layout(&self, area: Rect) -> Layout {
        let mut layout = if self.zoomed {
            Layout { panes: vec![(self.active, area)], ..Layout::default() }
        } else {
            self.tree.layout(area)
        };
        layout.floats = self.floats.iter().filter(|f| !f.hidden).map(|f| (f.pane, f.place.rect(area))).collect();
        layout
    }

    /// Keeps `floating` only while a float shows.
    fn settle(&mut self) {
        self.floating &= self.floats.iter().any(|f| !f.hidden);
    }
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

    /// The pane area in cells.
    pub fn area(&self) -> Rect {
        Rect { x: 0, y: 0, cols: self.size.cols, rows: self.size.rows }
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub fn tab_mut(&mut self, index: usize) -> &mut Tab {
        &mut self.tabs[index]
    }

    pub fn active(&self) -> usize {
        self.active
    }

    pub fn active_tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    pub fn active_tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
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

    /// The tab holding `pane`.
    pub fn position(&self, pane: PaneId) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.contains(pane))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: Geometry = Geometry { cols: 80, rows: 24, cell_width: 8, cell_height: 16 };

    fn window(n: u64) -> Window {
        let mut w = Window::new(Tab::new(PaneId(0)), SIZE);
        for i in 1..n {
            w.push(Tab::new(PaneId(i)));
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
        assert_eq!(w.remove(1).0.active, PaneId(1));
        assert_eq!(w.active_tab().active, PaneId(2), "the next tab takes its place");
        w.activate(2);
        w.remove(2);
        assert_eq!(w.active_tab().active, PaneId(2), "the last tab falls back to the one before");
        w.remove(0);
        assert_eq!(w.active_tab().active, PaneId(2), "removing before the active tab keeps it");
        assert!(w.remove(0).1, "the window is empty");
    }

    #[test]
    fn a_zoomed_tab_shows_only_its_active_pane() {
        let mut tab = Tab::new(PaneId(1));
        tab.tree.split(PaneId(1), PaneId(2), super::super::tree::Axis::Row, 0.5);
        tab.zoomed = true;
        let area = Rect { x: 0, y: 0, cols: 80, rows: 24 };
        assert_eq!(tab.layout(area).panes, vec![(PaneId(1), area)]);
    }

    fn floats(tab: &Tab) -> Vec<(u64, bool)> {
        tab.floats().iter().map(|f| (f.pane.0, f.hidden)).collect()
    }

    #[test]
    fn a_shown_float_takes_the_keys_until_hidden() {
        let mut tab = Tab::new(PaneId(0));
        tab.add_float(PaneId(1), Place::default(), false);
        tab.add_float(PaneId(2), Place::default(), true);
        assert_eq!(tab.focused(), PaneId(1), "a hidden float does not take the keys");
        tab.raise(PaneId(2));
        assert_eq!((tab.focused(), floats(&tab)), (PaneId(2), vec![(1, false), (2, false)]), "raised on top");
        tab.hide(PaneId(2));
        assert_eq!(tab.focused(), PaneId(1), "to the next float shown");
        tab.hide(PaneId(1));
        assert_eq!(tab.focused(), PaneId(0), "back to the tiled panes");
        assert!(tab.layout(Rect { x: 0, y: 0, cols: 80, rows: 24 }).floats.is_empty());
    }

    #[test]
    fn toggling_hides_all_floats_or_shows_them_all() {
        let mut tab = Tab::new(PaneId(0));
        assert!(!tab.toggle_floats(), "no floats");
        tab.add_float(PaneId(1), Place::default(), false);
        tab.add_float(PaneId(2), Place::default(), true);
        assert!(tab.toggle_floats());
        assert_eq!((tab.focused(), floats(&tab)), (PaneId(0), vec![(1, true), (2, true)]));
        tab.toggle_floats();
        assert_eq!((tab.focused(), floats(&tab)), (PaneId(2), vec![(1, false), (2, false)]));
    }

    #[test]
    fn cycling_goes_through_the_floats_shown_and_back() {
        let mut tab = Tab::new(PaneId(0));
        for (pane, hidden) in [(1, false), (2, true), (3, false)] {
            tab.add_float(PaneId(pane), Place::default(), hidden);
        }
        tab.focus_tiled(PaneId(0));
        let mut seen = Vec::new();
        for _ in 0..4 {
            tab.cycle_floats();
            seen.push(tab.focused().0);
        }
        assert_eq!(seen, [1, 3, 0, 1]);
    }

    #[test]
    fn a_closed_float_hands_the_keys_on() {
        let mut tab = Tab::new(PaneId(0));
        tab.add_float(PaneId(1), Place::default(), false);
        assert!(tab.contains(PaneId(1)));
        assert_eq!(tab.panes(), [PaneId(0), PaneId(1)]);
        assert!(tab.remove_float(PaneId(1)));
        assert!(!tab.remove_float(PaneId(0)), "a tiled pane is not a float");
        assert_eq!(tab.focused(), PaneId(0));
    }
}
