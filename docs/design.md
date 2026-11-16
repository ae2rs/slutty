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
