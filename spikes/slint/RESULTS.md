# Slint spike results

Throwaway prototype. Slint 1.18.1 (latest on crates.io on 2026-10-02), default features (winit backend, FemtoVG OpenGL renderer, software renderer, accessibility). macOS 15.5 (Darwin 24.5.0), Apple Silicon, rustc via cargo 1.98.0. Fixture server on port 8702.

## Machine load caveat

The iced and Tauri spikes were being built on the same machine at the same time. Load average was 5 to 10 during the runtime runs and 14 to 15 during the clean build. Timings are noisier and slower than on an idle machine. The load average is printed next to each block below.

## Measurements

Release profile as in SPEC.md (`opt-level = "s"`, `lto`, `codegen-units = 1`, `panic = "abort"`, `strip`). Five runs per metric unless stated. Median, then min and max.

| Metric | Median | Min | Max |
|---|---|---|---|
| Binary size | 10,460,720 bytes | | |
| first-frame (printed by app) | 163.9 ms | 151.0 | 200.1 |
| Launch-to-exit wall clock, `EXIT_ON=first-frame` | 174.7 ms | 161.3 | 295.9 |
| Idle RSS after 5 s | 72.5 MiB | 71.9 | 73.2 |
| 50 MB `array.json` viewer-ready | 655.3 ms | 618.1 | 739.0 |
| max-frame-gap, from send (the SPEC metric) | 106.5 ms | 103.6 | 146.9 |
| max-frame-gap, ticks after the first frame only | 23.4 ms | 22.7 | 28.0 |
| 50 MB peak RSS (`/usr/bin/time -l`) | 75.2 MiB | 74.4 | 75.8 |
| 1 MB body, one keystroke to next frame | 326 to 374 ms (per-run medians) | 266 | 915 |
| 1 MB body, peak RSS | 528 to 538 MiB | | |
| Release build from clean | 209.7 s wall (582 s user) | | |
| Crates in `Cargo.lock` | 624 | | |
| Crates actually built for this target (`cargo tree -e normal,build`) | 318 | | |

Runtime runs at load average 7.8 at start and 5.4 at end. Clean build at load average 5.0 at start and 14.1 at end.

### Reading the max-frame-gap numbers

The URL is sent at startup, before the event loop runs. So the SPEC metric (from send) includes window creation: the first tick cannot fire before the window exists. That gap is about 100 ms and is startup, not a freeze. The second row counts only gaps whose earlier tick came after the first frame. It shows the UI during the 50 MB download and `Document::build`: the worst gap was 23 to 28 ms against a 16 ms timer. The UI did not freeze.

### First-frame hook

`Window::set_rendering_notifier`, state `RenderingState::AfterRendering`, on the first call. That state fires after the scene is drawn into the back buffer and before the buffer swap. To get past the swap, the notifier schedules `Timer::single_shot(Duration::ZERO, ..)` and the timer callback prints `first-frame` and exits, on the next event loop turn. The raw `AfterRendering` time is printed to stderr as `after-rendering`; it is about 40 ms earlier.

`viewer-ready` uses the same notifier: the first `AfterRendering` after the visible-lines model became non-empty.

### 1 MB editor

The body is a 1,048,576-byte pretty-printed JSON text (72,295 short lines). It is loaded into the `TextEdit` with `REQLITE_SPIKE_BODY_FILE`. The bench then focuses the editor and types 30 characters through the real input path, `Window::dispatch_event(WindowEvent::KeyPressed { text: "x" })` plus `KeyReleased`, one per 16 ms tick. The final body length is 1,048,606 characters, so all 30 keys landed.

Result: typing is not smooth. Each keystroke takes 300 to 370 ms (median), and almost all of it is inside `dispatch_event`, so it is synchronous work on the UI thread (the text input relayouts the whole text). Peak RSS rises to about 530 MiB. With a 100 KB body, a keystroke takes about 26 ms. Cost grows about linearly with text size. A 1 MB body in Slint's `TextEdit` is unusable for editing. A real Reqlite body editor would need a size cap, or a custom virtualized editor.

Raw output of the five 1 MB runs:

```
edit-to-frame ms over 30 edits: median 325.8 min 298.6 max 660.8   key-dispatch median 324.0 max 459.7   peak RSS 532.6 MiB
edit-to-frame ms over 30 edits: median 358.1 min 297.6 max 746.6   key-dispatch median 349.1 max 385.0   peak RSS 537.1 MiB
edit-to-frame ms over 30 edits: median 374.0 min 330.7 max 848.8   key-dispatch median 371.8 max 427.4   peak RSS 528.2 MiB
edit-to-frame ms over 30 edits: median 326.7 min 266.1 max 814.7   key-dispatch median 318.3 max 375.7   peak RSS 537.8 MiB
edit-to-frame ms over 30 edits: median 336.4 min 315.8 max 914.8   key-dispatch median 334.5 max 441.5   peak RSS 537.0 MiB
```

## Commands

```sh
cd spikes/slint
python3 -m http.server 8702 --bind 127.0.0.1 --directory <scratchpad>/fixtures &

# build time and crate count
cargo clean && /usr/bin/time -p cargo build --release
grep -c '^name = ' Cargo.lock
cargo tree -e normal,build --prefix none | sed 's/ (\*)//' | sort -u | wc -l

# 1 MB body file (pretty JSON padded or cut to exactly 1 MiB)
python3 -c "import json; s=json.dumps([{'id':i,'name':f'user {i}','email':f'u{i}@example.com','tags':['a','b'],'active':i%2==0} for i in range(12000)],indent=2); open('body-1mb.json','w').write(s[:1048576].ljust(1048576))"

# every runtime metric, 5 runs each
python3 measure.py body-1mb.json
```

`measure.py` runs the binary under `/usr/bin/time -l` for each case and prints median, min and max.

1. Binary size is `os.path.getsize`.
2. Cold start is `REQLITE_SPIKE_EXIT_ON=first-frame`, wall clock measured around the process.
3. Idle RSS is a launch with no env vars, `sleep 5`, `ps -o rss= -p <pid>`.
4. The 50 MB case is `REQLITE_SPIKE_EXIT_ON=viewer-ready REQLITE_SPIKE_URL=http://127.0.0.1:8702/array.json`, with peak RSS from `/usr/bin/time -l`.
5. The editor case is `REQLITE_SPIKE_EXIT_ON=editor-bench REQLITE_SPIKE_BODY_FILE=<file>`.

Extra hooks this spike adds beyond SPEC.md:

| Variable | Behavior |
|---|---|
| `REQLITE_SPIKE_EXIT_ON=editor-bench` | With `REQLITE_SPIKE_BODY_FILE`, types 30 keys into the body editor and prints keystroke-to-frame times. |
| `REQLITE_SPIKE_BODY_FILE=<path>` | Loads the file into the body editor at startup. |
| `REQLITE_SPIKE_SNAPSHOT=<path>` | 500 ms after viewer-ready, writes `Window::take_snapshot()` as `"<w> <h>\n"` plus raw RGBA, then exits. |
| `REQLITE_SPIKE_TOP=<line>` | Scrolls the viewer to that line when the response arrives. |

## Screenshot

`screencapture -x -l <window id>` failed with `could not create image from window`. A full-screen `screencapture -x` returned only the wallpaper and menu bar, with no app windows, so this session has no screen recording permission. `screenshot.png` was instead made from inside the app with `Window::take_snapshot()`, which re-renders the window into a pixel buffer:

```sh
REQLITE_SPIKE_URL=http://127.0.0.1:8702/array.json REQLITE_SPIKE_SNAPSHOT=snap.rgba target/release/reqlite-slint-spike
python3 rgba2png.py snap.rgba screenshot.png
```

It shows the loaded 50 MB response: `200 · 20 ms · 52428818 bytes` and the pretty-printed JSON in Menlo. A second snapshot with `REQLITE_SPIKE_TOP=1500000` showed `"id": 150000` at the top, so deep scrolling reads the right lines.

## Licence

The `slint` 1.18.1 crate declares `license = "GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0"`. The licence texts ship in the crate under `LICENSES/`. I read the royalty-free text there and checked it against https://raw.githubusercontent.com/slint-ui/slint/master/LICENSES/LicenseRef-Slint-Royalty-free-2.0.md and https://slint.dev/pricing.

1. GPL-3.0-only. Distributing a binary that links Slint under this option makes the combined work GPL-3.0. That does not fit a binary meant to be distributed as MIT OR Apache-2.0.
2. Slint Royalty-free Desktop, Mobile, and Web Applications License 2.0. It grants royalty-free rights to use, modify and distribute Slint "as part of a Desktop, Mobile, or Web Application". Conditions, quoted from section 2: you must either "Display the AboutSlint widget in an 'About' screen or dialog that is accessible from the top level menu of the Application" (or in the splash screen if there is no About screen), or "Display the Slint attribution badge on a public webpage, preferably where the binaries of your Application can be downloaded from". Section 3 limits: no distributing Slint alone, no use in Embedded Systems, no distributing an application "that exposes the APIs, in part or in total, of the Software", and no removing licence notices.
3. Slint Software License 3.0 (the paid plans). The pricing page lists "Attribution to Slint is optional" and "No obligation to provide source code upon distribution" as obligations relieved under paid plans.

What this means for Reqlite. Reqlite's own source can stay MIT OR Apache-2.0. The distributed desktop binary would contain Slint under the royalty-free licence, so it carries an attribution duty (an About screen with `AboutSlint`, or the badge on the download page). The royalty-free licence is a proprietary licence, not an OSI open source licence, so a downstream user who forks Reqlite gets Slint on those terms, not on MIT/Apache terms. Anyone who wants to embed Reqlite's UI in an embedded device, or ship Slint's APIs onward, would fall outside it. I am stating what the texts say; I am not giving legal advice.

## Problems hit

1. No standalone scrollbar widget in std-widgets. `ScrollView` would need a viewport height of millions of pixels. I used a vertical `Slider` as the scrollbar plus a `TouchArea` `scroll-event` for the wheel. A vertical `Slider` has its minimum at the bottom, so the thumb sits at the bottom when the view is at line 0. The scrollbar direction is inverted. A real build needs a custom scrollbar component.
2. A `Slider` `value: root.top` binding breaks the first time the user drags. Fixed with a two-way binding `value <=> root.top`, which needs `top` to be a `float`, not an `int`.
3. `RenderingState` does not implement `PartialEq`. Use `matches!`.
4. `invoke_from_event_loop` needs a `Send` closure, so UI-side state (the `Rc` model and the deliver callback) lives in a `thread_local!` and the closure carries only the `Weak<App>` and the result.
5. Only one rendering notifier per window, so first-frame, viewer-ready and the editor bench share one callback.
6. The 1 MB `TextEdit` problem above. Each keystroke costs about 0.3 ms per KB of text on the UI thread, and memory reaches about 530 MiB.
7. Screen capture of other windows is not permitted in this session. `Window::take_snapshot()` was the workaround.
8. The rest worked first time. Line virtualization is a `VecModel<SharedString>` of the visible slice, refilled from `doc.lines(top, visible_count)` on scroll and on resize (checked each 16 ms tick). The 50 MB load kept ticks under 28 ms.
