//! The tab bar: one line of cells across the top of the window, like the
//! WezTerm tabline setup it replaces.

use std::ops::Range;
use std::path::Path;

use libghostty_vt::color::Rgb;

use crate::animation;
use crate::config::{self, Stat, Theme};
use crate::font::Metrics;
use crate::mux::{RhoActivity, TabView};
use crate::renderer::Line;
use crate::status::{self, Ink, Stats};
use crate::terminal::CellStyle;

/// The longest directory name shown before it is cut with `…`.
const CWD_MAX: usize = 10;

/// Cells the tab bar keeps clear at each end, under the window's rounded
/// corners.
pub const EDGE: usize = 2;

pub struct TabBar {
    pub line: Line,
    /// Each tab's cells, for clicks.
    pub tabs: Vec<Range<usize>>,
    /// The marks of working tabs, left blank in `line`: they animate on a
    /// layer of their own.
    pub spinners: Vec<Spinner>,
}

/// A cell of the tab bar that shows rho's working spinner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spinner {
    pub cell: usize,
    pub color: Rgb,
}

/// Lays out `tabs` across `cols` cells. Each reads `⠋ 2 dir `: rho's mark,
/// the index, then the active pane's directory. Without `focused`, the bar
/// greys out as WezTerm's `window_frame` does; rho's static marks keep their colors.
/// An inactive tab under the pointer (`hover`) turns aqua.
pub fn build(
    tabs: &[TabView<'_>],
    theme: &Theme,
    cols: usize,
    max_width: u16,
    width: f32,
    focused: bool,
    hover: Option<usize>,
) -> TabBar {
    let rgb = |[r, g, b]: config::Rgb| Rgb { r, g, b };
    let background = if focused { theme.background } else { theme.inactive_titlebar_bg };
    let mut line = Line::new(0.0, 0.0, width, rgb(background));
    let mut ranges = Vec::new();
    let mut spinners = Vec::new();
    for (i, tab) in tabs.iter().enumerate() {
        let inset = if i == 0 { " ".repeat(EDGE) } else { String::new() };
        let fg = match (focused, tab.active) {
            (false, _) => theme.inactive_titlebar_fg,
            (true, true) => theme.ansi[3],
            (true, false) if hover == Some(i) => theme.ansi[6],
            (true, false) => theme.foreground,
        };
        // A cell for a mark, so titles never shift when one appears. The
        // active tab's working spinner takes the title's color; the others
        // stay grey.
        let blue = theme.ansi[4];
        let working = tab.info.rho == Some(RhoActivity::Working);
        let (mark, mark_color) = match tab.info.rho {
            Some(RhoActivity::Working) => (' ', if tab.active { fg } else { theme.ansi[8] }),
            Some(RhoActivity::Question) => ('?', blue),
            Some(RhoActivity::Stopped) => ('✕', theme.ansi[1]),
            Some(RhoActivity::Unread) => ('•', theme.ansi[2]),
            None => (' ', theme.foreground),
        };
        let cwd = tab.info.cwd.as_deref().map(dir_name).unwrap_or_default();
        let text = format!("{inset}{mark} {} {cwd} ", i + 1);
        let text: String = text.chars().take(usize::from(max_width)).collect();
        let start = line.cells.len();
        if start + text.chars().count() > cols {
            break;
        }
        line.push_str(&text, rgb(fg), CellStyle::default());
        let mark_cell = start + inset.len();
        if let Some(cell) = line.cells.get_mut(mark_cell) {
            cell.fg = rgb(mark_color);
        }
        if working {
            spinners.push(Spinner { cell: mark_cell, color: rgb(mark_color) });
        }
        ranges.push(start..line.cells.len());
        // The separator belongs to no tab.
        line.push_str(" ", rgb(theme.foreground), CellStyle::default());
    }
    TabBar { line, tabs: ranges, spinners }
}

/// What the strip over the tab bar shows, on the tab bar's `background`: the
/// working tabs' marks at `frame` of the spinner, and the system stats ending
/// before cell `status_end`.
#[derive(Clone, Debug, PartialEq)]
pub struct Strip {
    pub spinners: Vec<Spinner>,
    pub background: Rgb,
    pub frame: usize,
    pub status_end: usize,
    pub status: Vec<(String, Rgb)>,
}

impl Strip {
    /// One line per mark, then the stats, for a layer over the tab bar in
    /// cells of `m`.
    pub fn lines(&self, m: Metrics) -> Vec<Line> {
        let cell_width = m.cell_width as f32;
        let mark = animation::SPINNER[self.frame % animation::SPINNER.len()];
        // The spinner's dots fill the top three of the cell's four braille
        // rows; an eighth of the cell down centers them, level with the title.
        let drop = (m.cell_height / 8) as f32;
        let mut lines: Vec<Line> = self
            .spinners
            .iter()
            .map(|s| {
                let mut line = Line::new(s.cell as f32 * cell_width, drop, cell_width, self.background);
                line.push_str(mark.encode_utf8(&mut [0; 4]), s.color, CellStyle::default());
                line
            })
            .collect();
        if !self.status.is_empty() {
            let mut line = Line::new(0.0, 0.0, 0.0, self.background);
            for (text, color) in &self.status {
                line.push_str(text, *color, CellStyle::default());
            }
            let cells = line.cells.len();
            line.x = self.status_end.saturating_sub(cells) as f32 * cell_width;
            line.width = cells as f32 * cell_width;
            lines.push(line);
        }
        lines
    }
}

/// The stats `items` show, colored as the tab bar is: grey without `focused`.
pub fn status(items: &[Stat], stats: &Stats, theme: &Theme, focused: bool) -> Vec<(String, Rgb)> {
    let rgb = |[r, g, b]: config::Rgb| Rgb { r, g, b };
    status::segments(items, stats)
        .into_iter()
        .map(|(text, ink)| {
            let color = match ink {
                _ if !focused => theme.inactive_titlebar_fg,
                Ink::Text => theme.foreground,
                Ink::Down => theme.ansi[4],
                Ink::Up => theme.ansi[2],
            };
            (text, rgb(color))
        })
        .collect()
}

fn dir_name(path: &Path) -> String {
    let name = path.file_name().map_or_else(|| path.to_string_lossy(), |name| name.to_string_lossy());
    if name.chars().count() > CWD_MAX {
        name.chars().take(CWD_MAX - 1).chain(['…']).collect()
    } else {
        name.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mux::PaneInfo;

    fn text(bar: &TabBar) -> String {
        bar.line
            .cells
            .iter()
            .map(|c| match &c.text {
                crate::terminal::Grapheme::Char(c) => *c,
                _ => '?',
            })
            .collect()
    }

    #[test]
    fn tabs_show_index_and_directory() {
        let home = PaneInfo { cwd: Some("/Users/me".into()), ..PaneInfo::default() };
        let long = PaneInfo { cwd: Some("/src/a-very-long-name".into()), ..PaneInfo::default() };
        let tabs = [TabView { active: false, info: &home }, TabView { active: true, info: &long }];
        let bar = build(&tabs, &Theme::default(), 80, 32, 800.0, true, None);
        assert_eq!(text(&bar), "    1 me    2 a-very-lo…  ");
        assert_eq!(bar.tabs, vec![0..9, 10..25]);
        assert_eq!(bar.line.cells[11].fg, bar.line.cells[12].fg);
        assert_ne!(bar.line.cells[2].fg, bar.line.cells[11].fg, "the active tab stands out");
    }

    #[test]
    fn rho_marks_its_tab() {
        let rho = PaneInfo { cwd: Some("/a".into()), rho: Some(RhoActivity::Question), ..PaneInfo::default() };
        let bar = build(&[TabView { active: true, info: &rho }], &Theme::default(), 80, 32, 800.0, true, None);
        assert_eq!(text(&bar), "  ? 1 a  ");
        let [r, g, b] = Theme::default().ansi[4];
        assert_eq!(bar.line.cells[2].fg, Rgb { r, g, b }, "the mark has its own color");
        assert_ne!(bar.line.cells[2].fg, bar.line.cells[4].fg);
    }

    #[test]
    fn working_tabs_leave_their_mark_to_a_spinner_in_the_title_color_only_when_active() {
        let rho = PaneInfo { cwd: Some("/a".into()), rho: Some(RhoActivity::Working), ..PaneInfo::default() };
        let tabs = [TabView { active: true, info: &rho }, TabView { active: false, info: &rho }];
        let theme = Theme::default();
        let bar = build(&tabs, &theme, 80, 32, 800.0, true, None);
        assert_eq!(text(&bar), "    1 a    2 a  ");
        let rgb = |[r, g, b]: config::Rgb| Rgb { r, g, b };
        let spinner = |cell, color| Spinner { cell, color: rgb(color) };
        assert_eq!(bar.spinners, vec![spinner(2, theme.ansi[3]), spinner(9, theme.ansi[8])]);
    }

    #[test]
    fn the_strip_draws_each_mark_and_the_stats_at_their_cells() {
        let blue = Rgb { r: 0, g: 0, b: 255 };
        let black = Rgb { r: 0, g: 0, b: 0 };
        let strip = Strip {
            spinners: vec![Spinner { cell: 3, color: blue }],
            background: black,
            frame: 12,
            status_end: 74,
            status: vec![("7%".into(), blue), (" |".into(), black)],
        };
        let m = Metrics { cell_width: 10, cell_height: 40, baseline: 30, underline_offset: 33, stroke: 2 };
        let lines = strip.lines(m);
        assert_eq!(lines.len(), 2);
        assert_eq!((lines[0].x, lines[0].y, lines[0].width, lines[0].background), (30.0, 5.0, 10.0, black));
        assert_eq!(lines[0].cells[0].text, crate::terminal::Grapheme::Char(animation::SPINNER[2]));
        assert_eq!(lines[0].cells[0].fg, blue);
        assert_eq!((lines[1].x, lines[1].y, lines[1].width, lines[1].cells.len()), (700.0, 0.0, 40.0, 4));
        assert_eq!((lines[1].cells[1].fg, lines[1].cells[3].fg), (blue, black));
    }

    #[test]
    fn stats_grey_out_with_the_bar() {
        let theme = Theme::default();
        let rgb = |[r, g, b]: config::Rgb| Rgb { r, g, b };
        let colors = |focused| -> Vec<Rgb> {
            status(&[Stat::Net], &Stats::default(), &theme, focused).into_iter().map(|(_, c)| c).collect()
        };
        assert!(colors(true).contains(&rgb(theme.ansi[4])), "the received arrow is blue");
        assert!(colors(false).iter().all(|&c| c == rgb(theme.inactive_titlebar_fg)));
    }

    #[test]
    fn tabs_that_do_not_fit_are_left_out() {
        let root = PaneInfo { cwd: Some("/".into()), ..PaneInfo::default() };
        let tabs = [TabView { active: true, info: &root }, TabView { active: false, info: &root }];
        let bar = build(&tabs, &Theme::default(), 10, 32, 100.0, true, None);
        assert_eq!(text(&bar), "    1 /  ");
        assert_eq!(bar.tabs.len(), 1);
    }

    #[test]
    fn an_unfocused_bar_greys_all_but_rho_marks() {
        let rho = PaneInfo { cwd: Some("/a".into()), rho: Some(RhoActivity::Stopped), ..PaneInfo::default() };
        let plain = PaneInfo { cwd: Some("/b".into()), ..PaneInfo::default() };
        let tabs = [TabView { active: true, info: &rho }, TabView { active: false, info: &plain }];
        let theme = Theme::default();
        let bar = build(&tabs, &theme, 80, 32, 800.0, false, None);
        let rgb = |[r, g, b]: config::Rgb| Rgb { r, g, b };
        assert_eq!(bar.line.background, rgb(theme.inactive_titlebar_bg));
        assert_eq!(text(&bar), "  ✕ 1 a    2 b  ");
        assert_eq!(bar.line.cells[2].fg, rgb(theme.ansi[1]), "the mark keeps its color");
        for i in [4, 6, 11, 13] {
            assert_eq!(bar.line.cells[i].fg, rgb(theme.inactive_titlebar_fg), "cell {i}");
        }
    }

    #[test]
    fn a_hovered_inactive_tab_turns_aqua() {
        let a = PaneInfo { cwd: Some("/a".into()), ..PaneInfo::default() };
        let tabs = [TabView { active: true, info: &a }, TabView { active: false, info: &a }];
        let theme = Theme::default();
        let rgb = |[r, g, b]: config::Rgb| Rgb { r, g, b };
        let bar = build(&tabs, &theme, 80, 32, 800.0, true, Some(1));
        assert_eq!(bar.line.cells[11].fg, rgb(theme.ansi[6]));
        let bar = build(&tabs, &theme, 80, 32, 800.0, true, Some(0));
        assert_eq!(bar.line.cells[4].fg, rgb(theme.ansi[3]), "the active tab stays yellow");
        assert_eq!(bar.line.cells[11].fg, rgb(theme.foreground));
    }
}
