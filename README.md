# Reqlite

A lean, local-first API client written in Rust. No account, no cloud, no telemetry.

**Status:** v0.0.1 scaffold. The engine and CLI work. The GUI does not exist yet.

## Try it

```sh
cargo run -p reqlite -- send examples/hello.toml
```

A request is one TOML file:

```toml
version = 1
name = "Hello"
method = "GET"
url = "https://httpbin.org/get"

[query]
from = "reqlite"
tag = ["a", "b"]   # a name can repeat: ?tag=a&tag=b
```

### Environments

`{{name}}` placeholders in the URL, header values, query values and body come from an environment file:

```toml
# envs/dev.toml (committed)
version = 1
secrets = ["token"]

[vars]
base = "http://localhost:3000"
```

Secret values never go in that file. Put them in `envs/dev.local.toml` next to it. The repo's `.gitignore` already ignores `*.local.toml`.

```toml
# envs/dev.local.toml (not committed)
version = 1

[vars]
token = "..."
```

```sh
reqlite send users.toml --env envs/dev.toml
```

An undefined placeholder stops the send and names the variable.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | The request completed, whatever the HTTP status |
| 1 | The request did not complete (connect, timeout, transport) |
| 2 | Wrong command-line usage |
| 3 | A request or environment file is unreadable or invalid, or a placeholder has no value |

## Layout

| Crate | Job |
|---|---|
| `crates/format` | The request file format (schema and parser) |
| `crates/engine` | Sends a request, returns the response. No UI code. |
| `crates/cli` | The `reqlite` command |

Design rules:

1. Request files are the source of truth. A future SQLite database is only a rebuildable cache.
2. The engine is headless. The CLI and the future GUI are thin clients over it.
3. No login and no server in v1.

## Resource budgets

| Metric | Budget |
|---|---|
| Idle RAM (GUI) | under 50 MB |
| Binary | under 25 MB |
| Cold start | under 300 ms |

Current CLI release build: 2.4 MB binary, about 6.2 MB peak RAM for one HTTPS request (macOS, three runs).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
