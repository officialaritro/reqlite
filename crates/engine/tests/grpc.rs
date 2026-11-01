//! gRPC against a local server: calls of every kind from a .proto file and
//! by reflection, metadata, error statuses, and messages that do not fit.
// The server's helpers sit outside `#[test]` functions, where
// `allow-unwrap-in-tests` does not reach.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::Frame;
use hyper_util::rt::{TokioExecutor, TokioIo};
use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage, Value};
use reqlite_engine::stream::{Event, Timed};
use reqlite_engine::{SendError, client, resolve_in, send};
use reqlite_format::Environment;
use std::convert::Infallible;
use std::path::Path;

const COMMON: &str = r#"syntax = "proto3";
package test.v1;
message Name { string first = 1; }
"#;

const GREETER: &str = r#"syntax = "proto3";
package test.v1;
import "common.proto";
service Greeter {
  rpc SayHello(HelloRequest) returns (HelloReply);
  rpc Watch(HelloRequest) returns (stream HelloReply);
  rpc Collect(stream HelloRequest) returns (HelloReply);
  rpc Chat(stream HelloRequest) returns (stream HelloReply);
}
message HelloRequest { Name name = 1; int64 times = 2; }
message HelloReply { string text = 1; repeated string tags = 2; int64 big = 3; bool flag = 4; }
"#;

fn protos(dir: &Path) {
    std::fs::write(dir.join("common.proto"), COMMON).unwrap();
    std::fs::write(dir.join("greeter.proto"), GREETER).unwrap();
}

#[derive(Clone, PartialEq, Message)]
struct ReflectionRequest {
    #[prost(string, optional, tag = "3")]
    file_by_filename: Option<String>,
    #[prost(string, optional, tag = "4")]
    file_containing_symbol: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
struct ReflectionResponse {
    #[prost(message, optional, tag = "4")]
    file_descriptor_response: Option<FileDescriptorResponse>,
}

#[derive(Clone, PartialEq, Message)]
struct FileDescriptorResponse {
    #[prost(bytes = "vec", repeated, tag = "1")]
    file_descriptor_proto: Vec<Vec<u8>>,
}

fn frame(m: &[u8]) -> Bytes {
    let mut out = vec![0];
    out.extend_from_slice(&(m.len() as u32).to_be_bytes());
    out.extend_from_slice(m);
    Bytes::from(out)
}

type Reply = hyper::Response<BoxBody<Bytes, Infallible>>;

fn trailers(status: u32, message: &str) -> http::HeaderMap {
    let mut trailers = http::HeaderMap::new();
    trailers.insert("grpc-status", status.to_string().parse().unwrap());
    if !message.is_empty() {
        trailers.insert("grpc-message", message.parse().unwrap());
    }
    trailers
}

fn reply(frames: BoxBody<Bytes, Infallible>) -> Reply {
    hyper::Response::builder()
        .header("content-type", "application/grpc")
        .header("x-served-by", "test")
        .body(frames)
        .unwrap()
}

/// A gRPC answer: the messages, then `grpc-status` and `grpc-message`.
fn answer(messages: Vec<Vec<u8>>, status: u32, message: &str) -> Reply {
    let mut frames: Vec<Result<Frame<Bytes>, Infallible>> =
        messages.iter().map(|m| Ok(Frame::data(frame(m)))).collect();
    frames.push(Ok(Frame::trailers(trailers(status, message))));
    reply(StreamBody::new(futures_util::stream::iter(frames)).boxed())
}

/// The messages in a request body.
fn messages(mut body: Bytes) -> Vec<Bytes> {
    let mut out = Vec::new();
    while body.len() >= 5 {
        let len = u32::from_be_bytes([body[1], body[2], body[3], body[4]]) as usize;
        out.push(body.slice(5..5 + len));
        body = body.slice(5 + len..);
    }
    out
}

/// The first name in a HelloRequest.
fn first_name(pool: &DescriptorPool, msg: Bytes) -> String {
    let input = pool.get_message_by_name("test.v1.HelloRequest").unwrap();
    let req = DynamicMessage::decode(input, msg).unwrap();
    match req.get_field_by_name("name").as_deref() {
        Some(Value::Message(n)) => n
            .get_field_by_name("first")
            .unwrap()
            .as_str()
            .unwrap()
            .to_string(),
        _ => String::new(),
    }
}

fn hello(pool: &DescriptorPool, text: &str) -> Vec<u8> {
    let output = pool.get_message_by_name("test.v1.HelloReply").unwrap();
    let mut reply = DynamicMessage::new(output);
    reply.set_field_by_name("text", Value::String(text.to_string()));
    reply.encode_to_vec()
}

/// Chat answers each message the moment it arrives, so a client sees each
/// answer before it is done sending.
fn chat(pool: DescriptorPool, body: hyper::body::Incoming) -> Reply {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Frame<Bytes>>();
    tokio::spawn(async move {
        let mut body = body;
        let mut buf = Vec::new();
        while let Some(Ok(part)) = body.frame().await {
            let Ok(chunk) = part.into_data() else {
                continue;
            };
            buf.extend_from_slice(&chunk);
            while buf.len() >= 5 {
                let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
                if buf.len() < 5 + len {
                    break;
                }
                let msg = Bytes::copy_from_slice(&buf[5..5 + len]);
                buf.drain(..5 + len);
                let name = first_name(&pool, msg);
                tx.send(Frame::data(frame(&hello(&pool, &format!("hi {name}")))))
                    .ok();
            }
        }
        tx.send(Frame::trailers(trailers(0, ""))).ok();
    });
    let frames = futures_util::stream::unfold(rx, async |mut rx| {
        rx.recv().await.map(|f| (Ok::<_, Infallible>(f), rx))
    });
    reply(StreamBody::new(frames).boxed())
}

/// Serves Greeter/SayHello over plain HTTP/2, and reflection when asked to.
/// Reflection sends only the file asked for, so the client must ask for
/// its import.
async fn server(reflection: bool) -> String {
    let dir = tempfile::tempdir().unwrap();
    protos(dir.path());
    let set = protox::compile(["greeter.proto"], [dir.path()]).unwrap();
    let pool = DescriptorPool::from_file_descriptor_set(set.clone()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            let (pool, set) = (pool.clone(), set.clone());
            let handle = move |req: hyper::Request<hyper::body::Incoming>| {
                let (pool, set) = (pool.clone(), set.clone());
                async move {
                    let path = req.uri().path().to_string();
                    let team = req.headers().get("x-team").map(|v| v.as_bytes().to_vec());
                    if path == "/test.v1.Greeter/Chat" {
                        return Ok::<_, Infallible>(chat(pool, req.into_body()));
                    }
                    let body = req.into_body().collect().await.unwrap().to_bytes();
                    let all = messages(body.clone());
                    let msg = body.slice(5.min(body.len())..);
                    Ok::<_, Infallible>(match path.as_str() {
                        "/test.v1.Greeter/Watch" => {
                            let name = first_name(&pool, all[0].clone());
                            let n = if name == "nobody" { 2 } else { 3 };
                            let replies = (1..=n)
                                .map(|i| hello(&pool, &format!("{name} {i}")))
                                .collect();
                            if name == "nobody" {
                                answer(replies, 5, "no%20such%20user")
                            } else {
                                answer(replies, 0, "")
                            }
                        }
                        "/test.v1.Greeter/Collect" => {
                            let names: Vec<String> =
                                all.into_iter().map(|m| first_name(&pool, m)).collect();
                            answer(
                                vec![hello(&pool, &format!("hello {}", names.join(", ")))],
                                0,
                                "",
                            )
                        }
                        "/test.v1.Greeter/SayHello" => {
                            if team.as_deref() != Some(b"a") {
                                return Ok(answer(vec![], 7, "x-team%20is%20missing"));
                            }
                            let input = pool.get_message_by_name("test.v1.HelloRequest").unwrap();
                            let req = DynamicMessage::decode(input, msg).unwrap();
                            let first = match req.get_field_by_name("name").as_deref() {
                                Some(Value::Message(n)) => n
                                    .get_field_by_name("first")
                                    .unwrap()
                                    .as_str()
                                    .unwrap()
                                    .to_string(),
                                _ => String::new(),
                            };
                            if first == "nobody" {
                                return Ok(answer(vec![], 5, "no%20such%20user"));
                            }
                            let times = req.get_field_by_name("times").unwrap().as_i64().unwrap();
                            let output = pool.get_message_by_name("test.v1.HelloReply").unwrap();
                            let mut reply = DynamicMessage::new(output);
                            reply
                                .set_field_by_name("text", Value::String(format!("hello {first}")));
                            reply.set_field_by_name(
                                "tags",
                                Value::List(vec![
                                    Value::String("a".into()),
                                    Value::String("b".into()),
                                ]),
                            );
                            reply.set_field_by_name("big", Value::I64(times * 1_000_000_000_000));
                            answer(vec![reply.encode_to_vec()], 0, "")
                        }
                        "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo"
                            if reflection =>
                        {
                            let ask = ReflectionRequest::decode(msg).unwrap();
                            let name = match (ask.file_by_filename, ask.file_containing_symbol) {
                                (Some(f), _) => f,
                                (None, Some(s)) if s == "test.v1.Greeter" => "greeter.proto".into(),
                                _ => String::new(),
                            };
                            let files = set
                                .file
                                .iter()
                                .filter(|f| f.name() == name)
                                .map(|f| f.encode_to_vec())
                                .collect();
                            let resp = ReflectionResponse {
                                file_descriptor_response: Some(FileDescriptorResponse {
                                    file_descriptor_proto: files,
                                }),
                            };
                            answer(vec![resp.encode_to_vec()], 0, "")
                        }
                        _ => answer(vec![], 12, ""),
                    })
                }
            };
            tokio::spawn(
                hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                    .serve_connection(TokioIo::new(sock), hyper::service::service_fn(handle)),
            );
        }
    });
    url
}

/// A request file in `dir` for `method` with `message`, from the .proto
/// files there when `proto` is set.
fn call(
    dir: &Path,
    url: &str,
    method: &str,
    message: &str,
    proto: bool,
) -> reqlite_engine::Resolved {
    let proto = if proto {
        "proto = \"greeter.proto\"\n"
    } else {
        ""
    };
    let text = format!(
        "version = 4\nname = \"g\"\nurl = \"{url}\"\n\n[headers]\nx-team = \"a\"\n\n[grpc]\nmethod = \"{method}\"\n{proto}message = '{message}'\n"
    );
    let req = reqlite_format::parse(&text).unwrap();
    resolve_in(&req, &Environment::default(), dir).unwrap()
}

async fn json(req: &reqlite_engine::Resolved) -> (serde_json::Value, String) {
    let resp = send(&client().unwrap(), req).await.unwrap();
    let mut body = Vec::new();
    std::io::Read::read_to_end(&mut resp.body.reader().unwrap(), &mut body).unwrap();
    let status = resp
        .headers
        .iter()
        .find(|(k, _)| k == "grpc-status")
        .map(|(_, v)| String::from_utf8_lossy(v).into_owned())
        .unwrap();
    assert!(resp.headers.iter().any(|(k, _)| k == "x-served-by"));
    (serde_json::from_slice(&body).unwrap(), status)
}

const HELLO: &str = "test.v1.Greeter/SayHello";

#[tokio::test]
async fn a_unary_call_from_a_proto_file_answers_in_json() {
    let dir = tempfile::tempdir().unwrap();
    protos(dir.path());
    let url = server(false).await;
    let req = call(
        dir.path(),
        &url,
        HELLO,
        r#"{"name": {"first": "ada"}, "times": "3"}"#,
        true,
    );
    let (body, status) = json(&req).await;
    assert_eq!(status, "0");
    assert_eq!(
        body,
        serde_json::json!({"text": "hello ada", "tags": ["a", "b"], "big": "3000000000000", "flag": false})
    );
}

#[tokio::test]
async fn reflection_finds_the_schema_and_its_imports() {
    let url = server(true).await;
    let dir = tempfile::tempdir().unwrap();
    let req = call(
        dir.path(),
        &url,
        HELLO,
        r#"{"name": {"first": "lin"}}"#,
        false,
    );
    let (body, _) = json(&req).await;
    assert_eq!(body["text"], "hello lin");
}

#[tokio::test]
async fn a_refused_call_is_a_response_with_its_status() {
    let dir = tempfile::tempdir().unwrap();
    protos(dir.path());
    let url = server(false).await;
    let req = call(
        dir.path(),
        &url,
        HELLO,
        r#"{"name": {"first": "nobody"}}"#,
        true,
    );
    let (body, status) = json(&req).await;
    assert_eq!(status, "5");
    assert_eq!(
        body,
        serde_json::json!({"code": 5, "status": "NOT_FOUND", "message": "no such user"})
    );
}

#[tokio::test]
async fn mistakes_name_what_is_wrong() {
    let dir = tempfile::tempdir().unwrap();
    protos(dir.path());
    let url = server(false).await;
    let err = |method: &str, message: &str, proto: bool| {
        let req = call(dir.path(), &url, method, message, proto);
        async move { send(&client().unwrap(), &req).await.unwrap_err() }
    };
    let e = err(HELLO, r#"{"nmae": {}}"#, true).await;
    assert!(
        matches!(&e, SendError::GrpcMessage(m) if m.contains("nmae")),
        "{e}"
    );
    let e = err("test.v1.Greeter/Hello", "{}", true).await.to_string();
    assert!(
        e.contains("has no method Hello; it has SayHello, Watch, Collect, Chat"),
        "{e}"
    );
    let e = err(HELLO, r#"[{}, {}]"#, true).await.to_string();
    assert!(e.contains("SayHello is unary and takes one message"), "{e}");
    let e = err("test.v1.Greeter/Collect", r#"[{}, {"nmae": 1}]"#, true)
        .await
        .to_string();
    assert!(e.contains("message 2:") && e.contains("nmae"), "{e}");
    let e = err(HELLO, "{}", false).await.to_string();
    assert!(e.contains("set proto in [grpc]"), "{e}");
    std::fs::write(
        dir.path().join("greeter.proto"),
        "syntax = \"proto3\";\nmessage {",
    )
    .unwrap();
    let e = err(HELLO, "{}", true).await;
    assert!(matches!(e, SendError::Proto { .. }), "{e}");
}

#[tokio::test]
async fn a_server_stream_comes_back_as_an_array() {
    let dir = tempfile::tempdir().unwrap();
    protos(dir.path());
    let url = server(false).await;
    let watch = "test.v1.Greeter/Watch";
    let req = call(
        dir.path(),
        &url,
        watch,
        r#"{"name": {"first": "ada"}}"#,
        true,
    );
    let (body, status) = json(&req).await;
    assert_eq!(status, "0");
    let texts: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, ["ada 1", "ada 2", "ada 3"]);

    // A stream that fails keeps what came before the failure.
    let req = call(
        dir.path(),
        &url,
        watch,
        r#"{"name": {"first": "nobody"}}"#,
        true,
    );
    let (body, status) = json(&req).await;
    assert_eq!(status, "5");
    assert_eq!(body["status"], "NOT_FOUND");
    assert_eq!(body["messages"][1]["text"], "nobody 2");
}

#[tokio::test]
async fn a_client_stream_sends_each_message_in_the_array() {
    let dir = tempfile::tempdir().unwrap();
    protos(dir.path());
    let url = server(false).await;
    let collect = "test.v1.Greeter/Collect";
    let many = r#"[{"name": {"first": "a"}}, {"name": {"first": "b"}}, {"name": {"first": "c"}}]"#;
    let (body, _) = json(&call(dir.path(), &url, collect, many, true)).await;
    assert_eq!(body["text"], "hello a, b, c");
    let one = r#"{"name": {"first": "solo"}}"#;
    let (body, _) = json(&call(dir.path(), &url, collect, one, true)).await;
    assert_eq!(body["text"], "hello solo");
}

#[tokio::test]
async fn a_bidirectional_send_collects_every_answer() {
    let dir = tempfile::tempdir().unwrap();
    protos(dir.path());
    let url = server(false).await;
    let many = r#"[{"name": {"first": "x"}}, {"name": {"first": "y"}}]"#;
    let (body, _) = json(&call(dir.path(), &url, "test.v1.Greeter/Chat", many, true)).await;
    assert_eq!(
        body,
        serde_json::json!([{"text": "hi x", "tags": [], "big": "0", "flag": false}, {"text": "hi y", "tags": [], "big": "0", "flag": false}])
    );
}

/// Runs `req` as a live session. `script` is called with each event; what
/// it returns is typed next, and `None` closes the input.
async fn live(
    req: &reqlite_engine::Resolved,
    mut script: impl FnMut(&Event) -> Option<Option<String>>,
) -> (Vec<Event>, Result<(), String>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut tx = Some(tx);
    let mut events = Vec::new();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        reqlite_engine::stream::open(req, None, rx, |t: Timed| {
            match script(&t.event) {
                Some(Some(text)) => drop(tx.as_ref().map(|tx| tx.send(text))),
                Some(None) => tx = None,
                None => {}
            }
            events.push(t.event);
            true
        }),
    )
    .await
    .expect("the session hung");
    (events, result)
}

fn shown(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .map(|e| match e {
            Event::Open { status } => format!("open {status}"),
            Event::Received { data, .. } => {
                let v: serde_json::Value = serde_json::from_str(data).unwrap();
                format!("< {}", v["text"].as_str().unwrap())
            }
            Event::Sent { data } => {
                let v: serde_json::Value = serde_json::from_str(data).unwrap();
                format!("> {}", v["name"]["first"].as_str().unwrap_or(""))
            }
            Event::Note { text } => format!("- {text}"),
            Event::Closed { reason } => format!("closed: {reason}"),
        })
        .collect()
}

#[tokio::test]
async fn a_live_bidirectional_call_answers_before_the_client_is_done() {
    let dir = tempfile::tempdir().unwrap();
    protos(dir.path());
    let url = server(false).await;
    let req = call(
        dir.path(),
        &url,
        "test.v1.Greeter/Chat",
        r#"{"name": {"first": "one"}}"#,
        true,
    );
    // Each answer prompts the next message; the second answer ends the input.
    let (events, result) = live(&req, |e| match e {
        Event::Received { data, .. } if data.contains("hi one") => {
            Some(Some(r#"[{"name": {"first": "two"}}]"#.into()))
        }
        Event::Received { data, .. } if data.contains("hi two") => Some(None),
        _ => None,
    })
    .await;
    result.unwrap();
    let shown = shown(&events);
    assert_eq!(
        shown,
        [
            "- Chat is bidirectional streaming",
            "> one",
            "open 200",
            "< hi one",
            "> two",
            "< hi two",
            "- done sending",
            "closed: ended with status 0 OK",
        ]
    );
}

#[tokio::test]
async fn a_live_call_keeps_bad_typed_messages_back_and_goes_on() {
    let dir = tempfile::tempdir().unwrap();
    protos(dir.path());
    let url = server(false).await;
    let req = call(dir.path(), &url, "test.v1.Greeter/Chat", "", true);
    let mut step = 0;
    let (events, result) = live(&req, |e| {
        if !matches!(e, Event::Note { .. } | Event::Received { .. }) {
            return None;
        }
        step += 1;
        match step {
            1 => Some(Some(r#"{"nmae": 1}"#.into())),
            2 => Some(Some(r#"{"name": {"first": "ok"}}"#.into())),
            _ => Some(None),
        }
    })
    .await;
    result.unwrap();
    let shown = shown(&events);
    assert!(
        shown
            .iter()
            .any(|l| l.starts_with("- not sent:") && l.contains("nmae")),
        "{shown:?}"
    );
    assert!(shown.contains(&"< hi ok".to_string()), "{shown:?}");
    assert_eq!(shown.last().unwrap(), "closed: ended with status 0 OK");
}

#[tokio::test]
async fn a_live_server_stream_sends_once_and_shows_each_answer() {
    let dir = tempfile::tempdir().unwrap();
    protos(dir.path());
    let url = server(false).await;
    let req = call(
        dir.path(),
        &url,
        "test.v1.Greeter/Watch",
        r#"{"name": {"first": "lin"}}"#,
        true,
    );
    let (events, result) = live(&req, |_| None).await;
    result.unwrap();
    assert_eq!(
        shown(&events),
        [
            "- Watch is server streaming",
            "> lin",
            "open 200",
            "< lin 1",
            "< lin 2",
            "< lin 3",
            "closed: ended with status 0 OK",
        ]
    );
}
