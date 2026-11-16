//! The tab bar: one line of cells across the top of the window, like the
//! WezTerm tabline setup it replaces.

use std::ops::Range;
use std::path::Path;

use libghostty_vt::color::Rgb;

use crate::config::{self, Theme};
use crate::mux::{RhoActivity, TabView};
use crate::renderer::Line;
use crate::terminal::CellStyle;

/// The longest directory name shown before it is cut with `…`.
const CWD_MAX: usize = 10;

pub struct TabBar {
    pub line: Line,
    /// Each tab's cells, for clicks.
    pub tabs: Vec<Range<usize>>,
}

/// Lays out `tabs` across `cols` cells. Each reads `✦ 2 dir `: rho's mark,
/// the index, then the active pane's directory.
pub fn build(tabs: &[TabView<'_>], theme: &Theme, cols: usize, max_width: u16, width: f32) -> TabBar {
    let rgb = |[r, g, b]: config::Rgb| Rgb { r, g, b };
    let mut line = Line::new(0.0, 0.0, width, rgb(theme.background));
    let mut ranges = Vec::new();
    for (i, tab) in tabs.iter().enumerate() {
        // The first tab starts clear of the window's rounded corner.
        let inset = if i == 0 { " " } else { "" };
        // A cell for a mark, so titles never shift when one appears.
        let mark = match tab.info.rho {
            Some(RhoActivity::Working) => '✦',
            Some(RhoActivity::Question) => '?',
            Some(RhoActivity::Stopped) => '✕',
            Some(RhoActivity::Unread) => '•',
            None => ' ',
        };
        let cwd = tab.info.cwd.as_deref().map(dir_name).unwrap_or_default();
        let text = format!("{inset}{mark} {} {cwd} ", i + 1);
        let text: String = text.chars().take(usize::from(max_width)).collect();
        let start = line.cells.len();
        if start + text.chars().count() > cols {
            break;
        }
        let fg = if tab.active { theme.ansi[3] } else { theme.foreground };
        line.push_str(&text, rgb(fg), CellStyle::default());
        ranges.push(start..line.cells.len());
        // The separator belongs to no tab.
        line.push_str(" ", rgb(theme.foreground), CellStyle::default());
    }
    TabBar { line, tabs: ranges }
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
        let bar = build(&tabs, &Theme::default(), 80, 32, 800.0);
        assert_eq!(text(&bar), "   1 me    2 a-very-lo…  ");
        assert_eq!(bar.tabs, vec![0..8, 9..24]);
        assert_eq!(bar.line.cells[10].fg, bar.line.cells[11].fg);
        assert_ne!(bar.line.cells[1].fg, bar.line.cells[10].fg, "the active tab stands out");
    }

    #[test]
    fn rho_marks_its_tab() {
        let rho = PaneInfo { cwd: Some("/a".into()), rho: Some(RhoActivity::Question), ..PaneInfo::default() };
        let bar = build(&[TabView { active: true, info: &rho }], &Theme::default(), 80, 32, 800.0);
        assert_eq!(text(&bar), " ? 1 a  ");
    }

    #[test]
    fn tabs_that_do_not_fit_are_left_out() {
        let root = PaneInfo { cwd: Some("/".into()), ..PaneInfo::default() };
        let tabs = [TabView { active: true, info: &root }, TabView { active: false, info: &root }];
        let bar = build(&tabs, &Theme::default(), 10, 32, 100.0);
        assert_eq!(text(&bar), "   1 /  ");
        assert_eq!(bar.tabs.len(), 1);
    }
}
