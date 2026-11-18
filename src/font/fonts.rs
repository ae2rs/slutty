//! Font faces, cell metrics, and the fonts and shapes the renderer asks for.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ptr::NonNull;
use std::rc::Rc;

use objc2_core_foundation::{
    CFArray, CFAttributedString, CFData, CFDictionary, CFNumber, CFRetained, CFString, CFType, kCFBooleanTrue,
};
use objc2_core_graphics::{CGColorSpace, kCGColorSpaceSRGB};
use objc2_core_text::{
    CTFont, CTFontDescriptor, CTFontManagerCreateFontDescriptorFromData, CTFontOrientation, CTFontSymbolicTraits,
    CTLine, kCTFontAttributeName, kCTFontCascadeListAttribute, kCTFontFamilyNameAttribute,
    kCTFontFeatureSettingsAttribute, kCTFontOpenTypeFeatureTag, kCTFontOpenTypeFeatureValue, kCTFontWeightTrait,
    kCTForegroundColorFromContextAttributeName,
};

use super::{raster, shape};
use crate::config::{self, FontWeight};

/// Shapes kept before the cache starts over.
/// Per face.
const SHAPED_MAX: usize = 10_000;

/// The OpenType features that join characters: JetBrains Mono's code
/// ligatures are contextual alternates (`calt`).
const LIGATURE_FEATURES: [&str; 3] = ["calt", "liga", "clig"];

/// The family slutty embeds, so it looks the same on every Mac.
const EMBEDDED_FAMILY: &str = "JetBrains Mono";

const EMBEDDED: [&[u8]; 6] = [
    include_bytes!("../../assets/fonts/jetbrains-mono/JetBrainsMono-Regular.ttf"),
    include_bytes!("../../assets/fonts/jetbrains-mono/JetBrainsMono-Italic.ttf"),
    include_bytes!("../../assets/fonts/jetbrains-mono/JetBrainsMono-SemiBold.ttf"),
    include_bytes!("../../assets/fonts/jetbrains-mono/JetBrainsMono-SemiBoldItalic.ttf"),
    include_bytes!("../../assets/fonts/jetbrains-mono/JetBrainsMono-ExtraLight.ttf"),
    include_bytes!("../../assets/fonts/jetbrains-mono/JetBrainsMono-ExtraLightItalic.ttf"),
];

/// Nerd Font icons, tried after the configured fallbacks. CoreText's own
/// fallback never picks a font for the private use area, where most of them are.
const SYMBOLS: &[u8] = include_bytes!("../../assets/fonts/symbols-nerd-font/SymbolsNerdFontMono-Regular.ttf");

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

/// A font a shaped glyph comes from: one of the faces, or a fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FontId(u32);

impl FontId {
    /// Braille, drawn by [`raster::braille`] rather than a font. Its glyph
    /// is the pattern: the codepoint less U+2800.
    const BRAILLE: Self = Self(u32::MAX);
}

/// A glyph placed by shaping.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shaped {
    /// The byte offset in the shaped text of the first character it draws.
    pub byte: usize,
    pub font: FontId,
    pub glyph: u16,
    /// The pen position from the start of the text, in pixels.
    pub x: f32,
}

type ShapeCache = HashMap<Box<str>, Rc<[Shaped]>>;

/// A rasterised glyph: premultiplied RGBA, cropped to its ink.
#[derive(Debug)]
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    /// Offset of the bitmap from the pen, at the cell's top.
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
    /// Every font shaping has used, indexed by [`FontId`].
    fonts: RefCell<Vec<CFRetained<CTFont>>>,
    ids: RefCell<HashMap<String, FontId>>,
    shaped: RefCell<HashMap<Face, ShapeCache>>,
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
        let mut faces = vec![
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
        if !config.ligatures {
            faces = faces.iter().map(|font| without_ligatures(font)).collect();
        }
        let cascade = cascade(&config.fallback)?;
        // SAFETY: reading a framework constant.
        let key = unsafe { kCTFontCascadeListAttribute };
        faces = faces.iter().map(|font| with_attribute(font, key, cascade.as_ref())).collect();
        Ok(Self {
            faces,
            metrics,
            color_space,
            fonts: RefCell::default(),
            ids: RefCell::default(),
            shaped: RefCell::default(),
        })
    }

    pub fn metrics(&self) -> Metrics {
        self.metrics
    }

    /// Shapes `text` set in `face` into glyphs, each with the byte of the
    /// text it draws and its pen position from the start.
    pub fn shape(&self, text: &str, face: Face) -> Rc<[Shaped]> {
        if let Some(shaped) = self.shaped.borrow().get(&face).and_then(|cache| cache.get(text)) {
            return Rc::clone(shaped);
        }
        let runs = shape::shape(text, &self.faces[face_index(face)]);
        let shaped: Rc<[Shaped]> = runs
            .into_iter()
            .flat_map(|run| {
                let font = self.intern(run.font);
                run.glyphs.into_iter().map(move |g| {
                    let (font, glyph) = braille(text, g.byte).map_or((font, g.glyph), |dots| (FontId::BRAILLE, dots));
                    Shaped { byte: g.byte, font, glyph, x: g.x as f32 }
                })
            })
            .collect();
        let mut cache = self.shaped.borrow_mut();
        let cache = cache.entry(face).or_default();
        if cache.len() >= SHAPED_MAX {
            cache.clear();
        }
        cache.insert(text.into(), Rc::clone(&shaped));
        shaped
    }

    /// Draws glyph `glyph` of `font`, with its offset from the pen at the
    /// cell's top. `None` for a glyph with no ink, such as a space.
    pub fn rasterize(&self, font: FontId, glyph: u16) -> Option<Bitmap> {
        if font == FontId::BRAILLE {
            return raster::braille(u8::try_from(glyph).ok()?, self.metrics);
        }
        let fonts = self.fonts.borrow();
        let font = fonts.get(font.0 as usize)?;
        raster::draw(font, glyph, self.metrics, &self.color_space)
    }

    /// The id of `font`, the same for every font of one PostScript name.
    fn intern(&self, font: CFRetained<CTFont>) -> FontId {
        // SAFETY: a plain getter.
        let name = unsafe { font.post_script_name() }.to_string();
        let mut ids = self.ids.borrow_mut();
        *ids.entry(name).or_insert_with(|| {
            let mut fonts = self.fonts.borrow_mut();
            fonts.push(font);
            FontId(fonts.len() as u32 - 1)
        })
    }
}

/// The braille pattern of the character at `byte` of `text`, if it is one.
fn braille(text: &str, byte: usize) -> Option<u16> {
    let c = text.get(byte..)?.chars().next()?;
    u16::try_from(u32::from(c).checked_sub(0x2800)?).ok().filter(|&dots| dots <= 0xff)
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
    let query = family_descriptor(family);
    // SAFETY: no mandatory attributes; the array holds font descriptors.
    let Some(found) = (unsafe { query.matching_font_descriptors(None) }) else { return Vec::new() };
    let found = unsafe { CFRetained::cast_unchecked::<CFArray<CTFontDescriptor>>(found) };
    found.iter().map(|descriptor| with_style(&descriptor, size)).collect()
}

fn family_descriptor(family: &str) -> CFRetained<CTFontDescriptor> {
    let name = CFString::from_str(family);
    // SAFETY: reading a framework constant.
    let key = unsafe { kCTFontFamilyNameAttribute };
    let attributes = CFDictionary::<CFString, CFString>::from_slices(&[key], &[&name]);
    // SAFETY: a valid attribute dictionary.
    unsafe { CTFontDescriptor::with_attributes(attributes.as_opaque()) }
}

/// The fonts CoreText tries, in order, for characters a face lacks: the
/// configured `families`, then the embedded symbols.
fn cascade(families: &[String]) -> Result<CFRetained<CFArray<CTFontDescriptor>>, FontError> {
    let mut descriptors = Vec::new();
    for family in families {
        if installed(family, 12.0).is_empty() {
            return Err(FontError(format!("fallback font family `{family}` is not installed")));
        }
        descriptors.push(family_descriptor(family));
    }
    let data = CFData::from_bytes(SYMBOLS);
    // SAFETY: `data` is a valid CFData.
    let symbols = unsafe { CTFontManagerCreateFontDescriptorFromData(&data) }
        .ok_or_else(|| FontError("embedded symbols font is invalid".into()))?;
    descriptors.push(symbols);
    Ok(CFArray::from_retained_objects(&descriptors))
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

/// `font` with the features that join characters turned off.
fn without_ligatures(font: &CTFont) -> CFRetained<CTFont> {
    // SAFETY: reading framework constants.
    let (tag, value, settings) =
        unsafe { (kCTFontOpenTypeFeatureTag, kCTFontOpenTypeFeatureValue, kCTFontFeatureSettingsAttribute) };
    let off = CFNumber::new_i32(0);
    let features: Vec<_> = LIGATURE_FEATURES
        .iter()
        .map(|feature| {
            let name = CFString::from_str(feature);
            let values: [&CFType; 2] = [name.as_ref(), off.as_ref()];
            CFDictionary::<CFString, CFType>::from_slices(&[tag, value], &values)
        })
        .collect();
    let features = CFArray::from_retained_objects(&features);
    with_attribute(font, settings, features.as_ref())
}

/// `font` with the font descriptor attribute `key` set to `value`.
fn with_attribute(font: &CTFont, key: &CFString, value: &CFType) -> CFRetained<CTFont> {
    let attributes = CFDictionary::<CFString, CFType>::from_slices(&[key], &[value]);
    // SAFETY: a valid attribute dictionary.
    let descriptor = unsafe { CTFontDescriptor::with_attributes(attributes.as_opaque()) };
    // SAFETY: valid font and descriptor; size 0 keeps the font's size and a
    // null matrix means identity.
    unsafe { font.copy_with_attributes(0.0, std::ptr::null(), Some(&descriptor)) }
}

pub(super) fn line(text: &str, font: &CTFont) -> Option<CFRetained<CTLine>> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Font;

    fn fonts(config: &Font) -> Fonts {
        Fonts::new(config, 32.0).expect("fonts")
    }

    /// The bitmap of the first glyph of `text`.
    fn draw(fonts: &Fonts, text: &str, face: Face) -> Option<Bitmap> {
        let glyph = fonts.shape(text, face)[0];
        fonts.rasterize(glyph.font, glyph.glyph)
    }

    fn ink(fonts: &Fonts, text: &str, face: Face) -> u64 {
        draw(fonts, text, face).expect("ink").data.iter().map(|&b| u64::from(b)).sum()
    }

    #[test]
    fn metrics_are_monospace_cells() {
        let m = fonts(&Font::default()).metrics();
        assert!((15..=25).contains(&m.cell_width), "{m:?}");
        assert!(m.cell_height > m.cell_width && m.baseline < m.cell_height, "{m:?}");
    }

    #[test]
    fn braille_is_drawn_to_fill_the_cell() {
        let fonts = fonts(&Font::default());
        let face = Face { weight: Weight::Regular, italic: false };
        let shaped = fonts.shape("a⠋", face);
        assert_eq!((shaped[1].font, shaped[1].glyph), (FontId::BRAILLE, 0x0b));
        assert_ne!(shaped[0].font, FontId::BRAILLE);
        let full = draw(&fonts, "⣿", face).expect("ink");
        let m = fonts.metrics();
        assert!(full.height * 4 >= m.cell_height * 3, "{}px of a {}px cell", full.height, m.cell_height);
    }

    #[test]
    fn text_is_white_and_emoji_is_colored() {
        let fonts = fonts(&Font::default());
        let a = draw(&fonts, "a", Face::default()).expect("ink");
        assert!(!a.colored && a.width > 0 && a.height > 0);
        assert!(draw(&fonts, " ", Face::default()).is_none());
        let emoji = fonts.shape("😀", Face::default())[0];
        assert_ne!(emoji.font, fonts.shape("a", Face::default())[0].font, "emoji falls back to another font");
        assert!(fonts.rasterize(emoji.font, emoji.glyph).expect("ink").colored);
    }

    #[test]
    fn glyphs_follow_their_characters() {
        let fonts = fonts(&Font::default());
        let shaped = fonts.shape("aé日b", Face::default());
        let bytes: Vec<usize> = shaped.iter().map(|g| g.byte).collect();
        assert_eq!(bytes, [0, 1, 3, 6]);
        let advance = fonts.metrics().cell_width as f32;
        assert!((shaped[1].x - advance).abs() < 1.0, "{shaped:?}");
        assert!(Rc::ptr_eq(&shaped, &fonts.shape("aé日b", Face::default())), "shapes are cached");
    }

    #[test]
    fn ligatures_can_be_turned_off() {
        let glyphs = |config: &Font| -> Vec<u16> {
            fonts(config).shape("a->b", Face::default()).iter().map(|g| g.glyph).collect()
        };
        let joined = glyphs(&Font::default());
        let plain = glyphs(&Font { ligatures: false, ..Font::default() });
        assert_eq!(joined.len(), 4, "one glyph per cell even when joined");
        assert_ne!(joined, plain);
        let separate = glyphs(&Font { ligatures: false, ..Font::default() });
        assert_eq!(plain, separate);
    }

    #[test]
    fn faces_differ() {
        let fonts = fonts(&Font::default());
        let regular = ink(&fonts, "m", Face::default());
        assert!(ink(&fonts, "m", Face { weight: Weight::Bold, italic: false }) > regular);
        assert!(ink(&fonts, "m", Face { weight: Weight::Faint, italic: false }) < regular);
    }

    #[test]
    fn weights_pick_the_nearest_embedded_face() {
        let ink = |config: &Font, weight| ink(&fonts(config), "m", Face { weight, italic: false });
        let regular = ink(&Font::default(), Weight::Regular);
        let plain = Font { bold_weight: FontWeight::Regular, faint_weight: FontWeight::Medium, ..Font::default() };
        assert_eq!(ink(&plain, Weight::Bold), regular);
        assert_eq!(ink(&plain, Weight::Faint), regular);
        let heavy = Font { bold_weight: FontWeight::Black, ..Font::default() };
        assert_eq!(ink(&heavy, Weight::Bold), ink(&Font::default(), Weight::Bold));
    }

    #[test]
    fn line_height_adds_room_on_both_sides() {
        let normal = fonts(&Font::default()).metrics();
        let tall = fonts(&Font { line_height: 1.5, ..Font::default() }).metrics();
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

    /// The PostScript name of the font the first glyph of `text` comes from.
    fn font_of(fonts: &Fonts, text: &str) -> String {
        let glyph = fonts.shape(text, Face::default())[0];
        // SAFETY: a plain getter.
        unsafe { fonts.fonts.borrow()[glyph.font.0 as usize].post_script_name() }.to_string()
    }

    #[test]
    fn nerd_font_icons_come_from_the_embedded_symbols() {
        let fonts = fonts(&Font::default());
        // A folder, in the private use area CoreText never falls back for.
        assert_eq!(font_of(&fonts, "\u{f115}"), "SymbolsNFM");
        assert_eq!(font_of(&fonts, "😀"), "AppleColorEmoji", "emoji keep the system's font");
    }

    #[test]
    fn configured_fallbacks_come_before_the_symbols() {
        // Both have ☰.
        assert_eq!(font_of(&fonts(&Font::default()), "☰"), "SymbolsNFM");
        let symbols = Font { fallback: vec!["Apple Symbols".into()], ..Font::default() };
        assert_eq!(font_of(&fonts(&symbols), "☰"), "AppleSymbols");
        let missing = Font { fallback: vec!["No Such Font 42".into()], ..Font::default() };
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
