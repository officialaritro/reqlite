//! `reqlite listen` against a local WebSocket server and a local SSE server.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
use common::{reqlite, write};
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::net::TcpListener;
use tungstenite::Message;
use tungstenite::protocol::CloseFrame;
use tungstenite::protocol::frame::coding::CloseCode;

/// Echoes text frames as `echo: TEXT`, and closes with 1000 "done" after `bye`.
fn ws_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("ws://{}/chat", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for sock in listener.incoming() {
            let mut ws = tungstenite::accept(sock.unwrap()).unwrap();
            while let Ok(Message::Text(t)) = ws.read() {
                if t.as_str() == "bye" {
                    let frame = CloseFrame {
                        code: CloseCode::Normal,
                        reason: "done".into(),
                    };
                    ws.close(Some(frame)).ok();
                    while ws.read().is_ok() {}
                    break;
                }
                ws.send(Message::text(format!("echo: {t}"))).unwrap();
            }
        }
    });
    url
}

/// Sends three events, a little apart, and keeps the stream open.
fn sse_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/events", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for sock in listener.incoming() {
            let mut sock = sock.unwrap();
            let mut buf = [0; 4096];
            let _read = sock.read(&mut buf).unwrap();
            sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n")
                .unwrap();
            for n in 1..=3 {
                if sock
                    .write_all(format!("event: tick\ndata: {n}\n\n").as_bytes())
                    .is_err()
                {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
    });
    url
}

fn file(url: &str, headers: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "s.toml",
        &format!("version = 1\nname = \"s\"\nurl = \"{url}\"\n\n[headers]\n{headers}"),
    );
    dir
}

#[test]
fn a_websocket_session_prints_both_directions() {
    let dir = file(&ws_server(), "");
    let path = dir.path().join("s.toml");
    let (code, out, err) = reqlite(&[
        OsStr::new("listen"),
        path.as_os_str(),
        OsStr::new("--send"),
        OsStr::new("hi"),
        OsStr::new("--send"),
        OsStr::new("bye"),
    ]);
    assert_eq!(code, Some(0), "{out}{err}");
    let lines: Vec<&str> = out.lines().map(|l| l.split_once("s ").unwrap().1).collect();
    assert_eq!(lines[0], "- open, status 101");
    assert!(
        lines.contains(&"> hi") && lines.contains(&"< echo: hi"),
        "{out}"
    );
    assert_eq!(
        lines.last(),
        Some(&"- closed by the server: 1000 done"),
        "{out}"
    );
}

#[test]
fn an_sse_stream_stops_after_count_events() {
    let dir = file(&sse_server(), "Accept = \"text/event-stream\"\n");
    let path = dir.path().join("s.toml");
    let (code, out, err) = reqlite(&[
        OsStr::new("listen"),
        path.as_os_str(),
        OsStr::new("--count"),
        OsStr::new("2"),
    ]);
    assert_eq!(code, Some(0), "{out}{err}");
    let lines: Vec<&str> = out.lines().map(|l| l.split_once("s ").unwrap().1).collect();
    assert_eq!(
        lines,
        [
            "- open, status 200",
            "< [tick] 1",
            "< [tick] 2",
            "- closed by you"
        ]
    );
}

#[test]
fn for_closes_a_quiet_stream_after_its_time() {
    let dir = file(&sse_server(), "Accept = \"text/event-stream\"\n");
    let path = dir.path().join("s.toml");
    let (code, out, err) = reqlite(&[
        OsStr::new("listen"),
        path.as_os_str(),
        OsStr::new("--for"),
        OsStr::new("1"),
    ]);
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(out.contains("< [tick] 3"), "{out}");
    assert!(err.contains("closed after 1 s"), "{err}");
}

#[test]
fn send_points_a_websocket_to_listen() {
    let dir = file("ws://127.0.0.1:9/x", "");
    let path = dir.path().join("s.toml");
    let (code, _, err) = reqlite(&[OsStr::new("send"), path.as_os_str()]);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("use `reqlite listen`"), "{err}");
}
