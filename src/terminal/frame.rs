//! What one pane looks like, as plain data. It is copied out of libghostty's
//! render state so drawing never touches the terminal, and so a later mux
//! server can ship rows to a client.

use libghostty_vt::color::Rgb;
use libghostty_vt::render::{CellIterator, Colors, CursorVisualStyle, Dirty, RowIteration, RowIterator, Snapshot};
use libghostty_vt::screen::CellWide;
use libghostty_vt::style::Underline;
use serde::{Deserialize, Serialize};

use crate::proto::wire;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Grapheme {
    Empty,
    Char(char),
    Cluster(Box<str>),
}

impl Grapheme {
    fn from_text(text: &str) -> Self {
        let mut chars = text.chars();
        match (chars.next(), chars.next()) {
            (None, _) => Self::Empty,
            (Some(c), None) => Self::Char(c),
            _ => Self::Cluster(text.into()),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CellStyle {
    pub bold: bool,
    pub italic: bool,
    pub faint: bool,
    pub underline: bool,
    pub strikethrough: bool,
    /// SGR 5 or 6; libghostty keeps one flag for both.
    pub blink: bool,
    /// The glyph spans this cell and the next.
    pub wide: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    pub text: Grapheme,
    #[serde(with = "wire::rgb")]
    pub fg: Rgb,
    /// `None` shows the terminal background.
    #[serde(with = "wire::opt_rgb")]
    pub bg: Option<Rgb>,
    pub style: CellStyle,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// Changed since the renderer last read it.
    #[serde(skip)]
    pub dirty: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CursorShape {
    Block,
    Bar,
    Underline,
    BlockHollow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub x: u16,
    pub y: u16,
    pub shape: CursorShape,
    pub wide: bool,
    #[serde(with = "wire::opt_rgb")]
    pub color: Option<Rgb>,
    /// The program asked for a blinking cursor (DECSCUSR, mode 12), or left
    /// the configured default.
    pub blinking: bool,
}

#[derive(Debug)]
pub struct Frame {
    pub cols: u16,
    pub rows: Vec<Row>,
    pub background: Rgb,
    pub foreground: Rgb,
    /// Visible and inside the viewport.
    pub cursor: Option<Cursor>,
    /// Anything changed since the renderer last drew. The renderer clears it.
    pub changed: bool,
}

impl Default for Frame {
    fn default() -> Self {
        let black = Rgb { r: 0, g: 0, b: 0 };
        Self { cols: 0, rows: Vec::new(), background: black, foreground: black, cursor: None, changed: true }
    }
}

/// How selected cells look.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionColors {
    pub fg: Rgb,
    pub bg: Rgb,
}

impl SelectionColors {
    pub fn new(theme: &crate::config::Theme) -> Self {
        let rgb = |[r, g, b]: crate::config::Rgb| Rgb { r, g, b };
        Self { fg: rgb(theme.selection_fg), bg: rgb(theme.selection_bg) }
    }
}

/// Reusable libghostty iterators, so a capture allocates nothing on their side.
pub(super) struct Iterators {
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    text: String,
}

impl Iterators {
    pub fn new() -> libghostty_vt::error::Result<Self> {
        Ok(Self { rows: RowIterator::new()?, cells: CellIterator::new()?, text: String::new() })
    }
}

impl Frame {
    /// Copies what changed in `snapshot` and marks the snapshot clean.
    pub(super) fn capture(
        &mut self,
        snapshot: &Snapshot<'static, '_>,
        it: &mut Iterators,
        selection: SelectionColors,
        full: bool,
    ) -> libghostty_vt::error::Result<()> {
        let colors = snapshot.colors()?;
        let cols = snapshot.cols()?;
        let rows = usize::from(snapshot.rows()?);
        let mut dirty = if full { Dirty::Full } else { snapshot.dirty()? };
        if cols != self.cols
            || rows != self.rows.len()
            || colors.background != self.background
            || colors.foreground != self.foreground
        {
            self.cols = cols;
            self.rows.resize_with(rows, Row::default);
            self.background = colors.background;
            self.foreground = colors.foreground;
            dirty = Dirty::Full;
        }

        let cursor = cursor(snapshot, colors.cursor)?;
        if cursor != self.cursor {
            self.cursor = cursor;
            self.changed = true;
        }

        let mut iteration = it.rows.update(snapshot)?;
        match dirty {
            Dirty::Clean => {}
            Dirty::Partial => {
                while let Some((y, row)) = iteration.next_dirty() {
                    if let Some(target) = self.rows.get_mut(usize::from(y)) {
                        copy_row(row, target, &colors, selection, &mut it.cells, &mut it.text)?;
                        self.changed = true;
                    }
                }
            }
            Dirty::Full => {
                for target in &mut self.rows {
                    let Some(row) = iteration.next() else { break };
                    copy_row(row, target, &colors, selection, &mut it.cells, &mut it.text)?;
                }
                self.changed = true;
            }
        }
        snapshot.clean()
    }
}

fn copy_row(
    row: &RowIteration<'static, '_>,
    target: &mut Row,
    colors: &Colors,
    selection: SelectionColors,
    cells: &mut CellIterator<'static>,
    text: &mut String,
) -> libghostty_vt::error::Result<()> {
    target.cells.clear();
    target.dirty = true;
    let selected = row.selection()?.map(|s| usize::from(s.start_x)..=usize::from(s.end_x));
    let mut iteration = cells.update(row)?;
    while let Some(cell) = iteration.next() {
        let wide = cell.raw_cell()?.wide()?;
        let mut fg = cell.fg_color()?.unwrap_or(colors.foreground);
        let mut bg = cell.bg_color()?;
        let mut style = CellStyle { wide: wide == CellWide::Wide, ..CellStyle::default() };
        let mut visible = !matches!(wide, CellWide::SpacerTail | CellWide::SpacerHead);
        if cell.has_styling()? {
            let s = cell.style()?;
            style.bold = s.bold;
            style.italic = s.italic;
            style.faint = s.faint;
            style.underline = s.underline != Underline::None;
            style.strikethrough = s.strikethrough;
            style.blink = s.blink;
            visible &= !s.invisible;
            if s.inverse {
                let old_fg = fg;
                fg = bg.unwrap_or(colors.background);
                bg = Some(old_fg);
            }
        }
        if selected.as_ref().is_some_and(|range| range.contains(&target.cells.len())) {
            fg = selection.fg;
            bg = Some(selection.bg);
        }
        text.clear();
        if visible && cell.graphemes_len()? > 0 {
            cell.graphemes_utf8(text)?;
        }
        target.cells.push(Cell { text: Grapheme::from_text(text), fg, bg, style });
    }
    Ok(())
}

fn cursor(snapshot: &Snapshot<'static, '_>, color: Option<Rgb>) -> libghostty_vt::error::Result<Option<Cursor>> {
    let cursor = snapshot.cursor()?;
    let Some(viewport) = cursor.viewport.filter(|_| cursor.visible) else {
        return Ok(None);
    };
    let shape = match cursor.visual_style {
        CursorVisualStyle::Bar => CursorShape::Bar,
        CursorVisualStyle::Underline => CursorShape::Underline,
        CursorVisualStyle::BlockHollow => CursorShape::BlockHollow,
        _ => CursorShape::Block,
    };
    Ok(Some(Cursor {
        x: viewport.x,
        y: viewport.y,
        shape,
        wide: viewport.at_wide_tail,
        color,
        blinking: cursor.blinking,
    }))
}
