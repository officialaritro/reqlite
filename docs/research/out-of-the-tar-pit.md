# Out of the Tar Pit: research notes for Reqlite

## 1. Citation and source

Ben Moseley and Peter Marks. "Out of the Tar Pit". Dated February 6, 2006 on its first page.

URL read: https://curtclifton.net/papers/MoseleyMarks06a.pdf

I read all 66 printed pages (title page to references). Page numbers below are the printed page numbers. Section numbers are the paper's own. The `file` tool reported "8 pages" for the download, but the Read tool rendered all 66.

## 2. Core ideas

1. Complexity, meaning "hard to understand", is the root problem. Testing and informal reasoning both fail as complexity grows, so simplicity matters more than either (Sections 2 and 3, pp. 2-5).
2. State is the biggest cause of complexity. A test in one state says nothing about another state. States double with every bit. One stateful callee contaminates every caller (Section 4.1, pp. 6-8).
3. Control (order of execution) and code volume are the next two causes. Forced ordering makes you specify "how" when you only want "what" (Sections 4.2 and 4.3, pp. 8-11).
4. Essential complexity is what the user's problem needs. Accidental complexity is the rest, for example performance work. It is "what the team would not have to deal with in an ideal world" (Section 6, pp. 21-22).
5. State classification (Table 1, p. 26). Only data a user inputs is essential state. Derived data, mutable or immutable, can be recomputed, so storing it is accidental state. Section 9.1.4 (p. 47, footnote 22) counts data from other connected systems as user input.
6. Recommendation: avoid state and control where they are not essential, and separate what remains into three parts: essential state, essential logic, accidental state and control (Section 7.3, pp. 31-34, Table 2 and Figure 1).
7. Static dependency rule (Figure 1, pp. 35-36). Essential state refers to nothing. Logic refers only to state. Accidental parts refer to both, and nothing refers to them. The system must still be correct, only slower, if the accidental parts are removed.
8. Functional Relational Programming (FRP, Section 9, pp. 42-52): state as relations, logic as relational algebra plus pure functions, declarative integrity constraints, accidental state as declared "performance hints" the infrastructure maintains. Input and output go through thin "feeders" and "observers" (Section 9.1.4, pp. 46-47).
9. Performance work (caches, indexes, stored derived data) is allowed but must be declared separately, and managed by infrastructure, not by the logic. The authors warn against "designing for performance" first (Sections 7.3.1 and 7.4, pp. 32 and 36).
10. FRP is "currently purely hypothetical" and unproven (Section 9, p. 42, footnote 17). The conclusion says that if the full separation is not possible, still avoid state, avoid explicit control, and delete code (Section 12, p. 64).

## 3. Requirements for Reqlite

State classification used below (my mapping, from Table 1 and Section 7.1.1):

| Item | Class | Why |
|---|---|---|
| Request TOML files, collection folder layout | Essential state | User typed it. |
| Environment variables (non-secret) | Essential state | User typed it. |
| Secret values (keychain) | Essential state, stored apart | User typed it. Kept out of files by HANDOFF decision 6. |
| Draft text in an editor tab, not yet saved | Essential state (user input) | Not yet on disk, cannot be re-derived. |
| History entries (what was sent, what came back) | Essential state by the paper's rule | The response came from the outside world (p. 47, fn 22). It cannot be re-derived from files. See R2. |
| Resolved request (template plus env applied) | Derived, do not store | Pure function of request and env. |
| Parsed or pretty-printed response, JSON tree, line index | Derived | Function of the response bytes. |
| History search index, request index in SQLite | Accidental state (cache) | Rebuildable from files and history rows. |
| Scroll position, panel size, selected tab | Accidental UI state | Not part of the user's problem. Loss is harmless. |
| Dirty flag, "is saved" | Derived | Compare draft to the file snapshot. |

### MUST

| ID | Decision | Crate or component | Section | Rationale |
|---|---|---|---|---|
| R1 | Add a pure `resolve(request, env) -> Resolved` step. `send` accepts only the `Resolved` type. An undefined `{{var}}` is an error, never sent as literal text. No network, no clock, no filesystem inside `resolve`. | `engine` (the `{{var}}` work) | 5.2.1, 7.3.2, 9.1.2 (pp. 15, 33-35, 44) | Without the type gate, a typo sends `{{tokn}}` to a real server. Pure code is also the cheap, structure-insensitive test target. |
| R2 | Treat history as essential, not as a rebuildable cache. Rebuildable parts (request index, search index) go in tables that may be dropped. History rows go in tables that are never dropped by "rebuild". Any command named rebuild, reset, or clear-cache must not touch history, and a separate command with a warning deletes history. | `store` | 7.1.1, Table 1, 9.1.4 fn 22 (pp. 24-26, 47) | HANDOFF says SQLite is a "rebuildable cache (history, search, UI state)". Past server responses cannot be rebuilt from TOML. Treating them as cache will destroy user data on the first rebuild. |
| R3 | History must never store resolved secret values. Store the response and the request as sent with secret-typed variables redacted, plus the env name and the template. | `store`, `engine` | 7.1.1, 9.2.1 (pp. 24-26, 50) | Storing derived data creates a second copy that must be kept safe. The paper does not name secrets. This rule comes from HANDOFF decision 6. Ignoring it writes API keys into a plain SQLite file. |
| R4 | The GUI holds each response body once. No second full copy, no full parsed JSON tree. Derived views (visible lines, line-offset index, pretty text for the visible window) are computed on demand from that single body and discarded or evicted. | GUI, `engine` | 7.1.1, 7.2.3, 7.3.1 (pp. 24-26, 30-32) | A 50 MB body plus a DOM tree breaks the RAM and "50 MB JSON without freezing" budgets. The paper allows performance state, but only declared and separate. |
| R5 | Dependency direction is one way: `format` depends on nothing in the workspace, `engine` depends on `format`, `store` depends on `format` (and `engine` only for response types, if needed), GUI and CLI depend on all, and no crate depends on GUI or CLI. | whole workspace | 7.3.2, Figure 1 (pp. 33-36) | This is already true. A reverse edge (for example `format` calling `engine`) would let accidental code change essential-state meaning. Cheap to keep, costly to untangle later. |

### SHOULD

| ID | Decision | Crate or component | Section | Rationale |
|---|---|---|---|---|
| R6 | The engine's `Response` hides its body storage behind an accessor (read a range, length), not a public `Vec<u8>` field. Today's memory buffer stays as the first implementation. The `SHORTCUT:` in `engine` then becomes a swap inside `engine` only. | `engine`, clients | 8.4, 9.1.3 (pp. 41, 45) | Data independence. The CLI and three spike GUIs would otherwise all bake in `Vec<u8>`. |
| R7 | Make the GUI state one struct of essential input (open request path, draft text, selected env name) plus a small accidental part (scroll, panel sizes). The view is a function of this struct. Derived values are not fields, or are fields with a named owner and an invalidation key. | GUI (all three toolkits) | 7.1.1, 7.3.2 (pp. 24-26, 33-34) | Stored derived data goes stale. iced and Svelte both reward this shape. |
| R8 | Derive the dirty flag by comparing the draft to the last-saved file text. Do not store a `dirty: bool`. | GUI | 7.1.1, Table 1 (p. 26) | A stored flag is derived state that can disagree with the data. |
| R9 | Put all request-file validity rules in one place in `format` (valid method token, non-empty URL, known version), run at parse. Remove the second method check from `engine` once `format` guarantees it. | `format`, `engine` | 8.3, 9.2.1 (pp. 41, 51) | Constraints in one declarative spot do not interact. Today the method is checked in `engine` and not in `format`. |
| R10 | Give every request a stable identity before the `store` schema exists, and decide whether it is the file path or an `id` field in the TOML. Version bump now is cheap, later it is a migration. | `format`, `store` | 8.1.1, 10.3.3 (pp. 38, 61) | History rows need a key that survives a file rename or a Git move. |
| R11 | Make the request index in SQLite idempotent and fingerprint-based (path plus mtime and size or hash). Rebuild from files gives the same rows as incremental update. | `store` | 7.3.2, 9.1.3, 9.2.1 (pp. 33-35, 45, 50) | Keeps "SQLite is a cache" true for the rebuildable part. |
| R12 | Keep the environment file schema in `format`, versioned like requests, with secret variables declared by name only (value from keychain). | `format` | 7.1.1, 9.1.1 (pp. 24, 44) | One place owns essential state. The engine reads it and never writes it. |
| R13 | The CLI, importers (cURL, Postman) and GUI are "feeders and observers": they translate to and from `format` types and do nothing else. Importers produce `format::Request` values and emit warnings. No logic in them. | `cli`, importers, GUI | 9.1.4 (pp. 46-47) | Keeps one logic implementation. Matches HANDOFF decision 4. |

### SKIP

| ID | Idea | Section | Why skip |
|---|---|---|---|
| S1 | Relational model, relational algebra, relvars, a constraint engine | 8, 9, 10 (pp. 37-62) | Reqlite has about five record kinds. TOML files and `serde` structs cover it. An FRP infrastructure is real work for no user benefit. |
| S2 | Different restricted language per component (state language, logic language, hint language) | 7.3.2, 7.4 (pp. 34, 36) | One language, Rust, and the crate boundaries give most of the benefit. |
| S3 | "No caches, no stored derived data" as a hard rule | 7.1.1 (pp. 24-27) | The budgets force some accidental state (line index, search index). The paper itself allows it (7.2.3). Declare it, do not ban it. |
| S4 | Banning control and concurrency from the design | 7.1.2 (p. 27) | Sending, cancelling and streaming a request are user-visible. See Section 5. |
| S5 | No product types, no data hiding, flat relations only | 9.2.4, 9.3 (pp. 52-53) | Rust structs and private fields are fine at this size. |
| S6 | Observers as live queries and triggers that push every derived relation change | 9.1.4 (p. 47) | The GUI toolkit's own update loop already does this. A separate reactive layer adds code. |
| S7 | Organizing teams around the four components | 9.2.5 (p. 53) | Solo project. |

## 4. Process and testing practices to adopt

1. Test `resolve` (R1) and request validation (R9) as plain table tests. No network, no mocks. The paper says pure code removes testing's second weakness (state) (5.2.1, 9.2.1, pp. 15, 50).
2. Add a "remove the accidental part" test for `store` (R2, R11): delete the cache tables, rebuild from files, and assert the same index rows. This is the paper's own correctness check: the system must still work, only slower, without accidental parts (7.3.2, p. 34).
3. Test that a rebuild or clear-cache command leaves history rows intact (R2).
4. Test that history never contains a known secret value after a send that used it (R3). Use a fake secret string and grep the stored row.
5. Keep CI resource budgets (RAM, binary, cold start, 50 MB JSON) as the guard for accidental state. They check R4 directly. The paper warns that undisciplined performance state is where bugs enter (7.3.1, p. 32).
6. Add a CI step that fails on a forbidden crate edge (R5), for example from `cargo metadata`. This encodes Figure 1 as a check, not as a note.
7. When a bug appears in derived data, fix the logic and re-derive. Do not patch stored values (9.2.1, p. 50).

## 5. Where the paper is wrong or overreaches for this project

1. It is hypothetical. The authors say FRP "has not in any way been proven in practice" (p. 42, fn 17). The prototype they mention is about 1500 lines of Scheme (p. 49, fn 25). It gives no evidence for a desktop GUI.
2. Its target is large enterprise systems (Sections 1, 4.3, 11). Reqlite is about 300 lines in three crates. Most of the separation exists already. The value here is a classification vocabulary and a few rules, not an architecture.
3. It says control and concurrency are accidental because users do not mention them (7.1.2, p. 27). For an API client, in-flight state, cancel, timeouts and streaming are in the user's problem. The ideal-world assumption that computation takes zero time (p. 27, fn 8) also fails: elapsed time is displayed data.
4. It says performance design is dangerous (p. 36). Reqlite has hard performance budgets from day one (RAM, size, start time, 50 MB JSON). Some performance structure must exist early, only behind a clean interface (R4, R6).
5. Its test of "essential" is what the user knows about (p. 22). Under that rule, history of responses is essential, which conflicts with the HANDOFF wording "rebuildable cache". The paper is right here and the HANDOFF text should change (R2).
6. It dismisses mutable derived state as always accidental (p. 25) but allows an exception for "ease of expression" (7.2.2). A GUI draft buffer fits that exception, so draft text is treated as input in the table above.
7. The paper rejects data hiding and compound types (9.2.4). That advice has little value in Rust, where private fields and newtypes (for example `Resolved`) prevent defects at no runtime cost.
