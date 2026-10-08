//! gRPC against a local server: unary calls from a .proto file and by
//! reflection, metadata, error statuses, and messages that do not fit.
// The server's helpers sit outside `#[test]` functions, where
// `allow-unwrap-in-tests` does not reach.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use bytes::Bytes;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::Frame;
use hyper_util::rt::{TokioExecutor, TokioIo};
use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage, Value};
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

type Reply = hyper::Response<
    StreamBody<futures_util::stream::Iter<std::vec::IntoIter<Result<Frame<Bytes>, Infallible>>>>,
>;

/// A gRPC answer: the messages, then `grpc-status` and `grpc-message`.
fn answer(messages: Vec<Vec<u8>>, status: u32, message: &str) -> Reply {
    let mut trailers = http::HeaderMap::new();
    trailers.insert("grpc-status", status.to_string().parse().unwrap());
    if !message.is_empty() {
        trailers.insert("grpc-message", message.parse().unwrap());
    }
    let mut frames: Vec<Result<Frame<Bytes>, Infallible>> =
        messages.iter().map(|m| Ok(Frame::data(frame(m)))).collect();
    frames.push(Ok(Frame::trailers(trailers)));
    hyper::Response::builder()
        .header("content-type", "application/grpc")
        .header("x-served-by", "test")
        .body(StreamBody::new(futures_util::stream::iter(frames)))
        .unwrap()
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
                    let body = req.into_body().collect().await.unwrap().to_bytes();
                    let msg = body.slice(5..);
                    Ok::<_, Infallible>(match path.as_str() {
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
        e.contains("has no method Hello; it has SayHello, Watch"),
        "{e}"
    );
    let e = err("test.v1.Greeter/Watch", "{}", true).await.to_string();
    assert!(e.contains("streaming method"), "{e}");
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
