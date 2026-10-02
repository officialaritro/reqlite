//! `reqlite-gui FILE [--env ENV]`: open, edit, save and send one request file.

use clap::Parser;
use iced::keyboard::{self, Key, key::Named};
use iced::task::Handle;
use iced::widget::{
    button, column, container, mouse_area, responsive, row, text, text_editor, text_input,
    vertical_slider,
};
use iced::{Element, Length, Subscription, Task, event, mouse, window};
use reqlite_gui::chain;
use reqlite_gui::draft::Draft;
use reqlite_gui::present::glass_supported;
use reqlite_viewer::Document;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

mod style;

const LINE_HEIGHT: f32 = 18.0;
const PAGE: i64 = 40;

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
    url: String,
    headers: text_editor::Content,
    query: text_editor::Content,
    body: text_editor::Content,
    /// Derived from the fields and `saved`, refreshed after each change.
    dirty: bool,
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
}

enum Send {
    Idle,
    Running(Handle),
    Cancelled,
    Finished(Result<String, String>),
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
    Save,
    Saved(Result<String, String>),
    Scroll(i64),
    ScrollTo(f64),
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
    summary: String,
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
        url: String::new(),
        headers: text_editor::Content::new(),
        query: text_editor::Content::new(),
        body: text_editor::Content::new(),
        dirty: false,
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
    };
    match opened {
        Opened::Missing => {}
        Opened::Loaded(req, canonical) => {
            load(&mut app, &Draft::from_request(req));
            app.saved = Some(canonical.clone());
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
    match msg {
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
            match result {
                Ok(loaded) => {
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
    app.dirty = draft(app).differs_from(app.saved.as_deref());
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
    let summary = format!(
        "{} · {} ms · {} bytes",
        resp.status,
        resp.elapsed.as_millis(),
        resp.body.len()
    );
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

fn view(app: &App) -> Element<'_, Msg> {
    let running = matches!(app.send, Send::Running(_));
    let action = if running {
        button("Cancel").on_press(Msg::Cancel).style(style::stop)
    } else {
        button("Send").on_press(Msg::Send).style(style::accent)
    };
    let save = button("Save")
        .on_press_maybe((app.dirty && app.file.is_some()).then_some(Msg::Save))
        .style(style::neutral);
    let top = row![
        text_input("GET", &app.method)
            .on_input(Msg::Method)
            .style(style::input)
            .width(90),
        text_input("https://", &app.url)
            .on_input(Msg::Url)
            .on_submit(Msg::Send)
            .style(style::input),
        action,
        save,
    ]
    .spacing(8);

    let editor = |label: &'static str, content: &'static str, value, on: fn(_) -> Msg, height| {
        column![
            text(label).size(12).color(style::MUTED),
            text_editor(value)
                .placeholder(content)
                .key_binding(editor_keys)
                .on_action(on)
                .font(style::MONO)
                .style(style::editor)
                .height(height),
        ]
        .spacing(4)
    };
    let fields = row![
        editor("Headers", "Name: value", &app.headers, Msg::Headers, 110),
        editor("Query", "name: value", &app.query, Msg::Query, 110),
    ]
    .spacing(8);
    let body = editor("Body", "", &app.body, Msg::Body, 140);

    let status = match &app.send {
        Send::Idle => String::new(),
        Send::Running(_) => "sending...".into(),
        Send::Cancelled => "cancelled".into(),
        Send::Finished(Ok(s)) => s.clone(),
        Send::Finished(Err(e)) => format!("error: {e}"),
    };
    let mut page = column![top, fields, body, text(status).font(style::MONO)].spacing(8);
    if let Some(e) = &app.open_error {
        page = page.push(text(e).size(12).font(style::MONO).color(style::DANGER));
    }
    if let Some(notice) = &app.notice {
        page = page.push(text(notice).size(12));
    }
    page.push(viewer(app.viewer.as_ref())).padding(12).into()
}

fn viewer(v: Option<&Viewer>) -> Element<'_, Msg> {
    let Some(v) = v else {
        return container(text("No response yet").size(12))
            .height(Length::Fill)
            .into();
    };
    let doc = v.doc.clone();
    let top = v.top;
    let lines = responsive(move |size| {
        let count = (size.height / LINE_HEIGHT).floor() as usize;
        let lines = doc
            .lines(top, count)
            .unwrap_or_else(|e| vec![format!("cannot read the response: {e}")]);
        let col = column(lines.into_iter().map(|l| {
            text(l)
                .font(style::MONO)
                .size(13)
                .line_height(iced::Pixels(LINE_HEIGHT))
                .wrapping(text::Wrapping::None)
                .into()
        }));
        container(col)
            .clip(true)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    });
    let max = v.doc.line_count().saturating_sub(1) as f64;
    column![
        text(format!("line {} of {}", v.top + 1, v.doc.line_count())).size(12),
        row![
            mouse_area(lines).on_scroll(|delta| Msg::Scroll(match delta {
                mouse::ScrollDelta::Lines { y, .. } => (-y * 3.0).round() as i64,
                mouse::ScrollDelta::Pixels { y, .. } => (-y / LINE_HEIGHT).round() as i64,
            })),
            // The slider's value grows upwards, so it shows the distance from the end.
            vertical_slider(0.0..=max.max(1.0), max - v.top as f64, move |x| {
                Msg::ScrollTo(max - x)
            })
            .step(1.0),
        ]
        .height(Length::Fill),
    ]
    .spacing(4)
    .height(Length::Fill)
    .into()
}

/// The editors' default bindings, minus the app's own shortcuts. Without this,
/// Cmd+S also types an "s" and Cmd+Enter a new line into the focused editor.
fn editor_keys(press: text_editor::KeyPress) -> Option<text_editor::Binding<Msg>> {
    let app_shortcut = press.modifiers.command()
        && matches!(
            press.key.as_ref(),
            Key::Character("s") | Key::Named(Named::Enter)
        );
    if app_shortcut {
        None
    } else {
        text_editor::Binding::from_key_press(press)
    }
}

fn keys(event: iced::Event, status: event::Status, _: window::Id) -> Option<Msg> {
    let iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event else {
        return None;
    };
    match key.as_ref() {
        Key::Character("s") if modifiers.command() => Some(Msg::Save),
        Key::Named(Named::Enter) if modifiers.command() => Some(Msg::Send),
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
    // Only the cold start check listens to frames. A frame subscription makes
    // iced redraw back to back, which costs a third drawable (issue #1).
    if app.exit_after_first_frame {
        subs.push(window::frames().map(|_| Msg::FirstFrame));
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
    let mut app = iced::application(move || boot(&args, &opened, started), update, view)
        .title(title)
        .subscription(subscription)
        .theme(style::theme())
        .style(move |_, _| style::window(glass))
        .default_font(style::SANS)
        .window(window::Settings {
            size: iced::Size::new(1000.0, 800.0),
            transparent: glass,
            blur: glass,
            ..window::Settings::default()
        });
    for face in style::FONTS {
        app = app.font(face);
    }
    app.run()
}
