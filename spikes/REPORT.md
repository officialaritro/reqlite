# GUI toolkit decision

**Decision: iced.** It has the smallest idle memory footprint, the fastest cold start, a pure-Rust single process and an MIT licence. Like the other two, it misses the idle RAM budget as built. The miss comes from window frame buffers and GPU driver memory, which can be tuned. Our own code is not the cause.

The three spikes in this directory are throwaway prototypes. [SPEC.md](SPEC.md) is the contract they share. Each spike's `RESULTS.md` has its own notes and commands.

## Measurements

One script, [measure_all.py](measure_all.py), measured the three release binaries one after another on an idle Apple M4 (macOS 15.5, load average about 2.7). The table gives the median and, in brackets, the range: 5 runs for start-up, 3 runs for RAM and the 50 MB load. Total RAM adds every WebKit helper process the app started.

| | iced 0.14 | Slint 1.18 | Tauri 2.12 + Svelte 5 |
|---|---|---|---|
| Binary | 5.9 MB | 10.0 MB | 4.6 MB |
| First frame | 145 ms (120 to 293) | 167 ms (157 to 244) | 325 ms (290 to 353) |
| Idle footprint, total (Activity Monitor "Memory") | **57 MB** (56 to 69) | 95 MB (94 to 95) | 72 MB (70 to 138) |
| Idle RSS, total | 68 MB | 73 MB | 157 MB |
| 50 MB JSON: viewer ready | 764 ms (660 to 1341) | 654 ms (638 to 689) | 754 ms (729 to 755) |
| 50 MB JSON: peak RSS, total | 77 MB | 75 MB | 160 MB |
| 1 MB body in the editor | 617 ms freeze on paste, then about 17 ms per key | 330 ms freeze on every key | 67 ms per key |
| Licence | MIT | GPL-3.0, or royalty-free with an attribution badge (not OSI) | MIT or Apache-2.0 |
| Processes | 1 | 1 | 1 plus 3 WebKit helpers |

## Budgets

| Budget | iced | Slint | Tauri |
|---|---|---|---|
| Idle RAM under 50 MB | miss (57) | miss (95) | miss (72 footprint, 157 RSS) |
| Binary under 25 MB | pass | pass | pass |
| Cold start under 300 ms | pass | pass | miss |
| 50 MB JSON without freezing | pass | pass | pass |

On "without freezing", the worst frame gap after the first frame stayed under 30 ms in all three. The larger `max-frame-gap` figures in the spike reports come from the window opening: AppKit menu setup blocks the main thread at launch. They do not come from the 50 MB load.

## Why iced

1. **Memory.** It has the lowest footprint. `vmmap` puts its 56 MB of dirty memory at about 25 MB of window IOSurface buffers (the frames it draws), about 12 MB of GPU driver memory and about 10 MB of heap. Fewer frame buffers and a smaller default window are the levers to get under 50 MB. The software renderer (`ICED_BACKEND=tiny-skia`) is worse, at about 115 MB, because it keeps a full-resolution CPU frame buffer.
2. **One language, one process.** The engine, the viewer and the UI are all Rust. Tauri adds a JavaScript build, three WebKit processes on macOS and WebView2 on Windows, and its RAM cost lives in processes the app does not control.
3. **The editor.** Typing stays at frame rate after a large paste. Slint blocks on every key. Tauri is usable, but slower per key.
4. **Licence.** Slint's licences do not fit an MIT OR Apache-2.0 project without either the GPL or a required badge.

## What iced costs

1. No real scrollbar widget for a virtual list. The spike uses a `vertical_slider`. The production viewer needs its own scrollbar.
2. A 1 MB paste blocks for about 0.6 s. If that matters in use, back the editor with `ropey`, per `docs/research/ropes.md` B2.
3. No "frame presented" callback. The `window::frames()` subscription has to be switched off when idle, or iced redraws forever.

## Next

1. Bring the iced idle footprint under 50 MB: frame buffer count, default window size, font database. Measure each change with `measure_all.py`.
2. Add the GUI idle RAM and cold start checks to `scripts/budgets.py` once the production GUI crate exists.
