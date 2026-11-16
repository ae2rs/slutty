//! A window's view: input, IME and drawing for the panes of one mux window.
//!
//! The view keeps only what belongs to the window on screen; the mux, the
//! renderer and the config are the app's ([`super::state`]). A display link
//! that is paused while nothing changes paces the frames.

use std::cell::{Cell, OnceCell, RefCell};
use std::ops::Range;

use libghostty_vt::color::Rgb;
use libghostty_vt::key::{self, Key, Mods};
use libghostty_vt::mouse;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSEvent, NSEventModifierFlags, NSResponder, NSTextInputClient, NSView, NSViewLayerContentsRedrawPolicy,
    NSWindowDelegate,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSAttributedStringKey, NSNotFound, NSNotification, NSObject, NSPoint, NSRange,
    NSRangePointer, NSRect, NSRunLoop, NSRunLoopCommonModes, NSSize, NSString, NSUInteger,
};
use objc2_metal::MTLDevice;
use objc2_quartz_core::{CADisplayLink, CALayer, CAMetalLayer, kCAGravityTopLeft};

use super::keys;
use super::layout::Layout;
use super::state::{self, App, with_app};
use super::tab_bar;
use crate::config::{self, Config, OptionAsAlt};
use crate::keys::Outcome;
use crate::mux::{PaneId, WindowId};
use crate::renderer::{Line, Overlay, PIXEL_FORMAT, PaneDraw};
use crate::terminal::{CellStyle, KeyInput, MouseInput};

pub struct Ivars {
    window: WindowId,
    layer: Retained<CAMetalLayer>,
    link: OnceCell<Retained<CADisplayLink>>,
    layout: Cell<Layout>,
    /// Each tab's cells in the tab bar, for clicks.
    tab_cells: RefCell<Vec<Range<usize>>>,
    focused: Cell<bool>,
    /// Text the input method is composing.
    marked: RefCell<String>,
    /// Text committed while `keyDown` runs the input method; `None` outside.
    key_texts: RefCell<Option<Vec<String>>>,
    /// Focus, preedit or the tab bar changed: draw even if no pane did.
    overlay_dirty: Cell<bool>,
    /// The cursor cell in view coordinates, for placing the IME window.
    cursor_rect: Cell<NSRect>,
    /// Fractions of a line left over from trackpad scrolling.
    scroll_remainder: Cell<f64>,
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
            self.ivars().layer.setContentsScale(scale);
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
            if !self.click_tab_bar(event) {
                self.send_mouse(event, mouse::Action::Press, Some(mouse::Button::Left));
            }
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
        let layer = CAMetalLayer::new();
        layer.setDevice(Some(device));
        layer.setPixelFormat(PIXEL_FORMAT);
        layer.setFramebufferOnly(true);
        layer.setContentsScale(scale);
        // A stale frame during a resize stays put instead of stretching.
        layer.setContentsGravity(unsafe { kCAGravityTopLeft });

        let ivars = Ivars {
            window,
            layer,
            link: OnceCell::new(),
            layout: Cell::new(layout),
            tab_cells: RefCell::new(Vec::new()),
            focused: Cell::new(false),
            marked: RefCell::new(String::new()),
            key_texts: RefCell::new(None),
            overlay_dirty: Cell::new(true),
            cursor_rect: Cell::new(NSRect::ZERO),
            scroll_remainder: Cell::new(0.0),
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
        self.relayout();
        self.tabs_changed();
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
        ivars.layer.setDrawableSize(NSSize::new(f64::from(layout.width), f64::from(layout.height)));
        ivars.overlay_dirty.set(true);
        self.draw();
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
            if layout.tab_bar {
                let cols = (layout.width / m.cell_width) as usize;
                let bar = tab_bar::build(
                    &mux.tabs(window),
                    &config.theme,
                    cols,
                    config.tab_bar.max_width,
                    layout.width as f32,
                );
                *ivars.tab_cells.borrow_mut() = bar.tabs;
                chrome.push(bar.line);
            }
            if let Some(err) = &settings.error {
                chrome.push(banner(err, config, layout));
            }
            let active = mux.active_pane(window);
            let visible = mux.visible(window);
            if !overlay_dirty && !visible.iter().any(|(_, _, frame)| frame.changed) {
                return None;
            }
            let several = visible.len() > 1;
            let mut cursor = None;
            let mut panes: Vec<PaneDraw<'_>> = visible
                .into_iter()
                .map(|(id, rect, frame)| {
                    let is_active = Some(id) == active;
                    let origin = [
                        layout.origin[0] + u32::from(rect.x) * m.cell_width,
                        layout.origin[1] + u32::from(rect.y) * m.cell_height,
                    ];
                    if is_active && let Some(c) = frame.cursor {
                        cursor = Some([
                            origin[0] + u32::from(c.x) * m.cell_width,
                            origin[1] + u32::from(c.y) * m.cell_height,
                        ]);
                    }
                    let preedit = if is_active { marked.as_str() } else { "" };
                    PaneDraw {
                        id,
                        frame,
                        origin: origin.map(|v| v as f32),
                        overlay: Overlay { focused: focused && is_active, preedit },
                        dim: several && !is_active,
                    }
                })
                .collect();
            let [r, g, b] = config.theme.background;
            renderer.render(&ivars.layer, &mut panes, &chrome, Rgb { r, g, b });
            Some((cursor, m))
        })
        .flatten();
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

    fn set_focused(&self, focused: bool) {
        self.ivars().focused.set(focused);
        let window = self.ivars().window;
        with_app(|app| app.mux.focus(window, focused));
        self.ivars().overlay_dirty.set(true);
        self.request_frame();
    }

    /// A point of a mouse event in device pixels from the view's corner.
    fn pixel_point(&self, event: &NSEvent) -> [f64; 2] {
        let point = self.convertPoint_fromView(event.locationInWindow(), None);
        let s = self.window().map_or(1.0, |w| w.backingScaleFactor());
        [point.x * s, point.y * s]
    }

    /// The pane under a point, with the point relative to the pane.
    fn pane_at(&self, app: &mut App, [x, y]: [f64; 2]) -> Option<(PaneId, f32, f32)> {
        let layout = self.ivars().layout.get();
        let pane = app.mux.active_pane(self.ivars().window)?;
        let x = x - f64::from(layout.origin[0]);
        let y = y - f64::from(layout.origin[1]);
        Some((pane, x as f32, y as f32))
    }

    /// Activates the tab under a click in the tab bar. Returns whether the
    /// click was in the tab bar.
    fn click_tab_bar(&self, event: &NSEvent) -> bool {
        let [x, y] = self.pixel_point(event);
        let Some(cell) = self.ivars().layout.get().tab_bar_cell(x, y) else { return false };
        let tab = self.ivars().tab_cells.borrow().iter().position(|cells| cells.contains(&cell));
        if let Some(tab) = tab {
            let window = self.ivars().window;
            with_app(|app| app.mux.activate_tab(window, tab));
            self.tabs_changed();
        }
        true
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
        let outcome = with_app(|app| {
            let outcome = app.matcher.press(&app.settings.config.keys, press);
            log::trace!("{press:?}: {outcome:?}");
            if outcome != Outcome::Unbound {
                // No key-up comes for keys pressed with command, so keep few.
                app.swallowed.retain(|&c| c != code);
                app.swallowed.push(code);
                if app.swallowed.len() > 8 {
                    app.swallowed.remove(0);
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

    fn send_mouse(&self, event: &NSEvent, action: mouse::Action, button: Option<mouse::Button>) {
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

fn pane_mut(app: &mut App, pane: PaneId, f: impl FnOnce(&mut crate::mux::Pane)) {
    if let Some(pane) = app.mux.pane_mut(pane) {
        f(pane);
    }
}

/// A line across the top that says why the config file is not in effect.
fn banner(error: &str, config: &Config, layout: Layout) -> Line {
    let rgb = |[r, g, b]: config::Rgb| Rgb { r, g, b };
    let theme = &config.theme;
    let mut line = Line::new(0.0, 0.0, layout.width as f32, rgb(theme.ansi[1]));
    let fits = (layout.width / layout.area.cell_width) as usize;
    let text: String = format!(" {error}").chars().take(fits).collect();
    line.push_str(&text, rgb(theme.background), CellStyle { bold: true, ..CellStyle::default() });
    line
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
