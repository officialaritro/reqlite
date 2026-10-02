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
}
