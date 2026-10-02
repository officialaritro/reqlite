//! One open request: its file, its form, its send and its response. Each tab
//! in the window holds one [`Doc`].

use crate::motion::Motion;
use iced::task::Handle;
use iced::widget::text_editor;
use reqlite_gui::chain;
use reqlite_gui::draft::Draft;
use reqlite_gui::present::entries;
use reqlite_viewer::Document;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub const METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

/// Stays the same while the tab lives, so a send or save that finishes after
/// the tabs moved still finds its tab.
pub type DocId = u64;

pub struct Doc {
    pub id: DocId,
    /// Where Save writes. `None` for a request with no file, or when the file
    /// failed to parse: a tab never writes over a file it could not read.
    pub file: Option<PathBuf>,
    /// The file's canonical text as last read or saved. `None` when the file
    /// does not exist yet.
    pub saved: Option<String>,
    pub name: String,
    pub method: String,
    /// The picker's choices: the common methods, plus the file's own if it
    /// uses another one.
    pub methods: Vec<String>,
    pub url: String,
    pub headers: text_editor::Content,
    pub query: text_editor::Content,
    pub body: text_editor::Content,
    pub section: Section,
    /// Derived from the fields and `saved`, refreshed after each change.
    pub dirty: bool,
    pub counts: Counts,
    pub send: Send,
    pub viewer: Option<Viewer>,
    /// Why the file could not be opened. Shown for as long as the tab lives.
    pub open_error: Option<String>,
    /// The last save found the file changed on disk. The next Save overwrites.
    pub conflict: bool,
    /// The file name for the tab and the title.
    pub label: String,
    pub motion: Motion,
}

pub enum Send {
    Idle,
    Running(Handle),
    Cancelled,
    Finished(Result<Summary, String>),
}

#[derive(Clone, Copy)]
pub struct Summary {
    pub status: u16,
    pub elapsed: Duration,
    pub bytes: u64,
}

/// The parts of a request, each on its own tab in the request pane.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    Query,
    Headers,
    Body,
}

/// What each section holds, shown on its tab.
#[derive(Default)]
pub struct Counts {
    pub query: usize,
    pub headers: usize,
    pub body: bool,
}

pub struct Viewer {
    pub doc: Arc<Document>,
    pub top: usize,
}

/// A request file as read from disk.
pub enum Opened {
    Missing,
    Loaded(reqlite_format::Request, String),
    Failed(String),
}

/// Reads a request file. Called before the window opens and off the UI thread.
pub fn open(path: &Path) -> Opened {
    match std::fs::read_to_string(path) {
        Ok(text) => match reqlite_format::parse(&text) {
            Ok(req) => {
                let canonical = reqlite_format::to_string(&req).unwrap_or(text);
                Opened::Loaded(req, canonical)
            }
            Err(e) => Opened::Failed(format!("cannot open {}: {}", path.display(), chain(&e))),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Opened::Missing,
        Err(e) => Opened::Failed(format!("cannot read {}: {e}", path.display())),
    }
}

impl Doc {
    /// A tab for `file` as `opened` found it. `None` is a request with no file.
    pub fn new(id: DocId, file: Option<PathBuf>, opened: &Opened, reduced_motion: bool) -> Doc {
        let stem = |p: &Path| p.file_stem().map(|s| s.to_string_lossy().into_owned());
        let label = |p: &Path| p.file_name().map(|s| s.to_string_lossy().into_owned());
        let mut doc = Doc {
            id,
            name: file.as_deref().and_then(stem).unwrap_or("Untitled".into()),
            label: file.as_deref().and_then(label).unwrap_or("Untitled".into()),
            file,
            saved: None,
            method: "GET".into(),
            methods: METHODS.map(String::from).to_vec(),
            url: String::new(),
            headers: text_editor::Content::new(),
            query: text_editor::Content::new(),
            body: text_editor::Content::new(),
            section: Section::Query,
            dirty: false,
            counts: Counts::default(),
            send: Send::Idle,
            viewer: None,
            open_error: None,
            conflict: false,
            motion: Motion::new(reduced_motion),
        };
        match opened {
            Opened::Missing => {}
            Opened::Loaded(req, canonical) => doc.load(req, canonical),
            Opened::Failed(e) => {
                doc.file = None;
                doc.open_error = Some(format!("{}\nThis tab will not save over it.", e.trim_end()));
            }
        }
        doc.refresh();
        // Open on the first section that has something in it.
        if doc.counts.query == 0 {
            if doc.counts.headers > 0 {
                doc.section = Section::Headers;
            } else if doc.counts.body {
                doc.section = Section::Body;
            }
        }
        doc
    }

    /// Replaces the form with `req`, read from disk as `canonical`.
    pub fn load(&mut self, req: &reqlite_format::Request, canonical: &str) {
        let d = Draft::from_request(req);
        self.name = d.name;
        self.method = d.method;
        self.url = d.url;
        self.headers = text_editor::Content::with_text(&d.headers);
        self.query = text_editor::Content::with_text(&d.query);
        self.body = text_editor::Content::with_text(&d.body);
        self.saved = Some(canonical.to_string());
        if !self.methods.contains(&self.method) {
            self.methods.push(self.method.clone());
        }
        self.refresh();
    }

    pub fn draft(&self) -> Draft {
        Draft {
            name: self.name.clone(),
            method: self.method.clone(),
            url: self.url.clone(),
            headers: self.headers.text(),
            query: self.query.text(),
            body: self.body.text(),
        }
    }

    pub fn refresh(&mut self) {
        let d = self.draft();
        self.dirty = d.differs_from(self.saved.as_deref());
        self.counts = Counts {
            query: entries(&d.query),
            headers: entries(&d.headers),
            body: !d.body.is_empty(),
        };
    }

    /// True when closing the tab would lose typing.
    pub fn unsaved(&self) -> bool {
        self.dirty && self.file.is_some()
    }

    pub fn running(&self) -> bool {
        matches!(self.send, Send::Running(_))
    }

    pub fn scroll(&mut self, to: impl Fn(i64) -> i64) {
        if let Some(v) = &mut self.viewer {
            let max = v.doc.line_count().saturating_sub(1) as i64;
            v.top = to(v.top as i64).clamp(0, max) as usize;
        }
    }

    /// The response to show: hidden after a failed send, so an old body never
    /// sits under a new error.
    pub fn shown(&self) -> Option<&Viewer> {
        match self.send {
            Send::Finished(Err(_)) => None,
            _ => self.viewer.as_ref(),
        }
    }

    pub fn title(&self) -> String {
        let state = if self.open_error.is_some() {
            " (cannot save)"
        } else if self.unsaved() {
            "*"
        } else {
            ""
        };
        format!("{}{state}", self.label)
    }
}

/// What a save found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Written {
    /// The file was written. It holds the canonical text.
    Saved(String),
    /// The file changed on disk since the tab read it. Nothing was written.
    Changed,
}

/// Saves `req` to `path`, unless the file on disk is no longer `expected`
/// (the canonical text the tab last read or wrote, `None` for a new file).
/// `force` writes anyway. Called off the UI thread.
pub fn write(
    path: &Path,
    req: &reqlite_format::Request,
    expected: Option<&str>,
    force: bool,
) -> Result<Written, String> {
    if !force {
        let on_disk = match open(path) {
            Opened::Missing => None,
            Opened::Loaded(_, canonical) => Some(canonical),
            // A file that cannot be read now differs from what the tab holds.
            Opened::Failed(_) => return Ok(Written::Changed),
        };
        if on_disk.as_deref() != expected {
            return Ok(Written::Changed);
        }
    }
    reqlite_format::save(path, req).map_err(|e| chain(&e))?;
    reqlite_format::to_string(req)
        .map(Written::Saved)
        .map_err(|e| e.to_string())
}

impl Doc {
    /// After a change on disk: a tab with no unsaved changes shows the file
    /// as it is now. A tab with changes keeps them; Save then finds the change.
    /// Returns a notice when the file is gone or cannot be read.
    pub fn reload(&mut self) -> Option<String> {
        if self.dirty || self.open_error.is_some() {
            return None;
        }
        let path = self.file.clone()?;
        match open(&path) {
            Opened::Loaded(req, canonical) => {
                if self.saved.as_deref() != Some(canonical.as_str()) {
                    self.load(&req, &canonical);
                }
                None
            }
            // Never saved yet, so nothing on disk to follow.
            Opened::Missing if self.saved.is_none() => None,
            Opened::Missing => Some(format!(
                "{} was deleted on disk. Save writes it again.",
                self.label
            )),
            Opened::Failed(e) => Some(format!("{e}. The tab keeps the last version it read.")),
        }
    }
}
