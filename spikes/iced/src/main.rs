use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use iced::widget::{button, column, container, mouse_area, responsive, row, text, text_editor, text_input, vertical_slider};
use iced::{Element, Font, Length, Subscription, Task, mouse, time, window};
use reqlite_viewer::Document;
use tokio::sync::{mpsc, oneshot};

static STARTED: OnceLock<Instant> = OnceLock::new();
const LINE_H: f32 = 18.0;

type Outcome = Result<Loaded, String>;

#[derive(Clone)]
struct Loaded {
    doc: Arc<Document>,
    status: u16,
    elapsed: Duration,
    bytes: u64,
}

type Job = (reqlite_engine::Resolved, oneshot::Sender<Outcome>);

#[derive(Clone)]
enum Msg {
    Method(String),
    Url(String),
    Edit(text_editor::Action),
    Send,
    Done(Outcome),
    Scroll(mouse::ScrollDelta),
    Slider(f64),
    Tick(Instant),
    Frame,
    BenchStep,
}

#[derive(PartialEq)]
enum Exit {
    None,
    FirstFrame,
    ViewerReady,
}

struct App {
    method: String,
    url: String,
    body: text_editor::Content,
    status: String,
    doc: Option<Arc<Document>>,
    top: usize,
    jobs: mpsc::UnboundedSender<Job>,
    exit: Exit,
    first_frame_seen: bool,
    loading_since: Option<Instant>,
    last_tick: Option<Instant>,
    max_gap: Duration,
    awaiting_ready: bool,
    bench: Option<Bench>,
    last_tick_at: Option<Instant>,
}

struct Bench {
    steps: usize,
    payload: Option<String>,
    last_step: Option<Instant>,
    step_gaps: Vec<Duration>,
    paste_at: Option<Instant>,
    perform: Vec<Duration>,
    last_frame: Option<Instant>,
    frame_gaps: Vec<Duration>,
}

fn spawn_worker() -> mpsc::UnboundedSender<Job> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Job>();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let client = reqlite_engine::client().unwrap();
            while let Some((resolved, reply)) = rx.recv().await {
                let client = client.clone();
                tokio::spawn(async move {
                    let out = async {
                        let resp = reqlite_engine::send(&client, &resolved).await.map_err(|e| {
                            let mut s = e.to_string();
                            let mut src = std::error::Error::source(&e);
                            while let Some(c) = src {
                                s += &format!(": {c}");
                                src = c.source();
                            }
                            s
                        })?;
                        let bytes = resp.body.len();
                        let (status, elapsed) = (resp.status, resp.elapsed);
                        let doc = tokio::task::spawn_blocking(move || {
                            Document::build(resp.body.reader()?)
                        })
                        .await
                        .map_err(|e| e.to_string())?
                        .map_err(|e| e.to_string())?;
                        Ok(Loaded { doc: Arc::new(doc), status, elapsed, bytes })
                    }
                    .await;
                    let _ = reply.send(out);
                });
            }
        });
    });
    tx
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn boot() -> (App, Task<Msg>) {
    let exit = match std::env::var("REQLITE_SPIKE_EXIT_ON").as_deref() {
        Ok("first-frame") => Exit::FirstFrame,
        Ok("viewer-ready") => Exit::ViewerReady,
        _ => Exit::None,
    };
    let url = std::env::var("REQLITE_SPIKE_URL").ok();
    let bench = std::env::var("REQLITE_SPIKE_EDITOR_BENCH").is_ok();
    let mut app = App {
        method: "GET".into(),
        url: url.clone().unwrap_or_default(),
        body: text_editor::Content::new(),
        status: String::new(),
        doc: None,
        top: 0,
        jobs: spawn_worker(),
        exit,
        first_frame_seen: false,
        loading_since: None,
        last_tick: None,
        max_gap: Duration::ZERO,
        awaiting_ready: false,
        bench: None,
        last_tick_at: None,
    };
    if bench {
        let mut payload = String::with_capacity(1 << 20);
        let mut i = 0;
        while payload.len() < 1 << 20 {
            payload += &format!("  {{\"id\": {i}, \"name\": \"user {i}\", \"email\": \"u{i}@example.com\"}},\n");
            i += 1;
        }
        app.bench = Some(Bench { steps: 0, payload: Some(payload), perform: vec![], last_frame: None, frame_gaps: vec![], last_step: None, step_gaps: vec![], paste_at: None });
    }
    let delay = std::env::var("REQLITE_SPIKE_SEND_DELAY_MS").ok().and_then(|v| v.parse().ok());
    let task = match (url.is_some(), delay) {
        (false, _) => Task::none(),
        (true, None) => Task::done(Msg::Send),
        (true, Some(d)) => Task::perform(tokio::time::sleep(Duration::from_millis(d)), |_| Msg::Send),
    };
    (app, task)
}

fn update(app: &mut App, msg: Msg) -> Task<Msg> {
    match msg {
        Msg::Method(m) => app.method = m,
        Msg::Url(u) => app.url = u,
        Msg::Edit(a) => app.body.perform(a),
        Msg::Send => {
            let method = match reqlite_format::Method::try_from(app.method.clone()) {
                Ok(m) => m,
                Err(e) => {
                    app.status = e;
                    return Task::none();
                }
            };
            let body = app.body.text();
            let req = reqlite_format::Request {
                version: 1,
                name: "spike".into(),
                method,
                url: app.url.clone(),
                headers: Default::default(),
                query: Default::default(),
                body: if body.trim().is_empty() { None } else { Some(body) },
            };
            let resolved = match reqlite_engine::resolve(&req, &reqlite_format::Environment::default()) {
                Ok(r) => r,
                Err(e) => {
                    app.status = e.to_string();
                    return Task::none();
                }
            };
            let (tx, rx) = oneshot::channel();
            let _ = app.jobs.send((resolved, tx));
            app.status = "sending...".into();
            let now = Instant::now();
            app.loading_since = Some(now);
            if std::env::var("REQLITE_SPIKE_SEND_DELAY_MS").is_ok() { println!("send-at {:.1}", ms(now - *STARTED.get().unwrap())); }
            app.last_tick = Some(now);
            app.max_gap = Duration::ZERO;
            return Task::perform(rx, |r| Msg::Done(r.unwrap_or_else(|e| Err(e.to_string()))));
        }
        Msg::Done(out) => {
            if std::env::var("REQLITE_SPIKE_DEBUG").is_ok() { eprintln!("done at {:.1}", ms(STARTED.get().unwrap().elapsed())); }
            match out {
                Ok(l) => {
                    app.status = format!("{} · {} ms · {} bytes", l.status, l.elapsed.as_millis(), l.bytes);
                    app.doc = Some(l.doc);
                    app.top = 0;
                    app.awaiting_ready = true;
                }
                Err(e) => {
                    app.status = e;
                    app.loading_since = None;
                }
            }
        }
        Msg::Scroll(d) => {
            let dy = match d {
                mouse::ScrollDelta::Lines { y, .. } => -y * 3.0,
                mouse::ScrollDelta::Pixels { y, .. } => -y / LINE_H,
            };
            scroll_by(app, dy.round() as i64);
        }
        Msg::Slider(v) => {
            let max = max_top(app);
            app.top = (max as f64 - v).round().clamp(0.0, max as f64) as usize;
        }
        Msg::Tick(at) => {
            if std::env::var("REQLITE_SPIKE_DEBUG").is_ok() {
                if let Some(prev) = app.last_tick_at { if at - prev > Duration::from_millis(40) { eprintln!("producer gap {:.1}", ms(at - prev)); } }
                app.last_tick_at = Some(at);
            }
            tick(app, Instant::now())
        }
        Msg::Frame => {
            let now = Instant::now();
            if !app.first_frame_seen {
                app.first_frame_seen = true;
                if std::env::var("REQLITE_SPIKE_DEBUG").is_ok() { eprintln!("first frame at {:.1}", ms(STARTED.get().unwrap().elapsed())); }
                if app.exit == Exit::FirstFrame {
                    println!("first-frame {:.1}", ms(STARTED.get().unwrap().elapsed()));
                    std::process::exit(0);
                }
            }
            if app.awaiting_ready {
                app.awaiting_ready = false;
                tick(app, now);
                let since = STARTED.get().unwrap().elapsed();
                app.loading_since = None;
                if app.exit == Exit::ViewerReady {
                    println!("viewer-ready {:.1}", ms(since));
                    println!("max-frame-gap {:.1}", ms(app.max_gap));
                    std::process::exit(0);
                }
            }
            if let Some(b) = &mut app.bench {
                if let Some(last) = b.last_frame {
                    if b.paste_at.is_some() {
                        b.frame_gaps.push(now - last);
                    }
                }
                if let Some(p) = b.paste_at {
                    if b.frame_gaps.len() == 1 {
                        println!("paste-to-next-frame {:.1} ms", ms(now - p));
                    }
                }
                b.last_frame = Some(now);
            }
        }
        Msg::BenchStep => {
            let Some(b) = &mut app.bench else { return Task::none() };
            let now = Instant::now();
            b.steps += 1;
            if b.paste_at.is_some() {
                if let Some(l) = b.last_step { b.step_gaps.push(now - l); }
            }
            b.last_step = Some(now);
            if b.steps < 60 {
                return Task::none();
            }
            if let Some(payload) = b.payload.take() {
                let t = Instant::now();
                app.body.perform(text_editor::Action::Edit(text_editor::Edit::Paste(Arc::new(payload))));
                println!("editor-paste-1mb-perform {:.1} ms ({} lines)", ms(t.elapsed()), app.body.line_count());
                app.body.perform(text_editor::Action::Move(text_editor::Motion::DocumentStart));
                b.paste_at = Some(Instant::now());
                return Task::none();
            }
            let action = if b.steps % 40 == 39 { text_editor::Edit::Enter } else { text_editor::Edit::Insert('x') };
            let t = Instant::now();
            app.body.perform(text_editor::Action::Edit(action));
            b.perform.push(t.elapsed());
            if b.steps == 360 {
                report("editor-keystroke-perform", &mut b.perform);
                println!("editor-paste-frame-gap {:.1} ms", ms(b.frame_gaps[0]));
                report("editor-frame-gap-typing", &mut b.frame_gaps[1..]);
                report("editor-ui-tick-gap-typing", &mut b.step_gaps[1..]);
                std::process::exit(0);
            }
        }
    }
    Task::none()
}

fn report(name: &str, v: &mut [Duration]) {
    v.sort();
    if v.is_empty() {
        println!("{name} no samples");
        return;
    }
    let p = |q: f64| ms(v[((v.len() - 1) as f64 * q) as usize]);
    println!("{name} n={} median={:.2} p95={:.2} max={:.2} ms", v.len(), p(0.5), p(0.95), p(1.0));
}

fn tick(app: &mut App, now: Instant) {
    if app.loading_since.is_some() {
        if let Some(last) = app.last_tick {
            if std::env::var("REQLITE_SPIKE_DEBUG").is_ok() && now - last > Duration::from_millis(40) {
                let s = STARTED.get().unwrap();
                eprintln!("gap {:.1} ms from {:.1} to {:.1}", ms(now - last), ms(last - *s), ms(now - *s));
            }
            app.max_gap = app.max_gap.max(now - last);
        }
        app.last_tick = Some(now);
    }
}

fn max_top(app: &App) -> usize {
    app.doc.as_ref().map_or(0, |d| d.line_count().saturating_sub(1))
}

fn scroll_by(app: &mut App, d: i64) {
    let max = max_top(app) as i64;
    app.top = (app.top as i64 + d).clamp(0, max) as usize;
}

fn view(app: &App) -> Element<'_, Msg> {
    let top = row![
        text_input("GET", &app.method).on_input(Msg::Method).width(90),
        text_input("https://", &app.url).on_input(Msg::Url).on_submit(Msg::Send),
        button("Send").on_press(Msg::Send),
    ]
    .spacing(8);
    let editor = text_editor(&app.body)
        .on_action(Msg::Edit)
        .font(Font::MONOSPACE)
        .height(160);
    let viewer: Element<'_, Msg> = match &app.doc {
        None => container(text("No response")).height(Length::Fill).into(),
        Some(doc) => {
            let top_line = app.top;
            let max = max_top(app) as f64;
            let lines = responsive(move |size| {
                let count = (size.height / LINE_H).floor() as usize;
                let lines = doc.lines(top_line, count).unwrap_or_else(|e| vec![e.to_string()]);
                let col = column(lines.into_iter().map(|l| {
                    text(l).font(Font::MONOSPACE).size(13).line_height(iced::Pixels(LINE_H)).wrapping(text::Wrapping::None).into()
                }));
                container(col).clip(true).width(Length::Fill).height(Length::Fill).into()
            });
            row![
                mouse_area(lines).on_scroll(Msg::Scroll),
                vertical_slider(0.0..=max.max(1.0), max - top_line as f64, Msg::Slider).step(1.0),
            ]
            .height(Length::Fill)
            .into()
        }
    };
    column![
        top,
        editor,
        text(&app.status).font(Font::MONOSPACE),
        text(format!("line {} of {}", app.top + 1, app.doc.as_ref().map_or(0, |d| d.line_count()))).size(12),
        viewer
    ]
    .spacing(8)
    .padding(12)
    .into()
}

fn subscription(app: &App) -> Subscription<Msg> {
    let mut subs = vec![];
    if !app.first_frame_seen || app.awaiting_ready || app.bench.is_some() {
        subs.push(window::frames().map(|_| Msg::Frame));
    }
    if app.loading_since.is_some() {
        subs.push(time::every(Duration::from_millis(16)).map(Msg::Tick));
    }
    if app.bench.is_some() {
        subs.push(time::every(Duration::from_millis(16)).map(|_| Msg::BenchStep));
    }
    Subscription::batch(subs)
}

fn main() -> iced::Result {
    let started = Instant::now();
    STARTED.set(started).unwrap();
    iced::application(boot, update, view)
        .title("Reqlite iced spike")
        .subscription(subscription)
        .window_size((1000.0, 800.0))
        .run()
}
