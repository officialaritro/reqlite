use reqlite_engine::Resolved;
use reqlite_format::{Environment, Method, Request};
use reqlite_viewer::Document;
use serde::Serialize;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};

struct Loaded {
    doc: Arc<Document>,
    status: u16,
    elapsed_ms: u128,
    bytes: u64,
}

type Job = (Resolved, oneshot::Sender<Result<Loaded, String>>);

struct App {
    started: Instant,
    jobs: mpsc::UnboundedSender<Job>,
    doc: Mutex<Option<Arc<Document>>>,
}

#[derive(Serialize)]
struct Sent {
    status: u16,
    elapsed_ms: u128,
    bytes: u64,
    line_count: usize,
}

#[derive(Serialize)]
struct Config {
    url: Option<String>,
    exit_on: Option<String>,
    editor_bytes: Option<usize>,
    send_after_paint: bool,
    debug: bool,
    scroll_to: Option<f64>,
    editor_no_event: bool,
}

fn worker(mut rx: mpsc::UnboundedReceiver<Job>) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async move {
        let client = reqlite_engine::client().unwrap();
        while let Some((req, reply)) = rx.recv().await {
            let result = async {
                let resp = reqlite_engine::send(&client, &req)
                    .await
                    .map_err(|e| format!("{e}: {:?}", std::error::Error::source(&e)))?;
                let status = resp.status;
                let elapsed_ms = resp.elapsed.as_millis();
                let bytes = resp.body.len();
                let doc = tokio::task::spawn_blocking(move || {
                    Document::build(resp.body.reader()?)
                })
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
                Ok(Loaded { doc: Arc::new(doc), status, elapsed_ms, bytes })
            }
            .await;
            let _ = reply.send(result);
        }
    });
}

#[tauri::command]
fn config() -> Config {
    Config {
        url: std::env::var("REQLITE_SPIKE_URL").ok(),
        exit_on: std::env::var("REQLITE_SPIKE_EXIT_ON").ok(),
        editor_bytes: std::env::var("REQLITE_SPIKE_EDITOR_BYTES").ok().and_then(|s| s.parse().ok()),
        send_after_paint: std::env::var_os("REQLITE_SPIKE_SEND_AFTER_PAINT").is_some(),
        debug: std::env::var_os("REQLITE_SPIKE_DEBUG").is_some(),
        editor_no_event: std::env::var_os("REQLITE_SPIKE_EDITOR_NO_EVENT").is_some(),
        scroll_to: std::env::var("REQLITE_SPIKE_SCROLL_TO").ok().and_then(|s| s.parse().ok()),
    }
}

#[tauri::command]
async fn send(
    state: tauri::State<'_, App>,
    method: String,
    url: String,
    body: String,
) -> Result<Sent, String> {
    let req = Request {
        version: 1,
        name: "spike".into(),
        method: Method::try_from(method)?,
        url,
        headers: Default::default(),
        query: Default::default(),
        body: if body.is_empty() { None } else { Some(body) },
    };
    let resolved = reqlite_engine::resolve(&req, &Environment::default()).map_err(|e| e.to_string())?;
    let (tx, rx) = oneshot::channel();
    state.jobs.send((resolved, tx)).map_err(|e| e.to_string())?;
    if std::env::var_os("REQLITE_SPIKE_DEBUG").is_some() { eprintln!("send-start {}", state.started.elapsed().as_millis()); }
    let loaded = rx.await.map_err(|e| e.to_string())??;
    if std::env::var_os("REQLITE_SPIKE_DEBUG").is_some() { eprintln!("doc-built {}", state.started.elapsed().as_millis()); }
    let line_count = loaded.doc.line_count();
    *state.doc.lock().unwrap() = Some(loaded.doc);
    Ok(Sent { status: loaded.status, elapsed_ms: loaded.elapsed_ms, bytes: loaded.bytes, line_count })
}

#[tauri::command]
fn lines(state: tauri::State<'_, App>, start: usize, count: usize) -> Result<Vec<String>, String> {
    let doc = state.doc.lock().unwrap().clone();
    match doc {
        Some(doc) => doc.lines(start, count).map_err(|e| e.to_string()),
        None => Ok(Vec::new()),
    }
}

#[tauri::command]
fn log(state: tauri::State<'_, App>, msg: String) {
    eprintln!("{} {msg}", state.started.elapsed().as_millis());
}

#[tauri::command]
fn report(state: tauri::State<'_, App>, lines: Vec<String>) {
    let ms = state.started.elapsed().as_millis();
    let mut out = std::io::stdout();
    for l in lines {
        let l = l.replace("{ms}", &ms.to_string());
        let _ = writeln!(out, "{l}");
    }
    let _ = out.flush();
    drop(state.doc.lock().unwrap().take());
    std::process::exit(0);
}

fn main() {
    let started = Instant::now();
    let (jobs, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || worker(rx));
    tauri::Builder::default()
        .manage(App { started, jobs, doc: Mutex::new(None) })
        .setup(|app| {
            if std::env::var_os("REQLITE_SPIKE_EXIT_ON").is_some() {
                use tauri::Manager;
                if let Some(w) = app.get_webview_window("main") {
                    w.set_always_on_top(true)?;
                    w.with_webview(|wv| unsafe {
                        use objc2::{msg_send, runtime::AnyObject, sel};
                        let view = &*(wv.inner() as *mut AnyObject);
                        let ok: bool = msg_send![view, respondsToSelector: sel!(_setWindowOcclusionDetectionEnabled:)];
                        if ok {
                            let _: () = msg_send![view, _setWindowOcclusionDetectionEnabled: false];
                        }
                        eprintln!("occlusion-detection-disabled {ok}");
                    })?;
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![config, send, lines, report, log])
        .run(tauri::generate_context!())
        .expect("tauri run");
}
