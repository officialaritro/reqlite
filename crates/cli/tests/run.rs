//! `reqlite run` against a local server: a login whose token the next request
//! uses, assertions that pass and fail, and the exit codes CI relies on.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
use common::{reqlite, serve, write};
use std::ffi::OsStr;
use std::path::Path;

/// Answers `POST /login` with a token, and `GET /users/7` only with that token.
fn server() -> String {
    serve(|seen| match seen.path.as_str() {
        "/login" => ("200 OK", r#"{"token": "t-9", "id": 7}"#.into()),
        "/users/7" if seen.header("authorization") == "Bearer t-9" => {
            ("200 OK", r#"{"name": "ada"}"#.into())
        }
        _ => ("401 Unauthorized", r#"{"error": "no"}"#.into()),
    })
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
    let (code, out, err) = reqlite(&[OsStr::new("run"), dir.as_os_str()]);
    (code, out + &err)
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
    let dir = flow(&server(), "json $.name == \"ada\"");
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
    let login = dir.path().join("1 login.toml");
    let (code, out, err) = reqlite(&[OsStr::new("send"), login.as_os_str()]);
    assert_eq!(code, Some(0), "{err}");
    assert!(err.contains("pass  status == 200"), "{err}");
    assert_eq!(out, r#"{"token": "t-9", "id": 7}"#);

    // The user request has no captured values without a run before it.
    let user = dir.path().join("2 user.toml");
    let text = std::fs::read_to_string(&user).unwrap();
    std::fs::write(
        &user,
        text.replace("{{id}}", "7").replace("{{token}}", "wrong"),
    )
    .unwrap();
    let (code, _, err) = reqlite(&[OsStr::new("send"), user.as_os_str()]);
    assert_eq!(code, Some(4), "{err}");
    assert!(err.contains("FAIL  status == 200  (got 401)"), "{err}");
    assert!(err.contains("2 of 2 checks failed"), "{err}");
}
