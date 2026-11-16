//! The terminal view: input, IME, drawing and the wake-ups from PTY threads.
//!
//! All state lives on the main thread. PTY reader threads only dispatch
//! [`wake`] to the main queue; a display link that is paused while nothing
//! changes paces the frames.

use std::cell::{Cell, OnceCell, RefCell};
use std::path::PathBuf;
use std::time::Instant;

use dispatch2::{DispatchQueue, DispatchTime};
use libghostty_vt::color::Rgb;
use libghostty_vt::key::{self, Key, Mods};
use libghostty_vt::mouse;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSEvent, NSEventModifierFlags, NSPasteboard, NSPasteboardTypeString, NSResponder, NSTextInputClient,
    NSView, NSViewLayerContentsRedrawPolicy, NSWindowDelegate,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSAttributedStringKey, NSNotFound, NSNotification, NSObject, NSPoint, NSRange,
    NSRangePointer, NSRect, NSRunLoop, NSRunLoopCommonModes, NSSize, NSString, NSUInteger,
};
use objc2_quartz_core::{CADisplayLink, CALayer, CAMetalLayer, kCAGravityTopLeft};

use super::keys;
use super::watch::Watcher;
use crate::config::{self, Config, OptionAsAlt, Padding};
use crate::font::{Fonts, Metrics};
use crate::keys::{Action, Matcher, Outcome};
use crate::mux::{Event, Mux};
use crate::renderer::{Line, Overlay, PIXEL_FORMAT, Renderer};
use crate::terminal::{CellStyle, Geometry, KeyInput, MouseInput};

thread_local! {
    /// The window's view, for wake-ups that arrive through the main queue.
    static VIEW: RefCell<Option<Retained<TerminalView>>> = const { RefCell::new(None) };
}

/// Runs on a PTY reader thread when output is waiting.
pub fn wake_from_any_thread() {
    DispatchQueue::main().exec_async(|| with_view(|view| view.pump()));
}

pub fn with_view(f: impl FnOnce(&TerminalView)) {
    // Cloned out, so `f` may close the window, which clears VIEW.
    if let Some(view) = VIEW.with(|v| v.borrow().clone()) {
        f(&view);
    }
}

/// The config in effect, where it came from, and why the file could not be
/// used, if it could not.
pub struct Settings {
    pub config: Config,
    pub path: Option<PathBuf>,
    pub error: Option<String>,
}

struct State {
    mux: Mux,
    renderer: Renderer,
    settings: Settings,
    watcher: Option<Watcher>,
    matcher: Matcher,
    /// Key codes whose press a binding used, so their release is not sent.
    swallowed: Vec<u16>,
    /// In points; the font-size keys change it.
    font_size: f64,
    scale: f64,
    focused: bool,
    /// Fractions of a line left over from trackpad scrolling.
    scroll_remainder: f64,
}

impl State {
    fn config(&self) -> &Config {
        &self.settings.config
    }

    /// Loads the fonts at the current size and scale. On failure the old ones
    /// stay, and the banner says why.
    fn load_fonts(&mut self) {
        match Fonts::new(&self.settings.config.font, self.font_size * self.scale) {
            Ok(fonts) => self.renderer.set_fonts(fonts),
            Err(err) => {
                log::error!("loading fonts failed: {err}");
                self.settings.error = Some(err.to_string());
            }
        }
    }

    /// Applies a config read from the file. Returns whether anything changed.
    fn reload(&mut self, loaded: Result<Config, String>) -> bool {
        let new = match loaded {
            Ok(new) => new,
            Err(err) => {
                let changed = self.settings.error.as_ref() != Some(&err);
                if changed {
                    log::error!("config: {err}");
                }
                self.settings.error = Some(err);
                return changed;
            }
        };
        let had_error = self.settings.error.take().is_some();
        let old = std::mem::replace(&mut self.settings.config, new);
        let new = &self.settings.config;
        if *new == old {
            return had_error;
        }
        log::info!("config reloaded");
        self.mux.configure(new);
        if new.keys != old.keys {
            self.matcher.reset();
        }
        if new.font != old.font {
            if new.font.size != old.font.size {
                self.font_size = new.font.size;
            }
            self.load_fonts();
        }
        true
    }
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
                state.scale = scale;
                state.load_fonts();
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
            if !self.bind_key(event) {
                self.handle_key_down(event);
            }
        }

        #[unsafe(method(keyUp:))]
        fn key_up(&self, event: &NSEvent) {
            let code = event.keyCode();
            let swallowed = self.with_state(|s| {
                let found = s.swallowed.iter().position(|&c| c == code);
                found.map(|i| s.swallowed.swap_remove(i)).is_some()
            });
            if swallowed != Some(true) && self.ivars().marked.borrow().is_empty() {
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
                    -(dy.signum() as isize) * isize::try_from(state.config().mouse.wheel_lines).unwrap_or(1)
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
    pub fn new(
        mtm: MainThreadMarker,
        frame: NSRect,
        renderer: Renderer,
        settings: Settings,
        scale: f64,
    ) -> Retained<Self> {
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

        let config = &settings.config;
        let geometry = geometry(frame.size, scale, renderer.metrics(), config.window.padding);
        match Mux::spawn(geometry, config, wake_from_any_thread) {
            Ok(mux) => {
                *view.ivars().state.borrow_mut() = Some(State {
                    mux,
                    renderer,
                    font_size: config.font.size,
                    settings,
                    watcher: None,
                    matcher: Matcher::default(),
                    swallowed: Vec::new(),
                    scale,
                    focused: false,
                    scroll_remainder: 0.0,
                });
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
        self.watch_config();
    }

    /// Watches the config file, replacing the previous watch.
    fn watch_config(&self) {
        self.with_state(|s| {
            s.watcher = None;
            s.watcher = s.settings.path.as_deref().map(|path| {
                // Later, on the main queue, so the watch is not replaced from
                // inside its own handler.
                Watcher::new(path, || DispatchQueue::main().exec_async(|| with_view(|view| view.reload_config())))
            });
        });
    }

    /// Reads the config file again and applies it. A broken file keeps the
    /// config in effect, and the banner says why.
    fn reload_config(&self) {
        self.watch_config();
        let changed = self.with_state(|s| {
            let loaded = s.settings.path.as_deref().map_or_else(|| Ok(Config::default()), config::load);
            s.reload(loaded)
        });
        if changed == Some(true) {
            self.relayout();
        }
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
        let chrome: Vec<Line> =
            state.settings.error.iter().map(|err| banner(err, &state.settings.config, geometry)).collect();
        let mut frame = state.mux.frame();
        if !frame.changed && !overlay_dirty {
            return false;
        }
        let overlay = Overlay { focused: state.focused, preedit: &marked };
        let origin = [geometry.padding_left as f32, geometry.padding_top as f32];
        state.renderer.render(&ivars.layer, &mut frame, &overlay, origin, &chrome);
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
            let geometry = geometry(size, state.scale, state.renderer.metrics(), state.config().window.padding);
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

    /// Runs the binding this key press belongs to. Returns whether a binding
    /// used the key, which then does not reach the program.
    fn bind_key(&self, event: &NSEvent) -> bool {
        if !self.ivars().marked.borrow().is_empty() {
            return false;
        }
        let typed = |flags| event.charactersByApplyingModifiers(flags).and_then(|s| s.to_string().chars().next());
        let code = event.keyCode();
        let flags = event.modifierFlags();
        let Some(press) = keys::press(code, flags, typed(NSEventModifierFlags(0)), typed(NSEventModifierFlags::Shift))
        else {
            return false;
        };
        let outcome = self.with_state(|s| {
            let outcome = s.matcher.press(&s.settings.config.keys, press);
            log::trace!("{press:?}: {outcome:?}");
            if outcome != Outcome::Unbound {
                // No key-up comes for keys pressed with command, so keep few.
                s.swallowed.retain(|&c| c != code);
                s.swallowed.push(code);
                if s.swallowed.len() > 8 {
                    s.swallowed.remove(0);
                }
            }
            outcome
        });
        match outcome {
            Some(Outcome::Run(action)) => {
                self.run_action(action);
                true
            }
            Some(Outcome::Handled) => true,
            Some(Outcome::Unbound) | None => false,
        }
    }

    fn run_action(&self, action: Action) {
        log::debug!("running {action:?}");
        match action {
            Action::Send { text } => {
                self.with_state(|s| s.mux.send(text.as_bytes()));
                self.request_frame();
            }
            Action::SendKey { key: chord } => {
                let (key, mods, unshifted) = chord.encoding();
                // A key with no command or control types its character.
                let text = unshifted
                    .filter(|_| !mods.intersects(Mods::CTRL | Mods::SUPER))
                    .map(|c| if mods.contains(Mods::SHIFT) { c.to_ascii_uppercase() } else { c }.to_string());
                let consumed = if text.is_some() { mods & Mods::SHIFT } else { Mods::empty() };
                let input =
                    KeyInput { key, action: key::Action::Press, mods, consumed, text: text.as_deref(), unshifted };
                self.with_state(|s| s.mux.key(&input));
                self.request_frame();
            }
            Action::Paste => self.paste_clipboard(),
            Action::Quit => NSApplication::sharedApplication(self.mtm()).terminate(None),
            Action::IncreaseFontSize => self.set_font_size(|size, _| size * 1.1),
            Action::DecreaseFontSize => self.set_font_size(|size, _| size / 1.1),
            Action::ResetFontSize => self.set_font_size(|_, configured| configured),
            Action::ReloadConfig => self.reload_config(),
            Action::ToggleFullscreen => {
                if let Some(window) = self.window() {
                    window.toggleFullScreen(None);
                }
            }
            // The matcher runs these itself.
            Action::KeyTable { .. } | Action::PopTable => {}
        }
    }

    fn paste_clipboard(&self) {
        let pasteboard = NSPasteboard::generalPasteboard();
        if let Some(text) = pasteboard.stringForType(unsafe { NSPasteboardTypeString }) {
            self.with_state(|s| s.mux.paste(&text.to_string()));
            self.request_frame();
        }
    }

    /// Sets the font size from the current and the configured one, keeping
    /// the window's size.
    fn set_font_size(&self, size: impl FnOnce(f64, f64) -> f64) {
        self.with_state(|s| {
            s.font_size = size(s.font_size, s.config().font.size).clamp(4.0, 200.0);
            s.load_fonts();
        });
        self.relayout();
    }

    /// Whether the app should ask before quitting.
    pub fn confirms_quit(&self) -> bool {
        self.with_state(|s| s.config().window.confirm_quit).unwrap_or(false)
    }

    fn option_is_alt(&self, flags: NSEventModifierFlags) -> bool {
        let setting = self.with_state(|s| s.config().terminal.option_as_alt).unwrap_or(OptionAsAlt::None);
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

/// A line across the top that says why the config file is not in effect.
fn banner(error: &str, config: &Config, g: Geometry) -> Line {
    let rgb = |[r, g, b]: config::Rgb| Rgb { r, g, b };
    let theme = &config.theme;
    let mut line = Line::new(0.0, 0.0, g.width as f32, rgb(theme.ansi[1]));
    let fits = (g.width / g.cell_width) as usize;
    let text: String = format!(" {error}").chars().take(fits).collect();
    line.push_str(&text, rgb(theme.background), CellStyle { bold: true, ..CellStyle::default() });
    line
}

/// The view's size in device pixels, as a grid of cells with padding.
fn geometry(size: NSSize, scale: f64, m: Metrics, padding: Padding) -> Geometry {
    let width = (size.width * scale).round().max(0.0) as u32;
    let height = (size.height * scale).round().max(0.0) as u32;
    let [left, right, top, bottom] = padding_pixels(padding, m, scale);
    let fit = |space: u32, cell: u32| u16::try_from((space / cell).max(1)).unwrap_or(u16::MAX);
    Geometry {
        cols: fit(width.saturating_sub(left + right), m.cell_width),
        rows: fit(height.saturating_sub(top + bottom), m.cell_height),
        cell_width: m.cell_width,
        cell_height: m.cell_height,
        width,
        height,
        padding_left: left,
        padding_top: top,
        padding_right: right,
        padding_bottom: bottom,
    }
}

/// Left, right, top and bottom padding in device pixels.
fn padding_pixels(p: Padding, m: Metrics, scale: f64) -> [u32; 4] {
    [
        p.left.pixels(m.cell_width, scale),
        p.right.pixels(m.cell_width, scale),
        p.top.pixels(m.cell_height, scale),
        p.bottom.pixels(m.cell_height, scale),
    ]
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
    let [left, right, top, bottom] = padding_pixels(config.window.padding, m, scale);
    NSSize::new(
        f64::from(u32::from(config.window.cols) * m.cell_width + left + right) / scale,
        f64::from(u32::from(config.window.rows) * m.cell_height + top + bottom) / scale,
    )
}
