//! Turns a frame into GPU instances. Each row's instances are kept, so only
//! rows libghostty marked dirty are rebuilt.

use libghostty_vt::color::Rgb;
use libghostty_vt::unicode;

use super::atlas::{Atlas, Full, Glyph};
use crate::font::{Face, Fonts, Metrics, Weight};
use crate::terminal::{Cell, CellStyle, CursorShape, Frame, Grapheme};

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
        for c in text.chars() {
            let wide = unicode::codepoint_width(c) > 1;
            self.cells.push(Cell { text: Grapheme::Char(c), fg, bg: None, style: CellStyle { wide, ..style } });
            if wide {
                self.cells.push(Cell { text: Grapheme::Empty, fg, bg: None, style });
            }
        }
    }
}

#[derive(Default)]
struct RowCache {
    rects: Vec<RectInstance>,
    glyphs: Vec<GlyphInstance>,
}

/// One pane's instances in draw order: `rects`, `glyphs`, then the overlay's
/// rects and glyphs (cursor, preedit) on top, relative to the pane.
#[derive(Default)]
pub struct Scene {
    rows: Vec<RowCache>,
    rebuild_all: bool,
    pub rects: Vec<RectInstance>,
    pub glyphs: Vec<GlyphInstance>,
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
        self.build_rows(frame, fonts, atlas)?;
        self.rebuild_all = false;

        self.rects.clear();
        self.glyphs.clear();
        for row in &self.rows {
            self.rects.extend_from_slice(&row.rects);
            self.glyphs.extend_from_slice(&row.glyphs);
        }
        self.overlay_rects.clear();
        self.overlay_glyphs.clear();
        self.build_overlay(frame, overlay, fonts, atlas);
        frame.changed = false;
        Ok(())
    }

    fn build_rows(&mut self, frame: &mut Frame, fonts: &Fonts, atlas: &mut Atlas) -> Result<(), Full> {
        let m = fonts.metrics();
        for (y, (row, cache)) in frame.rows.iter_mut().zip(&mut self.rows).enumerate() {
            if !row.dirty && !self.rebuild_all {
                continue;
            }
            cache.rects.clear();
            cache.glyphs.clear();
            let top = y as f32 * m.cell_height as f32;
            for (x, cell) in row.cells.iter().enumerate() {
                let left = x as f32 * m.cell_width as f32;
                if let Some(bg) = cell.bg.filter(|bg| *bg != frame.background) {
                    push_bg(&mut cache.rects, left, top, m, bg);
                }
                if let Some(glyph) = glyph(atlas, fonts, cell)? {
                    cache.glyphs.push(glyph_instance(glyph, left, top, cell.fg));
                }
                decorations(&mut cache.rects, cell, left, top, m);
            }
            row.dirty = false;
        }
        Ok(())
    }

    fn build_overlay(&mut self, frame: &Frame, overlay: &Overlay<'_>, fonts: &Fonts, atlas: &mut Atlas) {
        let m = fonts.metrics();
        let Some(cursor) = frame.cursor else { return };
        let x = if cursor.wide { cursor.x.saturating_sub(1) } else { cursor.x };
        let (left, top) = (f32::from(x) * m.cell_width as f32, f32::from(cursor.y) * m.cell_height as f32);
        let (cw, ch, stroke) = (m.cell_width as f32, m.cell_height as f32, m.stroke as f32);

        if !overlay.preedit.is_empty() {
            let mut pen = left;
            for c in overlay.preedit.chars() {
                let cells = unicode::codepoint_width(c).max(1);
                let width = cw * f32::from(cells);
                self.overlay_rects.push(rect(pen, top, width, ch, frame.background));
                let text = Grapheme::Char(c);
                if let Ok(Some(g)) = atlas.glyph(fonts, &text, Face::default(), cells) {
                    self.overlay_glyphs.push(glyph_instance(g, pen, top, frame.foreground));
                }
                pen += width;
            }
            let underline = top + m.underline_offset as f32;
            self.overlay_rects.push(rect(left, underline, pen - left, stroke, frame.foreground));
            return;
        }

        let under = frame.rows.get(usize::from(cursor.y)).and_then(|row| row.cells.get(usize::from(x)));
        let width = cw * if under.is_some_and(|c| c.style.wide) { 2.0 } else { 1.0 };
        let color = cursor.color.unwrap_or(frame.foreground);
        let shape = if overlay.focused { cursor.shape } else { CursorShape::BlockHollow };
        match shape {
            CursorShape::Block => {
                self.overlay_rects.push(rect(left, top, width, ch, color));
                // The text under a block cursor takes the background color.
                if let Some(cell) = under
                    && let Ok(Some(g)) = glyph(atlas, fonts, cell)
                {
                    self.overlay_glyphs.push(glyph_instance(g, left, top, frame.background));
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
    }
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
                if let Some(glyph) = glyph(atlas, fonts, cell)? {
                    self.glyphs.push(glyph_instance(glyph, left, line.y, cell.fg));
                }
                decorations(&mut self.rects, cell, left, line.y, m);
            }
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

fn glyph(atlas: &mut Atlas, fonts: &Fonts, cell: &Cell) -> Result<Option<Glyph>, Full> {
    atlas.glyph(fonts, &cell.text, face(cell.style), if cell.style.wide { 2 } else { 1 })
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

    const FOCUSED: Overlay<'static> = Overlay { focused: true, preedit: "" };

    #[test]
    fn builds_one_glyph_per_inked_cell_and_merges_backgrounds() {
        let (mut t, fonts, mut atlas, mut scene) = setup();
        t.write(b"a b\x1b[41m  \x1b[0m");
        scene.build(&mut t.frame(), &FOCUSED, &fonts, &mut atlas).expect("room");
        assert_eq!(scene.glyphs.len(), 2);
        assert_eq!(scene.rects.len(), 1);
        assert_eq!(scene.rects[0].size[0], 2.0 * fonts.metrics().cell_width as f32);
        assert_eq!(scene.overlay_rects.len(), 1, "bar cursor");
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
    fn preedit_replaces_the_cursor() {
        let (mut t, fonts, mut atlas, mut scene) = setup();
        let overlay = Overlay { focused: true, preedit: "日本" };
        scene.build(&mut t.frame(), &overlay, &fonts, &mut atlas).expect("room");
        assert_eq!(scene.overlay_glyphs.len(), 2);
        // Two wide backgrounds and an underline.
        assert_eq!(scene.overlay_rects.len(), 3);
        assert_eq!(scene.overlay_rects[2].size[0], 4.0 * fonts.metrics().cell_width as f32);
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
