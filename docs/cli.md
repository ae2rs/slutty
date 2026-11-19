# slutty cli

`slutty cli` drives the slutty server from a shell or a program. It is the way in for tools such as rho. This page is the contract: what a program inside a pane can rely on.

## Finding slutty

Every pane's program gets these variables:

| Variable | Value |
| --- | --- |
| `TERM_PROGRAM` | `slutty` |
| `SLUTTY_PANE` | The pane's id, such as `3`. |
| `SLUTTY_SOCKET` | The path of the server's socket. |

- A program is in slutty when `SLUTTY_PANE` and `SLUTTY_SOCKET` are set.
- Run the `slutty` binary that runs the server. Inside a pane, `slutty` is on `PATH` only if the user put it there. The app bundle's binary is `slutty.app/Contents/MacOS/slutty`.
- Ids are numbers. They are never reused while the server runs. A new server starts over.

## Calling it

```
slutty cli [--socket PATH] <command> [options] [-- PROG ARG…]
```

### Which server

1. `--socket PATH`.
2. Else `$SLUTTY_SOCKET`.
3. Else `~/Library/Application Support/slutty/mux.sock`.

The CLI never starts a server. With none running it fails: `no slutty server running`.

### Which pane

A command that takes `--pane-id` acts on:

1. `--pane-id N`.
2. Else `$SLUTTY_PANE`, but only when `$SLUTTY_SOCKET` names the same socket as the CLI targets.
3. Else the focused pane: the pane with the keys in the window focused last.

So a program in a pane acts on its own pane by default.

### Output and exit codes

- Commands that make a pane print its id alone on a line.
- Other commands print nothing on success, except `list` and `get-text`.
- Errors go to stderr, as `slutty cli: <reason>`.

| Code | Meaning |
| --- | --- |
| 0 | Done. |
| 1 | The server refused, or none runs. |
| 2 | Bad arguments. |

## Commands

### Panes and tabs

| Command | Does |
| --- | --- |
| `list [--format table\|json]` | Lists every pane. See [JSON](#list-json). |
| `spawn [--window-id N \| --new-window] [--pane-id N] [--cwd DIR] [-- PROG…]` | Opens a tab in the pane's window, or a window. Prints the pane id. |
| `split-pane [--pane-id N] [--right \| --down] [--percent P] [--cwd DIR] [-- PROG…]` | Splits a pane, below unless `--right`. The new pane takes `P`% (1–99), else half. Prints its id. |
| `activate-pane [--pane-id N]` | Shows the pane and gives it the keys. |
| `activate-pane-direction [--pane-id N] left\|right\|up\|down` | Gives the keys to the pane next to it. From a float, back to the tiled panes. |
| `adjust-pane-size [--pane-id N] --direction D [--cells N]` | Moves the nearest divider. A float grows instead. |
| `zoom-pane [--pane-id N]` | Zooms the pane, or ends the zoom. |
| `kill-pane [--pane-id N]` | Closes the pane, whatever runs in it. |
| `kill-server` | Closes every pane and stops the server. |

- Without `PROG`, the login shell runs.
- `--cwd` defaults to the directory of the pane acted on. A relative `--cwd` is read from the CLI's directory.

### Floating panes

| Command | Does |
| --- | --- |
| `float-pane [--pane-id N] [PLACE] [--hidden] [--cwd DIR] [-- PROG…]` | Opens a float over the pane's tab. Prints its id. |
| `place-pane [--pane-id N] [PLACE]` | Moves or resizes a float. What is not given stays. |
| `hide-pane [--pane-id N]` | Hides a float. Its program runs on. |
| `show-pane [--pane-id N]` | Shows a float on top and gives it the keys. |

`PLACE` is any of:

| Option | Value | Default |
| --- | --- | --- |
| `--anchor` | `top-left`, `top`, `top-right`, `left`, `center`, `right`, `bottom-left`, `bottom`, `bottom-right` | `center` |
| `--width` | `40%` of the tab, or `40` cells | `80%` |
| `--height` | Same | `80%` |
| `--x-offset` | Cells right of the anchored spot; negative goes left | `0` |
| `--y-offset` | Cells below; negative goes up | `0` |

- The float is sized first, then put at its anchor, then moved by the offset. It always stays inside the tab, at least one cell.
- A new float takes the keys, unless `--hidden`.
- `hide-pane` and `show-pane` on a tiled pane fail. So do `split-pane` and `zoom-pane` on a float.
- A float closes when its program exits, and with its tab.

### Text

| Command | Does |
| --- | --- |
| `send-text [--pane-id N] [--no-paste] [TEXT]` | Sends `TEXT`, or stdin, to the pane's program. |
| `get-text [--pane-id N] [--start-line L] [--end-line L]` | Prints lines as plain text. |

- `send-text` sends a bracketed paste when the program asked for it, unless `--no-paste`. To press enter, end the text with `\r` and use `--no-paste`.
- `get-text` counts lines from the top of the screen: 0 is the top row, negative lines are scrollback. The default is the whole screen. Each line ends in a newline, and trailing spaces are trimmed.

## List JSON

`list --format json` prints an array, one object per pane: window by window, tab by tab, tiled panes in reading order, then floats from the bottom up.

```json
{
  "window_id": 2,
  "tab_index": 0,
  "pane_id": 5,
  "x": 48, "y": 0, "cols": 32, "rows": 19,
  "title": "htop",
  "cwd": "/Users/lucas",
  "process": "htop",
  "rho": null,
  "active": true,
  "focused": true,
  "zoomed": false,
  "floating": {
    "anchor": "top-right", "width": "40%", "height": "80%",
    "x_offset": 0, "y_offset": 0, "hidden": false
  }
}
```

| Field | Meaning |
| --- | --- |
| `window_id`, `tab_index`, `pane_id` | Where it is. Tabs count from 0. |
| `x`, `y`, `cols`, `rows` | Its cells in the tab. A pane hidden by a zoom, or a hidden float, keeps the place it would have. |
| `title` | The title the program set, else the foreground program's name. |
| `cwd` | The directory the shell reported (OSC 7), else the foreground program's, or `null`. |
| `process` | The foreground program's name, or `null`. |
| `rho` | rho's state from its `rho_activity` user variable: `working`, `question`, `stopped`, `unread`, or `null`. |
| `active` | The pane with the keys in its tab. |
| `focused` | The pane commands act on when they name none. |
| `zoomed` | Its tab is zoomed on it. |
| `floating` | `null` for a tiled pane, else where it floats and whether it is hidden. |

New fields may be added. Existing ones keep their meaning.

## Keys for floating panes

| Keys | Action |
| --- | --- |
| `leader f` | Show or hide the tab's floats; with none, open one. |
| `leader F` | Open a float. |
| `leader tab` | Move the keys through the floats shown, then back to the tiled panes. |
| `leader w` | The float table: `hjkl` move, `HJKL` resize, `1`–`9` anchor as on a numpad, `escape` ends. |

The actions, for `[keys.bind]`: `toggle-floating`, `new-floating` (with `anchor`, `width`, `height`), `cycle-floating`, `move-pane` (with `direction`, `cells`) and `anchor-pane` (with `anchor`).

## For rho

A starting point, not a design.

- **Detect.** `TERM_PROGRAM=slutty` with `SLUTTY_PANE` set. rho already reads `TERM_PROGRAM`.
- **Open a side pane.** `slutty cli float-pane --anchor top-right --width 40% -- <cmd>` prints the new pane's id. `split-pane --right --percent 40` gives a tiled one instead.
- **Keep one around.** Open it `--hidden`, keep the id, then `show-pane` and `hide-pane` it. Check it still exists with `list --format json` before reusing the id.
- **Talk to it.** `send-text --pane-id N --no-paste $'cmd\r'` runs a command in a shell. `get-text --pane-id N --start-line -200` reads what it printed.
- **Report state.** Set the `rho_activity` user variable (OSC 1337 `SetUserVar`); it shows in the tab bar and in `list` as `rho`.
- **Versions.** The CLI and the server must be the same build. A mismatch fails with exit 1 and names both versions.
