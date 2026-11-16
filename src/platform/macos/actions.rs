//! What a key binding runs in a window.

use libghostty_vt::key::{self, Mods};
use objc2::MainThreadOnly;
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSApplication, NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::NSString;

use super::state::{self, with_app};
use super::view::TerminalView;
use crate::keys::Action;
use crate::terminal::KeyInput;

impl TerminalView {
    pub(super) fn run_action(&self, action: Action) {
        log::debug!("running {action:?}");
        let window = self.mux_window();
        match action {
            Action::Send { text } => self.with_active_pane(|pane| pane.send(text.as_bytes())),
            Action::SendKey { key: chord } => {
                let (key, mods, unshifted) = chord.encoding();
                // A key with no command or control types its character.
                let text = unshifted
                    .filter(|_| !mods.intersects(Mods::CTRL | Mods::SUPER))
                    .map(|c| if mods.contains(Mods::SHIFT) { c.to_ascii_uppercase() } else { c }.to_string());
                let consumed = if text.is_some() { mods & Mods::SHIFT } else { Mods::empty() };
                let input =
                    KeyInput { key, action: key::Action::Press, mods, consumed, text: text.as_deref(), unshifted };
                self.with_active_pane(|pane| pane.key(&input));
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
            Action::NewTab => {
                if let Some(Err(err)) = with_app(|app| app.mux.new_tab(window, &app.settings.config)) {
                    log::error!("opening a tab failed: {err}");
                }
                self.tabs_changed();
            }
            Action::CloseTab { confirm } => {
                let busy = with_app(|app| app.mux.active_tab_busy(window)).unwrap_or(false);
                // The alert runs the event loop, so the app is not borrowed here.
                if confirm
                    && busy
                    && !alert(self.mtm(), "Close this tab?", "A program is still running in it.", "Close")
                {
                    return;
                }
                if with_app(|app| app.mux.close_tab(window)) == Some(true) {
                    self.close_window();
                } else {
                    self.tabs_changed();
                }
            }
            Action::ActivateTab { index } => {
                with_app(|app| {
                    let count = app.mux.tabs(window).len();
                    if let Some(index) = index.resolve(count) {
                        app.mux.activate_tab(window, index);
                    }
                });
                self.tabs_changed();
            }
            Action::ToggleTabBar => {
                with_app(|app| app.tab_bar = !app.tab_bar);
                state::views().iter().for_each(|view| view.relayout());
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
            self.with_active_pane(|pane| pane.paste(&text));
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

/// An alert with a confirm and a cancel button.
pub(super) fn alert(mtm: objc2::MainThreadMarker, message: &str, detail: &str, confirm: &str) -> bool {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(message));
    alert.setInformativeText(&NSString::from_str(detail));
    alert.addButtonWithTitle(&NSString::from_str(confirm));
    alert.addButtonWithTitle(&NSString::from_str("Cancel"));
    alert.runModal() == NSAlertFirstButtonReturn
}
