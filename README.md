# Reqlite

A lean, local-first API client written in Rust. No account, no cloud, no telemetry.

**Status:** early alpha. The CLI works. The desktop app opens a folder of request files, edits, saves and sends them in tabs, and reopens past sends from history.

## Try it

```sh
cargo run -p reqlite -- send examples/hello.toml        # the CLI
cargo run --release -p reqlite-gui -- examples            # the desktop app on a folder
```

### The desktop app

#### Install on macOS

```sh
scripts/macos-app.sh     # builds target/macos/Reqlite.app and target/macos/Reqlite.dmg
open target/macos/Reqlite.dmg
```

Drag Reqlite to Applications. Run the script again to get a newer build. The app is signed ad-hoc, which is enough on the Mac that built it. Sharing it with other Macs needs a Developer ID signature and Apple notarization, which are not set up yet.

#### Use

```sh
reqlite-gui api/                      # a folder of request files: the workspace
reqlite-gui api/users.toml            # one file; its folder is the workspace
reqlite-gui api/ --env api/envs/dev.toml
```

![reqlite-gui showing a 50 MB JSON response, 5,467,582 lines](docs/images/reqlite-gui.png)

`reqlite-gui` opens a folder as a workspace. The sidebar shows its request files (`*.toml`) as a tree. `envs/` and `*.local.toml` secret files are not shown. Each request you open gets a tab. Open folder… (Cmd+O or Ctrl+O) picks a workspace in the system dialog. The app remembers the last workspace in `last-workspace.txt` next to `history.db`, and started with no path, for example from Finder or the Dock, it opens that folder again. On the first start it offers Open folder… and New request.

The sidebar can make a new request or folder, and rename or delete an entry: right-click it for its actions. A new request is written to disk by its first Save, once it has a URL. Delete asks first, and a folder is deleted only when it is empty.

The app watches the workspace. When a file changes on disk, the tree updates, and a tab with no unsaved changes shows the new version. A tab with unsaved changes keeps them. If you then save, the app tells you the file changed and writes only after a second Save.

The status bar picks the environment: "No environment", or any `envs/*.toml` in the workspace. The choice applies to the next send. `--env` sets the first choice.

The History panel lists recent sends, newest first, from every request or only the open one. Choosing an entry opens it in a new tab with what was sent and the stored response. That tab has no file, so a Save never writes the values that were resolved for the send over a request file. Secrets stay as `{{name}}`, as stored. History keeps the first 256 KB of each response.

In each tab, the request is on the left and the response is on the right. When the area right of the sidebar is narrower than 680 px, the response moves under the request. The request has five sections, Query, Headers, Body, Auth and Tests, and each shows what it holds. The Body section picks the body type, and the Auth section the auth type (see [Bodies and auth](#bodies-and-auth)). The Tests section holds the request's checks, one per line (see [Tests and captures](#tests-and-captures)). The response shows the status, the time and the size, with up to three tabs: the body with line numbers, the headers, and, when the request has checks, the result of each one. The number at the right of the body, such as `1/4`, is the top line shown and the line count. JSON, XML and HTML bodies are re-indented and coloured.

| Action | How |
|---|---|
| Send | Cmd+Enter or Ctrl+Enter, the Send button, or Enter in the URL field |
| Cancel a send | Escape, or the Cancel button |
| Save | Cmd+S or Ctrl+S, or the Save button. Enabled only when the form differs from the file. |
| Go to the URL | Cmd+L or Ctrl+L |
| Import a cURL command | Paste it into the URL field (Cmd+V or Ctrl+V). It fills the method, URL, headers, query, body and auth of the shown request, and the status bar lists anything it could not map. A command that cannot be read changes nothing. Other pasted text goes into the field as before. |
| Switch sections | Cmd+1 to 5 or Ctrl+1 to 5 for Query, Headers, Body, Auth, Tests |
| Open a folder | Cmd+O or Ctrl+O, Open… above the sidebar, or Open folder… in an empty window |
| New request | Cmd+N or Ctrl+N, or + Request in the sidebar |
| Next tab | Ctrl+Tab |
| Close a tab | Cmd+W or Ctrl+W, or × on the tab. A tab with unsaved changes asks first. |
| Show or hide the left panel | Cmd+B or Ctrl+B |
| History | Cmd+Y or Ctrl+Y, or History above the sidebar |
| Scroll the response | Mouse wheel, the scrollbar, Page Up, Page Down, Home, End |

Headers and query parameters are written one per line as `name: value`, and a name may repeat. The title shows `*` while there are unsaved changes, and a dot on the tab shows it too. If a file cannot be read, its tab shows the error and never saves over that file. Each send is saved to history, the same way as `reqlite send`.

The window is see-through over a blurred desktop where the OS can blur it: on macOS, and on KDE under Wayland. Elsewhere it is opaque. Two environment variables change the look:

| Variable | Effect |
|---|---|
| `REQLITE_GLASS=0` | An opaque window everywhere |
| `REQLITE_REDUCE_MOTION=1` | No animations: every change shows at once |

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

Secret values never go in that file. Keep them in the OS keychain (macOS Keychain, Windows Credential Manager, or the Secret Service on Linux), or in `envs/dev.local.toml` next to the file. The repo's `.gitignore` already ignores `*.local.toml`.

```sh
reqlite secret set --env envs/dev.toml token      # asks for the value without showing it
printf %s "$TOKEN" | reqlite secret set --env envs/dev.toml token   # or reads it from a pipe
reqlite secret delete --env envs/dev.toml token
```

`secret set` stores only names the environment lists in `secrets`. Each value is stored for that environment file's full path, so `dev.toml` in two projects never share a value. If you move a project folder, set its secrets again.

To find a secret's value, Reqlite looks in this order:

1. the environment's `.local.toml` file;
2. the OS keychain.

Both the CLI and the desktop app use this order. A secret with no value in either place stops the send and names both ways to set it. If the keychain fails to answer, for example because it is locked, the send stops with that reason. That happens only when the request uses the secret.

Or use the local file:

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

### Bodies and auth

A body written as a string is sent as it is, and the file stays at `version = 1`. Other body types and auth need `version = 2`, and checks and captures `version = 3`. Reqlite writes the lowest version a request needs, so files that use nothing new do not change.

```toml
version = 2
name = "Create user"
method = "POST"
url = "{{base}}/users"

[body]
type = "json"                 # adds Content-Type: application/json unless [headers] sets one
text = '{"name": "{{name}}"}'

[auth]
type = "bearer"               # Authorization: Bearer <token>; a JWT goes here
token = "{{token}}"
```

| Body `type` | Fields | Sent as |
|---|---|---|
| `text` | `text` | As it is, with no Content-Type of its own. Same as `body = "..."`. |
| `json` | `text` | As it is, with `Content-Type: application/json` |
| `form` | `[body.fields]`, `name = "value"`, a name may repeat | URL-encoded, with its Content-Type |
| `multipart` | `[[body.parts]]` with `name`, and `text` or `file`; a file part may have `content_type` | `multipart/form-data`, parts in order |
| `file` | `path` | The file's bytes, streamed. Set Content-Type in `[headers]`. |
| `graphql` | `query`, and `variables` as JSON text | `{"query": ..., "variables": ...}`, with `Content-Type: application/json`. See [GraphQL](#graphql). |

File paths are relative to the request file. Files are read when the request is sent, and streamed, never loaded whole. A missing file stops the send and names the file, with exit code 3.

| Auth `type` | Fields | Sends |
|---|---|---|
| `bearer` | `token` | `Authorization: Bearer <token>` |
| `basic` | `username`, `password` | `Authorization: Basic <base64 of username:password>` |
| `api_key` | `name`, `value`, `in = "header"` (the default) or `"query"` | The key as a header, or in the query |
| `oauth2` | See [OAuth 2.0](#oauth-20) | `Authorization: Bearer <token>`, with the token fetched by Reqlite |

Put secrets in placeholders, such as `password = "{{password}}"`. History hides them like any other secret, including a Basic auth value that a server echoes back. If `[auth]` would set a header that `[headers]` also sets, the file is refused, so one never quietly wins over the other.

#### OAuth 2.0

Reqlite gets the access token itself, with one of three grants:

```toml
[auth]
type = "oauth2"
grant = "client_credentials"        # or "authorization_code", or "device_code"
token_url = "https://id.example.com/oauth/token"
client_id = "my-app"
client_secret = "{{client_secret}}" # required for client_credentials; optional for the others
scope = "read write"                # optional
# auth_url = "https://id.example.com/authorize"     # authorization_code only
# device_url = "https://id.example.com/device/code" # device_code only
# client_auth = "body"              # send the client ID and secret as form fields; the default is a Basic header
```

| `grant` | What you do |
|---|---|
| `client_credentials` | Nothing. Reqlite signs in with the client ID and secret. |
| `authorization_code` | Sign in in your browser. Reqlite uses PKCE and waits for the redirect on `http://127.0.0.1:<port>/callback`, so register `http://127.0.0.1` as a redirect URI with your provider. |
| `device_code` | Open the page Reqlite shows and enter the code. |

The CLI prints the sign-in step to stderr, and opens the browser when it runs in a terminal. The app opens the browser and shows the step in the status bar. Reqlite waits up to 5 minutes for a sign-in. Press Esc in the app to stop waiting.

Tokens are kept in the OS keychain, under the token URL, the client ID and the scope, so they last between sends and runs. Reqlite renews a token 30 seconds before it expires, with the refresh token when there is one. If the server answers 401 to a stored token, Reqlite renews it and sends once more. Tokens are never written to the request file, and history shows them as `{{oauth_token}}`, also in response bodies and headers that echo them.

In the app, Form and Multipart are written one per line as `name: value`. A multipart file part is `name: @path`, or `name: @path;type=image/png`.

### GraphQL

```toml
version = 2
name = "User"
method = "POST"
url = "{{base}}/graphql"

[body]
type = "graphql"
query = "query ($id: ID!) { user(id: $id) { name role } }"
variables = '{"id": "{{user_id}}"}'
```

Placeholders work in the query and in the variables. The variables are checked as JSON after the placeholders are filled, so `{"id": {{user_id}}}` sends a number. Variables that are not JSON stop the send with exit code 3.

```sh
reqlite graphql schema user.toml --env envs/dev.toml
```

`reqlite graphql schema` asks the request's server for its schema by introspection, with the request's URL, headers and auth, and prints it as SDL: the root types first, then the others by name. In the app, the GraphQL body has a query editor, a variables editor, and a Schema button that shows the schema in the response pane. A server that refuses introspection gives its message, with exit code 1.

### Tests and captures

A request can check its response and pass values to the requests after it. This needs `version = 3`. There is no script runtime: each check is one line.

```toml
version = 3
name = "Create user"
method = "POST"
url = "{{base}}/users"
assert = [
  "status == 201",
  "header Content-Type contains json",
  "json $.name == \"ada\"",
  "json $.roles contains admin",
  "time < 500",
]

[capture]
user_id = "json $.id"           # later requests use {{user_id}}
token = "header X-Token"
```

A check is a subject, an operator and, for most operators, a value:

| Subject | Operators | Compares |
|---|---|---|
| `status` | `==`, `!=`, `<`, `>` | The status code |
| `header NAME` | `==`, `!=`, `contains`, `exists` | Each value of the header. The name is not case-sensitive. |
| `json PATH` | `==`, `!=`, `contains`, `exists`, `<`, `>` | The value at the path. `contains` looks in a string, an array, or an object's keys. |
| `body` | `contains` | The raw body |
| `time` | `<`, `>` | The response time in milliseconds |

A value is a JSON literal, such as `42`, `true` or `"ada"`, or plain text. `42` and `42.0` are equal. A value may use `{{placeholders}}`. A path starts at `$` and has `.key`, `["key with spaces"]` and `[0]` steps. A capture takes `json PATH` or `header NAME`.

```sh
reqlite run api/ --env api/envs/dev.toml
```

`reqlite run` sends every request file in a folder, in the order the app's sidebar shows them: folders first, then files, each by name. Prefix names with numbers, such as `1 login.toml`, to set the order. Captured values fill placeholders in the requests after the one that captured them. A captured name that the environment declares secret stays secret, so history hides it. Other captured values are stored in history as sent. The run prints one line per check and goes on after a failure. It exits with code 4 when a check fails or a request does not complete, so CI can use it.

`reqlite send` also runs the request's checks, prints them to stderr, and exits with code 4 when one fails. In the app, the Tests tab of the response lists each check with what came back. Values captured in the app fill the sends after them until the app quits.

Postman test scripts are not imported: each one prints a warning. cURL export leaves the checks out.

### cURL import and export

```sh
reqlite import curl "curl 'https://api.example.com/users' -H 'accept: application/json'" -o users.toml
pbpaste | reqlite import curl -o users.toml   # a command copied from the browser
reqlite export curl users.toml
```

`-F` and `--form-string` become a multipart body, `--data-binary @file` a file body, and `--json` a JSON body. `-u user:password` becomes Basic auth with the password as `{{password}}`, so it is not written to a file you might commit; a warning says to put it in your environment's `.local.toml`. Export writes each body type and auth back as curl options. A GraphQL body becomes its JSON, with `--json`. curl cannot sign in, so OAuth 2.0 auth exports as `-H 'Authorization: Bearer {{oauth_token}}'` for you to fill.

Import never drops an option silently. Anything it cannot map prints a warning, for example `-k`, or a `-d @file` body. A header that holds a literal token also gets a warning, so you can move the token to a secret. Import refuses to replace an existing file unless you pass `--force`.

### Postman import

```sh
reqlite import postman Shop.postman_collection.json -o shop/
```

Each folder becomes a directory and each request a file. `{{placeholders}}` carry over unchanged. Auth set on a folder or the collection is copied into each request that inherits it, as an `[auth]` table: bearer, basic and API key. A literal token or password becomes a `{{placeholder}}` with a warning. URL-encoded and form-data bodies, including file fields, file bodies, and GraphQL bodies with their variables carry over. Reqlite runs no scripts, so every pre-request and test script prints a warning that names its request. Disabled headers, parameters and form fields, saved example responses and collection variables also print warnings.

### History

Each send is saved to a local history file, `history.db` in your data directory (for example `~/Library/Application Support/reqlite` on macOS). Set `REQLITE_DATA_DIR` to put it somewhere else. History never leaves your machine and is never committed. Secret values are replaced with `{{name}}` before anything is saved, including in response bodies and headers that echo them. History keeps the first 256 KiB of each response body.

```sh
reqlite history -n 10
```

If the history file is damaged, Reqlite moves it aside as `history.db.corrupt-<time>`, starts a new one, and says so. A history problem never stops a send.

### Timeouts

Every send has limits, so a server that never answers cannot hang Reqlite:

| Limit | Default | What it covers |
|---|---|---|
| Connect | 10 s | Opening the connection, including TLS |
| No data | 30 s | The wait for the next bytes, headers or body. It resets on every read, so a large download that keeps moving never reaches it. |
| Total | none | The whole send. Set it with `--timeout SECS`. |

```sh
reqlite send slow.toml --timeout 5
```

A send that passes a limit exits with code 1, and the message names the limits that applied.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | The request completed, whatever the HTTP status, and every check passed |
| 1 | The request did not complete (connect, timeout, transport, or an OAuth 2.0 sign-in), or the OS keychain failed |
| 2 | Wrong command-line usage |
| 3 | A request or environment file is unreadable or invalid, a placeholder has no value or a secret cannot be read, or a body file is missing |
| 4 | A check failed, or, in `reqlite run`, a request did not complete |

## Layout

| Crate | Job |
|---|---|
| `crates/format` | Request and environment file formats: schema, parser, writer |
| `crates/engine` | Resolves placeholders, sends a request, returns the response. No UI code. |
| `crates/store` | Local request history in SQLite |
| `crates/import` | cURL import and export, Postman import |
| `crates/gui` | The `reqlite-gui` desktop app (iced) |
| `crates/cli` | The `reqlite` command |

Design rules:

1. Request files are the source of truth. History lives in a local SQLite file. It is your data, so no rebuild or reset ever deletes it.
2. The engine is headless. The CLI and the future GUI are thin clients over it.
3. No login and no server in v1.

## Resource budgets

| Metric | Budget | Now (macOS, M4) | Checked in CI |
|---|---|---|---|
| GUI idle footprint, 1000×800 window on a 2× display | under 70 MB | 58 MB | locally (`scripts/gui_idle.py`); CI runners have no 2× display |
| GUI idle footprint minus window frame buffers, any display | under 45 MB | 33 MB | yes, macOS |
| GUI idle CPU, over 5 s | under 0.05 s | 0.00 s | locally; CI prints it, because CI runners have no real GPU |
| Binary | under 25 MB | CLI 3.8 MB, GUI 8.4 MB | yes |
| Cold start | under 300 ms | CLI 3 ms, GUI first frame 97 ms | CLI yes; GUI locally, because CI runners have no real GPU |
| Peak RAM while opening a 50 MB JSON or XML response | under 50 MB | viewer 1.8 to 2.5 MB, CLI send 9.3 MB | yes |

The GUI rows use the physical footprint, which Activity Monitor shows as "Memory". The window's frame buffers grow with the window size and the display scale, so the first GUI row fixes both. The second row counts only the memory Reqlite controls. See [issue #1](https://github.com/officialaritro/reqlite/issues/1) for the measurements behind these numbers. The budgets were 60 MB and 35 MB until Phase A ([#11](https://github.com/officialaritro/reqlite/issues/11)): an empty iced window already uses 55 MB, which left about 5 MB for every v1 feature. `scripts/gui_idle.py target/release/reqlite-gui` checks both, plus idle CPU and the GUI cold start, on macOS. CI runs it on macOS. CI runners are virtual machines with a 1× display and no real GPU, so there it checks only the memory Reqlite controls and prints the other two numbers.

`scripts/budgets.py` runs the other checks on Linux and macOS in CI and fails the build on a miss. It also checks that each crate depends only on the crates below it. To run it yourself:

```sh
python3 scripts/budgets.py
```

The script builds the release binaries and examples first, so it never measures a stale build.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
