//! The left button and the pointer over panes: selecting text, copying it
//! on release, opening links on a click, and showing links under the
//! pointer. A program that tracks the mouse gets the button instead, unless
//! shift is held, as in WezTerm.

use std::time::Duration;

use dispatch2::{DispatchQueue, DispatchTime};
use libghostty_vt::mouse;
use objc2::DefinedClass;
use objc2_app_kit::{NSCursor, NSEvent, NSEventModifierFlags};

use super::actions;
use super::state::{self, App, with_app};
use super::view::TerminalView;
use crate::mux::PaneId;
use crate::scan::{self, RowText};
use crate::terminal::{Pointer, Release, Terminal};

/// How often a drag past an edge scrolls one more row.
const AUTOSCROLL: Duration = Duration::from_millis(50);

/// A link under the pointer: cells `start..end` of a viewport row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hover {
    pub pane: PaneId,
    pub row: u16,
    pub start: u16,
    pub end: u16,
    pub url: String,
}

impl TerminalView {
    pub(super) fn left_down(&self, event: &NSEvent) {
        if self.click_tab_bar(event) {
            return;
        }
        let point = self.pixel_point(event);
        let flags = event.modifierFlags();
        let shift = flags.contains(NSEventModifierFlags::Shift);
        let window = self.mux_window();
        // A click in another pane makes it active, and goes on to it.
        let pane = with_app(|app| {
            let (pane, ..) = self.pane_at(app, point)?;
            if app.mux.active_pane(window) != Some(pane) {
                app.mux.activate_pane(window, pane);
            }
            Some((pane, app.mux.pane_mut(pane)?.mouse_tracking()))
        })
        .flatten();
        self.ivars().mouse_pane.set(pane.map(|(id, _)| id));
        self.tabs_changed();
        let to_program = pane.is_some_and(|(_, tracking)| tracking) && !shift;
        self.ivars().selecting.set(!to_program);
        if to_program {
            self.send_mouse(event, mouse::Action::Press, Some(mouse::Button::Left));
        } else {
            self.with_pointer(point, |terminal, at| terminal.select_press(at, shift));
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
        if self.with_pointer(point, |terminal, at| terminal.select_drag(at, rectangle)) == Some(true) {
            self.schedule_autoscroll();
        }
    }

    pub(super) fn left_up(&self, event: &NSEvent) {
        self.ivars().drag.set(None);
        if self.ivars().selecting.replace(false) {
            let point = self.pixel_point(event);
            match self.with_pointer(point, |terminal, at| terminal.select_release(at)) {
                Some(Release::Selected) => self.copy_selection(),
                Some(Release::Clicked) => {
                    if let Some(link) = self.link_at(point) {
                        actions::open_url(&link.url);
                    }
                }
                None => {}
            }
        } else {
            self.send_mouse(event, mouse::Action::Release, Some(mouse::Button::Left));
        }
        self.ivars().mouse_pane.set(None);
    }

    /// Shows whether a link is under the pointer, and tells a program that
    /// tracks all motion.
    pub(super) fn pointer_moved(&self, event: &NSEvent) {
        self.send_mouse(event, mouse::Action::Motion, None);
        let hover = self.link_at(self.pixel_point(event));
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

    /// Puts the active pane's selection on the clipboard.
    pub(super) fn copy_selection(&self) {
        let window = self.mux_window();
        let text = with_app(|app| app.mux.pane_mut(app.mux.active_pane(window)?)?.terminal().selection_text());
        if let Some(text) = text.flatten() {
            actions::set_clipboard(&text);
        }
    }

    /// The link under a point: an OSC 8 link, else a URL in the text.
    fn link_at(&self, point: [f64; 2]) -> Option<Hover> {
        with_app(|app| {
            let (pane, at) = self.pointer(app, point)?;
            let pane_ref = app.mux.pane_mut(pane)?;
            if let Some((start, end, url)) = pane_ref.terminal().hyperlink_at(at.col, at.row) {
                return Some(Hover { pane, row: at.row, start, end, url });
            }
            let text = RowText::new(pane_ref.frame().rows.get(usize::from(at.row))?);
            let span = scan::link_at(at.row, &text, at.col)?;
            Some(Hover { pane, row: at.row, start: span.start, end: span.end, url: span.text })
        })
        .flatten()
    }

    /// The pane under a point and the cell there.
    fn pointer(&self, app: &mut App, point: [f64; 2]) -> Option<(PaneId, Pointer)> {
        let (pane, x, y) = self.pane_at(app, point)?;
        let m = app.renderer.metrics();
        // Saturating casts: a point above or left of the pane is cell 0.
        let col = (x / m.cell_width as f32) as u16;
        let row = (y / m.cell_height as f32) as u16;
        Some((pane, Pointer { col, row, x, y }))
    }

    /// Runs `f` on the terminal of the pane under a point, then draws.
    fn with_pointer<R>(&self, point: [f64; 2], f: impl FnOnce(&mut Terminal, Pointer) -> R) -> Option<R> {
        let result = with_app(|app| {
            let (pane, at) = self.pointer(app, point)?;
            Some(f(app.mux.pane_mut(pane)?.terminal_mut(), at))
        });
        self.request_frame();
        result.flatten()
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
        if self.with_pointer(point, |terminal, at| terminal.select_autoscroll(at, rectangle)) == Some(true) {
            self.schedule_autoscroll();
        }
    }
}
