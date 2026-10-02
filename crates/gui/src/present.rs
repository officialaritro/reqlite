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
}
