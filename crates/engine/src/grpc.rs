//! gRPC calls over HTTP/2, with messages written and shown as JSON.
//!
//! The schema comes from a `.proto` file, compiled when the call is made, or
//! from the server by reflection. A call is a POST to `/Service/Method` with
//! each message framed as one flag byte, a 4-byte length and the bytes; the
//! outcome is in the `grpc-status` trailer.

use crate::resolve::GrpcCall;
use crate::{Body, Resolved, Response, SendBody, SendError, Store};
use bytes::{Bytes, BytesMut};
use http_body_util::{BodyExt, Full};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage, MethodDescriptor, SerializeOptions};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// How long a connect may take, and the longest quiet time in an answer.
const CONNECT: Duration = Duration::from_secs(10);
const IDLE: Duration = Duration::from_secs(30);

type Http = Client<hyper_rustls::HttpsConnector<HttpConnector>, Full<Bytes>>;

/// An HTTP/2 client: TLS for `https://`, plain HTTP/2 for `http://`.
fn client() -> Result<Http, SendError> {
    let mut http = HttpConnector::new();
    http.set_connect_timeout(Some(CONNECT));
    http.enforce_http(false);
    let tls = crate::stream::tls_config().map_err(SendError::Grpc)?;
    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config((*tls).clone())
        .https_or_http()
        .enable_http2()
        .wrap_connector(http);
    Ok(Client::builder(TokioExecutor::new())
        .http2_only(true)
        .build(https))
}

/// The answer to one call: its messages, and its headers and trailers.
struct Answer {
    http_status: u16,
    headers: Vec<(String, Vec<u8>)>,
    messages: Vec<Bytes>,
    status: i32,
    message: String,
}

/// Sends `messages` to `url` and reads the whole answer.
async fn call(
    http: &Http,
    url: &str,
    metadata: &[(String, String)],
    messages: &[Vec<u8>],
) -> Result<Answer, SendError> {
    let mut body = BytesMut::new();
    for m in messages {
        body.extend_from_slice(&frame(m));
    }
    let mut builder = http::Request::post(url)
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .header("grpc-accept-encoding", "identity")
        .header("user-agent", concat!("reqlite/", env!("CARGO_PKG_VERSION")));
    for (k, v) in metadata {
        builder = builder.header(k.to_ascii_lowercase(), v);
    }
    let request = builder
        .body(Full::new(body.freeze()))
        .map_err(|e| SendError::Grpc(format!("bad request: {e}")))?;
    let wait = |what: &str| SendError::Grpc(format!("no {what} within {} s", IDLE.as_secs()));
    let resp = tokio::time::timeout(IDLE, http.request(request))
        .await
        .map_err(|_elapsed| wait("answer"))?
        .map_err(|e| SendError::Grpc(format!("cannot reach {url}: {}", crate::chain(&e))))?;
    let http_status = resp.status().as_u16();
    let mut headers: Vec<(String, Vec<u8>)> = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.as_bytes().to_vec()))
        .collect();
    let mut body = resp.into_body();
    let mut data = BytesMut::new();
    while let Some(frame) = tokio::time::timeout(IDLE, body.frame())
        .await
        .map_err(|_elapsed| wait("data"))?
    {
        let frame = frame.map_err(|e| SendError::Grpc(format!("the answer broke off: {e}")))?;
        match frame.into_data() {
            Ok(chunk) => data.extend_from_slice(&chunk),
            Err(frame) => {
                if let Ok(trailers) = frame.into_trailers() {
                    headers.extend(
                        trailers
                            .iter()
                            .map(|(k, v)| (k.to_string(), v.as_bytes().to_vec())),
                    );
                }
            }
        }
    }
    let field = |name: &str| {
        headers
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| String::from_utf8_lossy(v).into_owned())
    };
    let status = match field("grpc-status") {
        Some(s) => s.trim().parse().unwrap_or(2),
        None if http_status == 200 => {
            return Err(SendError::Grpc("the answer has no grpc-status".into()));
        }
        None => {
            return Err(SendError::Grpc(format!(
                "the server answered HTTP {http_status}; is this a gRPC server?"
            )));
        }
    };
    let message = field("grpc-message")
        .map(|m| percent_decode(&m))
        .unwrap_or_default();
    Ok(Answer {
        http_status,
        headers,
        messages: unframe(data.freeze())?,
        status,
        message,
    })
}

/// One message on the wire: not compressed, its length, its bytes.
fn frame(message: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(message.len() + 5);
    out.push(0);
    out.extend_from_slice(&(message.len() as u32).to_be_bytes());
    out.extend_from_slice(message);
    out
}

fn unframe(mut data: Bytes) -> Result<Vec<Bytes>, SendError> {
    let mut out = Vec::new();
    while !data.is_empty() {
        if data.len() < 5 {
            return Err(SendError::Grpc("a message frame is cut short".into()));
        }
        if data[0] != 0 {
            return Err(SendError::Grpc(
                "the server sent a compressed message, which Reqlite does not read".into(),
            ));
        }
        let len = u32::from_be_bytes([data[1], data[2], data[3], data[4]]) as usize;
        if data.len() < 5 + len {
            return Err(SendError::Grpc("a message is cut short".into()));
        }
        out.push(data.slice(5..5 + len));
        data = data.slice(5 + len..);
    }
    Ok(out)
}

/// `grpc-message` is percent-encoded.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        match (
            b[i],
            b.get(i + 1).and_then(|c| hex(*c)),
            b.get(i + 2).and_then(|c| hex(*c)),
        ) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (c, _, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The name of a gRPC status code.
pub fn status_name(code: i32) -> &'static str {
    const NAMES: [&str; 17] = [
        "OK",
        "CANCELLED",
        "UNKNOWN",
        "INVALID_ARGUMENT",
        "DEADLINE_EXCEEDED",
        "NOT_FOUND",
        "ALREADY_EXISTS",
        "PERMISSION_DENIED",
        "RESOURCE_EXHAUSTED",
        "FAILED_PRECONDITION",
        "ABORTED",
        "OUT_OF_RANGE",
        "UNIMPLEMENTED",
        "INTERNAL",
        "UNAVAILABLE",
        "DATA_LOSS",
        "UNAUTHENTICATED",
    ];
    usize::try_from(code)
        .ok()
        .and_then(|i| NAMES.get(i))
        .copied()
        .unwrap_or("UNKNOWN")
}

/// The method `call` names, from its `.proto` file or by reflection.
async fn method(
    http: &Http,
    base: &str,
    call: &GrpcCall,
    metadata: &[(String, String)],
) -> Result<MethodDescriptor, SendError> {
    let pool = match &call.proto {
        Some(path) => {
            let dir = path.parent().unwrap_or(std::path::Path::new("."));
            let files = protox::compile([path], [dir]).map_err(|e| SendError::Proto {
                path: path.clone(),
                reason: e.to_string(),
            })?;
            DescriptorPool::from_file_descriptor_set(files).map_err(|e| SendError::Proto {
                path: path.clone(),
                reason: e.to_string(),
            })?
        }
        None => reflect(http, base, &call.service, metadata).await?,
    };
    let service = pool
        .get_service_by_name(&call.service)
        .ok_or_else(|| SendError::Grpc(format!("the schema has no service {}", call.service)))?;
    let found = service.methods().find(|m| m.name() == call.method);
    found.ok_or_else(|| {
        let names: Vec<String> = service.methods().map(|m| m.name().to_string()).collect();
        SendError::Grpc(format!(
            "{} has no method {}; it has {}",
            call.service,
            call.method,
            names.join(", ")
        ))
    })
}

// The server reflection messages, as far as Reqlite reads them.
#[derive(Clone, PartialEq, Message)]
struct ReflectionRequest {
    #[prost(string, tag = "1")]
    host: String,
    #[prost(string, optional, tag = "3")]
    file_by_filename: Option<String>,
    #[prost(string, optional, tag = "4")]
    file_containing_symbol: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
struct ReflectionResponse {
    #[prost(message, optional, tag = "4")]
    file_descriptor_response: Option<FileDescriptorResponse>,
    #[prost(message, optional, tag = "7")]
    error_response: Option<ErrorResponse>,
}

#[derive(Clone, PartialEq, Message)]
struct FileDescriptorResponse {
    #[prost(bytes = "vec", repeated, tag = "1")]
    file_descriptor_proto: Vec<Vec<u8>>,
}

#[derive(Clone, PartialEq, Message)]
struct ErrorResponse {
    #[prost(int32, tag = "1")]
    error_code: i32,
    #[prost(string, tag = "2")]
    error_message: String,
}

/// Asks the server for the files that define `service`, and the files those
/// import, by server reflection: version 1 first, then v1alpha.
async fn reflect(
    http: &Http,
    base: &str,
    service: &str,
    metadata: &[(String, String)],
) -> Result<DescriptorPool, SendError> {
    let mut path = "grpc.reflection.v1.ServerReflection/ServerReflectionInfo";
    let ask = async |path: &str, req: ReflectionRequest| -> Result<Answer, SendError> {
        call(
            http,
            &format!("{base}/{path}"),
            metadata,
            &[req.encode_to_vec()],
        )
        .await
    };
    let first = ReflectionRequest {
        file_containing_symbol: Some(service.to_string()),
        ..Default::default()
    };
    let mut answer = ask(path, first.clone()).await?;
    if answer.status == 12 {
        path = "grpc.reflection.v1alpha.ServerReflection/ServerReflectionInfo";
        answer = ask(path, first).await?;
    }
    let mut files: BTreeMap<String, prost_types::FileDescriptorProto> = BTreeMap::new();
    loop {
        if answer.status != 0 {
            return Err(SendError::Grpc(match answer.status {
                12 => "the server has no reflection; set proto in [grpc] to a .proto file".into(),
                code => format!(
                    "reflection failed: {} {}",
                    status_name(code),
                    answer.message
                ),
            }));
        }
        for bytes in &answer.messages {
            let resp = ReflectionResponse::decode(bytes.as_ref())
                .map_err(|e| SendError::Grpc(format!("bad reflection answer: {e}")))?;
            if let Some(err) = resp.error_response {
                return Err(SendError::Grpc(format!(
                    "reflection: {} {}",
                    status_name(err.error_code),
                    err.error_message
                )));
            }
            for raw in resp
                .file_descriptor_response
                .into_iter()
                .flat_map(|f| f.file_descriptor_proto)
            {
                let file = prost_types::FileDescriptorProto::decode(raw.as_slice())
                    .map_err(|e| SendError::Grpc(format!("bad file from reflection: {e}")))?;
                files.insert(file.name().to_string(), file);
            }
        }
        // Servers send the imports too, or only the file: ask for any missing.
        let missing = files
            .values()
            .flat_map(|f| f.dependency.iter())
            .find(|d| !files.contains_key(*d))
            .cloned();
        let Some(name) = missing else { break };
        let req = ReflectionRequest {
            file_by_filename: Some(name.clone()),
            ..Default::default()
        };
        answer = ask(path, req).await?;
        if answer.status == 0 && answer.messages.is_empty() {
            return Err(SendError::Grpc(format!("reflection did not send {name}")));
        }
    }
    let mut pool = DescriptorPool::new();
    pool.add_file_descriptor_protos(files.into_values())
        .map_err(|e| SendError::Grpc(format!("the reflected schema is not valid: {e}")))?;
    Ok(pool)
}

/// Makes a unary call. The response holds the answer message as JSON, and
/// the headers and trailers, `grpc-status` among them. A call the server
/// refuses is still a response: its body is the status as JSON.
pub(crate) async fn unary(req: &Resolved, bearer: Option<&str>) -> Result<Response, SendError> {
    let Some(call) = req.grpc() else {
        return Err(SendError::Grpc("not a gRPC call".into()));
    };
    let sent = req.sent();
    let base = sent
        .url
        .strip_suffix(&format!("/{}/{}", call.service, call.method))
        .unwrap_or(&sent.url)
        .to_string();
    let mut metadata = sent.headers.clone();
    if let Some(t) = bearer {
        metadata.push(("authorization".into(), format!("Bearer {t}")));
    }
    let start = Instant::now();
    let http = client()?;
    let method = method(&http, &base, call, &metadata).await?;
    if method.is_client_streaming() || method.is_server_streaming() {
        return Err(SendError::Grpc(format!(
            "{} is a streaming method; Reqlite sends unary calls for now",
            call.method
        )));
    }
    let text = match &sent.body {
        Some(SendBody::Text(t)) if !t.trim().is_empty() => t.as_str(),
        _ => "{}",
    };
    let mut de = serde_json::Deserializer::from_str(text);
    let message = DynamicMessage::deserialize(method.input(), &mut de)
        .and_then(|m| de.end().map(|()| m))
        .map_err(|e| SendError::GrpcMessage(format!("{}: {e}", method.input().full_name())))?;
    let answer = call_once(&http, &sent.url, &metadata, &message).await?;
    let json = if answer.status == 0 {
        let Some(bytes) = answer.messages.first() else {
            return Err(SendError::Grpc("the answer has no message".into()));
        };
        let out = DynamicMessage::decode(method.output(), bytes.as_ref())
            .map_err(|e| SendError::Grpc(format!("bad answer message: {e}")))?;
        to_json(&out)?
    } else {
        let error = serde_json::json!({
            "code": answer.status,
            "status": status_name(answer.status),
            "message": answer.message,
        });
        serde_json::to_vec_pretty(&error).map_err(|e| SendError::Grpc(e.to_string()))?
    };
    Ok(Response {
        status: answer.http_status,
        headers: answer.headers,
        body: Body(Store::Memory(json)),
        elapsed: start.elapsed(),
        oauth_token: bearer.map(str::to_string),
    })
}

async fn call_once(
    http: &Http,
    url: &str,
    metadata: &[(String, String)],
    message: &DynamicMessage,
) -> Result<Answer, SendError> {
    call(http, url, metadata, &[message.encode_to_vec()]).await
}

/// A message in the protobuf JSON mapping, with fields at their default
/// values shown too, so the reader sees every field.
fn to_json(message: &DynamicMessage) -> Result<Vec<u8>, SendError> {
    let mut out = serde_json::Serializer::pretty(Vec::new());
    let options = SerializeOptions::new().skip_default_fields(false);
    message
        .serialize_with_options(&mut out, &options)
        .map_err(|e| SendError::Grpc(format!("cannot show the answer as JSON: {e}")))?;
    Ok(out.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_carry_a_flag_and_a_length() {
        let wire = [frame(b"ab"), frame(b""), frame(b"xyz")].concat();
        assert_eq!(&wire[..7], &[0, 0, 0, 0, 2, b'a', b'b']);
        let back = unframe(Bytes::from(wire)).unwrap();
        assert_eq!(back, [&b"ab"[..], b"", b"xyz"]);
        assert!(unframe(Bytes::from_static(&[0, 0, 0, 0, 9, 1])).is_err());
        assert!(unframe(Bytes::from_static(&[1, 0, 0, 0, 0])).is_err());
    }

    #[test]
    fn status_messages_are_percent_decoded() {
        assert_eq!(percent_decode("no%20user%20%E2%9C%93"), "no user ✓");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(status_name(5), "NOT_FOUND");
        assert_eq!(status_name(99), "UNKNOWN");
    }
}
