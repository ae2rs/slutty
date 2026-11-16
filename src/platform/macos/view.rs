//! A window's view: input, IME and drawing for the panes of one mux window.
//!
//! The view keeps only what belongs to the window on screen; the mux, the
//! renderer and the config are the app's ([`super::state`]). A display link
//! that is paused while nothing changes paces the frames.

use std::cell::{Cell, OnceCell, RefCell};
use std::ops::Range;
use std::time::Instant;

use libghostty_vt::color::Rgb;
use libghostty_vt::key::{self, Key, Mods};
use libghostty_vt::mouse;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSEvent, NSEventModifierFlags, NSResponder, NSScreen, NSTextInputClient, NSTrackingArea, NSTrackingAreaOptions,
    NSView, NSViewLayerContentsRedrawPolicy, NSWindowDelegate, NSWindowOcclusionState,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{
    NSArray, NSAttributedString, NSAttributedStringKey, NSNotFound, NSNotification, NSObject, NSPoint, NSRange,
    NSRangePointer, NSRect, NSRunLoop, NSRunLoopCommonModes, NSSize, NSString, NSUInteger,
};
use objc2_metal::MTLDevice;
use objc2_quartz_core::{CADisplayLink, CALayer, CATransaction};

use super::actions;
use super::keys;
use super::layout::Layout;
use super::pointer::Hover;
use super::quick_select::QuickSelect;
use super::state::{self, App, with_app};
use super::tab_bar::{self, SpinnerFrame};
use super::ticker;
use crate::animation::Live;
use crate::config::{self, Config, OptionAsAlt};
use crate::keys::Outcome;
use crate::mux::{Axis, ModeKey, PaneId, Rect, Visible, WindowId};
use crate::renderer::{Line, Overlay, PaneDraw, RectInstance, Renderer, Shown, Surface, Tint};
use crate::terminal::{CellStyle, Highlight, KeyInput, MouseInput, SearchStatus};

pub struct Ivars {
    window: WindowId,
    surface: RefCell<Surface>,
    /// Over the tab bar row: rho's spinners, so they animate without drawing
    /// the window.
    spinner_surface: RefCell<Surface>,
    /// What the spinner surface shows; `None` while it is hidden.
    spinner: RefCell<Option<SpinnerFrame>>,
    /// Some of the window shows on a screen; see [`TerminalView::update_on_screen`].
    on_screen: Cell<bool>,
    /// The zones last reported live, so the timer is armed again only when
    /// they change.
    live: Cell<Live>,
    /// The last key press or focus, which a blinking cursor's phase starts
    /// from.
    input_at: Cell<Instant>,
    /// The active pane's cursor blinks, as last drawn.
    cursor_blinks: Cell<bool>,
    /// The blinking zones the window's surface shows.
    shown: Cell<Shown>,
    link: OnceCell<Retained<CADisplayLink>>,
    layout: Cell<Layout>,
    /// Each tab's cells in the tab bar, for clicks.
    tab_cells: RefCell<Vec<Range<usize>>>,
    /// The tab under the pointer.
    hover_tab: Cell<Option<usize>>,
    focused: Cell<bool>,
    /// Text the input method is composing.
    marked: RefCell<String>,
    /// Text committed while `keyDown` runs the input method; `None` outside.
    key_texts: RefCell<Option<Vec<String>>>,
    /// Focus, preedit or the tab bar changed: draw even if no pane did.
    pub(super) overlay_dirty: Cell<bool>,
    /// The cursor cell in view coordinates, for placing the IME window.
    cursor_rect: Cell<NSRect>,
    /// Fractions of a line left over from trackpad scrolling.
    scroll_remainder: Cell<f64>,
    /// The pane a mouse button went down in; drags and the release go there.
    pub(super) mouse_pane: Cell<Option<PaneId>>,
    /// The left button went down to select text, not for the program.
    pub(super) selecting: Cell<bool>,
    /// Where a selecting drag is, and whether it selects a rectangle.
    pub(super) drag: Cell<Option<([f64; 2], bool)>>,
    /// An autoscroll step is scheduled.
    pub(super) autoscrolling: Cell<bool>,
    pub(super) hover: RefCell<Option<Hover>>,
    pub(super) quick: RefCell<Option<QuickSelect>>,
    /// While picking a pane, each pane's label; empty otherwise.
    pane_labels: RefCell<Vec<(char, PaneId)>>,
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    pub struct TerminalView;

    unsafe impl NSObjectProtocol for TerminalView {}

    impl TerminalView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method_id(makeBackingLayer))]
        fn make_backing_layer(&self) -> Retained<CALayer> {
            Retained::into_super(self.ivars().surface.borrow().layer().clone())
        }

        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, size: NSSize) {
            let _: () = unsafe { msg_send![super(self), setFrameSize: size] };
            self.relayout();
        }

        #[unsafe(method(viewDidChangeBackingProperties))]
        fn view_did_change_backing_properties(&self) {
            let Some(window) = self.window() else { return };
            let scale = window.backingScaleFactor();
            self.ivars().surface.borrow().layer().setContentsScale(scale);
            self.ivars().spinner_surface.borrow().layer().setContentsScale(scale);
            let changed = with_app(|app| {
                if app.scale == scale {
                    return false;
                }
                app.scale = scale;
                app.load_fonts();
                true
            });
            if changed == Some(true) {
                state::views().iter().for_each(|view| view.relayout());
            }
        }

        #[unsafe(method(step:))]
        fn step(&self, _link: &CADisplayLink) {
            self.draw();
            // Damage unpauses the link again; until then nothing runs.
            if let Some(link) = self.ivars().link.get() {
                link.setPaused(true);
            }
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            self.restart_blink();
            if !self.bind_key(event) {
                self.handle_key_down(event);
            }
        }

        #[unsafe(method(keyUp:))]
        fn key_up(&self, event: &NSEvent) {
            let code = event.keyCode();
            let swallowed = with_app(|app| {
                let found = app.swallowed.iter().position(|&c| c == code);
                found.map(|i| app.swallowed.swap_remove(i)).is_some()
            });
            if swallowed == Some(false) && self.ivars().marked.borrow().is_empty() {
                self.send_key(event, key::Action::Release, None, Mods::empty());
            }
        }

        // Command and control chords reach performKeyEquivalent before
        // keyDown and the menus, and some (ctrl+tab, ctrl+esc) never reach
        // keyDown at all.
        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
            self.key_equivalent(event)
        }

        #[unsafe(method(paste:))]
        fn paste(&self, _sender: Option<&AnyObject>) {
            self.paste_clipboard();
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.left_down(event);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            self.left_up(event);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            self.left_dragged(event);
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            self.pointer_moved(event);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            self.set_hover(None);
            self.set_hover_tab(None);
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            self.send_mouse(event, mouse::Action::Press, Some(mouse::Button::Right));
        }

        #[unsafe(method(rightMouseUp:))]
        fn right_mouse_up(&self, event: &NSEvent) {
            self.send_mouse(event, mouse::Action::Release, Some(mouse::Button::Right));
        }

        #[unsafe(method(otherMouseDown:))]
        fn other_mouse_down(&self, event: &NSEvent) {
            self.send_mouse(event, mouse::Action::Press, Some(mouse::Button::Middle));
        }

        #[unsafe(method(otherMouseUp:))]
        fn other_mouse_up(&self, event: &NSEvent) {
            self.send_mouse(event, mouse::Action::Release, Some(mouse::Button::Middle));
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let mods = keys::mods(event.modifierFlags());
            let dy = event.scrollingDeltaY();
            let precise = event.hasPreciseScrollingDeltas();
            let point = self.pixel_point(event);
            let remainder = &self.ivars().scroll_remainder;
            with_app(|app| {
                // Positive deltas scroll toward older output.
                let lines = if precise {
                    let cell = f64::from(app.renderer.metrics().cell_height) / app.scale;
                    let total = remainder.get() - dy / cell;
                    remainder.set(total.fract());
                    total.trunc() as isize
                } else if dy == 0.0 {
                    0
                } else {
                    -(dy.signum() as isize) * isize::try_from(app.config().mouse.wheel_lines).unwrap_or(1)
                };
                if let Some((pane, x, y)) = self.pane_at(app, point) {
                    pane_mut(app, pane, |p| p.scroll(lines, mods, x, y));
                }
            });
            self.request_frame();
        }
    }

    unsafe impl NSTextInputClient for TerminalView {
        #[unsafe(method(insertText:replacementRange:))]
        fn insert_text(&self, string: &AnyObject, _replacement: NSRange) {
            let text = string_of(string);
            let ivars = self.ivars();
            if !ivars.marked.borrow().is_empty() {
                ivars.marked.borrow_mut().clear();
                ivars.overlay_dirty.set(true);
                self.request_frame();
            }
            let mut texts = ivars.key_texts.borrow_mut();
            if let Some(texts) = texts.as_mut() {
                texts.push(text);
            } else {
                // Committed outside a key press, such as from the emoji picker.
                drop(texts);
                self.send_text(&text);
            }
        }

        #[unsafe(method(setMarkedText:selectedRange:replacementRange:))]
        fn set_marked_text(&self, string: &AnyObject, _selected: NSRange, _replacement: NSRange) {
            *self.ivars().marked.borrow_mut() = string_of(string);
            self.ivars().overlay_dirty.set(true);
            self.request_frame();
        }

        #[unsafe(method(unmarkText))]
        fn unmark_text(&self) {
            self.ivars().marked.borrow_mut().clear();
            self.ivars().overlay_dirty.set(true);
            self.request_frame();
        }

        #[unsafe(method(hasMarkedText))]
        fn has_marked_text(&self) -> bool {
            !self.ivars().marked.borrow().is_empty()
        }

        #[unsafe(method(markedRange))]
        fn marked_range(&self) -> NSRange {
            let marked = self.ivars().marked.borrow();
            if marked.is_empty() {
                NSRange::new(NSNotFound as NSUInteger, 0)
            } else {
                NSRange::new(0, marked.encode_utf16().count())
            }
        }

        #[unsafe(method(selectedRange))]
        fn selected_range(&self) -> NSRange {
            NSRange::new(NSNotFound as NSUInteger, 0)
        }

        #[unsafe(method_id(validAttributesForMarkedText))]
        fn valid_attributes_for_marked_text(&self) -> Retained<NSArray<NSAttributedStringKey>> {
            NSArray::new()
        }

        #[unsafe(method_id(attributedSubstringForProposedRange:actualRange:))]
        fn attributed_substring(&self, _range: NSRange, _actual: NSRangePointer) -> Option<Retained<NSAttributedString>> {
            None
        }

        #[unsafe(method(characterIndexForPoint:))]
        fn character_index_for_point(&self, _point: NSPoint) -> NSUInteger {
            NSNotFound as NSUInteger
        }

        #[unsafe(method(firstRectForCharacterRange:actualRange:))]
        fn first_rect_for_character_range(&self, _range: NSRange, _actual: NSRangePointer) -> NSRect {
            let rect = self.convertRect_toView(self.ivars().cursor_rect.get(), None);
            self.window().map_or(rect, |w| w.convertRectToScreen(rect))
        }

        #[unsafe(method(doCommandBySelector:))]
        fn do_command_by_selector(&self, _selector: Sel) {
            // Keys AppKit maps to editing commands (Enter, arrows, ctrl
            // chords) are sent by keyDown; doing nothing here also avoids
            // NSResponder's beep.
        }
    }

    unsafe impl NSWindowDelegate for TerminalView {
        #[unsafe(method(windowDidBecomeKey:))]
        fn window_did_become_key(&self, _notification: &NSNotification) {
            self.set_focused(true);
        }

        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _notification: &NSNotification) {
            self.set_focused(false);
        }

        #[unsafe(method(windowDidChangeOcclusionState:))]
        fn window_did_change_occlusion_state(&self, _notification: &NSNotification) {
            self.update_on_screen();
        }

        #[unsafe(method(windowDidMove:))]
        fn window_did_move(&self, _notification: &NSNotification) {
            self.update_on_screen();
        }

        #[unsafe(method(windowDidChangeScreen:))]
        fn window_did_change_screen(&self, _notification: &NSNotification) {
            self.update_on_screen();
        }

        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            // The link retains the view; invalidating it breaks the cycle.
            if let Some(link) = self.ivars().link.get() {
                link.invalidate();
            }
            // Dropping the panes hangs up their shells and joins their threads.
            let window = self.ivars().window;
            with_app(|app| app.mux.close_window(window));
            state::remove_view(window);
        }
    }
);

impl TerminalView {
    /// A view for mux window `window`. Call [`TerminalView::start`] once it
    /// is in its NSWindow.
    pub fn new(
        mtm: MainThreadMarker,
        frame: NSRect,
        window: WindowId,
        layout: Layout,
        device: &ProtocolObject<dyn MTLDevice>,
        scale: f64,
    ) -> Retained<Self> {
        let surface = Surface::new(device, scale);
        let spinner_surface = Surface::transparent(device, scale);
        spinner_surface.layer().setHidden(true);
        surface.layer().addSublayer(spinner_surface.layer());
        let ivars = Ivars {
            window,
            surface: RefCell::new(surface),
            spinner_surface: RefCell::new(spinner_surface),
            spinner: RefCell::new(None),
            on_screen: Cell::new(false),
            live: Cell::new(Live::default()),
            input_at: Cell::new(Instant::now()),
            cursor_blinks: Cell::new(false),
            shown: Cell::new(Shown::ALL),
            link: OnceCell::new(),
            layout: Cell::new(layout),
            tab_cells: RefCell::new(Vec::new()),
            hover_tab: Cell::new(None),
            focused: Cell::new(false),
            marked: RefCell::new(String::new()),
            key_texts: RefCell::new(None),
            overlay_dirty: Cell::new(true),
            cursor_rect: Cell::new(NSRect::ZERO),
            scroll_remainder: Cell::new(0.0),
            mouse_pane: Cell::new(None),
            selecting: Cell::new(false),
            drag: Cell::new(None),
            autoscrolling: Cell::new(false),
            hover: RefCell::new(None),
            quick: RefCell::new(None),
            pane_labels: RefCell::new(Vec::new()),
        };
        let this = Self::alloc(mtm).set_ivars(ivars);
        let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        view.setWantsLayer(true);
        view.setLayerContentsRedrawPolicy(NSViewLayerContentsRedrawPolicy::Never);
        view
    }

    pub fn mux_window(&self) -> WindowId {
        self.ivars().window
    }

    /// Starts frame pacing. Call once the view is in its window.
    pub fn start(&self) {
        // SAFETY: `step:` is defined above and takes the link.
        let link = unsafe { self.displayLinkWithTarget_selector(self, sel!(step:)) };
        // SAFETY: the main run loop and a valid mode.
        unsafe { link.addToRunLoop_forMode(&NSRunLoop::mainRunLoop(), NSRunLoopCommonModes) };
        let _ = self.ivars().link.set(link);
        // Pointer motion over the view, for links and programs that track it.
        let options = NSTrackingAreaOptions::MouseMoved
            | NSTrackingAreaOptions::MouseEnteredAndExited
            | NSTrackingAreaOptions::ActiveInKeyWindow
            | NSTrackingAreaOptions::InVisibleRect;
        // SAFETY: the view outlives its tracking area, which it owns.
        let area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(
                NSTrackingArea::alloc(),
                NSRect::ZERO,
                options,
                Some(self),
                None,
            )
        };
        self.addTrackingArea(&area);
        self.relayout();
        self.tabs_changed();
        self.update_on_screen();
    }

    /// Draws on the next display refresh.
    pub fn request_frame(&self) {
        if let Some(link) = self.ivars().link.get() {
            link.setPaused(false);
        }
    }

    /// Shows the tabs and title as they are now.
    pub fn tabs_changed(&self) {
        let window = self.ivars().window;
        let title = with_app(|app| {
            let tabs = app.mux.tabs(window);
            let active = tabs.iter().position(|t| t.active)?;
            let title = tabs[active].info.title();
            let title = if title.is_empty() { "slutty" } else { title };
            // AeroSpace and rho find windows by this title.
            Some(if tabs.len() > 1 { format!("[{}/{}] {title}", active + 1, tabs.len()) } else { title.to_owned() })
        })
        .flatten();
        if let (Some(title), Some(window)) = (title, self.window()) {
            window.setTitle(&NSString::from_str(&title));
        }
        self.ivars().overlay_dirty.set(true);
        self.request_frame();
    }

    pub fn close_window(&self) {
        if let Some(window) = self.window() {
            window.close();
        }
    }

    /// Fits the panes to the view after a resize, scale or font change, and
    /// draws at once so the window never shows a stretched frame.
    pub fn relayout(&self) {
        let ivars = self.ivars();
        let size = self.bounds().size;
        let window = ivars.window;
        let layout = with_app(|app| {
            let m = app.renderer.metrics();
            let layout = Layout::new(size, app.scale, m, app.config().window.padding, app.tab_bar);
            app.mux.resize(window, layout.area);
            layout
        });
        let Some(layout) = layout else { return };
        ivars.layout.set(layout);
        let size = NSSize::new(f64::from(layout.width), f64::from(layout.height));
        ivars.surface.borrow().layer().setDrawableSize(size);
        self.place_spinner_surface(layout);
        ivars.overlay_dirty.set(true);
        self.draw();
    }

    /// Lays the spinner surface over the tab bar row, hidden until it is
    /// drawn again.
    fn place_spinner_surface(&self, layout: Layout) {
        let ivars = self.ivars();
        let surface = ivars.surface.borrow();
        let spinner = ivars.spinner_surface.borrow();
        let scale = surface.layer().contentsScale();
        let height = f64::from(layout.area.cell_height);
        let size = CGSize::new(f64::from(layout.width) / scale, height / scale);
        let bounds = surface.layer().bounds();
        let y = if surface.layer().isGeometryFlipped() { 0.0 } else { bounds.size.height - size.height };
        // Moved, resized and hidden at once, not animated.
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        spinner.layer().setFrame(CGRect::new(CGPoint::new(0.0, y), size));
        spinner.layer().setHidden(true);
        CATransaction::commit();
        spinner.layer().setDrawableSize(CGSize::new(f64::from(layout.width), height));
        *ivars.spinner.borrow_mut() = None;
    }

    /// Draws if a visible pane or the window's own chrome changed. Returns
    /// whether it drew.
    fn draw(&self) -> bool {
        let ivars = self.ivars();
        let window = ivars.window;
        let layout = ivars.layout.get();
        let overlay_dirty = ivars.overlay_dirty.replace(false);
        let focused = ivars.focused.get();
        let marked = ivars.marked.borrow();
        let drawn = with_app(|app| {
            let App { mux, renderer, settings, .. } = app;
            let config = &settings.config;
            let m = renderer.metrics();
            let mut chrome = Vec::new();
            let mut spinner = None;
            if layout.tab_bar {
                let cols = (layout.width / m.cell_width) as usize;
                let bar = tab_bar::build(
                    &mux.tabs(window),
                    &config.theme,
                    cols,
                    config.tab_bar.max_width,
                    layout.width as f32,
                    focused,
                    ivars.hover_tab.get(),
                );
                *ivars.tab_cells.borrow_mut() = bar.tabs;
                if !bar.spinners.is_empty() && settings.error.is_none() {
                    let frame = ticker::phase(Live::default(), Instant::now()).spinner;
                    spinner = Some(SpinnerFrame { spinners: bar.spinners, background: bar.line.background, frame });
                }
                chrome.push(bar.line);
            }
            if let Some(err) = &settings.error {
                chrome.push(banner(err, config, layout));
            }
            let active = mux.active_pane(window);
            // Read before the frames, which hold the panes borrowed.
            let modes: Vec<(PaneId, PaneModes)> = mux
                .layout(window)
                .panes
                .iter()
                .filter_map(|(id, _)| {
                    let pane = mux.pane_mut(*id)?;
                    let highlights = pane.search_highlights();
                    Some((*id, PaneModes { highlights, search: pane.search_status(), copy_cursor: pane.copy_cursor() }))
                })
                .collect();
            let Visible { panes: visible, dividers } = mux.visible(window);
            if !overlay_dirty && !visible.iter().any(|(_, _, frame)| frame.changed) {
                return None;
            }
            let theme = &config.theme;
            let cell = |x: u16, y: u16| {
                [layout.origin[0] + u32::from(x) * m.cell_width, layout.origin[1] + u32::from(y) * m.cell_height]
                    .map(|v| v as f32)
            };
            let (cw, ch) = (m.cell_width as f32, m.cell_height as f32);
            let mut rects = Vec::new();
            let split = visible.len() > 1;
            let [r, g, b] = theme.split;
            let stroke = m.stroke.max(1) as f32;
            for (axis, rect) in &dividers {
                let [x, y] = cell(rect.x, rect.y);
                let (pos, size) = match axis {
                    // A vertical line down the middle of the gap column.
                    Axis::Row => ([x + (cw - stroke) / 2.0, y], [stroke, f32::from(rect.rows) * ch]),
                    Axis::Column => ([x, y + (ch - stroke) / 2.0], [f32::from(rect.cols) * cw, stroke]),
                };
                rects.push(RectInstance::new(pos, size, [r, g, b, 255]));
            }
            for (label, pane) in ivars.pane_labels.borrow().iter() {
                let Some((_, rect, _)) = visible.iter().find(|(id, ..)| id == pane) else { continue };
                let [x, y] = cell(rect.x + rect.cols.saturating_sub(3) / 2, rect.y + rect.rows / 2);
                let mut line = Line::new(x, y, 3.0 * cw, rgb(theme.ansi[4]));
                line.push_str(&format!(" {label} "), rgb(theme.background), BOLD);
                chrome.push(line);
            }
            let span_rect = |rect: Rect, row: u16, start: u16, end: u16, color: [u8; 4]| {
                let size = [f32::from(end.saturating_sub(start)) * cw, ch];
                RectInstance::new(cell(rect.x + start, rect.y + row), size, color)
            };
            let tint = |c: config::Rgb, alpha: u8| [c[0], c[1], c[2], alpha];
            for (id, modes) in &modes {
                let Some(&(_, rect, _)) = visible.iter().find(|(p, ..)| p == id) else { continue };
                for h in &modes.highlights {
                    let color = if h.current { tint(theme.ansi[3], 128) } else { tint(theme.ansi[4], 80) };
                    rects.push(span_rect(rect, h.row, h.start, h.end, color));
                }
                if let Some(search) = &modes.search {
                    let [x, y] = cell(rect.x, rect.y + rect.rows.saturating_sub(1));
                    let mut line = Line::new(x, y, f32::from(rect.cols) * cw, rgb(theme.selection_bg));
                    let text: String = search_bar(search).chars().take(usize::from(rect.cols)).collect();
                    line.push_str(&text, rgb(theme.foreground), CellStyle::default());
                    chrome.push(line);
                }
            }
            if let Some(quick) = &*ivars.quick.borrow()
                && let Some(&(_, rect, _)) = visible.iter().find(|(p, ..)| *p == quick.pane)
            {
                for m in quick.shown() {
                    let span = &m.span;
                    rects.push(span_rect(rect, span.row, span.start, span.end, tint(theme.ansi[4], 80)));
                    let [x, y] = cell(rect.x + span.start, rect.y + span.row);
                    let width = m.label.chars().count() as f32 * cw;
                    let mut line = Line::new(x, y, width, rgb(theme.ansi[3]));
                    line.push_str(&m.label, rgb(theme.background), BOLD);
                    chrome.push(line);
                }
            }
            if let Some(hover) = &*ivars.hover.borrow()
                && let Some(&(_, rect, _)) = visible.iter().find(|(p, ..)| *p == hover.pane)
            {
                let [x, y] = cell(rect.x + hover.start, rect.y + hover.row);
                let width = f32::from(hover.end.saturating_sub(hover.start)) * cw;
                let [r, g, b] = theme.foreground;
                let pos = [x, y + m.underline_offset as f32];
                rects.push(RectInstance::new(pos, [width, stroke], [r, g, b, 255]));
            }
            let mut cursor = None;
            let mut cursor_blinks = false;
            let mut panes: Vec<PaneDraw<'_>> = visible
                .into_iter()
                .map(|(id, rect, frame)| {
                    let is_active = Some(id) == active;
                    let origin = cell(rect.x, rect.y);
                    if is_active && let Some(c) = frame.cursor {
                        cursor = Some([origin[0] + f32::from(c.x) * cw, origin[1] + f32::from(c.y) * ch]);
                    }
                    let preedit = if is_active { marked.as_str() } else { "" };
                    let copy_cursor = modes.iter().find(|(p, _)| *p == id).and_then(|(_, modes)| modes.copy_cursor);
                    let overlay = Overlay { focused: focused && is_active, copy_cursor, preedit };
                    cursor_blinks |= is_active && overlay.cursor_blinks(frame.cursor);
                    let tint = if split && !is_active { INACTIVE } else { Tint::NONE };
                    PaneDraw { id, frame, origin, overlay, tint }
                })
                .collect();
            ivars.cursor_blinks.set(cursor_blinks);
            // Whether text blinks is known only once built; with none, `text`
            // hides nothing.
            let live = Live { text: ivars.on_screen.get() && focused, ..self.live() };
            let phase = ticker::phase(live, Instant::now());
            let shown = Shown { cursor: phase.cursor, text: phase.text };
            ivars.shown.set(shown);
            let surface = &mut ivars.surface.borrow_mut();
            renderer.render(surface, &mut panes, &rects, &chrome, Some(rgb(theme.background)), shown);
            self.show_spinner(renderer, spinner);
            Some((cursor, m))
        })
        .flatten();
        self.note_live();
        let Some((cursor, m)) = drawn else { return false };
        log::trace!("frame presented");
        if let Some([x, y]) = cursor {
            let s = self.window().map_or(1.0, |w| w.backingScaleFactor());
            ivars.cursor_rect.set(NSRect::new(
                NSPoint::new(f64::from(x) / s, f64::from(y) / s),
                NSSize::new(f64::from(m.cell_width) / s, f64::from(m.cell_height) / s),
            ));
        }
        true
    }

    /// Draws `spinner` on its surface if it differs from what is there, or
    /// hides the surface for `None`.
    fn show_spinner(&self, renderer: &mut Renderer, spinner: Option<SpinnerFrame>) {
        let ivars = self.ivars();
        if *ivars.spinner.borrow() == spinner {
            return;
        }
        let surface = &mut ivars.spinner_surface.borrow_mut();
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        surface.layer().setHidden(spinner.is_none());
        CATransaction::commit();
        if let Some(spinner) = &spinner {
            let lines = spinner.lines(renderer.metrics().cell_width as f32);
            renderer.render(surface, &mut [], &[], &lines, None, Shown::ALL);
            log::trace!("spinner presented");
        }
        *ivars.spinner.borrow_mut() = spinner;
    }

    /// The zones animating in this window now.
    pub fn live(&self) -> Live {
        let ivars = self.ivars();
        let on_screen = ivars.on_screen.get();
        Live {
            spinner: on_screen && ivars.spinner.borrow().is_some(),
            cursor: (on_screen && ivars.focused.get() && ivars.cursor_blinks.get()).then(|| ivars.input_at.get()),
            text: on_screen && ivars.focused.get() && ivars.surface.borrow().blinks_text(),
        }
    }

    /// Arms the timer again if the live zones changed.
    fn note_live(&self) {
        let live = self.live();
        if self.ivars().live.replace(live) != live {
            ticker::reschedule();
        }
    }

    /// Brings the live zones to what they show at `now`, drawing only those
    /// that changed.
    pub fn animate(&self, now: Instant) {
        let ivars = self.ivars();
        let live = self.live();
        let phase = ticker::phase(live, now);
        let spinner = ivars.spinner.borrow().clone();
        if let Some(spinner) = spinner
            && live.spinner
            && spinner.frame != phase.spinner
        {
            let spinner = SpinnerFrame { frame: phase.spinner, ..spinner };
            with_app(|app| self.show_spinner(&mut app.renderer, Some(spinner)));
        }
        let shown = Shown { cursor: phase.cursor, text: phase.text };
        if shown != ivars.shown.get() {
            let redrawn = with_app(|app| app.renderer.redraw(&ivars.surface.borrow(), shown));
            if redrawn == Some(true) {
                ivars.shown.set(shown);
                log::trace!("blink presented");
            } else {
                ivars.overlay_dirty.set(true);
                self.draw();
            }
        }
    }

    /// Shows a blinking cursor and starts its phase and cut-off over.
    fn restart_blink(&self) {
        let now = Instant::now();
        self.ivars().input_at.set(now);
        self.animate(now);
        self.note_live();
    }

    /// Reads whether the window shows on a screen: not covered, minimized or
    /// hidden, and with at least a cell of it on a display. AeroSpace parks
    /// windows of other workspaces in a screen corner, which counts as
    /// visible to macOS.
    fn update_on_screen(&self) {
        let on_screen = self.window().is_some_and(|window| {
            let visible = window.occlusionState().contains(NSWindowOcclusionState::Visible);
            let frame = window.frame();
            let area = self.ivars().layout.get().area;
            let scale = window.backingScaleFactor();
            let cell = (f64::from(area.cell_width) / scale, f64::from(area.cell_height) / scale);
            let shown = NSScreen::screens(self.mtm()).iter().any(|screen| {
                let s = screen.frame();
                let width = (frame.max().x.min(s.max().x) - frame.min().x.max(s.min().x)).max(0.0);
                let height = (frame.max().y.min(s.max().y) - frame.min().y.max(s.min().y)).max(0.0);
                width >= cell.0 && height >= cell.1
            });
            visible && shown
        });
        if self.ivars().on_screen.replace(on_screen) != on_screen {
            log::trace!("window on screen: {on_screen}");
            self.animate(Instant::now());
            self.note_live();
        }
    }

    fn set_focused(&self, focused: bool) {
        self.ivars().focused.set(focused);
        if focused {
            self.ivars().input_at.set(Instant::now());
        }
        let window = self.ivars().window;
        with_app(|app| app.mux.focus(window, focused));
        self.ivars().overlay_dirty.set(true);
        self.request_frame();
    }

    /// A point of a mouse event in device pixels from the view's corner.
    pub(super) fn pixel_point(&self, event: &NSEvent) -> [f64; 2] {
        let point = self.convertPoint_fromView(event.locationInWindow(), None);
        let s = self.window().map_or(1.0, |w| w.backingScaleFactor());
        [point.x * s, point.y * s]
    }

    /// The pane under a point, or the one a mouse button went down in, with
    /// the point relative to that pane.
    pub(super) fn pane_at(&self, app: &mut App, [x, y]: [f64; 2]) -> Option<(PaneId, f32, f32)> {
        let ivars = self.ivars();
        let layout = ivars.layout.get();
        let m = app.renderer.metrics();
        let panes = app.mux.layout(ivars.window).panes;
        let corner = |rect: Rect| {
            let x = f64::from(layout.origin[0] + u32::from(rect.x) * m.cell_width);
            let y = f64::from(layout.origin[1] + u32::from(rect.y) * m.cell_height);
            (
                x,
                y,
                x + f64::from(u32::from(rect.cols) * m.cell_width),
                y + f64::from(u32::from(rect.rows) * m.cell_height),
            )
        };
        let (pane, rect) = match ivars.mouse_pane.get() {
            Some(captured) => *panes.iter().find(|(id, _)| *id == captured)?,
            None => *panes.iter().find(|(_, rect)| {
                let (left, top, right, bottom) = corner(*rect);
                (left..right).contains(&x) && (top..bottom).contains(&y)
            })?,
        };
        let (left, top, ..) = corner(rect);
        Some((pane, (x - left) as f32, (y - top) as f32))
    }

    /// Activates the tab under a click in the tab bar. Returns whether the
    /// click was in the tab bar.
    pub(super) fn click_tab_bar(&self, event: &NSEvent) -> bool {
        let Some(tab) = self.tab_at(self.pixel_point(event)) else { return false };
        if let Some(tab) = tab {
            let window = self.ivars().window;
            with_app(|app| app.mux.activate_tab(window, tab));
            self.tabs_changed();
        }
        true
    }

    /// The tab under a point, or `None` outside the tab bar.
    pub(super) fn tab_at(&self, [x, y]: [f64; 2]) -> Option<Option<usize>> {
        let cell = self.ivars().layout.get().tab_bar_cell(x, y)?;
        Some(self.ivars().tab_cells.borrow().iter().position(|cells| cells.contains(&cell)))
    }

    pub(super) fn set_hover_tab(&self, tab: Option<usize>) {
        if self.ivars().hover_tab.replace(tab) != tab {
            self.ivars().overlay_dirty.set(true);
            self.request_frame();
        }
    }

    fn key_equivalent(&self, event: &NSEvent) -> bool {
        let focused =
            self.window().is_some_and(|w| w.firstResponder().is_some_and(|r| std::ptr::eq(&*r, self.as_ref())));
        if !focused {
            return false;
        }
        if self.bind_key(event) {
            return true;
        }
        let flags = event.modifierFlags();
        let ctrl = flags.contains(NSEventModifierFlags::Control) && !flags.contains(NSEventModifierFlags::Command);
        if ctrl {
            self.handle_key_down(event);
        }
        ctrl
    }

    /// Runs the binding or the pane mode this key press belongs to. Returns
    /// whether either used the key, which then does not reach the program.
    fn bind_key(&self, event: &NSEvent) -> bool {
        if !self.ivars().marked.borrow().is_empty() {
            return false;
        }
        if self.pick_pane(event) || self.quick_select_key(event) {
            return true;
        }
        let typed = |flags| event.charactersByApplyingModifiers(flags).and_then(|s| s.to_string().chars().next());
        let code = event.keyCode();
        let flags = event.modifierFlags();
        let Some(press) = keys::press(code, flags, typed(NSEventModifierFlags(0)), typed(NSEventModifierFlags::Shift))
        else {
            return false;
        };
        let window = self.ivars().window;
        // Copy mode and search take keys first, as WezTerm's key tables do,
        // unless a binding is half typed.
        let mode = with_app(|app| {
            if !app.matcher.is_idle() {
                return None;
            }
            Some(app.mux.pane_mut(app.mux.active_pane(window)?)?.mode_key(press))
        });
        match mode.flatten() {
            Some(ModeKey::Used) => {
                self.swallow(code);
                self.request_frame();
                return true;
            }
            Some(ModeKey::Closed(text)) => {
                self.swallow(code);
                if let Some(text) = text {
                    actions::set_clipboard(&text);
                }
                self.ivars().overlay_dirty.set(true);
                self.request_frame();
                return true;
            }
            Some(ModeKey::Unused) | None => {}
        }
        let outcome = with_app(|app| {
            let outcome = app.matcher.press(&app.settings.config.keys, press);
            log::trace!("{press:?}: {outcome:?}");
            outcome
        });
        let used = match outcome {
            Some(Outcome::Run(action)) => {
                self.run_action(action);
                true
            }
            Some(Outcome::Handled) => true,
            // A mode keeps the keys it does not know from the program.
            Some(Outcome::Unbound) | None => with_app(|app| {
                app.mux.active_pane(window).and_then(|p| app.mux.pane_mut(p)).is_some_and(|p| p.in_mode())
            })
            .unwrap_or(false),
        };
        if used {
            self.swallow(code);
        }
        used
    }

    /// Keeps the release of a key a binding used from the program.
    pub(super) fn swallow(&self, code: u16) {
        with_app(|app| {
            // No key-up comes for keys pressed with command, so keep few.
            app.swallowed.retain(|&c| c != code);
            app.swallowed.push(code);
            if app.swallowed.len() > 8 {
                app.swallowed.remove(0);
            }
        });
    }

    fn option_is_alt(&self, flags: NSEventModifierFlags) -> bool {
        let setting = with_app(|app| app.config().terminal.option_as_alt).unwrap_or(OptionAsAlt::None);
        let (left, right) = keys::option_sides(flags);
        match setting {
            OptionAsAlt::None => false,
            OptionAsAlt::Left => left && !right,
            OptionAsAlt::Right => right && !left,
            OptionAsAlt::Both => left || right,
        }
    }

    fn handle_key_down(&self, event: &NSEvent) {
        let ivars = self.ivars();
        let action = if event.isARepeat() { key::Action::Repeat } else { key::Action::Press };
        let flags = event.modifierFlags();
        let composing = !ivars.marked.borrow().is_empty();

        if !composing && self.option_is_alt(flags) {
            // An Alt chord skips the input method: its text is the key
            // without Option, which the encoder prefixes with ESC.
            let without = NSEventModifierFlags(flags.0 & !NSEventModifierFlags::Option.0);
            let text = event.charactersByApplyingModifiers(without).map(|s| s.to_string());
            let text = text.filter(|t| keys::is_text(t));
            let consumed = keys::mods(flags) & Mods::SHIFT;
            self.send_key(event, action, text.as_deref(), if text.is_some() { consumed } else { Mods::empty() });
            return;
        }

        *ivars.key_texts.borrow_mut() = Some(Vec::new());
        let events = NSArray::from_slice(&[event]);
        self.interpretKeyEvents(&events);
        let texts = ivars.key_texts.borrow_mut().take().unwrap_or_default();

        if composing {
            // Text the input method commits belongs to no key.
            for text in &texts {
                self.send_text(text);
            }
        } else if !texts.is_empty() {
            // Shift and a composing Option made the text.
            let consumed = keys::mods(flags) & (Mods::SHIFT | Mods::ALT | Mods::ALT_SIDE);
            for text in texts.iter().filter(|t| keys::is_text(t)) {
                self.send_key(event, action, Some(text), consumed);
            }
            if texts.iter().any(|t| !keys::is_text(t)) {
                self.send_key(event, action, None, Mods::empty());
            }
        } else if ivars.marked.borrow().is_empty() {
            self.send_key(event, action, None, Mods::empty());
        }
        // Otherwise the input method used the key.
    }

    fn send_key(&self, event: &NSEvent, action: key::Action, text: Option<&str>, consumed: Mods) {
        let unshifted = event
            .charactersByApplyingModifiers(NSEventModifierFlags(0))
            .and_then(|s| s.to_string().chars().next())
            .filter(|c| keys::is_text(c.encode_utf8(&mut [0; 4])));
        let input = KeyInput {
            key: keys::key(event.keyCode()),
            action,
            mods: keys::mods(event.modifierFlags()),
            consumed,
            text,
            unshifted,
        };
        self.with_active_pane(|pane| pane.key(&input));
    }

    fn send_text(&self, text: &str) {
        let input = KeyInput {
            key: Key::Unidentified,
            action: key::Action::Press,
            mods: Mods::empty(),
            consumed: Mods::empty(),
            text: Some(text),
            unshifted: None,
        };
        self.with_active_pane(|pane| pane.key(&input));
    }

    /// Runs `f` on the active pane, then draws: input scrolls to the bottom.
    pub fn with_active_pane(&self, f: impl FnOnce(&mut crate::mux::Pane)) {
        let window = self.ivars().window;
        with_app(|app| {
            if let Some(pane) = app.mux.active_pane(window) {
                pane_mut(app, pane, f);
            }
        });
        self.request_frame();
    }

    pub(super) fn send_mouse(&self, event: &NSEvent, action: mouse::Action, button: Option<mouse::Button>) {
        let mods = keys::mods(event.modifierFlags());
        let point = self.pixel_point(event);
        with_app(|app| {
            let Some((pane, x, y)) = self.pane_at(app, point) else { return };
            pane_mut(app, pane, |p| {
                if p.mouse_tracking() {
                    p.mouse(&MouseInput { action, button, mods, x, y });
                }
            });
        });
    }
}

/// Labels for pane select, as in Lucas's WezTerm setup.
const PANE_LABELS: &str = "asdfghjkl;";

impl TerminalView {
    /// Labels the visible panes; the next key picks one.
    pub(super) fn start_pane_select(&self) {
        let window = self.ivars().window;
        let Some(layout) = with_app(|app| app.mux.layout(window)) else { return };
        *self.ivars().pane_labels.borrow_mut() =
            PANE_LABELS.chars().zip(layout.panes.iter().map(|(id, _)| *id)).collect();
        self.ivars().overlay_dirty.set(true);
        self.request_frame();
    }

    /// While picking a pane, takes the key: a label activates its pane, and
    /// any other key cancels. Returns whether it took the key.
    fn pick_pane(&self, event: &NSEvent) -> bool {
        let labels = std::mem::take(&mut *self.ivars().pane_labels.borrow_mut());
        if labels.is_empty() {
            return false;
        }
        let typed = event.charactersIgnoringModifiers().and_then(|s| s.to_string().chars().next());
        let window = self.ivars().window;
        let code = event.keyCode();
        with_app(|app| {
            app.swallowed.push(code);
            if let Some((_, pane)) = labels.iter().find(|(label, _)| Some(*label) == typed) {
                app.mux.activate_pane(window, *pane);
            }
        });
        self.tabs_changed();
        true
    }
}

fn pane_mut(app: &mut App, pane: PaneId, f: impl FnOnce(&mut crate::mux::Pane)) {
    if let Some(pane) = app.mux.pane_mut(pane) {
        f(pane);
    }
}

/// The search bar's text: the mode, the needle with a block for a cursor,
/// then where search is.
fn search_bar(search: &SearchStatus) -> String {
    let mode = if search.regex { "Regex" } else { "Search" };
    let count = match search.current {
        _ if search.invalid => "invalid".to_owned(),
        Some(current) => format!("{current}/{}", search.total),
        None => format!("{} found", search.total),
    };
    format!(" {mode}: {}\u{2588}  {count}", search.needle)
}

/// A line across the top that says why the config file is not in effect.
fn banner(error: &str, config: &Config, layout: Layout) -> Line {
    let theme = &config.theme;
    let mut line = Line::new(0.0, 0.0, layout.width as f32, rgb(theme.ansi[1]));
    let fits = (layout.width / layout.area.cell_width) as usize;
    let text: String = format!(" {error}").chars().take(fits).collect();
    line.push_str(&text, rgb(theme.background), BOLD);
    line
}

/// What a pane's modes put on screen.
struct PaneModes {
    highlights: Vec<Highlight>,
    search: Option<SearchStatus>,
    copy_cursor: Option<(u16, u16)>,
}

const BOLD: CellStyle = CellStyle {
    bold: true,
    italic: false,
    faint: false,
    underline: false,
    strikethrough: false,
    blink: false,
    wide: false,
};

/// Panes without the keyboard, as WezTerm's default `inactive_pane_hsb`.
const INACTIVE: Tint = Tint { saturation: 0.9, brightness: 0.8 };

fn rgb([r, g, b]: config::Rgb) -> Rgb {
    Rgb { r, g, b }
}

/// The text of an NSString or NSAttributedString from the input system.
fn string_of(object: &AnyObject) -> String {
    if let Some(s) = object.downcast_ref::<NSString>() {
        s.to_string()
    } else if let Some(s) = object.downcast_ref::<NSAttributedString>() {
        s.string().to_string()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_search_bar_names_its_mode() {
        let text = SearchStatus { needle: "ab".into(), current: Some(2), total: 5, regex: false, invalid: false };
        assert_eq!(search_bar(&text), " Search: ab\u{2588}  2/5");
        let regex = SearchStatus { needle: "a+".into(), regex: true, ..SearchStatus::default() };
        assert_eq!(search_bar(&regex), " Regex: a+\u{2588}  0 found");
        let invalid = SearchStatus { needle: "(".into(), regex: true, invalid: true, ..SearchStatus::default() };
        assert_eq!(search_bar(&invalid), " Regex: (\u{2588}  invalid");
    }
}
