//! The model: panes, their terminals and child processes. No platform types,
//! so it can later move to its own thread or process (docs/design.md).
//!
//! The MVP has one pane; tabs and splits extend this in later milestones.

use std::cell::RefMut;
use std::ffi::OsString;
use std::io;
use std::time::Instant;

use libghostty_vt::key::Mods;

use crate::config::Config;
use crate::pty::{Pty, ReadStatus, Spawn, WindowSize};
use crate::terminal::{Frame, Geometry, KeyInput, MouseInput, Terminal};

/// Output parsed per wake. More waits for the next wake, so a flood of
/// output cannot starve input and drawing.
const PUMP_BUDGET: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    TitleChanged(String),
    /// The pane's program exited and closed its terminal.
    PaneClosed,
}

#[derive(Debug, Default)]
pub struct Pump {
    /// New output was parsed; a frame may be due.
    pub output: bool,
    /// Output is still queued; pump again soon.
    pub more: bool,
    pub events: Vec<Event>,
}

struct Pane {
    terminal: Terminal,
    pty: Pty,
    closed: bool,
}

pub struct Mux {
    pane: Pane,
    buf: Vec<u8>,
}

impl Mux {
    /// Starts the login shell. `wake` runs on another thread when output is
    /// waiting; the owner must then call [`Mux::pump`] on this thread.
    pub fn spawn(geometry: Geometry, config: &Config, wake: impl Fn() + Send + 'static) -> io::Result<Self> {
        let terminal = Terminal::new(geometry, config).map_err(io::Error::other)?;
        let mut spawn = Spawn::login_shell();
        spawn.env = shell_env();
        let pty = Pty::spawn(&spawn, window_size(geometry), wake)?;
        Ok(Self { pane: Pane { terminal, pty, closed: false }, buf: Vec::new() })
    }

    pub fn pump(&mut self) -> Pump {
        let mut pump = Pump::default();
        let pane = &mut self.pane;
        if pane.closed {
            return pump;
        }
        self.buf.clear();
        let status = pane.pty.read(&mut self.buf, PUMP_BUDGET);
        if !self.buf.is_empty() {
            pane.terminal.write(&self.buf);
            pump.output = true;
        }
        pane.flush();
        if let Some(title) = pane.terminal.take_title() {
            pump.events.push(Event::TitleChanged(title));
        }
        match status {
            ReadStatus::Drained => {}
            ReadStatus::More => pump.more = true,
            ReadStatus::Closed => {
                pane.closed = true;
                pump.events.push(Event::PaneClosed);
            }
        }
        pump
    }

    pub fn frame(&mut self) -> RefMut<'_, Frame> {
        self.pane.terminal.frame()
    }

    /// When a synchronized-output hold expires and a frame is due.
    pub fn hold_deadline(&self) -> Option<Instant> {
        self.pane.terminal.hold_deadline()
    }

    pub fn geometry(&self) -> Geometry {
        self.pane.terminal.geometry()
    }

    pub fn resize(&mut self, geometry: Geometry) {
        let pane = &mut self.pane;
        if let Err(err) = pane.terminal.resize(geometry) {
            log::warn!("terminal resize failed: {err}");
        }
        if let Err(err) = pane.pty.resize(window_size(geometry)) {
            log::warn!("pty resize failed: {err}");
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

    /// Applies a reloaded config to every terminal.
    pub fn configure(&mut self, config: &Config) {
        if let Err(err) = self.pane.terminal.configure(config) {
            log::warn!("applying the config failed: {err}");
        }
    }

    pub fn focus(&mut self, focused: bool) {
        self.input(|t| t.focus(focused));
    }

    pub fn mouse_tracking(&self) -> bool {
        self.pane.terminal.mouse_tracking()
    }

    pub fn mouse(&mut self, input: &MouseInput) {
        self.input(|t| t.mouse(input));
    }

    pub fn scroll(&mut self, lines: isize, mods: Mods, x: f32, y: f32) {
        self.input(|t| t.scroll(lines, mods, x, y));
    }

    fn input(&mut self, f: impl FnOnce(&mut Terminal) -> libghostty_vt::error::Result<()>) {
        if let Err(err) = f(&mut self.pane.terminal) {
            log::warn!("encoding input failed: {err}");
        }
        self.pane.flush();
    }
}

impl Pane {
    fn flush(&mut self) {
        self.pty.write(&self.terminal.take_replies());
    }
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
