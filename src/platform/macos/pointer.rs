//! The left button and the pointer over panes: selecting text, copying it
//! on release, opening links on a click, and showing links under the
//! pointer. A program that tracks the mouse gets the button instead, unless
//! shift is held, as in WezTerm. The server does the selecting; this side
//! says where the pointer is.

use std::time::Duration;

use dispatch2::{DispatchQueue, DispatchTime};
use libghostty_vt::mouse;
use objc2::DefinedClass;
use objc2_app_kit::{NSCursor, NSEvent, NSEventModifierFlags};

use super::state::{self, App, with_app};
use super::view::TerminalView;
use crate::mux::PaneId;
use crate::proto::{Command, Gesture, Hover, Input};
use crate::terminal::Pointer;

/// How often a drag past an edge scrolls one more row.
const AUTOSCROLL: Duration = Duration::from_millis(50);

impl TerminalView {
    pub(super) fn left_down(&self, event: &NSEvent) {
        if self.click_tab_bar(event) {
            return;
        }
        let point = self.pixel_point(event);
        let shift = event.modifierFlags().contains(NSEventModifierFlags::Shift);
        let window = self.mux_window();
        // A click in another pane makes it active, and goes on to it.
        let target = with_app(|app| {
            let (pane, at) = self.pointer(app, point)?;
            if app.mirror.active_pane(window) != Some(pane) {
                app.request(Command::ActivatePane { pane });
            }
            let tracking = app.mirror.overlay(pane).is_some_and(|o| o.mouse_tracking);
            Some((pane, at, tracking))
        })
        .flatten();
        self.ivars().mouse_pane.set(target.map(|(pane, ..)| pane));
        let to_program = target.is_some_and(|(.., tracking)| tracking) && !shift;
        self.ivars().selecting.set(!to_program);
        if to_program {
            self.send_mouse(event, mouse::Action::Press, Some(mouse::Button::Left));
        } else if let Some((pane, at, _)) = target {
            self.select(pane, Gesture::Press { at, extend: shift });
        }
    }

    pub(super) fn left_dragged(&self, event: &NSEvent) {
        if !self.ivars().selecting.get() {
            self.send_mouse(event, mouse::Action::Motion, Some(mouse::Button::Left));
            return;
        }
        let point = self.pixel_point(event);
        let rectangle = event.modifierFlags().contains(NSEventModifierFlags::Option);
        self.ivars().drag.set(Some((point, rectangle)));
        let Some((pane, at, past)) = with_app(|app| self.drag_point(app, point)).flatten() else { return };
        self.select(pane, Gesture::Drag { at, rectangle });
        if past {
            self.schedule_autoscroll();
        }
    }

    pub(super) fn left_up(&self, event: &NSEvent) {
        self.ivars().drag.set(None);
        if self.ivars().selecting.replace(false) {
            let point = self.pixel_point(event);
            if let Some((pane, at)) = with_app(|app| self.pointer(app, point)).flatten() {
                self.select(pane, Gesture::Release { at });
            }
        } else {
            self.send_mouse(event, mouse::Action::Release, Some(mouse::Button::Left));
        }
        self.ivars().mouse_pane.set(None);
    }

    /// Shows whether a tab is under the pointer, asks what link is, and
    /// tells a program that tracks all motion.
    pub(super) fn pointer_moved(&self, event: &NSEvent) {
        self.send_mouse(event, mouse::Action::Motion, None);
        let point = self.pixel_point(event);
        self.set_hover_tab(self.tab_at(point).flatten());
        let cell = with_app(|app| self.pointer(app, point)).flatten().map(|(pane, at)| (pane, at.col, at.row));
        if self.ivars().hover_cell.replace(cell) == cell {
            return;
        }
        match cell {
            Some((pane, col, row)) => _ = with_app(|app| app.input(Input::Hover { pane, col, row })),
            None => self.show_hover(None),
        }
    }

    /// Shows the link the server found under the pointer, if any, and the
    /// pointer that says so.
    pub(super) fn show_hover(&self, hover: Option<Hover>) {
        let cursor = if hover.is_some() { NSCursor::pointingHandCursor() } else { NSCursor::IBeamCursor() };
        cursor.set();
        self.set_hover(hover);
    }

    pub(super) fn set_hover(&self, hover: Option<Hover>) {
        if *self.ivars().hover.borrow() != hover {
            *self.ivars().hover.borrow_mut() = hover;
            self.ivars().overlay_dirty.set(true);
            self.request_frame();
        }
    }

    /// The pane under a point and the cell there.
    fn pointer(&self, app: &App, point: [f64; 2]) -> Option<(PaneId, Pointer)> {
        let (pane, x, y) = self.pane_at(app, point)?;
        let m = app.renderer.metrics();
        // Saturating casts: a point above or left of the pane is cell 0.
        let col = (x / m.cell_width as f32) as u16;
        let row = (y / m.cell_height as f32) as u16;
        Some((pane, Pointer { col, row, x, y }))
    }

    /// As [`TerminalView::pointer`], and whether the point is above or below
    /// the pane, where a drag scrolls.
    fn drag_point(&self, app: &App, point: [f64; 2]) -> Option<(PaneId, Pointer, bool)> {
        let (pane, at) = self.pointer(app, point)?;
        let window = app.mirror.window(self.mux_window())?;
        let rows = window.rect(pane)?.rows;
        let height = f32::from(rows) * app.renderer.metrics().cell_height as f32;
        Some((pane, at, at.y < 0.0 || at.y >= height))
    }

    fn select(&self, pane: PaneId, gesture: Gesture) {
        with_app(|app| app.input(Input::Select { pane, gesture }));
    }

    /// Scrolls on while a drag stays past an edge. The view is found again
    /// by its window, since it cannot cross into the dispatch block.
    fn schedule_autoscroll(&self) {
        if self.ivars().autoscrolling.replace(true) {
            return;
        }
        let window = self.mux_window();
        let when = DispatchTime::try_from(AUTOSCROLL).unwrap_or(DispatchTime::NOW);
        let scheduled = DispatchQueue::main().after(when, move || {
            if let Some(view) = state::view(window) {
                view.autoscroll();
            }
        });
        if scheduled.is_err() {
            self.ivars().autoscrolling.set(false);
        }
    }

    fn autoscroll(&self) {
        self.ivars().autoscrolling.set(false);
        let Some((point, rectangle)) = self.ivars().drag.get() else { return };
        let Some((pane, at, past)) = with_app(|app| self.drag_point(app, point)).flatten() else { return };
        if past {
            self.select(pane, Gesture::Autoscroll { at, rectangle });
            self.schedule_autoscroll();
        }
    }
}
