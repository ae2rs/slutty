//! Where each glyph lives in the atlas texture. Pure bookkeeping: bitmaps
//! wait in `pending` until the GPU side uploads them.

use std::collections::HashMap;

use crate::font::{Bitmap, Face, Fonts};
use crate::terminal::Grapheme;

pub const ATLAS_SIZE: u32 = 2048;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    text: Grapheme,
    face: Face,
    cells: u8,
}

/// A glyph's place in the atlas and its offset from the cell corner.
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

#[derive(Default)]
pub struct Atlas {
    glyphs: HashMap<Key, Option<Glyph>>,
    shelf_x: u32,
    shelf_y: u32,
    shelf_height: u32,
    pending: Vec<Upload>,
}

impl Atlas {
    /// The glyph for `text`, rasterised on first use. `None` if it has no ink.
    pub fn glyph(&mut self, fonts: &Fonts, text: &Grapheme, face: Face, cells: u8) -> Result<Option<Glyph>, Full> {
        let key = Key { text: text.clone(), face, cells };
        if let Some(glyph) = self.glyphs.get(&key) {
            return Ok(*glyph);
        }
        let mut buf = [0; 4];
        let string = match text {
            Grapheme::Empty => return Ok(None),
            Grapheme::Char(c) => &*c.encode_utf8(&mut buf),
            Grapheme::Cluster(s) => s,
        };
        let glyph = match fonts.rasterize(string, face, u32::from(cells)) {
            Some(bitmap) => Some(self.place(bitmap)?),
            None => None,
        };
        self.glyphs.insert(key, glyph);
        Ok(glyph)
    }

    pub fn take_pending(&mut self) -> Vec<Upload> {
        std::mem::take(&mut self.pending)
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Shelf packing: glyphs fill a row left to right, rows stack down.
    fn place(&mut self, bitmap: Bitmap) -> Result<Glyph, Full> {
        // One pixel of gap so nearest sampling never reads a neighbour.
        let (w, h) = (bitmap.width + 1, bitmap.height + 1);
        if w > ATLAS_SIZE || h > ATLAS_SIZE {
            return Err(Full);
        }
        if self.shelf_x + w > ATLAS_SIZE {
            self.shelf_y += self.shelf_height;
            self.shelf_x = 0;
            self.shelf_height = 0;
        }
        if self.shelf_y + h > ATLAS_SIZE {
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
