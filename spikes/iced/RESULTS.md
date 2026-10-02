# iced spike results

Throwaway prototype. iced 0.14.0 (latest stable on crates.io on 2026-10-02), `tokio` feature, wgpu renderer.
Machine: Apple M4, 10 cores, macOS 15 (Darwin 24.5.0), rustc 1.98.0. Release profile as in SPEC.md.
All numbers are 5 runs: median (min to max). Every run used the release binary.

## Summary

| Metric | Result |
|---|---|
| Binary size | 6,204,608 bytes (5.9 MiB) |
| first-frame | 204.9 ms (179.4 to 388.7) |
| Wall clock launch to exit, `EXIT_ON=first-frame` | 0.22 s (0.19 to 0.62) |
| Idle RSS after 5 s | 70,160 KB (69,344 to 72,336) |
| 50 MB `array.json`, viewer-ready (send at startup, per SPEC) | 1010.0 ms (810.6 to 1143.7) |
| 50 MB, max-frame-gap (send at startup, per SPEC) | 84.1 ms (27.5 to 403.1) |
| 50 MB, peak RSS (`/usr/bin/time -l`) | 81,428,480 bytes (80,543,744 to 81,625,088) |
| 50 MB, send 2 s after launch: send to viewer-ready | 638.3 ms (569.8 to 649.8) |
| 50 MB, send 2 s after launch: max-frame-gap | 18.3 ms (17.7 to 18.5) |
| 50 MB, send 2 s after launch: peak RSS | 83,558,400 bytes (82,804,736 to 83,984,384) |
| 1 MB paste into `text_editor` (UI thread, blocking) | 616.8 ms (569.5 to 700.9) |
| Typing after the 1 MB paste, max frame gap per run | 17.28 ms (17.13 to 34.44) |
| Release build from clean | 136.3 s and 179.9 s wall (two clean builds, see notes) |
| Crates in `Cargo.lock` | 469 |

Response status line shown in the window: `200 · 92 ms · 52428818 bytes`, 5,467,582 lines.

## First-frame hook

`iced::window::frames()` subscription. Its first message is the hook. In `iced_winit-0.14.1/src/lib.rs`, the `RedrawRequested` handler draws the UI, broadcasts the redraw event to subscriptions, and then calls `compositor.present(...)` in the same handler. The subscription message reaches `update` on a later turn of the event loop, so it is after the first present. It is not a compositor "frame shown" callback; iced has none. The subscription is only active until the first frame, during a load, or in the editor bench, because an always-on `frames()` subscription keeps the app redrawing forever.

## Commands

```sh
cd spikes/iced
cargo fetch
cargo clean && /usr/bin/time -p cargo build --release
stat -f %z target/release/reqlite-spike-iced
grep -c '^\[\[package\]\]' Cargo.lock

# cold start
for i in 1 2 3 4 5; do /usr/bin/time -p env REQLITE_SPIKE_EXIT_ON=first-frame ./target/release/reqlite-spike-iced; done

# idle RSS
for i in 1 2 3 4 5; do ./target/release/reqlite-spike-iced & P=$!; sleep 5; ps -o rss= -p $P; kill $P; done

# fixture server
python3 -m http.server 8701 --bind 127.0.0.1 --directory <scratchpad>/fixtures

# 50 MB, per SPEC
for i in 1 2 3 4 5; do /usr/bin/time -l env REQLITE_SPIKE_EXIT_ON=viewer-ready REQLITE_SPIKE_URL=http://127.0.0.1:8701/array.json ./target/release/reqlite-spike-iced; done

# 50 MB, send delayed 2 s past the launch stall (extra diagnostic; prints send-at)
for i in 1 2 3 4 5; do /usr/bin/time -l env REQLITE_SPIKE_SEND_DELAY_MS=2000 REQLITE_SPIKE_EXIT_ON=viewer-ready REQLITE_SPIKE_URL=http://127.0.0.1:8701/array.json ./target/release/reqlite-spike-iced; done

# 1 MB editor bench
for i in 1 2 3 4 5; do REQLITE_SPIKE_EDITOR_BENCH=1 ./target/release/reqlite-spike-iced; done

# screenshot (wid.swift prints the CGWindowNumber for a pid)
REQLITE_SPIKE_URL=http://127.0.0.1:8701/array.json ./target/release/reqlite-spike-iced & P=$!; sleep 4
screencapture -x -l $(swift wid.swift $P) screenshot.png; kill $P
```

Extra env vars, spike only: `REQLITE_SPIKE_DEBUG=1` logs every UI tick gap over 40 ms with its time since launch. `REQLITE_SPIKE_SEND_DELAY_MS` delays the startup send. `REQLITE_SPIKE_EDITOR_BENCH=1` runs the editor bench.

## Raw runs

First-frame, final binary (headline): 388.7, 204.9, 184.8, 179.4, 211.5 ms. Wall 0.62, 0.22, 0.20, 0.19, 0.23 s.
Two earlier batches on near-identical binaries (same startup code): 725.1, 185.0, 194.2, 190.0, 312.1 ms, and 206.1, 153.9, 161.5, 162.4, 160.4 ms. The first launch after a rebuild is slow every time (388 to 725 ms).

viewer-ready per SPEC, final binary: 1010.5, 1143.7, 810.6, 896.4, 1010.0 ms. Gaps 27.5, 403.1, 336.6, 84.1, 30.8 ms.
Earlier batch A: viewer-ready 2155.1, 2183.6, 1928.8, 1574.9, 1376.8 ms. Gaps 315.6, 299.9, 311.9, 79.8, 1085.9 ms. Peak RSS 79.9 to 83.2 MB.
Earlier batch B: viewer-ready 998.5, 1012.3, 1185.5, 1044.2, 1446.4 ms. Gaps 27.1, 34.4, 34.5, 35.2, 18.9 ms. Peak RSS 81.4 to 84.2 MB.

Delayed send, final binary (viewer-ready minus send-at): 638.3, 569.8, 649.8, 643.5, 585.8 ms. Gaps 17.7, 18.3, 18.2, 18.3, 18.5 ms.

Editor bench, per run: paste perform 616.8, 591.7, 674.0, 700.9, 569.5 ms. Typing frame gap p95 16.95, 16.94, 16.91, 16.88, 16.89 ms; max 17.23, 34.44, 17.54, 17.28, 17.13 ms. Keystroke `Content::perform` max 0.37 to 0.50 ms.

## What the max-frame-gap number means

The SPEC number swings from 19 ms to 1086 ms between runs. The 50 MB load does not cause it.

1. With `REQLITE_SPIKE_DEBUG=1`, the large gaps sit between about 250 ms and 1400 ms after launch. They also appear when the server sleeps 3 s and returns 11 bytes. So they happen with no load at all.
2. The tick producer (`time::every` on iced's tokio executor) kept a steady 16 ms cadence during those gaps. The UI thread did not consume the ticks. So the main thread is blocked.
3. A `/usr/bin/sample` of the main thread at launch shows it inside `NSApplication setMainMenu:` called from winit's `applicationDidFinishLaunching` observer. AppKit there calls `+[NSTextView _supportsWritingTools]`, which `dlopen`s the Writing Tools UI framework. The sampler inflates that dlopen (dyld notifies it synchronously), so this proves the code path, not the exact duration in an unsampled run.
4. When the send starts 2 s after launch, the max gap is 17.7 to 18.5 ms in every run, while the 50 MB body downloads and the `Document` builds. The UI does not freeze during the load.

So the honest reading is two numbers. Per SPEC (send at launch), the gap includes a one-time launch stall of up to about 1.1 s on the main thread. Load alone, the gap is one frame.

## 1 MB editor

`text_editor` with `Edit::Paste` of a 1 MB JSON-like string (15,457 lines), done in `update` after the window is up. The paste blocks the UI thread for 570 to 700 ms. That is the cosmic-text buffer insert in `Content::perform`; the next frame after it takes only 2 to 7 ms. After the paste, 300 synthetic keystrokes (one per 16 ms tick, `Insert('x')`, an `Enter` every 40th) at the start of the document keep frames at 16.7 ms median and about 17 ms p95. Keystrokes are synthetic `Action`s, not real key events, because this session has no permission to send input events. A real Cmd+V of 1 MB would freeze the window for over half a second.

## Problems hit

1. No "frame presented" hook in iced. `window::frames()` is the closest. Left on, it redraws continuously, so the spike turns it on and off.
2. Launch stall on macOS. Up to about 1.1 s of the main thread blocked shortly after the first frame, in AppKit menu setup that winit triggers. It dominates the SPEC max-frame-gap number.
3. `time::every` ticks queue up while the UI thread is blocked, so gaps must be timed at arrival in `update` with `Instant::now()`, not with the tick's own `Instant`.
4. `vertical_slider` puts the maximum at the top, so the scrollbar value is inverted (`max - top`). It is a slider, not a scrollbar; it has no thumb sized to the viewport.
5. No virtualized list widget. The viewer is `responsive` (to learn the height), a `column` of `text` lines from `doc.lines(top, height / 18)`, a `mouse_area` with `on_scroll`, and the slider. This worked without a fight.
6. 1 MB paste into `text_editor` blocks the UI thread for about 0.6 s.
7. Build time differs between the two clean builds (136 s, then 180 s). Other spikes were likely building at the same time on this machine; treat it as about 2 to 3 minutes. `cargo fetch` was done before timing.
8. Not verified: wheel scrolling and slider dragging were not driven, because this session cannot send input events. The screenshot shows the loaded response at line 1.
