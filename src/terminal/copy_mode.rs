//! Copy mode: a cursor that moves over the screen and its scrollback with
//! WezTerm's vim-like motions, and the selection it spans. Pure: it reads
//! the text through [`Grid`].

use serde::{Deserialize, Serialize};

/// A cell in screen coordinates: rows count from the top of the scrollback.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub y: u32,
    pub x: u16,
}

/// The text copy mode moves over.
pub trait Grid {
    fn cols(&self) -> u16;
    /// Rows in the scrollback and the screen.
    fn rows(&self) -> u32;
    /// One character per cell; empty cells are spaces.
    fn line(&self, y: u32) -> Vec<char>;
    /// The first row on screen, and how many rows show.
    fn viewport(&self) -> (u32, u16);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Cell,
    Line,
    Block,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordForward,
    WordBackward,
    WordEnd,
    LineStart,
    LineContentStart,
    LineContentEnd,
    NextLineStart,
    Top,
    Bottom,
    ViewportTop,
    ViewportMiddle,
    ViewportBottom,
    /// By half pages: -2 is a page up.
    Halves(i8),
    JumpAgain,
    JumpReverse,
}

/// `f`, `t`, `F` and `T`: to the next character typed, or just before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Jump {
    pub forward: bool,
    pub till: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {
    Move(Motion),
    /// Starts a selection of this kind, or ends it when it is already one.
    Select(Kind),
    /// Swaps the cursor with the other end of the selection.
    OtherEnd,
    /// The same, for the column only: the other corner of a block.
    OtherEndHoriz,
    /// Waits for the character to jump to.
    Jump(Jump),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyMode {
    cursor: Pos,
    anchor: Option<(Pos, Kind)>,
    pending: Option<Jump>,
    last_jump: Option<(Jump, char)>,
}

impl CopyMode {
    pub fn new(cursor: Pos) -> Self {
        Self { cursor, anchor: None, pending: None, last_jump: None }
    }

    pub fn cursor(&self) -> Pos {
        self.cursor
    }

    /// Whether the next character is a jump target rather than a command.
    pub fn awaits_jump(&self) -> bool {
        self.pending.is_some()
    }

    pub fn apply(&mut self, grid: &impl Grid, command: Command) {
        match command {
            Command::Move(motion) => self.cursor = self.moved(grid, motion),
            Command::Select(kind) => {
                self.anchor = match self.anchor {
                    Some((_, current)) if current == kind => None,
                    Some((anchor, _)) => Some((anchor, kind)),
                    None => Some((self.cursor, kind)),
                }
            }
            Command::OtherEnd => {
                if let Some((anchor, _)) = &mut self.anchor {
                    std::mem::swap(anchor, &mut self.cursor);
                }
            }
            Command::OtherEndHoriz => {
                if let Some((anchor, _)) = &mut self.anchor {
                    std::mem::swap(&mut anchor.x, &mut self.cursor.x);
                }
            }
            Command::Jump(jump) => self.pending = Some(jump),
        }
    }

    /// Jumps to `target` on the cursor's line, after a [`Command::Jump`].
    pub fn jump_to(&mut self, grid: &impl Grid, target: char) {
        if let Some(jump) = self.pending.take() {
            self.last_jump = Some((jump, target));
            self.cursor = jumped(grid, self.cursor, jump, target);
        }
    }

    /// The selection's ends in order, and whether it is a block.
    pub fn selection(&self, cols: u16) -> Option<(Pos, Pos, bool)> {
        let (anchor, kind) = self.anchor?;
        let (start, end) = if anchor <= self.cursor { (anchor, self.cursor) } else { (self.cursor, anchor) };
        Some(match kind {
            Kind::Cell => (start, end, false),
            Kind::Line => (Pos { x: 0, ..start }, Pos { x: cols.saturating_sub(1), ..end }, false),
            Kind::Block => (anchor, self.cursor, true),
        })
    }

    fn moved(&self, grid: &impl Grid, motion: Motion) -> Pos {
        let Pos { x, y } = self.cursor;
        let last_row = grid.rows().saturating_sub(1);
        let last_col = grid.cols().saturating_sub(1);
        let (top, height) = grid.viewport();
        let row = |y: u32| Pos { x, y: y.min(last_row) };
        match motion {
            Motion::Left => Pos { x: x.saturating_sub(1), y },
            Motion::Right => Pos { x: (x + 1).min(last_col), y },
            Motion::Up => row(y.saturating_sub(1)),
            Motion::Down => row(y + 1),
            Motion::WordForward => word_forward(grid, self.cursor),
            Motion::WordBackward => word_backward(grid, self.cursor),
            Motion::WordEnd => word_end(grid, self.cursor),
            Motion::LineStart => Pos { x: 0, y },
            Motion::LineContentStart => {
                let line = grid.line(y);
                Pos { x: line.iter().position(|c| !c.is_whitespace()).unwrap_or(0) as u16, y }
            }
            Motion::LineContentEnd => {
                let line = grid.line(y);
                Pos { x: line.iter().rposition(|c| !c.is_whitespace()).unwrap_or(0) as u16, y }
            }
            Motion::NextLineStart => Pos { x: 0, y: (y + 1).min(last_row) },
            Motion::Top => Pos { x: 0, y: 0 },
            Motion::Bottom => Pos { x: 0, y: last_row },
            Motion::ViewportTop => row(top),
            Motion::ViewportMiddle => row(top + u32::from(height) / 2),
            Motion::ViewportBottom => row(top + u32::from(height).saturating_sub(1)),
            Motion::Halves(halves) => {
                let delta = i64::from(halves) * i64::from(height) / 2;
                row((i64::from(y) + delta).clamp(0, i64::from(last_row)) as u32)
            }
            Motion::JumpAgain | Motion::JumpReverse => match self.last_jump {
                Some((jump, target)) => {
                    let jump =
                        if motion == Motion::JumpReverse { Jump { forward: !jump.forward, ..jump } } else { jump };
                    jumped(grid, self.cursor, jump, target)
                }
                None => self.cursor,
            },
        }
    }
}

/// Spaces, word characters and punctuation: a word is a run of one class.
fn class(c: char) -> u8 {
    if c.is_whitespace() {
        0
    } else if c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

/// Walks the text cell by cell, with a space between lines so a word never
/// runs across a line break.
struct Walker<'g, G> {
    grid: &'g G,
    pos: Pos,
    line: Vec<char>,
    /// At the break after the line, not on a cell.
    at_break: bool,
}

impl<'g, G: Grid> Walker<'g, G> {
    fn new(grid: &'g G, pos: Pos) -> Self {
        Self { grid, pos, line: grid.line(pos.y), at_break: false }
    }

    fn char(&self) -> char {
        if self.at_break { ' ' } else { self.line.get(usize::from(self.pos.x)).copied().unwrap_or(' ') }
    }

    fn class(&self) -> u8 {
        class(self.char())
    }

    /// Returns false at the end of the text.
    fn forward(&mut self) -> bool {
        if self.at_break {
            if self.pos.y + 1 >= self.grid.rows() {
                return false;
            }
            self.at_break = false;
            self.pos = Pos { x: 0, y: self.pos.y + 1 };
            self.line = self.grid.line(self.pos.y);
        } else if self.pos.x + 1 >= self.grid.cols() {
            self.at_break = true;
        } else {
            self.pos.x += 1;
        }
        true
    }

    /// Returns false at the start of the text.
    fn backward(&mut self) -> bool {
        if self.at_break {
            self.at_break = false;
        } else if self.pos.x > 0 {
            self.pos.x -= 1;
        } else {
            if self.pos.y == 0 {
                return false;
            }
            self.pos = Pos { x: self.grid.cols().saturating_sub(1), y: self.pos.y - 1 };
            self.line = self.grid.line(self.pos.y);
            self.at_break = true;
        }
        true
    }

    /// The cell it is on, or the last cell of the line at a break.
    fn pos(&self) -> Pos {
        self.pos
    }
}

/// `w`: the start of the next word.
fn word_forward(grid: &impl Grid, from: Pos) -> Pos {
    let mut w = Walker::new(grid, from);
    let start = w.class();
    while start != 0 && w.class() == start {
        if !w.forward() {
            return from;
        }
    }
    while w.class() == 0 {
        if !w.forward() {
            return from;
        }
    }
    w.pos()
}

/// `b`: the start of this word, or of the one before.
fn word_backward(grid: &impl Grid, from: Pos) -> Pos {
    let mut w = Walker::new(grid, from);
    if !w.backward() {
        return from;
    }
    while w.class() == 0 {
        if !w.backward() {
            return from;
        }
    }
    let class = w.class();
    loop {
        let here = w.pos();
        if !w.backward() || w.class() != class {
            return here;
        }
    }
}

/// `e`: the end of this word, or of the next.
fn word_end(grid: &impl Grid, from: Pos) -> Pos {
    let mut w = Walker::new(grid, from);
    if !w.forward() {
        return from;
    }
    while w.class() == 0 {
        if !w.forward() {
            return from;
        }
    }
    let class = w.class();
    loop {
        let here = w.pos();
        if !w.forward() || w.class() != class {
            return here;
        }
    }
}

fn jumped(grid: &impl Grid, from: Pos, jump: Jump, target: char) -> Pos {
    let line = grid.line(from.y);
    let x = usize::from(from.x);
    let found = if jump.forward {
        // `t` again from just before a match must reach the next one.
        let skip = if jump.till { 2 } else { 1 };
        line.iter()
            .enumerate()
            .skip(x + skip)
            .find(|(_, c)| **c == target)
            .map(|(i, _)| if jump.till { i - 1 } else { i })
    } else {
        let skip = if jump.till { 1 } else { 0 };
        line[..x.saturating_sub(skip).min(line.len())]
            .iter()
            .rposition(|c| *c == target)
            .map(|i| if jump.till { i + 1 } else { i })
    };
    found.map_or(from, |x| Pos { x: x as u16, y: from.y })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Text(Vec<&'static str>);

    impl Grid for Text {
        fn cols(&self) -> u16 {
            20
        }
        fn rows(&self) -> u32 {
            self.0.len() as u32
        }
        fn line(&self, y: u32) -> Vec<char> {
            let mut line: Vec<char> = self.0[y as usize].chars().collect();
            line.resize(20, ' ');
            line
        }
        fn viewport(&self) -> (u32, u16) {
            (1, 2)
        }
    }

    fn text() -> Text {
        Text(vec!["foo.bar  baz", "  qux", "", "last"])
    }

    fn at(x: u16, y: u32) -> Pos {
        Pos { x, y }
    }

    fn moved(from: Pos, motions: &[Motion]) -> Pos {
        let grid = text();
        let mut mode = CopyMode::new(from);
        for &m in motions {
            mode.apply(&grid, Command::Move(m));
        }
        mode.cursor()
    }

    #[test]
    fn words_move_by_class_and_across_lines() {
        use Motion::*;
        assert_eq!(moved(at(0, 0), &[WordForward]), at(3, 0), "punctuation is its own word");
        assert_eq!(moved(at(0, 0), &[WordForward, WordForward, WordForward]), at(9, 0));
        assert_eq!(moved(at(9, 0), &[WordForward]), at(2, 1), "on to the next line");
        assert_eq!(moved(at(2, 1), &[WordForward]), at(0, 3), "over an empty line");
        assert_eq!(moved(at(2, 1), &[WordBackward]), at(9, 0));
        assert_eq!(moved(at(5, 0), &[WordBackward]), at(4, 0));
        assert_eq!(moved(at(4, 0), &[WordBackward, WordBackward]), at(0, 0));
        assert_eq!(moved(at(0, 0), &[WordEnd]), at(2, 0));
        assert_eq!(moved(at(4, 0), &[WordEnd]), at(6, 0));
        assert_eq!(moved(at(0, 3), &[WordForward]), at(0, 3), "nothing after the last word");
    }

    #[test]
    fn lines_and_the_viewport() {
        use Motion::*;
        assert_eq!(moved(at(5, 1), &[LineContentStart]), at(2, 1));
        assert_eq!(moved(at(0, 0), &[LineContentEnd]), at(11, 0));
        assert_eq!(moved(at(5, 0), &[Up]), at(5, 0));
        assert_eq!(moved(at(5, 2), &[Down, Down]), at(5, 3), "stops at the last row");
        assert_eq!(moved(at(0, 0), &[Left]), at(0, 0));
        assert_eq!(moved(at(3, 3), &[ViewportTop]), at(3, 1));
        assert_eq!(moved(at(3, 3), &[ViewportBottom]), at(3, 2));
        assert_eq!(moved(at(3, 0), &[Bottom]), at(0, 3));
        assert_eq!(moved(at(3, 3), &[Halves(-2)]), at(3, 1));
    }

    #[test]
    fn jumps_find_a_character_on_the_line() {
        let grid = text();
        let mut mode = CopyMode::new(at(0, 0));
        mode.apply(&grid, Command::Jump(Jump { forward: true, till: false }));
        assert!(mode.awaits_jump());
        mode.jump_to(&grid, 'a');
        assert_eq!(mode.cursor(), at(5, 0));
        mode.apply(&grid, Command::Move(Motion::JumpAgain));
        assert_eq!(mode.cursor(), at(10, 0));
        mode.apply(&grid, Command::Move(Motion::JumpReverse));
        assert_eq!(mode.cursor(), at(5, 0));
        mode.apply(&grid, Command::Jump(Jump { forward: true, till: true }));
        mode.jump_to(&grid, 'z');
        assert_eq!(mode.cursor(), at(10, 0));
    }

    #[test]
    fn selections_follow_their_kind() {
        let grid = text();
        let mut mode = CopyMode::new(at(4, 1));
        assert_eq!(mode.selection(20), None);
        mode.apply(&grid, Command::Select(Kind::Cell));
        mode.apply(&grid, Command::Move(Motion::Up));
        assert_eq!(mode.selection(20), Some((at(4, 0), at(4, 1), false)));
        mode.apply(&grid, Command::Select(Kind::Line));
        assert_eq!(mode.selection(20), Some((at(0, 0), at(19, 1), false)));
        mode.apply(&grid, Command::OtherEnd);
        assert_eq!(mode.cursor(), at(4, 1));
        mode.apply(&grid, Command::Select(Kind::Line));
        assert_eq!(mode.selection(20), None, "the same kind again ends it");
    }
}
