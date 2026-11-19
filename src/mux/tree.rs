//! A tab's panes as a tree of splits. Pure geometry in cells.
//!
//! Each split leaves one cell between its sides for the divider, as WezTerm
//! does, and keeps the share of the first side so a window resize scales
//! both.

use serde::{Deserialize, Serialize};

use super::PaneId;

/// A rectangle of cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub cols: u16,
    pub rows: u16,
}

/// How a split lays out its two sides.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Axis {
    /// Side by side: the second side is to the right.
    Row,
    /// Stacked: the second side is below.
    Column,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    fn axis(self) -> Axis {
        match self {
            Self::Left | Self::Right => Axis::Row,
            Self::Up | Self::Down => Axis::Column,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Node {
    Leaf(PaneId),
    Split { axis: Axis, ratio: f32, first: Box<Node>, second: Box<Node> },
}

/// Where each pane goes, and the dividers between them.
#[derive(Debug, Default, PartialEq)]
pub struct Layout {
    pub panes: Vec<(PaneId, Rect)>,
    pub dividers: Vec<(Axis, Rect)>,
}

impl Layout {
    pub fn rect(&self, pane: PaneId) -> Option<Rect> {
        self.panes.iter().find(|(id, _)| *id == pane).map(|(_, rect)| *rect)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tree {
    root: Node,
}

impl Tree {
    pub fn new(pane: PaneId) -> Self {
        Self { root: Node::Leaf(pane) }
    }

    /// The panes in reading order: first sides before second sides.
    pub fn panes(&self) -> Vec<PaneId> {
        let mut panes = Vec::new();
        self.root.leaves(&mut panes);
        panes
    }

    pub fn contains(&self, pane: PaneId) -> bool {
        self.panes().contains(&pane)
    }

    /// Puts `new` beside `target`, sharing its space evenly. Returns whether
    /// `target` was found.
    pub fn split(&mut self, target: PaneId, new: PaneId, axis: Axis) -> bool {
        let Some(node) = self.root.find_mut(target) else { return false };
        let first = Box::new(Node::Leaf(target));
        *node = Node::Split { axis, ratio: 0.5, first, second: Box::new(Node::Leaf(new)) };
        true
    }

    /// Takes `pane` out; its sibling gets its space. Returns the pane that
    /// takes its place, or `None` when it was the last or is not here.
    pub fn remove(&mut self, pane: PaneId) -> Option<PaneId> {
        self.root.remove(pane)
    }

    pub fn layout(&self, rect: Rect) -> Layout {
        let mut layout = Layout::default();
        self.root.layout(rect, &mut layout);
        layout
    }

    /// The pane next to `from` in `direction`: of those touching that edge,
    /// the one sharing the most of it.
    pub fn neighbour(&self, rect: Rect, from: PaneId, direction: Direction) -> Option<PaneId> {
        let layout = self.layout(rect);
        let a = layout.rect(from)?;
        // The span along the edge: rows for left and right, columns for up
        // and down.
        let span = |r: Rect| match direction.axis() {
            Axis::Row => (r.y, r.y + r.rows),
            Axis::Column => (r.x, r.x + r.cols),
        };
        let (a_start, a_end) = span(a);
        layout
            .panes
            .iter()
            // Reversed, so ties go to the first pane in reading order.
            .rev()
            .filter(|(_, b)| match direction {
                Direction::Left => b.x + b.cols + 1 == a.x,
                Direction::Right => a.x + a.cols + 1 == b.x,
                Direction::Up => b.y + b.rows + 1 == a.y,
                Direction::Down => a.y + a.rows + 1 == b.y,
            })
            .map(|(id, b)| {
                let (start, end) = span(*b);
                (*id, a_end.min(end).saturating_sub(a_start.max(start)))
            })
            .filter(|&(_, overlap)| overlap > 0)
            .max_by_key(|&(_, overlap)| overlap)
            .map(|(id, _)| id)
    }

    /// Moves the nearest divider of `direction`'s axis around `pane` by
    /// `cells` in that direction. Each side keeps at least one cell.
    pub fn resize(&mut self, rect: Rect, pane: PaneId, direction: Direction, cells: u16) {
        let delta = match direction {
            Direction::Left | Direction::Up => -f32::from(cells),
            Direction::Right | Direction::Down => f32::from(cells),
        };
        self.root.resize(rect, pane, direction.axis(), delta);
    }

    /// Moves every pane one place along reading order, the last to the
    /// first, keeping the splits as they are.
    pub fn rotate_clockwise(&mut self) {
        let mut panes = self.panes();
        panes.rotate_right(1);
        self.root.relabel(&mut panes.into_iter());
    }
}

impl Node {
    fn leaves(&self, out: &mut Vec<PaneId>) {
        match self {
            Self::Leaf(id) => out.push(*id),
            Self::Split { first, second, .. } => {
                first.leaves(out);
                second.leaves(out);
            }
        }
    }

    fn first_leaf(&self) -> PaneId {
        match self {
            Self::Leaf(id) => *id,
            Self::Split { first, .. } => first.first_leaf(),
        }
    }

    fn find_mut(&mut self, pane: PaneId) -> Option<&mut Self> {
        match self {
            Self::Leaf(id) if *id == pane => Some(self),
            Self::Leaf(_) => None,
            Self::Split { first, second, .. } => first.find_mut(pane).or_else(|| second.find_mut(pane)),
        }
    }

    fn contains(&self, pane: PaneId) -> bool {
        match self {
            Self::Leaf(id) => *id == pane,
            Self::Split { first, second, .. } => first.contains(pane) || second.contains(pane),
        }
    }

    fn remove(&mut self, pane: PaneId) -> Option<PaneId> {
        let Self::Split { first, second, .. } = self else { return None };
        let sibling = if **first == Self::Leaf(pane) {
            second
        } else if **second == Self::Leaf(pane) {
            first
        } else {
            return first.remove(pane).or_else(|| second.remove(pane));
        };
        let sibling = std::mem::replace(&mut **sibling, Self::Leaf(pane));
        let next = sibling.first_leaf();
        *self = sibling;
        Some(next)
    }

    /// The sizes of the two sides of a split across `size` cells.
    fn sides(ratio: f32, size: u16) -> (u16, u16) {
        let space = size.saturating_sub(1);
        if space < 2 {
            return (space, 0);
        }
        let first = (ratio * f32::from(space)).round().clamp(1.0, f32::from(space - 1)) as u16;
        (first, space - first)
    }

    fn halves(axis: Axis, ratio: f32, rect: Rect) -> (Rect, Rect, Rect) {
        match axis {
            Axis::Row => {
                let (a, b) = Self::sides(ratio, rect.cols);
                (
                    Rect { cols: a, ..rect },
                    Rect { x: rect.x + a, cols: 1.min(rect.cols), ..rect },
                    Rect { x: rect.x + a + 1, cols: b, ..rect },
                )
            }
            Axis::Column => {
                let (a, b) = Self::sides(ratio, rect.rows);
                (
                    Rect { rows: a, ..rect },
                    Rect { y: rect.y + a, rows: 1.min(rect.rows), ..rect },
                    Rect { y: rect.y + a + 1, rows: b, ..rect },
                )
            }
        }
    }

    fn layout(&self, rect: Rect, out: &mut Layout) {
        match self {
            Self::Leaf(id) => out.panes.push((*id, rect)),
            Self::Split { axis, ratio, first, second } => {
                let (a, divider, b) = Self::halves(*axis, *ratio, rect);
                first.layout(a, out);
                out.dividers.push((*axis, divider));
                second.layout(b, out);
            }
        }
    }

    /// Returns whether a divider moved.
    fn resize(&mut self, rect: Rect, pane: PaneId, axis: Axis, delta: f32) -> bool {
        let Self::Split { axis: own, ratio, first, second } = self else { return false };
        let (a, _, b) = Self::halves(*own, *ratio, rect);
        let inner = if first.contains(pane) {
            first.resize(a, pane, axis, delta)
        } else if second.contains(pane) {
            second.resize(b, pane, axis, delta)
        } else {
            return false;
        };
        if inner || *own != axis {
            return inner;
        }
        let size = match axis {
            Axis::Row => rect.cols,
            Axis::Column => rect.rows,
        };
        let space = f32::from(size.saturating_sub(1).max(1));
        *ratio = (*ratio + delta / space).clamp(1.0 / space, 1.0 - 1.0 / space);
        true
    }

    fn relabel(&mut self, panes: &mut impl Iterator<Item = PaneId>) {
        match self {
            Self::Leaf(id) => {
                if let Some(pane) = panes.next() {
                    *id = pane;
                }
            }
            Self::Split { first, second, .. } => {
                first.relabel(panes);
                second.relabel(panes);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect { x: 0, y: 0, cols: 81, rows: 25 };

    fn p(id: u64) -> PaneId {
        PaneId(id)
    }

    /// 1 on the left; 2 above 3 on the right.
    fn three() -> Tree {
        let mut tree = Tree::new(p(1));
        assert!(tree.split(p(1), p(2), Axis::Row));
        assert!(tree.split(p(2), p(3), Axis::Column));
        tree
    }

    #[test]
    fn splits_share_the_space_around_a_divider() {
        let layout = three().layout(AREA);
        assert_eq!(layout.rect(p(1)), Some(Rect { x: 0, y: 0, cols: 40, rows: 25 }));
        assert_eq!(layout.rect(p(2)), Some(Rect { x: 41, y: 0, cols: 40, rows: 12 }));
        assert_eq!(layout.rect(p(3)), Some(Rect { x: 41, y: 13, cols: 40, rows: 12 }));
        assert_eq!(
            layout.dividers,
            vec![
                (Axis::Row, Rect { x: 40, y: 0, cols: 1, rows: 25 }),
                (Axis::Column, Rect { x: 41, y: 12, cols: 40, rows: 1 })
            ]
        );
    }

    #[test]
    fn removing_gives_the_space_to_the_sibling() {
        let mut tree = three();
        assert_eq!(tree.remove(p(2)), Some(p(3)));
        assert_eq!(tree.layout(AREA).rect(p(3)), Some(Rect { x: 41, y: 0, cols: 40, rows: 25 }));
        assert_eq!(tree.remove(p(1)), Some(p(3)));
        assert_eq!(tree.layout(AREA).rect(p(3)), Some(AREA));
        assert_eq!(tree.remove(p(3)), None, "the last pane stays");
        assert_eq!(tree.remove(p(9)), None);
    }

    #[test]
    fn neighbours_touch_the_edge() {
        let tree = three();
        assert_eq!(tree.neighbour(AREA, p(1), Direction::Right), Some(p(2)), "the larger overlap, then the first");
        assert_eq!(tree.neighbour(AREA, p(3), Direction::Left), Some(p(1)));
        assert_eq!(tree.neighbour(AREA, p(3), Direction::Up), Some(p(2)));
        assert_eq!(tree.neighbour(AREA, p(2), Direction::Down), Some(p(3)));
        assert_eq!(tree.neighbour(AREA, p(1), Direction::Left), None);
        assert_eq!(tree.neighbour(AREA, p(2), Direction::Up), None);
    }

    #[test]
    fn resizing_moves_the_nearest_divider_of_that_axis() {
        let mut tree = three();
        tree.resize(AREA, p(3), Direction::Left, 5);
        assert_eq!(tree.layout(AREA).rect(p(1)).map(|r| r.cols), Some(35));
        tree.resize(AREA, p(3), Direction::Up, 2);
        assert_eq!(tree.layout(AREA).rect(p(2)).map(|r| r.rows), Some(10));
        tree.resize(AREA, p(1), Direction::Right, 500);
        assert_eq!(tree.layout(AREA).rect(p(2)).map(|r| r.cols), Some(1), "each side keeps a cell");
        let mut single = Tree::new(p(1));
        single.resize(AREA, p(1), Direction::Left, 5);
        assert_eq!(single.layout(AREA).rect(p(1)), Some(AREA));
    }

    #[test]
    fn rotating_moves_panes_not_splits() {
        let mut tree = three();
        let before = tree.layout(AREA);
        tree.rotate_clockwise();
        assert_eq!(tree.panes(), vec![p(3), p(1), p(2)]);
        assert_eq!(tree.layout(AREA).rect(p(3)), before.rect(p(1)));
    }

    #[test]
    fn tiny_areas_do_not_underflow() {
        let tree = three();
        let layout = tree.layout(Rect { x: 0, y: 0, cols: 1, rows: 1 });
        assert_eq!(layout.panes.len(), 3);
    }
}
