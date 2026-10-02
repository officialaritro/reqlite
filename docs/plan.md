# Reqlite v0.1 foundation plan

This plan turns the six research notes in `docs/research/` into build steps. Each rule names its source note: TP (Out of the Tar Pit), AD (Adapton), RS (Rust safety study), LF (Local-First Software), RO (Ropes), ST (Simple Testing).

## Decisions from the research

1. **History is essential data, not cache (TP R2).** A past response cannot be rebuilt from request files. History lives in its own SQLite file. No rebuild or clear-cache path touches it. A corrupt history file is moved aside, never deleted, and the user sees a notice (ST 2). A future search index goes in a separate, freely deletable file.
2. **Repeated query and header keys.** `BTreeMap<String, String>` cannot hold `?tag=a&tag=b` (LF note 10). The format changes to one name mapping to one or more values. A single value is still written as a plain string. The format is unreleased, so `version` stays 1.
3. **Validation lives in `format` (TP R9, ST 1).** Method token, header name and value, and a non-empty URL are checked at parse time. A file that fails to parse is never rewritten.
4. **Deterministic writer (LF 2).** `format::to_string` gives byte-identical output for an unchanged request. A save happens only when the model differs from the file (TP R8). Saves go through temp file, fsync, rename (LF 3).
5. **Resolve is pure and typed (TP R1, AD R1, R2, ST 7).** `engine::resolve(&Request, &Vars) -> Result<Resolved, ResolveError>`. `send` accepts only `Resolved`. An undefined `{{var}}` is an error that names it. Values are not re-scanned, so no recursion exists. Resolution runs on demand at send time, never on an environment switch.
6. **Secrets are declared by name (TP R12, R3).** An environment file lists secret names. Their values come from a gitignored `*.local.toml` file next to it. The OS keychain is a later phase. History stores the request with secret values replaced by their `{{name}}`.
7. **Large bodies spill to a temp file (RO A2, TP R4).** Above 1 MiB the engine streams the body to a temp file. The response holds the body once. Header values stay bytes, so no lossy decode touches saved data (ST 6).
8. **Errors are typed per crate (ST 10, 11).** Each library crate has one `thiserror` enum with sources kept. `SendError` splits into connect, timeout, redirect, body and other transport failures. Only the CLI prints.
9. **Concurrency by ownership (RS 4, 5, 6).** The store owns its `rusqlite::Connection` on one thread and takes commands over a channel. Every receive loop ends when its senders drop. No lock is held across `.await`.
10. **Lints as the checker (RS 1, 2, 3; ST 4).** The workspace forbids `unsafe` and denies `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `let_underscore_must_use`, `map_err_ignore`, `await_holding_lock` and `await_holding_refcell_ref`. Tests may unwrap.
11. **No incremental framework (AD R9 to R11).** No salsa, no dependency graph. A content-hash memo is added only if a measured budget fails.
12. **No rope for responses (RO A8).** The viewer reads the temp file through a line-offset index and draws only visible lines. The request editor uses the toolkit's own text widget first.

## Dependency direction (TP R5)

`format` depends on nothing in the workspace. `engine` and `import` depend on `format`. `store` depends on `format` and `engine`. `cli` and the GUI depend on all of them. CI checks this.

## Phases

Each phase ends green (`cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`) and a run on the real surface. Each feature is one commit with a one-line message.

| # | Phase | Crates | Proof |
|---|---|---|---|
| 1 | Workspace lints, fix the `map_err_ignore` hit | all | clippy clean with the new lint set |
| 2 | Multi-value params, validation in `format` | format, engine | tests for repeated keys and each validation error |
| 3 | Deterministic writer and atomic save | format | round-trip test on `examples/`, failed-save test keeps the old bytes |
| 4 | Environments: env file, local secrets file, pure `resolve` | format, engine | tests for undefined var, unterminated `{{`, secret redaction |
| 5 | Body spill to temp file, typed `SendError` | engine | 5 MB body spills and reads back, refused connection maps to `Connect` |
| 6 | CLI: `--env`, exit codes, cause chain | cli | run against a local server |
| 7 | `store`: history on an owner thread, corrupt-file recovery | store, cli | corrupt file is moved aside and a fresh one works |
| 8 | cURL import and export with warnings | import, cli | round trip cURL to file to cURL |
| 9 | Postman v2.1 import with script warnings | import, cli | fixture collection imports to a folder tree |
| 10 | GUI spike: iced, Slint, Tauri + Svelte | spikes/ | measured RAM, cold start, binary size, 50 MB JSON |
| 11 | CI budgets: binary size, cold start, peak RAM on 50 MB JSON, crate direction | CI | CI fails on a deliberate miss |

The GUI spike moves from first to tenth. It measures the 50 MB response case, and that needs phase 5. All three prototypes use the same channel design (RS process 5), so the comparison measures the toolkit, not the threading model.

## Not in this plan

The OS keychain, the collection tree and file watching (LF 7, 8), the search index, and the production GUI. These follow once the spike picks a toolkit.
