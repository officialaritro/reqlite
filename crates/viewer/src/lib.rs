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
    kind: Kind,
}

/// How a body is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Re-indented JSON.
    Json,
    /// Re-indented XML or HTML.
    Markup,
    /// Byte for byte.
    Plain,
}

impl Document {
    /// Reads `body` to the end. JSON (a body starting with `{` or `[`) and
    /// markup (starting with `<`) are pretty-printed. Anything else is kept
    /// byte for byte.
    pub fn build(body: impl Read) -> io::Result<Document> {
        Document::build_with(body, None)
    }

    /// Like [`Document::build`], but the response's Content-Type decides the
    /// kind when it names JSON, XML, HTML or plain text.
    pub fn build_with(body: impl Read, content_type: Option<&str>) -> io::Result<Document> {
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
        let kind = kind_of(content_type).unwrap_or(match first {
            Some(b'{' | b'[') => Kind::Json,
            Some(b'<') => Kind::Markup,
            _ => Kind::Plain,
        });
        match kind {
            Kind::Json => prettify(&mut input, &mut out)?,
            Kind::Markup => prettify_markup(&mut input, &mut out)?,
            Kind::Plain => {
                out.write_all(&lead)?;
                io::copy(&mut input, &mut out)?;
            }
        }
        let (checkpoints, line_count) = out.finish()?;
        Ok(Document {
            file,
            checkpoints,
            line_count,
            kind,
        })
    }

    pub fn line_count(&self) -> usize {
        self.line_count
    }

    /// True when the body was JSON and is shown pretty-printed.
    pub fn is_pretty(&self) -> bool {
        self.kind == Kind::Json
    }

    pub fn kind(&self) -> Kind {
        self.kind
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

/// The kind a Content-Type names, if it names one this viewer knows.
fn kind_of(content_type: Option<&str>) -> Option<Kind> {
    let media = content_type?.split(';').next()?.trim().to_ascii_lowercase();
    if media.ends_with("json") {
        Some(Kind::Json)
    } else if media.ends_with("xml") || media == "text/html" || media.ends_with("+html") {
        Some(Kind::Markup)
    } else if media.starts_with("text/") {
        Some(Kind::Plain)
    } else {
        None
    }
}

/// HTML elements with no closing tag. They do not indent what follows.
const VOID: [&str; 14] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// Elements whose content is code or spacing-sensitive text. It is kept as is.
const RAW: [&str; 4] = ["script", "style", "pre", "textarea"];

/// Text waiting next to an open tag is kept up to this size to join `<b>hi</b>`
/// on one line. Past it, it is written out, so memory stays flat.
const MAX_PENDING: usize = 4096;

/// A tag or comment longer than this is written out as it stands, so one huge
/// attribute cannot hold the whole body in memory.
const MAX_TAG: usize = 1 << 20;

/// Re-indents XML or HTML as it streams: one tag per line, by depth. A tag
/// holding only short text stays on one line. Like the JSON printer it works
/// on tokens, so broken markup still comes out whole, indented oddly.
fn prettify_markup(input: &mut impl BufRead, out: &mut impl Write) -> io::Result<()> {
    let mut m = Markup {
        out,
        depth: 0,
        state: State::Text,
        tag: Vec::new(),
        text: Vec::new(),
        quote: None,
        open: None,
        raw_end: Vec::new(),
    };
    loop {
        let buf = input.fill_buf()?;
        if buf.is_empty() {
            break;
        }
        let len = buf.len();
        for &b in buf {
            m.byte(b)?;
        }
        input.consume(len);
    }
    m.end()
}

enum State {
    Text,
    Tag,
    /// `<!--` or `<![CDATA[`, until this end.
    Comment(&'static [u8]),
    /// Inside `<script>` and friends, until `</name`.
    Raw,
}

struct Markup<'w, W: Write> {
    out: &'w mut W,
    depth: usize,
    state: State,
    tag: Vec<u8>,
    text: Vec<u8>,
    quote: Option<u8>,
    /// An open tag not written yet, and its name: if only short text follows
    /// before its closing tag, all three go on one line.
    open: Option<(Vec<u8>, String)>,
    raw_end: Vec<u8>,
}

impl<W: Write> Markup<'_, W> {
    fn byte(&mut self, b: u8) -> io::Result<()> {
        if self.tag.len() >= MAX_TAG && matches!(self.state, State::Tag | State::Comment(_)) {
            self.flush_open()?;
            self.flush_text()?;
            let tag = std::mem::take(&mut self.tag);
            self.line(&tag)?;
            self.state = State::Text;
            self.quote = None;
        }
        match self.state {
            State::Text if b == b'<' => {
                self.state = State::Tag;
                self.tag.push(b);
            }
            State::Text => {
                self.text.push(b);
                if self.text.len() > MAX_PENDING {
                    self.flush_open()?;
                    self.flush_text()?;
                }
            }
            State::Tag => {
                self.tag.push(b);
                match self.quote {
                    Some(q) if b == q => self.quote = None,
                    Some(_) => {}
                    None if self.tag == b"<!--" => self.state = State::Comment(b"-->"),
                    None if self.tag == b"<![CDATA[" => self.state = State::Comment(b"]]>"),
                    None if (b == b'"' || b == b'\'') && self.tag.len() > 2 => self.quote = Some(b),
                    None if b == b'>' => self.close_tag()?,
                    None => {}
                }
            }
            State::Comment(end) => {
                self.tag.push(b);
                if self.tag.ends_with(end) {
                    self.flush_open()?;
                    self.flush_text()?;
                    let tag = std::mem::take(&mut self.tag);
                    self.line(&tag)?;
                    self.state = State::Text;
                }
            }
            State::Raw => {
                self.text.push(b);
                if self.text.len() >= self.raw_end.len()
                    && self.text[self.text.len() - self.raw_end.len()..]
                        .eq_ignore_ascii_case(&self.raw_end)
                {
                    let cut = self.text.len() - self.raw_end.len();
                    self.tag = self.text.split_off(cut);
                    let raw = std::mem::take(&mut self.text);
                    for l in raw.split(|&c| c == b'\n') {
                        let l = l.trim_ascii();
                        if !l.is_empty() {
                            self.line(l)?;
                        }
                    }
                    self.state = State::Tag;
                }
            }
        }
        Ok(())
    }

    fn close_tag(&mut self) -> io::Result<()> {
        let tag = std::mem::take(&mut self.tag);
        self.state = State::Text;
        let name = tag_name(&tag);
        if tag.starts_with(b"</") {
            let text = self.text.trim_ascii();
            let joins =
                self.open.as_ref().is_some_and(|(_, open)| *open == name) && !text.contains(&b'\n');
            if joins {
                if let Some((open, _)) = self.open.take() {
                    let mut l = open;
                    l.extend_from_slice(text);
                    l.extend_from_slice(&tag);
                    self.text.clear();
                    return self.line(&l);
                }
            }
            self.flush_open()?;
            self.flush_text()?;
            self.depth = self.depth.saturating_sub(1);
            return self.line(&tag);
        }
        self.flush_open()?;
        self.flush_text()?;
        let leaf = tag.starts_with(b"<!")
            || tag.starts_with(b"<?")
            || tag.ends_with(b"/>")
            || VOID.contains(&name.as_str());
        if leaf {
            return self.line(&tag);
        }
        if RAW.contains(&name.as_str()) {
            self.line(&tag)?;
            self.depth += 1;
            self.raw_end = format!("</{name}").into_bytes();
            self.state = State::Raw;
            return Ok(());
        }
        self.open = Some((tag, name));
        Ok(())
    }

    /// Writes a held open tag on its own line; what follows is one deeper.
    fn flush_open(&mut self) -> io::Result<()> {
        if let Some((open, _)) = self.open.take() {
            self.line(&open)?;
            self.depth += 1;
        }
        Ok(())
    }

    fn flush_text(&mut self) -> io::Result<()> {
        let text = std::mem::take(&mut self.text);
        for l in text.split(|&c| c == b'\n') {
            let l = l.trim_ascii();
            if !l.is_empty() {
                self.line(l)?;
            }
        }
        Ok(())
    }

    /// One line at the current depth. Line breaks inside a tag become spaces.
    fn line(&mut self, bytes: &[u8]) -> io::Result<()> {
        for _ in 0..self.depth.min(MAX_INDENT) {
            self.out.write_all(b"  ")?;
        }
        for &b in bytes {
            self.out
                .write_all(&[if b == b'\n' || b == b'\r' { b' ' } else { b }])?;
        }
        self.out.write_all(b"\n")
    }

    fn end(mut self) -> io::Result<()> {
        self.flush_open()?;
        self.flush_text()?;
        let tag = std::mem::take(&mut self.tag);
        if !tag.is_empty() {
            self.line(&tag)?;
        }
        Ok(())
    }
}

/// `<user id="1">` and `</user>` are both `user`, in lower case for HTML.
fn tag_name(tag: &[u8]) -> String {
    let rest = tag
        .strip_prefix(b"</")
        .or_else(|| tag.strip_prefix(b"<"))
        .unwrap_or(tag);
    let end = rest
        .iter()
        .position(|b| b.is_ascii_whitespace() || *b == b'>' || *b == b'/')
        .unwrap_or(rest.len());
    String::from_utf8_lossy(&rest[..end]).to_ascii_lowercase()
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

    fn markup(text: &str) -> Vec<String> {
        let doc = Document::build_with(text.as_bytes(), Some("text/html; charset=utf-8")).unwrap();
        assert_eq!(doc.kind(), Kind::Markup);
        all_lines(&doc)
    }

    #[test]
    fn markup_puts_each_tag_on_its_own_line_by_depth() {
        assert_eq!(
            markup(
                "<?xml version=\"1.0\"?><users><user id=\"1\"><name>Ada</name><tags><tag/></tags></user></users>"
            ),
            [
                "<?xml version=\"1.0\"?>",
                "<users>",
                "  <user id=\"1\">",
                "    <name>Ada</name>",
                "    <tags>",
                "      <tag/>",
                "    </tags>",
                "  </user>",
                "</users>",
            ]
        );
    }

    #[test]
    fn html_voids_comments_and_scripts_keep_their_shape() {
        assert_eq!(
            markup(
                "<!DOCTYPE html><html><head><meta charset=utf-8><script>if (a < b) { go(); }\n</script></head><body><!-- a > b --><p>Hi <b>there</b></p><br></body></html>"
            ),
            [
                "<!DOCTYPE html>",
                "<html>",
                "  <head>",
                "    <meta charset=utf-8>",
                "    <script>",
                "      if (a < b) { go(); }",
                "    </script>",
                "  </head>",
                "  <body>",
                "    <!-- a > b -->",
                "    <p>",
                "      Hi",
                "      <b>there</b>",
                "    </p>",
                "    <br>",
                "  </body>",
                "</html>",
            ]
        );
    }

    #[test]
    fn a_quoted_greater_than_sign_does_not_end_a_tag() {
        assert_eq!(
            markup("<a title=\"x > y\" href='/q?a=1&b=2'>link</a>"),
            ["<a title=\"x > y\" href='/q?a=1&b=2'>link</a>"]
        );
    }

    #[test]
    fn the_content_type_picks_the_kind_and_the_first_byte_is_the_fallback() {
        let kind = |body: &str, ct: Option<&str>| {
            Document::build_with(body.as_bytes(), ct).unwrap().kind()
        };
        assert_eq!(kind("<a/>", None), Kind::Markup);
        assert_eq!(kind("{\"a\":1}", None), Kind::Json);
        assert_eq!(kind("plain", None), Kind::Plain);
        assert_eq!(kind("[1]", Some("application/xml")), Kind::Markup);
        assert_eq!(kind("<a>", Some("application/problem+json")), Kind::Json);
        assert_eq!(kind("{\"a\":1}", Some("text/plain")), Kind::Plain);
    }

    #[test]
    fn broken_markup_keeps_every_byte_of_its_text() {
        let lines = markup("<a><b>one</a> two <c");
        let joined: String = lines.concat();
        for word in ["one", "two", "<c"] {
            assert!(joined.contains(word), "{lines:?}");
        }
    }

    #[test]
    fn a_huge_tag_is_written_out_whole_in_pieces() {
        let value = "v".repeat(3 * MAX_TAG);
        let lines = markup(&format!("<a title=\"{value}\">x</a>"));
        let joined: String = lines.concat();
        assert_eq!(joined.matches('v').count(), 3 * MAX_TAG, "every byte kept");
        assert!(lines.len() > 1, "split instead of buffered whole");
    }
}
