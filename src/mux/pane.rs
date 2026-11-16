//! One terminal and the program running in it.

use std::cell::RefMut;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use libghostty_vt::key::Mods;

use crate::config::Config;
use crate::pty::{Process, Pty, ReadStatus, Spawn, WindowSize};
use crate::terminal::{Frame, Geometry, KeyInput, MouseInput, Terminal};

/// rho sets this user variable to `1` once it has printed its transcript
/// onto the normal screen for copy mode, and back to `0` when it wipes it.
const SCROLLBACK: &str = "rho_scrollback";

/// rho's key for the scrollback handoff: it prints its transcript, or
/// brings its own screen back.
const HANDOFF: &[u8] = b"\x13";

/// Output parsed per wake. More waits for the next wake, so a flood of
/// output cannot starve input and drawing.
const PUMP_BUDGET: usize = 1024 * 1024;

pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// What the tab bar and the window title show of a pane.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaneInfo {
    /// The title the program set, if any.
    pub title: String,
    pub cwd: Option<PathBuf>,
    pub process: Option<Process>,
    /// What rho reports it is doing, while it is the foreground program.
    pub rho: Option<RhoActivity>,
}

/// rho's state, from its `rho_activity` user variable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RhoActivity {
    Working,
    Question,
    Stopped,
    Unread,
}

impl RhoActivity {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "working" => Self::Working,
            "question" => Self::Question,
            "stopped" => Self::Stopped,
            "unread" => Self::Unread,
            _ => return None,
        })
    }
}

impl PaneInfo {
    /// The program's title, else the foreground program's name.
    pub fn title(&self) -> &str {
        if self.title.is_empty() { self.process.as_ref().map_or("", Process::name) } else { &self.title }
    }
}

/// What a pump found in one pane.
#[derive(Default)]
pub(super) struct Pumped {
    pub output: bool,
    pub more: bool,
    pub info_changed: bool,
    pub closed: bool,
    pub clipboard: Option<String>,
}

pub struct Pane {
    pub(super) terminal: Terminal,
    pty: Pty,
    closed: bool,
    info: PaneInfo,
    /// The directory the program reported (OSC 7); the process's own cwd is
    /// used until it does.
    reported_cwd: Option<PathBuf>,
    /// Variables the program set with OSC 1337 SetUserVar.
    user_vars: BTreeMap<String, String>,
}

impl Pane {
    /// Starts the login shell in `cwd`, or in the home directory.
    pub(super) fn spawn(geometry: Geometry, config: &Config, cwd: Option<&Path>, wake: &Wake) -> io::Result<Self> {
        let terminal = Terminal::new(geometry, config).map_err(io::Error::other)?;
        let mut spawn = Spawn::login_shell();
        spawn.env = shell_env();
        if let Some(cwd) = cwd.filter(|dir| dir.is_dir()) {
            spawn.cwd = Some(cwd.to_path_buf());
        }
        let wake = Arc::clone(wake);
        let pty = Pty::spawn(&spawn, window_size(geometry), move || wake())?;
        let mut pane = Self {
            terminal,
            pty,
            closed: false,
            info: PaneInfo::default(),
            reported_cwd: None,
            user_vars: BTreeMap::new(),
        };
        pane.refresh_info();
        Ok(pane)
    }

    pub(super) fn pump(&mut self, buf: &mut Vec<u8>) -> Pumped {
        let mut pumped = Pumped::default();
        if self.closed {
            return pumped;
        }
        buf.clear();
        let status = self.pty.read(buf, PUMP_BUDGET);
        if !buf.is_empty() {
            let vars = self.terminal.write(buf);
            let printed = vars.iter().any(|(name, value)| name == SCROLLBACK && value == "1");
            self.user_vars.extend(vars);
            pumped.output = true;
            pumped.info_changed = self.refresh_info();
            if printed && self.shows_rho_scrollback() {
                self.terminal.close_search();
                self.terminal.start_copy_mode();
            }
        }
        self.flush();
        pumped.clipboard = self.terminal.take_clipboard();
        match status {
            ReadStatus::Drained => {}
            ReadStatus::More => pumped.more = true,
            ReadStatus::Closed => {
                self.closed = true;
                pumped.closed = true;
            }
        }
        pumped
    }

    /// Reads the title, directory and foreground program again. Returns
    /// whether any changed.
    fn refresh_info(&mut self) -> bool {
        if let Some(cwd) = self.terminal.take_pwd() {
            self.reported_cwd = Some(cwd);
        }
        let mut info =
            PaneInfo { title: self.info.title.clone(), cwd: None, process: self.pty.foreground(), rho: None };
        if let Some(title) = self.terminal.take_title() {
            info.title = title;
        }
        info.cwd = self.reported_cwd.clone().or_else(|| info.process.as_ref().and_then(|p| p.cwd.clone()));
        // A rho that died leaves its last state behind; only a running one counts.
        if runs_rho(info.process.as_ref()) {
            info.rho = self.user_vars.get("rho_activity").and_then(|v| RhoActivity::parse(v));
        }
        if info == self.info {
            return false;
        }
        self.info = info;
        true
    }

    /// For selecting and reading text, which sends nothing to the program.
    pub fn terminal(&self) -> &Terminal {
        &self.terminal
    }

    pub fn terminal_mut(&mut self) -> &mut Terminal {
        &mut self.terminal
    }

    pub fn info(&self) -> &PaneInfo {
        &self.info
    }

    /// Whether rho runs here.
    pub(super) fn runs_rho(&self) -> bool {
        runs_rho(self.info.process.as_ref())
    }

    /// Whether rho runs here with its transcript printed for copy mode.
    pub(super) fn shows_rho_scrollback(&self) -> bool {
        self.runs_rho() && self.user_vars.get(SCROLLBACK).is_some_and(|v| v == "1")
    }

    /// Asks rho to print its transcript, or to bring its screen back.
    pub(super) fn rho_handoff(&mut self) {
        self.send(HANDOFF);
    }

    /// Whether closing the pane would stop something other than a shell.
    pub fn is_busy(&self) -> bool {
        self.pty.foreground().is_some_and(|p| !p.is_shell())
    }

    pub fn frame(&mut self) -> RefMut<'_, Frame> {
        self.terminal.frame()
    }

    /// When a synchronized-output hold expires and a frame is due.
    pub fn hold_deadline(&self) -> Option<Instant> {
        self.terminal.hold_deadline()
    }

    pub(super) fn resize(&mut self, geometry: Geometry) {
        if geometry == self.terminal.geometry() {
            return;
        }
        if let Err(err) = self.terminal.resize(geometry) {
            log::warn!("terminal resize failed: {err}");
        }
        if let Err(err) = self.pty.resize(window_size(geometry)) {
            log::warn!("pty resize failed: {err}");
        }
    }

    pub(super) fn configure(&mut self, config: &Config) {
        if let Err(err) = self.terminal.configure(config) {
            log::warn!("applying the config failed: {err}");
        }
    }

    pub fn key(&mut self, input: &KeyInput<'_>) {
        self.input(|t| t.key(input));
    }

    pub fn paste(&mut self, text: &str) {
        self.input(|t| t.paste(text));
    }

    /// Sends bytes to the program as if typed.
    pub fn send(&mut self, data: &[u8]) {
        self.input(|t| {
            t.send(data);
            Ok(())
        });
    }

    pub(super) fn focus(&mut self, focused: bool) {
        self.input(|t| t.focus(focused));
    }

    /// Whether the program wants mouse events.
    pub fn mouse_tracking(&self) -> bool {
        self.terminal.mouse_tracking()
    }

    /// Positions are device pixels from the pane's top-left corner.
    pub fn mouse(&mut self, input: &MouseInput) {
        self.input(|t| t.mouse(input));
    }

    pub fn scroll(&mut self, lines: isize, mods: Mods, x: f32, y: f32) {
        self.input(|t| t.scroll(lines, mods, x, y));
    }

    fn input(&mut self, f: impl FnOnce(&mut Terminal) -> libghostty_vt::error::Result<()>) {
        if let Err(err) = f(&mut self.terminal) {
            log::warn!("encoding input failed: {err}");
        }
        self.flush();
    }

    fn flush(&mut self) {
        self.pty.write(&self.terminal.take_replies());
    }
}

/// The process check matters: a rho that died leaves its variables set.
fn runs_rho(process: Option<&Process>) -> bool {
    process.is_some_and(|p| p.name() == "rho")
}

fn window_size(g: Geometry) -> WindowSize {
    let px = |v: u32| u16::try_from(v).unwrap_or(u16::MAX);
    WindowSize { cols: g.cols, rows: g.rows, cell_width: px(g.cell_width), cell_height: px(g.cell_height) }
}

/// The environment the shell starts with, on top of ours.
fn shell_env() -> Vec<(OsString, OsString)> {
    let mut env = vec![
        ("TERM".into(), "xterm-256color".into()),
        ("COLORTERM".into(), "truecolor".into()),
        ("TERM_PROGRAM".into(), "slutty".into()),
        ("TERM_PROGRAM_VERSION".into(), env!("CARGO_PKG_VERSION").into()),
    ];
    // Apps started from the Dock have no locale, and shells then fall back
    // to ASCII.
    if std::env::var_os("LANG").is_none() {
        env.push(("LANG".into(), "en_US.UTF-8".into()));
    }
    env
}
