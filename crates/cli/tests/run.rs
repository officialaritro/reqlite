//! `reqlite run` against a local server: a login whose token the next request
//! uses, assertions that pass and fail, and the exit codes CI relies on.
// The server's helpers sit outside `#[test]` functions, where
// `allow-unwrap-in-tests` does not reach.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;

/// Answers `POST /login` with a token, and `GET /users/7` only with that token.
fn server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for sock in listener.incoming() {
            let mut sock = sock.unwrap();
            let mut reader = BufReader::new(sock.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let (mut auth, mut len) = (String::new(), 0);
            loop {
                let mut h = String::new();
                reader.read_line(&mut h).unwrap();
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                let (k, v) = h.split_once(':').unwrap();
                match k.to_ascii_lowercase().as_str() {
                    "authorization" => auth = v.trim().to_string(),
                    "content-length" => len = v.trim().parse().unwrap(),
                    _ => {}
                }
            }
            reader.read_exact(&mut vec![0; len]).unwrap();
            let (status, body) = match line.split_whitespace().nth(1).unwrap() {
                "/login" => ("200 OK", r#"{"token": "t-9", "id": 7}"#),
                "/users/7" if auth == "Bearer t-9" => ("200 OK", r#"{"name": "ada"}"#),
                _ => ("401 Unauthorized", r#"{"error": "no"}"#),
            };
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

fn write(dir: &Path, name: &str, text: &str) {
    let path = dir.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A workspace with a login and a request that needs its token.
fn flow(base: &str, user_assert: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "1 login.toml",
        &format!(
            "version = 3\nname = \"login\"\nmethod = \"POST\"\nurl = \"{base}/login\"\nassert = [\"status == 200\"]\n\n[capture]\nid = \"json $.id\"\ntoken = \"json $.token\"\n"
        ),
    );
    write(
        dir.path(),
        "2 user.toml",
        &format!(
            "version = 3\nname = \"user\"\nurl = \"{base}/users/{{{{id}}}}\"\nassert = [\"status == 200\", {user_assert:?}]\n\n[headers]\nAuthorization = \"Bearer {{{{token}}}}\"\n"
        ),
    );
    dir
}

fn run(dir: &Path) -> (Option<i32>, String) {
    let data = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_reqlite"))
        .arg("run")
        .arg(dir)
        .env("REQLITE_DATA_DIR", data.path())
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    (out.status.code(), text)
}

#[test]
fn a_captured_token_signs_the_next_request() {
    let dir = flow(&server(), "json $.name == \"ada\"");
    let (code, out) = run(dir.path());
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("1 login.toml  200"), "{out}");
    assert!(out.contains("pass  capture token = json $.token"), "{out}");
    assert!(out.contains("2 user.toml  200"), "{out}");
    assert!(out.contains("pass  json $.name == \"ada\""), "{out}");
    assert!(
        out.contains("2 requests, 0 did not complete; 5 checks, 0 failed"),
        "{out}"
    );
}

#[test]
fn a_failed_assertion_exits_4_and_says_what_came_back() {
    let dir = flow(&server(), "json $.name == \"bob\"");
    let (code, out) = run(dir.path());
    assert_eq!(code, Some(4), "{out}");
    assert!(
        out.contains("FAIL  json $.name == \"bob\"  (got \"ada\")"),
        "{out}"
    );
    assert!(out.contains("5 checks, 1 failed"), "{out}");
}

#[test]
fn a_bad_file_fails_and_the_run_goes_on() {
    let base = server();
    let dir = flow(&base, "json $.name == \"ada\"");
    write(
        dir.path(),
        "0 broken.toml",
        "version = 3\nname = \"x\"\nurl = \"\"\n",
    );
    let (code, out) = run(dir.path());
    assert_eq!(code, Some(4), "{out}");
    assert!(out.contains("0 broken.toml  FAIL"), "{out}");
    assert!(out.contains("pass  json $.name == \"ada\""), "{out}");
    assert!(out.contains("3 requests, 1 did not complete"), "{out}");
}

#[test]
fn send_checks_its_assertions_too() {
    let dir = flow(&server(), "json $.name == \"ada\"");
    let data = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_reqlite"))
        .arg("send")
        .arg(dir.path().join("1 login.toml"))
        .env("REQLITE_DATA_DIR", data.path())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{err}");
    assert!(err.contains("pass  status == 200"), "{err}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        r#"{"token": "t-9", "id": 7}"#
    );

    // The user request has no captured values without a run before it.
    let user = dir.path().join("2 user.toml");
    let text = std::fs::read_to_string(&user).unwrap();
    std::fs::write(
        &user,
        text.replace("{{id}}", "7").replace("{{token}}", "wrong"),
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_reqlite"))
        .arg("send")
        .arg(&user)
        .env("REQLITE_DATA_DIR", data.path())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(4), "{err}");
    assert!(err.contains("FAIL  status == 200  (got 401)"), "{err}");
    assert!(err.contains("2 of 2 checks failed"), "{err}");
}
