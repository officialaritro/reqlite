# Adapton: demand-driven incremental computation

## 1. Citation and source

Matthew A. Hammer, Yit Phang Khoo, Michael Hicks, Jeffrey S. Foster. "Adapton: Composable, Demand-Driven Incremental Computation." PLDI 2014, Edinburgh, 9 to 11 June 2014. DOI 10.1145/2594291.2594324.

URL read (all 11 pages, downloaded with curl): http://matthewhammer.org/adapton/adapton-pldi2014.pdf

Crates checked on the crates.io API on 2026-10-02:

1. `salsa` 0.28.5, repo github.com/salsa-rs/salsa, described as "A generic framework for on-demand, incrementalized computation (experimental)".
2. `adapton` 0.3.31, repo github.com/Adapton/adapton.rust, last updated 2019-12-22. Treat it as unmaintained.

Note on the brief. "Memoization with names" (named thunks) is NOT in this paper. The paper memoizes by syntactic equality of the function and its argument (Sec 5.1). Named memoization is from later Adapton work that I did not read. This file covers only the 2014 paper.

## 2. Core ideas

1. Classic incremental computation (IC) keeps a total order of events. That blocks reuse in three patterns: sharing, swapping, switching. Sec 1, p1.
2. Classic IC is eager. Changing an input updates every dependent value, even values nobody needs now. Sec 1, p1.
3. Adapton joins IC with lazy thunks. `ref` cells are mutable inputs. `thunk` cells are suspended computations. Dependents of a changed `ref` are recomputed only when forced. Sec 1, p1 to p2.
4. The demanded computation graph (DCG) records which thunk read which `ref` or forced which thunk. It keeps only partial order, so subcomputations are reusable in any context. Sec 2, p2 to p3.
5. A strict split between an inner layer (read-only, incremental) and an outer layer (may allocate and mutate refs). Only the outer layer sets inputs. Sec 1 and Sec 3, p2 and p4.
6. Change propagation has two phases. Dirtying runs eagerly on `set`: walk incoming edges backward and mark them dirty. Propagation runs lazily on `force`: walk dirty outgoing edges in the order they were created, and re-evaluate a thunk only if a read value changed. Sec 5.2, Algorithm 1, p7 to p8.
7. Propagation stops early. If a re-evaluated target gives the same value as before, the caller is not re-run. Sec 5.2, p8.
8. Memoization is by function plus argument, using syntactic equality, in weak hash tables so the GC drops unused thunks. Sec 5.1, p7.
9. Soundness theorem: an incremental run gives the same result as from-scratch evaluation. Sec 4.2, p6. This matters because a wrong dependency edge is the classic IC bug.
10. Results: on lazy workloads, speedups over naive recompute are 2x to 2000x. When ALL output is demanded (batch), Adapton is only about 1.5x to 3.5x slower than the eager system, and it costs extra memory (Table 1, Sec 6.1, p8 to p9). A spreadsheet case study beats naive recompute once there are about four sheets (Sec 6.2, p9 to p10).

## 3. Requirements for Reqlite

Verdict first. A full Adapton engine (DCG, dirtying, propagation) is NOT needed. At hundreds to low thousands of small TOML files, a re-parse costs microseconds to a few milliseconds each. The `toml` parse of one request file is far below the 300 ms cold-start budget even for 5000 files. What IS worth taking is the discipline: pure inner functions, explicit inputs, and demand-driven evaluation. I did not benchmark Reqlite parse time. Treat the cost claim as an estimate to confirm in the store spike.

| # | Level | Requirement | Maps to | Paper section | Rationale |
|---|---|---|---|---|---|
| R1 | MUST | `interpolate(request, env) -> Request` is a pure function. It takes the env as an explicit argument and reads no globals, files, or clock. | `engine` env/interpolation | Sec 2, 3 (inner layer is read-only) | Purity is what makes any later caching correct. Hidden inputs cause stale values. |
| R2 | MUST | Resolve `{{var}}` lazily at send time (and at preview time for the URL under the cursor). Do not eagerly re-resolve every request when the active env changes. | `engine`, GUI | Sec 1 p1 (eager updates waste work), Sec 2 | Env switch must be O(1) and not touch 1000 files. This is the demand-driven idea at its simplest, with no graph. |
| R3 | MUST | The cache layer holds only derived data, with the TOML file as the one input. Any cached value is invalidated by file content hash (or mtime plus size), never by "I think it did not change". | `store` crate, cache rebuild | Sec 4.2 Theorem 4.3 (incremental result must equal from-scratch) | Matches the existing rule that SQLite is a rebuildable cache. A stale cache is a correctness defect for a git-synced file format. |
| R4 | SHOULD | Add a test that for any sequence of edits, cached/incremental results equal a full rebuild from disk (the soundness property, tested not proved). | `store`, collection tree tests | Sec 4.2 | Cheap property test. Catches invalidation bugs, the main risk of any cache. |
| R5 | SHOULD | Parse request files lazily: on startup list paths and names only, and parse a file body when its request is opened or searched. Keep a small memo `HashMap<PathBuf, (ContentHash, Arc<Request>)>`. | `format` consumer, collection loader | Sec 5.1 (memo table), Sec 1 (demand) | Protects cold start (300 ms) and idle RAM (50 MB) as collections grow. A hash-keyed map is enough, no DCG. |
| R6 | SHOULD | Derive the collection tree as a pure function of the sorted file list plus per-file name. On a file-watcher event, recompute only the affected directory subtree, or just rebuild the whole tree. | collection crate / GUI model | Sec 2 (reuse of unchanged subcomputations) | Whole rebuild of a few thousand entries is cheap. Subtree rebuild is a simple optimisation if profiling asks for it. |
| R7 | SHOULD | Make the GUI view function depend on small, comparable state slices, and skip work when the slice is equal (`PartialEq` or version counter). | GUI spike (iced, Slint, Svelte) | Sec 5.2 line 12 (stop when value unchanged) | Early cut-off is the transferable idea. The toolkits already have their own diffing. Check what each gives you before adding any of your own. |
| R8 | SHOULD | Keep file-watching events debounced, then treat each event as "mark path dirty". Re-read dirty paths on next demand. | collection loader | Sec 5.2 (dirty eagerly, repair lazily) | A tiny dirty-set plus lazy re-read is the whole useful core of the paper for this product. |
| R9 | SKIP | Full DCG with back-edges and ordered outgoing edge lists. | none | Sec 5, Algorithm 1 | Needs a graph, GC of weak tables, layer discipline. Costs more RAM and code than the work it saves at this scale. |
| R10 | SKIP | Adopt `salsa` as the engine. | none | Sec 7 (related), crates check | It exists and is maintained (0.28.5), but its own description says experimental. It adds a macro-heavy framework and compile time for a problem that a HashMap solves. Revisit only if a derived-query graph (imports, OpenAPI refs, cross-file vars) grows deep. |
| R11 | SKIP | Adopt the `adapton` crate. | none | crates check | Last update 2019. Do not depend on it. |
| R12 | SKIP | Swapping and switching reuse patterns. | none | Sec 1, 2 | They need a structural change of a computation tree. Reqlite has no such tree. A request file does not reorder its own subcomputations. |
| R13 | SKIP | Weak-pointer memo tables that rely on a GC. | none | Sec 5.1 | Rust has no tracing GC. Use an LRU or a bounded map instead (SHOULD cap the memo at N entries so idle RAM stays under 50 MB). |
| R14 | SKIP | Incremental re-render of the 50 MB JSON viewer through this mechanism. | GUI response viewer | none | The budget is about virtualised rendering and lazy parse. The paper does not address it. |

## 4. Process and testing practices

1. State the invariant: "cache equals a rebuild from disk". Write one randomised test: apply random file add, edit, delete, rename events; after each, compare the cache to a rebuild (R4).
2. Keep inner functions pure and put them in `format` or `engine`, so they run with no IO in unit tests.
3. Add a CI benchmark that loads a generated 2000-file collection and checks cold start and idle RAM against the budgets. Do this before adding any memo table, then add the memo only if the number fails. The paper's own results show overhead (Table 1, memory overhead 2.7x to 14x vs plain lazy), so measure the cost of caching too.
4. Test the early cut-off: change an env var that no request uses, and assert no request is re-resolved (count calls through a test seam on the function, or via observable output equality).
5. Keep a `SHORTCUT:` comment on the memo table naming the ceiling (for example, unbounded growth) and the trigger (collection over N files).

## 5. Where the paper is wrong for, or overreaches for, this project

1. The setting is a functional language with thunks. Reqlite is Rust with owned data. The inner/outer layer rule is enforced by OCaml convention (footnote 1, p7), not by types. In Rust you would use API shape instead.
2. The benchmarks use inputs of 1e5 to 1e6 items (Sec 6.1, p8). Reqlite has thousands of small files. At that size, the bookkeeping overhead of a DCG can exceed the work saved, as the batch row of Table 1 hints.
3. Speedups are measured against naive recompute of huge pure computations. In Reqlite the heavy costs are network, TLS, and 50 MB response handling. None of them is an incremental computation.
4. The memory overhead (Table 1) is a poor fit for a 50 MB idle budget.
5. The paper relies on OCaml's GC and weak tables to free dead thunks (Sec 5.1, 5.2). That has no direct Rust analogue.
6. The paper gives memoization by syntactic equality only. It says nothing about named memoization, hashing large inputs, or content-addressed invalidation, which is the real question for files on disk.
7. The paper has no treatment of IO, file watching, or external change. Its inputs are in-memory refs set by the program.
8. The spreadsheet study (Sec 6.2) has a deep formula DAG. A Reqlite environment lookup is one map access, so the analogy is weak.
