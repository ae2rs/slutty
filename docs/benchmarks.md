# slutty vs WezTerm

slutty against WezTerm on throughput, idle cost and memory. Lower is better everywhere.

## Setup

- Mac16,5, Apple M4 Max, 48 GB, macOS 26.6.2.
- slutty 0.1.0 (`60963a4`), release build. WezTerm `20260909-081506-9fa147c9`. vtebench 0.3.1.
- Each window alone on an empty AeroSpace workspace, so it fills the screen.
- Two setups:
  - **mine**: both terminals with my own config. JetBrains Mono 16, 100 000 lines of scrollback. WezTerm runs its tab bar plugin; slutty its tab bar stats.
  - **defaults**: both as shipped, with font size 16 and 100 000 lines of scrollback.
- Raw results: [`docs/bench/`](bench/). Script: [`scripts/bench.sh`](../scripts/bench.sh).

## Throughput

Mean time per vtebench sample, in ms. Each sample writes at least 1 MB.

### mine

slutty 267×66 cells, WezTerm 267×64.

| Benchmark | slutty | WezTerm | slutty |
| --- | --- | --- | --- |
| `cursor_motion` | 8.3 | 12.0 | 1.4× faster |
| `dense_cells` | 8.7 | 10.7 | 1.2× faster |
| `light_cells` | 7.0 | 26.2 | 3.7× faster |
| `scrolling` | 28.2 | 66.1 | 2.3× faster |
| `scrolling_bottom_region` | 19.7 | 86.9 | 4.4× faster |
| `scrolling_bottom_small_region` | 19.1 | 79.9 | 4.2× faster |
| `scrolling_fullscreen` | 7.0 | 24.5 | 3.5× faster |
| `scrolling_top_region` | 19.3 | 86.4 | 4.5× faster |
| `scrolling_top_small_region` | 19.1 | 94.0 | 4.9× faster |
| `unicode` | 9.7 | 33.9 | 3.5× faster |

### defaults

slutty 214×60 cells, WezTerm 267×61. slutty's window came out smaller in this run, so this table favours it a little.

| Benchmark | slutty | WezTerm | slutty |
| --- | --- | --- | --- |
| `cursor_motion` | 8.3 | 12.2 | 1.5× faster |
| `dense_cells` | 8.9 | 10.9 | 1.2× faster |
| `light_cells` | 7.0 | 26.3 | 3.8× faster |
| `scrolling` | 26.2 | 65.8 | 2.5× faster |
| `scrolling_bottom_region` | 18.2 | 86.9 | 4.8× faster |
| `scrolling_bottom_small_region` | 19.1 | 80.4 | 4.2× faster |
| `scrolling_fullscreen` | 8.0 | 24.6 | 3.1× faster |
| `scrolling_top_region` | 19.1 | 87.0 | 4.6× faster |
| `scrolling_top_small_region` | 18.9 | 94.8 | 5.0× faster |
| `unicode` | 9.5 | 164.8 | 17.3× faster |

### What it measures

- vtebench times how fast the terminal reads output, not how many frames it draws.
- During a flood, slutty draws fewer and cheaper frames, so reading keeps up. WezTerm reads on a thread of its own.
- `unicode` prints 46 000 different symbols, mostly CJK. Every screen is new glyphs.

## Idle

60 s at a still screen, sampled every 20 s with `top`.

| | slutty | WezTerm |
| --- | --- | --- |
| mine: CPU time used | 0.00 s | 0.78 s |
| mine: idle wake-ups | 0 | 13 |
| defaults: CPU time used | 0.01 s | 0.00 s |
| defaults: idle wake-ups | 0 | 2 |

- The windows were not focused: macOS does not activate an app a script starts.
- So slutty's stats, which refresh only while focused, were off. WezTerm's tab bar plugin keeps polling either way.

## Memory

Physical footprint, as Activity Monitor shows it. Resident size in brackets.

| | slutty | WezTerm |
| --- | --- | --- |
| mine: at launch | 187 MB (88 MB) | 380 MB (123 MB) |
| mine: 100 000 lines of scrollback | 421 MB (309 MB) | 675 MB (198 MB) |
| defaults: at launch | 148 MB (85 MB) | 261 MB (109 MB) |
| defaults: 100 000 lines of scrollback | 337 MB (264 MB) | 431 MB (183 MB) |

- The scrollback test prints 200 000 lines of 72 characters; both keep the last 100 000.
- slutty has the smaller footprint but the larger resident size.

## Reproduce

```sh
scripts/bench.sh            # both setups, about 15 minutes
scripts/bench.sh defaults   # one setup
```

- Needs AeroSpace with an empty workspace, and WezTerm on `PATH`.
- Every run opens a window. Leave the Mac alone; the script stops on any keyboard or mouse input.
