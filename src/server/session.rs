//! The mux as a server sees it: requests and input in, events out. It keeps
//! what the attached UI was last told, so it sends only what changed, and
//! holds a pane's next update until the UI has drawn the last one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::config::{self, Config};
use crate::mux::{Layout, Mux, Pane, PaneId, Placement, WindowId};
use crate::proto::{
    Command, Event, Gesture, Hover, Input, Mode, ModeOp, PaneEntry, PaneOverlay, PaneUpdate, Program, Reply, TabState,
    WindowState,
};
use crate::pty::Spawn;
use crate::scan::{self, RowText};
use crate::terminal::{Geometry, KeyInput, MouseInput, Release};

/// A window the CLI opens while no other gives it a size.
const DEFAULT_SIZE: Geometry = Geometry { cols: 80, rows: 24, cell_width: 16, cell_height: 32 };

pub struct Session {
    mux: Mux,
    config: Config,
    config_path: Option<PathBuf>,
    /// What the attached UI was told; `None` while no UI is attached.
    ui: Option<Ui>,
    /// Events that are not state: the clipboard, notifications, links.
    queued: Vec<Event>,
    /// A window was open once; the server ends when the last one closes.
    opened: bool,
    killed: bool,
}

#[derive(Default)]
struct Ui {
    windows: BTreeMap<WindowId, WindowState>,
    /// The panes on screen, and what the UI has of them.
    panes: BTreeMap<PaneId, Shown>,
}

struct Shown {
    overlay: PaneOverlay,
    /// An update went out that the UI has not drawn yet.
    in_flight: bool,
}

impl Session {
    /// `wake` runs on another thread when output is waiting; the owner must
    /// then call [`Session::pump`]. Panes are told the server is at `socket`.
    pub fn new(wake: impl Fn() + Send + Sync + 'static, config_path: Option<PathBuf>, socket: &Path) -> Self {
        let config = load(config_path.as_deref());
        let env = vec![("SLUTTY_SOCKET".into(), socket.as_os_str().to_owned())];
        Self {
            mux: Mux::new(wake, env),
            config,
            config_path,
            ui: None,
            queued: Vec::new(),
            opened: false,
            killed: false,
        }
    }

    /// A UI attached: it hears about every window and pane from scratch.
    pub fn attach(&mut self) {
        self.ui = Some(Ui::default());
        self.queued.clear();
    }

    pub fn detach(&mut self) {
        self.ui = None;
        self.queued.clear();
    }

    /// The last window closed, or a client asked the server to stop.
    pub fn is_over(&self) -> bool {
        self.killed || (self.opened && self.mux.is_empty())
    }

    /// The soonest a synchronized-output hold expires; [`Session::events`]
    /// then has a frame to send.
    pub fn hold_deadline(&self) -> Option<Instant> {
        self.mux.hold_deadline()
    }

    /// Parses waiting output. Returns whether output is still queued.
    pub fn pump(&mut self) -> bool {
        let pump = self.mux.pump();
        if let Some(text) = pump.clipboard {
            self.queue(Event::Clipboard(text));
        }
        for (pane, notification) in pump.notifications {
            self.queue(Event::Notification { pane, notification });
        }
        pump.more
    }

    pub fn request(&mut self, command: Command) -> Result<Reply, String> {
        log::debug!("request {command:?}");
        let spawned = |window, pane| Reply::Spawned { window, pane };
        match command {
            Command::OpenFirstWindow { .. } if !self.mux.is_empty() => Ok(Reply::Done),
            Command::OpenWindow { program, size } => self.open_window(program, size),
            Command::OpenFirstWindow { program, size } => self.open_window(program, Some(size)),
            Command::List => Ok(Reply::List(self.list())),
            Command::GetText { pane, start, end } => Ok(Reply::Text(self.pane(pane)?.terminal().text(start, end))),
            Command::NewTab { window, program } => {
                let spawn = spawn(program, self.mux.active_cwd(window));
                let pane = self.mux.new_tab(window, &self.config, spawn).map_err(|e| e.to_string())?;
                Ok(spawned(window, pane))
            }
            Command::Split { pane, axis, percent, program } => {
                let share = percent.map_or(0.5, |p| f32::from(p.clamp(1, 99)) / 100.0);
                let spawn = spawn(program, self.mux.cwd(pane));
                let new = self.mux.split(pane, axis, share, &self.config, spawn).map_err(|e| e.to_string())?;
                let (window, _) = self.mux.find(new).ok_or("the split closed")?;
                Ok(spawned(window, new))
            }
            Command::CloseWindow { window } => {
                self.mux.close_window(window);
                Ok(Reply::Done)
            }
            Command::CloseTab { window, confirm } => {
                if confirm && self.mux.active_tab_busy(window) {
                    return Ok(Reply::Busy);
                }
                self.mux.close_tab(window);
                Ok(Reply::Done)
            }
            Command::ClosePane { pane, confirm } => {
                self.pane(pane)?;
                if confirm && self.mux.pane_busy(pane) {
                    return Ok(Reply::Busy);
                }
                self.mux.close_pane(pane);
                Ok(Reply::Done)
            }
            Command::ActivateTab { window, index } => {
                self.mux.activate_tab(window, index);
                Ok(Reply::Done)
            }
            Command::ActivatePane { pane } => {
                self.mux.reveal(pane).ok_or_else(|| no_pane(pane))?;
                Ok(Reply::Done)
            }
            Command::ActivateDirection { pane, direction } => {
                self.pane(pane)?;
                self.mux.focus_direction(pane, direction);
                Ok(Reply::Done)
            }
            Command::ResizePane { pane, direction, cells } => {
                self.pane(pane)?;
                self.mux.resize_pane(pane, direction, cells);
                Ok(Reply::Done)
            }
            Command::ToggleZoom { pane } => {
                self.pane(pane)?;
                self.mux.toggle_zoom(pane);
                Ok(Reply::Done)
            }
            Command::RotatePanes { window } => {
                self.mux.rotate_panes(window);
                Ok(Reply::Done)
            }
            Command::PopPane { pane } => {
                self.pane(pane)?;
                self.mux.pop_pane(pane);
                Ok(Reply::Done)
            }
            Command::PopTab { window } => {
                self.mux.pop_tab(window);
                Ok(Reply::Done)
            }
            Command::CopyMode { pane } => {
                self.pane(pane)?.start_copy_mode();
                Ok(Reply::Done)
            }
            Command::Search { pane } => {
                let p = self.pane(pane)?;
                // As WezTerm: start from the selected text.
                let selected = p.terminal().selection_text().unwrap_or_default();
                p.start_search(selected.lines().next().unwrap_or_default());
                Ok(Reply::Done)
            }
            Command::Copy { pane } => {
                if let Some(text) = self.pane(pane)?.terminal().selection_text() {
                    self.queue(Event::Clipboard(text));
                }
                Ok(Reply::Done)
            }
            Command::Paste { pane, text } => {
                let p = self.pane(pane)?;
                if !p.paste_into_search(&text) {
                    p.paste(&text);
                }
                Ok(Reply::Done)
            }
            Command::SendText { pane, text, paste } => {
                let p = self.pane(pane)?;
                if paste {
                    p.paste(&text);
                } else {
                    p.send(text.as_bytes());
                }
                Ok(Reply::Done)
            }
            Command::KillServer => {
                self.killed = true;
                Ok(Reply::Done)
            }
            Command::ReloadConfig => {
                let config = load(self.config_path.as_deref());
                if config != self.config {
                    self.mux.configure(&config);
                    self.config = config;
                }
                Ok(Reply::Done)
            }
        }
    }

    pub fn input(&mut self, input: Input) {
        match input {
            Input::Key { pane, key } => {
                let input = KeyInput {
                    key: key.key,
                    action: key.action,
                    mods: key.mods,
                    consumed: key.consumed,
                    text: key.text.as_deref(),
                    unshifted: key.unshifted,
                };
                self.with_pane(pane, |p| p.key(&input));
            }
            Input::Mouse { pane, mouse } => self.with_pane(pane, |p| {
                if p.mouse_tracking() {
                    let (action, button, mods, x, y) = (mouse.action, mouse.button, mouse.mods, mouse.x, mouse.y);
                    p.mouse(&MouseInput { action, button, mods, x, y });
                }
            }),
            Input::Scroll { pane, lines, mods, x, y } => {
                self.with_pane(pane, |p| p.scroll(lines as isize, mods, x, y));
            }
            Input::Focus { window, focused } => self.mux.focus(window, focused),
            Input::Resize { window, size } => self.mux.resize(window, size),
            Input::Select { pane, gesture } => self.select(pane, gesture),
            Input::Mode { pane, op } => self.mode(pane, op),
            Input::Hover { pane, col, row } => {
                let hover = self.link_at(pane, col, row);
                self.queue(Event::Hover(hover));
            }
            Input::Drawn { panes } => {
                let Some(ui) = &mut self.ui else { return };
                for pane in panes {
                    if let Some(shown) = ui.panes.get_mut(&pane) {
                        shown.in_flight = false;
                    }
                }
            }
        }
    }

    /// What the UI should hear: queued events, then the windows and the
    /// panes on screen that changed since it was last told.
    pub fn events(&mut self) -> Vec<Event> {
        let Some(ui) = &mut self.ui else { return Vec::new() };
        let mut events = std::mem::take(&mut self.queued);
        let ids = self.mux.window_ids();
        ui.windows.retain(|id, _| {
            let open = ids.contains(id);
            if !open {
                events.push(Event::WindowClosed(*id));
            }
            open
        });
        for id in ids {
            let Some(state) = window_state(&self.mux, id) else { continue };
            if ui.windows.get(&id) != Some(&state) {
                ui.windows.insert(id, state.clone());
                events.push(Event::Window(state));
            }
        }
        let visible: Vec<PaneId> = ui.windows.values().flat_map(|w| w.panes.iter().map(|(id, _)| *id)).collect();
        ui.panes.retain(|id, _| visible.contains(id));
        for id in visible {
            let shown = ui.panes.get(&id);
            if shown.is_some_and(|s| s.in_flight) {
                continue;
            }
            let Some(pane) = self.mux.pane_mut(id) else { continue };
            let overlay = overlay(pane);
            let full = shown.is_none();
            let unchanged_overlay = shown.is_some_and(|s| s.overlay == overlay);
            if let Some(update) = update(id, pane, full, unchanged_overlay, overlay.clone()) {
                ui.panes.insert(id, Shown { overlay, in_flight: true });
                events.push(Event::Pane(update));
            }
        }
        events
    }

    fn open_window(&mut self, program: Program, size: Option<Geometry>) -> Result<Reply, String> {
        let size = size.or_else(|| self.mux.usual_size()).unwrap_or(DEFAULT_SIZE);
        let (window, pane) =
            self.mux.open_window(size, &self.config, spawn(program, None)).map_err(|e| e.to_string())?;
        self.opened = true;
        Ok(Reply::Spawned { window, pane })
    }

    fn list(&self) -> Vec<PaneEntry> {
        let focused = self.mux.focused_pane();
        let entry = |p: Placement| {
            let info = self.mux.info(p.pane).cloned().unwrap_or_default();
            PaneEntry {
                window_id: p.window,
                tab_index: p.tab,
                pane_id: p.pane,
                x: p.rect.x,
                y: p.rect.y,
                cols: p.rect.cols,
                rows: p.rect.rows,
                title: info.title().to_owned(),
                process: info.process.as_ref().map(|p| p.name().to_owned()),
                cwd: info.cwd,
                rho: info.rho,
                active: p.active,
                focused: focused == Some(p.pane),
                zoomed: p.zoomed,
            }
        };
        self.mux.placements().into_iter().map(entry).collect()
    }

    fn select(&mut self, pane: PaneId, gesture: Gesture) {
        let Some(p) = self.mux.pane_mut(pane) else { return };
        let terminal = p.terminal_mut();
        let at = match gesture {
            Gesture::Press { at, extend } => return terminal.select_press(at, extend),
            Gesture::Drag { at, rectangle } => {
                terminal.select_drag(at, rectangle);
                return;
            }
            Gesture::Autoscroll { at, rectangle } => {
                terminal.select_autoscroll(at, rectangle);
                return;
            }
            Gesture::Release { at } => at,
        };
        let event = match terminal.select_release(at) {
            Release::Selected => terminal.selection_text().map(Event::Clipboard),
            Release::Clicked => self.link_at(pane, at.col, at.row).map(|link| Event::OpenUrl(link.url)),
        };
        if let Some(event) = event {
            self.queue(event);
        }
    }

    fn mode(&mut self, pane: PaneId, op: ModeOp) {
        let Some(p) = self.mux.pane_mut(pane) else { return };
        match op {
            ModeOp::Copy(command) => p.copy_mode(command),
            ModeOp::CopyJump(target) => p.copy_mode_jump(target),
            ModeOp::CopyClose { copy } => {
                if let Some(text) = p.close_copy_mode(copy) {
                    self.queue(Event::Clipboard(text));
                }
            }
            ModeOp::SearchType(c) => p.edit_search(|needle| needle.push(c)),
            ModeOp::SearchBackspace => p.edit_search(|needle| _ = needle.pop()),
            ModeOp::SearchClear => p.edit_search(String::clear),
            ModeOp::SearchStep { older } => p.search_step(older),
            ModeOp::SearchToggleRegex => p.toggle_search_regex(),
            ModeOp::SearchClose => p.close_search(),
        }
    }

    /// The link over a viewport cell: an OSC 8 link, else a URL in the text.
    fn link_at(&mut self, pane: PaneId, col: u16, row: u16) -> Option<Hover> {
        let p = self.mux.pane_mut(pane)?;
        if let Some((start, end, url)) = p.terminal().hyperlink_at(col, row) {
            return Some(Hover { pane, row, start, end, url });
        }
        let text = RowText::new(p.frame().rows.get(usize::from(row))?);
        let span = scan::link_at(row, &text, col)?;
        Some(Hover { pane, row, start: span.start, end: span.end, url: span.text })
    }

    fn pane(&mut self, pane: PaneId) -> Result<&mut Pane, String> {
        self.mux.pane_mut(pane).ok_or_else(|| no_pane(pane))
    }

    fn with_pane(&mut self, pane: PaneId, f: impl FnOnce(&mut Pane)) {
        if let Some(p) = self.mux.pane_mut(pane) {
            f(p);
        }
    }

    fn queue(&mut self, event: Event) {
        if self.ui.is_some() {
            self.queued.push(event);
        }
    }
}

/// The config file, or the defaults when there is none or it is broken; the
/// UI shows why.
fn load(path: Option<&std::path::Path>) -> Config {
    path.map_or_else(|| Ok(Config::default()), config::load).unwrap_or_else(|err| {
        log::warn!("config: {err}");
        Config::default()
    })
}

fn no_pane(pane: PaneId) -> String {
    format!("no pane {pane}")
}

/// What to start for `program`, in `cwd` unless it names a directory.
fn spawn(program: Program, cwd: Option<PathBuf>) -> Spawn {
    let cwd = program.cwd.or(cwd);
    match program.command {
        None => Spawn::login_shell(cwd.as_deref()),
        Some((path, args)) => Spawn { cwd, ..Spawn::command(path, args) },
    }
}

fn window_state(mux: &Mux, id: WindowId) -> Option<WindowState> {
    let tabs = mux.tabs(id).into_iter().map(|tab| TabState { info: tab.info.clone(), panes: tab.panes }).collect();
    let Layout { panes, dividers } = mux.layout(id);
    Some(WindowState {
        id,
        size: mux.size(id)?,
        tabs,
        active_tab: mux.active_tab(id)?,
        panes,
        dividers,
        active_pane: mux.active_pane(id)?,
    })
}

fn overlay(pane: &mut Pane) -> PaneOverlay {
    let mode = match pane.search_status() {
        _ if pane.in_copy_mode() => Mode::Copy,
        Some(status) => Mode::Search(status),
        None => Mode::None,
    };
    PaneOverlay {
        mode,
        highlights: pane.search_highlights(),
        copy_cursor: pane.copy_cursor(),
        mouse_tracking: pane.mouse_tracking(),
    }
}

/// The rows that changed in `pane`, or all of them when `full`; `None`
/// when nothing did.
fn update(id: PaneId, pane: &mut Pane, full: bool, same_overlay: bool, overlay: PaneOverlay) -> Option<PaneUpdate> {
    let mut frame = pane.frame();
    if !full && !frame.changed && same_overlay {
        return None;
    }
    let changed = frame
        .rows
        .iter_mut()
        .enumerate()
        .filter(|(_, row)| full || row.dirty)
        .map(|(y, row)| {
            row.dirty = false;
            (u16::try_from(y).unwrap_or(u16::MAX), row.clone())
        })
        .collect();
    frame.changed = false;
    Some(PaneUpdate {
        pane: id,
        cols: frame.cols,
        rows: u16::try_from(frame.rows.len()).unwrap_or(u16::MAX),
        background: frame.background,
        foreground: frame.foreground,
        cursor: frame.cursor,
        changed,
        overlay,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::terminal::{Geometry, Grapheme};

    const SIZE: Geometry = Geometry { cols: 40, rows: 5, cell_width: 8, cell_height: 16 };

    fn sh(script: &str) -> Program {
        Program { command: Some(("/bin/sh".into(), vec!["-c".into(), script.into()])), cwd: None }
    }

    fn text(update: &PaneUpdate) -> String {
        let cell = |c: &crate::terminal::Cell| match &c.text {
            Grapheme::Char(c) => c.to_string(),
            Grapheme::Cluster(s) => s.to_string(),
            Grapheme::Empty => " ".into(),
        };
        update.changed.iter().map(|(_, row)| row.cells.iter().map(cell).collect::<String>()).collect()
    }

    /// Pumps until an update for `pane` holds `needle`.
    fn wait_for(session: &mut Session, pane: PaneId, needle: &str) -> PaneUpdate {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            session.pump();
            for event in session.events() {
                if let Event::Pane(update) = event
                    && update.pane == pane
                {
                    session.input(Input::Drawn { panes: vec![pane] });
                    if text(&update).contains(needle) {
                        return update;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("no update with {needle:?}");
    }

    fn open(session: &mut Session, program: Program) -> (WindowId, PaneId) {
        match session.request(Command::OpenWindow { program, size: Some(SIZE) }) {
            Ok(Reply::Spawned { window, pane }) => (window, pane),
            other => panic!("opening a window: {other:?}"),
        }
    }

    #[test]
    fn a_ui_hears_of_windows_and_gets_output_one_update_at_a_time() {
        let mut session = Session::new(|| {}, None, Path::new("/tmp/slutty-test.sock"));
        session.attach();
        let (window, pane) = open(&mut session, sh("printf ready; read line; printf \"got $line\"; sleep 5"));
        let events = session.events();
        assert!(events.iter().any(|e| matches!(e, Event::Window(w) if w.id == window && w.active_pane == pane)));
        session.input(Input::Drawn { panes: vec![pane] });
        wait_for(&mut session, pane, "ready");

        // An update not yet drawn holds back the next.
        session.request(Command::SendText { pane, text: "one\r".into(), paste: false }).expect("sent");
        let start = Instant::now();
        let mut held = None;
        while start.elapsed() < Duration::from_millis(500) {
            session.pump();
            for event in session.events() {
                if let Event::Pane(update) = event {
                    assert!(held.is_none(), "one update until the UI draws");
                    held = Some(update);
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let held = held.expect("an update");
        session.input(Input::Drawn { panes: vec![pane] });
        if !text(&held).contains("got one") {
            wait_for(&mut session, pane, "got one");
        }
    }

    #[test]
    fn a_new_ui_gets_every_row_again() {
        let mut session = Session::new(|| {}, None, Path::new("/tmp/slutty-test.sock"));
        session.attach();
        let (_, pane) = open(&mut session, sh("printf marker; sleep 5"));
        wait_for(&mut session, pane, "marker");
        session.attach();
        let update = wait_for(&mut session, pane, "marker");
        assert_eq!(update.changed.len(), usize::from(SIZE.rows), "a full update");
    }

    #[test]
    fn closing_asks_first_when_more_than_a_shell_runs() {
        let mut session = Session::new(|| {}, None, Path::new("/tmp/slutty-test.sock"));
        session.attach();
        let (window, pane) = open(&mut session, sh("printf ready; exec sleep 5"));
        wait_for(&mut session, pane, "ready");
        // The foreground program is sleep, not a shell.
        let start = Instant::now();
        while session.request(Command::ClosePane { pane, confirm: true }) != Ok(Reply::Busy) {
            assert!(start.elapsed() < Duration::from_secs(5), "sleep never ran");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(session.request(Command::ClosePane { pane, confirm: false }), Ok(Reply::Done));
        assert!(session.events().contains(&Event::WindowClosed(window)));
        assert!(session.request(Command::CopyMode { pane }).is_err(), "the pane is gone");
    }
}
