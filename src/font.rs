//! Font faces, cell metrics and glyph rasterisation with CoreText.
//!
//! A grapheme is drawn as a whole with a CTLine, so CoreText handles
//! combining marks, emoji sequences and fallback to other fonts.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_core_foundation::{
    CFAttributedString, CFData, CFDictionary, CFRetained, CFString, CFType, CGFloat, kCFBooleanTrue,
};
use objc2_core_graphics::{CGBitmapContextCreate, CGColorSpace, CGContext, CGImageAlphaInfo, kCGColorSpaceSRGB};
use objc2_core_text::{
    CTFont, CTFontManagerCreateFontDescriptorFromData, CTFontOrientation, CTLine, kCTFontAttributeName,
    kCTForegroundColorFromContextAttributeName,
};

/// Embedded JetBrains Mono, bold as SemiBold and faint as ExtraLight, like
/// the WezTerm config.
const FACES: [&[u8]; 6] = [
    include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-Regular.ttf"),
    include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-Italic.ttf"),
    include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-SemiBold.ttf"),
    include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-SemiBoldItalic.ttf"),
    include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-ExtraLight.ttf"),
    include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-ExtraLightItalic.ttf"),
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Weight {
    #[default]
    Regular,
    Bold,
    Faint,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Face {
    pub weight: Weight,
    pub italic: bool,
}

/// Cell geometry in device pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metrics {
    pub cell_width: u32,
    pub cell_height: u32,
    /// Distance from the top of the cell to the baseline.
    pub baseline: u32,
    pub underline_offset: u32,
    pub stroke: u32,
}

/// A rasterised glyph: premultiplied RGBA, cropped to its ink.
#[derive(Debug)]
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    /// Offset of the bitmap from the cell's top-left corner.
    pub left: i32,
    pub top: i32,
    /// Keeps its own colors (emoji) instead of taking the text color.
    pub colored: bool,
    pub data: Vec<u8>,
}

#[derive(Debug)]
pub struct FontError(&'static str);

impl std::fmt::Display for FontError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for FontError {}

pub struct Fonts {
    faces: Vec<CFRetained<CTFont>>,
    metrics: Metrics,
    color_space: CFRetained<CGColorSpace>,
}

impl Fonts {
    /// Loads the faces at `size` device pixels.
    pub fn new(size: f64) -> Result<Self, FontError> {
        let faces = FACES
            .iter()
            .map(|bytes| {
                let data = CFData::from_bytes(bytes);
                // SAFETY: `data` is a valid CFData.
                let descriptor = unsafe { CTFontManagerCreateFontDescriptorFromData(&data) }
                    .ok_or(FontError("embedded font is invalid"))?;
                // SAFETY: valid descriptor; a null matrix means identity.
                Ok(unsafe { CTFont::with_font_descriptor(&descriptor, size, std::ptr::null()) })
            })
            .collect::<Result<Vec<_>, FontError>>()?;
        let color_space =
            CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).ok_or(FontError("no sRGB color space"))?;
        let metrics = metrics(&faces[0])?;
        Ok(Self { faces, metrics, color_space })
    }

    pub fn metrics(&self) -> Metrics {
        self.metrics
    }

    /// Draws `text` as it appears in a cell `cells` wide. `None` for text
    /// with no ink, such as a space.
    pub fn rasterize(&self, text: &str, face: Face, cells: u32) -> Option<Bitmap> {
        let m = self.metrics;
        // Room on both sides for glyphs that overhang their cell (italics).
        let margin = m.cell_width;
        let width = (m.cell_width * cells + 2 * margin) as usize;
        let height = m.cell_height as usize;
        let mut data = vec![0u8; width * height * 4];

        let font = &self.faces[face_index(face)];
        let line = line(text, font)?;
        // SAFETY: `data` outlives the context and has `width * 4` bytes per row.
        let context = unsafe {
            CGBitmapContextCreate(
                data.as_mut_ptr().cast::<c_void>(),
                width,
                height,
                8,
                width * 4,
                Some(&self.color_space),
                CGImageAlphaInfo::PremultipliedLast.0,
            )
        }?;
        CGContext::set_should_antialias(Some(&context), true);
        CGContext::set_should_smooth_fonts(Some(&context), false);
        CGContext::set_rgb_fill_color(Some(&context), 1.0, 1.0, 1.0, 1.0);
        // CoreGraphics counts y from the bottom.
        CGContext::set_text_position(Some(&context), CGFloat::from(margin), CGFloat::from(m.cell_height - m.baseline));
        // SAFETY: valid line and context.
        unsafe { line.draw(&context) };
        drop(context);

        crop(&data, width, height).map(|(x0, y0, x1, y1)| {
            let (w, h) = (x1 - x0, y1 - y0);
            let mut out = Vec::with_capacity(w * h * 4);
            let mut colored = false;
            for y in y0..y1 {
                let row = &data[(y * width + x0) * 4..(y * width + x1) * 4];
                // Text drawn in white has r = g = b = a; anything else is color.
                colored |= row.as_chunks::<4>().0.iter().any(|p| p[0] != p[3] || p[1] != p[3] || p[2] != p[3]);
                out.extend_from_slice(row);
            }
            Bitmap {
                width: w as u32,
                height: h as u32,
                left: x0 as i32 - margin as i32,
                top: y0 as i32,
                colored,
                data: out,
            }
        })
    }
}

fn face_index(face: Face) -> usize {
    let weight = match face.weight {
        Weight::Regular => 0,
        Weight::Bold => 2,
        Weight::Faint => 4,
    };
    weight + usize::from(face.italic)
}

fn metrics(font: &CTFont) -> Result<Metrics, FontError> {
    let mut chars = [u16::from(b'M')];
    let mut glyphs = [0u16];
    // SAFETY: both buffers hold one element.
    let found = unsafe { font.glyphs_for_characters(NonNull::from(&mut chars[0]), NonNull::from(&mut glyphs[0]), 1) };
    if !found {
        return Err(FontError("font has no 'M'"));
    }
    // SAFETY: one glyph in; a null advances pointer is allowed.
    let advance = unsafe {
        font.advances_for_glyphs(CTFontOrientation::Horizontal, NonNull::from(&mut glyphs[0]), std::ptr::null_mut(), 1)
    };
    // SAFETY: plain metric getters.
    let (ascent, descent, leading, underline, thickness) = unsafe {
        (font.ascent(), font.descent(), font.leading(), font.underline_position(), font.underline_thickness())
    };
    let px = |v: f64| v.round().max(1.0) as u32;
    let baseline = px(ascent);
    let cell_height = px(ascent + descent + leading);
    Ok(Metrics {
        cell_width: px(advance),
        cell_height,
        baseline,
        underline_offset: (baseline + px(-underline)).min(cell_height - 1),
        stroke: px(thickness),
    })
}

fn line(text: &str, font: &CTFont) -> Option<CFRetained<CTLine>> {
    let string = CFString::from_str(text);
    // SAFETY: reading framework constants.
    let (font_key, from_context_key, yes) =
        unsafe { (kCTFontAttributeName, kCTForegroundColorFromContextAttributeName, kCFBooleanTrue?) };
    let values: [&CFType; 2] = [font.as_ref(), yes.as_ref()];
    let attributes = CFDictionary::<CFString, CFType>::from_slices(&[font_key, from_context_key], &values);
    // SAFETY: valid string and dictionary.
    let attributed = unsafe { CFAttributedString::new(None, Some(&string), Some(attributes.as_opaque())) }?;
    // SAFETY: valid attributed string.
    Some(unsafe { CTLine::with_attributed_string(&attributed) })
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

    #[test]
    fn metrics_are_monospace_cells() {
        let fonts = Fonts::new(32.0).expect("fonts");
        let m = fonts.metrics();
        assert!((15..=25).contains(&m.cell_width), "{m:?}");
        assert!(m.cell_height > m.cell_width && m.baseline < m.cell_height, "{m:?}");
    }

    #[test]
    fn text_is_white_and_emoji_is_colored() {
        let fonts = Fonts::new(32.0).expect("fonts");
        let a = fonts.rasterize("a", Face::default(), 1).expect("ink");
        assert!(!a.colored && a.width > 0 && a.height > 0);
        assert!(fonts.rasterize(" ", Face::default(), 1).is_none());
        let emoji = fonts.rasterize("😀", Face::default(), 2).expect("emoji falls back to another font");
        assert!(emoji.colored);
    }

    #[test]
    fn faces_differ() {
        let fonts = Fonts::new(32.0).expect("fonts");
        let ink = |face| fonts.rasterize("m", face, 1).expect("ink").data.iter().map(|&b| u64::from(b)).sum::<u64>();
        let regular = ink(Face::default());
        assert!(ink(Face { weight: Weight::Bold, italic: false }) > regular);
        assert!(ink(Face { weight: Weight::Faint, italic: false }) < regular);
    }
}
