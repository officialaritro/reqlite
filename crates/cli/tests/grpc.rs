//! gRPC mistakes the CLI catches before it connects, and `reqlite listen`
//! against a local client-streaming server. The calls themselves are tested
//! in the engine's tests.
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

/// `Req { id }` on the wire: field 1, a string.
fn req(id: &str) -> Vec<u8> {
    let mut out = vec![0x0a, id.len() as u8];
    out.extend_from_slice(id.as_bytes());
    out
}

/// Serves `t.S/Join`, client streaming: its answer joins the ids of all the
/// messages, and it answers only when the client is done sending.
fn join_server() -> String {
    use http_body_util::{BodyExt, Full};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            loop {
                let (sock, _) = listener.accept().await.unwrap();
                let serve = hyper::service::service_fn(
                    |req: hyper::Request<hyper::body::Incoming>| async move {
                        let body = req.into_body().collect().await.unwrap().to_bytes();
                        let mut ids = Vec::new();
                        let mut rest = &body[..];
                        while rest.len() >= 5 {
                            let len =
                                u32::from_be_bytes([rest[1], rest[2], rest[3], rest[4]]) as usize;
                            let msg = &rest[5..5 + len];
                            ids.push(String::from_utf8_lossy(&msg[2..]).into_owned());
                            rest = &rest[5 + len..];
                        }
                        let answer = req_frame(&ids.join("+"));
                        let mut trailers = http::HeaderMap::new();
                        trailers.insert("grpc-status", "0".parse().unwrap());
                        let body = Full::new(bytes::Bytes::from(answer))
                            .with_trailers(async { Some(Ok(trailers)) });
                        Ok::<_, std::convert::Infallible>(
                            hyper::Response::builder()
                                .header("content-type", "application/grpc")
                                .body(body)
                                .unwrap(),
                        )
                    },
                );
                tokio::spawn(
                    hyper::server::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                        .serve_connection(hyper_util::rt::TokioIo::new(sock), serve),
                );
            }
        });
    });
    url
}

fn req_frame(id: &str) -> Vec<u8> {
    let m = req(id);
    let mut out = vec![0];
    out.extend_from_slice(&(m.len() as u32).to_be_bytes());
    out.extend_from_slice(&m);
    out
}

#[test]
fn listen_ends_a_grpc_call_s_sending_side_at_the_end_of_stdin() {
    let proto = "syntax = \"proto3\";\npackage t;\nservice S { rpc Join(stream Req) returns (Req); }\nmessage Req { string id = 1; }\n";
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "s.proto", proto);
    write(
        dir.path(),
        "join.toml",
        &format!(
            "version = 4\nname = \"join\"\nurl = \"{}\"\n\n[grpc]\nmethod = \"t.S/Join\"\nproto = \"s.proto\"\nmessage = '[{{\"id\": \"a\"}}]'\n",
            join_server()
        ),
    );
    let path = dir.path().join("join.toml");
    // The test's stdin is empty, so the call's sending side ends at once,
    // after the file's message and the one from --send. --for turns a call
    // that never ends into a failed assertion instead of a hung test.
    let (code, out, err) = reqlite(&[
        OsStr::new("listen"),
        path.as_os_str(),
        OsStr::new("--send"),
        OsStr::new(r#"{"id": "b"}"#),
        OsStr::new("--for"),
        OsStr::new("10"),
    ]);
    assert_eq!(code, Some(0), "{out}{err}");
    let lines: Vec<&str> = out.lines().map(|l| l.split_once("s ").unwrap().1).collect();
    assert_eq!(
        lines,
        [
            "- Join is client streaming",
            r#"> {"id":"a"}"#,
            r#"> {"id":"b"}"#,
            "- done sending",
            "- open, status 200",
            r#"< {"id":"a+b"}"#,
            "- ended with status 0 OK",
        ],
        "{out}"
    );
}
