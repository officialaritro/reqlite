# Local-First Software: notes for Reqlite

## 1. Citation and source

Martin Kleppmann, Adam Wiggins, Peter van Hardenberg, Mark McGranaghan. "Local-First Software: You Own Your Data, in spite of the Cloud." Proceedings of Onward! '19, October 23-24, 2019, Athens, Greece. ACM. DOI https://doi.org/10.1145/3359591.3359737

URL read: https://martin.kleppmann.com/papers/local-first.pdf (downloaded with curl; all 25 PDF pages read, printed pages 154 to 178, including the reference list).

Note: https://www.inkandswitch.com/media/local-first/local-first.pdf returned a 404 HTML page. Do not use it.

## 2. Core ideas

1. The local copy is the primary copy. Servers are secondary copies that help with multi-device access. In cloud apps the roles are the reverse (Sec. 2, p.155).
2. Seven ideals: no spinners, work not trapped on one device, network optional, seamless collaboration, the Long Now, security and privacy by default, user retains ultimate control (Sec. 2.1 to 2.7, pp.155-161).
3. Local files give speed, offline use, backup, and full user control. Their weak point is multi-device access and collaboration (Sec. 3.1.1, p.162).
4. Folder-sync tools (Dropbox and similar) work on any file format. That is a strength (compatibility) and a weakness: they cannot do format-aware merges, so concurrent edits make "conflicted copy" files (Sec. 3.1.3, p.163, Fig. 2).
5. Git is "the closest thing to a true local-first software package". It has two weaknesses: no real-time fine-grained merge, and non-text formats are second-class (Sec. 3.1.4, p.163).
6. Table 1 scores Git + GitHub as: fast yes, multi-device partial, offline yes, collaboration partial, longevity yes, privacy partial, user control yes. No existing technology meets all seven (Sec. 3, Table 1, p.162).
7. Longevity means the data stays readable after the vendor is gone. The paper names plain text, JPEG, PDF, and cites the US Library of Congress recommending XML, JSON, or SQLite as archival formats (Sec. 2.5, p.160).
8. Ownership has a cost: the user must handle backups and protection against data loss (Sec. 2.7, p.161).
9. CRDTs are a candidate base for real-time and offline collaboration. They merge concurrent edits automatically and only surface a conflict when two users change the same property of the same object (Sec. 4.1, p.168; Sec. 4.2.4, p.172). The authors say it is "not yet advisable" to use them in production over a proven product (Sec. 4.3, p.173).
10. CRDT costs seen in practice: history grows without bound, performance and memory problems on real documents, and unsolved network communication and peer discovery (Sec. 4.2.4, pp.172-173). Schema migration across app versions with no central schema is an open problem (Sec. 4.3.1, p.173).

## 3. Requirements for Reqlite

Labels follow the strict rule: MUST only if ignoring it causes a defect or data loss for this product. The paper gives principles, not file-format rules. Where a requirement is an engineering inference (not a paper claim), the rationale says so.

| # | Label | Requirement | Crate or component | Paper section | Rationale |
|---|---|---|---|---|---|
| 1 | MUST | One request per file. A request never shares a file with another request. A folder is a collection. | `format`, collections | 3.1.3, 3.1.4 | Git and folder sync merge per file. Two people editing different requests then never conflict. The paper shows that same-file edits cause conflicted copies. |
| 2 | MUST | Deterministic serialization. A save of an unchanged request must produce byte-identical output. Fixed field order, sorted map keys, one newline style, fixed quoting. Add a `format::to_string` and a round-trip test (parse, write, parse, write gives the same bytes). | `format` | 3.1.4 (Git is line-oriented, so churn becomes diff noise and merge conflicts) | Inference. The paper says Git works on line-based text only. A writer that reorders lines turns a no-op save into a diff. Today `format` has only `parse`, no writer. `BTreeMap` already gives sorted keys for `headers` and `query`. |
| 3 | MUST | Atomic file writes: write to a temp file in the same directory, fsync, rename over the target. Never truncate in place. | `format` writer or `store`/collections layer | 2.7, 2.5 (user owns the data and must not lose it) | Inference. A crash mid-write would corrupt the only copy of the user's request. The paper puts the data burden on the local copy, so the app must not be the cause of loss. |
| 4 | MUST | Versioned format with a hard reject of unknown future versions. Keep the existing `version` field and the `UnsupportedVersion` error. Never silently drop fields written by a newer build. Keep `deny_unknown_fields` for same-version files. | `format` | 2.5, 4.3.1 | Different collaborators may run different app versions with no central schema. A newer file opened by an older build must fail loudly, not be rewritten lossy and committed. |
| 5 | MUST | Request files stay plain, human-readable TOML, parseable without Reqlite. Never move source-of-truth data into SQLite. | `format`, `store` | 2.5, 2.7, Table 1 | This is the longevity and user-control ideal. It is already decided. Keep it as a regression guard. |
| 6 | MUST | The `store` SQLite file is deletable and rebuildable from the files alone. No request content lives only in SQLite. History entries that are not derivable from files (past responses) are accepted loss on delete, and the UI must say so. | `store` | 2.5, 2.7, 3.2.2 | If the DB holds unique data, a Git clone on a second machine is incomplete. That is the "trapped on one device" failure (2.2). |
| 7 | SHOULD | Detect external edits. Watch the collection folder (the `notify` crate is the likely choice; verify before adding). On change, reload the file. If the open editor has unsaved edits to the same file, show a conflict choice. Never silently overwrite the on-disk version. | GUI, collections | 2.4, 3.1.3 | Git pull, an editor, or a sync tool will change files under the app. The paper's conflicted-copy figures show what happens when the app ignores this. SHOULD, not MUST, because v0.1 can ship with reload-on-focus. |
| 8 | SHOULD | Do not use the file watcher as the only trigger. Also re-stat files on window focus and before each save. Compare mtime plus size or a content hash recorded at load time. | collections | 2.3, 3.1.3 | Inference. Watchers drop events on some platforms and network drives. A save-time check prevents overwriting an external change. |
| 9 | SHOULD | No spinners on local operations. Load, edit, and save of request files must not wait on the network or the DB. Send is the only async step. Measure keystroke-to-render in the CI budget set. | GUI, `engine` | 2.1 | This matches the existing budgets. Add an input-latency check to the GUI spike. |
| 10 | SHOULD | Export and backup are trivial because the folder is the data. Document "copy the folder" as the backup. Provide cURL export. | docs, CLI | 2.5, 4.3.3 (Longevity, User control) | The paper's practitioner advice is to export to stable flat formats. Reqlite already is one. Only docs and cURL export are needed. |
| 11 | SHOULD | Keep the secrets split visible. The UI should say which data stays on the device (keychain, local env file) and which is committed (request files). | GUI, env | 2.6, 4.3.3 (Privacy) | The paper advises making clear to users where data is stored. This matters because request files are committed to Git. |
| 12 | SHOULD | Add a lint that warns when a request file contains a likely secret (a literal `Authorization` value, long token strings) instead of `{{var}}`. | `format` or CLI | 2.6, 2.7 | Inference. Git sync copies the data to a server that may be public. Warn, do not block. |
| 13 | SKIP | CRDTs (Automerge or similar) for request files. | none | 4.1, 4.2.4 | Collaboration here is asynchronous through Git. Per-file merge by Git is enough. CRDTs add unbounded history and the paper itself calls them not production-ready. A CRDT also would not stay human-readable TOML. Revisit only if real-time co-editing is demanded. |
| 14 | SKIP | Real-time multi-user editing and presence. | none | 2.4, 4.2 | Out of scope for v1. No server exists. |
| 15 | SKIP | Peer-to-peer sync, WebRTC, Hypercore, Dat. | none | 4.2.4 | The paper reports NAT traversal as unreliable. Git covers sync. |
| 16 | SKIP | Version-history or "time travel" UI built by Reqlite. | none | 4.2.1, 4.3.2 | Git already is the history. A Git panel is already planned for v0.4. |
| 17 | SKIP | Treating the cloud as a "cloud peer" for backup. | none | 4.2.4 | Already decided: optional later sync via the user's own GitHub repo or Gist. Nothing to add. |
| 18 | SKIP | Format-aware three-way merge driver for TOML. | none | 3.1.4 | Looks relevant, since Git is weak on non-text. It is not needed: TOML is text and one request per file makes conflicts rare. Revisit only if user reports show frequent conflicts inside one file. |

Where state belongs (inferred from rows 5 and 6):

| Data | Home |
|---|---|
| Request definitions, collection layout, environment definitions without secrets | Files (committed) |
| Secrets | OS keychain and gitignored local env file |
| Response history, search index, tab and window state | SQLite (rebuildable cache) |

## 4. Process and testing practices to adopt

1. Round-trip golden test: parse then write every `examples/*.toml` file, assert byte-identical output. Add a second case: a request with keys in shuffled order is written back in canonical order.
2. Idempotence property test: `write(parse(write(x))) == write(x)` for generated requests.
3. Git merge test in CI: create a temp repo, two branches edit different requests in one collection, merge, assert no conflict. Then two branches edit different fields of the same request, assert the merge result still parses. This shows the clean-merge claim, not just asserts it.
4. Crash-safety test for atomic write: inject a failure between temp write and rename, assert the original file is intact and parses.
5. Forward-compat test: a `version = 2` file gives `UnsupportedVersion` and the file on disk is not modified by any code path.
6. Store rebuild test: delete the SQLite file, reopen the collection, assert requests and tree are identical. Name the break first: a bug that stores a request only in SQLite makes this fail.
7. External edit test (once the watcher exists): modify a file on disk while the model holds a dirty copy, assert no silent overwrite.
8. Offline test: run the CLI and GUI with the network blocked, assert load, edit, and save work. Only send fails.
9. Score the product against the seven ideals at each minor release (the paper's practitioner step, Sec. 4.3.3, p.174). Keep a short table in the docs.
10. Adjacent observation, not from the paper: `headers` and `query` are `BTreeMap<String, String>`. This gives sorted, stable output, but it cannot represent repeated keys (for example `?tag=a&tag=b`). Decide this before the format is frozen, because changing the shape later is a breaking change that needs `version = 2`.

## 5. Where the paper is wrong for, or overreaches for, this project

1. Its core problem is collaboration and sync. Reqlite v1 has one user per machine. Ideals 2.2 and 2.4 are served by Git, which the paper rates only "partial" for both. For API requests that is enough, because requests are small text files edited rarely and by few people.
2. The CRDT argument targets rich, fine-grained, real-time documents (text, canvases, boards). A request file has a few short fields. Automatic merge at the character level has no value here.
3. The paper calls Git weak on non-text formats. Reqlite chose text (TOML), so that weakness does not apply.
4. The paper treats peer-to-peer as the decentralized end goal. Reqlite should not follow it. The paper's own evidence (NAT traversal, discovery) argues against it, and Git hosts already solve it.
5. The prototypes are small and the findings are self-described as subjective: five team members, about ten external testers, no formal method (Sec. 4.2.4, p.171). Treat "conflicts are rare" as a hypothesis, not a measurement.
6. The paper is silent on file-level engineering: serialization determinism, atomic writes, file watching, schema evolution mechanics. Rows 2, 3, 7, and 8 above come from engineering practice, not from the paper. It names schema migration only as an open problem (Sec. 4.3.1).
7. It does not discuss secrets in synced data. Its privacy section is about servers and end-to-end encryption. Reqlite's secret handling (keychain plus gitignored file) is a separate design that the paper does not inform.
8. Ideal 2.6 (end-to-end encryption) does not apply to v1. Files sit in plain text in the user's own repo.
