//! Selecting text: mouse gestures, copy mode and search, and reading what is
//! selected or linked.
//!
//! Positions are viewport cells unless they say otherwise. libghostty's
//! selections and grid refs borrow the terminal and go stale at its next
//! change, so each is used and dropped inside one call.

use std::time::{Duration, Instant};

use libghostty_vt::error::Result;
use libghostty_vt::fmt::{Format, Formatter};
use libghostty_vt::point::{Coordinate, Point, Space};
use libghostty_vt::search::{MatchBuffer, Search};
use libghostty_vt::selection::Selection;
use libghostty_vt::selection::gesture::{
    self, Autoscroll, AutoscrollTickEvent, DragEvent, Gesture, PressEvent, ReleaseEvent,
};
use libghostty_vt::terminal::ScrollViewport;
use serde::{Deserialize, Serialize};

use super::copy_mode::{self, Command, CopyMode, Pos};
use super::emulator::Terminal;
use super::regex_search::{RegexSearch, Span};

/// macOS's default double-click interval.
const MULTI_CLICK: Duration = Duration::from_millis(500);

/// The mouse over a pane: the cell under it and its place in device pixels
/// from the pane's corner.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pointer {
    pub col: u16,
    pub row: u16,
    pub x: f32,
    pub y: f32,
}

/// How a mouse button went up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Release {
    /// A drag, or a double or triple click, chose text.
    Selected,
    /// A plain click.
    Clicked,
}

/// One row's part of a search match.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Highlight {
    pub row: u16,
    pub start: u16,
    /// Exclusive.
    pub end: u16,
    /// Part of the match search moved to.
    pub current: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchStatus {
    pub needle: String,
    /// The match moved to, counting from 1.
    pub current: Option<usize>,
    pub total: usize,
    /// The needle is a regex rather than text.
    pub regex: bool,
    /// The needle is a regex that does not parse.
    pub invalid: bool,
}

/// The mouse selection gesture and its reusable events.
pub(super) struct Selector {
    gesture: Gesture<'static>,
    press: PressEvent<'static>,
    drag: DragEvent<'static>,
    release: ReleaseEvent<'static>,
    tick: AutoscrollTickEvent<'static>,
    epoch: Instant,
}

impl Selector {
    pub fn new() -> Result<Self> {
        let mut press = PressEvent::new()?;
        press.set_repeat_interval(MULTI_CLICK)?;
        Ok(Self {
            gesture: Gesture::new()?,
            press,
            drag: DragEvent::new()?,
            release: ReleaseEvent::new()?,
            tick: AutoscrollTickEvent::new()?,
            epoch: Instant::now(),
        })
    }
}

/// The search and the needle it looks for.
pub(super) struct Searching {
    needle: String,
    by: By,
}

impl Searching {
    pub fn output_arrived(&mut self) {
        if let By::Regex(regex) = &mut self.by {
            regex.output_arrived();
        }
    }
}

/// How a search matches: libghostty's text search, or a regex.
enum By {
    Text { search: Search<'static, 'static>, matches: MatchBuffer },
    Regex(RegexSearch),
}

impl By {
    fn text(vt: &mut libghostty_vt::Terminal<'static, 'static>) -> Result<Self> {
        Ok(Self::Text { search: Search::new(vt)?, matches: MatchBuffer::new() })
    }
}

impl Terminal {
    /// A mouse button went down at `at`. `extend` (shift) moves the end of
    /// the selection there instead of starting a new one.
    pub fn select_press(&mut self, at: Pointer, extend: bool) {
        let result = (|| -> Result<()> {
            let cell = Point::Viewport(self.viewport_cell(at));
            let vt = &self.vt;
            if extend && let Some(current) = vt.selection()? {
                let end = vt.grid_ref(cell)?;
                vt.set_selection(Some(&Selection::new(current.start(), end, current.is_rectangle())))?;
                return Ok(());
            }
            let s = &mut self.selector;
            let width = f64::from(self.shared.geometry.get().cell_width);
            s.press
                .set_position(f64::from(at.x), f64::from(at.y))?
                .set_repeat_distance(width)?
                .set_time(s.epoch.elapsed())?;
            let selection = s.press.apply(&mut s.gesture, vt, vt.grid_ref(cell)?)?;
            vt.set_selection(selection.as_ref())?;
            Ok(())
        })();
        self.selection_changed(result);
    }

    /// The mouse moved with the button down. Returns whether the pointer is
    /// past an edge, so the caller should call [`Terminal::select_autoscroll`]
    /// on a timer.
    pub fn select_drag(&mut self, at: Pointer, rectangle: bool) -> bool {
        let result = (|| -> Result<bool> {
            let cell = Point::Viewport(self.viewport_cell(at));
            let geometry = self.gesture_geometry();
            let (vt, s) = (&self.vt, &mut self.selector);
            s.drag.set_position(f64::from(at.x), f64::from(at.y))?.set_rectangle(rectangle)?;
            if let Some(selection) = s.drag.apply(&mut s.gesture, vt, vt.grid_ref(cell)?, geometry)? {
                vt.set_selection(Some(&selection))?;
            }
            Ok(s.gesture.autoscroll(vt)? != Autoscroll::None)
        })();
        self.selection_changed(result).unwrap_or(false)
    }

    /// Scrolls one row toward the edge the pointer is past and extends the
    /// selection. Returns whether to keep going.
    pub fn select_autoscroll(&mut self, at: Pointer, rectangle: bool) -> bool {
        let result = (|| -> Result<bool> {
            let coordinate = self.viewport_cell(at);
            let geometry = self.gesture_geometry();
            let (vt, s) = (&self.vt, &mut self.selector);
            s.tick.set_position(f64::from(at.x), f64::from(at.y))?.set_rectangle(rectangle)?;
            if let Some(selection) = s.tick.apply(&mut s.gesture, vt, coordinate, geometry)? {
                vt.set_selection(Some(&selection))?;
            }
            Ok(s.gesture.autoscroll(vt)? != Autoscroll::None)
        })();
        self.selection_changed(result).unwrap_or(false)
    }

    pub fn select_release(&mut self, at: Pointer) -> Release {
        let result = (|| -> Result<bool> {
            let cell = Point::Viewport(self.viewport_cell(at));
            let (vt, s) = (&self.vt, &mut self.selector);
            let chose = s.gesture.dragged(vt)? || s.gesture.click_count(vt)? > 1;
            s.release.apply(&mut s.gesture, vt, vt.grid_ref(cell).ok())?;
            Ok(chose && vt.selection()?.is_some())
        })();
        match result {
            Ok(true) => Release::Selected,
            Ok(false) => Release::Clicked,
            Err(err) => {
                log::warn!("ending a selection failed: {err}");
                Release::Clicked
            }
        }
    }

    pub fn clear_selection(&mut self) {
        let result = self.vt.set_selection(None).map(|_| ());
        self.selection_changed(result);
    }

    /// The selected text, with soft-wrapped lines joined and trailing spaces
    /// trimmed.
    pub fn selection_text(&self) -> Option<String> {
        let text = (|| -> Result<Option<String>> {
            let Some(mut formatter) = Formatter::new_selection(&self.vt)? else { return Ok(None) };
            let bytes = formatter.set_format(Format::Plain)?.set_unwrap(true)?.set_trim(true)?.format_alloc(None)?;
            Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
        })();
        text.unwrap_or_else(|err| {
            log::warn!("reading the selection failed: {err}");
            None
        })
    }

    /// The OSC 8 link over a viewport cell: the cells it covers on that row
    /// (`start..end`) and its URI.
    pub fn hyperlink_at(&self, col: u16, row: u16) -> Option<(u16, u16, String)> {
        let uri = self.hyperlink_uri(col, row)?;
        let same = |x: u16| self.hyperlink_uri(x, row).as_ref() == Some(&uri);
        let cols = self.shared.geometry.get().cols;
        let start = (0..col).rev().take_while(|&x| same(x)).last().unwrap_or(col);
        let end = (col + 1..cols).take_while(|&x| same(x)).last().unwrap_or(col) + 1;
        Some((start, end, uri))
    }

    fn hyperlink_uri(&self, col: u16, row: u16) -> Option<String> {
        let cell = self.vt.grid_ref(Point::Viewport(Coordinate { x: col, y: u32::from(row) })).ok()?;
        let mut buf = vec![0; 256];
        let len = match cell.hyperlink_uri(&mut buf) {
            Err(libghostty_vt::Error::OutOfSpace { required }) => {
                buf.resize(required, 0);
                cell.hyperlink_uri(&mut buf).ok()?
            }
            other => other.ok()?,
        };
        (len > 0).then(|| String::from_utf8_lossy(&buf[..len]).into_owned())
    }

    /// Starts copy mode at the cursor, or at the bottom left when the cursor
    /// is scrolled out of view.
    pub fn start_copy_mode(&mut self) {
        let Ok(bar) = self.vt.scrollbar() else { return };
        let (top, rows) = (bar.offset as u32, bar.len as u32);
        let cursor = self.frame().cursor;
        let pos = match cursor {
            Some(c) => Pos { x: c.x, y: top + u32::from(c.y) },
            None => Pos { x: 0, y: top + rows.saturating_sub(1) },
        };
        self.copy = Some(CopyMode::new(pos));
        self.show_copy_mode();
    }

    pub fn in_copy_mode(&self) -> bool {
        self.copy.is_some()
    }

    /// Whether copy mode waits for the character of an `f` or `t` jump.
    pub fn copy_mode_awaits_jump(&self) -> bool {
        self.copy.as_ref().is_some_and(CopyMode::awaits_jump)
    }

    pub fn copy_mode(&mut self, command: Command) {
        let Some(mut copy) = self.copy.take() else { return };
        copy.apply(&ScreenGrid(&self.vt), command);
        self.copy = Some(copy);
        self.show_copy_mode();
    }

    pub fn copy_mode_jump(&mut self, target: char) {
        let Some(mut copy) = self.copy.take() else { return };
        copy.jump_to(&ScreenGrid(&self.vt), target);
        self.copy = Some(copy);
        self.show_copy_mode();
    }

    /// Ends copy mode, with the selected text when `copy` is set.
    pub fn close_copy_mode(&mut self, copy: bool) -> Option<String> {
        self.copy.take()?;
        let text = if copy { self.selection_text() } else { None };
        self.clear_selection();
        self.vt.scroll_viewport(ScrollViewport::Bottom);
        text
    }

    /// The copy-mode cursor's viewport cell, while it is on screen.
    pub fn copy_cursor(&self) -> Option<(u16, u16)> {
        let pos = self.copy.as_ref()?.cursor();
        let bar = self.vt.scrollbar().ok()?;
        let row = u16::try_from(pos.y.checked_sub(bar.offset as u32)?).ok()?;
        (u64::from(row) < bar.len).then_some((pos.x, row))
    }

    /// Selects what copy mode spans and scrolls its cursor into view.
    fn show_copy_mode(&mut self) {
        let Some(copy) = &self.copy else { return };
        let cols = self.shared.geometry.get().cols;
        let cursor = copy.cursor();
        let span = copy.selection(cols);
        let result = (|| -> Result<()> {
            let screen = |p: Pos| Point::Screen(Coordinate { x: p.x, y: p.y });
            let selection = match span {
                Some((start, end, block)) => {
                    Some(Selection::new(self.vt.grid_ref(screen(start))?, self.vt.grid_ref(screen(end))?, block))
                }
                None => None,
            };
            self.vt.set_selection(selection.as_ref())?;
            Ok(())
        })();
        self.selection_changed(result);
        self.scroll_into_view(cursor.y);
        self.shared.full_redraw.set(true);
    }

    /// Scrolls the viewport the least that shows screen row `y`.
    fn scroll_into_view(&mut self, y: u32) {
        let Ok(bar) = self.vt.scrollbar() else { return };
        let (top, rows) = (bar.offset as u32, bar.len as u32);
        if y < top {
            self.vt.scroll_viewport(ScrollViewport::Row(y as usize));
        } else if y >= top + rows {
            self.vt.scroll_viewport(ScrollViewport::Row((y + 1 - rows) as usize));
        }
    }

    /// Looks for `needle` and moves to the newest match. An empty needle
    /// keeps search open with nothing found. A new search looks for text.
    pub fn search(&mut self, needle: &str) {
        let result = (|| -> Result<Option<Span>> {
            let searching = match &mut self.searching {
                Some(searching) => searching,
                None => self.searching.insert(Searching { needle: String::new(), by: By::text(&mut self.vt)? }),
            };
            searching.needle = needle.to_owned();
            match &mut searching.by {
                By::Text { search, .. } => {
                    search.set_needle(&mut self.vt, needle.as_bytes())?;
                    search.run(&mut self.vt)?;
                    if !needle.is_empty() {
                        search.select_next(&mut self.vt)?;
                    }
                    Ok(None)
                }
                By::Regex(regex) => {
                    regex.set_pattern(&self.vt, needle);
                    Ok(regex.current())
                }
            }
        })();
        match result {
            Ok(Some(span)) => self.scroll_into_view(span.row),
            Ok(None) => {}
            Err(err) => log::warn!("search failed: {err}"),
        }
        self.shared.full_redraw.set(true);
    }

    /// Switches the open search between text and a regex, keeping what it
    /// looks for.
    pub fn toggle_search_regex(&mut self) {
        let Some(searching) = &mut self.searching else { return };
        searching.by = match searching.by {
            By::Text { .. } => By::Regex(RegexSearch::default()),
            By::Regex(_) => match By::text(&mut self.vt) {
                Ok(text) => text,
                Err(err) => return log::warn!("search failed: {err}"),
            },
        };
        let needle = searching.needle.clone();
        self.search(&needle);
    }

    /// Moves to the next older match, or the next newer when `older` is not
    /// set, scrolling it into view.
    pub fn search_step(&mut self, older: bool) {
        let Some(searching) = &mut self.searching else { return };
        match &mut searching.by {
            By::Text { search, .. } => {
                let moved = if older { search.select_next(&mut self.vt) } else { search.select_prev(&mut self.vt) };
                if let Err(err) = moved {
                    log::warn!("moving to a match failed: {err}");
                }
            }
            By::Regex(regex) => {
                regex.refresh(&self.vt);
                if let Some(span) = regex.step(older) {
                    self.scroll_into_view(span.row);
                }
            }
        }
        self.shared.full_redraw.set(true);
    }

    pub fn close_search(&mut self) {
        if self.searching.take().is_some() {
            self.shared.full_redraw.set(true);
        }
    }

    pub fn search_status(&self) -> Option<SearchStatus> {
        let searching = self.searching.as_ref()?;
        let needle = searching.needle.clone();
        Some(match &searching.by {
            By::Text { search, .. } => SearchStatus {
                needle,
                current: search.selected_index().ok().flatten().map(|i| i + 1),
                total: search.total_matches().unwrap_or(0),
                regex: false,
                invalid: false,
            },
            By::Regex(regex) => SearchStatus {
                needle,
                current: regex.position(),
                total: regex.total(),
                regex: true,
                invalid: regex.invalid(),
            },
        })
    }

    /// The parts of the matches on screen, catching up with new output
    /// first.
    pub fn search_highlights(&mut self) -> Vec<Highlight> {
        let Some(searching) = &mut self.searching else { return Vec::new() };
        let geometry = self.shared.geometry.get();
        let (search, matches) = match &mut searching.by {
            By::Text { search, matches } => (search, matches),
            By::Regex(regex) => {
                regex.refresh(&self.vt);
                let Ok(bar) = self.vt.scrollbar() else { return Vec::new() };
                let top = bar.offset as u32;
                return regex
                    .visible(top, bar.len as u32)
                    .map(|(span, current)| Highlight {
                        row: (span.row - top) as u16,
                        start: span.start,
                        end: span.end,
                        current,
                    })
                    .collect();
            }
        };
        let highlights = (|| -> Result<Vec<Highlight>> {
            let snapshot = search.snapshot(&mut self.vt)?;
            let vt = snapshot.terminal();
            let current = snapshot.selected_match()?;
            let mut out = Vec::new();
            for found in snapshot.viewport_matches(matches)? {
                let is_current = match &current {
                    Some(current) => current.equals(vt, &found)?,
                    None => false,
                };
                let start = vt.point_from_grid_ref(&found.start(), Space::Viewport)?;
                let end = vt.point_from_grid_ref(&found.end(), Space::Viewport)?;
                let (Some(start), Some(end)) = (start, end) else { continue };
                for y in start.y..=end.y.min(u32::from(geometry.rows).saturating_sub(1)) {
                    let from = if y == start.y { start.x } else { 0 };
                    let to = if y == end.y { end.x + 1 } else { geometry.cols };
                    out.push(Highlight { row: y as u16, start: from, end: to, current: is_current });
                }
            }
            Ok(out)
        })();
        highlights.unwrap_or_else(|err| {
            log::warn!("reading search matches failed: {err}");
            Vec::new()
        })
    }

    /// The viewport cell under the pointer, inside the grid.
    fn viewport_cell(&self, at: Pointer) -> Coordinate {
        let g = self.shared.geometry.get();
        let x = at.col.min(g.cols.saturating_sub(1));
        let y = at.row.min(g.rows.saturating_sub(1));
        Coordinate { x, y: u32::from(y) }
    }

    fn gesture_geometry(&self) -> gesture::Geometry {
        let g = self.shared.geometry.get();
        gesture::Geometry {
            columns: u32::from(g.cols),
            cell_width: g.cell_width,
            padding_left: 0,
            screen_height: g.height(),
        }
    }

    /// Logs a failed selection change, and redraws: the selection may have
    /// changed anyway.
    fn selection_changed<T>(&self, result: Result<T>) -> Option<T> {
        self.shared.full_redraw.set(true);
        result.inspect_err(|err| log::warn!("selecting failed: {err}")).ok()
    }
}

/// The terminal's text by screen row, for copy mode.
struct ScreenGrid<'a>(&'a libghostty_vt::Terminal<'static, 'static>);

impl copy_mode::Grid for ScreenGrid<'_> {
    fn cols(&self) -> u16 {
        self.0.cols().unwrap_or(1)
    }

    fn rows(&self) -> u32 {
        self.0.total_rows().map_or(1, |rows| u32::try_from(rows).unwrap_or(u32::MAX))
    }

    fn line(&self, y: u32) -> Vec<char> {
        let mut buf = ['\0'; 8];
        (0..self.cols())
            .map(|x| {
                let cell = self.0.grid_ref(Point::Screen(Coordinate { x, y }));
                match cell.and_then(|cell| cell.graphemes(&mut buf)) {
                    Ok(len) if len > 0 => buf[0],
                    _ => ' ',
                }
            })
            .collect()
    }

    fn viewport(&self) -> (u32, u16) {
        self.0.scrollbar().map_or((0, 1), |bar| (bar.offset as u32, bar.len as u16))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::terminal::Geometry;
    use crate::terminal::frame::SelectionColors;

    fn terminal(text: &str) -> Terminal {
        let geometry = Geometry { cols: 20, rows: 4, cell_width: 8, cell_height: 16 };
        let mut t = Terminal::new(geometry, &Config::default()).expect("terminal");
        t.write(text.as_bytes());
        t
    }

    /// The left edge of a cell: a drag there ends before the cell.
    fn at(col: u16, row: u16) -> Pointer {
        Pointer { col, row, x: f32::from(col) * 8.0 + 1.0, y: f32::from(row) * 16.0 + 8.0 }
    }

    #[test]
    fn a_drag_selects_and_reads_back() {
        let mut t = terminal("hello world\r\nsecond");
        t.select_press(at(0, 0), false);
        assert!(!t.select_drag(at(5, 0), false), "inside the screen: no autoscroll");
        assert_eq!(t.select_release(at(5, 0)), Release::Selected);
        assert_eq!(t.selection_text().as_deref(), Some("hello"));
        let selected = &t.frame().rows[0].cells[2];
        assert_eq!(selected.bg, Some(SelectionColors::new(&Config::default().theme).bg));
    }

    #[test]
    fn a_click_selects_nothing_and_shift_extends() {
        let mut t = terminal("hello world");
        t.select_press(at(2, 0), false);
        assert_eq!(t.select_release(at(2, 0)), Release::Clicked);
        assert_eq!(t.selection_text(), None);
        t.select_press(at(0, 0), false);
        t.select_drag(at(2, 0), false);
        t.select_release(at(2, 0));
        t.select_press(at(10, 0), true);
        assert_eq!(t.selection_text().as_deref(), Some("hello world"));
    }

    #[test]
    fn a_double_click_selects_a_word() {
        let mut t = terminal("hello world");
        for _ in 0..2 {
            t.select_press(at(7, 0), false);
            t.select_release(at(7, 0));
        }
        assert_eq!(t.selection_text().as_deref(), Some("world"));
    }

    #[test]
    fn copy_mode_selects_from_its_cursor() {
        let mut t = terminal("one two\r\nthree");
        t.start_copy_mode();
        assert_eq!(t.copy_cursor(), Some((5, 1)), "starts at the terminal cursor");
        t.copy_mode(Command::Move(copy_mode::Motion::LineStart));
        t.copy_mode(Command::Select(copy_mode::Kind::Cell));
        t.copy_mode(Command::Move(copy_mode::Motion::LineContentEnd));
        assert_eq!(t.close_copy_mode(true).as_deref(), Some("three"));
        assert!(!t.in_copy_mode());
        assert_eq!(t.selection_text(), None);
    }

    #[test]
    fn search_counts_and_highlights_matches() {
        let mut t = terminal("abc x abc\r\nAbc");
        t.search("abc");
        let status = t.search_status().expect("open");
        assert_eq!((status.total, status.current), (3, Some(1)));
        let highlights = t.search_highlights();
        assert_eq!(highlights.len(), 3);
        let current: Vec<_> = highlights.iter().filter(|h| h.current).collect();
        assert_eq!(current.len(), 1);
        assert_eq!((current[0].row, current[0].start, current[0].end), (1, 0, 3), "the newest first");
        t.search_step(true);
        assert_eq!(t.search_status().and_then(|s| s.current), Some(2));
        t.close_search();
        assert_eq!(t.search_status(), None);
    }

    #[test]
    fn regex_search_reaches_into_the_scrollback() {
        // Four rows on screen; `id 0`..`id 2` scroll into the scrollback.
        let mut t = terminal("id 0\r\nid 1\r\nid 2\r\nx\r\ny\r\nid 5\r\nz");
        t.search("id");
        t.toggle_search_regex();
        t.search(r"id [0-2]");
        let status = t.search_status().expect("open");
        assert!(status.regex && !status.invalid);
        assert_eq!((status.total, status.current), (3, Some(1)));
        // The newest match, `id 2`, was scrolled into view at the top.
        let highlights = t.search_highlights();
        assert_eq!(highlights.iter().filter(|h| h.current).map(|h| (h.row, h.start, h.end)).next(), Some((0, 0, 4)));
        t.search_step(true);
        t.search_step(true);
        assert_eq!(t.search_status().and_then(|s| s.current), Some(3));
        let highlights = t.search_highlights();
        assert!(highlights.iter().any(|h| h.current && h.row == 0), "`id 0` is in view");

        t.search("id [");
        let status = t.search_status().expect("open");
        assert!(status.invalid);
        assert_eq!(status.total, 0);
        t.toggle_search_regex();
        assert!(!t.search_status().expect("open").regex);
    }

    #[test]
    fn regex_search_catches_up_with_new_output() {
        let mut t = terminal("one");
        t.search("");
        t.toggle_search_regex();
        t.search("o.e");
        assert_eq!(t.search_status().map(|s| s.total), Some(1));
        t.write(b" ole");
        assert_eq!(t.search_highlights().len(), 2);
        assert_eq!(t.search_status().map(|s| s.total), Some(2));
    }

    #[test]
    fn links_span_their_cells() {
        let t = terminal("go \x1b]8;;https://a.io\x1b\\here\x1b]8;;\x1b\\ now");
        assert_eq!(t.hyperlink_at(4, 0), Some((3, 7, "https://a.io".to_owned())));
        assert_eq!(t.hyperlink_at(1, 0), None);
    }

    #[test]
    fn programs_can_ask_for_notifications() {
        let t = terminal("\x1b]9;done\x07\x1b]777;notify;rho;needs you\x1b\\");
        let notifications: Vec<(String, String)> =
            t.take_notifications().into_iter().map(|n| (n.title, n.body)).collect();
        assert_eq!(notifications, [(String::new(), "done".to_owned()), ("rho".to_owned(), "needs you".to_owned())]);
    }

    #[test]
    fn programs_can_set_the_clipboard() {
        let t = terminal("\x1b]52;c;aGVsbG8=\x07");
        assert_eq!(t.take_clipboard().as_deref(), Some("hello"));
        assert_eq!(t.take_clipboard(), None);
    }
}
