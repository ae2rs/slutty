//! Drawing one glyph into a bitmap with CoreGraphics.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{CGBitmapContextCreate, CGColorSpace, CGContext, CGImageAlphaInfo};
use objc2_core_text::{CTFont, CTFontOrientation};

use super::fonts::{Bitmap, Metrics};

/// Room around the glyph's bounds for antialiasing.
const MARGIN: i32 = 2;

/// Draws `glyph` of `font` in white, cropped to its ink. The canvas is one
/// cell tall, so ink above or below the cell is cut. `None` for a glyph with
/// no ink, such as a space.
pub(super) fn draw(font: &CTFont, glyph: u16, m: Metrics, color_space: &CGColorSpace) -> Option<Bitmap> {
    let mut glyph = glyph;
    // SAFETY: one glyph in; a null rects pointer is allowed.
    let bounds = unsafe {
        font.bounding_rects_for_glyphs(
            CTFontOrientation::Horizontal,
            NonNull::from(&mut glyph),
            std::ptr::null_mut(),
            1,
        )
    };
    if bounds.size.width <= 0.0 || bounds.size.height <= 0.0 {
        return None;
    }
    // The pen sits `origin` pixels from the canvas's left edge.
    let origin = MARGIN - bounds.origin.x.floor() as i32;
    let max_width = 8 * m.cell_width as i32;
    let width = (bounds.size.width.ceil() as i32 + 2 * MARGIN + 1).min(max_width) as usize;
    let height = m.cell_height as usize;
    let mut data = vec![0u8; width * height * 4];

    // SAFETY: `data` outlives the context and has `width * 4` bytes per row.
    let context = unsafe {
        CGBitmapContextCreate(
            data.as_mut_ptr().cast::<c_void>(),
            width,
            height,
            8,
            width * 4,
            Some(color_space),
            CGImageAlphaInfo::PremultipliedLast.0,
        )
    }?;
    CGContext::set_should_antialias(Some(&context), true);
    CGContext::set_should_smooth_fonts(Some(&context), false);
    CGContext::set_rgb_fill_color(Some(&context), 1.0, 1.0, 1.0, 1.0);
    // CoreGraphics counts y from the bottom.
    let mut position = CGPoint::new(f64::from(origin), f64::from(m.cell_height - m.baseline));
    // SAFETY: one glyph and one position, a valid context.
    unsafe { font.draw_glyphs(NonNull::from(&mut glyph), NonNull::from(&mut position), 1, &context) };
    drop(context);

    let mut bitmap = cropped(&data, width, height)?;
    bitmap.left -= origin;
    // Text drawn in white has r = g = b = a; anything else is color.
    bitmap.colored = bitmap.data.as_chunks::<4>().0.iter().any(|p| p[0] != p[3] || p[1] != p[3] || p[2] != p[3]);
    Some(bitmap)
}

/// Draws braille pattern `dots` (bit n is dot n + 1) in white as round dots
/// spread over the whole cell, as Ghostty and WezTerm do: fallback fonts draw
/// them a fraction of the size. `None` for the blank pattern.
pub(super) fn braille(dots: u8, m: Metrics) -> Option<Bitmap> {
    // Each dot's column and row: 1 to 3 down the left, 4 to 6 down the right,
    // then 7 and 8 below.
    const PLACES: [(u32, u32); 8] = [(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2), (0, 3), (1, 3)];
    // Samples per pixel along each axis, for antialiased edges.
    const SUB: usize = 4;
    let (width, height) = (m.cell_width as usize, m.cell_height as usize);
    let (cw, ch) = (m.cell_width as f32, m.cell_height as f32);
    // Two columns and four rows share the cell; a dot is about half its slot.
    let radius = (cw / 2.0).min(ch / 4.0) * 0.28;
    let centers: Vec<(f32, f32)> = PLACES
        .iter()
        .enumerate()
        .filter(|&(i, _)| dots & (1 << i) != 0)
        // On pixel centers, so every dot antialiases alike.
        .map(|(_, &(col, row))| {
            ((cw * (2 * col + 1) as f32 / 4.0).floor() + 0.5, (ch * (2 * row + 1) as f32 / 8.0).floor() + 0.5)
        })
        .collect();
    let mut data = vec![0u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let mut hits = 0;
            for sy in 0..SUB {
                for sx in 0..SUB {
                    let px = x as f32 + (sx as f32 + 0.5) / SUB as f32;
                    let py = y as f32 + (sy as f32 + 0.5) / SUB as f32;
                    hits += usize::from(
                        centers.iter().any(|&(cx, cy)| (px - cx).powi(2) + (py - cy).powi(2) <= radius * radius),
                    );
                }
            }
            data[(y * width + x) * 4..][..4].fill((hits * 255 / (SUB * SUB)) as u8);
        }
    }
    cropped(&data, width, height)
}

/// The ink of a `width` by `height` RGBA canvas, placed from its top-left.
fn cropped(data: &[u8], width: usize, height: usize) -> Option<Bitmap> {
    let (x0, y0, x1, y1) = crop(data, width, height)?;
    let mut out = Vec::with_capacity((x1 - x0) * (y1 - y0) * 4);
    for y in y0..y1 {
        out.extend_from_slice(&data[(y * width + x0) * 4..(y * width + x1) * 4]);
    }
    Some(Bitmap {
        width: (x1 - x0) as u32,
        height: (y1 - y0) as u32,
        left: x0 as i32,
        top: y0 as i32,
        colored: false,
        data: out,
    })
}

/// The smallest box holding every non-transparent pixel, as x0, y0, x1, y1.
fn crop(data: &[u8], width: usize, height: usize) -> Option<(usize, usize, usize, usize)> {
    let mut bounds: Option<(usize, usize, usize, usize)> = None;
    for y in 0..height {
        for x in 0..width {
            if data[(y * width + x) * 4 + 3] != 0 {
                let b = bounds.get_or_insert((x, y, x + 1, y + 1));
                b.0 = b.0.min(x);
                b.1 = b.1.min(y);
                b.2 = b.2.max(x + 1);
                b.3 = b.3.max(y + 1);
            }
        }
    }
    bounds
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> Metrics {
        Metrics { cell_width: 19, cell_height: 42, baseline: 33, underline_offset: 36, stroke: 2 }
    }

    #[test]
    fn braille_dots_spread_over_the_cell() {
        let full = braille(0xff, metrics()).expect("ink");
        assert!(full.width >= 12 && full.height >= 34, "{}x{}", full.width, full.height);
        assert!(full.data.chunks(4).all(|p| p[0] == p[3]), "white, premultiplied");
        let first = braille(0x01, metrics()).expect("ink");
        assert!(first.left + first.width as i32 <= 10 && first.top + first.height as i32 <= 11, "dot 1 is top left");
        let eighth = braille(0x80, metrics()).expect("ink");
        assert!(eighth.left >= 9 && eighth.top >= 31, "dot 8 is bottom right");
        assert!(braille(0, metrics()).is_none());
    }
}
