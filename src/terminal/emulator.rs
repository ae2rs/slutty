use std::cell::{Cell, RefCell, RefMut};
use std::rc::Rc;
use std::time::{Duration, Instant};

use libghostty_vt::color::{Palette, Rgb};
use libghostty_vt::effect::{
    ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType, PrimaryDeviceAttributes,
    SecondaryDeviceAttributes,
};
use libghostty_vt::error::Result;
use libghostty_vt::key::{self, Key, Mods, OptionAsAlt};
use libghostty_vt::mode::Mode;
use libghostty_vt::mouse;
use libghostty_vt::report::{ColorScheme, SizeReportSize};
use libghostty_vt::terminal::{CursorStyle, ScrollViewport};
use libghostty_vt::{Error, RenderState, focus, paste};

use super::frame::{Frame, Iterators};
use crate::config::{self, Config};

/// A program that holds the screen longer than this (synchronized output)
/// gets its hold cancelled. libghostty has no clock, so this is ours.
const HOLD_TIMEOUT: Duration = Duration::from_secs(1);

/// Pane geometry in device pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub cols: u16,
    pub rows: u16,
    pub cell_width: u32,
    pub cell_height: u32,
    /// The whole drawable area, padding included.
    pub width: u32,
    pub height: u32,
    pub padding_left: u32,
    pub padding_top: u32,
}

/// One key event, already translated by the platform.
#[derive(Clone, Copy, Debug)]
pub struct KeyInput<'a> {
    pub key: Key,
    pub action: key::Action,
    pub mods: Mods,
    /// Modifiers the platform used to produce `text`, such as shift for `A`.
    pub consumed: Mods,
    pub text: Option<&'a str>,
    /// The character the key produces with no modifiers.
    pub unshifted: Option<char>,
}

#[derive(Clone, Copy, Debug)]
pub struct MouseInput {
    pub action: mouse::Action,
    pub button: Option<mouse::Button>,
    pub mods: Mods,
    /// Device pixels from the top-left of the pane.
    pub x: f32,
    pub y: f32,
}

/// State the libghostty callbacks share with us. Callbacks run inside
/// `vt_write`, on this thread.
struct Shared {
    render_state: RefCell<RenderState<'static>>,
    iterators: RefCell<Iterators>,
    frame: RefCell<Frame>,
    held_since: Cell<Option<Instant>>,
    replies: RefCell<Vec<u8>>,
    title_changed: Cell<bool>,
    geometry: Cell<Geometry>,
}

impl Shared {
    fn capture(&self, vt: &libghostty_vt::Terminal<'static, 'static>) {
        let mut state = self.render_state.borrow_mut();
        let result = state
            .update(vt)
            .and_then(|snapshot| self.frame.borrow_mut().capture(&snapshot, &mut self.iterators.borrow_mut()));
        if let Err(err) = result {
            log::error!("capturing the screen failed: {err}");
        }
    }
}

/// One libghostty terminal: bytes in, frames and replies out.
pub struct Terminal {
    vt: libghostty_vt::Terminal<'static, 'static>,
    shared: Rc<Shared>,
    key_encoder: key::Encoder<'static>,
    key_event: key::Event<'static>,
    mouse_encoder: mouse::Encoder<'static>,
    mouse_event: mouse::Event<'static>,
    option_as_alt: OptionAsAlt,
    buttons_down: u8,
}

impl Terminal {
    pub fn new(geometry: Geometry, config: &Config) -> Result<Self> {
        let mut vt = libghostty_vt::Terminal::new(geometry.cols, geometry.rows)?;
        let shared = Rc::new(Shared {
            render_state: RefCell::new(RenderState::new()?),
            iterators: RefCell::new(Iterators::new()?),
            frame: RefCell::new(Frame::default()),
            held_since: Cell::new(None),
            replies: RefCell::new(Vec::new()),
            title_changed: Cell::new(false),
            geometry: Cell::new(geometry),
        });

        let theme = &config.theme;
        vt.set_scrollback_max_lines(Some(config.terminal.scrollback_lines))?
            .set_terminfo_name("xterm-256color")?
            .set_default_fg_color(Some(rgb(theme.foreground)))?
            .set_default_bg_color(Some(rgb(theme.background)))?
            .set_default_cursor_color(Some(rgb(theme.cursor)))?
            .set_default_cursor_style(Some(cursor_style(config.terminal.cursor)))?
            .set_default_cursor_blink(Some(false))?
            .set_default_color_palette(Some(xterm_palette(&theme.ansi)))?;
        vt.resize(geometry.cols, geometry.rows, geometry.cell_width, geometry.cell_height)?;

        vt.on_pty_write({
            let shared = Rc::clone(&shared);
            move |_, data| shared.replies.borrow_mut().extend_from_slice(data)
        })?
        .on_title_changed({
            let shared = Rc::clone(&shared);
            move |_| shared.title_changed.set(true)
        })?
        .on_size({
            let shared = Rc::clone(&shared);
            move |_| {
                let g = shared.geometry.get();
                Some(SizeReportSize {
                    rows: g.rows,
                    columns: g.cols,
                    cell_width: g.cell_width,
                    cell_height: g.cell_height,
                })
            }
        })?
        .on_render_hold({
            let shared = Rc::clone(&shared);
            move |vt, held| {
                if held {
                    // The terminal now holds exactly the frame the program
                    // wants on screen; keep it until the hold ends.
                    shared.capture(vt);
                    shared.held_since.set(Some(Instant::now()));
                } else {
                    shared.held_since.set(None);
                }
            }
        })?
        .on_device_attributes(|_| Some(device_attributes()))?
        .on_xtversion(|_| Some(concat!("slutty ", env!("CARGO_PKG_VERSION"))))?
        .on_color_scheme(|_| Some(ColorScheme::Dark))?;

        let option_as_alt = match config.terminal.option_as_alt {
            config::OptionAsAlt::None => OptionAsAlt::False,
            config::OptionAsAlt::Left => OptionAsAlt::Left,
            config::OptionAsAlt::Right => OptionAsAlt::Right,
            config::OptionAsAlt::Both => OptionAsAlt::True,
        };
        Ok(Self {
            vt,
            shared,
            key_encoder: key::Encoder::new()?,
            key_event: key::Event::new()?,
            mouse_encoder: mouse::Encoder::new()?,
            mouse_event: mouse::Event::new()?,
            option_as_alt,
            buttons_down: 0,
        })
    }

    /// Feeds program output in.
    pub fn write(&mut self, data: &[u8]) {
        self.vt.vt_write(data);
    }

    /// Bytes for the program: replies to its queries and encoded input.
    pub fn take_replies(&self) -> Vec<u8> {
        std::mem::take(&mut self.shared.replies.borrow_mut())
    }

    /// The new title, if the program changed it since the last call.
    pub fn take_title(&self) -> Option<String> {
        self.shared
            .title_changed
            .replace(false)
            .then(|| String::from_utf8_lossy(self.vt.title().unwrap_or_default()).into_owned())
    }

    pub fn geometry(&self) -> Geometry {
        self.shared.geometry.get()
    }

    pub fn resize(&mut self, geometry: Geometry) -> Result<()> {
        let old = self.shared.geometry.replace(geometry);
        if (old.cols, old.rows, old.cell_width, old.cell_height)
            != (geometry.cols, geometry.rows, geometry.cell_width, geometry.cell_height)
        {
            self.vt.resize(geometry.cols, geometry.rows, geometry.cell_width, geometry.cell_height)?;
        }
        Ok(())
    }

    /// When an active hold expires, so the caller can come back then.
    pub fn hold_deadline(&self) -> Option<Instant> {
        self.shared.held_since.get().map(|since| since + HOLD_TIMEOUT)
    }

    /// The screen to draw. While a program holds the screen this is the frame
    /// from the start of the hold.
    pub fn frame(&mut self) -> RefMut<'_, Frame> {
        if self.hold_deadline().is_some_and(|deadline| Instant::now() >= deadline) {
            log::debug!("synchronized output held too long; releasing it");
            if let Err(err) = self.vt.set_mode(Mode::SYNC_OUTPUT, false) {
                log::warn!("releasing synchronized output failed: {err}");
            }
            self.shared.held_since.set(None);
        }
        if self.shared.held_since.get().is_none() {
            self.shared.capture(&self.vt);
        }
        self.shared.frame.borrow_mut()
    }

    pub fn key(&mut self, input: &KeyInput<'_>) -> Result<()> {
        self.key_event
            .set_key(input.key)
            .set_action(input.action)
            .set_mods(input.mods)
            .set_consumed_mods(input.consumed)
            .set_unshifted_codepoint(input.unshifted.unwrap_or('\0'))
            .set_utf8(input.text);
        let mut replies = self.shared.replies.borrow_mut();
        let before = replies.len();
        self.key_encoder
            .set_options_from_terminal(&self.vt)
            .set_macos_option_as_alt(self.option_as_alt)
            .encode_to_vec(&self.key_event, &mut replies)?;
        let sent = replies.len() > before;
        drop(replies);
        if sent {
            self.vt.scroll_viewport(ScrollViewport::Bottom);
        }
        Ok(())
    }

    pub fn paste(&mut self, text: &str) -> Result<()> {
        let bracketed = self.vt.mode(Mode::BRACKETED_PASTE)?;
        let mut data = text.as_bytes().to_vec();
        let mut out = vec![0; data.len() + 16];
        let len = match paste::encode(&mut data.clone(), bracketed, &mut out) {
            Err(Error::OutOfSpace { required }) => {
                out.resize(required, 0);
                paste::encode(&mut data, bracketed, &mut out)?
            }
            other => other?,
        };
        self.shared.replies.borrow_mut().extend_from_slice(&out[..len]);
        self.vt.scroll_viewport(ScrollViewport::Bottom);
        Ok(())
    }

    pub fn focus(&mut self, focused: bool) -> Result<()> {
        if self.vt.mode(Mode::FOCUS_EVENT)? {
            let event = if focused { focus::Event::Gained } else { focus::Event::Lost };
            let mut buf = [0; 8];
            let len = event.encode(&mut buf)?;
            self.shared.replies.borrow_mut().extend_from_slice(&buf[..len]);
        }
        Ok(())
    }

    /// Whether the program wants mouse events.
    pub fn mouse_tracking(&self) -> bool {
        self.vt.is_mouse_tracking().unwrap_or(false)
    }

    pub fn mouse(&mut self, input: &MouseInput) -> Result<()> {
        match input.action {
            mouse::Action::Press => self.buttons_down = self.buttons_down.saturating_add(1),
            mouse::Action::Release => self.buttons_down = self.buttons_down.saturating_sub(1),
            _ => {}
        }
        let g = self.geometry();
        self.mouse_event
            .set_action(input.action)
            .set_button(input.button)
            .set_mods(input.mods)
            .set_position(mouse::Position { x: input.x, y: input.y });
        let mut replies = self.shared.replies.borrow_mut();
        self.mouse_encoder
            .set_options_from_terminal(&self.vt)
            .set_size(mouse::EncoderSize {
                screen_width: g.width,
                screen_height: g.height,
                cell_width: g.cell_width,
                cell_height: g.cell_height,
                padding_top: g.padding_top,
                padding_bottom: 0,
                padding_left: g.padding_left,
                padding_right: g.padding_left,
            })?
            .set_any_button_pressed(self.buttons_down > 0)
            .set_track_last_cell(true)
            .encode_to_vec(&self.mouse_event, &mut replies)
    }

    /// Scrolls by `lines` (negative is up, into history), or sends wheel
    /// buttons to a program that tracks the mouse.
    pub fn scroll(&mut self, lines: isize, mods: Mods, x: f32, y: f32) -> Result<()> {
        if lines == 0 {
            return Ok(());
        }
        if self.mouse_tracking() {
            let button = if lines < 0 { mouse::Button::Four } else { mouse::Button::Five };
            for _ in 0..lines.unsigned_abs() {
                for action in [mouse::Action::Press, mouse::Action::Release] {
                    self.mouse(&MouseInput { action, button: Some(button), mods, x, y })?;
                }
            }
        } else {
            self.vt.scroll_viewport(ScrollViewport::Delta(lines));
        }
        Ok(())
    }
}

fn rgb([r, g, b]: config::Rgb) -> Rgb {
    Rgb { r, g, b }
}

/// The standard xterm palette with the theme's 16 ANSI colors.
fn xterm_palette(ansi: &[config::Rgb; 16]) -> Palette {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let mut palette = Palette([Rgb { r: 0, g: 0, b: 0 }; 256]);
    for (i, color) in palette.0.iter_mut().enumerate() {
        *color = match i {
            0..16 => rgb(ansi[i]),
            16..232 => {
                let i = i - 16;
                Rgb { r: LEVELS[i / 36], g: LEVELS[i / 6 % 6], b: LEVELS[i % 6] }
            }
            _ => {
                let level = 8 + 10 * (i - 232) as u8;
                Rgb { r: level, g: level, b: level }
            }
        };
    }
    palette
}

fn cursor_style(shape: config::CursorShape) -> CursorStyle {
    match shape {
        config::CursorShape::Block => CursorStyle::Block,
        config::CursorShape::Bar => CursorStyle::Bar,
        config::CursorShape::Underline => CursorStyle::Underline,
    }
}

fn device_attributes() -> DeviceAttributes {
    DeviceAttributes {
        primary: PrimaryDeviceAttributes::new(
            ConformanceLevel::VT220,
            &[
                DeviceAttributeFeature::COLUMNS_132,
                DeviceAttributeFeature::SELECTIVE_ERASE,
                DeviceAttributeFeature::ANSI_COLOR,
            ],
        ),
        secondary: SecondaryDeviceAttributes { device_type: DeviceType::VT220, firmware_version: 1, rom_cartridge: 0 },
        tertiary: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::Grapheme;

    fn terminal() -> Terminal {
        let geometry = Geometry {
            cols: 10,
            rows: 3,
            cell_width: 8,
            cell_height: 16,
            width: 96,
            height: 48,
            padding_left: 8,
            padding_top: 0,
        };
        Terminal::new(geometry, &Config::default()).expect("terminal")
    }

    fn row_text(t: &mut Terminal, y: usize) -> String {
        t.frame().rows[y]
            .cells
            .iter()
            .map(|c| match &c.text {
                Grapheme::Empty => ' ',
                Grapheme::Char(c) => *c,
                Grapheme::Cluster(s) => s.chars().next().unwrap_or('?'),
            })
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    #[test]
    fn captures_text_and_inverse_colors() {
        let mut t = terminal();
        t.write(b"hi \x1b[7mX\x1b[0m");
        assert_eq!(row_text(&mut t, 0), "hi X");
        let frame = t.frame();
        let x = &frame.rows[0].cells[3];
        assert_eq!(x.fg, frame.background);
        assert_eq!(x.bg, Some(frame.foreground));
        assert!(frame.rows[0].dirty);
    }

    #[test]
    fn frame_is_clean_until_output_arrives() {
        let mut t = terminal();
        t.write(b"a");
        t.frame().changed = false;
        assert!(!t.frame().changed);
        t.write(b"b");
        assert!(t.frame().changed);
    }

    #[test]
    fn synchronized_output_shows_the_frame_from_the_start_of_the_hold() {
        let mut t = terminal();
        t.write(b"old");
        assert_eq!(row_text(&mut t, 0), "old");
        t.write(b"\r\x1b[2K\x1b[?2026hnew");
        assert_eq!(row_text(&mut t, 0), "");
        assert!(t.hold_deadline().is_some());
        t.write(b"\x1b[?2026l");
        assert_eq!(row_text(&mut t, 0), "new");
        assert!(t.hold_deadline().is_none());
    }

    #[test]
    fn scrolls_into_history_and_back_on_key_press() {
        let mut t = terminal();
        for i in 0..10 {
            t.write(format!("line{i}\r\n").as_bytes());
        }
        assert_eq!(row_text(&mut t, 0), "line8");
        t.scroll(-5, Mods::empty(), 0.0, 0.0).expect("scroll");
        assert_eq!(row_text(&mut t, 0), "line3");
        let input = KeyInput {
            key: Key::A,
            action: key::Action::Press,
            mods: Mods::empty(),
            consumed: Mods::empty(),
            text: Some("a"),
            unshifted: Some('a'),
        };
        t.key(&input).expect("key");
        assert_eq!(row_text(&mut t, 0), "line8");
        assert_eq!(t.take_replies(), b"a");
    }

    fn press(t: &mut Terminal, key: Key, mods: Mods, text: &str, unshifted: char) -> Vec<u8> {
        let consumed = if mods.contains(Mods::ALT_SIDE) { mods & (Mods::ALT | Mods::ALT_SIDE) } else { Mods::empty() };
        let input =
            KeyInput { key, action: key::Action::Press, mods, consumed, text: Some(text), unshifted: Some(unshifted) };
        t.key(&input).expect("key");
        t.take_replies()
    }

    #[test]
    fn left_option_is_alt_and_right_option_composes() {
        let mut t = terminal();
        assert_eq!(press(&mut t, Key::B, Mods::ALT, "b", 'b'), b"\x1bb");
        assert_eq!(press(&mut t, Key::D, Mods::ALT | Mods::ALT_SIDE, "∂", 'd'), "∂".as_bytes());
    }

    #[test]
    fn replies_to_device_attribute_queries() {
        let t = &mut terminal();
        t.write(b"\x1b[c");
        assert!(t.take_replies().starts_with(b"\x1b[?62"));
    }

    #[test]
    fn reports_title_changes_once() {
        let mut t = terminal();
        t.write(b"\x1b]2;hello\x07");
        assert_eq!(t.take_title().as_deref(), Some("hello"));
        assert_eq!(t.take_title(), None);
    }
}
