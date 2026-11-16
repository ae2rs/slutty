//! The terminal view: input, IME, drawing and the wake-ups from PTY threads.
//!
//! All state lives on the main thread. PTY reader threads only dispatch
//! [`wake`] to the main queue; a display link that is paused while nothing
//! changes paces the frames.

use std::cell::{Cell, OnceCell, RefCell};
use std::time::Instant;

use dispatch2::{DispatchQueue, DispatchTime};
use libghostty_vt::key::{self, Key, Mods};
use libghostty_vt::mouse;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSEvent, NSEventModifierFlags, NSPasteboard, NSPasteboardTypeString, NSResponder, NSTextInputClient, NSView,
    NSViewLayerContentsRedrawPolicy, NSWindowDelegate,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSAttributedStringKey, NSNotFound, NSNotification, NSObject, NSPoint, NSRange,
    NSRangePointer, NSRect, NSRunLoop, NSRunLoopCommonModes, NSSize, NSString, NSUInteger,
};
use objc2_quartz_core::{CADisplayLink, CALayer, CAMetalLayer, kCAGravityTopLeft};

use super::keys;
use crate::config::{Config, OptionAsAlt};
use crate::font::{Fonts, Metrics};
use crate::mux::{Event, Mux};
use crate::renderer::{Overlay, PIXEL_FORMAT, Renderer};
use crate::terminal::{Geometry, KeyInput, MouseInput};

thread_local! {
    /// The window's view, for wake-ups that arrive through the main queue.
    static VIEW: RefCell<Option<Retained<TerminalView>>> = const { RefCell::new(None) };
}

/// Runs on a PTY reader thread when output is waiting.
pub fn wake_from_any_thread() {
    DispatchQueue::main().exec_async(|| with_view(|view| view.pump()));
}

fn with_view(f: impl FnOnce(&TerminalView)) {
    // Cloned out, so `f` may close the window, which clears VIEW.
    if let Some(view) = VIEW.with(|v| v.borrow().clone()) {
        f(&view);
    }
}

struct State {
    mux: Mux,
    renderer: Renderer,
    config: Config,
    scale: f64,
    focused: bool,
    /// Fractions of a line left over from trackpad scrolling.
    scroll_remainder: f64,
}

pub struct Ivars {
    state: RefCell<Option<State>>,
    layer: Retained<CAMetalLayer>,
    link: OnceCell<Retained<CADisplayLink>>,
    /// Text the input method is composing.
    marked: RefCell<String>,
    /// Text committed while `keyDown` runs the input method; `None` outside.
    key_texts: RefCell<Option<Vec<String>>>,
    /// Focus or preedit changed: draw even if the terminal did not.
    overlay_dirty: Cell<bool>,
    /// The cursor cell in view coordinates, for placing the IME window.
    cursor_rect: Cell<NSRect>,
    hold_timer: Cell<bool>,
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
            Retained::into_super(self.ivars().layer.clone())
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
            let changed = self.with_state(|state| {
                if state.scale == scale {
                    return false;
                }
                match Fonts::new(state.config.font.size * scale) {
                    Ok(fonts) => state.renderer.set_fonts(fonts),
                    Err(err) => log::error!("loading fonts failed: {err}"),
                }
                state.scale = scale;
                true
            });
            if changed == Some(true) {
                self.ivars().layer.setContentsScale(scale);
                self.relayout();
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
            self.handle_key_down(event);
        }

        #[unsafe(method(keyUp:))]
        fn key_up(&self, event: &NSEvent) {
            if self.ivars().marked.borrow().is_empty() {
                self.send_key(event, key::Action::Release, None, Mods::empty());
            }
        }

        // Control chords reach performKeyEquivalent before keyDown, and some
        // (ctrl+tab, ctrl+esc) never reach keyDown at all.
        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
            let flags = event.modifierFlags();
            let focused = self.window().is_some_and(|w| w.firstResponder().is_some_and(|r| std::ptr::eq(&*r, self.as_ref())));
            let ours = focused
                && flags.contains(NSEventModifierFlags::Control)
                && !flags.contains(NSEventModifierFlags::Command);
            if ours {
                self.handle_key_down(event);
            }
            ours
        }

        #[unsafe(method(paste:))]
        fn paste(&self, _sender: Option<&AnyObject>) {
            let pasteboard = NSPasteboard::generalPasteboard();
            if let Some(text) = pasteboard.stringForType(unsafe { NSPasteboardTypeString }) {
                self.with_state(|s| s.mux.paste(&text.to_string()));
                self.request_frame();
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.send_mouse(event, mouse::Action::Press, Some(mouse::Button::Left));
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            self.send_mouse(event, mouse::Action::Release, Some(mouse::Button::Left));
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            self.send_mouse(event, mouse::Action::Motion, Some(mouse::Button::Left));
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
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            let mods = keys::mods(event.modifierFlags());
            let dy = event.scrollingDeltaY();
            self.with_state(|state| {
                // Positive deltas scroll toward older output.
                let lines = if event.hasPreciseScrollingDeltas() {
                    let cell = f64::from(state.renderer.metrics().cell_height) / state.scale;
                    state.scroll_remainder -= dy / cell;
                    let lines = state.scroll_remainder.trunc();
                    state.scroll_remainder -= lines;
                    lines as isize
                } else if dy == 0.0 {
                    0
                } else {
                    -(dy.signum() as isize) * isize::try_from(state.config.mouse.wheel_lines).unwrap_or(1)
                };
                let (x, y) = ((point.x * state.scale) as f32, (point.y * state.scale) as f32);
                state.mux.scroll(lines, mods, x, y);
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

        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            // The link retains the view; invalidating it breaks the cycle.
            if let Some(link) = self.ivars().link.get() {
                link.invalidate();
            }
            // Dropping the mux hangs up the shell and joins its threads.
            let state = self.ivars().state.borrow_mut().take();
            drop(state);
            VIEW.with(|v| v.borrow_mut().take());
        }
    }
);

impl TerminalView {
    pub fn new(mtm: MainThreadMarker, frame: NSRect, renderer: Renderer, config: Config, scale: f64) -> Retained<Self> {
        let layer = CAMetalLayer::new();
        layer.setDevice(Some(renderer.device()));
        layer.setPixelFormat(PIXEL_FORMAT);
        layer.setFramebufferOnly(true);
        layer.setContentsScale(scale);
        // A stale frame during a resize stays put instead of stretching.
        layer.setContentsGravity(unsafe { kCAGravityTopLeft });

        let ivars = Ivars {
            state: RefCell::new(None),
            layer,
            link: OnceCell::new(),
            marked: RefCell::new(String::new()),
            key_texts: RefCell::new(None),
            overlay_dirty: Cell::new(true),
            cursor_rect: Cell::new(NSRect::ZERO),
            hold_timer: Cell::new(false),
        };
        let this = Self::alloc(mtm).set_ivars(ivars);
        let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        view.setWantsLayer(true);
        view.setLayerContentsRedrawPolicy(NSViewLayerContentsRedrawPolicy::Never);

        let geometry = geometry(frame.size, scale, renderer.metrics(), config.window.padding_cells);
        match Mux::spawn(geometry, &config, wake_from_any_thread) {
            Ok(mux) => {
                *view.ivars().state.borrow_mut() =
                    Some(State { mux, renderer, config, scale, focused: false, scroll_remainder: 0.0 });
            }
            Err(err) => log::error!("starting the shell failed: {err}"),
        }
        VIEW.with(|v| *v.borrow_mut() = Some(view.clone()));
        view
    }

    /// Starts frame pacing. Call once the view is in its window.
    pub fn start(&self) {
        // SAFETY: `step:` is defined above and takes the link.
        let link = unsafe { self.displayLinkWithTarget_selector(self, sel!(step:)) };
        // SAFETY: the main run loop and a valid mode.
        unsafe { link.addToRunLoop_forMode(&NSRunLoop::mainRunLoop(), NSRunLoopCommonModes) };
        let _ = self.ivars().link.set(link);
        self.relayout();
    }

    /// Draws on the next display refresh.
    fn request_frame(&self) {
        if let Some(link) = self.ivars().link.get() {
            link.setPaused(false);
        }
    }

    fn with_state<R>(&self, f: impl FnOnce(&mut State) -> R) -> Option<R> {
        self.ivars().state.borrow_mut().as_mut().map(f)
    }

    /// Parses waiting output and reacts to what the panes report.
    fn pump(&self) {
        let Some((pump, deadline)) = self.with_state(|s| (s.mux.pump(), s.mux.hold_deadline())) else { return };
        if pump.output {
            self.request_frame();
        }
        if pump.more {
            wake_from_any_thread();
        }
        if let Some(deadline) = deadline {
            self.schedule_hold_timeout(deadline);
        }
        for event in pump.events {
            match event {
                Event::TitleChanged(title) => {
                    if let Some(window) = self.window() {
                        window.setTitle(&NSString::from_str(&title));
                    }
                }
                Event::PaneClosed => {
                    if let Some(window) = self.window() {
                        window.close();
                    }
                }
            }
        }
    }

    /// Comes back when a synchronized-output hold expires, so a program that
    /// never releases it cannot freeze the screen.
    fn schedule_hold_timeout(&self, deadline: Instant) {
        if self.ivars().hold_timer.replace(true) {
            return;
        }
        let delay = deadline.saturating_duration_since(Instant::now());
        let when = DispatchTime::try_from(delay).unwrap_or(DispatchTime::NOW);
        let scheduled = DispatchQueue::main().after(when, || {
            with_view(|view| {
                view.ivars().hold_timer.set(false);
                view.request_frame();
            });
        });
        if scheduled.is_err() {
            self.ivars().hold_timer.set(false);
        }
    }

    /// Draws if the terminal or the overlay changed. Returns whether it drew.
    fn draw(&self) -> bool {
        let ivars = self.ivars();
        let overlay_dirty = ivars.overlay_dirty.replace(false);
        let marked = ivars.marked.borrow();
        let mut state = ivars.state.borrow_mut();
        let Some(state) = state.as_mut() else { return false };
        let geometry = state.mux.geometry();
        let mut frame = state.mux.frame();
        if !frame.changed && !overlay_dirty {
            return false;
        }
        let overlay = Overlay { focused: state.focused, preedit: &marked };
        let origin = [geometry.padding_left as f32, geometry.padding_top as f32];
        state.renderer.render(&ivars.layer, &mut frame, &overlay, origin);
        log::trace!("frame presented");

        if let Some(cursor) = frame.cursor {
            let m = state.renderer.metrics();
            let s = state.scale;
            ivars.cursor_rect.set(NSRect::new(
                NSPoint::new(
                    f64::from(geometry.padding_left + u32::from(cursor.x) * m.cell_width) / s,
                    f64::from(geometry.padding_top + u32::from(cursor.y) * m.cell_height) / s,
                ),
                NSSize::new(f64::from(m.cell_width) / s, f64::from(m.cell_height) / s),
            ));
        }
        true
    }

    /// Fits the grid to the view after a resize or scale change, and draws at
    /// once so the window never shows a stretched frame.
    fn relayout(&self) {
        let size = self.bounds().size;
        let resized = self.with_state(|state| {
            let geometry = geometry(size, state.scale, state.renderer.metrics(), state.config.window.padding_cells);
            self.ivars().layer.setDrawableSize(NSSize::new(f64::from(geometry.width), f64::from(geometry.height)));
            state.mux.resize(geometry);
        });
        if resized.is_some() {
            self.ivars().overlay_dirty.set(true);
            self.draw();
        }
    }

    fn set_focused(&self, focused: bool) {
        self.with_state(|s| {
            s.focused = focused;
            s.mux.focus(focused);
        });
        self.ivars().overlay_dirty.set(true);
        self.request_frame();
    }

    fn option_is_alt(&self, flags: NSEventModifierFlags) -> bool {
        let setting = self.with_state(|s| s.config.terminal.option_as_alt).unwrap_or(OptionAsAlt::None);
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
        self.with_state(|s| s.mux.key(&input));
        // A key press scrolls back to the bottom.
        self.request_frame();
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
        self.with_state(|s| s.mux.key(&input));
        self.request_frame();
    }

    fn send_mouse(&self, event: &NSEvent, action: mouse::Action, button: Option<mouse::Button>) {
        let point = self.convertPoint_fromView(event.locationInWindow(), None);
        let mods = keys::mods(event.modifierFlags());
        self.with_state(|state| {
            if state.mux.mouse_tracking() {
                let (x, y) = ((point.x * state.scale) as f32, (point.y * state.scale) as f32);
                state.mux.mouse(&MouseInput { action, button, mods, x, y });
            }
        });
    }
}

/// The view's size in device pixels, as a grid of cells with padding.
fn geometry(size: NSSize, scale: f64, m: Metrics, padding_cells: u16) -> Geometry {
    let width = (size.width * scale).round().max(0.0) as u32;
    let height = (size.height * scale).round().max(0.0) as u32;
    let padding = m.cell_width * u32::from(padding_cells);
    let fit = |space: u32, cell: u32| u16::try_from((space / cell).max(1)).unwrap_or(u16::MAX);
    Geometry {
        cols: fit(width.saturating_sub(2 * padding), m.cell_width),
        rows: fit(height, m.cell_height),
        cell_width: m.cell_width,
        cell_height: m.cell_height,
        width,
        height,
        padding_left: padding,
        padding_top: 0,
    }
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

/// Window content size, in points, that fits `cols` x `rows` cells.
pub fn content_size(config: &Config, m: Metrics, scale: f64) -> NSSize {
    let cols = u32::from(config.window.cols + 2 * config.window.padding_cells);
    NSSize::new(
        f64::from(cols * m.cell_width) / scale,
        f64::from(u32::from(config.window.rows) * m.cell_height) / scale,
    )
}
