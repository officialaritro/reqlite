# Simple Testing Can Prevent Most Critical Failures

## 1. Citation and source

Ding Yuan, Yu Luo, Xin Zhuang, Guilherme Renna Rodrigues, Xu Zhao, Yongle Zhang, Pranay U. Jain, Michael Stumm. "Simple Testing Can Prevent Most Critical Failures: An Analysis of Production Failures in Distributed Data-Intensive Systems." OSDI 2014 (11th USENIX Symposium on Operating Systems Design and Implementation), pages 249-265.

URL read: https://www.usenix.org/system/files/conference/osdi14/osdi14-paper-yuan.pdf

I read the body from the cover through the Conclusions (printed pages 249-262). The remaining pages are references. Page numbers below are the printed USENIX page numbers.

## 2. Core ideas

1. The study covers 198 sampled user-reported failures in Cassandra, HBase, HDFS, MapReduce, and Redis. 48 were catastrophic (all or most users affected). (Sec 1, Sec 2, Table 1, p. 249-250)
2. 92% of catastrophic failures came from incorrect handling of non-fatal errors that the software had explicitly signalled. (Finding 10, p. 256)
3. 35% of catastrophic failures came from three trivial handler mistakes. The handler ignores the error or only logs it (25%). The handler aborts on an over-broad catch (8%). The handler holds a TODO or FIXME comment (2%). (Finding 11, Sec 4.1, Fig 5, p. 256-257)
4. Another 23% were system-specific but easy to detect. Statement coverage of the handler would have exposed them. (Finding 12, Sec 4.2, p. 257)
5. Developers did anticipate the errors. Only one case in the data was an unchecked error. The handlers were sloppy, not missing. (Sec 4, p. 256)
6. Handlers get little testing because the error path is rare. Common reasons for ignoring errors are "will never happen", and a belief that the error is not critical. Developers also swallowed library exceptions only to make the code compile. (Sec 5.3, p. 260-261)
7. 77% of failures needed more than one input event, and the order of events mattered in 88% of those. 90% needed three events or fewer. (Findings 1, 2, Table 3, p. 252)
8. 77% of failures can be reproduced by a unit test. 98% need no more than 3 nodes. (Findings 3, 9, Table 5, Table 8, p. 253, 255)
9. Logs held the failure data. 76% printed explicit error messages. In 84% all triggering events were logged. (Findings 6, 7, p. 254)
10. Aspirator is a static checker. It flags empty or log-only catch blocks, over-broad catches that call abort or exit, and TODO or FIXME in handlers. It found 121 bugs and 379 bad practices in 9 systems, with a 19% false positive rate. It was tuned by ignoring some exceptions and skipping shutdown, close, and cleanup methods. (Sec 5.1-5.3, Table 9, p. 258-260)

The paper's three-part advice: use an Aspirator-like checker, review error-handling code, and build tests bottom-up from the handler. (Sec 7, p. 262)

## 3. Requirements for Reqlite

Verified in this repo with `cargo clippy` 0.1.98 (`rtk proxy cargo clippy -p reqlite-format -- -W clippy::<name>`): every lint named below exists. A fake lint name printed "unknown lint", the real ones did not. The workspace run found one hit: `crates/engine/src/lib.rs:29` uses `map_err(|_| ...)` and drops the source error (`map_err_ignore`). The `disallowed_methods` config in `clippy.toml` parsed without error and found no uses today. I did not prove that it fires on a violation. I removed the temporary `clippy.toml` afterwards.

| # | Level | Component | Requirement | Section | Rationale |
|---|---|---|---|---|---|
| 1 | MUST | format, cli, GUI | A request file that fails to parse is never rewritten, truncated, or replaced by a default. Show the full `toml` error with file path and line. Writes to request files happen only through an explicit user save of a successfully parsed model, and use write-to-temp then rename. | Finding 10, 11 (ignored error); Fig 11 (trusting bad state); p. 256-258 | The file is the source of truth. A swallowed parse error plus an autosave would destroy user data. |
| 2 | MUST | store (planned) | The SQLite cache is disposable. On open failure, schema mismatch, `SQLITE_CORRUPT`, or `PRAGMA integrity_check` failure: rename the bad file to `*.corrupt-<time>`, create a new one, and rebuild from request files. Show one visible notice. Never abort the app. Never silently continue with an empty history. | Fig 8 (abort on over-catch), Fig 12b (ignore a recovery error); p. 257, 259 | Aborting on a recoverable cache error is the paper's abort pattern. Silently emptying history is its ignore pattern. Only history that exists nowhere else is lost, so say so. |
| 3 | MUST | all crates | No `let _ = <Result>`, no `.ok()` that drops an error, and no `unwrap_or_default()` or `unwrap_or(..)` on data the user owns (request fields, env values, file reads). If a default is right, the code states why in a comment and returns it as a typed warning. | Finding 11 (25% ignored errors), Sec 5.3 "will never happen"; p. 256, 261 | A fallback on user data hides parse and IO failures and may later be saved over the original. |
| 4 | MUST | CI, workspace `Cargo.toml` | Add a `[workspace.lints.clippy]` block set to `deny` for `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `let_underscore_must_use`, and `map_err_ignore`. Allow `unwrap_used` and `expect_used` in tests with `#[cfg(test)]` or `clippy.toml` `allow-unwrap-in-tests = true`. Fix the existing `map_err_ignore` hit at engine `lib.rs:29`. | Sec 5, Aspirator rules; p. 258 | This is the enforceable Aspirator for Rust. The current CI uses only `-D warnings` on default lints, which does not catch these. All names verified to exist. |
| 5 | MUST | cURL and Postman importers | Every construct the importer cannot map becomes a typed `Warning` in the result. A Postman script yields a warning that names the item and the script kind. The importer never drops it silently. Import returns `(Request, Vec<Warning>)`, and the CLI and GUI must show the warnings. | Finding 11, "errors ignored", Fig 7; p. 256-257 | Dropping a script quietly is an ignored error. It is already a project decision. This makes it testable. |
| 6 | MUST | engine | Do not use `String::from_utf8_lossy` on bytes that the app saves or re-sends. Keep `Vec<u8>` and decode only for display. Header values: keep bytes or return a typed error for non-UTF-8, and do not repair them silently. | Finding 11 "errors ignored"; p. 256 | Lossy decoding silently changes data. Save-response and re-send features would corrupt it. Display-only use in the CLI is acceptable. |
| 7 | MUST | env interpolation (planned) | An undefined `{{var}}` is a hard error that names the variable and the request. It is never replaced by an empty string and never sent as-is. | Finding 11, Sec 5.3; p. 256, 260 | An empty replacement sends a wrong request to a real server, possibly with a missing auth value. |
| 8 | MUST | keychain secrets (planned) | A `keyring` failure (locked, denied, absent backend) is a visible error that blocks the send. It does not fall back to empty or to plaintext in a file. Distinguish "no entry" from "backend failure" in the error type. | Finding 10, 11; p. 256 | Falling back to nothing sends an unauthenticated request. Falling back to a file breaks the secrets decision. |
| 9 | MUST | tests | Each error variant that the product can show gets one test that triggers it through the public API and asserts the variant and message content. See Section 4 for the list. | Finding 12, bottom-up tests; p. 257 | The paper's strongest result: untested handlers are where catastrophic failures live. |
| 10 | SHOULD | error types | Library crates (`format`, `engine`, `store`, importers) keep `thiserror` enums with `#[source]` or `#[from]`, one enum per crate, no `Box<dyn Error>` in public signatures. The CLI keeps `main` as the only place that prints. Add `anyhow` only in `cli` (or the GUI's top layer) if context chains are wanted. Use `.context()` there so the path and operation remain visible. | Sec 4.1 (catch the precise exception, not Throwable); Fig 8; p. 257 | Typed enums let callers match precisely. Matching a specific variant is the Rust form of "catch the precise exception". Today `cli/src/main.rs` flattens `read_to_string` with `format!` and loses the `io::ErrorKind`. |
| 11 | SHOULD | engine | Split `SendError::Http(reqwest::Error)` into variants the UI can act on: connect, timeout, TLS, body decode, and redirect. Keep the source. Do not use a catch-all `Other`. | Fig 8, Fig 10; p. 257 | An over-broad variant invites an over-broad handler later in the GUI. |
| 12 | SHOULD | GUI | One error-to-UI mapping function, exhaustive `match` with no `_ =>` arm on error enums, so a new variant fails the build. Enable `clippy::wildcard_enum_match_arm` for those modules only. | Sec 4.1 "later... other exceptions may be over-caught"; p. 257 | Turns the paper's "code evolves" risk into a compile error. I did not run this lint name. Verify before adding. |
| 13 | SHOULD | clippy.toml | Add `disallowed-methods` for `Option::unwrap_or_default` and `Result::unwrap_or_default` with a reason, then allow per call site with `#[allow(clippy::disallowed_methods)]` plus a comment. Consider `Result::ok` the same way. | Sec 5.1 (configurable ignore list); p. 259 | Plain `manual_unwrap_or_default` is a style lint and does not stop data-loss fallbacks. The config parsed in this repo but I did not test it against a violation. |
| 14 | SHOULD | cli, GUI | Distinct exit codes in the CLI: 2 usage, 1 request failed or transport error, and a separate non-zero for file or parse errors. Print the underlying cause chain. | Finding 6, Finding 7; p. 254 | A clear message with the cause is how the study's failures were diagnosed. |
| 15 | SHOULD | engine | Log (to stderr in CLI, to a log in GUI) the inputs of each send: method, URL with secrets masked, and file path. | Finding 7, Fig 4; p. 254 | If a user reports a bug, the log alone should reproduce it. Mask secrets first. |
| 16 | SHOULD | store | Treat each cache write failure (disk full, locked) as non-fatal to sending a request. Surface a one-line warning. Do not abort the send and do not ignore the error. | Fig 8, Fig 7; p. 257 | History failure must not block the main job, and must not be silent either. |
| 17 | SKIP | Aspirator clone | Do not write a custom static analysis tool. | Sec 5.1; p. 258 | Rust has no checked exceptions and no empty `catch`. Clippy restriction lints plus rule 4 cover the same patterns. |
| 18 | SKIP | Systematic fault injection, symbolic execution of handlers | No fault-injection framework or symbolic execution. | Sec 4.2, Sec 7; p. 257, 262 | The product is a local single-process client with about 300 lines. Hand-written tests for each error variant cost less. |
| 19 | SKIP | Multi-node, ordering and concurrency findings | Ignore Findings 1-5 about node counts and event order, except for the GUI. | Sec 3.1-3.3; p. 252-254 | Reqlite is not distributed. A test of two events (for example send, then cancel) is enough. |
| 20 | SKIP | TODO and FIXME scanning in handlers | Do not add a separate TODO scanner. | Sec 5.1; p. 258-259 | `clippy::todo` and `clippy::unimplemented` cover macros. For comments, a one-line `grep` in CI against `TODO\|FIXME` inside error arms is enough, and optional. |

## 4. Process and testing practices to adopt

1. Test every handler bottom-up. For each error variant, write one test that reaches it through the public function and asserts the variant and message. This follows Finding 12. The current tests cover `UnsupportedVersion`, a misspelled field, and `Method`. The ones missing are listed below.
2. Error paths that deserve tests, in priority order.
   1. Parse failure on a request file leaves the file bytes unchanged on disk (requirement 1). Test with a temp dir and compare bytes before and after.
   2. Corrupt SQLite file: open, expect rename plus rebuild, and expect a warning. Also a schema version newer than the build.
   3. Undefined `{{var}}`, a recursive `{{var}}`, and a value that contains `{{`.
   4. Importers: unknown Postman field, Postman script present gives a warning, malformed cURL quoting gives an error and not a partial request.
   5. Keychain backend unavailable gives a blocking error, and "no entry" gives a different error.
   6. Engine: connection refused, timeout, TLS failure, invalid header name or value, non-UTF-8 header, truncated body. The current engine test uses a fake TCP server, which makes these cheap. A closed listener gives connection refused. A server that sleeps gives timeout.
   7. Write failure to a request file (read-only dir) returns an error and keeps the old file.
3. Mutation check on each of these tests. Ask what one-character change would make it pass wrongly. Example: if the parse-failure code deletes the file, does the byte comparison fail?
4. Run clippy with the requirement 4 lint set in CI, on every OS. Do not add `-A` for the same lints in a crate without a comment that names the reason.
5. Code review rule for the three Aspirator patterns. A reviewer rejects any `Err(_) => {}`, `Err(_) => default`, `if let Ok(..)` that has no `else`, `.ok();` as a statement, and `process::exit` or `panic!` in a recoverable path. Use a short checklist in the PR template.
6. Keep logs useful. Log the triggering input at the point of failure (Finding 7), but never log secret values.

## 5. Where the paper is wrong for, or overreaches for, this project

1. Wrong language model. The core patterns assume Java checked exceptions, `catch (Throwable)`, and `System.exit`. In Rust, errors are values. The nearest equivalents are `Err(_) =>` arms, `let _ =`, `.ok()`, `unwrap_or_default`, and `?` into `Box<dyn Error>`. The paper's numbers do not transfer. They tell us where to look, not how often to expect trouble.
2. Wrong scale. The failures are cluster-wide outages in distributed systems. Reqlite has one user and one process. The cost of a swallowed error is a wrong request or a lost file, not an outage. Findings 1-5 and the node counts do not apply.
3. Selection bias. The sample is only high-priority, developer-confirmed tickets, 2010 or later, from five mature Java and C projects (Sec 2, p. 250-251). The paper itself says it excluded misconfigurations and cannot rank fault causes. The 92% figure is a share of catastrophic failures that were reported, not of all failures.
4. The 58% "detectable by simple testing" claim is partly judgment (Fig 5, Finding 12). It counts the handler as detectable once the block is exercised. It does not prove that a test author would have found the trigger. The authors also say the "complex bugs" (34%) need real system knowledge (p. 258).
5. Aspirator is a Java bytecode tool with a 19% false-positive rate after tuning (Sec 5.2, p. 259-260). I did not find a Rust port in the paper. Do not copy its numbers into Reqlite CI targets.
6. "Abort on error is bad" is not always true here. The paper says abort is wrong when the error is recoverable. For Reqlite, failing loudly on a bad request file or a missing variable is the right call. The rule is: abort the operation, not the process, and never destroy data.
7. Statement coverage as a goal (Finding 12) conflicts with the project's testing rule that coverage is a diagnostic. Use it only for the error arms listed in Section 4.
8. Written in 2014. It says nothing about `Result`-based design, `#[must_use]`, or lints like the ones above. Rust's compiler already prevents the unchecked-error case that the paper found only once (Redis).
