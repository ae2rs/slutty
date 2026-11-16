use std::path::{Path, PathBuf};
use std::process::ExitCode;

use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSApplicationTerminateReply,
    NSBackingStoreType, NSEventModifierFlags, NSMenu, NSMenuItem, NSScreen, NSWindow, NSWindowButton,
    NSWindowStyleMask, NSWindowTitleVisibility,
};
use objc2_foundation::{NSObject, NSPoint, NSRect, NSSize, NSString};

use super::actions;
use super::layout::{self, Layout};
use super::state::{self, App, Settings, with_app};
use super::view::TerminalView;
use crate::config::{self, Config};
use crate::font::Fonts;
use crate::mux::{Mux, WindowId};
use crate::renderer::Renderer;

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window_closed(&self, _app: &NSApplication) -> bool {
            true
        }

        #[unsafe(method(applicationShouldTerminate:))]
        fn should_terminate(&self, _app: &NSApplication) -> NSApplicationTerminateReply {
            // With no window left, as after the last shell exits, nothing is
            // lost by quitting.
            let ask = with_app(|app| app.config().window.confirm_quit).unwrap_or(false) && !state::views().is_empty();
            if !ask || actions::alert(self.mtm(), "Quit slutty?", "Every shell in every window will be closed.", "Quit")
            {
                NSApplicationTerminateReply::TerminateNow
            } else {
                NSApplicationTerminateReply::TerminateCancel
            }
        }
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        unsafe { msg_send![Self::alloc(mtm), init] }
    }
}

/// Reads the config file at `path`, if any. A broken file gives the defaults
/// and the reason, for the banner.
fn load_settings(path: Option<PathBuf>) -> Settings {
    let (config, error) = match path.as_deref().map(config::load) {
        Some(Err(err)) => {
            log::error!("config: {err}");
            (Config::default(), Some(err))
        }
        Some(Ok(config)) => (config, None),
        None => (Config::default(), None),
    };
    Settings { config, path, error }
}

/// Opens one terminal window and runs the AppKit event loop.
pub fn run(config_path: Option<PathBuf>) -> ExitCode {
    let Some(mtm) = MainThreadMarker::new() else {
        log::error!("slutty must start on the main thread");
        return ExitCode::FAILURE;
    };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let delegate = AppDelegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.setMainMenu(Some(&main_menu(mtm)));

    let mut settings = load_settings(config_path);
    let scale = NSScreen::mainScreen(mtm).map_or(2.0, |s| s.backingScaleFactor());
    let size = settings.config.font.size * scale;
    let fonts = Fonts::new(&settings.config.font, size).or_else(|err| {
        log::error!("loading fonts failed: {err}");
        settings.error = Some(err.to_string());
        Fonts::new(&config::Font::default(), size)
    });
    let renderer =
        match fonts.map_err(|e| e.to_string()).and_then(|fonts| Renderer::new(fonts).map_err(|e| e.to_string())) {
            Ok(renderer) => renderer,
            Err(err) => {
                log::error!("starting the renderer failed: {err}");
                return ExitCode::FAILURE;
            }
        };
    App::start(Mux::new(state::wake_from_any_thread), renderer, settings, scale);

    if !open_window(mtm, None, None) {
        return ExitCode::FAILURE;
    }
    app.activate();
    app.run();
    ExitCode::SUCCESS
}

/// Opens a window with one tab running the login shell in `cwd`, cascaded
/// from `after` or else centered. Returns whether it opened.
pub(super) fn open_window(mtm: MainThreadMarker, cwd: Option<&Path>, after: Option<&NSWindow>) -> bool {
    let opened = with_app(|app| {
        let m = app.renderer.metrics();
        let size = layout::content_size(app.config(), m, app.scale, app.tab_bar);
        let layout = Layout::new(size, app.scale, m, app.config().window.padding, app.tab_bar);
        let id = app.mux.open_window(layout.area, &app.settings.config, cwd)?;
        Ok::<_, std::io::Error>((id, size))
    });
    match opened {
        Some(Ok((id, size))) => {
            show_window(mtm, id, size, after);
            true
        }
        Some(Err(err)) => {
            log::error!("starting the shell failed: {err}");
            false
        }
        None => false,
    }
}

/// Shows mux window `id` in an NSWindow whose content is `size` points,
/// cascaded from `after` or else centered.
pub(super) fn show_window(mtm: MainThreadMarker, id: WindowId, size: NSSize, after: Option<&NSWindow>) {
    let made = with_app(|app| {
        let m = app.renderer.metrics();
        let layout = Layout::new(size, app.scale, m, app.config().window.padding, app.tab_bar);
        (layout, app.renderer.device().retain(), app.scale)
    });
    let Some((layout, device, scale)) = made else { return };
    // Made outside `with_app`: AppKit sizes the view as it starts, which
    // relays it out.
    let view = TerminalView::new(mtm, NSRect::new(NSPoint::new(0.0, 0.0), size), id, layout, &device, scale);
    // Titled keeps the rounded corners, shadow and key focus; the title bar
    // itself is hidden below.
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::FullSizeContentView
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), size),
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // The window is owned by its view's entry in the app, not released by
    // AppKit on close.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str("slutty"));
    window.setContentView(Some(&view));
    hide_titlebar(&window);
    match after {
        // A zero point leaves `after` in place and gives the next spot.
        Some(after) => _ = window.cascadeTopLeftFromPoint(after.cascadeTopLeftFromPoint(NSPoint::ZERO)),
        None => window.center(),
    }
    window.setDelegate(Some(ProtocolObject::from_ref(&*view)));
    window.makeFirstResponder(Some(&view));
    state::add_view(view.mux_window(), view.clone());
    view.start();
    window.makeKeyAndOrderFront(None);
}

/// Hides the title bar and its traffic lights, like WezTerm's
/// `window_decorations = "RESIZE"`. The window keeps its title for Mission
/// Control and window managers.
pub(super) fn hide_titlebar(window: &NSWindow) {
    window.setTitlebarAppearsTransparent(true);
    window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
    for button in [NSWindowButton::CloseButton, NSWindowButton::MiniaturizeButton, NSWindowButton::ZoomButton] {
        if let Some(button) = window.standardWindowButton(button) {
            button.setHidden(true);
        }
    }
    // The empty title bar strip would still take clicks on the top rows to
    // drag the window. AppKit has no public switch for it, so hide its view.
    let frame = window.contentView().and_then(|view| unsafe { view.superview() });
    for view in frame.map(|frame| frame.subviews()).iter().flat_map(|views| views.iter()) {
        if view.class().name() == c"NSTitlebarContainerView" {
            view.setHidden(true);
        }
    }
}

fn main_menu(mtm: MainThreadMarker) -> Retained<NSMenu> {
    let bar = NSMenu::new(mtm);
    let submenu = |title: &str, items: &[(&str, Sel, &str, NSEventModifierFlags)]| {
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
        for &(title, action, key, mods) in items {
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str(title),
                    Some(action),
                    &NSString::from_str(key),
                )
            };
            item.setKeyEquivalentModifierMask(mods);
            menu.addItem(&item);
        }
        let item = NSMenuItem::new(mtm);
        item.setSubmenu(Some(&menu));
        bar.addItem(&item);
    };
    let cmd = NSEventModifierFlags::Command;
    submenu("slutty", &[("Quit slutty", sel!(terminate:), "q", cmd)]);
    submenu("Edit", &[("Paste", sel!(paste:), "v", cmd)]);
    submenu(
        "Window",
        &[
            ("Close", sel!(performClose:), "w", cmd),
            ("Minimize", sel!(performMiniaturize:), "m", cmd),
            ("Toggle Full Screen", sel!(toggleFullScreen:), "f", cmd | NSEventModifierFlags::Control),
        ],
    );
    bar
}
