//! Where each glyph lives in the atlas texture. Pure bookkeeping: bitmaps
//! wait in `pending` until the GPU side uploads them.

use std::collections::HashMap;

use crate::font::{Bitmap, FontId, Fonts};

/// The atlas starts this wide and high, and doubles while one screen does not
/// fit, up to [`MAX_SIZE`].
pub const INITIAL_SIZE: u32 = 2048;
pub const MAX_SIZE: u32 = 8192;

/// A glyph's place in the atlas and its offset from the pen, at the cell's
/// top.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glyph {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub left: i32,
    pub top: i32,
    pub colored: bool,
}

/// The atlas has no room left. Reset it and rebuild every row.
#[derive(Debug)]
pub struct Full;

pub struct Upload {
    pub glyph: Glyph,
    pub data: Vec<u8>,
}

pub struct Atlas {
    size: u32,
    glyphs: HashMap<(FontId, u16), Option<Glyph>>,
    shelf_x: u32,
    shelf_y: u32,
    shelf_height: u32,
    pending: Vec<Upload>,
    /// Counts resets, so instances made before one can be told stale.
    generation: u64,
}

impl Default for Atlas {
    fn default() -> Self {
        Self::new(INITIAL_SIZE, 0)
    }
}

impl Atlas {
    /// Glyph `glyph` of `font`, rasterised on first use. `None` if it has no
    /// ink.
    pub fn glyph(&mut self, fonts: &Fonts, font: FontId, glyph: u16) -> Result<Option<Glyph>, Full> {
        let key = (font, glyph);
        if let Some(glyph) = self.glyphs.get(&key) {
            return Ok(*glyph);
        }
        let glyph = match fonts.rasterize(font, glyph) {
            Some(bitmap) => Some(self.place(bitmap)?),
            None => None,
        };
        self.glyphs.insert(key, glyph);
        Ok(glyph)
    }

    pub fn take_pending(&mut self) -> Vec<Upload> {
        std::mem::take(&mut self.pending)
    }

    /// Empties the atlas, keeping its size.
    pub fn reset(&mut self) {
        *self = Self::new(self.size, self.generation + 1);
    }

    /// Empties the atlas and makes it `size` wide and high.
    pub fn reset_to(&mut self, size: u32) {
        *self = Self::new(size, self.generation + 1);
    }

    pub fn size(&self) -> u32 {
        self.size
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn new(size: u32, generation: u64) -> Self {
        Self { size, glyphs: HashMap::new(), shelf_x: 0, shelf_y: 0, shelf_height: 0, pending: Vec::new(), generation }
    }

    /// Shelf packing: glyphs fill a row left to right, rows stack down.
    fn place(&mut self, bitmap: Bitmap) -> Result<Glyph, Full> {
        // One pixel of gap so nearest sampling never reads a neighbour.
        let (w, h) = (bitmap.width + 1, bitmap.height + 1);
        if w > self.size || h > self.size {
            return Err(Full);
        }
        if self.shelf_x + w > self.size {
            self.shelf_y += self.shelf_height;
            self.shelf_x = 0;
            self.shelf_height = 0;
        }
        if self.shelf_y + h > self.size {
            return Err(Full);
        }
        let glyph = Glyph {
            x: self.shelf_x,
            y: self.shelf_y,
            width: bitmap.width,
            height: bitmap.height,
            left: bitmap.left,
            top: bitmap.top,
            colored: bitmap.colored,
        };
        self.shelf_x += w;
        self.shelf_height = self.shelf_height.max(h);
        self.pending.push(Upload { glyph, data: bitmap.data });
        Ok(glyph)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Places `count` square glyphs `side` pixels wide; how many fit.
    fn fill(atlas: &mut Atlas, side: u32, count: usize) -> usize {
        let bitmap = || Bitmap { width: side, height: side, left: 0, top: 0, colored: false, data: Vec::new() };
        (0..count).take_while(|_| atlas.place(bitmap()).is_ok()).count()
    }

    #[test]
    fn a_bigger_atlas_holds_what_a_smaller_one_could_not() {
        let mut atlas = Atlas::default();
        // 63 + 1 pixels of gap: 32 a row and 32 rows at 2048.
        assert_eq!(fill(&mut atlas, 63, 2000), 32 * 32);
        atlas.reset_to(INITIAL_SIZE * 2);
        assert_eq!(atlas.size(), INITIAL_SIZE * 2);
        assert_eq!(fill(&mut atlas, 63, 2000), 2000);
    }

    #[test]
    fn every_reset_makes_older_instances_stale() {
        let mut atlas = Atlas::default();
        fill(&mut atlas, 10, 3);
        atlas.reset();
        assert_eq!((atlas.size(), atlas.generation()), (INITIAL_SIZE, 1));
        assert!(atlas.take_pending().is_empty());
        atlas.reset_to(MAX_SIZE);
        assert_eq!((atlas.size(), atlas.generation()), (MAX_SIZE, 2));
    }
}
