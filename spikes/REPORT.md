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
| Idle RAM under 50 MB (the original budget) | miss (57) | miss (95) | miss (72 footprint, 157 RSS) |
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

## Idle memory after issue #1

[Issue #1](https://github.com/officialaritro/reqlite/issues/1) found that an empty iced window (`iced/examples/hello.rs`) already uses 55 MB at 1000×800 on a 2× display. About 25 MB is the window's 2 drawables, and about 30 MB is the toolkit's fixed cost. So the budget now has two parts: total idle footprint under 60 MB at 1000×800 on a 2× display, and footprint minus drawables under 35 MB. The iced spike meets both, at 57 MB and 32 MB.

The spike sometimes kept a third drawable. It subscribed to every frame until its first frame, so it redrew back to back at startup. With that subscription limited to the first-frame hook, 6 of 6 runs at 800×600 kept 2 drawables (14.9 MB, 44 MB footprint). Before the change, 4 of 6 runs kept 3 (22.4 MB, 52 MB). The production GUI must redraw only when its state changes.

## Next

1. Build the production GUI crate on iced, and add `scripts/gui_idle.py` and a cold start check to CI on macOS.
