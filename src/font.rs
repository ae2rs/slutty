//! Font faces, cell metrics and glyph rasterisation with CoreText.
//!
//! A grapheme is drawn as a whole with a CTLine, so CoreText handles
//! combining marks, emoji sequences and fallback to other fonts.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_core_foundation::{
    CFArray, CFAttributedString, CFData, CFDictionary, CFNumber, CFRetained, CFString, CFType, CGFloat, kCFBooleanTrue,
};
use objc2_core_graphics::{CGBitmapContextCreate, CGColorSpace, CGContext, CGImageAlphaInfo, kCGColorSpaceSRGB};
use objc2_core_text::{
    CTFont, CTFontDescriptor, CTFontManagerCreateFontDescriptorFromData, CTFontOrientation, CTFontSymbolicTraits,
    CTLine, kCTFontAttributeName, kCTFontFamilyNameAttribute, kCTFontWeightTrait,
    kCTForegroundColorFromContextAttributeName,
};

use crate::config::{self, FontWeight};

/// The family slutty embeds, so it looks the same on every Mac.
const EMBEDDED_FAMILY: &str = "JetBrains Mono";

const EMBEDDED: [&[u8]; 6] = [
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
pub struct FontError(String);

impl std::fmt::Display for FontError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for FontError {}

pub struct Fonts {
    /// Indexed by [`face_index`].
    faces: Vec<CFRetained<CTFont>>,
    metrics: Metrics,
    color_space: CFRetained<CGColorSpace>,
}

impl Fonts {
    /// Loads the configured family at `size` device pixels, with the faces
    /// nearest to each configured weight.
    pub fn new(config: &config::Font, size: f64) -> Result<Self, FontError> {
        let candidates = if config.family.eq_ignore_ascii_case(EMBEDDED_FAMILY) {
            embedded(size)?
        } else {
            installed(&config.family, size)
        };
        if candidates.is_empty() {
            return Err(FontError(format!("font family `{}` is not installed", config.family)));
        }
        let pick = |weight: FontWeight, italic: bool| {
            let want = css_weight(weight);
            let cost = |c: &Candidate| (c.weight - want).abs() + if c.italic == italic { 0.0 } else { 1000.0 };
            let best = candidates.iter().min_by(|a, b| cost(a).total_cmp(&cost(b))).expect("not empty");
            best.font.clone()
        };
        let faces = vec![
            pick(FontWeight::Regular, false),
            pick(FontWeight::Regular, true),
            pick(config.bold_weight, false),
            pick(config.bold_weight, true),
            pick(config.faint_weight, false),
            pick(config.faint_weight, true),
        ];
        let color_space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))
            .ok_or_else(|| FontError("no sRGB color space".into()))?;
        let metrics = metrics(&faces[0], config.line_height)?;
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

/// A face of the configured family.
struct Candidate {
    font: CFRetained<CTFont>,
    /// As CSS: 400 regular, 700 bold.
    weight: f64,
    italic: bool,
}

/// The embedded faces.
fn embedded(size: f64) -> Result<Vec<Candidate>, FontError> {
    EMBEDDED
        .iter()
        .map(|bytes| {
            let data = CFData::from_bytes(bytes);
            // SAFETY: `data` is a valid CFData.
            let descriptor = unsafe { CTFontManagerCreateFontDescriptorFromData(&data) }
                .ok_or_else(|| FontError("embedded font is invalid".into()))?;
            Ok(with_style(&descriptor, size))
        })
        .collect()
}

/// Every installed face of `family`.
fn installed(family: &str, size: f64) -> Vec<Candidate> {
    let name = CFString::from_str(family);
    // SAFETY: reading a framework constant.
    let key = unsafe { kCTFontFamilyNameAttribute };
    let attributes = CFDictionary::<CFString, CFString>::from_slices(&[key], &[&name]);
    // SAFETY: a valid attribute dictionary.
    let query = unsafe { CTFontDescriptor::with_attributes(attributes.as_opaque()) };
    // SAFETY: no mandatory attributes; the array holds font descriptors.
    let Some(found) = (unsafe { query.matching_font_descriptors(None) }) else { return Vec::new() };
    let found = unsafe { CFRetained::cast_unchecked::<CFArray<CTFontDescriptor>>(found) };
    found.iter().map(|descriptor| with_style(&descriptor, size)).collect()
}

fn with_style(descriptor: &CTFontDescriptor, size: f64) -> Candidate {
    // SAFETY: valid descriptor; a null matrix means identity.
    let font = unsafe { CTFont::with_font_descriptor(descriptor, size, std::ptr::null()) };
    // SAFETY: plain getters; the traits dictionary maps names to numbers.
    let (symbolic, traits) = unsafe { (font.symbolic_traits(), font.traits()) };
    let traits = unsafe { CFRetained::cast_unchecked::<CFDictionary<CFString, CFNumber>>(traits) };
    let weight = traits.get(unsafe { kCTFontWeightTrait }).and_then(|n| n.as_cgfloat()).unwrap_or(0.0);
    Candidate { font, weight: ct_to_css(weight), italic: symbolic.contains(CTFontSymbolicTraits::ItalicTrait) }
}

/// CoreText weights (-1 to 1) against CSS weights, from AppKit's NSFontWeight.
const WEIGHTS: [(f64, f64); 9] = [
    (-0.8, 100.0),
    (-0.6, 200.0),
    (-0.4, 300.0),
    (0.0, 400.0),
    (0.23, 500.0),
    (0.3, 600.0),
    (0.4, 700.0),
    (0.56, 800.0),
    (0.62, 900.0),
];

fn ct_to_css(weight: f64) -> f64 {
    let (first, last) = (WEIGHTS[0], WEIGHTS[WEIGHTS.len() - 1]);
    if weight <= first.0 {
        return first.1;
    }
    WEIGHTS
        .windows(2)
        .find(|w| weight <= w[1].0)
        .map_or(last.1, |w| w[0].1 + (weight - w[0].0) / (w[1].0 - w[0].0) * (w[1].1 - w[0].1))
}

fn css_weight(weight: FontWeight) -> f64 {
    match weight {
        FontWeight::Thin => 100.0,
        FontWeight::Extralight => 200.0,
        FontWeight::Light => 300.0,
        FontWeight::Regular => 400.0,
        FontWeight::Medium => 500.0,
        FontWeight::Demibold => 600.0,
        FontWeight::Bold => 700.0,
        FontWeight::Extrabold => 800.0,
        FontWeight::Black => 900.0,
    }
}

fn metrics(font: &CTFont, line_height: f64) -> Result<Metrics, FontError> {
    let mut chars = [u16::from(b'M')];
    let mut glyphs = [0u16];
    // SAFETY: both buffers hold one element.
    let found = unsafe { font.glyphs_for_characters(NonNull::from(&mut chars[0]), NonNull::from(&mut glyphs[0]), 1) };
    if !found {
        return Err(FontError("font has no 'M'".into()));
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
    let natural = ascent + descent + leading;
    let cell_height = px(natural * line_height);
    // Extra line height is shared above and below the text.
    let baseline = px(ascent + (f64::from(cell_height) - natural) / 2.0);
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
    use crate::config::Font;

    #[test]
    fn metrics_are_monospace_cells() {
        let fonts = Fonts::new(&crate::config::Font::default(), 32.0).expect("fonts");
        let m = fonts.metrics();
        assert!((15..=25).contains(&m.cell_width), "{m:?}");
        assert!(m.cell_height > m.cell_width && m.baseline < m.cell_height, "{m:?}");
    }

    #[test]
    fn text_is_white_and_emoji_is_colored() {
        let fonts = Fonts::new(&crate::config::Font::default(), 32.0).expect("fonts");
        let a = fonts.rasterize("a", Face::default(), 1).expect("ink");
        assert!(!a.colored && a.width > 0 && a.height > 0);
        assert!(fonts.rasterize(" ", Face::default(), 1).is_none());
        let emoji = fonts.rasterize("😀", Face::default(), 2).expect("emoji falls back to another font");
        assert!(emoji.colored);
    }

    #[test]
    fn faces_differ() {
        let fonts = Fonts::new(&crate::config::Font::default(), 32.0).expect("fonts");
        let ink = |face| fonts.rasterize("m", face, 1).expect("ink").data.iter().map(|&b| u64::from(b)).sum::<u64>();
        let regular = ink(Face::default());
        assert!(ink(Face { weight: Weight::Bold, italic: false }) > regular);
        assert!(ink(Face { weight: Weight::Faint, italic: false }) < regular);
    }

    #[test]
    fn weights_pick_the_nearest_embedded_face() {
        let ink = |config: &Font, weight| {
            let fonts = Fonts::new(config, 32.0).expect("fonts");
            let bitmap = fonts.rasterize("m", Face { weight, italic: false }, 1).expect("ink");
            bitmap.data.iter().map(|&b| u64::from(b)).sum::<u64>()
        };
        let regular = ink(&Font::default(), Weight::Regular);
        let plain = Font { bold_weight: FontWeight::Regular, faint_weight: FontWeight::Medium, ..Font::default() };
        assert_eq!(ink(&plain, Weight::Bold), regular);
        assert_eq!(ink(&plain, Weight::Faint), regular);
        let heavy = Font { bold_weight: FontWeight::Black, ..Font::default() };
        assert_eq!(ink(&heavy, Weight::Bold), ink(&Font::default(), Weight::Bold));
    }

    #[test]
    fn line_height_adds_room_on_both_sides() {
        let normal = Fonts::new(&Font::default(), 32.0).expect("fonts").metrics();
        let tall = Fonts::new(&Font { line_height: 1.5, ..Font::default() }, 32.0).expect("fonts").metrics();
        assert!(tall.cell_height > normal.cell_height + 10, "{tall:?}");
        assert!(tall.baseline > normal.baseline && tall.baseline < tall.cell_height, "{tall:?}");
    }

    #[test]
    fn installed_families_load_and_missing_ones_fail() {
        let menlo = Font { family: "Menlo".into(), ..Font::default() };
        assert!(Fonts::new(&menlo, 32.0).is_ok());
        let missing = Font { family: "No Such Font 42".into(), ..Font::default() };
        let err = Fonts::new(&missing, 32.0).err().expect("missing");
        assert!(err.to_string().contains("No Such Font 42"), "{err}");
    }

    #[test]
    fn coretext_weights_map_to_css() {
        assert_eq!(ct_to_css(0.0), 400.0);
        assert_eq!(ct_to_css(0.4), 700.0);
        assert_eq!(ct_to_css(-1.0), 100.0);
        assert_eq!(ct_to_css(1.0), 900.0);
        assert!((ct_to_css(0.115) - 450.0).abs() < 1.0);
    }
}
