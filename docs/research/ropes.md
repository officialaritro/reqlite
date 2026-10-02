# Ropes: an Alternative to Strings

## 1. Citation and source

Boehm, H.-J., Atkinson, R., Plass, M. "Ropes: an Alternative to Strings." Software: Practice and Experience, vol. 25, no. 12, pp. 1315-1330, December 1995. Xerox PARC.

URL read in full (16 PDF pages, journal pages 1315-1330, including the appendix and references):
https://www.cs.tufts.edu/comp/150FP/archive/hans-boehm/ropes.pdf

Page numbers below are journal page numbers.

Two Rust crates were checked on crates.io and the ropey README on GitHub in this session:

1. `ropey` 1.6.1 (a 2.0.0-beta.1 also exists). "A fast and robust text rope for Rust". UTF-8 only. Edits and indexing use char indices. Tracks line breaks. Has `Rope::from_reader`. The README says it is an in-memory structure and is not good for texts larger than available memory, or for texts under a couple of KB.
2. `memmap2` 0.9.11. "Cross-platform Rust API for memory-mapped file IO".

`crop` 0.4.3 ("A pretty fast text rope") also exists. Its API was not read, so no claim is made about it.

## 2. Core ideas

1. A rope is a tree. Internal nodes mean "concatenate children". Leaves are flat strings. The string is the leaves read left to right. (p. 1316-1317, Figure 1)
2. Ropes are immutable. Operations never change an input, so they need no locking and share structure freely. (pp. 1315, 1318)
3. Fetch, concatenate, and substring cost O(log n) on a balanced tree. Only the nodes on the path are copied. Traversal is O(n). (p. 1317)
4. Concatenation is made O(1) in practice. It adds a root node and does not rebalance. Rebalancing runs on demand, or when depth passes a threshold. (p. 1317-1318)
5. Small-leaf rule. Concatenating short flat strings merges them into one flat leaf. This keeps leaves a reasonable size when a rope is built one character at a time. (p. 1318)
6. Rebalancing uses Fibonacci length intervals. A rope of depth n is balanced if its length is at least F(n+2). The result is a new tree. The original is untouched. (pp. 1319-1320, Figure 2)
7. Function leaves. A leaf holds a length and a function that returns the i-th character. This lets any sequence, such as a file, act as a rope without copying. (pp. 1318, 1321)
8. Lazy substring nodes. Substring of a long flat leaf returns a node pointing into it, not a copy. Nested substring nodes are collapsed. (pp. 1319, 1321, 1323)
9. Edit history is a stack of whole-file ropes. The toy editor keeps full undo this way. History entries share nearly all space, and large files are read lazily. (p. 1322)
10. Measured results (SPARCstation 2, 1994). Rope concatenation stays near constant (about 10 microseconds) from 10 to 100,000 characters. Flat C concatenation grows linearly. Traversal of a rope is slower than a flat string, roughly 1.5x to 8x on the plotted lines. Numbers are "rough values". (pp. 1323-1326, 1327)

## 3. Requirements for Reqlite

Two separate problems. Do not mix them.

- Problem A. Read-only response viewer. Body arrives once, never edited, up to 50 MB. Needs view, pretty-print, search, line index.
- Problem B. Editable request body editor. Small to medium text, many edits, undo.

Memory math. The current engine holds the body as `Vec<u8>` (`crates/engine/src/lib.rs`, SHORTCUT comment). A 50 MB body is 50 MB of heap. The idle GUI budget is under 50 MB, and idle means no big response open. So a 50 MB body held in RAM by itself equals the whole idle budget. Any second copy (pretty-printed text, a rope with node overhead, a UTF-8 String copy, a line index) adds on top. The HANDOFF budget for "50 MB JSON without freezing" has no peak RAM figure. That gap is itself a risk (see MUST 1).

### Problem A: response viewer

| # | Level | Decision | Target | Paper section | Rationale |
|---|---|---|---|---|---|
| A1 | MUST | Set an explicit peak RAM budget for "open 50 MB JSON" and measure it in CI, in addition to idle RAM. | CI budget check, `store`/GUI | Performance, pp. 1323-1326 (the paper measures space and time, not just speed) | Idle < 50 MB does not bound the open case. Without a peak figure the 50 MB budget can pass while memory use is 3x the body. |
| A2 | MUST | Resolve the engine SHORTCUT: stream the body to a temp file above a threshold, keep only a bounded in-memory prefix. The viewer reads from the file. | `reqlite-engine` `Response.body` becomes an enum (in-memory bytes or temp file path plus length) | Alternative, p. 1316 (leaf can be a file, "convert files into strings without first reading them") | A 50 MB `Vec<u8>` equals the idle budget. The SHORTCUT already names this trigger. The paper's file-backed leaf is the same idea. |
| A3 | MUST | Build a line-offset index (sorted `u32` or `u64` byte offsets) over the body or temp file. Render only visible lines from it (virtualized list). | GUI viewer component, new small module | Fetch ith element via length fields, p. 1317 (position index into a long sequence) | The viewer needs "line N" in O(1) or O(log n). An offset array is the simplest structure that does this for a read-only text. About 1.5M lines at 33 bytes per line costs about 6 MB with `u32` offsets. |
| A4 | MUST | Pretty-print by streaming into a second temp file, then index lines of the pretty file. Never build a pretty `String` of 50 MB in RAM. Do not assume the raw body has lines. | engine or viewer helper | Substring and lazy nodes avoid copying, pp. 1319, 1321 | Minified JSON is often one 50 MB line. A line index on the raw body is then useless. Pretty-print creates the lines. Doing it in RAM adds a full second copy and breaks A1. |
| A5 | SHOULD | Map the temp file with `memmap2` (verified to exist) for random access, or use buffered `pread`-style reads of the visible window. Pick by measuring. | viewer file access | Function leaf, pp. 1318, 1321 | Read only the visible window and pay no heap for the rest. Mmap has hazards if the file is truncated while mapped. The app owns the temp file, so risk is low. Windows mmap RAM accounting differs, so measure there. |
| A6 | SHOULD | Search by scanning the file in chunks with a streaming matcher. Keep match offsets, not match text. Cap the number of stored matches. | viewer search | Traversal, p. 1317 and p. 1326 (traversal is linear) | Linear scan is the honest cost. A 50 MB scan takes tens of milliseconds to low hundreds on a modern CPU (estimate, not measured here). Run it off the UI thread. |
| A7 | SHOULD | Detect binary or non-UTF-8 bodies and show a hex or "download" view. Do not feed them to a text path. | viewer | n/a (ropey README: UTF-8 only) | Response bodies can be any bytes. `String::from_utf8_lossy` over 50 MB is a hidden full copy. |
| A8 | SKIP | Do not use a rope (`ropey` or custom) as the response body store. | viewer | Whole paper | A rope buys cheap concatenation, substring, and edit. The response is written once and never edited. The ropey README says it is an in-memory structure, so it does nothing for the RAM problem. It adds per-node and per-chunk overhead on top of 50 MB. A temp file plus offset index is smaller and simpler. |
| A9 | SKIP | Do not write a custom rope with Fibonacci rebalancing. | n/a | Rebalancing, pp. 1319-1320 | No concatenation-heavy workload exists in the viewer. |
| A10 | SKIP | Do not build function leaves, lazy substring nodes, or a persistent history of ropes for responses. | n/a | pp. 1318, 1321, 1322 | The temp file already is the lazy leaf. Substring is just an offset range. |

### Problem B: request body editor

| # | Level | Decision | Target | Paper section | Rationale |
|---|---|---|---|---|---|
| B1 | SHOULD | Use the GUI toolkit's built-in text editing widget first for request bodies. Measure it with a 1 MB body before adding any rope. | GUI spike (iced / Slint / Tauri+Svelte) | Introduction, p. 1316 (ropes matter for long strings and middle edits) | Typical request bodies are small. A flat buffer handles them. The edit-cost argument applies only past very large sizes. |
| B2 | SHOULD | If the toolkit editor is too slow on large bodies (for example a pasted 20 MB JSON), back the editor with `ropey` (verified, UTF-8 text rope with line tracking and `from_reader`). Do not write a rope. | editor component | Edit history as stack of ropes, p. 1322; "insert into the middle of a 100,000 character string", p. 1324 | Mid-text insert and delete are the case ropes fix. `ropey` is an existing, maintained rope with line indexing, which the editor needs for line numbers. |
| B3 | SHOULD | Implement undo as snapshots of the immutable rope (cheap clone), if ropey is adopted. Check that `Rope::clone` shares structure before relying on it. | editor component | p. 1322 ("complete edit history as simply a stack of ropes") | Matches the paper's technique. Ropey's README claims low memory use. The structure-sharing clone claim was not verified in this session, so test it. |
| B4 | SKIP | Do not add a rope or edit-history structure for request bodies under about 1 MB. | editor | p. 1316 | No defect and no budget at stake. YAGNI. |
| B5 | SKIP | Do not use a rope for TOML request files in `reqlite-format`. | `crates/format` | n/a | They are parsed whole with `toml` and are small. |

## 4. Process and testing practices

1. Generate fixtures with a script, not by hand. Make three 50 MB JSON files: minified single line, pretty-printed with deep nesting, and one large array of small objects. Add a 50 MB file with one very long string value. Do not commit the files. Generate them in CI.
2. Add a benchmark that serves the fixture from a local one-shot server (the engine tests already have a `serve_once` pattern) and records peak RSS and time to first visible line. Fail CI above the budget. This extends HANDOFF budget 4.
3. Test the viewer through behaviour. Assert that line 1,000,000 of the pretty file shows the right text, and that search finds a known needle at a known offset. Do not assert on internal structures.
4. Test edge input. Empty body, body with no newline, CRLF, invalid UTF-8, a body that is a single 50 MB line, and a body that ends without a trailing newline.
5. Measure on all three OSes. Idle RAM and mmap behaviour differ on Windows.
6. If B2 is adopted, run a mutation check on undo: apply 10,000 random edits, undo all, assert the text equals the original.

## 5. Where the paper is wrong for, or overreaches for, this project

1. It is about general-purpose strings in a language runtime with a garbage collector. Reqlite has no GC and a hard RAM budget. Node overhead and shared structure are paid for in memory, which the paper does not weigh against a fixed ceiling. (pp. 1316, 1322)
2. It targets immutable, concatenation-heavy workloads, such as building output and edit history. A response viewer is write-once and read-only. The core benefit does not apply.
3. Performance data is from a 1994 SPARCstation 2, 100,000 characters at most, and the authors call the numbers "rough values". Nothing there is evidence for 50 MB on a current CPU with caches. (p. 1327)
4. The paper's traversal is slower than flat strings (Figure 6). For search over a 50 MB body, a flat file scan is the faster path.
5. The paper indexes by character and has no notion of lines, UTF-8, or Unicode line breaks. A viewer needs lines. `ropey` adds this, the paper does not.
6. Lazy function leaves (reading a file on demand) are the one idea that carries over. The paper does not discuss disk read cost, caching, or file changes during use. Those are Reqlite's job.
7. The paper says it is "not clear" that its design is optimal (Conclusions, p. 1326). Treat it as one valid design, not a requirement.
