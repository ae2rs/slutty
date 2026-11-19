//! The UI's copy of what the server shows: each window's tabs and layout,
//! and the screens of the panes on show. Views draw from it and never wait
//! on the server.

use std::collections::{BTreeMap, BTreeSet};

use crate::mux::{PaneId, Rect, WindowId};
use crate::proto::{Event, Hover, PaneOverlay, PaneUpdate, WindowState};
use crate::terminal::{Frame, Notification};

#[derive(Default)]
pub struct Mirror {
    windows: BTreeMap<WindowId, WindowState>,
    panes: BTreeMap<PaneId, Shown>,
    /// Panes with an update the window has not drawn yet.
    undrawn: BTreeSet<PaneId>,
}

#[derive(Default)]
struct Shown {
    frame: Frame,
    overlay: PaneOverlay,
}

/// What events changed, for the windows to show.
#[derive(Debug, Default)]
pub struct Changes {
    /// Windows seen for the first time.
    pub opened: Vec<WindowId>,
    pub closed: Vec<WindowId>,
    /// Windows whose tabs, layout or focus changed.
    pub windows: BTreeSet<WindowId>,
    /// Windows with a pane update to draw.
    pub output: BTreeSet<WindowId>,
    /// The last text for the clipboard.
    pub clipboard: Option<String>,
    pub notifications: Vec<(PaneId, Notification)>,
    pub open_urls: Vec<String>,
    /// The answer to the last hover, if one came.
    pub hover: Option<Option<Hover>>,
    /// A newer UI took over.
    pub detached: bool,
}

impl Mirror {
    pub fn apply(&mut self, event: Event, changes: &mut Changes) {
        match event {
            Event::Window(state) => {
                let id = state.id;
                if self.windows.insert(id, state).is_none() {
                    changes.opened.push(id);
                }
                changes.windows.insert(id);
                self.drop_hidden_panes();
            }
            Event::WindowClosed(id) => {
                self.windows.remove(&id);
                changes.closed.push(id);
                self.drop_hidden_panes();
            }
            Event::Pane(update) => {
                let Some(window) = self.window_showing(update.pane) else { return };
                changes.output.insert(window);
                self.undrawn.insert(update.pane);
                apply_update(self.panes.entry(update.pane).or_default(), update);
            }
            Event::Clipboard(text) => changes.clipboard = Some(text),
            Event::Notification { pane, notification } => changes.notifications.push((pane, notification)),
            Event::OpenUrl(url) => changes.open_urls.push(url),
            Event::Hover(hover) => changes.hover = Some(hover),
            Event::Detached => changes.detached = true,
        }
    }

    pub fn window(&self, window: WindowId) -> Option<&WindowState> {
        self.windows.get(&window)
    }

    /// The pane keys go to.
    pub fn active_pane(&self, window: WindowId) -> Option<PaneId> {
        self.windows.get(&window).map(|w| w.active_pane)
    }

    /// The directory of the window's active pane.
    pub fn active_cwd(&self, window: WindowId) -> Option<std::path::PathBuf> {
        let w = self.windows.get(&window)?;
        w.tabs.get(w.active_tab)?.info.cwd.clone()
    }

    /// The window holding `pane`, in any of its tabs.
    pub fn find(&self, pane: PaneId) -> Option<WindowId> {
        self.windows.values().find(|w| w.tabs.iter().any(|tab| tab.panes.contains(&pane))).map(|w| w.id)
    }

    pub fn overlay(&self, pane: PaneId) -> Option<&PaneOverlay> {
        self.panes.get(&pane).map(|p| &p.overlay)
    }

    pub fn frame(&self, pane: PaneId) -> Option<&Frame> {
        self.panes.get(&pane).map(|p| &p.frame)
    }

    /// The panes on screen in `window` with their place in cells, and the
    /// frames they show.
    pub fn visible(&mut self, window: WindowId) -> Vec<(PaneId, Rect, &mut Frame)> {
        let Some(w) = self.windows.get(&window) else { return Vec::new() };
        let rects: BTreeMap<PaneId, Rect> = w.panes.iter().copied().collect();
        let mut panes: Vec<_> =
            self.panes.iter_mut().filter_map(|(id, shown)| Some((*id, *rects.get(id)?, &mut shown.frame))).collect();
        // In layout order, as the server sent them.
        panes.sort_by_key(|(id, ..)| w.panes.iter().position(|(p, _)| p == id));
        panes
    }

    /// The panes of `window` with an update not yet drawn, which a draw now
    /// takes.
    pub fn take_undrawn(&mut self, window: WindowId) -> Vec<PaneId> {
        let Some(w) = self.windows.get(&window) else { return Vec::new() };
        let drawn: Vec<PaneId> = w.panes.iter().map(|(id, _)| *id).filter(|id| self.undrawn.contains(id)).collect();
        for id in &drawn {
            self.undrawn.remove(id);
        }
        drawn
    }

    fn window_showing(&self, pane: PaneId) -> Option<WindowId> {
        self.windows.values().find(|w| w.panes.iter().any(|(id, _)| *id == pane)).map(|w| w.id)
    }

    /// Forgets panes no window shows; the server sends them whole when they
    /// show again.
    fn drop_hidden_panes(&mut self) {
        let shown: BTreeSet<PaneId> = self.windows.values().flat_map(|w| w.panes.iter().map(|(id, _)| *id)).collect();
        self.panes.retain(|id, _| shown.contains(id));
        self.undrawn.retain(|id| shown.contains(id));
    }
}

fn apply_update(shown: &mut Shown, update: PaneUpdate) {
    let frame = &mut shown.frame;
    let rows = usize::from(update.rows);
    if frame.cols != update.cols
        || frame.rows.len() != rows
        || frame.background != update.background
        || frame.foreground != update.foreground
    {
        frame.cols = update.cols;
        frame.rows.resize_with(rows, Default::default);
        frame.background = update.background;
        frame.foreground = update.foreground;
    }
    for (y, mut row) in update.changed {
        if let Some(target) = frame.rows.get_mut(usize::from(y)) {
            row.dirty = true;
            *target = row;
        }
    }
    frame.cursor = update.cursor;
    frame.changed = true;
    shown.overlay = update.overlay;
}

#[cfg(test)]
mod tests {
    use libghostty_vt::color::Rgb;

    use super::*;
    use crate::proto::TabState;
    use crate::terminal::{Cell, CellStyle, Geometry, Grapheme, Row};

    fn window(id: WindowId, panes: &[PaneId]) -> WindowState {
        let rect = Rect { x: 0, y: 0, cols: 10, rows: 2 };
        WindowState {
            id,
            size: Geometry { cols: 10, rows: 2, cell_width: 8, cell_height: 16 },
            tabs: vec![TabState { info: Default::default(), panes: panes.to_vec() }],
            active_tab: 0,
            panes: panes.iter().map(|p| (*p, rect)).collect(),
            dividers: Vec::new(),
            active_pane: panes[0],
        }
    }

    fn update(pane: PaneId, rows: &[(u16, char)]) -> PaneUpdate {
        let black = Rgb { r: 0, g: 0, b: 0 };
        let row = |c| Row {
            cells: vec![Cell { text: Grapheme::Char(c), fg: black, bg: None, style: CellStyle::default() }],
            dirty: false,
        };
        PaneUpdate {
            pane,
            cols: 10,
            rows: 2,
            background: black,
            foreground: black,
            cursor: None,
            changed: rows.iter().map(|&(y, c)| (y, row(c))).collect(),
            overlay: PaneOverlay::default(),
        }
    }

    fn ids() -> (WindowId, PaneId, PaneId) {
        (WindowId::for_test(1), PaneId::for_test(2), PaneId::for_test(3))
    }

    #[test]
    fn updates_patch_rows_and_wait_to_be_drawn() {
        let (w, a, _) = ids();
        let mut mirror = Mirror::default();
        let mut changes = Changes::default();
        mirror.apply(Event::Window(window(w, &[a])), &mut changes);
        assert_eq!(changes.opened, [w]);
        mirror.apply(Event::Pane(update(a, &[(0, 'x'), (1, 'y')])), &mut changes);
        mirror.apply(Event::Pane(update(a, &[(1, 'z')])), &mut changes);
        assert!(changes.output.contains(&w));
        let frame = mirror.frame(a).unwrap();
        let text = |y: usize| frame.rows[y].cells[0].text.clone();
        assert_eq!((text(0), text(1)), (Grapheme::Char('x'), Grapheme::Char('z')));
        assert_eq!(mirror.take_undrawn(w), [a]);
        assert!(mirror.take_undrawn(w).is_empty());
    }

    #[test]
    fn panes_that_leave_the_screen_are_forgotten() {
        let (w, a, b) = ids();
        let mut mirror = Mirror::default();
        let mut changes = Changes::default();
        mirror.apply(Event::Window(window(w, &[a, b])), &mut changes);
        mirror.apply(Event::Pane(update(b, &[(0, 'x')])), &mut changes);
        mirror.apply(Event::Window(window(w, &[a])), &mut changes);
        assert!(mirror.frame(b).is_none());
        assert!(mirror.take_undrawn(w).is_empty());
        assert_eq!(mirror.find(a), Some(w));
        mirror.apply(Event::WindowClosed(w), &mut changes);
        assert_eq!(changes.closed, [w]);
        assert_eq!(mirror.find(a), None);
    }
}
