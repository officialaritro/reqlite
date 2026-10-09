//! gRPC mistakes the CLI catches before it connects. The calls themselves
//! are tested against a local server in the engine's tests.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
use common::{reqlite, write};
use std::ffi::OsStr;

const PROTO: &str = "syntax = \"proto3\";\npackage t;\nservice S { rpc Get(Req) returns (Req); }\nmessage Req { string id = 1; }\n";

fn call(proto: &str, message: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "s.proto", proto);
    write(
        dir.path(),
        "get.toml",
        &format!(
            "version = 4\nname = \"get\"\nurl = \"http://127.0.0.1:9\"\n\n[grpc]\nmethod = \"t.S/Get\"\nproto = \"s.proto\"\nmessage = '{message}'\n"
        ),
    );
    dir
}

#[test]
fn input_mistakes_exit_3_and_name_the_problem() {
    for (proto, message, want) in [
        ("syntax = \"proto3\";\nmessage {", "{}", "s.proto"),
        (PROTO, r#"{"idd": "1"}"#, "t.Req"),
    ] {
        let dir = call(proto, message);
        let path = dir.path().join("get.toml");
        let (code, _, err) = reqlite(&[OsStr::new("send"), path.as_os_str()]);
        assert_eq!(code, Some(3), "{err}");
        assert!(err.contains(want), "{err}");
    }
}

#[test]
fn curl_export_refuses_a_grpc_call() {
    let dir = call(PROTO, "{}");
    let path = dir.path().join("get.toml");
    let (code, _, err) = reqlite(&[OsStr::new("export"), OsStr::new("curl"), path.as_os_str()]);
    assert_eq!(code, Some(3), "{err}");
    assert!(err.contains("curl cannot make gRPC calls"), "{err}");
}
