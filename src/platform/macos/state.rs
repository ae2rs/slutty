//! What every window shares: the line to the mux server and the copy of what
//! it shows, the renderer, the config and the key bindings. It lives on the
//! main thread; other threads only wake it.
//!
//! Callers borrow it through [`with_app`] and must not call into AppKit
//! while they do: AppKit may call back (closing a window, running an alert)
//! and need it again.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use dispatch2::{DispatchQueue, DispatchTime};
use objc2::MainThreadMarker;
use objc2::rc::Retained;

use super::view::TerminalView;
use super::watch::Watcher;
use crate::client::{Changes, Connection, Mirror, log_failure};
use crate::config::{self, Config};
use crate::font::Fonts;
use crate::keys::Matcher;
use crate::mux::{PaneId, WindowId};
use crate::proto::{Command, Input, Reply, ServerMessage};
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
    pub connection: Connection,
    pub mirror: Mirror,
    /// Requests whose answer matters, by id.
    pending: BTreeMap<u32, Pending>,
    /// Copy mode in this pane waits for the character of an `f` or `t`.
    pub jump_pending: Option<PaneId>,
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

enum Pending {
    /// The first window: without it the app has nothing to show.
    FirstWindow,
    /// A [`Reply::Busy`] asks the user, then sends the command again
    /// without `confirm`.
    Confirm { message: &'static str, command: Command },
}

/// A pump is scheduled and has not started.
static WAKE_PENDING: AtomicBool = AtomicBool::new(false);

/// Runs on any thread when the server has something to say. Bursts become
/// one pump.
pub fn wake_from_any_thread() {
    if !WAKE_PENDING.swap(true, Ordering::AcqRel) {
        DispatchQueue::main().exec_async(pump);
    }
}

impl App {
    pub fn start(connection: Connection, renderer: Renderer, settings: Settings, scale: f64) {
        let app = Self {
            connection,
            mirror: Mirror::default(),
            pending: BTreeMap::new(),
            jump_pending: None,
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

    pub fn request(&mut self, command: Command) {
        self.connection.request(command);
    }

    /// Sends `command`, asking the user first if it would stop something
    /// other than a shell; `command` must carry `confirm`.
    pub fn request_confirmed(&mut self, message: &'static str, command: Command) {
        let id = self.connection.request(command.clone());
        self.pending.insert(id, Pending::Confirm { message, command });
    }

    /// Sends the request for the first window; the app quits if it fails.
    pub fn request_first_window(&mut self, command: Command) {
        let id = self.connection.request(command);
        self.pending.insert(id, Pending::FirstWindow);
    }

    pub fn input(&mut self, input: Input) {
        self.connection.input(input);
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
        self.connection.request(Command::ReloadConfig);
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

/// Reads what the server said and shows what changed.
fn pump() {
    WAKE_PENDING.store(false, Ordering::Release);
    let received = with_app(|app| {
        let mut changes = Changes::default();
        let mut confirms = Vec::new();
        for message in app.connection.receive() {
            match message {
                ServerMessage::Event(event) => app.mirror.apply(event, &mut changes),
                ServerMessage::Response { id, result } => match (app.pending.remove(&id), result) {
                    (Some(Pending::Confirm { message, command }), Ok(Reply::Busy)) => confirms.push((message, command)),
                    (Some(Pending::FirstWindow), Err(err)) => {
                        log::error!("starting the program failed: {err}");
                        std::process::exit(1);
                    }
                    (_, result) => log_failure(&result),
                },
            }
        }
        if app
            .jump_pending
            .is_some_and(|pane| app.mirror.overlay(pane).is_none_or(|o| o.mode != crate::proto::Mode::Copy))
        {
            app.jump_pending = None;
        }
        (changes, confirms, app.connection.deadline())
    });
    let Some((changes, confirms, deadline)) = received else { return };
    show(changes);
    if let Some(deadline) = deadline {
        schedule_hold_timeout(deadline);
    }
    for confirm in confirms {
        ask_and_send(confirm);
    }
}

fn show(changes: Changes) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    if let Some(text) = &changes.clipboard {
        super::actions::set_clipboard(text);
    }
    for (pane, notification) in &changes.notifications {
        super::notify::post(*pane, notification);
    }
    for url in &changes.open_urls {
        super::actions::open_url(url);
    }
    for window in &changes.opened {
        super::app::show_window(mtm, *window);
    }
    for window in &changes.windows {
        if let Some(view) = view(*window) {
            view.tabs_changed();
        }
    }
    for window in &changes.output {
        if let Some(view) = view(*window) {
            view.output_arrived();
        }
    }
    if let Some(hover) = changes.hover {
        let window = hover.as_ref().and_then(|h| with_app(|app| app.mirror.find(h.pane)).flatten());
        for view in views() {
            if Some(view.mux_window()) == window {
                view.show_hover(hover.clone());
            } else {
                view.set_hover(None);
            }
        }
    }
    for window in changes.closed {
        if let Some(view) = view(window) {
            view.close_window();
        }
    }
}

/// Asks before closing what runs more than a shell, and closes it if the
/// user agrees.
fn ask_and_send((message, command): (&'static str, Command)) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    // The alert runs the event loop, so the app is not borrowed here.
    if super::actions::alert(mtm, message, "A program is still running in it.", "Close") {
        let command = match command {
            Command::CloseTab { window, .. } => Command::CloseTab { window, confirm: false },
            Command::ClosePane { pane, .. } => Command::ClosePane { pane, confirm: false },
            other => other,
        };
        with_app(|app| app.request(command));
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
        pump();
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
