# Tauri v2 + Svelte spike results

Machine: Apple Silicon (arm64), macOS 15 (Darwin 24.5.0), rustc 1.98.0, node 25.6.0, pnpm 11.22.0.
Versions: tauri 2.12.1, tauri-build 2.7.1, @tauri-apps/cli 2.12.1, @tauri-apps/api 2.12.1, svelte 5.57.1, @sveltejs/vite-plugin-svelte 7.3.1, vite 8.3.2.
All numbers are from the release build (`opt-level = "s"`, `lto = true`, `codegen-units = 1`, `panic = "abort"`, `strip = true`). Five runs, median (min to max), unless noted.

## Summary

| Measurement | Result |
|---|---|
| Binary size (app executable, no bundle) | 4,801,200 bytes (4.6 MiB) |
| first-frame | 343 ms (309 to 387) |
| Launch-to-exit wall time, `EXIT_ON=first-frame` | 363 ms (328 to 396) |
| Idle RSS, app alone | 76 MB (75 to 76) |
| Idle RSS, app + WebKit helpers | 157 MB (156 to 159) |
| 50 MB `array.json`, viewer-ready | 732 ms (692 to 871) |
| 50 MB, max-frame-gap (send at startup, as SPEC says) | 51 ms (31 to 72) |
| 50 MB, max-frame-gap (send after first paint) | 18 ms (17 to 18) |
| 50 MB, peak RSS app alone (`/usr/bin/time -l`) | 79 MB (78 to 80) |
| 50 MB, peak RSS app + WebKit helpers (50 ms sampler) | 159 MB (158 to 160) |
| 1 MB body in the editor, one inserted character to second frame | 67 ms (66 to 67 medians; max single keystroke 68 ms) |
| Viewer scroll jump to painted frame (41 random jumps per run) | 17 ms median, 20 ms max |
| Release build from clean | 125.4 s wall (cargo 2m 03s, vite 91 ms) |
| Crates in `Cargo.lock` | 464 |
| npm packages | 77 in `pnpm-lock.yaml` (44 installed on this machine; the rest are other platforms' optional binaries). Runtime npm dependency: `@tauri-apps/api` only. |
| Front-end bundle | 39.4 KB JS + 0.7 KB CSS (`dist/` is 48 KB) |

## How each hook works

1. `started` is the first statement in `main`.
2. **first-frame.** The Svelte `onMount` handler calls `requestAnimationFrame` twice, nested, then invokes the `report` command. The second callback runs only after the frame scheduled by the first has been painted. Rust prints `first-frame <ms since started>` and exits 0. This is the earliest page-level hook that proves WebKit painted the window.
3. **viewer-ready.** After `send` resolves, the front end fetches the visible lines with `lines(top, count)`, waits for Svelte to flush the DOM, then waits two `requestAnimationFrame` callbacks so the rows are on screen. Then it invokes `report`, and Rust prints `viewer-ready <ms since started>` and `max-frame-gap <ms>`.
4. **max-frame-gap.** A `requestAnimationFrame` loop in the webview starts when Send is pressed and stops at viewer-ready. The largest gap between callbacks is reported to Rust. rAF in WKWebView is driven by the display (about 16.7 ms here), so it is the 16 ms tick SPEC asks for.
5. **Editor.** `REQLITE_SPIKE_EXIT_ON=editor` fills the textarea with 1 MB, then inserts one character 60 times at the middle with `setRangeText` and an `input` event (which Svelte's `bind:value` handles). Each sample is the time from the insert to the second rAF callback after it.
6. **Scroll.** `REQLITE_SPIKE_EXIT_ON=scroll` loads `array.json`, then jumps the scroll position 41 times across the document, timing each jump to the next painted frame.

Extra env vars (spike-only): `REQLITE_SPIKE_SEND_AFTER_PAINT=1` waits two rAFs before the startup send; `REQLITE_SPIKE_DEBUG=1` adds timing lines on stderr; `REQLITE_SPIKE_SCROLL_TO=<0..1>` scrolls after the load; `REQLITE_SPIKE_EDITOR_BYTES`, `REQLITE_SPIKE_EDITOR_NO_EVENT`.

## Design as built

1. Rust holds the `Document` in managed state (`Mutex<Option<Arc<Document>>>`). The body never crosses to the webview.
2. One background thread runs a current-thread tokio runtime. The async `send` command builds the `Request`, calls `reqlite_engine::resolve`, and sends the `Resolved` with a oneshot reply over an mpsc channel. The worker calls `reqlite_engine::send`, builds the `Document` in `spawn_blocking`, and replies with the `Arc<Document>` plus status, elapsed and bytes. The command stores the document and returns `{status, elapsed_ms, bytes, line_count}`.
3. `lines(start, count)` is a sync command. In Tauri v2 sync commands run on the main thread, which SPEC allows for line reads.
4. The viewer is a virtual list. A spacer div gives the scroll range, and only `ceil(height / 18) + 1` rows exist, absolutely positioned at the scroll offset.

## Commands

```sh
cd spikes/tauri
pnpm install
(cd src-tauri && cargo clean)
rm -rf dist
/usr/bin/time -p pnpm tauri build --no-bundle      # build time; binary at src-tauri/target/release/reqlite-tauri-spike
stat -f %z src-tauri/target/release/reqlite-tauri-spike
grep -c '^\[\[package\]\]' src-tauri/Cargo.lock

python3 -m http.server 8703 --bind 127.0.0.1 --directory <fixtures dir> &
python3 measure.py                    # first_frame, idle, big, editor, scroll (RUNS=5 by default)
python3 measure.py big_after_paint    # the 50 MB case with the send after first paint
```

`measure.py` does the following.

1. **first_frame.** Runs `REQLITE_SPIKE_EXIT_ON=first-frame <bin>` and records the printed ms and the wall time from spawn to exit.
2. **idle.** Snapshots `ps -axo pid=,rss=,comm=`, launches with no env vars, waits 5 s, snapshots again. App RSS is `ps` RSS of the app pid. Helpers are new pids whose command contains `com.apple.WebKit.` (it found exactly one each of WebContent, Networking and GPU every run: about 33, 15 and 33 MB).
3. **big.** Runs `/usr/bin/time -l <bin>` with `REQLITE_SPIKE_URL=http://127.0.0.1:8703/array.json REQLITE_SPIKE_EXIT_ON=viewer-ready`. Every 50 ms it reads one `ps` snapshot and sums the app and the new WebKit helpers. It reports the `time -l` maximum RSS, the sampler's peak of the app, the sampler's peak of the sum, and an upper bound (time -l peak plus each helper's own peak). The last two agreed within 1 MB in every run.
4. **editor** and **scroll.** Run the hooks above and print their lines.

Raw output of the measured runs (2026-10-02, load average 2 to 3):

```
first-frame ms: median 343, min 309, max 387  (runs: 387, 326, 359, 309, 343)
launch-to-exit wall ms: median 363, min 328, max 396  (runs: 396, 346, 377, 328, 363)
idle RSS app MB: median 76, min 75, max 76  (runs: 75, 76, 76, 76, 76)
idle RSS app+WebKit MB: median 157, min 156, max 159  (runs: 156, 159, 156, 157, 157)
viewer-ready ms: median 732, min 692, max 871  (runs: 871, 713, 692, 759, 732)
max-frame-gap ms: median 51, min 31, max 72  (runs: 72, 57, 51, 31, 35)
peak RSS app, /usr/bin/time -l MB: median 79, min 78, max 80  (runs: 78, 79, 80, 79, 78)
peak RSS app, 50 ms sampler MB: median 79, min 78, max 79  (runs: 78, 79, 79, 79, 78)
peak RSS app+WebKit, 50 ms sampler (max of summed sample) MB: median 159, min 158, max 160  (runs: 158, 159, 159, 160, 159)
peak RSS app(time -l) + per-helper peaks MB (upper bound): median 159, min 158, max 160  (runs: 158, 159, 160, 160, 159)
editor run (x5): editor-bytes 1048636 | keystroke-to-second-frame median 66 to 67 ms, max 67 to 68 ms
scroll run (x5): scroll-jump-to-frame median 17.0 ms, max 18.0 to 20.0 ms over 41 jumps | last-top 4784111 rows 28 of 5467582
== big_after_paint
viewer-ready ms: median 743, min 738, max 802  (runs: 802, 738, 743, 759, 743)
max-frame-gap ms: median 18, min 17, max 18  (runs: 17, 18, 18, 18, 18)
peak RSS app+WebKit, 50 ms sampler MB: median 160, min 159, max 160
```

Other fixtures, one run each with the send after first paint: `nested.json` viewer-ready 1539 ms, max-frame-gap 18 ms; `string.json` viewer-ready 615 ms, max-frame-gap 18 ms.

## Reading the numbers

1. **The 50 MB load does not freeze the UI.** With the send after first paint, the largest frame gap is 18 ms, which is one display frame. The 31 to 72 ms gap in the SPEC-exact run comes from startup, not from the load. The send starts at about 300 ms, before the first paint at about 340 ms, so the first gap includes the window's first paint. Debug traces (`REQLITE_SPIKE_DEBUG=1`) showed the long gap always started at 0 to 1 ms after the send.
2. **Peak RSS barely moves.** The body stays in Rust and in temp files. The app goes from 76 MB idle to 79 MB at peak, and the WebKit helpers do not grow, because the webview only ever holds about 30 lines.
3. **The WebKit helpers double the memory.** About 81 MB idle sits in three processes outside the app. `ps` RSS counts shared framework pages in each process, so the total overstates the private cost. Activity Monitor's "memory" (phys_footprint) would be a fairer number. I did not measure it.
4. **The 1 MB editor lags.** One inserted character takes about 67 ms to reach the second frame, against 33 ms for a 10 KB or 100 KB body. That is about two dropped frames per keystroke: usable, but visibly slower. The cost is WebKit's `<textarea>`, not Svelte. With the `input` event suppressed, so Svelte's binding never runs, the number is the same 67 ms.
5. **Scrolling is one frame per jump.** A jump to any position in 5.47 million lines paints in 17 ms. `doc.lines` plus the IPC round trip fits inside the frame.

## Problems hit

1. **WKWebView stops rAF when the window is hidden, and that broke the measurements.** Runs launched from a terminal open the window behind the frontmost app. While the user worked in Chrome, WebKit marked the page `hidden` and stopped `requestAnimationFrame`. Some runs stalled for 1 to 23 s, and some never finished. A `visibilitychange` log confirmed it (`hidden@102 visible@1540` matched a 1466 ms stall). `alwaysOnTop` alone did not fix it. The fix, only when a `REQLITE_SPIKE_EXIT_ON` hook is set, is `set_always_on_top(true)` plus the private WebKit SPI `-[WKWebView _setWindowOcclusionDetectionEnabled:NO]`, called through `objc2::msg_send!` on the pointer from `WebviewWindow::with_webview`. After that, 8 of 8 repeat runs and all measured runs had no visibility changes and no stalls. Idle RSS runs have no env vars, so they run without this tweak. For a real app this throttling is good behavior. For a benchmark it is a trap, and the other two toolkits do not have it.
2. **WebKit cannot lay out a 98 million px spacer.** 5,467,582 lines at 18 px is about 98 M px, beyond WebKit's layout limit (about 2^25 px). The spacer is capped at 8 M px, and the scroll offset maps proportionally onto the line range. One pixel of scroll then moves about 0.7 lines at the far end. A real build needs this or a custom scrollbar.
3. **`process::exit` leaks `Document`'s temp file.** The hooks exit with `std::process::exit(0)`, which skips destructors, so each early run left an 82.5 MB pretty-printed temp file in `$TMPDIR`. I fixed it by dropping the document before exit, and checked that the `.tmp` count stays at 92 across a run. The leaked files from my early runs are still in `$TMPDIR`. I did not delete them, because files from the sibling spikes sit in the same directory and I could not tell them apart. The iced and Slint spikes will hit the same leak if they exit the same way.
4. **Sync commands run on the main thread in Tauri v2.** That is fine for `lines`, but anything heavier must be `async` or `#[tauri::command(async)]`.
5. **Async commands that borrow `State` must return `Result`.** This is the documented Tauri limitation (tauri-apps/tauri#2533).
6. **No synthetic real keystrokes.** `osascript` was refused (`osascript is not allowed to send keystrokes. (1002)`), so the editor test uses `setRangeText` plus an `input` event, not real key events. Typing by hand was not tested. The Send button path was also not clicked. The startup `REQLITE_SPIKE_URL` path calls the same `send()` function.
7. **pnpm 11 supply-chain gate.** `pnpm install` added `vite@8.3.2` to `minimumReleaseAgeExclude` in a local `pnpm-workspace.yaml`, because the release was too new for the default age policy.
8. **Build-time caveat.** The clean build ran while the machine's load average was 9 to 10, probably from the sibling spikes building at the same time. Treat 125 s as an upper-ish bound. Cargo sources were already downloaded. The later `objc2` dependency was already in the tree (wry uses it), so the crate count did not change.
9. **Icon required.** `tauri::generate_context!` needs an icon file. I generated a plain 128x128 RGBA PNG.

## Screenshot

`screenshot.png` shows the window after loading `array.json` (`200 · 35 ms · 52428818 bytes`, lines 1 to 28 of 5467582), captured with `screencapture -x -o -l <window id>`. I got the window id with a small CoreGraphics `CGWindowListCopyWindowInfo` Swift script. A second capture with `REQLITE_SPIKE_SCROLL_TO=1.0` showed the last lines of the document, which confirms the scroll mapping reaches the end.
