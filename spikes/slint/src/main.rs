use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqlite_viewer::Document;
use slint::{ComponentHandle, Model, ModelRc, RenderingState, SharedString, Timer, TimerMode, VecModel};

slint::include_modules!();

struct Loaded {
    doc: Arc<Document>,
    status: String,
}

#[derive(Default)]
struct State {
    doc: Option<Arc<Document>>,
    shown: (usize, usize),
    sent_at: Option<Instant>,
    last_tick: Option<Instant>,
    max_gap: Duration,
    max_gap_post: Duration,
    awaiting_viewer: bool,
    frames: u64,
    first_after_render: Option<Instant>,
    edit_pending: Option<Instant>,
    edit_times: Vec<f64>,
    dispatch_times: Vec<f64>,
    edits_left: u32,
}

fn main() {
    let started = Instant::now();
    let exit_on = std::env::var("REQLITE_SPIKE_EXIT_ON").unwrap_or_default();

    let ui = App::new().unwrap();
    let lines = Rc::new(VecModel::<SharedString>::default());
    ui.set_lines(ModelRc::from(lines.clone()));
    let state = Rc::new(RefCell::new(State::default()));

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<reqlite_engine::Resolved>();
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            let client = reqlite_engine::client().unwrap();
            while let Some(req) = rx.recv().await {
                let client = client.clone();
                let weak = weak.clone();
                tokio::spawn(async move {
                    let result: Result<Loaded, String> = async {
                        let resp = reqlite_engine::send(&client, &req).await.map_err(|e| {
                            let mut s = e.to_string();
                            let mut src = std::error::Error::source(&e);
                            while let Some(c) = src {
                                s += &format!(": {c}");
                                src = c.source();
                            }
                            s
                        })?;
                        let status = format!(
                            "{} · {} ms · {} bytes",
                            resp.status,
                            resp.elapsed.as_millis(),
                            resp.body.len()
                        );
                        let doc = tokio::task::spawn_blocking(move || {
                            Document::build(resp.body.reader()?)
                        })
                        .await
                        .map_err(|e| e.to_string())?
                        .map_err(|e| e.to_string())?;
                        Ok(Loaded { doc: Arc::new(doc), status })
                    }
                    .await;
                    let _ = slint::invoke_from_event_loop(move || {
                        let Some(ui) = weak.upgrade() else { return };
                        DELIVER.with(|d| (d.borrow_mut().as_mut().unwrap())(&ui, result));
                    });
                });
            }
        });
    });

    let refill = {
        let state = state.clone();
        let lines = lines.clone();
        let weak = ui.as_weak();
        Rc::new(move || {
            let ui = weak.upgrade().unwrap();
            let mut st = state.borrow_mut();
            let Some(doc) = st.doc.clone() else { return };
            let n = ui.get_visible_count().max(1) as usize;
            let max_top = doc.line_count().saturating_sub(n);
            let top = (ui.get_top().max(0.0) as usize).min(max_top);
            if st.shown == (top, n) && lines.row_count() > 0 {
                return;
            }
            st.shown = (top, n);
            let v: Vec<SharedString> = doc.lines(top, n).unwrap().into_iter().map(Into::into).collect();
            lines.set_vec(v);
        })
    };

    {
        let state = state.clone();
        let refill = refill.clone();
        DELIVER.with(move |d| {
            *d.borrow_mut() = Some(Box::new(move |ui: &App, result: Result<Loaded, String>| {
                match result {
                    Ok(l) => {
                        ui.set_status(l.status.into());
                        ui.set_line_count(l.doc.line_count() as i32);
                        ui.set_top(std::env::var("REQLITE_SPIKE_TOP").ok().and_then(|t| t.parse().ok()).unwrap_or(0.0));
                        let mut st = state.borrow_mut();
                        st.doc = Some(l.doc);
                        st.shown = (usize::MAX, 0);
                        drop(st);
                        refill();
                    }
                    Err(e) => {
                        ui.set_status(e.into());
                        state.borrow_mut().awaiting_viewer = false;
                    }
                }
            }));
        });
    }

    let send = {
        let state = state.clone();
        let weak = ui.as_weak();
        move || {
            let ui = weak.upgrade().unwrap();
            let body = ui.get_body().to_string();
            let method = match reqlite_format::Method::try_from(ui.get_method().to_string()) {
                Ok(m) => m,
                Err(e) => return ui.set_status(e.into()),
            };
            let req = reqlite_format::Request {
                version: 1,
                name: "spike".into(),
                method,
                url: ui.get_url().to_string(),
                headers: Default::default(),
                query: Default::default(),
                body: (!body.is_empty()).then_some(body),
            };
            match reqlite_engine::resolve(&req, &reqlite_format::Environment::default()) {
                Ok(r) => {
                    ui.set_status("sending…".into());
                    let now = Instant::now();
                    let mut st = state.borrow_mut();
                    st.sent_at = Some(now);
                    st.last_tick = Some(now);
                    st.max_gap = Duration::ZERO;
                    st.max_gap_post = Duration::ZERO;
                    st.awaiting_viewer = true;
                    tx.send(r).unwrap();
                }
                Err(e) => ui.set_status(e.to_string().into()),
            }
        }
    };
    let send = Rc::new(send);
    ui.on_send({
        let send = send.clone();
        move || send()
    });
    ui.on_scrolled({
        let weak = ui.as_weak();
        let refill = refill.clone();
        move |top| {
            let ui = weak.upgrade().unwrap();
            let max = (ui.get_line_count() - ui.get_visible_count()).max(0);
            ui.set_top(top.clamp(0, max) as f32);
            refill();
        }
    });

    let tick = Timer::default();
    tick.start(TimerMode::Repeated, Duration::from_millis(16), {
        let state = state.clone();
        let refill = refill.clone();
        let weak = ui.as_weak();
        move || {
            let now = Instant::now();
            {
                let mut st = state.borrow_mut();
                if st.awaiting_viewer {
                    if let Some(last) = st.last_tick {
                        st.max_gap = st.max_gap.max(now - last);
                        if st.first_after_render.is_some_and(|f| last >= f) {
                            st.max_gap_post = st.max_gap_post.max(now - last);
                        }
                    }
                    st.last_tick = Some(now);
                }
            }
            refill();
            let mut st = state.borrow_mut();
            if st.edits_left > 0 && st.edit_pending.is_none() {
                st.edits_left -= 1;
                st.edit_pending = Some(Instant::now());
                drop(st);
                let ui = weak.upgrade().unwrap();
                ui.invoke_focus_body();
                let t = Instant::now();
                ui.window().dispatch_event(slint::platform::WindowEvent::KeyPressed { text: "x".into() });
                ui.window().dispatch_event(slint::platform::WindowEvent::KeyReleased { text: "x".into() });
                state.borrow_mut().dispatch_times.push(ms(t.elapsed()));
            }
        }
    });

    {
        let state = state.clone();
        let exit_on = exit_on.clone();
        let lines = lines.clone();
        let weak = ui.as_weak();
        ui.window()
            .set_rendering_notifier(move |rs, _| {
                if !matches!(rs, RenderingState::AfterRendering) {
                    return;
                }
                let now = Instant::now();
                let mut st = state.borrow_mut();
                st.frames += 1;
                if st.first_after_render.is_none() {
                    st.first_after_render = Some(now);
                    if exit_on == "first-frame" {
                        eprintln!("after-rendering {:.1}", ms(now - started));
                        Timer::single_shot(Duration::ZERO, move || {
                            println!("first-frame {:.1}", ms(started.elapsed()));
                            std::process::exit(0);
                        });
                    }
                }
                if st.awaiting_viewer && lines.row_count() > 0 {
                    st.awaiting_viewer = false;
                    let last = st.last_tick.unwrap();
                    let gap = st.max_gap.max(now - last);
                    let gap_post = st.max_gap_post.max(now - last);
                    if exit_on == "viewer-ready" {
                        println!("viewer-ready {:.1}", ms(now - started));
                        println!("max-frame-gap {:.1}", ms(gap));
                        println!("max-frame-gap-after-first-frame {:.1}", ms(gap_post));
                        std::process::exit(0);
                    } else {
                        eprintln!("viewer-ready {:.1} max-frame-gap {:.1}", ms(now - started), ms(gap));
                        if let Ok(path) = std::env::var("REQLITE_SPIKE_SNAPSHOT") {
                            let weak = weak.clone();
                            Timer::single_shot(Duration::from_millis(500), move || {
                                let ui = weak.upgrade().unwrap();
                                let img = ui.window().take_snapshot().unwrap();
                                let mut out = format!("{} {}\n", img.width(), img.height()).into_bytes();
                                out.extend_from_slice(img.as_bytes());
                                std::fs::write(path, out).unwrap();
                                std::process::exit(0);
                            });
                        }
                    }
                }
                if let Some(t) = st.edit_pending.take() {
                    st.edit_times.push(ms(now - t));
                    if st.edits_left == 0 {
                        let mut v = st.edit_times.clone();
                        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                        println!(
                            "edit-to-frame ms over {} edits: median {:.1} min {:.1} max {:.1}",
                            v.len(),
                            v[v.len() / 2],
                            v[0],
                            v[v.len() - 1]
                        );
                        let mut d = st.dispatch_times.clone();
                        d.sort_by(|a, b| a.partial_cmp(b).unwrap());
                        println!("key-dispatch ms: median {:.1} max {:.1}", d[d.len() / 2], d[d.len() - 1]);
                        println!("body-chars {}", weak.upgrade().unwrap().get_body().chars().count());
                        if exit_on == "editor-bench" {
                            std::process::exit(0);
                        }
                    }
                }
            })
            .unwrap();
    }

    if let Ok(path) = std::env::var("REQLITE_SPIKE_BODY_FILE") {
        let t = Instant::now();
        ui.set_body(std::fs::read_to_string(path).unwrap().into());
        eprintln!("body set {:.1} ms", ms(t.elapsed()));
        if exit_on == "editor-bench" {
            state.borrow_mut().edits_left = 30;
        }
    }
    if let Ok(url) = std::env::var("REQLITE_SPIKE_URL") {
        ui.set_url(url.into());
        send();
    }

    ui.run().unwrap();
}

type Deliver = Box<dyn FnMut(&App, Result<Loaded, String>)>;
thread_local! {
    static DELIVER: RefCell<Option<Deliver>> = const { RefCell::new(None) };
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}
