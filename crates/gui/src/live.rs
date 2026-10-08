//! The message log of a WebSocket or SSE connection. It keeps the newest
//! messages within fixed limits, so a session that runs for hours uses the
//! same memory as one that runs for a minute.

use std::collections::VecDeque;
use std::time::Duration;

/// The most messages kept. Older ones are dropped and counted.
pub const MAX_ENTRIES: usize = 500;
/// The most text kept, over all messages.
pub const MAX_BYTES: usize = 2 << 20;
/// A longer message keeps its start and says how much was cut.
pub const MAX_ENTRY: usize = 16 << 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// From the server.
    In,
    /// From this side.
    Out,
    /// About the connection: open, closed.
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub at: Duration,
    pub dir: Dir,
    /// The SSE event name, when it has one.
    pub name: Option<String>,
    pub text: String,
}

#[derive(Debug, Default)]
pub struct Log {
    entries: VecDeque<Entry>,
    bytes: usize,
    dropped: u64,
}

impl Log {
    pub fn push(&mut self, mut entry: Entry) {
        if entry.text.len() > MAX_ENTRY {
            let mut cut = MAX_ENTRY;
            while !entry.text.is_char_boundary(cut) {
                cut -= 1;
            }
            let more = entry.text.len() - cut;
            entry.text.truncate(cut);
            entry.text.push_str(&format!("… ({more} more bytes)"));
        }
        self.bytes += entry.text.len();
        self.entries.push_back(entry);
        while self.entries.len() > MAX_ENTRIES || self.bytes > MAX_BYTES {
            let Some(old) = self.entries.pop_front() else {
                break;
            };
            self.bytes -= old.text.len();
            self.dropped += 1;
        }
    }

    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Messages dropped to stay within the limits.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(text: String) -> Entry {
        Entry {
            at: Duration::ZERO,
            dir: Dir::In,
            name: None,
            text,
        }
    }

    #[test]
    fn a_long_session_keeps_the_newest_messages_within_the_limits() {
        let mut log = Log::default();
        for n in 0..MAX_ENTRIES + 25 {
            log.push(entry(n.to_string()));
        }
        assert_eq!(log.len(), MAX_ENTRIES);
        assert_eq!(log.dropped(), 25);
        assert_eq!(log.entries().next().unwrap().text, "25");
        assert_eq!(
            log.entries().last().unwrap().text,
            (MAX_ENTRIES + 24).to_string()
        );
    }

    #[test]
    fn big_messages_are_cut_and_the_byte_limit_holds() {
        let mut log = Log::default();
        for _ in 0..1000 {
            log.push(entry("é".repeat(MAX_ENTRY)));
        }
        let first = log.entries().next().unwrap();
        assert!(
            first
                .text
                .ends_with(&format!("… ({} more bytes)", MAX_ENTRY))
        );
        let kept: usize = log.entries().map(|e| e.text.len()).sum();
        assert!(kept <= MAX_BYTES, "{kept}");
        assert_eq!(log.dropped() as usize + log.len(), 1000);
    }
}
