# slutty design

slutty is a terminal emulator and multiplexer for macOS. It replaces WezTerm.

## Goals

- **Native on macOS.** AppKit window, Metal drawing, system input methods.
- **Fast and idle when idle.** Draw only when something changed. No polling, no timers while nothing happens.
- **Reliable.** Small code, clear ownership, no crash on bad input.
- **Only the features I use.** The WezTerm config is the feature list, not WezTerm itself.

## Non-goals (for now)

- Linux. Platform code stays behind a seam so a port is possible later.
- A detachable mux server. The design leaves room for it (see [Mux server](#mux-server-later)).
- Lua or any scripting. Config is declarative TOML.

## Stack

- Rust, one cargo package.
- [libghostty-vt](https://lib.ghostty.org/vt/) 1.0 for terminal emulation, through the [libghostty-rs](https://github.com/uzaaft/libghostty-rs) bindings.
- AppKit, Metal, CoreText and GCD through the `objc2` crates. No Swift, no Xcode project.

## libghostty-vt: what it gives, what we build

libghostty-vt is a C library. It turns the bytes a program writes into terminal state. It has no threads, no PTY, no fonts and no drawing.

### What libghostty-vt gives us

- **Parsing and state.** Every escape sequence, screen, cursor, modes, alternate screen.
- **Scrollback.** Bounded by lines or bytes, with reflow on resize.
- **Render state.** A copy of what to draw: rows, cells, colors, cursor, selection.
  - Dirty tracking on two levels: the whole frame (clean, partial, full) and each row.
  - `clean()` after a frame. An update only adds dirt, never clears it.
- **Render holds.** Synchronized output (mode 2026) through `on_render_hold`. The library has no clock, so we end a hold after 1 s ourselves.
- **Input encoding.**
  - Keys: legacy, Kitty keyboard protocol, modifyOtherKeys, cursor and keypad modes, macOS option-as-alt.
  - Mouse: X10, SGR, UTF-8, URxvt and pixel formats, all tracking modes.
  - Paste (bracketed), focus reports.
- **Effects.** Callbacks for PTY replies, bell, title, working directory, clipboard (OSC 52), desktop notifications (OSC 9/777), semantic prompts (OSC 133), size queries, device attributes, unknown sequences.
- **Extras.** Selection, search, formatter (screen to text/HTML/VT), Kitty graphics.

### What we build

| Layer | Work |
| --- | --- |
| PTY | Open a PTY, start the login shell, resize, read and write threads, child exit. |
| Mux | Windows, tabs, panes, layout, focus. Owns the terminals. |
| Fonts | Find faces with CoreText, cell metrics, fallback, rasterise glyphs. Shaping and ligatures later. |
| Renderer | Metal pipeline, glyph atlas, damage-driven frames. |
| Platform | App, windows, views, key and mouse events, IME, clipboard, menus. |
| Config | TOML schema, defaults, loading, reload. |

### Gaps we fill around libghostty-vt

- **No thread safety.** A `Terminal` is `!Send` in the Rust bindings. It lives on one thread.
- **User vars.** WezTerm's `OSC 1337 SetUserVar` is not parsed. We read it through `on_unknown_sequence`.
- **Option key.** The encoder knows option-as-alt, but the platform layer must pick which text to send (composed or not).

## Layers

```
          ┌──────────────────────────────┐
          │ platform/macos               │  AppKit app, windows, views, input, IME
          └──────┬────────────────┬──────┘
   input events  │                │  draw requests
          ┌──────▼──────┐   ┌─────▼──────┐
          │ mux         │──▶│ renderer   │  Metal, glyph atlas
          │ panes, tabs │   └─────┬──────┘
          └──────┬──────┘         │
          ┌──────▼──────┐   ┌─────▼──────┐
          │ terminal    │   │ font       │  CoreText
          │ libghostty  │   └────────────┘
          └──────┬──────┘
          ┌──────▼──────┐
          │ pty         │  child process, reader and writer threads
          └─────────────┘
               config (read by all)
```

### Modules

- `pty`: spawn a process on a PTY. Resize. Reader and writer threads. Knows nothing about terminals.
- `terminal`: one libghostty terminal per pane. Feeds bytes in, wires effects, exposes a render snapshot.
- `mux`: the model. Windows hold tabs, tabs hold a layout of panes. No AppKit types.
- `font`: faces, metrics, glyph lookup and rasterisation.
- `renderer`: turns a render snapshot into a Metal frame.
- `platform/macos`: the only module that talks to AppKit.
- `config`: types with defaults. Parsing comes in the config milestone.

### Rules between layers

- `mux`, `terminal` and `pty` never import `platform` or `renderer`. This keeps a mux server and a Linux port possible.
- The platform calls into the mux with plain events (key, text, mouse, resize, focus). The mux answers with damage: which pane needs a frame.
- The platform owns the event loop. The mux never blocks.

## Threads

- **Main thread.** Runs the AppKit loop. Owns the mux, every terminal and the renderer.
- **One reader thread per pane.** Blocks on `read()` of the PTY. Sends each chunk over a channel. Wakes the main thread.
- **One writer thread per pane.** Blocks on a channel and writes to the PTY. A child that stops reading never blocks the UI.

### Waking the main thread

- The reader sets an atomic "pending" flag. Only the first set dispatches a block to the GCD main queue. Bursts become one wake.
- On wake, the main thread clears the flag, drains the channel into the terminal, and asks for a frame if the render state is dirty.

### Why not a terminal thread

- libghostty-vt parses at GB/s. Parsing on the main thread is cheap.
- One owner means no lock and no `unsafe impl Send`.
- If profiling shows input lag under heavy output, move the mux to its own thread. The rule above (no AppKit in the mux) keeps that move small.

## Rendering

### Damage-driven frames

- A frame happens only after damage: PTY output that dirtied the render state, input, resize, focus change, scale change, selection change.
- Frames are paced by the view's display link (`NSView.displayLink`, macOS 14+).
  - Damage unpauses it. The next vsync draws one frame.
  - After a frame with no new damage, it pauses again.
  - So a burst of output costs at most one frame per refresh, and idle costs nothing.
- Each frame: update the render state, rebuild instance data for dirty rows only, draw, `clean()`.

### Pipeline

- `CAMetalLayer` backing the view. One drawable per frame.
- Two instanced passes, one quad per cell:
  1. Backgrounds.
  2. Glyphs, sampled from the atlas.
- Then the cursor, and the IME preedit text at the cursor.
- Per-cell instance data lives in a buffer kept between frames. Dirty rows overwrite their slice.

### Glyph atlas

- Two textures: one channel for text, RGBA for colour emoji.
- Key: face, glyph id, scale. Rasterised once with CoreText.
- Rebuilt when the backing scale or font size changes.

### Cursor

- Shapes: block, bar, underline, hollow block when unfocused.
- No blinking in the MVP: a blink needs a timer. Later, blink only while focused and asked for, and stop after a few seconds of no input.

## Input

### Keys

- `keyDown` goes through `interpretKeyEvents` so IME and dead keys work.
- `NSTextInputClient` on the view:
  - `insertText` sends committed text.
  - `setMarkedText` shows the preedit at the cursor. Nothing goes to the PTY until it commits.
  - `firstRectForCharacterRange` places the IME window at the cursor.
- Keys with no text (arrows, function keys, ctrl chords) go straight to libghostty's key encoder.
- The encoder reads modes from the terminal before each key (`set_options_from_terminal`), then we set option-as-alt.

### Option key

- Default like WezTerm: left Option is Alt, right Option composes (`∂`, `é`).
- For an Alt key, the text sent is the character without Option. The encoder adds the ESC prefix or the Kitty modifier.

### Mouse

- Wheel: scroll the viewport through scrollback. When the program tracks the mouse, send wheel buttons instead.
- Clicks and motion go through the mouse encoder when the program tracks the mouse.
- Selection comes in a later milestone.

## PTY

- `openpty`, then `fork`/`exec` through `std::process::Command` with `setsid` and `TIOCSCTTY` in `pre_exec`.
- Shell from `getpwuid`, started as a login shell (`argv[0]` is `-zsh`), in `$HOME`.
- Environment: `TERM=xterm-256color`, `COLORTERM=truecolor`, `TERM_PROGRAM=slutty`.
- Resize with `TIOCSWINSZ`. The kernel sends `SIGWINCH`.
- EOF on the reader means the child is gone. Reap it with `waitpid`, then close the pane.

## Feature inventory

Taken from `~/dotfiles/wezterm/.config/wezterm/`. Milestones refer to the [roadmap](#roadmap).

### Appearance

| Feature | WezTerm setting | Decision | Milestone |
| --- | --- | --- | --- |
| Tokyo Night colors | `color_scheme` | Keep. Built-in default theme. | M1 (hardcoded), M2 (config) |
| JetBrains Mono 16, line height 1 | `font`, `font_size`, `line_height` | Keep. | M1 (hardcoded), M2 |
| Bold as DemiBold, half as ExtraLight | `font_rules` | Keep as weight settings, not rules. | M2 |
| Ligatures | WezTerm default | Keep. Needs run shaping. | M7 |
| Steady bar cursor | `default_cursor_style` | Keep. Programs can still change it. | M1 |
| No bell sound | `audible_bell` | Keep. Bell does nothing by default. | M1 |
| No close confirmation | `window_close_confirmation` | Keep. | M1 |
| No titlebar, resizable | `window_decorations = "RESIZE"` | Keep. Tab bar takes the titlebar area. | M3 |
| Titlebar colors | `window_frame` | Keep, from the theme. | M3 |
| Padding 1 cell left and right | `window_padding` | Keep. | M2 |
| Rapid blink text (SGR 6) | `text_blink_rate_rapid` | Show blinking text steady. A blink needs a timer. | M1 |

### Keys

| Feature | WezTerm binding | Decision | Milestone |
| --- | --- | --- | --- |
| Leader `ctrl+s`, no timeout | `leader` | Keep. | M2 |
| No default bindings | `disable_default_key_bindings` | Keep. Defaults ship as a config file the user owns. | M2 |
| Kitty keyboard protocol | `enable_kitty_keyboard` | Keep. Always on; programs opt in. | M1 |
| Copy, paste | `cmd+c`, `cmd+v` | Keep. | M1 |
| Font size | `cmd/ctrl + - = 0` | Keep. | M2 |
| Cmd+q asks first | `InputSelector` | Keep as a native alert. | M2 |
| Cmd/Opt arrows, cmd+backspace remaps | `SendString`, `SendKey` | Keep as `send` actions. | M2 |
| Fullscreen | `alt+enter` | Keep, native fullscreen. | M6 |
| Spawn window | `cmd+n` | Keep. | M6 |
| Reload config | `leader/cmd+R` | Keep, and reload on file change too. | M2 |

### Tabs

| Feature | WezTerm binding | Decision | Milestone |
| --- | --- | --- | --- |
| New tab, close tab | `leader t`, `cmd+t`, `cmd+w` | Keep. | M3 |
| Go to tab 1-9, last | `leader/cmd + 1-9, 0` | Keep. | M3 |
| Tab bar: index and cwd | tabline plugin | Keep. Cwd from OSC 7. | M3 |
| Toggle tab bar | `leader/cmd+Z` | Keep. | M3 |
| Tab bar always shown | `hide_tab_bar_if_only_one_tab = false` | Keep. | M3 |
| Max tab width 32 | `tab_max_width` | Keep. | M3 |
| Window title `[i/n] title` | WezTerm default | Keep. AeroSpace finds windows by it. | M3 |
| RAM, CPU, net, disk widgets | tabline + `popen` every 2 s | Drop for now. Polling breaks the idle rule. Maybe later as an opt-in widget. | M8 |
| Animated rho star at 60 fps | `animation_fps`, blink easing | Drop the animation. Show a static glyph per state. | M6 |

### Panes

| Feature | WezTerm binding | Decision | Milestone |
| --- | --- | --- | --- |
| Split right, split down | `leader v`, `leader -` | Keep. | M4 |
| Focus pane hjkl | `leader/cmd + hjkl` | Keep. | M4 |
| Resize pane | `leader shift+hjkl`, resize key table | Keep. | M4 |
| Close pane, confirm | `leader x` | Keep. | M4 |
| Zoom pane | `leader/cmd+z` | Keep. | M4 |
| Rotate panes | `leader/cmd+e` | Keep. | M4 |
| Pane select by letter | `leader/cmd+p` | Keep. | M4 |
| Pop pane or tab to a new window | `leader m`, `leader M` | Keep. Simple in-process with our own mux. | M6 |

### Scrollback, selection and search

| Feature | WezTerm setting | Decision | Milestone |
| --- | --- | --- | --- |
| 100 000 lines of scrollback | `scrollback_lines` | Keep. | M1 |
| Wheel scrolls 5 lines | `mouse_bindings` | Keep. | M1 |
| Mouse selection | WezTerm default | Keep. libghostty has selection. | M5 |
| Copy mode (vim keys) | `ActivateCopyMode` | Keep. | M5 |
| Search | `cmd+f` | Keep. libghostty has search. | M5 |
| Quick select | `leader space` | Keep. | M5 |
| Open URL | `leader/cmd+o` | Keep. | M5 |

### rho integration

| Feature | WezTerm mechanism | Decision | Milestone |
| --- | --- | --- | --- |
| `rho_activity` glyph in the tab | user var, `format-tab-title` | Keep. Built in, no scripting. | M6 |
| Copy mode over rho's transcript | `rho_scrollback` user var, `ctrl+s` handoff | Keep. Built in. | M6 |
| Click a notification to focus a pane | `focus_pane` user var | Keep. Raise window and tab, then AeroSpace. | M6 |
| AeroSpace focus across workspaces | `aerospace list-windows` by title | Keep. rho must learn slutty's bundle id. | M6 |

### Dropped

- **Lua config and callbacks.** TOML covers the use cases above. rho's Lua becomes built-in behaviour.
- **Character select** (`ctrl+shift+u`). macOS has `ctrl+cmd+space`.
- **Debug overlay** (`ctrl+shift+l`). Logs replace it.
- **Command palette** (`cmd+shift+p`). Maybe later; not on the roadmap.

## Roadmap

Each milestone is one PR. Each must keep the idle rule: no work while nothing changes.

### M1: MVP

- One window, one pane, the login shell.
- Metal renderer with damage-driven frames.
- Keys through libghostty, IME, Option as Alt on the left.
- Scrollback with the wheel. Copy and paste.
- Hardcoded font and theme.

### M2: Config and keybindings

- Load `~/.config/slutty/slutty.toml`. Reload when the file changes (FSEvents, no polling).
- Fonts, weights, colors, padding, scrollback size.
- Key engine: leader, chords, key tables, `send` actions, font size, quit confirm.

### M3: Tabs and tab bar

- Tabs in the mux. Spawn, close, activate by index.
- Tab bar drawn by the renderer in the titlebar area. Redrawn only when a tab changes.
- Window title `[i/n] title`. Toggle tab bar.

### M4: Splits

- Split tree per tab. Split, close, focus by direction, resize, zoom, rotate, pane select.
- One renderer pass per pane into the same drawable. Damage per pane.

### M5: Selection, copy mode, search

- Mouse selection and copy.
- Copy mode with vim keys. Search. Quick select. Open URL.

### M6: Windows and rho

- Several windows. New window. Pop pane or tab out. Fullscreen.
- User vars through `on_unknown_sequence`. rho activity glyph, transcript copy mode, `focus_pane`, AeroSpace focus.

### M7: Font shaping

- Shape runs with CoreText. Ligatures. Better fallback.

### M8: Polish

- Cursor blink with an idle cut-off. Blinking text.
- Optional status widgets, off by default.

WezTerm parity for my use is reached after M6.
