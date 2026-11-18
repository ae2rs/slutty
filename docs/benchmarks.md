# slutty vs WezTerm

slutty against WezTerm on throughput, idle cost and memory. Lower is better everywhere.

## Setup

- Mac16,5, Apple M4 Max, 48 GB, macOS 26.6.2, built-in display.
- slutty 0.1.0 (`6885bb0`), release build. WezTerm `20260909-081506-9fa147c9`. vtebench 0.3.1.
- Each window alone and in front on an empty AeroSpace workspace, so it fills the screen. Every window is 214 columns wide. WezTerm has fewer rows, because its tab bar is taller.
- Two setups:
  - **mine**: both terminals with my own config. JetBrains Mono 16, 100 000 lines of scrollback. WezTerm runs its tab bar plugin; slutty shows its tab bar stats.
  - **defaults**: both as shipped, with font size 16 and 100 000 lines of scrollback.
- Raw results: [`docs/bench/`](bench/). Script: [`scripts/bench.sh`](../scripts/bench.sh).

## Throughput

Mean time per vtebench sample, in ms. Each sample writes at least 1 MB.

### mine

slutty 214×60 cells, WezTerm 214×58.

| Benchmark | slutty | WezTerm | slutty |
| --- | --- | --- | --- |
| `cursor_motion` | 8.8 | 12.2 | 1.4× faster |
| `dense_cells` | 8.9 | 10.8 | 1.2× faster |
| `light_cells` | 7.0 | 26.3 | 3.8× faster |
| `scrolling` | 26.1 | 65.9 | 2.5× faster |
| `scrolling_bottom_region` | 18.2 | 89.6 | 4.9× faster |
| `scrolling_bottom_small_region` | 18.0 | 80.4 | 4.5× faster |
| `scrolling_fullscreen` | 7.3 | 24.5 | 3.4× faster |
| `scrolling_top_region` | 18.4 | 86.7 | 4.7× faster |
| `scrolling_top_small_region` | 18.0 | 94.1 | 5.2× faster |
| `unicode` | 9.4 | 31.9 | 3.4× faster |

### defaults

slutty 214×60 cells, WezTerm 214×56.

| Benchmark | slutty | WezTerm | slutty |
| --- | --- | --- | --- |
| `cursor_motion` | 8.8 | 12.1 | 1.4× faster |
| `dense_cells` | 8.9 | 10.9 | 1.2× faster |
| `light_cells` | 7.5 | 25.7 | 3.4× faster |
| `scrolling` | 26.2 | 65.3 | 2.5× faster |
| `scrolling_bottom_region` | 19.2 | 89.9 | 4.7× faster |
| `scrolling_bottom_small_region` | 18.8 | 81.8 | 4.4× faster |
| `scrolling_fullscreen` | 7.5 | 26.7 | 3.6× faster |
| `scrolling_top_region` | 18.4 | 88.3 | 4.8× faster |
| `scrolling_top_small_region` | 18.7 | 96.6 | 5.2× faster |
| `unicode` | 9.1 | 47.4 | 5.2× faster |

### What it measures

- vtebench times how fast the terminal reads output, not how many frames it draws.
- During a flood, slutty draws fewer and cheaper frames, so reading keeps up. WezTerm reads on a thread of its own.
- `unicode` prints 46 000 different symbols, mostly CJK. Every screen is new glyphs.

## Idle

60 s at a still screen, with the window focused, sampled every 20 s with `top`.

| | slutty | WezTerm |
| --- | --- | --- |
| mine: CPU time used | 0.32 s | 0.61 s |
| mine: idle wake-ups | 3 | 9 |
| defaults: CPU time used | 0.00 s | 0.00 s |
| defaults: idle wake-ups | 0 | 1 |

- In **mine**, slutty's tab bar stats refresh every few seconds, and WezTerm's tab bar plugin polls.
- In **defaults**, neither has anything to update.

## Memory

Physical footprint, as Activity Monitor shows it. Resident size in brackets.

| | slutty | WezTerm |
| --- | --- | --- |
| mine: at launch | 401 MB (92 MB) | 294 MB (129 MB) |
| mine: 100 000 lines of scrollback | 625 MB (271 MB) | 603 MB (210 MB) |
| defaults: at launch | 159 MB (93 MB) | 289 MB (116 MB) |
| defaults: 100 000 lines of scrollback | 341 MB (271 MB) | 362 MB (196 MB) |

- The scrollback test prints 200 000 lines of 72 characters; both keep the last 100 000.
- slutty stores every scrollback row at full width, 8 bytes per cell, so its resident size grows with the window's width rather than the text.
- With my config, slutty's footprint is about 240 MB higher than with the defaults. The only difference is the tab bar stats, so they are the likely cause. Not yet explained.

## Reproduce

```sh
scripts/bench.sh            # both setups, about 15 minutes
scripts/bench.sh defaults   # one setup
```

- Needs AeroSpace with an empty workspace, and WezTerm on `PATH`.
- Every run opens a window and brings it to the front. Leave the Mac alone; the script stops on any keyboard or mouse input.
