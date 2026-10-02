//! `reqlite-gui FILE [--env ENV]`: open, edit, save and send one request file.

use clap::Parser;
use iced::animation::{Animation, Easing};
use iced::keyboard::{self, Key, key::Named};
use iced::task::Handle;
use iced::widget::{self, text_editor};
use iced::{Subscription, Task, event, keyboard::Modifiers, time, window};
use reqlite_gui::chain;
use reqlite_gui::draft::Draft;
use reqlite_gui::present::{entries, glass_supported};
use reqlite_viewer::Document;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod scrollbar;
mod style;
mod view;

const LINE_HEIGHT: f32 = 18.0;
const PAGE: i64 = 40;
const URL: widget::Id = widget::Id::new("url");
/// The window draws its own title row where the OS lets content go under the
/// title bar.
const TITLE_ROW: bool = cfg!(target_os = "macos");
const METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

/// A desktop client for one Reqlite request file.
#[derive(Parser, Clone)]
#[command(version)]
struct Args {
    /// The request file. It is created on the first save if it does not exist.
    file: Option<PathBuf>,
    /// Environment file that fills `{{var}}` placeholders.
    #[arg(long, short)]
    env: Option<PathBuf>,
}

struct App {
    /// Where Save writes. `None` when no file was given, or when the file failed
    /// to parse: a window never writes over a file it could not read.
    file: Option<PathBuf>,
    /// The file's canonical text as last read or saved.
    saved: Option<String>,
    name: String,
    method: String,
    /// The picker's choices: the common methods, plus the file's own if it
    /// uses another one.
    methods: Vec<String>,
    url: String,
    headers: text_editor::Content,
    query: text_editor::Content,
    body: text_editor::Content,
    tab: Tab,
    /// Derived from the fields and `saved`, refreshed after each change.
    dirty: bool,
    counts: Counts,
    env: Option<PathBuf>,
    client: Option<reqwest::Client>,
    /// Opened by the first send. Opening it at startup would deliver a message
    /// right after the first frame, and that second frame costs a third
    /// drawable (issue #5).
    history: Option<reqlite_store::Store>,
    history_tried: bool,
    send: Send,
    viewer: Option<Viewer>,
    notice: Option<String>,
    /// Why the file could not be opened. Shown for as long as the window lives.
    open_error: Option<String>,
    /// The file name for the title, kept even when saving is off.
    label: String,
    started: Instant,
    exit_after_first_frame: bool,
    motion: Motion,
}

/// The few animations, and the clock they read. Frames are drawn only while
/// one of them runs, so an idle window costs no CPU.
struct Motion {
    now: Instant,
    /// The response fading in.
    reveal: Animation<bool>,
    /// Send turning into Cancel and back.
    running: Animation<bool>,
    /// When the current send started, for the pulse.
    since: Instant,
    /// `REQLITE_REDUCE_MOTION=1`: every change shows at once.
    reduced: bool,
}

/// The "sending" pulse redraws at most this often.
const PULSE: Duration = Duration::from_millis(33);

impl Motion {
    fn new(reduced: bool) -> Motion {
        let now = Instant::now();
        Motion {
            now,
            reveal: Animation::new(true),
            running: Animation::new(false)
                .duration(if reduced {
                    Duration::ZERO
                } else {
                    Duration::from_millis(200)
                })
                .easing(Easing::EaseOut),
            since: now,
            reduced,
        }
    }

    /// Fades a new response in from nothing: 0.5 s, rising 4 px (Zeron's fade-in).
    fn reveal(&mut self, now: Instant) {
        let fade = Animation::new(false).easing(Easing::EaseOutExpo);
        self.reveal = fade
            .duration(if self.reduced {
                Duration::ZERO
            } else {
                Duration::from_millis(500)
            })
            .go(true, now);
    }

    fn animating(&self, at: Instant) -> bool {
        self.reveal.is_animating(at) || self.running.is_animating(at)
    }
}

enum Send {
    Idle,
    Running(Handle),
    Cancelled,
    Finished(Result<Summary, String>),
}

#[derive(Clone, Copy)]
struct Summary {
    status: u16,
    elapsed: Duration,
    bytes: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    Query,
    Headers,
    Body,
}

/// What each tab holds, shown on the tab.
#[derive(Default)]
struct Counts {
    query: usize,
    headers: usize,
    body: bool,
}

struct Viewer {
    doc: Arc<Document>,
    top: usize,
}

#[derive(Clone)]
enum Msg {
    Method(String),
    Url(String),
    Headers(text_editor::Action),
    Query(text_editor::Action),
    Body(text_editor::Action),
    Send,
    Cancel,
    Sent(Box<Finished>),
    /// A frame or pulse tick, while something moves.
    Tick(Instant),
    Save,
    Saved(Result<String, String>),
    Scroll(i64),
    ScrollTo(f64),
    Tab(Tab),
    FocusUrl,
    FirstFrame,
}

/// The request file as read before the window opens.
enum Opened {
    Missing,
    Loaded(reqlite_format::Request, String),
    Failed(String),
}

#[derive(Clone)]
struct Loaded {
    doc: Arc<Document>,
    summary: Summary,
}

#[derive(Clone)]
struct Finished {
    result: Result<Loaded, String>,
    /// Set when this send opened the history database.
    opened: Option<reqlite_store::Store>,
    warning: Option<String>,
}

/// Reads the request file before the window opens, so nothing the user types
/// can be replaced by a late load.
fn open(file: Option<&Path>) -> Opened {
    let Some(path) = file else {
        return Opened::Missing;
    };
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

fn boot(args: &Args, opened: &Opened, started: Instant) -> (App, Task<Msg>) {
    let name = args
        .file
        .as_deref()
        .and_then(Path::file_stem)
        .map_or("Untitled".to_string(), |s| s.to_string_lossy().into_owned());
    let mut app = App {
        file: args.file.clone(),
        saved: None,
        name,
        method: "GET".into(),
        methods: METHODS.map(String::from).to_vec(),
        url: String::new(),
        headers: text_editor::Content::new(),
        query: text_editor::Content::new(),
        body: text_editor::Content::new(),
        tab: Tab::Query,
        dirty: false,
        counts: Counts::default(),
        env: args.env.clone(),
        client: None,
        history: None,
        history_tried: false,
        send: Send::Idle,
        viewer: None,
        notice: None,
        open_error: None,
        label: args
            .file
            .as_deref()
            .and_then(Path::file_name)
            .map_or("Untitled".to_string(), |f| f.to_string_lossy().into_owned()),
        started,
        exit_after_first_frame: std::env::var_os("REQLITE_GUI_EXIT_ON_FIRST_FRAME").is_some(),
        motion: Motion::new(std::env::var_os("REQLITE_REDUCE_MOTION").is_some_and(|v| v != "0")),
    };
    match opened {
        Opened::Missing => {}
        Opened::Loaded(req, canonical) => {
            load(&mut app, &Draft::from_request(req));
            app.saved = Some(canonical.clone());
            if !app.methods.contains(&app.method) {
                app.methods.push(app.method.clone());
            }
        }
        Opened::Failed(e) => {
            app.file = None;
            app.open_error = Some(format!(
                "{}\nThis window will not save over it.",
                e.trim_end()
            ));
        }
    }
    match reqlite_engine::client() {
        Ok(client) => app.client = Some(client),
        Err(e) => app.notice = Some(format!("cannot start the HTTP client: {}", chain(&e))),
    }
    refresh_dirty(&mut app);
    // Open on the first tab that has something in it.
    if app.counts.query == 0 {
        if app.counts.headers > 0 {
            app.tab = Tab::Headers;
        } else if app.counts.body {
            app.tab = Tab::Body;
        }
    }
    (app, Task::none())
}

/// Opens the history database. Called off the UI thread.
fn open_history() -> Result<Option<reqlite_store::Store>, String> {
    match reqlite_store::default_path() {
        None => Ok(None),
        Some(path) => reqlite_store::open(&path)
            .map(|opened| Some(opened.store))
            .map_err(|e| format!("history is off: {}", chain(&e))),
    }
}

async fn blocking<T: std::marker::Send + 'static>(
    f: impl FnOnce() -> T + std::marker::Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| format!("background work failed: {e}"))
}

fn update(app: &mut App, msg: Msg) -> Task<Msg> {
    // A tick only moves the clock: no form refresh at 30 or 120 Hz.
    if let Msg::Tick(at) = msg {
        app.motion.now = at;
        return Task::none();
    }
    let now = Instant::now();
    app.motion.now = now;
    match msg {
        Msg::Tick(_) => {}
        Msg::Method(m) => app.method = m,
        Msg::Url(u) => app.url = u,
        Msg::Headers(a) => app.headers.perform(a),
        Msg::Query(a) => app.query.perform(a),
        Msg::Body(a) => app.body.perform(a),
        Msg::Send => return start_send(app),
        Msg::Cancel => {
            if let Send::Running(handle) = &app.send {
                handle.abort();
                app.send = Send::Cancelled;
                app.motion.running.go_mut(false, now);
            }
        }
        Msg::Sent(done) => {
            let Finished {
                result,
                opened,
                warning,
            } = *done;
            if opened.is_some() {
                app.history = opened;
            }
            app.notice = warning;
            app.motion.running.go_mut(false, now);
            match result {
                Ok(loaded) => {
                    app.motion.reveal(now);
                    app.viewer = Some(Viewer {
                        doc: loaded.doc,
                        top: 0,
                    });
                    app.send = Send::Finished(Ok(loaded.summary));
                }
                Err(e) => app.send = Send::Finished(Err(e)),
            }
        }
        Msg::Save => return save(app),
        Msg::Saved(Ok(text)) => {
            app.saved = Some(text);
            app.notice = None;
        }
        Msg::Saved(Err(e)) => app.notice = Some(e),
        Msg::Scroll(lines) => scroll(app, |top| top + lines),
        Msg::ScrollTo(v) => scroll(app, |_| v.round() as i64),
        Msg::Tab(t) => app.tab = t,
        Msg::FocusUrl => {
            return Task::batch([
                widget::operation::focus(URL),
                widget::operation::select_all(URL),
            ]);
        }
        Msg::FirstFrame => {
            println!(
                "first-frame {:.1}",
                app.started.elapsed().as_secs_f64() * 1000.0
            );
            return iced::exit();
        }
    }
    refresh_dirty(app);
    Task::none()
}

fn draft(app: &App) -> Draft {
    Draft {
        name: app.name.clone(),
        method: app.method.clone(),
        url: app.url.clone(),
        headers: app.headers.text(),
        query: app.query.text(),
        body: app.body.text(),
    }
}

fn load(app: &mut App, d: &Draft) {
    app.name = d.name.clone();
    app.method = d.method.clone();
    app.url = d.url.clone();
    app.headers = text_editor::Content::with_text(&d.headers);
    app.query = text_editor::Content::with_text(&d.query);
    app.body = text_editor::Content::with_text(&d.body);
}

fn refresh_dirty(app: &mut App) {
    let d = draft(app);
    app.dirty = d.differs_from(app.saved.as_deref());
    app.counts = Counts {
        query: entries(&d.query),
        headers: entries(&d.headers),
        body: !d.body.is_empty(),
    };
}

fn start_send(app: &mut App) -> Task<Msg> {
    if matches!(app.send, Send::Running(_)) {
        return Task::none();
    }
    let Some(client) = app.client.clone() else {
        return Task::none();
    };
    let req = match draft(app).to_request() {
        Ok(r) => r,
        Err(e) => {
            app.send = Send::Finished(Err(e));
            return Task::none();
        }
    };
    let env = match &app.env {
        None => Ok(reqlite_format::Environment::default()),
        Some(path) => reqlite_format::load_env(path).map_err(|e| chain(&e)),
    };
    let resolved =
        match env.and_then(|env| reqlite_engine::resolve(&req, &env).map_err(|e| e.to_string())) {
            Ok(r) => r,
            Err(e) => {
                app.send = Send::Finished(Err(e));
                return Task::none();
            }
        };
    let history = app.history.clone();
    let open = !app.history_tried;
    app.history_tried = true;
    let file = app.file.as_ref().map(|p| p.display().to_string());
    let env = app.env.as_ref().map(|p| p.display().to_string());
    let (task, handle) = Task::perform(run_send(client, resolved, history, open, file, env), |f| {
        Msg::Sent(Box::new(f))
    })
    .abortable();
    app.send = Send::Running(handle);
    let now = Instant::now();
    app.motion.since = now;
    app.motion.running.go_mut(true, now);
    task
}

async fn run_send(
    client: reqwest::Client,
    req: reqlite_engine::Resolved,
    history: Option<reqlite_store::Store>,
    open: bool,
    file: Option<String>,
    env: Option<String>,
) -> Finished {
    let mut warning = None;
    let mut opened = None;
    let store = match history {
        Some(store) => Some(store),
        None if open => match blocking(open_history).await.and_then(|r| r) {
            Ok(store) => {
                opened.clone_from(&store);
                store
            }
            Err(e) => {
                warning = Some(e);
                None
            }
        },
        None => None,
    };
    let result = reqlite_engine::send(&client, &req).await;
    if let Some(store) = store {
        let recorded = match reqlite_store::Entry::from_send(file, env, &req, result.as_ref()) {
            Ok(entry) => store.record(entry).await.map(drop).map_err(|e| chain(&e)),
            Err(e) => Err(e.to_string()),
        };
        if let Err(e) = recorded {
            warning = Some(format!("this send was not saved to history: {e}"));
        }
    }
    Finished {
        result: show(result).await,
        opened,
        warning,
    }
}

async fn show(
    result: Result<reqlite_engine::Response, reqlite_engine::SendError>,
) -> Result<Loaded, String> {
    let resp = result.map_err(|e| chain(&e))?;
    let summary = Summary {
        status: resp.status,
        elapsed: resp.elapsed,
        bytes: resp.body.len(),
    };
    let doc = blocking(move || {
        let reader = resp.body.reader()?;
        Document::build(reader)
    })
    .await?
    .map_err(|e| format!("cannot show the response: {e}"))?;
    Ok(Loaded {
        doc: Arc::new(doc),
        summary,
    })
}

fn save(app: &mut App) -> Task<Msg> {
    let Some(path) = app.file.clone() else {
        if app.open_error.is_none() {
            app.notice = Some("No file to save to. Start reqlite-gui with a file path.".into());
        }
        return Task::none();
    };
    let req = match draft(app).to_request() {
        Ok(r) => r,
        Err(e) => {
            app.notice = Some(format!("not saved: {e}"));
            return Task::none();
        }
    };
    let work = blocking(move || {
        reqlite_format::save(&path, &req).map_err(|e| chain(&e))?;
        reqlite_format::to_string(&req).map_err(|e| e.to_string())
    });
    Task::perform(async move { work.await.and_then(|r| r) }, Msg::Saved)
}

fn scroll(app: &mut App, to: impl Fn(i64) -> i64) {
    if let Some(v) = &mut app.viewer {
        let max = v.doc.line_count().saturating_sub(1) as i64;
        v.top = to(v.top as i64).clamp(0, max) as usize;
    }
}

fn title(app: &App) -> String {
    let state = if app.open_error.is_some() {
        " (cannot save)"
    } else if app.dirty && app.file.is_some() {
        "*"
    } else {
        ""
    };
    format!("{}{state} - Reqlite", app.label)
}

impl App {
    /// The response to show: hidden after a failed send, so an old body never
    /// sits under a new error.
    fn shown(&self) -> Option<&Viewer> {
        match self.send {
            Send::Finished(Err(_)) => None,
            _ => self.viewer.as_ref(),
        }
    }
}

/// The app's Cmd (Ctrl) shortcuts. They work wherever the focus is.
fn shortcut(key: &Key, modifiers: Modifiers) -> Option<Msg> {
    if !modifiers.command() {
        return None;
    }
    match key.as_ref() {
        Key::Character("s") => Some(Msg::Save),
        Key::Named(Named::Enter) => Some(Msg::Send),
        Key::Character("l") => Some(Msg::FocusUrl),
        Key::Character("1") => Some(Msg::Tab(Tab::Query)),
        Key::Character("2") => Some(Msg::Tab(Tab::Headers)),
        Key::Character("3") => Some(Msg::Tab(Tab::Body)),
        _ => None,
    }
}

/// The editors' default bindings, minus the app's own shortcuts. Without this,
/// Cmd+S also types an "s" and Cmd+Enter a new line into the focused editor.
fn editor_keys(press: text_editor::KeyPress) -> Option<text_editor::Binding<Msg>> {
    if shortcut(&press.key, press.modifiers).is_some() {
        None
    } else {
        text_editor::Binding::from_key_press(press)
    }
}

fn keys(event: iced::Event, status: event::Status, _: window::Id) -> Option<Msg> {
    let iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event else {
        return None;
    };
    if let Some(msg) = shortcut(&key, modifiers) {
        return Some(msg);
    }
    match key.as_ref() {
        Key::Named(Named::Escape) => Some(Msg::Cancel),
        _ if status == event::Status::Captured => None,
        Key::Named(Named::PageDown) => Some(Msg::Scroll(PAGE)),
        Key::Named(Named::PageUp) => Some(Msg::Scroll(-PAGE)),
        Key::Named(Named::Home) => Some(Msg::ScrollTo(0.0)),
        Key::Named(Named::End) => Some(Msg::ScrollTo(f64::MAX)),
        _ => None,
    }
}

fn subscription(app: &App) -> Subscription<Msg> {
    let mut subs = vec![event::listen_with(keys)];
    // A frame subscription makes iced redraw back to back, which costs a third
    // drawable (issue #1). So it runs only for the cold start check and while
    // an animation moves.
    if app.exit_after_first_frame {
        subs.push(window::frames().map(|_| Msg::FirstFrame));
    } else if app.motion.animating(Instant::now()) {
        subs.push(window::frames().map(Msg::Tick));
    } else if matches!(app.send, Send::Running(_)) && !app.motion.reduced {
        subs.push(time::every(PULSE).map(Msg::Tick));
    }
    Subscription::batch(subs)
}

/// Glass where the OS blurs a transparent window. `REQLITE_GLASS=0` turns it off.
fn glass() -> bool {
    std::env::var_os("REQLITE_GLASS").is_none_or(|v| v != "0")
        && glass_supported(
            std::env::consts::OS,
            std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref(),
            std::env::var_os("WAYLAND_DISPLAY").is_some(),
        )
}

fn main() -> iced::Result {
    let started = Instant::now();
    let args = Args::parse();
    let opened = open(args.file.as_deref());
    let glass = glass();
    let mut app = iced::application(move || boot(&args, &opened, started), update, view::view)
        .title(title)
        .subscription(subscription)
        .theme(style::theme())
        .style(move |_, _| style::window(glass))
        .default_font(style::SANS)
        .window(window::Settings {
            size: iced::Size::new(1000.0, 800.0),
            min_size: Some(iced::Size::new(560.0, 420.0)),
            transparent: glass,
            blur: glass,
            #[cfg(target_os = "macos")]
            platform_specific: window::settings::PlatformSpecific {
                title_hidden: true,
                titlebar_transparent: true,
                fullsize_content_view: true,
            },
            ..window::Settings::default()
        });
    for face in style::FONTS {
        app = app.font(face);
    }
    app.run()
}

#[cfg(test)]
mod tests;
