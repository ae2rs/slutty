//! Where a floating pane goes: an anchor in its tab, a size in percent of
//! the tab or in cells, and an offset in cells. Pure geometry.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::Rect;

/// A point of the tab the pane sticks to, as on a numpad.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Anchor {
    TopLeft,
    Top,
    TopRight,
    Left,
    #[default]
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl Anchor {
    /// Where along each axis: 0 the start, 1 the middle, 2 the end.
    fn thirds(self) -> (u16, u16) {
        match self {
            Self::TopLeft => (0, 0),
            Self::Top => (1, 0),
            Self::TopRight => (2, 0),
            Self::Left => (0, 1),
            Self::Center => (1, 1),
            Self::Right => (2, 1),
            Self::BottomLeft => (0, 2),
            Self::Bottom => (1, 2),
            Self::BottomRight => (2, 2),
        }
    }
}

/// A width or height: `40%` of the tab, or `40` cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Extent {
    Percent(u8),
    Cells(u16),
}

impl Extent {
    /// In cells, out of `whole`: at least one, at most `whole`.
    fn cells(self, whole: u16) -> u16 {
        let cells = match self {
            Self::Percent(p) => (u32::from(whole) * u32::from(p.min(100)) / 100) as u16,
            Self::Cells(cells) => cells,
        };
        cells.clamp(1, whole.max(1))
    }
}

impl fmt::Display for Extent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Percent(p) => write!(f, "{p}%"),
            Self::Cells(cells) => write!(f, "{cells}"),
        }
    }
}

impl FromStr for Extent {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || format!("{s:?} is neither a percentage, such as 40%, nor a number of cells");
        match s.strip_suffix('%') {
            Some(p) => match p.parse() {
                Ok(p @ 1..=100) => Ok(Self::Percent(p)),
                _ => Err(bad()),
            },
            None => match s.parse() {
                Ok(0) | Err(_) => Err(bad()),
                Ok(cells) => Ok(Self::Cells(cells)),
            },
        }
    }
}

/// As text, `40%` or `40`, in JSON as on the command line.
impl Serialize for Extent {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Extent {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?.parse().map_err(serde::de::Error::custom)
    }
}

/// Where a floating pane goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Place {
    pub anchor: Anchor,
    pub width: Extent,
    pub height: Extent,
    /// Cells right of the anchored spot; negative goes left.
    pub x_offset: i16,
    /// Cells below the anchored spot; negative goes up.
    pub y_offset: i16,
}

/// A centred 80% by 80% pane, as zellij's.
impl Default for Place {
    fn default() -> Self {
        Self {
            anchor: Anchor::Center,
            width: Extent::Percent(80),
            height: Extent::Percent(80),
            x_offset: 0,
            y_offset: 0,
        }
    }
}

impl Place {
    /// The pane's cells in `area`: sized, put at the anchor, moved by the
    /// offset, then kept inside `area`.
    pub fn rect(&self, area: Rect) -> Rect {
        let cols = self.width.cells(area.cols);
        let rows = self.height.cells(area.rows);
        let (h, v) = self.anchor.thirds();
        let along = |third: u16, room: u16, offset: i16| {
            let start = i32::from(room) * i32::from(third) / 2 + i32::from(offset);
            start.clamp(0, i32::from(room)) as u16
        };
        let x = along(h, area.cols.saturating_sub(cols), self.x_offset);
        let y = along(v, area.rows.saturating_sub(rows), self.y_offset);
        Rect { x: area.x + x, y: area.y + y, cols, rows }
    }
}

/// A change to a [`Place`]: what is `None` stays.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaceChange {
    pub anchor: Option<Anchor>,
    pub width: Option<Extent>,
    pub height: Option<Extent>,
    pub x_offset: Option<i16>,
    pub y_offset: Option<i16>,
}

impl PlaceChange {
    pub fn apply(&self, place: &mut Place) {
        place.anchor = self.anchor.unwrap_or(place.anchor);
        place.width = self.width.unwrap_or(place.width);
        place.height = self.height.unwrap_or(place.height);
        place.x_offset = self.x_offset.unwrap_or(place.x_offset);
        place.y_offset = self.y_offset.unwrap_or(place.y_offset);
    }
}

impl Place {
    /// Moves the pane by `cols` and `rows`, no further than the edges of
    /// `area`: a move past an edge is not kept, so the way back is short.
    pub fn shift(&mut self, area: Rect, cols: i16, rows: i16) {
        let spot = Self { x_offset: 0, y_offset: 0, ..*self }.rect(area);
        self.x_offset = self.x_offset.saturating_add(cols);
        self.y_offset = self.y_offset.saturating_add(rows);
        let rect = self.rect(area);
        self.x_offset = (i32::from(rect.x) - i32::from(spot.x)) as i16;
        self.y_offset = (i32::from(rect.y) - i32::from(spot.y)) as i16;
    }

    /// Grows the pane by `cols` and `rows`, or shrinks it for negative ones,
    /// in cells from what it shows now.
    pub fn grow(&mut self, area: Rect, cols: i16, rows: i16) {
        let rect = self.rect(area);
        let size =
            |now: u16, by: i16, whole: u16| (i32::from(now) + i32::from(by)).clamp(1, i32::from(whole.max(1))) as u16;
        self.width = Extent::Cells(size(rect.cols, cols, area.cols));
        self.height = Extent::Cells(size(rect.rows, rows, area.rows));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect { x: 0, y: 0, cols: 100, rows: 40 };

    fn place(anchor: Anchor, width: Extent, height: Extent) -> Place {
        Place { anchor, width, height, ..Place::default() }
    }

    #[test]
    fn every_anchor_puts_the_pane_at_its_spot() {
        let at = |anchor| {
            let r = place(anchor, Extent::Cells(10), Extent::Cells(4)).rect(AREA);
            (r.x, r.y)
        };
        assert_eq!(at(Anchor::TopLeft), (0, 0));
        assert_eq!(at(Anchor::Top), (45, 0));
        assert_eq!(at(Anchor::TopRight), (90, 0));
        assert_eq!(at(Anchor::Left), (0, 18));
        assert_eq!(at(Anchor::Center), (45, 18));
        assert_eq!(at(Anchor::Right), (90, 18));
        assert_eq!(at(Anchor::BottomLeft), (0, 36));
        assert_eq!(at(Anchor::Bottom), (45, 36));
        assert_eq!(at(Anchor::BottomRight), (90, 36));
    }

    #[test]
    fn sizes_are_percentages_of_the_tab_or_cells() {
        let r = place(Anchor::TopRight, Extent::Percent(40), Extent::Percent(50)).rect(AREA);
        assert_eq!(r, Rect { x: 60, y: 0, cols: 40, rows: 20 });
        assert_eq!(Place::default().rect(AREA), Rect { x: 10, y: 4, cols: 80, rows: 32 });
    }

    #[test]
    fn offsets_move_the_pane_and_it_stays_inside() {
        let moved = Place { x_offset: -5, y_offset: 3, ..place(Anchor::TopRight, Extent::Cells(10), Extent::Cells(4)) };
        assert_eq!(moved.rect(AREA), Rect { x: 85, y: 3, cols: 10, rows: 4 });
        let pushed = Place { x_offset: 50, y_offset: -9, ..moved };
        assert_eq!(pushed.rect(AREA), Rect { x: 90, y: 0, cols: 10, rows: 4 });
        let huge = place(Anchor::Center, Extent::Cells(500), Extent::Cells(500));
        assert_eq!(huge.rect(AREA), AREA);
        let tiny = Rect { x: 0, y: 1, cols: 0, rows: 0 };
        assert_eq!(Place::default().rect(tiny), Rect { x: 0, y: 1, cols: 1, rows: 1 }, "at least one cell");
    }

    #[test]
    fn a_move_past_an_edge_is_not_kept() {
        let mut p = Place { x_offset: -2, ..place(Anchor::TopRight, Extent::Cells(10), Extent::Cells(4)) };
        p.shift(AREA, 5, 0);
        assert_eq!((p.x_offset, p.rect(AREA).x), (0, 90), "stopped at the right edge");
        p.shift(AREA, -1, -3);
        assert_eq!((p.x_offset, p.y_offset), (-1, 0));
        p.grow(AREA, 3, -10);
        assert_eq!((p.width, p.height), (Extent::Cells(13), Extent::Cells(1)));
    }

    #[test]
    fn extents_read_and_print_as_on_the_command_line() {
        assert_eq!("40%".parse(), Ok(Extent::Percent(40)));
        assert_eq!("12".parse(), Ok(Extent::Cells(12)));
        for bad in ["0", "0%", "101%", "-3", "40 %", "wide", ""] {
            assert!(bad.parse::<Extent>().is_err(), "{bad}");
        }
        assert_eq!(Extent::Percent(40).to_string(), "40%");
        let json = serde_json::to_string(&Place::default()).expect("json");
        assert_eq!(json, r#"{"anchor":"center","width":"80%","height":"80%","x_offset":0,"y_offset":0}"#);
    }
}
