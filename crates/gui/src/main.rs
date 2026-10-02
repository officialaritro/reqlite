//! `reqlite-gui [FILE] [--env ENV]`: open, edit, save and send request files,
//! one per tab.

use clap::Parser;
use doc::{Doc, DocId, Opened, Section, Send, Summary, Viewer};
use iced::keyboard::{self, Key, key::Named};
use iced::widget::{self, text_editor};
use iced::{Subscription, Task, event, keyboard::Modifiers, time, window};
use reqlite_gui::chain;
use reqlite_gui::present::glass_supported;
use reqlite_viewer::Document;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

mod doc;
mod motion;
mod scrollbar;
mod style;
mod view;

const LINE_HEIGHT: f32 = 18.0;
const PAGE: i64 = 40;
const URL: widget::Id = widget::Id::new("url");
/// The window draws its own title row where the OS lets content go under the
/// title bar.
const TITLE_ROW: bool = cfg!(target_os = "macos");

/// A desktop client for Reqlite request files.
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
    /// The open tabs, left to right. It can be empty.
    docs: Vec<Doc>,
    /// The index of the shown tab in `docs`.
    active: usize,
    next_id: DocId,
    /// A tab with unsaved changes the user asked to close. It closes only
    /// after a second answer.
    closing: Option<DocId>,
    env: Option<PathBuf>,
    client: Option<reqwest::Client>,
    /// Opened by the first send. Opening it at startup would deliver a message
    /// right after the first frame, and that second frame costs a third
    /// drawable (issue #5).
    history: Option<reqlite_store::Store>,
    history_tried: bool,
    notice: Option<String>,
    started: Instant,
    exit_after_first_frame: bool,
    reduced_motion: bool,
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
    Sent(DocId, Box<Finished>),
    /// A frame or pulse tick, while something moves.
    Tick(Instant),
    Save,
    Saved(DocId, Result<String, String>),
    Scroll(i64),
    ScrollTo(f64),
    Section(Section),
    FocusUrl,
    Select(usize),
    NextTab,
    /// Close a tab. One with unsaved changes asks first.
    Close(DocId),
    /// The answer to "discard changes?".
    Discard(bool),
    FirstFrame,
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

fn boot(args: &Args, opened: &Opened, started: Instant) -> (App, Task<Msg>) {
    let reduced_motion = std::env::var_os("REQLITE_REDUCE_MOTION").is_some_and(|v| v != "0");
    let mut app = App {
        docs: Vec::new(),
        active: 0,
        next_id: 0,
        closing: None,
        env: args.env.clone(),
        client: None,
        history: None,
        history_tried: false,
        notice: None,
        started,
        exit_after_first_frame: std::env::var_os("REQLITE_GUI_EXIT_ON_FIRST_FRAME").is_some(),
        reduced_motion,
    };
    app.open_tab(args.file.clone(), opened);
    match reqlite_engine::client() {
        Ok(client) => app.client = Some(client),
        Err(e) => app.notice = Some(format!("cannot start the HTTP client: {}", chain(&e))),
    }
    (app, Task::none())
}

impl App {
    fn doc(&self) -> Option<&Doc> {
        self.docs.get(self.active)
    }

    fn doc_mut(&mut self) -> Option<&mut Doc> {
        self.docs.get_mut(self.active)
    }

    /// Opens `file` as `opened` read it, in a new tab, and shows that tab.
    fn open_tab(&mut self, file: Option<PathBuf>, opened: &Opened) -> DocId {
        let id = self.next_id;
        self.next_id += 1;
        self.docs
            .push(Doc::new(id, file, opened, self.reduced_motion));
        self.active = self.docs.len() - 1;
        id
    }

    fn by_id(&mut self, id: DocId) -> Option<&mut Doc> {
        self.docs.iter_mut().find(|d| d.id == id)
    }

    fn close(&mut self, id: DocId) {
        if let Some(i) = self.docs.iter().position(|d| d.id == id) {
            if let Send::Running(handle) = &self.docs[i].send {
                handle.abort();
            }
            self.docs.remove(i);
            if self.active > i || self.active >= self.docs.len() {
                self.active = self.active.saturating_sub(1);
            }
        }
        self.closing = None;
    }
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
        for d in &mut app.docs {
            d.motion.now = at;
        }
        return Task::none();
    }
    let now = Instant::now();
    for d in &mut app.docs {
        d.motion.now = now;
    }
    match msg {
        Msg::Tick(_) => {}
        Msg::Send => return start_send(app),
        Msg::Save => return save(app),
        Msg::Sent(id, done) => {
            let Finished {
                result,
                opened,
                warning,
            } = *done;
            if opened.is_some() {
                app.history = opened;
            }
            app.notice = warning;
            if let Some(doc) = app.by_id(id) {
                doc.motion.running.go_mut(false, now);
                match result {
                    Ok(loaded) => {
                        doc.motion.reveal(now);
                        doc.viewer = Some(Viewer {
                            doc: loaded.doc,
                            top: 0,
                        });
                        doc.send = Send::Finished(Ok(loaded.summary));
                    }
                    Err(e) => doc.send = Send::Finished(Err(e)),
                }
            }
        }
        Msg::Saved(id, result) => match result {
            Ok(text) => {
                if let Some(doc) = app.by_id(id) {
                    doc.saved = Some(text);
                    doc.conflict = false;
                    doc.refresh();
                }
                app.notice = None;
            }
            Err(e) => app.notice = Some(e),
        },
        Msg::FocusUrl => {
            return Task::batch([
                widget::operation::focus(URL),
                widget::operation::select_all(URL),
            ]);
        }
        Msg::Select(i) => {
            if i < app.docs.len() {
                app.active = i;
            }
        }
        Msg::NextTab => {
            if !app.docs.is_empty() {
                app.active = (app.active + 1) % app.docs.len();
            }
        }
        Msg::Close(id) => {
            let unsaved = app.docs.iter().any(|d| d.id == id && d.unsaved());
            if unsaved && app.closing != Some(id) {
                app.closing = Some(id);
                if let Some(i) = app.docs.iter().position(|d| d.id == id) {
                    app.active = i;
                }
            } else {
                app.close(id);
            }
        }
        Msg::Discard(yes) => match app.closing {
            Some(id) if yes => app.close(id),
            _ => app.closing = None,
        },
        Msg::FirstFrame => {
            println!(
                "first-frame {:.1}",
                app.started.elapsed().as_secs_f64() * 1000.0
            );
            return iced::exit();
        }
        msg => {
            if let Some(doc) = app.doc_mut() {
                edit(doc, msg, now);
                doc.refresh();
            }
        }
    }
    Task::none()
}

/// The messages that change the shown tab and nothing else.
fn edit(doc: &mut Doc, msg: Msg, now: Instant) {
    match msg {
        Msg::Method(m) => doc.method = m,
        Msg::Url(u) => doc.url = u,
        Msg::Headers(a) => doc.headers.perform(a),
        Msg::Query(a) => doc.query.perform(a),
        Msg::Body(a) => doc.body.perform(a),
        Msg::Cancel => {
            if let Send::Running(handle) = &doc.send {
                handle.abort();
                doc.send = Send::Cancelled;
                doc.motion.running.go_mut(false, now);
            }
        }
        Msg::Scroll(lines) => doc.scroll(|top| top + lines),
        Msg::ScrollTo(v) => doc.scroll(|_| v.round() as i64),
        Msg::Section(s) => doc.section = s,
        _ => {}
    }
}

fn start_send(app: &mut App) -> Task<Msg> {
    let (Some(client), env_path) = (app.client.clone(), app.env.clone()) else {
        return Task::none();
    };
    let history = app.history.clone();
    let history_tried = app.history_tried;
    let Some(doc) = app.doc_mut() else {
        return Task::none();
    };
    if doc.running() {
        return Task::none();
    }
    let req = match doc.draft().to_request() {
        Ok(r) => r,
        Err(e) => {
            doc.send = Send::Finished(Err(e));
            return Task::none();
        }
    };
    let env = match &env_path {
        None => Ok(reqlite_format::Environment::default()),
        Some(path) => reqlite_format::load_env(path).map_err(|e| chain(&e)),
    };
    let resolved =
        match env.and_then(|env| reqlite_engine::resolve(&req, &env).map_err(|e| e.to_string())) {
            Ok(r) => r,
            Err(e) => {
                doc.send = Send::Finished(Err(e));
                return Task::none();
            }
        };
    let id = doc.id;
    let file = doc.file.as_ref().map(|p| p.display().to_string());
    let env = env_path.as_ref().map(|p| p.display().to_string());
    let (task, handle) = Task::perform(
        run_send(client, resolved, history, !history_tried, file, env),
        move |f| Msg::Sent(id, Box::new(f)),
    )
    .abortable();
    doc.send = Send::Running(handle);
    let now = Instant::now();
    doc.motion.since = now;
    doc.motion.running.go_mut(true, now);
    app.history_tried = true;
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
    let Some(doc) = app.doc() else {
        return Task::none();
    };
    let Some(path) = doc.file.clone() else {
        if doc.open_error.is_none() {
            app.notice = Some("No file to save to. Start reqlite-gui with a file path.".into());
        }
        return Task::none();
    };
    let req = match doc.draft().to_request() {
        Ok(r) => r,
        Err(e) => {
            app.notice = Some(format!("not saved: {e}"));
            return Task::none();
        }
    };
    let id = doc.id;
    let work = blocking(move || {
        reqlite_format::save(&path, &req).map_err(|e| chain(&e))?;
        reqlite_format::to_string(&req).map_err(|e| e.to_string())
    });
    Task::perform(async move { work.await.and_then(|r| r) }, move |r| {
        Msg::Saved(id, r)
    })
}

fn title(app: &App) -> String {
    match app.doc() {
        Some(doc) => format!("{} - Reqlite", doc.title()),
        None => "Reqlite".into(),
    }
}

/// The app's Cmd (Ctrl) shortcuts. They work wherever the focus is.
fn shortcut(key: &Key, modifiers: Modifiers) -> Option<Msg> {
    if modifiers.control() && matches!(key.as_ref(), Key::Named(Named::Tab)) {
        return Some(Msg::NextTab);
    }
    if !modifiers.command() {
        return None;
    }
    match key.as_ref() {
        Key::Character("s") => Some(Msg::Save),
        Key::Named(Named::Enter) => Some(Msg::Send),
        Key::Character("l") => Some(Msg::FocusUrl),
        Key::Character("1") => Some(Msg::Section(Section::Query)),
        Key::Character("2") => Some(Msg::Section(Section::Headers)),
        Key::Character("3") => Some(Msg::Section(Section::Body)),
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
    let now = Instant::now();
    if app.exit_after_first_frame {
        subs.push(window::frames().map(|_| Msg::FirstFrame));
    } else if app.docs.iter().any(|d| d.motion.animating(now)) {
        subs.push(window::frames().map(Msg::Tick));
    } else if app.doc().is_some_and(Doc::running) && !app.reduced_motion {
        subs.push(time::every(motion::PULSE).map(Msg::Tick));
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
    let opened = args.file.as_deref().map_or(Opened::Missing, doc::open);
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
