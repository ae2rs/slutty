//! Shaping text into glyphs with CoreText. A CTLine applies the font's
//! ligatures and contextual forms, and takes characters the font lacks from
//! a fallback font of its own choosing.

use std::ptr::NonNull;

use objc2_core_foundation::{CFArray, CFDictionary, CFIndex, CFRange, CFRetained, CFString, CGPoint};
use objc2_core_text::{CTFont, CTRun, kCTFontAttributeName};

/// Glyphs drawn from one font.
pub(super) struct Run {
    pub font: CFRetained<CTFont>,
    pub glyphs: Vec<RunGlyph>,
}

pub(super) struct RunGlyph {
    /// The byte offset in the text of the first character it draws.
    pub byte: usize,
    pub glyph: u16,
    /// The pen position from the line's start, in pixels.
    pub x: f64,
}

/// Shapes `text` set in `font`.
pub(super) fn shape(text: &str, font: &CTFont) -> Vec<Run> {
    let Some(line) = super::fonts::line(text, font) else { return Vec::new() };
    // CoreText counts in UTF-16 units; this maps each back to its byte.
    let mut bytes = Vec::with_capacity(text.len());
    for (byte, c) in text.char_indices() {
        bytes.extend(std::iter::repeat_n(byte, c.len_utf16()));
    }
    // SAFETY: a line's glyph runs are CTRuns.
    let runs = unsafe { CFRetained::cast_unchecked::<CFArray<CTRun>>(line.glyph_runs()) };
    runs.iter().filter_map(|run| read_run(&run, &bytes)).collect()
}

fn read_run(run: &CTRun, bytes: &[usize]) -> Option<Run> {
    // SAFETY: a plain getter.
    let count = usize::try_from(unsafe { run.glyph_count() }).ok().filter(|&n| n > 0)?;
    let mut glyphs = vec![0u16; count];
    let mut positions = vec![CGPoint::new(0.0, 0.0); count];
    let mut indices: Vec<CFIndex> = vec![0; count];
    let all = CFRange { location: 0, length: 0 };
    // SAFETY: each buffer holds `count` elements, the run's glyph count.
    unsafe {
        run.glyphs(all, NonNull::new(glyphs.as_mut_ptr())?);
        run.positions(all, NonNull::new(positions.as_mut_ptr())?);
        run.string_indices(all, NonNull::new(indices.as_mut_ptr())?);
    }
    // SAFETY: a run's font attribute is a CTFont; no other key is read.
    let attributes = unsafe { CFRetained::cast_unchecked::<CFDictionary<CFString, CTFont>>(run.attributes()) };
    let font = attributes.get(unsafe { kCTFontAttributeName })?;
    let glyphs = glyphs
        .into_iter()
        .zip(positions)
        .zip(indices)
        .map(|((glyph, position), index)| RunGlyph {
            byte: usize::try_from(index).ok().and_then(|i| bytes.get(i)).copied().unwrap_or(0),
            glyph,
            x: position.x,
        })
        .collect();
    Some(Run { font, glyphs })
}
