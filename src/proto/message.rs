//! What clients and the server say to each other. The UI and `slutty cli`
//! share these: a key binding and a CLI command send the same [`Command`].

use std::ffi::OsString;
use std::path::PathBuf;

use libghostty_vt::color::Rgb;
use libghostty_vt::key::{self, Key, Mods};
use libghostty_vt::mouse;
use serde::{Deserialize, Serialize};

use super::wire;
use crate::mux::{Axis, Direction, PaneId, PaneInfo, Rect, RhoActivity, WindowId};
use crate::terminal::copy_mode;
use crate::terminal::{Cursor, Geometry, Highlight, Notification, Pointer, Row, SearchStatus};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ClientMessage {
    /// The first message, once: who is asking.
    Hello { role: Role },
    /// Answered by a [`ServerMessage::Response`] with the same id.
    Request { id: u32, command: Command },
    /// The UI's input; never answered.
    Input(Input),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    /// Hears every event. One at a time: a new one detaches the last.
    Ui,
    /// Only sends requests.
    Cli,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ServerMessage {
    Response {
        id: u32,
        result: Result<Reply, String>,
    },
    /// Sent to the UI only.
    Event(Event),
}

/// A program to start. No command runs the login shell. No directory starts
/// in the directory of the pane it is opened from, else in the home.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Program {
    pub command: Option<(PathBuf, Vec<OsString>)>,
    pub cwd: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Command {
    /// Opens a window running `program`, with a grid of `size`; with none,
    /// of the size of the window focused last, else 80×24.
    OpenWindow {
        program: Program,
        size: Option<Geometry>,
    },
    /// As [`Command::OpenWindow`] when no window is open; else
    /// [`Reply::Done`].
    OpenFirstWindow {
        program: Program,
        size: Geometry,
    },
    /// Opens a tab after the others.
    NewTab {
        window: WindowId,
        program: Program,
    },
    /// Splits `pane`; the new pane takes `percent` of the space, else half,
    /// and becomes active.
    Split {
        pane: PaneId,
        axis: Axis,
        percent: Option<u8>,
        program: Program,
    },
    CloseWindow {
        window: WindowId,
    },
    /// Closes the window's active tab. With `confirm`, a tab running more
    /// than shells is not closed and the reply is [`Reply::Busy`].
    CloseTab {
        window: WindowId,
        confirm: bool,
    },
    /// As [`Command::CloseTab`], for one pane.
    ClosePane {
        pane: PaneId,
        confirm: bool,
    },
    /// Counts from 0.
    ActivateTab {
        window: WindowId,
        index: usize,
    },
    /// Shows `pane` and gives it the keys: its tab becomes active, out of a
    /// zoom that hid it.
    ActivatePane {
        pane: PaneId,
    },
    ActivateDirection {
        pane: PaneId,
        direction: Direction,
    },
    /// Moves the divider nearest `pane`.
    ResizePane {
        pane: PaneId,
        direction: Direction,
        cells: u16,
    },
    ToggleZoom {
        pane: PaneId,
    },
    RotatePanes {
        window: WindowId,
    },
    /// Moves `pane` into a new window, or its tab when it is alone there.
    PopPane {
        pane: PaneId,
    },
    /// Moves the window's active tab into a new window.
    PopTab {
        window: WindowId,
    },
    CopyMode {
        pane: PaneId,
    },
    /// Opens search, starting from the selected text.
    Search {
        pane: PaneId,
    },
    /// Puts the selection on the clipboard, through [`Event::Clipboard`].
    Copy {
        pane: PaneId,
    },
    /// Pastes into the program, or into the search while it is open.
    Paste {
        pane: PaneId,
        text: String,
    },
    /// Sends text as typed, or as a paste.
    SendText {
        pane: PaneId,
        text: String,
        paste: bool,
    },
    /// Every pane: [`Reply::List`].
    List,
    /// Lines of `pane` as plain text: [`Reply::Text`]. Line 0 is the top row
    /// of the screen and negative lines are scrollback; no `end` is the
    /// bottom row.
    GetText {
        pane: PaneId,
        start: i32,
        end: Option<i32>,
    },
    /// Reads the config file again.
    ReloadConfig,
    /// Closes every pane and stops the server.
    KillServer,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Reply {
    Done,
    Spawned {
        window: WindowId,
        pane: PaneId,
    },
    /// Not done: something other than a shell runs there.
    Busy,
    List(Vec<PaneEntry>),
    Text(String),
}

/// A pane as `slutty cli list` shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneEntry {
    pub window_id: WindowId,
    /// Counts from 0.
    pub tab_index: usize,
    pub pane_id: PaneId,
    /// The pane's place in its tab, in cells. A pane hidden by a zoom keeps
    /// the place it had.
    pub x: u16,
    pub y: u16,
    pub cols: u16,
    pub rows: u16,
    pub title: String,
    pub cwd: Option<PathBuf>,
    /// The foreground program's name.
    pub process: Option<String>,
    pub rho: Option<RhoActivity>,
    /// The active pane of its tab.
    pub active: bool,
    /// The pane requests go to when they name none.
    pub focused: bool,
    /// Its tab is zoomed on it.
    pub zoomed: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Input {
    Key {
        pane: PaneId,
        key: KeyEvent,
    },
    Mouse {
        pane: PaneId,
        mouse: MouseEvent,
    },
    /// Positive lines scroll toward older output.
    Scroll {
        pane: PaneId,
        lines: i32,
        #[serde(with = "wire::mods")]
        mods: Mods,
        x: f32,
        y: f32,
    },
    Focus {
        window: WindowId,
        focused: bool,
    },
    Resize {
        window: WindowId,
        size: Geometry,
    },
    Select {
        pane: PaneId,
        gesture: Gesture,
    },
    Mode {
        pane: PaneId,
        op: ModeOp,
    },
    /// The pointer is over a cell; [`Event::Hover`] says what link is there.
    Hover {
        pane: PaneId,
        col: u16,
        row: u16,
    },
    /// The UI drew these panes' last updates and takes the next ones.
    Drawn {
        panes: Vec<PaneId>,
    },
}

/// A key event as the platform read it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeyEvent {
    #[serde(with = "wire::key")]
    pub key: Key,
    #[serde(with = "wire::key_action")]
    pub action: key::Action,
    #[serde(with = "wire::mods")]
    pub mods: Mods,
    #[serde(with = "wire::mods")]
    pub consumed: Mods,
    pub text: Option<String>,
    pub unshifted: Option<char>,
}

/// A mouse event for a program that tracks the mouse. Positions are device
/// pixels from the pane's top-left corner.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MouseEvent {
    #[serde(with = "wire::mouse_action")]
    pub action: mouse::Action,
    #[serde(with = "wire::opt_mouse_button")]
    pub button: Option<mouse::Button>,
    #[serde(with = "wire::mods")]
    pub mods: Mods,
    pub x: f32,
    pub y: f32,
}

/// Selecting text with the left button.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Gesture {
    /// `extend` (shift) moves the end of the selection instead.
    Press {
        at: Pointer,
        extend: bool,
    },
    Drag {
        at: Pointer,
        rectangle: bool,
    },
    /// The drag stays past an edge: scroll a row and extend.
    Autoscroll {
        at: Pointer,
        rectangle: bool,
    },
    /// Copies what a drag or a multiple click chose, or opens the link
    /// under a plain click.
    Release {
        at: Pointer,
    },
}

/// A key in copy mode or search, as the UI read it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum ModeOp {
    Copy(copy_mode::Command),
    /// The character an `f` or `t` jump goes to.
    CopyJump(char),
    /// Closes copy mode, copying the selection when `copy` is set.
    CopyClose {
        copy: bool,
    },
    SearchType(char),
    SearchBackspace,
    SearchClear,
    SearchStep {
        older: bool,
    },
    SearchToggleRegex,
    SearchClose,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Event {
    /// A window as it is now; the first for an id opens it.
    Window(WindowState),
    WindowClosed(WindowId),
    Pane(PaneUpdate),
    Clipboard(String),
    Notification {
        pane: PaneId,
        notification: Notification,
    },
    OpenUrl(String),
    Hover(Option<Hover>),
    /// A newer UI attached; this one hears nothing more.
    Detached,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    pub id: WindowId,
    /// The pane area.
    pub size: Geometry,
    pub tabs: Vec<TabState>,
    pub active_tab: usize,
    /// The active tab's panes on screen, with their place in cells.
    pub panes: Vec<(PaneId, Rect)>,
    pub dividers: Vec<(Axis, Rect)>,
    /// The pane keys go to.
    pub active_pane: PaneId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TabState {
    /// The tab's active pane, as the tab bar shows it.
    pub info: PaneInfo,
    pub panes: Vec<PaneId>,
}

/// What changed in a pane's screen since its last update. The first update
/// after a pane shows carries every row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PaneUpdate {
    pub pane: PaneId,
    pub cols: u16,
    pub rows: u16,
    #[serde(with = "wire::rgb")]
    pub background: Rgb,
    #[serde(with = "wire::rgb")]
    pub foreground: Rgb,
    pub cursor: Option<Cursor>,
    pub changed: Vec<(u16, Row)>,
    pub overlay: PaneOverlay,
}

/// What a pane shows over its cells, and how it takes input.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PaneOverlay {
    pub mode: Mode,
    pub highlights: Vec<Highlight>,
    pub copy_cursor: Option<(u16, u16)>,
    /// The program wants mouse events.
    pub mouse_tracking: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum Mode {
    #[default]
    None,
    Copy,
    Search(SearchStatus),
}

/// A link under the pointer: cells `start..end` of a viewport row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hover {
    pub pane: PaneId,
    pub row: u16,
    pub start: u16,
    pub end: u16,
    pub url: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{Cell, CellStyle, CursorShape, Grapheme};

    fn round_trip<T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug>(value: T) {
        let bytes = postcard::to_stdvec(&value).expect("encodes");
        assert_eq!(postcard::from_bytes::<T>(&bytes).expect("decodes"), value);
    }

    #[test]
    fn messages_survive_the_wire() {
        let pane = PaneId::for_test(7);
        let red = Rgb { r: 255, g: 0, b: 0 };
        round_trip(ServerMessage::Event(Event::Pane(PaneUpdate {
            pane,
            cols: 2,
            rows: 1,
            background: red,
            foreground: red,
            cursor: Some(Cursor { x: 1, y: 0, shape: CursorShape::Bar, wide: false, color: None, blinking: true }),
            changed: vec![(
                0,
                Row {
                    cells: vec![
                        Cell {
                            text: Grapheme::Cluster("e\u{301}".into()),
                            fg: red,
                            bg: None,
                            style: CellStyle::default(),
                        },
                        Cell { text: Grapheme::Char('x'), fg: red, bg: Some(red), style: CellStyle::default() },
                    ],
                    dirty: false,
                },
            )],
            overlay: PaneOverlay { mode: Mode::Search(SearchStatus::default()), ..PaneOverlay::default() },
        })));
        round_trip(Input::Key {
            pane,
            key: KeyEvent {
                key: Key::Comma,
                action: key::Action::Repeat,
                mods: Mods::SHIFT | Mods::ALT,
                consumed: Mods::SHIFT,
                text: Some("A".into()),
                unshifted: Some('a'),
            },
        });
        round_trip(Input::Mouse {
            pane,
            mouse: MouseEvent {
                action: mouse::Action::Press,
                button: Some(mouse::Button::Right),
                mods: Mods::empty(),
                x: 1.5,
                y: 2.0,
            },
        });
        round_trip(Command::Split {
            pane,
            axis: Axis::Column,
            percent: Some(30),
            program: Program { command: Some(("vim".into(), vec!["a b".into()])), cwd: Some("/tmp".into()) },
        });
        round_trip(ServerMessage::Response { id: 3, result: Err("no pane 7".into()) });
    }

    #[test]
    fn an_unknown_c_enum_value_is_an_error() {
        // A mouse action of 99 has no meaning; it must not decode as anything.
        let bytes = postcard::to_stdvec(&(99i32, Option::<i32>::None, 0u16, 0f32, 0f32)).expect("encodes");
        assert!(postcard::from_bytes::<MouseEvent>(&bytes).is_err());
    }
}
