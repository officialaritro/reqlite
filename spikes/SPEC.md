# GUI spike contract

Three throwaway prototypes build the same screen, so the comparison measures the toolkit and nothing else. The spike code exists to make one decision: which GUI toolkit Reqlite uses. It is not production code.

| Spike | Directory | Fixture server port |
|---|---|---|
| iced | `spikes/iced` | 8701 |
| Slint | `spikes/slint` | 8702 |
| Tauri v2 + Svelte (Vite, static build, no SvelteKit SSR) | `spikes/tauri` | 8703 |

## Rules

1. Each spike is a standalone Cargo project. Its `Cargo.toml` has an empty `[workspace]` table. It depends on the real crates by path: `reqlite-format`, `reqlite-engine`, `reqlite-viewer` under `../../crates/` (for Tauri, adjust the relative path from `src-tauri`).
2. Do not edit anything outside your spike directory. Do not run git write commands.
3. Use the latest stable release of the toolkit. Check the version on crates.io or npm. Never guess an API: read the toolkit's docs (context7 MCP, or the crate source in `~/.cargo/registry/src`).
4. Release profile for the measured build: `opt-level = "s"`, `lto = true`, `codegen-units = 1`, `panic = "abort"`, `strip = true`, the same as the main workspace.

## The screen

1. Top row: a method field (text input or picker, default `GET`), a URL text input, a Send button.
2. A multi-line request body editor, using the toolkit's own text editing widget.
3. A status line: `200 · 123 ms · 52428818 bytes`, or the error text.
4. A response viewer over `reqlite_viewer::Document`. It renders only the visible lines: call `doc.lines(top, visible_count)` for the current scroll position, never the whole body. Scrolling covers `doc.line_count()` lines. A monospace font.

## Threading (same in all three)

1. The UI thread never does IO and never builds a `Document`.
2. One background thread runs a tokio runtime. The UI sends it a `reqlite_engine::Resolved` over a channel. Build the request as `reqlite_format::Request { version: 1, name: "spike".into(), method: Method::try_from(..)?, url, headers: Default::default(), query: Default::default(), body }` and resolve it with `reqlite_engine::resolve(&req, &reqlite_format::Environment::default())`.
3. The background side calls `reqlite_engine::send`, then builds the `Document` with `Document::build(resp.body.reader()?)` inside `tokio::task::spawn_blocking`, then hands the UI an `Arc<Document>` plus status, elapsed time and byte count. Use the toolkit's own way to deliver a result to the UI thread (an iced `Task`, Slint `invoke_from_event_loop`, an async Tauri command).
4. Line reads for scrolling (`doc.lines`) take under 1 ms for 60 lines, so they may run on the UI thread.

## Measurement hooks (environment variables, same in all three)

Capture `let started = std::time::Instant::now();` as the first statement in `main`.

| Variable | Behavior |
|---|---|
| `REQLITE_SPIKE_URL=<url>` | At startup, put the URL in the field and send it at once. |
| `REQLITE_SPIKE_EXIT_ON=first-frame` | After the first frame is on screen, print `first-frame <ms since started>` to stdout and exit with code 0. |
| `REQLITE_SPIKE_EXIT_ON=viewer-ready` | When the viewer first shows lines of the response, print `viewer-ready <ms since started>` and `max-frame-gap <ms>`, then exit with code 0. |

`max-frame-gap` is the longest gap between two UI ticks, from the send until the viewer is ready. Drive the ticks with a 16 ms timer on the UI thread (an iced `time::every` subscription, a Slint `Timer`, `requestAnimationFrame` in the webview, reported back to Rust). It shows whether the UI froze during a 50 MB load.

"First frame on screen" means the toolkit's earliest hook after the window is drawn, not after the window is created. Say which hook you used.

## Fixtures

Three 50 MB JSON files are in `/private/tmp/claude-501/-Users-aritro404-Documents-reqlite/dcc8b791-8753-4aa7-b4f9-aea6649e5034/scratchpad/fixtures/` (`array.json`, `nested.json`, `string.json`). Serve them with `python3 -m http.server <your port> --bind 127.0.0.1 --directory <that dir>`. Stop the server when you are done.

## What to measure and report

Measure on the release build. Use 5 runs and report the median, plus the minimum and maximum.

1. Binary size in bytes. For Tauri, the app executable, built without an installer bundle.
2. Cold start: the `first-frame` number, and the wall-clock time from launch to exit for `EXIT_ON=first-frame`.
3. Idle RAM: launch with no env vars, wait 5 s, read RSS with `ps -o rss= -p <pid>`. For Tauri, also add the RSS of every WebKit helper process the app started (`com.apple.WebKit.WebContent`, `com.apple.WebKit.Networking`, `com.apple.WebKit.GPU`). Find them by diffing the process list before and after launch. Report the app alone and the total.
4. The 50 MB case with `array.json`: `viewer-ready` ms, `max-frame-gap` ms, and peak RSS. Get peak RSS from `/usr/bin/time -l`, with the WebKit helpers added for Tauri.
5. The text editor with a 1 MB body pasted in. Does typing stay smooth? Describe it, and measure it if the toolkit lets you.
6. Release build time from clean, and the number of crates in `Cargo.lock`.
7. A screenshot of the window showing a loaded response, via `screencapture -x -l <window id>`, or `screencapture -x` for the full screen. Save it in your spike directory as `screenshot.png`, and look at it with the Read tool to confirm the screen renders. If screen capture is not permitted, say so.

Write the results to `spikes/<name>/RESULTS.md`, with the exact commands you ran, so a reviewer can rerun them. Also write down any problem you hit: an API that fought you, a widget missing, a workaround.
