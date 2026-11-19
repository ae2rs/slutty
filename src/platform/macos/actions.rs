//! What a key binding runs in a window.

use libghostty_vt::key::{self, Mods};
use objc2::MainThreadOnly;
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSPasteboard, NSPasteboardTypeString, NSWorkspace,
};
use objc2_foundation::{NSString, NSURL};

use super::app;
use super::state::{self, with_app};
use super::view::TerminalView;
use crate::keys::Action;
use crate::mux::{Axis, PaneId, Place, PlaceChange};
use crate::proto::{Command, KeyEvent, Program};

impl TerminalView {
    pub(super) fn run_action(&self, action: Action) {
        log::debug!("running {action:?}");
        let window = self.mux_window();
        let pane = self.active_pane();
        // What the action asks of the server about the active pane.
        let on_pane = |command: fn(PaneId) -> Command| {
            if let Some(pane) = pane {
                with_app(|app| app.request(command(pane)));
            }
        };
        match action {
            Action::Send { text } => {
                if let Some(pane) = pane {
                    with_app(|app| app.request(Command::SendText { pane, text, paste: false }));
                }
            }
            Action::SendKey { key: chord } => {
                let (key, mods, unshifted) = chord.encoding();
                // A key with no command or control types its character.
                let text = unshifted
                    .filter(|_| !mods.intersects(Mods::CTRL | Mods::SUPER))
                    .map(|c| if mods.contains(Mods::SHIFT) { c.to_ascii_uppercase() } else { c }.to_string());
                let consumed = if text.is_some() { mods & Mods::SHIFT } else { Mods::empty() };
                self.send_key_event(KeyEvent { key, action: key::Action::Press, mods, consumed, text, unshifted });
            }
            Action::Paste => self.paste_clipboard(),
            Action::Quit => NSApplication::sharedApplication(self.mtm()).terminate(None),
            Action::IncreaseFontSize => set_font_size(|size, _| size * 1.1),
            Action::DecreaseFontSize => set_font_size(|size, _| size / 1.1),
            Action::ResetFontSize => set_font_size(|_, configured| configured),
            Action::ReloadConfig => state::reload_config(),
            Action::ToggleFullscreen => {
                if let Some(window) = self.window() {
                    window.toggleFullScreen(None);
                }
            }
            Action::NewTab => _ = with_app(|app| app.request(Command::NewTab { window, program: Program::default() })),
            Action::CloseTab { confirm } => {
                with_app(|app| app.request_confirmed("Close this tab?", Command::CloseTab { window, confirm }));
            }
            Action::ActivateTab { index } => {
                with_app(|app| {
                    let count = app.mirror.window(window).map_or(0, |w| w.tabs.len());
                    if let Some(index) = index.resolve(count) {
                        app.request(Command::ActivateTab { window, index });
                    }
                });
            }
            Action::ToggleTabBar => {
                with_app(|app| app.tab_bar = !app.tab_bar);
                state::views().iter().for_each(|view| view.relayout());
            }
            Action::NewWindow => {
                let cwd = with_app(|app| app.mirror.active_cwd(window)).flatten();
                app::open_window(Program { command: None, cwd });
            }
            Action::PopPane => on_pane(|pane| Command::PopPane { pane }),
            Action::PopTab => _ = with_app(|app| app.request(Command::PopTab { window })),
            Action::SplitRight | Action::SplitDown => {
                let axis = if action == Action::SplitRight { Axis::Row } else { Axis::Column };
                if let Some(pane) = pane {
                    let program = Program::default();
                    with_app(|app| app.request(Command::Split { pane, axis, percent: None, program }));
                }
            }
            Action::ClosePane { confirm } => {
                if let Some(pane) = pane {
                    with_app(|app| app.request_confirmed("Close this pane?", Command::ClosePane { pane, confirm }));
                }
            }
            Action::FocusPane { direction } => {
                if let Some(pane) = pane {
                    with_app(|app| app.request(Command::ActivateDirection { pane, direction }));
                }
            }
            Action::ResizePane { direction, cells } => {
                if let Some(pane) = pane {
                    with_app(|app| app.request(Command::ResizePane { pane, direction, cells }));
                }
            }
            Action::ToggleZoom => on_pane(|pane| Command::ToggleZoom { pane }),
            Action::RotatePanes => _ = with_app(|app| app.request(Command::RotatePanes { window })),
            Action::PaneSelect => self.start_pane_select(),
            Action::Copy => on_pane(|pane| Command::Copy { pane }),
            Action::CopyMode => on_pane(|pane| Command::CopyMode { pane }),
            Action::Search => on_pane(|pane| Command::Search { pane }),
            Action::QuickSelect => self.start_quick_select(false),
            Action::OpenUrl => self.start_quick_select(true),
            Action::ToggleFloating => _ = with_app(|app| app.request(Command::ToggleFloats { window })),
            Action::NewFloating { anchor, width, height } => {
                if let Some(pane) = pane {
                    let place = Place { anchor, width, height, x_offset: 0, y_offset: 0 };
                    let program = Program::default();
                    with_app(|app| app.request(Command::FloatPane { pane, place, hidden: false, program }));
                }
            }
            Action::CycleFloating => _ = with_app(|app| app.request(Command::CycleFloats { window })),
            Action::MovePane { direction, cells } => {
                if let Some(pane) = pane {
                    with_app(|app| app.request(Command::MovePane { pane, direction, cells }));
                }
            }
            Action::AnchorPane { anchor } => {
                if let Some(pane) = pane {
                    let change = PlaceChange {
                        anchor: Some(anchor),
                        x_offset: Some(0),
                        y_offset: Some(0),
                        ..Default::default()
                    };
                    with_app(|app| app.request(Command::PlacePane { pane, change }));
                }
            }
            // The matcher runs these itself.
            Action::KeyTable { .. } | Action::PopTable => {}
        }
    }

    pub(super) fn paste_clipboard(&self) {
        let pasteboard = NSPasteboard::generalPasteboard();
        // SAFETY: a constant AppKit defines.
        if let Some(text) = pasteboard.stringForType(unsafe { NSPasteboardTypeString }) {
            let text = text.to_string();
            self.restart_blink();
            if let Some(pane) = self.active_pane() {
                with_app(|app| app.request(Command::Paste { pane, text }));
            }
        }
    }
}

/// Sets the font size from the current and the configured one. Windows keep
/// their size and fit more or fewer cells.
fn set_font_size(size: impl FnOnce(f64, f64) -> f64) {
    with_app(|app| {
        app.font_size = size(app.font_size, app.config().font.size).clamp(4.0, 200.0);
        app.load_fonts();
    });
    state::views().iter().for_each(|view| view.relayout());
}

pub(super) fn set_clipboard(text: &str) {
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    // SAFETY: a constant AppKit defines.
    pasteboard.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString });
}

/// Opens a URL in its default app.
pub(super) fn open_url(url: &str) {
    match NSURL::URLWithString(&NSString::from_str(url)) {
        Some(nsurl) => {
            if !NSWorkspace::sharedWorkspace().openURL(&nsurl) {
                log::warn!("opening {url} failed");
            }
        }
        None => log::warn!("not a URL: {url}"),
    }
}

/// An alert with a confirm and a cancel button.
pub(super) fn alert(mtm: objc2::MainThreadMarker, message: &str, detail: &str, confirm: &str) -> bool {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(message));
    alert.setInformativeText(&NSString::from_str(detail));
    alert.addButtonWithTitle(&NSString::from_str(confirm));
    alert.addButtonWithTitle(&NSString::from_str("Cancel"));
    alert.runModal() == NSAlertFirstButtonReturn
}
