//! Where things go in a window, in device pixels: the tab bar on top, then
//! the panes inside the padding.

use objc2_foundation::NSSize;

use crate::config::{Config, Padding};
use crate::font::Metrics;
use crate::terminal::Geometry;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub width: u32,
    pub height: u32,
    /// The top-left corner of the pane area.
    pub origin: [u32; 2],
    /// The pane area as a grid of cells.
    pub area: Geometry,
    pub tab_bar: bool,
}

impl Layout {
    /// Fits a view of `size` points.
    pub fn new(size: NSSize, scale: f64, m: Metrics, padding: Padding, tab_bar: bool) -> Self {
        let width = (size.width * scale).round().max(0.0) as u32;
        let height = (size.height * scale).round().max(0.0) as u32;
        let [left, right, top, bottom] = padding_pixels(padding, m, scale);
        let top = top + if tab_bar { m.cell_height } else { 0 };
        let fit = |space: u32, cell: u32| u16::try_from((space / cell).max(1)).unwrap_or(u16::MAX);
        let area = Geometry {
            cols: fit(width.saturating_sub(left + right), m.cell_width),
            rows: fit(height.saturating_sub(top + bottom), m.cell_height),
            cell_width: m.cell_width,
            cell_height: m.cell_height,
        };
        Self { width, height, origin: [left, top], area, tab_bar }
    }

    /// The cell under a point in device pixels, if it is in the tab bar.
    pub fn tab_bar_cell(&self, x: f64, y: f64) -> Option<usize> {
        let in_bar = self.tab_bar && y >= 0.0 && y < f64::from(self.area.cell_height);
        in_bar.then(|| (x.max(0.0) / f64::from(self.area.cell_width)) as usize)
    }
}

/// Left, right, top and bottom padding in device pixels.
fn padding_pixels(p: Padding, m: Metrics, scale: f64) -> [u32; 4] {
    [
        p.left.pixels(m.cell_width, scale),
        p.right.pixels(m.cell_width, scale),
        p.top.pixels(m.cell_height, scale),
        p.bottom.pixels(m.cell_height, scale),
    ]
}

/// Window content size, in points, that fits the configured grid.
pub fn content_size(config: &Config, m: Metrics, scale: f64, tab_bar: bool) -> NSSize {
    let [left, right, top, bottom] = padding_pixels(config.window.padding, m, scale);
    let bar = if tab_bar { m.cell_height } else { 0 };
    NSSize::new(
        f64::from(u32::from(config.window.cols) * m.cell_width + left + right) / scale,
        f64::from(u32::from(config.window.rows) * m.cell_height + top + bottom + bar) / scale,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    const M: Metrics = Metrics { cell_width: 10, cell_height: 20, baseline: 15, underline_offset: 17, stroke: 1 };

    #[test]
    fn the_tab_bar_and_padding_come_off_the_pane_area() {
        let config = Config::default();
        let size = content_size(&config, M, 2.0, true);
        let layout = Layout::new(size, 2.0, M, config.window.padding, true);
        assert_eq!((layout.area.cols, layout.area.rows), (config.window.cols, config.window.rows));
        assert_eq!(layout.origin, [10, 20]);
        assert_eq!(layout.tab_bar_cell(25.0, 5.0), Some(2));
        assert_eq!(layout.tab_bar_cell(25.0, 25.0), None);
        let bare = Layout::new(size, 2.0, M, config.window.padding, false);
        assert_eq!(bare.area.rows, config.window.rows + 1);
        assert_eq!(bare.tab_bar_cell(25.0, 5.0), None);
    }
}
