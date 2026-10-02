//! The history panel: recent sends, newest first. Choosing one opens it in a
//! new tab with no file, so a restore never writes over a request file.
//! History keeps what was sent, with `{{secret}}` placeholders still in place.

use super::{App, Msg, blocking, doc, open_history};
use crate::doc::{Send, Summary, Viewer};
use crate::style;
use iced::widget::{Space, button, column, container, row, scrollable, text};
use iced::{Alignment, Element, Length, Task};
use reqlite_gui::draft::Draft;
use reqlite_store::{Entry, Outcome, SentRequest, Store};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How many sends the panel lists.
const LIMIT: usize = 200;

#[derive(Default)]
pub struct History {
    pub rows: Vec<reqlite_store::Summary>,
    /// Show only the sends of the shown tab's file.
    pub this_request: bool,
    pub error: Option<String>,
    pub loading: bool,
}

#[derive(Clone)]
pub enum HistMsg {
    /// The store, opened off the UI thread for the panel.
    Opened(Result<Option<Store>, String>),
    Listed(Result<Vec<reqlite_store::Summary>, String>),
    Open(i64),
    Fetched(Result<Option<Entry>, String>),
    ThisRequest(bool),
}

/// Lists the recent sends, opening the history file first if no send did yet.
pub fn refresh(app: &mut App) -> Task<Msg> {
    app.history_view.loading = true;
    match app.history.clone() {
        Some(store) => list(store),
        None => {
            app.history_tried = true;
            Task::perform(
                async { blocking(open_history).await.and_then(|r| r) },
                |r| Msg::History(HistMsg::Opened(r)),
            )
        }
    }
}

fn list(store: Store) -> Task<Msg> {
    Task::perform(
        async move { store.recent(LIMIT).await.map_err(|e| e.to_string()) },
        |r| Msg::History(HistMsg::Listed(r)),
    )
}

pub fn update(app: &mut App, msg: HistMsg) -> Task<Msg> {
    let view = &mut app.history_view;
    match msg {
        HistMsg::Opened(Ok(Some(store))) => {
            app.history = Some(store.clone());
            return list(store);
        }
        HistMsg::Opened(Ok(None)) => {
            view.loading = false;
            view.error = Some("History is off: this system has no data folder.".into());
        }
        HistMsg::Opened(Err(e)) | HistMsg::Listed(Err(e)) | HistMsg::Fetched(Err(e)) => {
            view.loading = false;
            view.error = Some(e);
        }
        HistMsg::Listed(Ok(rows)) => {
            view.loading = false;
            view.error = None;
            view.rows = rows;
        }
        HistMsg::ThisRequest(on) => view.this_request = on,
        HistMsg::Open(id) => {
            if let Some(store) = app.history.clone() {
                return Task::perform(
                    async move { store.get(id).await.map_err(|e| e.to_string()) },
                    |r| Msg::History(HistMsg::Fetched(r)),
                );
            }
        }
        HistMsg::Fetched(Ok(None)) => {
            app.notice = Some("That history entry is gone.".into());
        }
        HistMsg::Fetched(Ok(Some(entry))) => restore(app, entry),
    }
    Task::none()
}

/// Opens `entry` in a new tab: the request as it was sent, and its response.
fn restore(app: &mut App, entry: Entry) {
    let draft = draft(&entry.request);
    let id = app.open_tab(None, &doc::Opened::Missing);
    let Some(d) = app.by_id(id) else { return };
    d.fill(&draft);
    d.label = format!("{} {}", entry.request.method, short(&entry.request.url));
    match entry.outcome {
        Outcome::Failed { error } => d.send = Send::Finished(Err(error)),
        Outcome::Response {
            status,
            body,
            body_len,
            elapsed_ms,
            ..
        } => {
            let shown = body.len() as u64;
            match reqlite_viewer::Document::build(&body[..]) {
                Ok(doc) => {
                    d.viewer = Some(Viewer {
                        doc: Arc::new(doc),
                        top: 0,
                    });
                    d.send = Send::Finished(Ok(Summary {
                        status,
                        elapsed: Duration::from_millis(elapsed_ms),
                        bytes: body_len,
                    }));
                }
                Err(e) => d.send = Send::Finished(Err(format!("cannot show the response: {e}"))),
            }
            if shown < body_len {
                app.notice = Some(format!(
                    "History keeps the first {} of this response.",
                    reqlite_gui::present::human_size(shown)
                ));
            }
        }
    }
}

fn draft(r: &SentRequest) -> Draft {
    let lines = |pairs: &[(String, String)]| -> String {
        pairs.iter().map(|(k, v)| format!("{k}: {v}\n")).collect()
    };
    Draft {
        name: format!("{} {}", r.method, short(&r.url)),
        method: r.method.clone(),
        url: r.url.clone(),
        headers: lines(&r.headers),
        query: lines(&r.query),
        body: r.body.clone().unwrap_or_default(),
    }
}

/// The path of a URL, for a tab label: `https://h/users?id=1` is `/users`.
fn short(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let path = rest.find('/').map_or("/", |i| &rest[i..]);
    let path = path.split(['?', '#']).next().unwrap_or(path);
    path.chars().take(32).collect()
}

/// "just now", "5 min ago", "3 h ago", "2 d ago".
fn ago(at_ms: i64, now: SystemTime) -> String {
    let now_ms = now
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let s = (now_ms - at_ms).max(0) / 1000;
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", s / 60),
        3600..86400 => format!("{} h ago", s / 3600),
        _ => format!("{} d ago", s / 86400),
    }
}

pub fn view(app: &App) -> Element<'_, Msg> {
    let h = &app.history_view;
    let file = app
        .doc()
        .and_then(|d| d.file.as_ref())
        .map(|p| p.display().to_string());
    let filter = button(text("This request").size(11))
        .padding([2, 6])
        .on_press_maybe(
            file.is_some()
                .then_some(Msg::History(HistMsg::ThisRequest(!h.this_request))),
        )
        .style(move |t, s| style::tab(t, s, h.this_request));
    let header = row![
        text("History")
            .size(12)
            .font(style::MEDIUM)
            .color(style::MUTED),
        Space::new().width(Length::Fill),
        filter,
    ]
    .align_y(Alignment::Center);

    let now = SystemTime::now();
    let rows = h
        .rows
        .iter()
        .filter(|r| !h.this_request || (file.is_some() && r.file == file))
        .map(|r| {
            let status: Element<'_, Msg> = match (r.status, &r.error) {
                (Some(s), _) => text(s.to_string())
                    .size(11)
                    .font(style::MONO)
                    .color(style::status_color(s))
                    .into(),
                (None, _) => text("error").size(11).color(style::DANGER).into(),
            };
            let line = column![
                row![
                    text(&r.method)
                        .size(11)
                        .font(style::MONO)
                        .color(style::method_color(&r.method)),
                    text(short(&r.url)).size(12),
                    Space::new().width(Length::Fill),
                    status,
                ]
                .spacing(6)
                .align_y(Alignment::Center),
                text(match r.elapsed_ms {
                    Some(ms) => format!("{} · {ms} ms", ago(r.at_ms, now)),
                    None => ago(r.at_ms, now),
                })
                .size(10)
                .color(style::FAINT),
            ]
            .spacing(2);
            button(line)
                .padding([4, 6])
                .width(Length::Fill)
                .on_press(Msg::History(HistMsg::Open(r.id)))
                .style(|t, s| style::tab(t, s, false))
                .into()
        });
    let mut list = column(rows).spacing(1);
    if let Some(e) = &h.error {
        list = list.push(text(e).size(12).color(style::DANGER));
    } else if h.rows.is_empty() && !h.loading {
        list = list.push(text("No sends yet.").size(12).color(style::FAINT));
    }
    container(column![header, scrollable(list).height(Length::Fill)].spacing(8))
        .padding(10)
        .width(crate::sidebar::WIDTH)
        .height(Length::Fill)
        .style(style::panel)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_use_the_url_path() {
        assert_eq!(short("https://api.example.com/users?id=1"), "/users");
        assert_eq!(short("http://h"), "/");
        assert_eq!(short("{{base}}/users/7#x"), "/users/7");
    }

    #[test]
    fn times_read_as_how_long_ago() {
        let now = UNIX_EPOCH + Duration::from_secs(100_000);
        let at = |s: i64| (100_000 - s) * 1000;
        assert_eq!(ago(at(5), now), "just now");
        assert_eq!(ago(at(125), now), "2 min ago");
        assert_eq!(ago(at(7300), now), "2 h ago");
        assert_eq!(ago(at(90_000), now), "1 d ago");
        assert_eq!(ago(at(-30), now), "just now", "a clock that moved back");
    }
}
