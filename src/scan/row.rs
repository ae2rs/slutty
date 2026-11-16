//! A screen row as a string, with the cell each character sits in.

use crate::terminal::{Grapheme, Row};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RowText {
    pub text: String,
    /// For each character of `text`, its byte offset and its first cell.
    starts: Vec<(usize, u16)>,
    cols: u16,
}

impl RowText {
    pub fn new(row: &Row) -> Self {
        let mut out = Self::default();
        let mut cells = row.cells.iter().enumerate();
        while let Some((col, cell)) = cells.next() {
            let col = u16::try_from(col).unwrap_or(u16::MAX);
            out.starts.push((out.text.len(), col));
            match &cell.text {
                Grapheme::Empty => out.text.push(' '),
                Grapheme::Char(c) => out.text.push(*c),
                Grapheme::Cluster(s) => out.text.push_str(s),
            }
            if cell.style.wide {
                // The spacer after a wide glyph is part of it.
                cells.next();
            }
        }
        out.cols = u16::try_from(row.cells.len()).unwrap_or(u16::MAX);
        out
    }

    #[cfg(test)]
    pub fn from_str(text: &str) -> Self {
        let mut out = Self::default();
        for (i, (byte, _)) in text.char_indices().enumerate() {
            out.starts.push((byte, i as u16));
        }
        out.text = text.to_owned();
        out.cols = out.starts.len() as u16;
        out
    }

    /// The cell where the character at byte `offset` starts; the end of the
    /// row for the end of the text.
    pub fn col(&self, offset: usize) -> u16 {
        match self.starts.binary_search_by_key(&offset, |&(byte, _)| byte) {
            Ok(i) => self.starts[i].1,
            Err(i) => self.starts.get(i).map_or(self.cols, |&(_, col)| col),
        }
    }
}

/// Cells `start..end` of row `row`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub row: u16,
    pub start: u16,
    pub end: u16,
    pub text: String,
}

impl Span {
    pub fn new(row: u16, text: &RowText, bytes: std::ops::Range<usize>) -> Self {
        Self { row, start: text.col(bytes.start), end: text.col(bytes.end), text: text.text[bytes].to_owned() }
    }

    pub fn contains(&self, col: u16) -> bool {
        (self.start..self.end).contains(&col)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{Cell, CellStyle};
    use libghostty_vt::color::Rgb;

    fn cell(text: Grapheme, wide: bool) -> Cell {
        let black = Rgb { r: 0, g: 0, b: 0 };
        Cell { text, fg: black, bg: None, style: CellStyle { wide, ..CellStyle::default() } }
    }

    #[test]
    fn wide_glyphs_take_two_cells() {
        let row = Row {
            cells: vec![
                cell(Grapheme::Char('a'), false),
                cell(Grapheme::Char('界'), true),
                cell(Grapheme::Empty, false),
                cell(Grapheme::Char('b'), false),
            ],
            dirty: false,
        };
        let text = RowText::new(&row);
        assert_eq!(text.text, "a界b");
        assert_eq!(text.col(1), 1);
        assert_eq!(text.col(4), 3, "after the three bytes of 界");
        assert_eq!(text.col(5), 4, "the end of the row");
    }
}
