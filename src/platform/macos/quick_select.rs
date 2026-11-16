//! Quick select: labels over what is worth copying on screen, or over its
//! URLs. Typing a label copies its text (an uppercase label also pastes it)
//! or opens its URL.

use objc2::DefinedClass;
use objc2_app_kit::NSEvent;

use super::actions;
use super::state::with_app;
use super::view::TerminalView;
use crate::mux::PaneId;
use crate::scan::{self, Labelled, RowText};

pub struct QuickSelect {
    pub pane: PaneId,
    pub matches: Vec<Labelled>,
    /// The label typed so far, in lowercase.
    pub typed: String,
    /// An uppercase letter was typed: paste as well as copy.
    paste: bool,
    /// Open the URL instead of copying.
    open: bool,
}

impl QuickSelect {
    /// The matches whose label starts with what was typed.
    pub fn shown(&self) -> impl Iterator<Item = &Labelled> {
        self.matches.iter().filter(|m| m.label.starts_with(&self.typed))
    }
}

impl TerminalView {
    /// Labels the matches on the active pane's screen; URLs only for `open`.
    pub(super) fn start_quick_select(&self, open: bool) {
        let window = self.mux_window();
        let quick = with_app(|app| {
            let pane = app.mux.active_pane(window)?;
            let rows: Vec<RowText> = app.mux.pane_mut(pane)?.frame().rows.iter().map(RowText::new).collect();
            let regex = if open { scan::url_pattern() } else { scan::default_patterns() };
            let matches = scan::find(&rows, regex);
            Some(QuickSelect { pane, matches, typed: String::new(), paste: false, open })
        })
        .flatten();
        *self.ivars().quick.borrow_mut() = quick.filter(|q| !q.matches.is_empty());
        self.ivars().overlay_dirty.set(true);
        self.request_frame();
    }

    /// While quick select shows, takes the key: a label acts on its match,
    /// the start of one waits for the rest, and any other key cancels.
    /// Returns whether it took the key.
    pub(super) fn quick_select_key(&self, event: &NSEvent) -> bool {
        let Some(mut quick) = self.ivars().quick.borrow_mut().take() else { return false };
        self.swallow(event.keyCode());
        self.ivars().overlay_dirty.set(true);
        self.request_frame();
        let Some(c) = event.characters().and_then(|s| s.to_string().chars().next()) else { return true };
        quick.paste |= c.is_uppercase();
        quick.typed.extend(c.to_lowercase());
        let chosen = quick.shown().find(|m| m.label == quick.typed).map(|m| m.span.text.clone());
        match chosen {
            Some(text) if quick.open => actions::open_url(&text),
            Some(text) => {
                actions::set_clipboard(&text);
                if quick.paste {
                    with_app(|app| app.mux.pane_mut(quick.pane).map(|pane| pane.paste(&text)));
                }
            }
            None if quick.shown().next().is_some() => *self.ivars().quick.borrow_mut() = Some(quick),
            None => {}
        }
        true
    }
}
