//! WebSocket and SSE against local servers: what each side sends, in order,
//! and why the connection ended.
// The servers' helpers sit outside `#[test]` functions, where
// `allow-unwrap-in-tests` does not reach. The handshake callback's error type
// is set by tungstenite.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::result_large_err
)]

use futures_util::{SinkExt, StreamExt};
use reqlite_engine::resolve;
use reqlite_engine::stream::{Event, Kind, Timed, kind, open};
use reqlite_format::Environment;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

fn request(url: &str, headers: &str) -> reqlite_engine::Resolved {
    let text = format!("version = 1\nname = \"s\"\nurl = \"{url}\"\n\n[headers]\n{headers}");
    resolve(
        &reqlite_format::parse(&text).unwrap(),
        &Environment::default(),
    )
    .unwrap()
}

/// Echoes each text frame as `echo: TEXT`, only to a client that sent the
/// header `X-Token: t-1`. Closes with 1000 "done" after `bye`.
async fn ws_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/chat", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let check = |req: &tokio_tungstenite::tungstenite::handshake::server::Request,
                             resp| {
                    match req.headers().get("x-token").and_then(|v| v.to_str().ok()) {
                        Some("t-1") => Ok(resp),
                        _ => {
                            let mut no = tokio_tungstenite::tungstenite::handshake::server::ErrorResponse::new(None);
                            *no.status_mut() =
                                tokio_tungstenite::tungstenite::http::StatusCode::UNAUTHORIZED;
                            Err(no)
                        }
                    }
                };
                let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(sock, check).await else {
                    return;
                };
                while let Some(Ok(Message::Text(t))) = ws.next().await {
                    if t.as_str() == "bye" {
                        let frame = CloseFrame {
                            code: CloseCode::Normal,
                            reason: "done".into(),
                        };
                        ws.send(Message::Close(Some(frame))).await.ok();
                        break;
                    }
                    ws.send(Message::text(format!("echo: {t}"))).await.unwrap();
                }
            });
        }
    });
    url
}

/// Runs a session, sending `messages` one at a time, each after the previous
/// one's echo. Returns the events without their times, and the result.
async fn session(
    req: &reqlite_engine::Resolved,
    messages: &[&str],
    stop_after: Option<usize>,
) -> (Vec<Event>, Result<(), String>, Vec<Duration>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let mut queue: Vec<String> = messages.iter().rev().map(|m| m.to_string()).collect();
    let mut events = Vec::new();
    let mut received = 0;
    let result = open(req, None, rx, |t: Timed| {
        if matches!(t.event, Event::Open { .. } | Event::Received { .. }) {
            if let Some(m) = queue.pop() {
                tx.send(m).unwrap();
            }
        }
        if matches!(t.event, Event::Received { .. }) {
            received += 1;
        }
        events.push(t);
        stop_after.is_none_or(|n| received < n)
    })
    .await;
    let times = events.iter().map(|t| t.at).collect();
    (events.into_iter().map(|t| t.event).collect(), result, times)
}

fn got(data: &str) -> Event {
    Event::Received {
        name: None,
        data: data.into(),
    }
}

fn sent(data: &str) -> Event {
    Event::Sent { data: data.into() }
}

#[tokio::test]
async fn a_websocket_logs_both_directions_and_the_server_close() {
    let url = ws_server().await;
    let req = request(&url, "X-Token = \"t-1\"\n");
    assert_eq!(kind(&req), Some(Kind::WebSocket));
    let (events, result, times) = session(&req, &["hi", "{\"a\": 1}", "bye"], None).await;
    assert_eq!(result, Ok(()));
    assert_eq!(
        events,
        [
            Event::Open { status: 101 },
            sent("hi"),
            got("echo: hi"),
            sent("{\"a\": 1}"),
            got("echo: {\"a\": 1}"),
            sent("bye"),
            Event::Closed {
                reason: "closed by the server: 1000 done".into()
            },
        ]
    );
    assert!(times.windows(2).all(|w| w[0] <= w[1]), "{times:?}");
}

#[tokio::test]
async fn the_caller_can_end_a_websocket() {
    let url = ws_server().await;
    let req = request(&url, "X-Token = \"t-1\"\n");
    let (events, result, _) = session(&req, &["one", "two"], Some(1)).await;
    assert_eq!(result, Ok(()));
    assert_eq!(
        events.last(),
        Some(&Event::Closed {
            reason: "closed by you".into()
        })
    );
    assert!(!events.contains(&sent("two")));
}

#[tokio::test]
async fn a_refused_handshake_is_an_error() {
    let url = ws_server().await;
    let (events, result, _) = session(&request(&url, ""), &[], None).await;
    let err = result.unwrap_err();
    assert!(
        err.contains("handshake failed") && err.contains("401"),
        "{err}"
    );
    assert_eq!(events, [Event::Closed { reason: err }]);
}

/// Answers once with `head`, then writes each chunk with a pause, then closes.
async fn raw_server(head: &'static str, chunks: &'static [&'static str]) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/events", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = vec![0; 4096];
        let n = sock.read(&mut buf).await.unwrap();
        assert!(String::from_utf8_lossy(&buf[..n]).contains("accept: text/event-stream"));
        sock.write_all(head.as_bytes()).await.unwrap();
        for c in chunks {
            sock.write_all(c.as_bytes()).await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
    url
}

#[tokio::test]
async fn sse_events_arrive_as_they_come() {
    let url = raw_server(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
        &[
            "event: tick\ndata: 1\n\n",
            ": keep-alive\n\n",
            "data: line one\ndata: line two\n\n",
        ],
    )
    .await;
    let req = request(&url, "Accept = \"text/event-stream\"\n");
    assert_eq!(kind(&req), Some(Kind::Sse));
    let (events, result, times) = session(&req, &[], None).await;
    assert_eq!(result, Ok(()));
    assert_eq!(
        events,
        [
            Event::Open { status: 200 },
            Event::Received {
                name: Some("tick".into()),
                data: "1".into()
            },
            got("line one\nline two"),
            Event::Closed {
                reason: "closed by the server".into()
            },
        ]
    );
    assert!(
        times[2] > times[1],
        "the second event came later: {times:?}"
    );
}

#[tokio::test]
async fn an_answer_that_is_not_a_stream_is_an_error() {
    let url = raw_server(
        "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: 2\r\n\r\nno",
        &[],
    )
    .await;
    let (_, result, _) = session(
        &request(&url, "Accept = \"text/event-stream\"\n"),
        &[],
        None,
    )
    .await;
    assert_eq!(
        result,
        Err("the server answered 404, not an event stream".into())
    );
}
