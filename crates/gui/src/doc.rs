//! One open request: its file, its form, its send and its response. Each tab
//! in the window holds one [`Doc`].

use crate::motion::Motion;
use iced::task::Handle;
use iced::widget::text_editor;
use reqlite_gui::chain;
use reqlite_gui::draft::{AuthDraft, BodyKind, Draft};
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
    pub variables: text_editor::Content,
    pub grpc_method: String,
    pub grpc_proto: String,
    pub body_kind: BodyKind,
    /// The path a File body sends.
    pub body_file: String,
    pub auth: AuthDraft,
    pub tests: text_editor::Content,
    pub section: Section,
    /// Derived from the fields and `saved`, refreshed after each change.
    pub dirty: bool,
    pub counts: Counts,
    pub send: Send,
    pub viewer: Option<Viewer>,
    /// The last response's headers, decoded for display.
    pub response_headers: Vec<(String, String)>,
    pub response_tab: ResponseTab,
    /// The WebSocket or SSE connection of the last Connect, with its log.
    pub live: Option<Live>,
    /// The last response's assertion and capture results.
    pub checks: Option<reqlite_engine::check::Checked>,
    /// Why the file could not be opened. Shown for as long as the tab lives.
    pub open_error: Option<String>,
    /// The last save found the file changed on disk. The next Save overwrites.
    pub conflict: bool,
    /// The file name for the tab and the title.
    pub label: String,
    pub motion: Motion,
}

pub struct Live {
    pub log: reqlite_gui::live::Log,
    /// Takes WebSocket messages to send. `None` for SSE, and once closed.
    pub tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    pub websocket: bool,
    /// The handshake finished and the connection has not closed.
    pub open: bool,
    /// The message being typed.
    pub message: String,
    /// When Connect was pressed: log times count from here.
    pub started: std::time::Instant,
}

impl Live {
    pub fn info(&mut self, at: Duration, text: String) {
        self.log.push(reqlite_gui::live::Entry {
            at,
            dir: reqlite_gui::live::Dir::Info,
            name: None,
            text,
        });
    }
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
    Auth,
    Tests,
}

/// The views of a response.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResponseTab {
    Body,
    Headers,
    Tests,
}

/// What each section holds, shown on its tab.
#[derive(Default)]
pub struct Counts {
    pub query: usize,
    pub headers: usize,
    pub body: bool,
    pub auth: bool,
    pub tests: usize,
}

pub struct Viewer {
    pub doc: Arc<Document>,
    pub top: usize,
}

/// A request file as read from disk.
pub enum Opened {
    Missing,
    Loaded(Box<reqlite_format::Request>, String),
    Failed(String),
}

/// Reads a request file. Called before the window opens and off the UI thread.
pub fn open(path: &Path) -> Opened {
    match std::fs::read_to_string(path) {
        Ok(text) => match reqlite_format::parse(&text) {
            Ok(req) => {
                let canonical = reqlite_format::to_string(&req).unwrap_or(text);
                Opened::Loaded(Box::new(req), canonical)
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
            variables: text_editor::Content::new(),
            grpc_method: String::new(),
            grpc_proto: String::new(),
            body_kind: BodyKind::default(),
            body_file: String::new(),
            auth: AuthDraft::default(),
            tests: text_editor::Content::new(),
            section: Section::Query,
            dirty: false,
            counts: Counts::default(),
            send: Send::Idle,
            viewer: None,
            response_headers: Vec::new(),
            response_tab: ResponseTab::Body,
            checks: None,
            live: None,
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
        doc.show_first_filled();
        doc
    }

    /// Shows the first request section that has something in it.
    pub fn show_first_filled(&mut self) {
        let c = &self.counts;
        self.section = if c.query > 0 {
            Section::Query
        } else if c.headers > 0 {
            Section::Headers
        } else if c.body {
            Section::Body
        } else if c.auth {
            Section::Auth
        } else {
            Section::Query
        };
    }

    /// Replaces the form with `req`, read from disk as `canonical`.
    pub fn load(&mut self, req: &reqlite_format::Request, canonical: &str) {
        self.saved = Some(canonical.to_string());
        self.fill(&Draft::from_request(req));
    }

    /// Replaces the form with `d`, as typed. The file and `saved` stay.
    pub fn fill(&mut self, d: &Draft) {
        self.name.clone_from(&d.name);
        self.method.clone_from(&d.method);
        self.url.clone_from(&d.url);
        self.headers = text_editor::Content::with_text(&d.headers);
        self.query = text_editor::Content::with_text(&d.query);
        self.body = text_editor::Content::with_text(&d.body);
        self.variables = text_editor::Content::with_text(&d.variables);
        self.grpc_method.clone_from(&d.grpc_method);
        self.grpc_proto.clone_from(&d.grpc_proto);
        self.body_kind = d.body_kind;
        self.body_file.clone_from(&d.body_file);
        self.auth = d.auth.clone();
        self.tests = text_editor::Content::with_text(&d.tests);
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
            body_kind: self.body_kind,
            body_file: self.body_file.clone(),
            variables: self.variables.text(),
            grpc_method: self.grpc_method.clone(),
            grpc_proto: self.grpc_proto.clone(),
            auth: self.auth.clone(),
            tests: self.tests.text(),
        }
    }

    pub fn refresh(&mut self) {
        let d = self.draft();
        self.dirty = d.differs_from(self.saved.as_deref());
        self.counts = Counts {
            query: entries(&d.query),
            headers: entries(&d.headers),
            body: match d.body_kind {
                BodyKind::File => !d.body_file.trim().is_empty(),
                BodyKind::Grpc => true,
                _ => !d.body.is_empty(),
            },
            auth: d.auth.kind != reqlite_gui::draft::AuthKind::None,
            tests: d.tests.lines().filter(|l| !l.trim().is_empty()).count(),
        };
    }

    /// True when closing the tab would lose typing.
    pub fn unsaved(&self) -> bool {
        self.dirty && self.file.is_some()
    }

    /// True when Send opens a connection: a WebSocket URL, or a request for
    /// an event stream.
    pub fn opens_stream(&self) -> bool {
        let url = self.url.trim_start().to_ascii_lowercase();
        url.starts_with("ws://")
            || url.starts_with("wss://")
            || self.headers.text().lines().any(|l| {
                l.split_once(':').is_some_and(|(k, v)| {
                    k.trim().eq_ignore_ascii_case("accept") && v.contains("text/event-stream")
                })
            })
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
