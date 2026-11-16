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

    let (x0, y0, x1, y1) = crop(&data, width, height)?;
    let (w, h) = (x1 - x0, y1 - y0);
    let mut out = Vec::with_capacity(w * h * 4);
    let mut colored = false;
    for y in y0..y1 {
        let row = &data[(y * width + x0) * 4..(y * width + x1) * 4];
        // Text drawn in white has r = g = b = a; anything else is color.
        colored |= row.as_chunks::<4>().0.iter().any(|p| p[0] != p[3] || p[1] != p[3] || p[2] != p[3]);
        out.extend_from_slice(row);
    }
    Some(Bitmap { width: w as u32, height: h as u32, left: x0 as i32 - origin, top: y0 as i32, colored, data: out })
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
