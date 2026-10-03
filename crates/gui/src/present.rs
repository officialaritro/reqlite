//! How values are shown, and which window the OS can give us. No iced types.

/// The number of `name: value` entries in a field. Blank lines do not count,
/// the same as when the field is parsed.
pub fn entries(text: &str) -> usize {
    text.lines().filter(|l| !l.trim().is_empty()).count()
}

/// A byte count for people: "512 B", "1.5 KB", "50.0 MB".
pub fn human_size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut units = ["KB", "MB", "GB"].into_iter().peekable();
    let mut unit = "KB";
    while let Some(u) = units.next() {
        unit = u;
        // Round first, so 1023.96 KB shows as 1.0 MB, not 1024.0 KB.
        if (value * 10.0).round() < 10240.0 || units.peek().is_none() {
            break;
        }
        value /= 1024.0;
    }
    format!("{value:.1} {unit}")
}

/// True when the window can be transparent with a blurred backdrop.
/// winit blurs on macOS, and on Linux only under KDE on Wayland. Elsewhere a
/// transparent window would show the raw desktop, so it stays opaque.
pub fn glass_supported(os: &str, desktop: Option<&str>, wayland: bool) -> bool {
    match os {
        "macos" => true,
        "linux" => wayland && desktop.is_some_and(|d| d.split(':').any(|d| d == "KDE")),
        _ => false,
    }
}

/// The shortest scrollbar thumb, in pixels, so it stays easy to grab.
pub const MIN_THUMB: f32 = 24.0;

/// The scrollbar thumb on a `track` pixels tall: `(offset from the top, length)`.
/// `top` runs from 0 to `lines - 1`, the same range the viewer scrolls over.
pub fn thumb(track: f32, lines: usize, visible: usize, top: usize) -> (f32, f32) {
    let len = (track * visible as f32 / lines.max(1) as f32).clamp(MIN_THUMB.min(track), track);
    let max_top = lines.saturating_sub(1);
    let offset = if max_top == 0 {
        0.0
    } else {
        ((track - len) as f64 * top.min(max_top) as f64 / max_top as f64) as f32
    };
    (offset, len)
}

/// The top line for a thumb whose top edge sits `offset` pixels down the track.
pub fn top_at(track: f32, lines: usize, visible: usize, offset: f32) -> usize {
    let travel = track - thumb(track, lines, visible, 0).1;
    if travel <= 0.0 {
        return 0;
    }
    let max_top = lines.saturating_sub(1);
    ((offset / travel).clamp(0.0, 1.0) as f64 * max_top as f64).round() as usize
}

/// What a piece of a pretty-printed JSON line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    Key,
    String,
    Number,
    Literal,
    Punct,
    /// Markup: `<name`, `</name`, `>`, `/>`.
    Tag,
    /// Markup: an attribute name.
    Attr,
    /// Markup: text between tags.
    Text,
    /// Markup: `<!-- ... -->`, a doctype, or `<?xml ...?>`.
    Comment,
}

/// Splits one pretty-printed JSON line into coloured pieces. Spaces stay in
/// the piece before them, so the pieces always join back into the line.
pub fn json_tokens(line: &str) -> Vec<(&str, Token)> {
    let bytes = line.as_bytes();
    let mut out: Vec<(&str, Token)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        let token = match bytes[i] {
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i = (i + 1).min(bytes.len());
                let rest = line[i..].trim_start();
                if rest.starts_with(':') {
                    Token::Key
                } else {
                    Token::String
                }
            }
            b'-' | b'0'..=b'9' => {
                i += 1;
                while i < bytes.len()
                    && matches!(bytes[i], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
                {
                    i += 1;
                }
                Token::Number
            }
            b'a'..=b'z' | b'A'..=b'Z' => {
                while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                    i += 1;
                }
                Token::Literal
            }
            _ => {
                // Everything else, up to the next token, is one piece. Multi-byte
                // characters land here whole, as their bytes are all >= 0x80.
                i += 1;
                while i < bytes.len()
                    && !matches!(bytes[i], b'"' | b'-' | b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z')
                {
                    i += 1;
                }
                Token::Punct
            }
        };
        out.push((&line[start..i], token));
    }
    out
}

/// Splits one pretty-printed XML or HTML line into coloured pieces. Like
/// [`json_tokens`], the pieces always join back into the line.
pub fn markup_tokens(line: &str) -> Vec<(&str, Token)> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    let mut i = b.iter().take_while(|c| c.is_ascii_whitespace()).count();
    if i > 0 {
        out.push((&line[..i], Token::Punct));
    }
    let rest = &line[i..];
    if rest.starts_with("<!") || rest.starts_with("<?") {
        out.push((rest, Token::Comment));
        return out;
    }
    // Runs of bytes while `keep` holds. Every stop byte is ASCII, so each
    // piece ends on a character boundary.
    let run = |i: usize, keep: &dyn Fn(u8) -> bool| -> usize {
        i + b[i..].iter().take_while(|&&c| keep(c)).count()
    };
    let name = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b':' | b'.');
    while i < b.len() {
        if b[i] != b'<' {
            let end = run(i, &|c| c != b'<');
            out.push((&line[i..end], Token::Text));
            i = end;
            continue;
        }
        let start = i;
        i += 1;
        if b.get(i) == Some(&b'/') {
            i += 1;
        }
        i = run(i, &name);
        out.push((&line[start..i], Token::Tag));
        while i < b.len() {
            match b[i] {
                b'>' => {
                    out.push((&line[i..=i], Token::Tag));
                    i += 1;
                    break;
                }
                b'/' if b.get(i + 1) == Some(&b'>') => {
                    out.push((&line[i..i + 2], Token::Tag));
                    i += 2;
                    break;
                }
                c if c.is_ascii_whitespace() => {
                    let end = run(i, &|c| c.is_ascii_whitespace());
                    out.push((&line[i..end], Token::Punct));
                    i = end;
                }
                b'=' => {
                    out.push((&line[i..=i], Token::Punct));
                    i += 1;
                    let end = match b.get(i) {
                        Some(&q @ (b'"' | b'\'')) => {
                            let close = run(i + 1, &|c| c != q);
                            (close + 1).min(b.len())
                        }
                        _ => run(i, &|c| !c.is_ascii_whitespace() && c != b'>' && c != b'/'),
                    };
                    if end > i {
                        out.push((&line[i..end], Token::String));
                        i = end;
                    }
                }
                _ => {
                    let end = run(i, &|c| {
                        !c.is_ascii_whitespace() && !matches!(c, b'=' | b'>' | b'/')
                    })
                    .max(i + 1);
                    out.push((&line[i..end], Token::Attr));
                    i = end;
                }
            }
        }
    }
    out
}

/// True when pasted text is a cURL command, so it should be imported and not
/// typed into the URL field: the first word is `curl`, and something follows.
pub fn is_curl(text: &str) -> bool {
    let mut words = text.split_whitespace();
    let first = words.next().unwrap_or_default();
    // `/usr/bin/curl` is a path to the program. `https://h/curl` is a URL.
    let is_curl = first == "curl"
        || (first.ends_with("/curl") && !first.contains("://"))
        || first.eq_ignore_ascii_case("curl.exe");
    is_curl && words.next().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_skip_blank_lines() {
        assert_eq!(entries(""), 0);
        assert_eq!(entries("\n  \n"), 0);
        assert_eq!(entries("A: 1\n\nA: 2\n"), 2);
    }

    #[test]
    fn sizes_use_the_largest_unit_under_1024() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(52_428_818), "50.0 MB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn a_size_that_rounds_up_moves_to_the_next_unit() {
        assert_eq!(human_size(1_048_575), "1.0 MB");
    }

    #[test]
    fn glass_only_where_the_backdrop_is_blurred() {
        assert!(glass_supported("macos", None, false));
        assert!(glass_supported("linux", Some("KDE"), true));
        assert!(glass_supported("linux", Some("ubuntu:KDE"), true));
        assert!(!glass_supported("linux", Some("KDE"), false));
        assert!(!glass_supported("linux", Some("GNOME"), true));
        assert!(!glass_supported("linux", None, true));
        assert!(!glass_supported("windows", None, false));
    }

    #[test]
    fn the_thumb_spans_the_track_by_the_share_of_lines_on_screen() {
        assert_eq!(thumb(400.0, 100, 25, 0), (0.0, 100.0));
        // At the last line the thumb touches the bottom.
        assert_eq!(thumb(400.0, 100, 25, 99), (300.0, 100.0));
        // A short body fills the track.
        assert_eq!(thumb(400.0, 10, 40, 0), (0.0, 400.0));
        // Millions of lines still give a thumb that can be grabbed.
        assert_eq!(thumb(400.0, 5_467_582, 40, 0).1, MIN_THUMB);
    }

    #[test]
    fn dragging_the_thumb_maps_back_to_the_line_under_it() {
        assert_eq!(top_at(400.0, 100, 25, 0.0), 0);
        assert_eq!(top_at(400.0, 100, 25, 150.0), 50);
        assert_eq!(top_at(400.0, 100, 25, 300.0), 99);
        // Past either end it stops at the end.
        assert_eq!(top_at(400.0, 100, 25, -40.0), 0);
        assert_eq!(top_at(400.0, 100, 25, 900.0), 99);
        assert_eq!(top_at(400.0, 10, 40, 100.0), 0);
        let (offset, _) = thumb(400.0, 5_467_582, 40, 2_000_000);
        assert!(top_at(400.0, 5_467_582, 40, offset).abs_diff(2_000_000) < 14_000);
    }

    #[test]
    fn json_lines_split_into_keys_values_and_punctuation() {
        use Token::*;
        assert_eq!(
            json_tokens(r#"  "id": 12,"#),
            [
                ("  ", Punct),
                (r#""id""#, Key),
                (": ", Punct),
                ("12", Number),
                (",", Punct)
            ]
        );
        assert_eq!(
            json_tokens(r#""a \"q\" b": "x:y","#),
            [
                (r#""a \"q\" b""#, Key),
                (": ", Punct),
                (r#""x:y""#, String),
                (",", Punct)
            ]
        );
        assert_eq!(
            json_tokens("[true, null, -1.5e3]"),
            [
                ("[", Punct),
                ("true", Literal),
                (", ", Punct),
                ("null", Literal),
                (", ", Punct),
                ("-1.5e3", Number),
                ("]", Punct)
            ]
        );
    }

    #[test]
    fn json_pieces_always_join_back_into_the_line() {
        // A long string split across lines starts mid-string.
        for line in [
            r#"tail of a string","#,
            r#""unclosed"#,
            "",
            "  }",
            "é: ü",
            r#""ends in \"#,
            r#""\é""#,
        ] {
            let joined: std::string::String = json_tokens(line).iter().map(|(s, _)| *s).collect();
            assert_eq!(joined, line);
        }
    }

    #[test]
    fn markup_lines_split_into_tags_attributes_values_and_text() {
        use Token::*;
        assert_eq!(
            markup_tokens(r#"  <user id="1" admin>Ada</user>"#),
            [
                ("  ", Punct),
                ("<user", Tag),
                (" ", Punct),
                ("id", Attr),
                ("=", Punct),
                (r#""1""#, String),
                (" ", Punct),
                ("admin", Attr),
                (">", Tag),
                ("Ada", Text),
                ("</user", Tag),
                (">", Tag),
            ]
        );
        assert_eq!(
            markup_tokens("<meta charset=utf-8/>"),
            [
                ("<meta", Tag),
                (" ", Punct),
                ("charset", Attr),
                ("=", Punct),
                ("utf-8", String),
                ("/>", Tag),
            ]
        );
        assert_eq!(
            markup_tokens("    <!-- a > b -->"),
            [("    ", Punct), ("<!-- a > b -->", Comment)]
        );
        assert_eq!(
            markup_tokens("<?xml version=\"1.0\"?>"),
            [("<?xml version=\"1.0\"?>", Comment)]
        );
        assert_eq!(
            markup_tokens("  plain words"),
            [("  ", Punct), ("plain words", Text)]
        );
    }

    #[test]
    fn markup_pieces_always_join_back_into_the_line() {
        for line in [
            "<a title='x > y' href=\"/q\">é</a>",
            "<a b=\"unclosed",
            "<",
            "</",
            "<a =x>",
            "text < more",
            "",
            "<br><br>",
        ] {
            let joined: std::string::String = markup_tokens(line).iter().map(|(s, _)| *s).collect();
            assert_eq!(joined, line);
        }
    }

    #[test]
    fn only_a_curl_command_counts_as_one() {
        for yes in [
            "curl https://h/",
            "  curl -X POST 'https://h/' \\\n  -H 'a: b'\n",
            "curl.exe https://h/",
            "/usr/bin/curl -s https://h/",
            "CURL.EXE https://h/",
        ] {
            assert!(is_curl(yes), "{yes:?}");
        }
        for no in [
            "",
            "curl",
            "curl   ",
            "https://h/curl https://x",
            "curlhost.example/path",
            "echo curl https://h/",
            "wget https://h/",
        ] {
            assert!(!is_curl(no), "{no:?}");
        }
    }
}
