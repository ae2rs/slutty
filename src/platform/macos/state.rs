//! What every window shares: the mux, the renderer, the config and the key
//! bindings. It lives on the main thread; PTY threads only wake it.
//!
//! Callers borrow it through [`with_app`] and must not call into AppKit
//! while they do: AppKit may call back (closing a window, running an alert)
//! and need it again.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use dispatch2::{DispatchQueue, DispatchTime};
use objc2::rc::Retained;

use super::view::TerminalView;
use super::watch::Watcher;
use crate::config::{self, Config};
use crate::font::Fonts;
use crate::keys::Matcher;
use crate::mux::{Mux, WindowId};
use crate::renderer::Renderer;
use crate::status::Monitor;

/// The config in effect, where it came from, and why the file could not be
/// used, if it could not.
pub struct Settings {
    pub config: Config,
    pub path: Option<PathBuf>,
    pub error: Option<String>,
}

pub struct App {
    pub mux: Mux,
    pub renderer: Renderer,
    pub settings: Settings,
    pub matcher: Matcher,
    /// Key codes whose press a binding used, so their release is not sent.
    pub swallowed: Vec<u16>,
    /// In points; the font-size keys change it.
    pub font_size: f64,
    /// Device pixels per point of the screen the fonts are made for.
    pub scale: f64,
    pub tab_bar: bool,
    /// The system stats the tab bar shows, for every window.
    pub status: Monitor,
    watcher: Option<Watcher>,
    hold_timer: bool,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    static VIEWS: RefCell<BTreeMap<WindowId, Retained<TerminalView>>> = const { RefCell::new(BTreeMap::new()) };
}

/// Runs `f` with the app. `None` before it starts, after it ends, or when
/// it is already borrowed, which is a bug: AppKit called back during `f`.
pub fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|app| match app.try_borrow_mut() {
        Ok(mut app) => app.as_mut().map(f),
        Err(_) => {
            log::error!("the app is already in use; dropping a reentrant call");
            None
        }
    })
}

pub fn view(window: WindowId) -> Option<Retained<TerminalView>> {
    VIEWS.with(|views| views.borrow().get(&window).cloned())
}

pub fn views() -> Vec<Retained<TerminalView>> {
    VIEWS.with(|views| views.borrow().values().cloned().collect())
}

pub fn add_view(window: WindowId, view: Retained<TerminalView>) {
    VIEWS.with(|views| views.borrow_mut().insert(window, view));
}

pub fn remove_view(window: WindowId) {
    // Dropped outside the borrow: the view may be the last owner.
    let view = VIEWS.with(|views| views.borrow_mut().remove(&window));
    drop(view);
}

/// Runs on a PTY reader thread when output is waiting.
pub fn wake_from_any_thread() {
    DispatchQueue::main().exec_async(pump);
}

impl App {
    pub fn start(mux: Mux, renderer: Renderer, settings: Settings, scale: f64) {
        let app = Self {
            mux,
            renderer,
            font_size: settings.config.font.size,
            tab_bar: settings.config.tab_bar.enabled,
            status: Monitor::new(),
            settings,
            matcher: Matcher::default(),
            swallowed: Vec::new(),
            scale,
            watcher: None,
            hold_timer: false,
        };
        APP.with(|cell| *cell.borrow_mut() = Some(app));
        watch_config();
    }

    pub fn config(&self) -> &Config {
        &self.settings.config
    }

    /// Loads the fonts at the current size and scale. On failure the old ones
    /// stay, and the banner says why.
    pub fn load_fonts(&mut self) {
        match Fonts::new(&self.settings.config.font, self.font_size * self.scale) {
            Ok(fonts) => self.renderer.set_fonts(fonts),
            Err(err) => {
                log::error!("loading fonts failed: {err}");
                self.settings.error = Some(err.to_string());
            }
        }
    }

    /// Applies a config read from the file. Returns whether anything changed.
    fn reload(&mut self, loaded: Result<Config, String>) -> bool {
        let new = match loaded {
            Ok(new) => new,
            Err(err) => {
                let changed = self.settings.error.as_ref() != Some(&err);
                if changed {
                    log::error!("config: {err}");
                }
                self.settings.error = Some(err);
                return changed;
            }
        };
        let had_error = self.settings.error.take().is_some();
        let old = std::mem::replace(&mut self.settings.config, new);
        let new = &self.settings.config;
        if *new == old {
            return had_error;
        }
        log::info!("config reloaded");
        self.mux.configure(new);
        if new.keys != old.keys {
            self.matcher.reset();
        }
        if new.tab_bar.enabled != old.tab_bar.enabled {
            self.tab_bar = new.tab_bar.enabled;
        }
        if new.font != old.font {
            if new.font.size != old.font.size {
                self.font_size = new.font.size;
            }
            self.load_fonts();
        }
        true
    }
}

/// Parses waiting output and shows what changed.
fn pump() {
    let Some((pump, deadline)) = with_app(|app| (app.mux.pump(), app.mux.hold_deadline())) else { return };
    if pump.more {
        wake_from_any_thread();
    }
    if let Some(text) = &pump.clipboard {
        super::actions::set_clipboard(text);
    }
    for (pane, notification) in &pump.notifications {
        super::notify::post(*pane, notification);
    }
    if let Some(deadline) = deadline {
        schedule_hold_timeout(deadline);
    }
    for window in &pump.redraw {
        if let Some(view) = view(*window) {
            view.output_arrived();
        }
    }
    for window in &pump.tabs {
        if let Some(view) = view(*window) {
            view.tabs_changed();
        }
    }
    for window in pump.closed {
        if let Some(view) = view(window) {
            view.close_window();
        }
    }
}

/// Comes back when a synchronized-output hold expires, so a program that
/// never releases it cannot freeze the screen.
fn schedule_hold_timeout(deadline: Instant) {
    if with_app(|app| std::mem::replace(&mut app.hold_timer, true)) != Some(false) {
        return;
    }
    let delay = deadline.saturating_duration_since(Instant::now());
    let when = DispatchTime::try_from(delay).unwrap_or(DispatchTime::NOW);
    let scheduled = DispatchQueue::main().after(when, || {
        with_app(|app| app.hold_timer = false);
        views().iter().for_each(|view| view.request_frame());
    });
    if scheduled.is_err() {
        with_app(|app| app.hold_timer = false);
    }
}

/// Watches the config file, replacing the previous watch.
fn watch_config() {
    with_app(|app| {
        app.watcher = None;
        app.watcher = app.settings.path.as_deref().map(|path| {
            // Later, on the main queue, so the watch is not replaced from
            // inside its own handler.
            Watcher::new(path, || DispatchQueue::main().exec_async(reload_config))
        });
    });
}

/// Reads the config file again and applies it. A broken file keeps the
/// config in effect, and the banner says why.
pub fn reload_config() {
    watch_config();
    let changed = with_app(|app| {
        let loaded = app.settings.path.as_deref().map_or_else(|| Ok(Config::default()), config::load);
        app.reload(loaded)
    });
    if changed == Some(true) {
        views().iter().for_each(|view| view.relayout());
    }
}
