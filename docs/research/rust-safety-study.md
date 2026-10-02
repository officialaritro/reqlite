# Rust safety study (Qin et al., PLDI 2020)

## 1. Citation and source

Boqin Qin, Yilun Chen, Zeming Yu, Linhai Song, Yiying Zhang. "Understanding Memory and Thread Safety Practices and Issues in Real-World Rust Programs". PLDI '20, June 15-20, 2020, London. DOI 10.1145/3385412.3386036.

URL read in full (all 17 pages, including references): https://cseweb.ucsd.edu/~yiying/RustStudy-PLDI20.pdf

Section and page references below use the paper's section numbers and the printed page numbers (763 to 779).

## 2. Core ideas

1. The study covers 850 unsafe usages and 170 bugs (70 memory, 100 thread) in Servo, Tock, Ethereum, TiKV, Redox and five libraries. 145 of 170 bugs were fixed after Rust 1.0 (Sec 1, Sec 3, Fig 2, p764-767).
2. Most unsafe is for good reasons: reuse of existing code (42%), performance (22%), sharing data across threads (14%). Raw pointer work is 66% of usages (Sec 4.1, p768).
3. Good practice is to wrap a small unsafe core in a safe interface. Mark only the real source of unsafety (Insight 1 and 2, Suggestion 1 and 2, Sec 4.1 and 4.2, p768).
4. If the safety of a function depends on how callers use it, mark it `unsafe`, not interior-unsafe. Interior mutability that returns references is risky (Suggestion 3 and 4, Sec 4.3, p769).
5. All memory-safety bugs involve unsafe code. Most also involve safe code. The cause is often in safe code and the effect in unsafe code, for example a wrong buffer size or an object dropped too early (Insight 4, Sec 5.1, p769-771).
6. Lifetime misunderstanding is the main cause of use-after-free. A raw pointer outlives the object it came from (Fig 7, Sec 5.1, p770).
7. All 59 blocking bugs are in safe code. 38 are Mutex or RwLock bugs and 30 of those are double locks. The guard's implicit unlock at end of lifetime hides the critical section (Table 3, Sec 6.1, p771-772).
8. Typical double lock: a lock taken in a `match` condition is still held in the match body, because the temporary lives for the whole `match` (Fig 8, p772). The fix is to save the result in a local first. Other blocking causes are Condvar with no notifier (8 of 10 bugs), channel receive with no sender (5 bugs), and Once reentry (Sec 6.1, p772).
9. Of 38 non-blocking bugs, 23 share data with unsafe code and 15 with safe code (Mutex or atomics). 25 of 41 happen in safe code. Message passing caused only 3 (Table 4, Sec 6.2, p773).
10. Several non-blocking bugs come from `&self` methods that mutate state (atomic check-then-store, interior mutability). Taking `&mut self` would make the compiler reject them (Fig 9, Insight 10, p774). Existing tools missed these bugs: Rust-clippy found none, and Miri needs a triggering test and gives false positives (Sec 7, p774).

## 3. Requirements for Reqlite

Current state checked: 261 lines across 3 crates, no `unsafe`, no locks, no channels yet. The engine is `async` (tokio, reqwest). Workspace CI runs `cargo clippy --workspace --all-targets -- -D warnings`.

Lints below were run with `cargo clippy --workspace -- -W clippy::<name>` on clippy in this repo. None reported "unknown lint". `rustc` lint `unsafe_code` was also accepted. `await_holding_lock` and `await_holding_refcell_ref` are warn-by-default in the `suspicious` group, so `-D warnings` in CI already makes them hard errors.

| # | Level | Decision | Crate or component | Section | Rationale |
|---|---|---|---|---|---|
| 1 | MUST | Add `[workspace.lints.rust] unsafe_code = "forbid"` and make each crate use `[lints] workspace = true`. No crate may opt out without a written reason in the PR. | all crates | 4.1, 5.1, Insight 4 | Every memory bug in the study needs unsafe. No Reqlite code needs unsafe: reqwest, rusqlite and the GUI toolkits own their unsafe. Cost is zero today. |
| 2 | MUST | Never hold a `std::sync::Mutex` or `RwLock` guard across `.await`. Keep `clippy::await_holding_lock` and `clippy::await_holding_refcell_ref` on. Write them explicitly in `[workspace.lints.clippy]` as `deny` so a future `allow` is visible. | engine, store, GUI glue | 6.1 (double lock, 30 of 38 lock bugs), Insight 6 | A guard held over `.await` blocks the tokio worker and can deadlock when another task wants the same lock. It would freeze the GUI request path. |
| 3 | MUST | Never put a lock acquisition (`.lock()`, `.read()`, `.write()`) in a `match`, `if let`, `while let` or `for` scrutinee, when the arm body can lock again or `.await`. Bind the result to a local first, as in the TiKV fix. Enable `clippy::significant_drop_in_scrutinee` (nursery group, lint exists) at `warn`. | engine, store | 6.1, Fig 8, p772 | This is the single most repeated Rust blocking bug pattern in the paper (6 of the double locks). It is cheap to prevent. Because the lint is nursery, review must also check for it. Edition 2024 shortened some temporaries (`if let` and tail expression scope), but `match` scrutinee temporaries still live to the end of the match. Verify per case. |
| 4 | MUST | The store owns its rusqlite `Connection` on one dedicated thread (or one `spawn_blocking` task that loops on a `std::sync::mpsc` or tokio `mpsc` receiver). Other code sends commands and gets results by a `oneshot`. Do not wrap a `Connection` in `Arc<Mutex<_>>` and lock it from async tasks. | planned `store` crate | 6.1 (Mutex, Condvar, Channel blocking), 6.2 | `Connection` is `Send` but not `Sync`. One owner thread removes shared state, so it removes lock bugs. It also keeps writes serialised and keeps SQLite I/O off the UI thread and the tokio workers. |
| 5 | MUST | Shared mutable state between GUI and engine is limited to what the toolkit forces. The default is: the GUI sends a request value to the engine by channel and receives an event or `Response` back by channel. The engine holds no reference into GUI state. | GUI spike, engine | 6.2 (only 3 of 41 non-blocking bugs came from message passing; 38 came from shared memory), Insight 7 and 8 | The data-sharing choice decides most of the bug surface. A channel moves ownership, so the compiler enforces one writer. |
| 6 | MUST | For channels, every receive loop has a defined exit: sender dropped, a cancel signal, or a timeout. Prefer bounded channels for event streams, and do not `.await` a `send` while holding a lock. Cancelling a request must drop its future, not wait on a channel that will never get data. | engine, GUI glue, store | 6.1 Channel (5 bugs wait on a receiver; 1 bug blocks on a full bounded channel; 1 bug holds a lock while waiting on a channel) | A request that never completes, or a stop button that waits forever, is a user-visible hang. |
| 7 | SHOULD | Prefer `&mut self` on engine and store types over `&self` with interior mutability. If a type must be shared across tasks (`Arc<T>`), keep its state behind one lock or one owner task, not a mix of atomics and flags. Use one lock per invariant, never a check-then-act on an atomic. | engine, store | 6.2 Interior Mutability, Fig 9, Insight 10, Suggestion 8 | Check-then-store on an atomic (Fig 9) is a race. Today `send(&Client, &Request)` is stateless, which is the right shape. Environments and cookie jar (v0.3) are where this will matter. |
| 8 | SHOULD | Do not use a `Mutex` around a plain counter or flag in new code. Use an atomic for a single independent value, a channel or owner task for anything with an invariant. Enable `clippy::mutex_atomic` (lint exists) at `warn` only if it does not fire on legitimate use. | engine, GUI glue | 6.2 | Mixed styles are where the study's atomicity bugs sit. Low value on its own, so SHOULD. |
| 9 | SHOULD | Add `clippy::let_underscore_lock` at `deny` (lint exists, default `correctness`, so already deny by default; list it to make intent visible). | all | 6.1 (forgotten unlock, `let _ = m.lock()` drops the guard at once) | `let _ = mutex.lock()` releases at once and the section is unprotected. The lint already catches it. |
| 10 | SHOULD | If unsafe is ever needed (for example a GUI FFI shim), isolate it in one small crate, mark only the unsafe core, document the safety condition at each site, and enable `clippy::undocumented_unsafe_blocks` and `clippy::missing_safety_doc` (both exist). Keep the rest of the workspace on `forbid`. | any future crate | 4.1 and 4.3, Suggestions 1 to 3 | Gives a fixed rule for the one case where #1 must be relaxed, so the exception is small and reviewable. |
| 11 | SHOULD | Handle a poisoned lock on purpose: either avoid `std::Mutex` poisoning by using `parking_lot` or an owner task, or decide per lock whether to recover. Do not `.unwrap()` on `lock()` in production paths. Note `panic = "abort"` in the release profile already makes poisoning impossible in release builds, but tests and debug builds still poison. | store, engine | 6.2 (poisoning ignored in 1 bug, mishandled in 2) | Debug and test behaviour should match release behaviour, or tests prove nothing. |
| 12 | SHOULD | Make the engine's lifetime boundaries explicit: `Response` owns its bytes (it does). When the 50 MB body streams to a temp file (see the `SHORTCUT:` in `engine`), hand the GUI an owned path or an owned chunk, never a borrowed slice that outlives a task. | engine, GUI | 5.1 (lifetime misuse is the main cause of use-after-free) | In safe Rust this fails to compile instead of corrupting memory, so this is a design nudge, not a bug risk. SHOULD, not MUST. |
| 13 | SKIP | Miri in CI. | CI | 2.4, 7 (Miri only finds bugs a test triggers, gives false positives) | With `unsafe_code = "forbid"` there is no unsafe in our code for Miri to check. Miri also cannot run reqwest, sockets or SQLite FFI. Revisit only if rule #10 ever applies. |
| 14 | SKIP | `loom` model checking. | engine, store | 7.2 | `loom` needs code written against its own sync types and is for lock-free or custom-primitive code. Reqlite has none. Owner-thread design (#4, #5) removes the need. |
| 15 | SKIP | A custom double-lock static detector, or the paper's MIR use-after-free checker. | tooling | 7.1, 7.2 | The paper's own tools are research prototypes (3 false positives on UAF). The clippy lints in #2 and #3 cover the same patterns for our use. |
| 16 | SKIP | Replace `std::sync::Mutex` with an explicit `unlock()` API (Suggestion 7). | - | 6.1 | This is a language change request, not something we can adopt. Use a short block scope or `drop(guard)` instead (the paper found 11 uses of explicit drop, 9 to avoid double lock). |
| 17 | SKIP | Auditing `unsafe impl Send` or `Sync` in our code. | - | 6.2, Suggestion 8 | Forbidden by #1. Review third-party crates through `cargo-geiger` style tools only if a supply-chain audit is added later. |
| 18 | SKIP | IDE plug-ins that highlight implicit unlock scope (Suggestion 6). | - | 6.1, 7.2 | Tooling for IDE vendors. The rust-analyzer inlay hints for drop order already exist on the user's side. |

Suggested `Cargo.toml` shape (not applied, this task edits no code):

```toml
[workspace.lints.rust]
unsafe_code = "forbid"

[workspace.lints.clippy]
await_holding_lock = "deny"
await_holding_refcell_ref = "deny"
significant_drop_in_scrutinee = "warn"
let_underscore_lock = "deny"
```

Each crate adds `[lints] workspace = true`. Caveat: `forbid` cannot be overridden by `allow` in a crate, which is the point. If a macro from a dependency expands to unsafe, `forbid` still accepts it, because the lint checks code written in the crate and the macro-generated code usually carries an allow. Check this once when adding the first derive-heavy crate.

## 4. Process and testing practices to adopt

1. Add the lints in section 3 to CI via the existing `cargo clippy ... -D warnings` step. No new job is needed.
2. Add one regression test for each concurrency rule once the code exists. Example: a store test that sends 100 writes from many tasks to the owner thread and checks that all rows land and the call returns (no hang). Another: a test that cancels an in-flight request and asserts the future completes within a timeout.
3. Use timeouts in every concurrency test (`tokio::time::timeout`), so a deadlock fails the test instead of hanging CI. The paper's blocking bugs are hangs.
4. In code review, check three things on every new lock: where does the guard drop, is there an `.await` or a second lock before that point, is the guard in a `match` or `if let` scrutinee.
5. Run the GUI spike with the owner-thread and channel design (#4, #5) for all three toolkits, so the toolkit comparison does not also compare threading models.
6. Keep `SHORTCUT:` comments for any deliberate lock or shared state, naming the upgrade trigger (already a repo rule).

## 5. Where the paper is wrong for, or overreaches for, this project

1. Sample bias. The subjects are an OS, a browser engine, a database, a blockchain node and low-level libraries. Reqlite is an application over safe crates. The 66% raw-pointer and 22% performance-unsafe figures do not apply to us. Alastair Reid's note on the paper (found in search results, not in the PDF) says the same about bias toward system code; treat that as a search-result summary, not a claim I verified.
2. Age. Bugs were collected up to 2019 on Rust 1.39. Async and `.await` did not exist then. The paper has no section on `async`, tokio, or holding a guard across `.await`. Rule #2 comes from the paper's lock-guard lifetime findings plus current clippy, not from the paper's data.
3. The paper's "Rust should add explicit unlock" and IDE suggestions (Suggestion 6 and 7) are aimed at language and tool designers, not at us.
4. The paper treats `Mutex` as the main primitive. Our design avoids locks with an owner thread. That makes most of the 38 lock bugs not apply, which is a design choice the paper supports only indirectly (Table 4: message passing caused 3 of 41 bugs, but the sample of message-passing programs is small, so this is weak evidence).
5. Counts are small (170 bugs, 5 apps). Percentages like "30 of 38 are double locks" mostly come from Ethereum (27 of 38, Table 3). One project dominates that row.
6. The two bug detectors are prototypes, evaluated only on the paper's own subjects. Do not expect them or Miri to fit our CI (see SKIP 13 and 15).
7. Edition 2024 changed temporary scope rules for `if let` and tail expressions, after the paper. Fig 8 shows `match` scrutinee behaviour, which is unchanged. Do not assume that every example in Sec 6.1 reproduces on edition 2024.
