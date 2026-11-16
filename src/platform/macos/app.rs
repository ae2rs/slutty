use std::process::ExitCode;

use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType, NSEventModifierFlags,
    NSMenu, NSMenuItem, NSScreen, NSWindow, NSWindowButton, NSWindowStyleMask, NSWindowTitleVisibility,
};
use objc2_foundation::{NSObject, NSPoint, NSRect, NSString};

use super::view::{TerminalView, content_size};
use crate::config::Config;
use crate::font::Fonts;
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
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        unsafe { msg_send![Self::alloc(mtm), init] }
    }
}

/// Opens one terminal window and runs the AppKit event loop.
pub fn run(config: Config) -> ExitCode {
    let Some(mtm) = MainThreadMarker::new() else {
        log::error!("slutty must start on the main thread");
        return ExitCode::FAILURE;
    };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let delegate = AppDelegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.setMainMenu(Some(&main_menu(mtm)));

    let scale = NSScreen::mainScreen(mtm).map_or(2.0, |s| s.backingScaleFactor());
    let renderer = match Fonts::new(config.font.size * scale)
        .map_err(|e| e.to_string())
        .and_then(|fonts| Renderer::new(fonts).map_err(|e| e.to_string()))
    {
        Ok(renderer) => renderer,
        Err(err) => {
            log::error!("starting the renderer failed: {err}");
            return ExitCode::FAILURE;
        }
    };

    let size = content_size(&config, renderer.metrics(), scale);
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), size);
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
            rect,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // The window is owned here, not released by AppKit on close.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str("slutty"));
    hide_titlebar(&window);
    window.center();

    let view = TerminalView::new(mtm, rect, renderer, config, scale);
    window.setContentView(Some(&view));
    window.setDelegate(Some(ProtocolObject::from_ref(&*view)));
    window.makeFirstResponder(Some(&view));
    view.start();
    window.makeKeyAndOrderFront(None);
    app.activate();
    app.run();
    ExitCode::SUCCESS
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
