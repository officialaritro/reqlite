//! A local HTTP server for the CLI tests.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;

/// What the server saw of one request.
pub struct Seen {
    pub method: String,
    pub path: String,
    /// Header names in lower case.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Seen {
    pub fn header(&self, name: &str) -> &str {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map_or("", |(_, v)| v.as_str())
    }
}

/// Serves on a free port; `answer` gives the status line and a JSON body.
/// Returns the base URL.
pub fn serve(answer: impl Fn(&Seen) -> (&'static str, String) + Send + 'static) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for sock in listener.incoming() {
            let mut sock = sock.unwrap();
            let mut reader = BufReader::new(sock.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut words = line.split_whitespace();
            let (method, path) = (
                words.next().unwrap().to_string(),
                words.next().unwrap().to_string(),
            );
            let mut headers = Vec::new();
            loop {
                let mut h = String::new();
                reader.read_line(&mut h).unwrap();
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                let (k, v) = h.split_once(':').unwrap();
                headers.push((k.to_ascii_lowercase(), v.trim().to_string()));
            }
            let len = headers
                .iter()
                .find(|(k, _)| k == "content-length")
                .map_or(0, |(_, v)| v.parse().unwrap());
            let mut body = vec![0; len];
            reader.read_exact(&mut body).unwrap();
            let seen = Seen {
                method,
                path,
                headers,
                body: String::from_utf8(body).unwrap(),
            };
            let (status, body) = answer(&seen);
            write!(
                sock,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    });
    base
}

pub fn write(dir: &Path, name: &str, text: &str) {
    let path = dir.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Runs `reqlite` with its own history folder: exit code, stdout, stderr.
pub fn reqlite(args: &[&std::ffi::OsStr]) -> (Option<i32>, String, String) {
    let data = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_reqlite"))
        .args(args)
        .env("REQLITE_DATA_DIR", data.path())
        .output()
        .unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}
