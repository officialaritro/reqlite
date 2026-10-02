//! A read-only view of a response body of any size.
//!
//! [`Document::build`] streams the body once into a temp file, pretty-printing
//! JSON on the way, and records where every 64th line starts. A GUI then asks
//! for the lines on screen with [`Document::lines`]. Memory stays flat: about
//! 8 bytes per 64 lines, whatever the body size. The body is never held whole.

use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};

/// Lines longer than this are split for display, so one huge line (minified
/// text, a long JSON string) cannot stall rendering.
pub const MAX_LINE: usize = 2000;

const CHECKPOINT_EVERY: usize = 64;

/// Indentation stops growing past this depth, so pathological nesting cannot
/// make the pretty file grow with the square of the body size.
const MAX_INDENT: usize = 64;

pub struct Document {
    /// Has no name on disk, so the OS frees it even if the process is killed.
    file: File,
    /// Byte offset of line 0, 64, 128, ...
    checkpoints: Vec<u64>,
    line_count: usize,
    pretty: bool,
}

impl Document {
    /// Reads `body` to the end. JSON (a body starting with `{` or `[`) is
    /// pretty-printed. Anything else is kept byte for byte.
    pub fn build(body: impl Read) -> io::Result<Document> {
        let file = tempfile::tempfile()?;
        let mut out = Indexer::new(BufWriter::new(&file));
        let mut input = BufReader::with_capacity(1 << 16, body);

        let mut lead = Vec::new();
        let first = loop {
            let buf = input.fill_buf()?;
            let Some(&b) = buf.first() else { break None };
            if b.is_ascii_whitespace() {
                lead.push(b);
                input.consume(1);
            } else {
                break Some(b);
            }
        };
        let pretty = matches!(first, Some(b'{' | b'['));
        if pretty {
            prettify(&mut input, &mut out)?;
        } else {
            out.write_all(&lead)?;
            io::copy(&mut input, &mut out)?;
        }
        let (checkpoints, line_count) = out.finish()?;
        Ok(Document {
            file,
            checkpoints,
            line_count,
            pretty,
        })
    }

    pub fn line_count(&self) -> usize {
        self.line_count
    }

    /// True when the body was JSON and is shown pretty-printed.
    pub fn is_pretty(&self) -> bool {
        self.pretty
    }

    /// Up to `count` lines starting at line `start`, without line endings.
    /// Bytes that are not UTF-8 show as U+FFFD; this text is for display only.
    pub fn lines(&self, start: usize, count: usize) -> io::Result<Vec<String>> {
        let end = start.saturating_add(count).min(self.line_count);
        if start >= end {
            return Ok(Vec::new());
        }
        let checkpoint = start / CHECKPOINT_EVERY;
        let mut reader = BufReader::new(ReadAt {
            file: &self.file,
            pos: self.checkpoints[checkpoint],
        });
        let mut line = Vec::new();
        let mut out = Vec::with_capacity(end - start);
        for n in checkpoint * CHECKPOINT_EVERY..end {
            line.clear();
            read_display_line(&mut reader, &mut line)?;
            if n >= start {
                out.push(String::from_utf8_lossy(&line).into_owned());
            }
        }
        Ok(out)
    }
}

/// Reads from its own position, so concurrent [`Document::lines`] calls never
/// move each other.
struct ReadAt<'a> {
    file: &'a File,
    pos: u64,
}

impl Read for ReadAt<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        #[cfg(unix)]
        let n = std::os::unix::fs::FileExt::read_at(self.file, buf, self.pos)?;
        #[cfg(windows)]
        let n = std::os::windows::fs::FileExt::seek_read(self.file, buf, self.pos)?;
        self.pos += n as u64;
        Ok(n)
    }
}

/// Reads one display line: up to `\n`, or up to [`MAX_LINE`] bytes ending on a
/// UTF-8 boundary. Must split lines exactly as [`Indexer`] counts them.
fn read_display_line(reader: &mut impl BufRead, line: &mut Vec<u8>) -> io::Result<()> {
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            return Ok(());
        }
        let mut used = 0;
        for &b in buf {
            if b == b'\n' {
                reader.consume(used + 1);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(());
            }
            if line.len() >= MAX_LINE && !is_continuation(b) {
                reader.consume(used);
                return Ok(());
            }
            line.push(b);
            used += 1;
        }
        reader.consume(used);
    }
}

fn is_continuation(b: u8) -> bool {
    b & 0b1100_0000 == 0b1000_0000
}

/// Counts display lines as bytes pass through, and records checkpoints.
struct Indexer<W: Write> {
    inner: W,
    offset: u64,
    /// A line has started and no `\n` has ended it yet.
    in_line: bool,
    line_len: usize,
    line_count: usize,
    checkpoints: Vec<u64>,
}

impl<W: Write> Indexer<W> {
    fn new(inner: W) -> Self {
        Indexer {
            inner,
            offset: 0,
            in_line: false,
            line_len: 0,
            line_count: 0,
            checkpoints: Vec::new(),
        }
    }

    fn start_line(&mut self) {
        if self.line_count % CHECKPOINT_EVERY == 0 {
            self.checkpoints.push(self.offset);
        }
        self.line_count += 1;
        self.line_len = 0;
    }

    fn finish(mut self) -> io::Result<(Vec<u64>, usize)> {
        self.inner.flush()?;
        Ok((self.checkpoints, self.line_count))
    }
}

impl<W: Write> Write for Indexer<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write_all(buf)?;
        for &b in buf {
            let split = b != b'\n' && self.line_len >= MAX_LINE && !is_continuation(b);
            if !self.in_line || split {
                self.start_line();
                self.in_line = true;
            }
            if b == b'\n' {
                self.in_line = false;
            } else {
                self.line_len += 1;
            }
            self.offset += 1;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Re-indents JSON as it streams. It works on tokens, not on a parsed tree,
/// so memory does not grow with the body. Invalid JSON still comes out with
/// every byte of its strings and values, just indented oddly.
fn prettify(input: &mut impl BufRead, out: &mut impl Write) -> io::Result<()> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut opened = false;
    let newline = |out: &mut dyn Write, depth: usize| -> io::Result<()> {
        out.write_all(b"\n")?;
        for _ in 0..depth.min(MAX_INDENT) {
            out.write_all(b"  ")?;
        }
        Ok(())
    };
    loop {
        let buf = input.fill_buf()?;
        if buf.is_empty() {
            break;
        }
        let len = buf.len();
        for &b in buf {
            if in_string {
                out.write_all(&[b])?;
                if escaped {
                    escaped = false;
                } else if b == b'\\' {
                    escaped = true;
                } else if b == b'"' {
                    in_string = false;
                }
                continue;
            }
            if b.is_ascii_whitespace() {
                continue;
            }
            if opened {
                opened = false;
                if b == b'}' || b == b']' {
                    out.write_all(&[b])?;
                    continue;
                }
                depth += 1;
                newline(out, depth)?;
            }
            match b {
                b'{' | b'[' => {
                    out.write_all(&[b])?;
                    opened = true;
                }
                b'}' | b']' => {
                    depth = depth.saturating_sub(1);
                    newline(out, depth)?;
                    out.write_all(&[b])?;
                }
                b',' => {
                    out.write_all(b",")?;
                    newline(out, depth)?;
                }
                b':' => out.write_all(b": ")?,
                b'"' => {
                    in_string = true;
                    out.write_all(b"\"")?;
                }
                _ => out.write_all(&[b])?,
            }
        }
        input.consume(len);
    }
    out.write_all(b"\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_lines(doc: &Document) -> Vec<String> {
        doc.lines(0, usize::MAX).unwrap()
    }

    #[test]
    fn pretty_prints_json_without_touching_strings() {
        let doc = Document::build(&br#" {"a":[1,2,{}],"b":"x, {y}: \"z\"","c":[]}"#[..]).unwrap();
        assert!(doc.is_pretty());
        assert_eq!(
            all_lines(&doc),
            [
                "{",
                "  \"a\": [",
                "    1,",
                "    2,",
                "    {}",
                "  ],",
                "  \"b\": \"x, {y}: \\\"z\\\"\",",
                "  \"c\": []",
                "}",
            ]
        );
    }

    #[test]
    fn keeps_other_bodies_byte_for_byte() {
        let doc = Document::build(&b"  hello\r\nworld\n\nend"[..]).unwrap();
        assert!(!doc.is_pretty());
        assert_eq!(all_lines(&doc), ["  hello", "world", "", "end"]);
        let mut raw = Vec::new();
        ReadAt {
            file: &doc.file,
            pos: 0,
        }
        .read_to_end(&mut raw)
        .unwrap();
        assert_eq!(raw, b"  hello\r\nworld\n\nend");
    }

    #[test]
    fn empty_body_has_no_lines() {
        let doc = Document::build(&b""[..]).unwrap();
        assert_eq!(doc.line_count(), 0);
        assert!(doc.lines(0, 10).unwrap().is_empty());
    }

    #[test]
    fn reads_any_window_across_checkpoints() {
        let text: String = (0..1000).map(|i| format!("line {i}\n")).collect();
        let doc = Document::build(text.as_bytes()).unwrap();
        assert_eq!(doc.line_count(), 1000);
        assert_eq!(
            doc.lines(127, 3).unwrap(),
            ["line 127", "line 128", "line 129"]
        );
        assert_eq!(doc.lines(998, 50).unwrap(), ["line 998", "line 999"]);
        assert!(doc.lines(1000, 5).unwrap().is_empty());
    }

    #[test]
    fn splits_a_huge_line_on_char_boundaries() {
        let long = "é".repeat(3 * MAX_LINE);
        let doc = Document::build(format!("{long}\nnext").as_bytes()).unwrap();
        let lines = all_lines(&doc);
        assert!(
            lines.iter().all(|l| !l.contains('\u{FFFD}')),
            "split inside a character"
        );
        assert!(
            lines[..lines.len() - 1]
                .iter()
                .all(|l| l.len() <= MAX_LINE + 1)
        );
        assert_eq!(lines.concat(), format!("{long}next"));
        assert_eq!(doc.line_count(), lines.len());
        let last = doc.line_count() - 1;
        assert_eq!(doc.lines(last, 1).unwrap(), ["next"]);
    }

    #[test]
    fn indentation_stops_growing_at_the_cap() {
        let depth = MAX_INDENT + 10;
        let body = format!("{}1{}", "[".repeat(depth), "]".repeat(depth));
        let doc = Document::build(body.as_bytes()).unwrap();
        let widest = all_lines(&doc).iter().map(String::len).max().unwrap();
        assert_eq!(widest, 2 * MAX_INDENT + 1);
    }

    #[test]
    fn line_count_matches_reading_for_every_split_case() {
        let cases: [&[u8]; 6] = [b"a\n", b"a\n\n", b"\n", b"\n\na", b"x", b"a\r\nb\r\n"];
        for body in cases {
            let doc = Document::build(body).unwrap();
            let lines = all_lines(&doc);
            assert_eq!(
                lines.len(),
                doc.line_count(),
                "{:?}",
                String::from_utf8_lossy(body)
            );
        }
    }
}
