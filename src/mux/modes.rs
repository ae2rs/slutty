//! A pane's copy and search modes. The keys they take are read by the UI
//! (`client::mode_keys`); the modes themselves live here, by the terminal.

use super::pane::Pane;
use crate::terminal::copy_mode::Command;
use crate::terminal::{Highlight, SearchStatus};

impl Pane {
    /// Opens copy mode. rho draws on the alternate screen, which has no
    /// scrollback, so in a rho pane copy mode opens once rho has printed its
    /// transcript onto the normal screen.
    pub fn start_copy_mode(&mut self) {
        if self.runs_rho() && !self.shows_rho_scrollback() {
            self.rho_handoff();
            return;
        }
        self.terminal.close_search();
        self.terminal.start_copy_mode();
    }

    pub fn in_copy_mode(&self) -> bool {
        self.terminal.in_copy_mode()
    }

    pub fn copy_cursor(&self) -> Option<(u16, u16)> {
        self.terminal.copy_cursor()
    }

    pub fn copy_mode(&mut self, command: Command) {
        self.terminal.copy_mode(command);
    }

    pub fn copy_mode_jump(&mut self, target: char) {
        self.terminal.copy_mode_jump(target);
    }

    /// Ends copy mode, with the selected text when `copy` is set, and brings
    /// rho's own screen back.
    pub fn close_copy_mode(&mut self, copy: bool) -> Option<String> {
        if !self.terminal.in_copy_mode() {
            return None;
        }
        let text = self.terminal.close_copy_mode(copy);
        if self.shows_rho_scrollback() {
            self.rho_handoff();
        }
        text
    }

    /// Opens search, looking for `needle`.
    pub fn start_search(&mut self, needle: &str) {
        self.terminal.close_copy_mode(false);
        self.terminal.search(needle);
    }

    pub fn search_status(&self) -> Option<SearchStatus> {
        self.terminal.search_status()
    }

    pub fn search_highlights(&mut self) -> Vec<Highlight> {
        self.terminal.search_highlights()
    }

    /// Changes the open search's needle through `f`.
    pub fn edit_search(&mut self, f: impl FnOnce(&mut String)) {
        let Some(status) = self.terminal.search_status() else { return };
        let mut needle = status.needle;
        f(&mut needle);
        self.terminal.search(&needle);
    }

    pub fn search_step(&mut self, older: bool) {
        self.terminal.search_step(older);
    }

    pub fn toggle_search_regex(&mut self) {
        self.terminal.toggle_search_regex();
    }

    pub fn close_search(&mut self) {
        self.terminal.close_search();
    }

    /// Adds pasted text to the search, if it is open. Returns whether it was.
    pub fn paste_into_search(&mut self, text: &str) -> bool {
        if self.terminal.search_status().is_none() {
            return false;
        }
        self.edit_search(|needle| needle.push_str(&text.replace(['\n', '\r'], " ")));
        true
    }
}
