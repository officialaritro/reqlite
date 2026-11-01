//! gRPC calls over HTTP/2, with messages written and shown as JSON.
//!
//! The schema comes from a `.proto` file, compiled when the call is made, or
//! from the server by reflection. A call is a POST to `/Service/Method` with
//! each message framed as one flag byte, a 4-byte length and the bytes; the
//! outcome is in the `grpc-status` trailer.
//!
//! [`send`] makes a call of any kind and collects it: a JSON array in the
//! message sends several messages to a client-streaming method, and a
//! server-streaming answer comes back as a JSON array. [`live`] runs a call as
//! a session: messages go out as they are typed and show as they arrive.

use crate::resolve::GrpcCall;
use crate::stream::Event;
use crate::{Body, Resolved, Response, SendBody, SendError, Store};
use bytes::{Bytes, BytesMut};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage, MethodDescriptor, SerializeOptions};
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// How long a connect may take, and the longest quiet time in an answer.
const CONNECT: Duration = Duration::from_secs(10);
const IDLE: Duration = Duration::from_secs(30);

type Http = Client<hyper_rustls::HttpsConnector<HttpConnector>, BoxBody<Bytes, Infallible>>;

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

/// Starts a call: sends the headers and `body`, and waits for the answer's
/// headers. `wait` bounds that wait; a live session passes `None`, as a
/// server may hold its headers until it has something to say.
async fn exchange(
    http: &Http,
    url: &str,
    metadata: &[(String, String)],
    body: BoxBody<Bytes, Infallible>,
    wait: Option<Duration>,
) -> Result<(u16, Vec<(String, Vec<u8>)>, Incoming), SendError> {
    let mut builder = http::Request::post(url)
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .header("grpc-accept-encoding", "identity")
        .header("user-agent", concat!("reqlite/", env!("CARGO_PKG_VERSION")));
    for (k, v) in metadata {
        builder = builder.header(k.to_ascii_lowercase(), v);
    }
    let request = builder
        .body(body)
        .map_err(|e| SendError::Grpc(format!("bad request: {e}")))?;
    let reached = |r: Result<http::Response<Incoming>, _>| {
        r.map_err(|e: hyper_util::client::legacy::Error| {
            SendError::Grpc(format!("cannot reach {url}: {}", crate::chain(&e)))
        })
    };
    let resp = match wait {
        Some(limit) => reached(
            tokio::time::timeout(limit, http.request(request))
                .await
                .map_err(|_elapsed| no_within("answer"))?,
        )?,
        None => reached(http.request(request).await)?,
    };
    let headers = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.as_bytes().to_vec()))
        .collect();
    Ok((resp.status().as_u16(), headers, resp.into_body()))
}

fn no_within(what: &str) -> SendError {
    SendError::Grpc(format!("no {what} within {} s", IDLE.as_secs()))
}

/// How a call ended: its `grpc-status` and `grpc-message`.
struct Ending {
    status: i32,
    message: String,
}

/// Reads an answer's messages, each one as it is complete, until the
/// trailers. `on_message` returns `false` to stop reading. The trailers are
/// added to `headers`. `wait` bounds each quiet time.
async fn read(
    mut body: Incoming,
    http_status: u16,
    headers: &mut Vec<(String, Vec<u8>)>,
    wait: Option<Duration>,
    mut on_message: impl FnMut(Bytes) -> bool,
) -> Result<Option<Ending>, SendError> {
    let mut deframer = Deframer::default();
    loop {
        let next = match wait {
            Some(limit) => tokio::time::timeout(limit, body.frame())
                .await
                .map_err(|_elapsed| no_within("data"))?,
            None => body.frame().await,
        };
        let Some(frame) = next else { break };
        let frame = frame.map_err(|e| SendError::Grpc(format!("the answer broke off: {e}")))?;
        match frame.into_data() {
            Ok(chunk) => {
                for message in deframer.push(&chunk)? {
                    if !on_message(message) {
                        return Ok(None);
                    }
                }
            }
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
    deframer.finish()?;
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
    Ok(Some(Ending { status, message }))
}

/// The answer to one call: its messages, and its headers and trailers.
struct Answer {
    http_status: u16,
    headers: Vec<(String, Vec<u8>)>,
    messages: Vec<Bytes>,
    status: i32,
    message: String,
}

/// Sends `messages` to `url`, closes the sending side, and reads the whole
/// answer.
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
    let body = Full::new(body.freeze()).boxed();
    let (http_status, mut headers, incoming) =
        exchange(http, url, metadata, body, Some(IDLE)).await?;
    let mut got = Vec::new();
    let ending = read(incoming, http_status, &mut headers, Some(IDLE), |m| {
        got.push(m);
        true
    })
    .await?;
    let Some(Ending { status, message }) = ending else {
        return Err(SendError::Grpc("the answer stopped early".into()));
    };
    Ok(Answer {
        http_status,
        headers,
        messages: got,
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

/// Cuts a byte stream into messages, wherever the chunks happen to break.
#[derive(Default)]
struct Deframer {
    buf: BytesMut,
}

impl Deframer {
    /// The messages `chunk` completes.
    fn push(&mut self, chunk: &[u8]) -> Result<Vec<Bytes>, SendError> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while self.buf.len() >= 5 {
            if self.buf[0] != 0 {
                return Err(SendError::Grpc(
                    "the server sent a compressed message, which Reqlite does not read".into(),
                ));
            }
            let len =
                u32::from_be_bytes([self.buf[1], self.buf[2], self.buf[3], self.buf[4]]) as usize;
            if self.buf.len() < 5 + len {
                break;
            }
            let mut message = self.buf.split_to(5 + len);
            out.push(message.split_off(5).freeze());
        }
        Ok(out)
    }

    /// Fails when the stream ended inside a message.
    fn finish(&self) -> Result<(), SendError> {
        match self.buf.len() {
            0 => Ok(()),
            1..5 => Err(SendError::Grpc("a message frame is cut short".into())),
            _ => Err(SendError::Grpc("a message is cut short".into())),
        }
    }
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

/// Where a call goes, and what goes with it.
struct Target {
    http: Http,
    url: String,
    metadata: Vec<(String, String)>,
    method: MethodDescriptor,
}

async fn target(req: &Resolved, bearer: Option<&str>) -> Result<Target, SendError> {
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
    let http = client()?;
    let method = method(&http, &base, call, &metadata).await?;
    Ok(Target {
        http,
        url: sent.url.clone(),
        metadata,
        method,
    })
}

/// The kind of a method, as people name it.
fn kind(method: &MethodDescriptor) -> &'static str {
    match (method.is_client_streaming(), method.is_server_streaming()) {
        (false, false) => "unary",
        (false, true) => "server streaming",
        (true, false) => "client streaming",
        (true, true) => "bidirectional streaming",
    }
}

/// The messages a text holds. A client-streaming method takes a JSON array
/// of them, or one message. Any other method takes one message. Empty text
/// is one empty message, or none for a client-streaming method.
fn messages_in(method: &MethodDescriptor, text: &str) -> Result<Vec<DynamicMessage>, SendError> {
    let input = method.input();
    let one = |text: &str| {
        let mut de = serde_json::Deserializer::from_str(text);
        DynamicMessage::deserialize(input.clone(), &mut de)
            .and_then(|m| de.end().map(|()| m))
            .map_err(|e| SendError::GrpcMessage(format!("{}: {e}", input.full_name())))
    };
    let text = text.trim();
    if !text.starts_with('[') {
        return match (text.is_empty(), method.is_client_streaming()) {
            (true, true) => Ok(Vec::new()),
            (true, false) => one("{}").map(|m| vec![m]),
            (false, _) => one(text).map(|m| vec![m]),
        };
    }
    if !method.is_client_streaming() {
        return Err(SendError::GrpcMessage(format!(
            "{} is {} and takes one message, not an array",
            method.name(),
            kind(method)
        )));
    }
    let items: Vec<serde_json::Value> = serde_json::from_str(text)
        .map_err(|e| SendError::GrpcMessage(format!("the message array: {e}")))?;
    items
        .into_iter()
        .enumerate()
        .map(|(i, item)| {
            DynamicMessage::deserialize(input.clone(), item).map_err(|e| {
                SendError::GrpcMessage(format!("message {}: {}: {e}", i + 1, input.full_name()))
            })
        })
        .collect()
}

fn body_text(req: &Resolved) -> &str {
    match &req.sent().body {
        Some(SendBody::Text(text)) => text.as_str(),
        _ => "",
    }
}

/// Makes a call of any kind and collects it. The response holds the answer
/// as JSON: one message, or for a server-streaming method an array of all of
/// them, and the headers and trailers, `grpc-status` among them. A call the
/// server refuses is still a response: its body is the status as JSON, with
/// any streamed messages that came before it.
pub(crate) async fn send(req: &Resolved, bearer: Option<&str>) -> Result<Response, SendError> {
    let start = Instant::now();
    let t = target(req, bearer).await?;
    let messages: Vec<Vec<u8>> = messages_in(&t.method, body_text(req))?
        .iter()
        .map(Message::encode_to_vec)
        .collect();
    let answer = call(&t.http, &t.url, &t.metadata, &messages).await?;
    let decoded = answer
        .messages
        .iter()
        .map(|bytes| {
            DynamicMessage::decode(t.method.output(), bytes.as_ref())
                .map_err(|e| SendError::Grpc(format!("bad answer message: {e}")))
                .and_then(|m| to_value(&m))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let streams_back = t.method.is_server_streaming();
    let json = if answer.status != 0 {
        let mut error = serde_json::json!({
            "code": answer.status,
            "status": status_name(answer.status),
            "message": answer.message,
        });
        if streams_back && !decoded.is_empty() {
            error["messages"] = serde_json::Value::Array(decoded);
        }
        error
    } else if streams_back {
        serde_json::Value::Array(decoded)
    } else {
        let mut decoded = decoded.into_iter();
        match (decoded.next(), decoded.next()) {
            (Some(one), None) => one,
            (None, _) => return Err(SendError::Grpc("the answer has no message".into())),
            (Some(_), Some(_)) => {
                return Err(SendError::Grpc(format!(
                    "{} is {} but the server sent more than one message",
                    t.method.name(),
                    kind(&t.method)
                )));
            }
        }
    };
    let body = serde_json::to_vec_pretty(&json).map_err(|e| SendError::Grpc(e.to_string()))?;
    Ok(Response {
        status: answer.http_status,
        headers: answer.headers,
        body: Body(Store::Memory(body)),
        elapsed: start.elapsed(),
        oauth_token: bearer.map(str::to_string),
    })
}

/// What the reader of a live call passes back.
enum Heard {
    Open(u16),
    Message(Bytes),
    End(Result<Ending, SendError>),
}

/// Runs a call of any kind as a session. The file's messages go first. Each
/// text on `outgoing` holds more messages for a client-streaming method, one
/// JSON message or an array. When `outgoing` closes, the sending side closes
/// and the session goes on until the server ends the call: that is how a
/// client says it has no more to send. A unary or server-streaming call
/// closes its sending side after its one message. Returns why it ended.
pub(crate) async fn live(
    req: &Resolved,
    bearer: Option<&str>,
    outgoing: &mut mpsc::UnboundedReceiver<String>,
    tell: &mut impl FnMut(Event) -> bool,
) -> Result<String, String> {
    const BY_YOU: &str = "closed by you";
    let show = |e: SendError| crate::chain(&e);
    let t = target(req, bearer).await.map_err(show)?;
    let first = messages_in(&t.method, body_text(req)).map_err(show)?;
    let (frames_tx, queued) = mpsc::unbounded_channel::<Bytes>();
    let body = StreamBody::new(futures_util::stream::unfold(queued, async |mut rx| {
        rx.recv()
            .await
            .map(|b| (Ok::<_, Infallible>(Frame::data(b)), rx))
    }))
    .boxed();
    if !tell(Event::Note {
        text: format!("{} is {}", t.method.name(), kind(&t.method)),
    }) {
        return Ok(BY_YOU.into());
    }
    let (method, output) = (t.method.clone(), t.method.output());
    let (heard_tx, mut heard) = mpsc::unbounded_channel();
    let reader = tokio::spawn(async move {
        let (status, mut headers, incoming) =
            match exchange(&t.http, &t.url, &t.metadata, body, None).await {
                Ok(parts) => parts,
                Err(e) => {
                    heard_tx.send(Heard::End(Err(e))).ok();
                    return;
                }
            };
        heard_tx.send(Heard::Open(status)).ok();
        let ended = read(incoming, status, &mut headers, None, |m| {
            heard_tx.send(Heard::Message(m)).is_ok()
        })
        .await;
        match ended {
            Ok(Some(ending)) => heard_tx.send(Heard::End(Ok(ending))).ok(),
            Ok(None) => None,
            Err(e) => heard_tx.send(Heard::End(Err(e))).ok(),
        };
    });
    // Leaving this function, for any reason, cancels the call.
    let _stop = AbortOnDrop(reader);
    let mut frames = Some(frames_tx);
    // Queues `ms` on the sending side and logs each one.
    let send_all = |ms: &[DynamicMessage],
                    frames: &Option<mpsc::UnboundedSender<Bytes>>,
                    tell: &mut dyn FnMut(Event) -> bool| {
        for m in ms {
            if let Some(f) = frames {
                f.send(Bytes::from(frame(&m.encode_to_vec()))).ok();
            }
            let data = to_line(m).unwrap_or_else(|e| e.to_string());
            if !tell(Event::Sent { data }) {
                return false;
            }
        }
        true
    };
    if !send_all(&first, &frames, tell) {
        return Ok(BY_YOU.into());
    }
    if !method.is_client_streaming() {
        frames = None;
    }
    let mut input_open = true;
    loop {
        let event = tokio::select! {
            got = heard.recv() => match got {
                Some(Heard::Open(status)) => Event::Open { status },
                Some(Heard::Message(bytes)) => {
                    let m = DynamicMessage::decode(output.clone(), bytes.as_ref())
                        .map_err(|e| format!("bad answer message: {e}"))?;
                    Event::Received { name: None, data: to_line(&m).map_err(show)? }
                }
                Some(Heard::End(Ok(Ending { status, message }))) => {
                    let mut reason = format!("ended with status {status} {}", status_name(status));
                    if !message.is_empty() {
                        reason.push_str(&format!(": {message}"));
                    }
                    return Ok(reason);
                }
                Some(Heard::End(Err(e))) => return Err(show(e)),
                None => return Err("the call stopped".into()),
            },
            typed = outgoing.recv(), if input_open => match typed {
                None => {
                    input_open = false;
                    if frames.take().is_none() {
                        continue;
                    }
                    Event::Note { text: "done sending".into() }
                }
                Some(_) if frames.is_none() => Event::Note {
                    text: format!("not sent: {} takes no more messages", method.name()),
                },
                Some(text) => match messages_in(&method, &text) {
                    Ok(ms) if ms.is_empty() => Event::Note { text: "not sent: no message".into() },
                    Ok(ms) => {
                        if !send_all(&ms, &frames, tell) {
                            return Ok(BY_YOU.into());
                        }
                        continue;
                    }
                    Err(e) => Event::Note { text: format!("not sent: {}", show(e)) },
                },
            },
        };
        if !tell(event) {
            return Ok(BY_YOU.into());
        }
    }
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A message as one line of JSON, for a log.
fn to_line(message: &DynamicMessage) -> Result<String, SendError> {
    serde_json::to_string(&to_value(message)?).map_err(|e| SendError::Grpc(e.to_string()))
}

/// A message in the protobuf JSON mapping, with fields at their default
/// values shown too, so the reader sees every field.
fn to_value(message: &DynamicMessage) -> Result<serde_json::Value, SendError> {
    let options = SerializeOptions::new().skip_default_fields(false);
    message
        .serialize_with_options(serde_json::value::Serializer, &options)
        .map_err(|e| SendError::Grpc(format!("cannot show the answer as JSON: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_carry_a_flag_and_a_length_across_any_chunking() {
        let wire = [frame(b"ab"), frame(b""), frame(b"xyz")].concat();
        assert_eq!(&wire[..7], &[0, 0, 0, 0, 2, b'a', b'b']);
        for cut in 0..=wire.len() {
            let mut d = Deframer::default();
            let mut back = d.push(&wire[..cut]).unwrap();
            back.extend(d.push(&wire[cut..]).unwrap());
            assert_eq!(back, [&b"ab"[..], b"", b"xyz"], "cut at {cut}");
            d.finish().unwrap();
        }
        let mut d = Deframer::default();
        assert!(d.push(&[0, 0, 0, 0, 9, 1]).unwrap().is_empty());
        assert!(d.finish().is_err());
        assert!(Deframer::default().push(&[1, 0, 0, 0, 0]).is_err());
    }

    #[test]
    fn status_messages_are_percent_decoded() {
        assert_eq!(percent_decode("no%20user%20%E2%9C%93"), "no user ✓");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(status_name(5), "NOT_FOUND");
        assert_eq!(status_name(99), "UNKNOWN");
    }
}
