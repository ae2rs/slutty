//! Regex search over the screen and its scrollback, which libghostty's own
//! search does not do. The text is read one screen row per line, so a match
//! never spans a soft wrap.

use libghostty_vt::fmt::{Format, Formatter};
use libghostty_vt::unicode;
use regex::Regex;

/// Part of one screen row: cells `start..end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Span {
    /// From the top of the scrollback.
    pub row: u32,
    pub start: u16,
    pub end: u16,
}

/// A regex search: its pattern, and the matches from oldest to newest.
#[derive(Debug, Default)]
pub(super) struct RegexSearch {
    /// `None` while the pattern is empty or invalid.
    regex: Option<Regex>,
    invalid: bool,
    matches: Vec<Span>,
    current: Option<usize>,
    /// Output arrived since the last scan.
    stale: bool,
}

impl RegexSearch {
    /// Looks for `pattern` and moves to the newest match.
    pub fn set_pattern(&mut self, vt: &libghostty_vt::Terminal<'static, 'static>, pattern: &str) {
        self.regex = None;
        self.invalid = false;
        if !pattern.is_empty() {
            match Regex::new(pattern) {
                Ok(regex) => self.regex = Some(regex),
                Err(_) => self.invalid = true,
            }
        }
        self.current = None;
        self.scan(vt);
        self.current = self.matches.len().checked_sub(1);
    }

    pub fn output_arrived(&mut self) {
        self.stale = true;
    }

    /// Scans again if output arrived since the last scan, keeping the
    /// current match where it can.
    pub fn refresh(&mut self, vt: &libghostty_vt::Terminal<'static, 'static>) {
        if self.stale {
            self.scan(vt);
            self.current = self.current.map(|i| i.min(self.matches.len().saturating_sub(1)));
            if self.matches.is_empty() {
                self.current = None;
            }
        }
    }

    /// Moves to the next older match, or the next newer when `older` is not
    /// set, wrapping around. Returns the match moved to.
    pub fn step(&mut self, older: bool) -> Option<Span> {
        let len = self.matches.len();
        let current = self.current?;
        let next = if older { (current + len - 1) % len } else { (current + 1) % len };
        self.current = Some(next);
        self.current()
    }

    pub fn current(&self) -> Option<Span> {
        self.matches.get(self.current?).copied()
    }

    /// The match moved to, counting from the newest, which is 1, as text
    /// search counts.
    pub fn position(&self) -> Option<usize> {
        Some(self.matches.len() - self.current?)
    }

    pub fn total(&self) -> usize {
        self.matches.len()
    }

    pub fn invalid(&self) -> bool {
        self.invalid
    }

    /// The matches on rows `top..top + rows`, and whether each is current.
    pub fn visible(&self, top: u32, rows: u32) -> impl Iterator<Item = (Span, bool)> + '_ {
        let current = self.current;
        let first = self.matches.partition_point(|m| m.row < top);
        self.matches[first..]
            .iter()
            .enumerate()
            .take_while(move |(_, m)| m.row < top + rows)
            .map(move |(i, m)| (*m, current == Some(first + i)))
    }

    fn scan(&mut self, vt: &libghostty_vt::Terminal<'static, 'static>) {
        self.stale = false;
        self.matches.clear();
        let Some(regex) = &self.regex else { return };
        let text = Formatter::new(vt)
            .and_then(|mut f| f.set_format(Format::Plain)?.set_unwrap(false)?.set_trim(true)?.format_alloc(None));
        match text {
            Ok(bytes) => self.matches = find(&String::from_utf8_lossy(&bytes), regex),
            Err(err) => log::warn!("reading the screen to search failed: {err}"),
        }
    }
}

/// The non-empty matches of `regex` in `text`, one screen row per line, as
/// cells.
fn find(text: &str, regex: &Regex) -> Vec<Span> {
    let mut found = Vec::new();
    for (row, line) in text.split('\n').enumerate() {
        let row = u32::try_from(row).unwrap_or(u32::MAX);
        let mut cells = None;
        for m in regex.find_iter(line).filter(|m| !m.is_empty()) {
            let cells = cells.get_or_insert_with(|| cell_starts(line));
            let cell = |byte: usize| cells[cells.partition_point(|&(b, _)| b < byte)].1;
            found.push(Span { row, start: cell(m.start()), end: cell(m.end()) });
        }
    }
    found
}

/// The cell each char of `line` starts at, by byte, then the end of the line.
fn cell_starts(line: &str) -> Vec<(usize, u16)> {
    let mut col = 0u16;
    let mut starts: Vec<(usize, u16)> = line
        .char_indices()
        .map(|(byte, c)| {
            let start = (byte, col);
            col = col.saturating_add(u16::from(unicode::codepoint_width(c)));
            start
        })
        .collect();
    starts.push((line.len(), col));
    starts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(row: u32, start: u16, end: u16) -> Span {
        Span { row, start, end }
    }

    #[test]
    fn matches_map_to_cells_past_wide_characters() {
        let regex = Regex::new(r"x\w+").expect("valid");
        let found = find("no\n日本 xab\n\nxy xz", &regex);
        assert_eq!(found, vec![span(1, 5, 8), span(3, 0, 2), span(3, 3, 5)]);
    }

    #[test]
    fn empty_matches_are_skipped() {
        let regex = Regex::new("a*").expect("valid");
        assert_eq!(find("bab", &regex), vec![span(0, 1, 2)]);
    }

    #[test]
    fn visible_matches_are_those_in_view() {
        let search = RegexSearch {
            matches: vec![span(1, 0, 1), span(5, 0, 1), span(6, 2, 3), span(9, 0, 1)],
            current: Some(2),
            ..RegexSearch::default()
        };
        let shown: Vec<_> = search.visible(5, 3).collect();
        assert_eq!(shown, vec![(span(5, 0, 1), false), (span(6, 2, 3), true)]);
    }

    #[test]
    fn steps_wrap_around() {
        let mut search =
            RegexSearch { matches: vec![span(0, 0, 1), span(1, 0, 1)], current: Some(1), ..RegexSearch::default() };
        assert_eq!(search.position(), Some(1));
        assert_eq!(search.step(true), Some(span(0, 0, 1)));
        assert_eq!(search.position(), Some(2));
        assert_eq!(search.step(true), Some(span(1, 0, 1)));
        assert_eq!(search.step(false), Some(span(0, 0, 1)));
    }
}
