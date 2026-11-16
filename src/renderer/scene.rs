//! Turns a frame into GPU instances. Each row's instances are kept, so only
//! rows libghostty marked dirty, and rows the cursor left or entered, are
//! rebuilt.

use libghostty_vt::color::Rgb;
use libghostty_vt::unicode;

use super::atlas::{Atlas, Full, Glyph};
use crate::font::{Face, Fonts, Metrics, Weight};
use crate::terminal::{Cell, CellStyle, Cursor, CursorShape, Frame, Grapheme};

/// A solid rectangle, in pixels from the grid's top-left corner. The layout
/// matches `RectInstance` in shaders.metal.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RectInstance {
    pub pos: [f32; 2],
    pub size: [f32; 2],
    pub color: [u8; 4],
    pad: u32,
}

/// A glyph from the atlas. The layout matches `GlyphInstance` in
/// shaders.metal.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlyphInstance {
    pub pos: [f32; 2],
    pub size: [f32; 2],
    pub uv: [f32; 2],
    pub color: [u8; 4],
    pub colored: u32,
}

/// What is drawn on top of the terminal's own content.
#[derive(Clone, Copy, Debug)]
pub struct Overlay<'a> {
    pub focused: bool,
    /// Copy mode's cursor, a block in place of the terminal's.
    pub copy_cursor: Option<(u16, u16)>,
    /// Text the input method is composing, shown at the cursor.
    pub preedit: &'a str,
}

/// A line of text outside the grid, such as a banner or the tab bar, in
/// device pixels from the drawable's top-left corner.
#[derive(Clone, Debug)]
pub struct Line {
    pub x: f32,
    pub y: f32,
    /// The background fills this width.
    pub width: f32,
    pub background: Rgb,
    pub cells: Vec<Cell>,
}

impl Line {
    pub fn new(x: f32, y: f32, width: f32, background: Rgb) -> Self {
        Self { x, y, width, background, cells: Vec::new() }
    }

    /// Appends `text`, one cell per character (two for wide ones).
    pub fn push_str(&mut self, text: &str, fg: Rgb, style: CellStyle) {
        self.cells.extend(text_cells(text, fg, style));
    }
}

/// `text` as cells, one per character (two for wide ones).
fn text_cells(text: &str, fg: Rgb, style: CellStyle) -> impl Iterator<Item = Cell> {
    text.chars().flat_map(move |c| {
        let wide = unicode::codepoint_width(c) > 1;
        let cell = Cell { text: Grapheme::Char(c), fg, bg: None, style: CellStyle { wide, ..style } };
        let spacer = wide.then_some(Cell { text: Grapheme::Empty, fg, bg: None, style });
        std::iter::once(cell).chain(spacer)
    })
}

#[derive(Default)]
struct RowCache {
    rects: Vec<RectInstance>,
    glyphs: Vec<GlyphInstance>,
    blink_rects: Vec<RectInstance>,
    blink_glyphs: Vec<GlyphInstance>,
}

/// One pane's instances in draw order: `rects` (the pane's background
/// first), `glyphs`, the blinking cells' decorations and glyphs, then the
/// overlay's rects and glyphs (cursor, preedit) on top, relative to the pane.
/// Blinking cells and the overlay have batches of their own, so they can be
/// left out of a frame without building it again.
#[derive(Default)]
pub struct Scene {
    rows: Vec<RowCache>,
    rebuild_all: bool,
    /// The cell rows were shaped around: runs break at the cursor.
    split: Option<(u16, u16)>,
    pub rects: Vec<RectInstance>,
    pub glyphs: Vec<GlyphInstance>,
    pub blink_rects: Vec<RectInstance>,
    pub blink_glyphs: Vec<GlyphInstance>,
    pub overlay_rects: Vec<RectInstance>,
    pub overlay_glyphs: Vec<GlyphInstance>,
}

impl Scene {
    /// Rebuilds every row on the next build, after the atlas or fonts changed.
    pub fn invalidate(&mut self) {
        self.rebuild_all = true;
    }

    /// Fails when the atlas is full; the caller resets it, invalidates every
    /// scene and builds again.
    pub fn build(
        &mut self,
        frame: &mut Frame,
        overlay: &Overlay<'_>,
        fonts: &Fonts,
        atlas: &mut Atlas,
    ) -> Result<(), Full> {
        if self.rows.len() != frame.rows.len() {
            self.rows.resize_with(frame.rows.len(), RowCache::default);
            self.rebuild_all = true;
        }
        let cursor = cursor(frame, overlay);
        let split = cursor.map(|c| (c.x, c.y));
        if split != self.split {
            for (_, y) in [self.split, split].into_iter().flatten() {
                if let Some(row) = frame.rows.get_mut(usize::from(y)) {
                    row.dirty = true;
                }
            }
            self.split = split;
        }
        self.build_rows(frame, split, fonts, atlas)?;
        self.rebuild_all = false;

        self.rects.clear();
        self.glyphs.clear();
        self.blink_rects.clear();
        self.blink_glyphs.clear();
        // The pane's own background, so its tint covers the empty cells too.
        let m = fonts.metrics();
        let size = [f32::from(frame.cols) * m.cell_width as f32, frame.rows.len() as f32 * m.cell_height as f32];
        self.rects.push(rect(0.0, 0.0, size[0], size[1], frame.background));
        for row in &self.rows {
            self.rects.extend_from_slice(&row.rects);
            self.glyphs.extend_from_slice(&row.glyphs);
            self.blink_rects.extend_from_slice(&row.blink_rects);
            self.blink_glyphs.extend_from_slice(&row.blink_glyphs);
        }
        self.overlay_rects.clear();
        self.overlay_glyphs.clear();
        self.build_overlay(frame, overlay, cursor, fonts, atlas)?;
        frame.changed = false;
        Ok(())
    }

    fn build_rows(
        &mut self,
        frame: &mut Frame,
        split: Option<(u16, u16)>,
        fonts: &Fonts,
        atlas: &mut Atlas,
    ) -> Result<(), Full> {
        let m = fonts.metrics();
        for (y, (row, cache)) in frame.rows.iter_mut().zip(&mut self.rows).enumerate() {
            if !row.dirty && !self.rebuild_all {
                continue;
            }
            cache.rects.clear();
            cache.glyphs.clear();
            cache.blink_rects.clear();
            cache.blink_glyphs.clear();
            let top = y as f32 * m.cell_height as f32;
            for (x, cell) in row.cells.iter().enumerate() {
                let left = x as f32 * m.cell_width as f32;
                if let Some(bg) = cell.bg.filter(|bg| *bg != frame.background) {
                    push_bg(&mut cache.rects, left, top, m, bg);
                }
                let rects = if cell.style.blink { &mut cache.blink_rects } else { &mut cache.rects };
                decorations(rects, cell, left, top, m);
            }
            let split = split.filter(|&(_, cy)| usize::from(cy) == y).map(|(x, _)| usize::from(x));
            let (glyphs, blink) = (&mut cache.glyphs, &mut cache.blink_glyphs);
            let mut out = |cell: &Cell, g| if cell.style.blink { blink.push(g) } else { glyphs.push(g) };
            push_text(&mut out, &row.cells, [0.0, top], split, None, fonts, atlas)?;
            row.dirty = false;
        }
        Ok(())
    }

    fn build_overlay(
        &mut self,
        frame: &Frame,
        overlay: &Overlay<'_>,
        cursor: Option<Cursor>,
        fonts: &Fonts,
        atlas: &mut Atlas,
    ) -> Result<(), Full> {
        let m = fonts.metrics();
        let Some(cursor) = cursor else { return Ok(()) };
        let (left, top) = (f32::from(cursor.x) * m.cell_width as f32, f32::from(cursor.y) * m.cell_height as f32);
        let (cw, ch, stroke) = (m.cell_width as f32, m.cell_height as f32, m.stroke as f32);

        if !overlay.preedit.is_empty() {
            let cells: Vec<Cell> = text_cells(overlay.preedit, frame.foreground, CellStyle::default()).collect();
            let width = cw * cells.len() as f32;
            self.overlay_rects.push(rect(left, top, width, ch, frame.background));
            push_text(&mut |_, g| self.overlay_glyphs.push(g), &cells, [left, top], None, None, fonts, atlas)?;
            let underline = top + m.underline_offset as f32;
            self.overlay_rects.push(rect(left, underline, width, stroke, frame.foreground));
            return Ok(());
        }

        let under = frame.rows.get(usize::from(cursor.y)).and_then(|row| row.cells.get(usize::from(cursor.x)));
        let width = cw * if under.is_some_and(|c| c.style.wide) { 2.0 } else { 1.0 };
        let color = cursor.color.unwrap_or(frame.foreground);
        let shape =
            if overlay.focused || overlay.copy_cursor.is_some() { cursor.shape } else { CursorShape::BlockHollow };
        match shape {
            CursorShape::Block => {
                self.overlay_rects.push(rect(left, top, width, ch, color));
                // The text under a block cursor takes the background color.
                if let Some(cell) = under {
                    let cell = std::slice::from_ref(cell);
                    let out = &mut |_: &Cell, g| self.overlay_glyphs.push(g);
                    push_text(out, cell, [left, top], None, Some(frame.background), fonts, atlas)?;
                }
            }
            CursorShape::Bar => self.overlay_rects.push(rect(left, top, stroke.max(2.0), ch, color)),
            CursorShape::Underline => self.overlay_rects.push(rect(left, top + ch - stroke, width, stroke, color)),
            CursorShape::BlockHollow => {
                self.overlay_rects.extend([
                    rect(left, top, width, stroke, color),
                    rect(left, top + ch - stroke, width, stroke, color),
                    rect(left, top, stroke, ch, color),
                    rect(left + width - stroke, top, stroke, ch, color),
                ]);
            }
        }
        Ok(())
    }
}

/// Where the cursor is drawn: copy mode's, or else the terminal's, on the
/// first cell of a wide character.
fn cursor(frame: &Frame, overlay: &Overlay<'_>) -> Option<Cursor> {
    let mut cursor = match overlay.copy_cursor {
        Some((x, y)) => Cursor { x, y, shape: CursorShape::Block, wide: false, color: None, blinking: false },
        None => frame.cursor?,
    };
    if cursor.wide {
        cursor.x = cursor.x.saturating_sub(1);
    }
    Some(cursor)
}

/// Lines outside the grid, relative to the drawable. A few lines, rebuilt
/// whole each frame.
#[derive(Default)]
pub struct Chrome {
    pub rects: Vec<RectInstance>,
    pub glyphs: Vec<GlyphInstance>,
}

impl Chrome {
    pub fn build(&mut self, lines: &[Line], fonts: &Fonts, atlas: &mut Atlas) -> Result<(), Full> {
        let m = fonts.metrics();
        self.rects.clear();
        self.glyphs.clear();
        for line in lines {
            self.rects.push(rect(line.x, line.y, line.width, m.cell_height as f32, line.background));
            for (x, cell) in line.cells.iter().enumerate() {
                let left = line.x + x as f32 * m.cell_width as f32;
                if let Some(bg) = cell.bg {
                    push_bg(&mut self.rects, left, line.y, m, bg);
                }
                decorations(&mut self.rects, cell, left, line.y, m);
            }
            push_text(&mut |_, g| self.glyphs.push(g), &line.cells, [line.x, line.y], None, None, fonts, atlas)?;
        }
        Ok(())
    }
}

fn face(style: CellStyle) -> Face {
    let weight = if style.bold {
        Weight::Bold
    } else if style.faint {
        Weight::Faint
    } else {
        Weight::Regular
    };
    Face { weight, italic: style.italic }
}

/// Shapes `cells`, starting at `left, top`, in runs of one face and blink,
/// and gives `out` each glyph with its cell, placed at that cell, so text
/// keeps to the grid whatever the font's advances. Runs also end around cell
/// `split`, so the cursor never sits inside a ligature. Glyphs take their
/// cell's color, or `color`.
fn push_text(
    out: &mut impl FnMut(&Cell, GlyphInstance),
    cells: &[Cell],
    [left, top]: [f32; 2],
    split: Option<usize>,
    color: Option<Rgb>,
    fonts: &Fonts,
    atlas: &mut Atlas,
) -> Result<(), Full> {
    let cell_width = fonts.metrics().cell_width as f32;
    // Trailing blanks draw nothing; leaving them out lets more rows share a shape.
    let cells = &cells[..cells.iter().rposition(|c| c.text != Grapheme::Empty).map_or(0, |i| i + 1)];
    let mut text = String::new();
    // The byte each cell's text starts at, and the cell.
    let mut starts: Vec<(usize, usize)> = Vec::new();
    let mut start = 0;
    while start < cells.len() {
        let run = |cell: &Cell| (face(cell.style), cell.style.blink);
        let (run_face, _) = run(&cells[start]);
        let mut end = start + 1;
        while end < cells.len()
            && run(&cells[end]) == run(&cells[start])
            && split != Some(end)
            && split != Some(end - 1)
        {
            end += 1;
        }
        text.clear();
        starts.clear();
        for x in start..end {
            // The cell after a wide one is its spacer.
            if x > 0 && cells[x - 1].style.wide {
                continue;
            }
            starts.push((text.len(), x));
            match &cells[x].text {
                Grapheme::Empty => text.push(' '),
                Grapheme::Char(c) => text.push(*c),
                Grapheme::Cluster(s) => text.push_str(s),
            }
        }
        // Each cell's glyphs are placed from the pen position of its first.
        let mut anchor: Option<(usize, f32)> = None;
        for g in fonts.shape(&text, run_face).iter() {
            let x = starts[starts.partition_point(|&(byte, _)| byte <= g.byte).saturating_sub(1)].1;
            let origin = match anchor {
                Some((cell, origin)) if cell == x => origin,
                _ => anchor.insert((x, g.x)).1,
            };
            if let Some(glyph) = atlas.glyph(fonts, g.font, g.glyph)? {
                let pen = left + x as f32 * cell_width + (g.x - origin);
                out(&cells[x], glyph_instance(glyph, pen, top, color.unwrap_or(cells[x].fg)));
            }
        }
        start = end;
    }
    Ok(())
}

fn glyph_instance(g: Glyph, left: f32, top: f32, color: Rgb) -> GlyphInstance {
    GlyphInstance {
        pos: [left + g.left as f32, top + g.top as f32],
        size: [g.width as f32, g.height as f32],
        uv: [g.x as f32, g.y as f32],
        color: rgba(color),
        colored: u32::from(g.colored),
    }
}

/// Adds a cell background, extending the previous one when it continues it.
fn push_bg(rects: &mut Vec<RectInstance>, left: f32, top: f32, m: Metrics, color: Rgb) {
    if let Some(last) = rects.last_mut()
        && last.color == rgba(color)
        && last.pos[1] == top
        && last.size[1] == m.cell_height as f32
        && last.pos[0] + last.size[0] == left
    {
        last.size[0] += m.cell_width as f32;
        return;
    }
    rects.push(rect(left, top, m.cell_width as f32, m.cell_height as f32, color));
}

fn decorations(rects: &mut Vec<RectInstance>, cell: &Cell, left: f32, top: f32, m: Metrics) {
    let width = m.cell_width as f32 * if cell.style.wide { 2.0 } else { 1.0 };
    let stroke = m.stroke as f32;
    if cell.style.underline {
        rects.push(rect(left, top + m.underline_offset as f32, width, stroke, cell.fg));
    }
    if cell.style.strikethrough {
        // Roughly half the x-height above the baseline.
        let y = top + m.baseline as f32 * 0.65;
        rects.push(rect(left, y, width, stroke, cell.fg));
    }
}

impl RectInstance {
    pub fn new(pos: [f32; 2], size: [f32; 2], color: [u8; 4]) -> Self {
        Self { pos, size, color, pad: 0 }
    }
}

fn rect(x: f32, y: f32, w: f32, h: f32, color: Rgb) -> RectInstance {
    RectInstance { pos: [x, y], size: [w, h], color: rgba(color), pad: 0 }
}

fn rgba(c: Rgb) -> [u8; 4] {
    [c.r, c.g, c.b, 255]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::terminal::{Geometry, Terminal};

    fn setup() -> (Terminal, Fonts, Atlas, Scene) {
        let geometry = Geometry { cols: 10, rows: 3, cell_width: 19, cell_height: 42 };
        let terminal = Terminal::new(geometry, &Config::default()).expect("terminal");
        (
            terminal,
            Fonts::new(&crate::config::Font::default(), 32.0).expect("fonts"),
            Atlas::default(),
            Scene::default(),
        )
    }

    const FOCUSED: Overlay<'static> = Overlay { focused: true, copy_cursor: None, preedit: "" };

    #[test]
    fn builds_one_glyph_per_inked_cell_and_merges_backgrounds() {
        let (mut t, fonts, mut atlas, mut scene) = setup();
        t.write(b"a b\x1b[41m  \x1b[0m");
        scene.build(&mut t.frame(), &FOCUSED, &fonts, &mut atlas).expect("room");
        assert_eq!(scene.glyphs.len(), 2);
        assert_eq!(scene.rects.len(), 2, "the pane's background and one merged red run");
        assert_eq!(scene.rects[1].size[0], 2.0 * fonts.metrics().cell_width as f32);
        assert_eq!(scene.overlay_rects.len(), 1, "bar cursor");
    }

    #[test]
    fn blinking_cells_have_batches_of_their_own() {
        let (mut t, fonts, mut atlas, mut scene) = setup();
        t.write(b"ab\x1b[5;4mcd\x1b[0me");
        scene.build(&mut t.frame(), &FOCUSED, &fonts, &mut atlas).expect("room");
        assert_eq!((scene.glyphs.len(), scene.blink_glyphs.len()), (3, 2));
        assert_eq!(scene.blink_rects.len(), 2, "the underlines blink too");
        assert_eq!(scene.rects.len(), 1, "the pane's background");
        let left = |g: &GlyphInstance| g.pos[0] / fonts.metrics().cell_width as f32;
        assert!((2.0..4.0).contains(&left(&scene.blink_glyphs[0])), "c sits in its own cell");
    }

    #[test]
    fn rebuilds_only_dirty_rows() {
        let (mut t, fonts, mut atlas, mut scene) = setup();
        t.write(b"one\r\ntwo");
        scene.build(&mut t.frame(), &FOCUSED, &fonts, &mut atlas).expect("room");
        let glyphs = scene.glyphs.clone();
        // Nothing changed: the same instances come back.
        scene.build(&mut t.frame(), &FOCUSED, &fonts, &mut atlas).expect("room");
        assert_eq!(scene.glyphs, glyphs);
        t.write(b"!");
        scene.build(&mut t.frame(), &FOCUSED, &fonts, &mut atlas).expect("room");
        assert_eq!(scene.glyphs.len(), glyphs.len() + 1);
        assert_eq!(scene.glyphs[..3], glyphs[..3]);
    }

    #[test]
    fn ligatures_break_at_the_cursor_and_join_again_when_it_leaves() {
        let (mut t, fonts, mut atlas, mut scene) = setup();
        t.write(b"a->b");
        scene.build(&mut t.frame(), &FOCUSED, &fonts, &mut atlas).expect("room");
        let joined = scene.glyphs.clone();
        let on_arrow = Overlay { copy_cursor: Some((2, 0)), ..FOCUSED };
        scene.build(&mut t.frame(), &on_arrow, &fonts, &mut atlas).expect("room");
        assert_ne!(scene.glyphs, joined, "the cursor's row is shaped again around it");
        scene.build(&mut t.frame(), &FOCUSED, &fonts, &mut atlas).expect("room");
        assert_eq!(scene.glyphs, joined);
    }

    #[test]
    fn preedit_replaces_the_cursor() {
        let (mut t, fonts, mut atlas, mut scene) = setup();
        let overlay = Overlay { focused: true, copy_cursor: None, preedit: "日本" };
        scene.build(&mut t.frame(), &overlay, &fonts, &mut atlas).expect("room");
        assert_eq!(scene.overlay_glyphs.len(), 2);
        // A background and an underline, four cells wide.
        assert_eq!(scene.overlay_rects.len(), 2);
        assert_eq!(scene.overlay_rects[1].size[0], 4.0 * fonts.metrics().cell_width as f32);
    }

    #[test]
    fn chrome_lines_sit_at_their_own_position() {
        let (_, fonts, mut atlas, _) = setup();
        let white = Rgb { r: 255, g: 255, b: 255 };
        let mut line = Line::new(5.0, 7.0, 300.0, Rgb { r: 200, g: 0, b: 0 });
        line.push_str("a 日", white, CellStyle::default());
        assert_eq!(line.cells.len(), 4, "the wide character takes two cells");
        let mut chrome = Chrome::default();
        chrome.build(&[line], &fonts, &mut atlas).expect("room");
        assert_eq!(chrome.rects.len(), 1);
        assert_eq!(chrome.rects[0].pos, [5.0, 7.0]);
        assert_eq!(chrome.glyphs.len(), 2);
        assert!(chrome.glyphs[1].pos[0] > 5.0 + 2.0 * fonts.metrics().cell_width as f32 - 4.0);
    }
}
