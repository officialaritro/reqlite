//! Connections that stay open: WebSocket, and server-sent events (SSE).
//!
//! A `ws://` or `wss://` URL is a WebSocket. A request with the header
//! `Accept: text/event-stream` is an SSE stream. Both report what happens as
//! timed [`Event`]s.

use crate::{Resolved, SendError, oauth};
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{self, Message, client::IntoClientRequest};

/// How long a connect may take, as for a send.
const CONNECT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    WebSocket,
    Sse,
}

/// The kind of connection `req` opens, or `None` for a plain send.
pub fn kind(req: &Resolved) -> Option<Kind> {
    let sent = req.sent();
    let url = sent.url.trim_start().to_ascii_lowercase();
    if url.starts_with("ws://") || url.starts_with("wss://") {
        return Some(Kind::WebSocket);
    }
    sent.headers
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("accept") && v.contains("text/event-stream"))
        .then_some(Kind::Sse)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The connection is open. `status` is the server's HTTP answer: 101 for
    /// a WebSocket, 200 for an event stream.
    Open { status: u16 },
    /// A message from the server: a WebSocket frame, or an SSE event with
    /// its name when it has one.
    Received { name: Option<String>, data: String },
    /// A message this side sent.
    Sent { data: String },
    /// The connection ended, and why.
    Closed { reason: String },
}

/// An event and when it happened, from the start of the connect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timed {
    pub at: Duration,
    pub event: Event,
}

/// Opens the connection `req` describes and reports each event to `emit`,
/// until the server closes it, `emit` returns `false`, or `outgoing` closes.
/// Each message on `outgoing` is sent as a WebSocket text frame; an SSE
/// stream ignores them. The last event is always [`Event::Closed`]. Returns
/// an error when the connection failed rather than ended.
pub async fn open(
    req: &Resolved,
    auth: Option<&oauth::Authorizer<'_>>,
    mut outgoing: mpsc::UnboundedReceiver<String>,
    mut emit: impl FnMut(Timed) -> bool,
) -> Result<(), String> {
    let start = Instant::now();
    let mut tell = |event: Event| {
        emit(Timed {
            at: start.elapsed(),
            event,
        })
    };
    let result = match kind(req) {
        Some(Kind::WebSocket) => websocket(req, auth, &mut outgoing, &mut tell).await,
        _ => sse(req, auth, &mut tell).await,
    };
    let reason = match &result {
        Ok(reason) => reason.clone(),
        Err(e) => e.clone(),
    };
    tell(Event::Closed { reason });
    result.map(drop)
}

/// A client for streams: a connect limit, and no limit on quiet time, as a
/// stream may wait long for its next event.
fn stream_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(CONNECT)
        .build()
        .map_err(|e| crate::chain(&e))
}

async fn bearer(
    client: &reqwest::Client,
    req: &Resolved,
    auth: Option<&oauth::Authorizer<'_>>,
) -> Result<Option<String>, String> {
    let Some(cfg) = req.oauth() else {
        return Ok(None);
    };
    let auth = auth.ok_or_else(|| oauth::OAuthError::NoAuthorizer.to_string())?;
    let (token, _) = oauth::token(client, cfg, auth)
        .await
        .map_err(|e| crate::chain(&e))?;
    Ok(Some(token.access_token))
}

/// Reads an SSE stream. Returns why it ended.
async fn sse(
    req: &Resolved,
    auth: Option<&oauth::Authorizer<'_>>,
    tell: &mut impl FnMut(Event) -> bool,
) -> Result<String, String> {
    let client = stream_client()?;
    let token = bearer(&client, req, auth).await?;
    let show = |e: SendError| crate::chain(&e);
    let resp = crate::build(&client, req, token.as_deref())
        .await
        .map_err(show)?
        .send()
        .await
        .map_err(|e| show(e.into()))?;
    let status = resp.status().as_u16();
    let is_stream = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream"));
    if !resp.status().is_success() || !is_stream {
        return Err(format!("the server answered {status}, not an event stream"));
    }
    if !tell(Event::Open { status }) {
        return Ok("closed by you".into());
    }
    let mut parser = SseParser::default();
    let mut body = resp.bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|e| show(e.into()))?;
        for (name, data) in parser.push(&chunk) {
            if !tell(Event::Received { name, data }) {
                return Ok("closed by you".into());
            }
        }
    }
    Ok("closed by the server".into())
}

/// The SSE wire format: `field: value` lines, an event per blank line.
#[derive(Default)]
struct SseParser {
    /// Bytes after the last line ending.
    partial: Vec<u8>,
    name: Option<String>,
    data: Option<String>,
    /// The last byte was a `\r`, so a `\n` right after it ends nothing.
    after_cr: bool,
}

impl SseParser {
    /// Events completed by `bytes`, as names and data.
    fn push(&mut self, bytes: &[u8]) -> Vec<(Option<String>, String)> {
        let mut done = Vec::new();
        for &b in bytes {
            let after_cr = std::mem::replace(&mut self.after_cr, b == b'\r');
            match b {
                b'\n' if after_cr => {}
                b'\r' | b'\n' => {
                    let line =
                        String::from_utf8_lossy(&std::mem::take(&mut self.partial)).into_owned();
                    done.extend(self.line(&line));
                }
                _ => self.partial.push(b),
            }
        }
        done
    }

    fn line(&mut self, line: &str) -> Option<(Option<String>, String)> {
        if line.is_empty() {
            let name = self.name.take();
            return self.data.take().map(|data| (name, data));
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => self.name = Some(value.to_string()),
            "data" => match &mut self.data {
                Some(d) => {
                    d.push('\n');
                    d.push_str(value);
                }
                None => self.data = Some(value.to_string()),
            },
            // Comments (an empty field), `id` and `retry` show nothing.
            _ => {}
        }
        None
    }
}

/// A byte stream TLS or plain, for the WebSocket handshake.
trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// Runs a WebSocket session. Returns why it ended.
async fn websocket(
    req: &Resolved,
    auth: Option<&oauth::Authorizer<'_>>,
    outgoing: &mut mpsc::UnboundedReceiver<String>,
    tell: &mut impl FnMut(Event) -> bool,
) -> Result<String, String> {
    let sent = req.sent();
    let mut url = reqwest::Url::parse(sent.url.trim()).map_err(|e| format!("bad URL: {e}"))?;
    if !sent.query.is_empty() {
        url.query_pairs_mut().extend_pairs(&sent.query);
    }
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("bad WebSocket request: {e}"))?;
    let token = match req.oauth() {
        Some(_) => bearer(&stream_client()?, req, auth).await?,
        None => None,
    };
    let bearer_line = token.map(|t| ("Authorization".to_string(), format!("Bearer {t}")));
    for (k, v) in sent.headers.iter().cloned().chain(bearer_line) {
        let name = tungstenite::http::HeaderName::from_bytes(k.as_bytes())
            .map_err(|e| format!("bad header {k:?}: {e}"))?;
        let value = tungstenite::http::HeaderValue::from_str(&v)
            .map_err(|e| format!("bad value for header {k:?}: {e}"))?;
        request.headers_mut().append(name, value);
    }
    let host = url.host_str().ok_or("the URL has no host")?.to_string();
    let port = url.port_or_known_default().ok_or("the URL has no port")?;
    let connect = async {
        let tcp = tokio::net::TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|e| format!("cannot connect to {host}:{port}: {e}"))?;
        let io: Box<dyn Io> = if url.scheme() == "wss" {
            let name = rustls::pki_types::ServerName::try_from(host.clone())
                .map_err(|e| format!("bad host name {host:?}: {e}"))?;
            let tls = tokio_rustls::TlsConnector::from(tls_config()?)
                .connect(name, tcp)
                .await
                .map_err(|e| format!("TLS with {host} failed: {e}"))?;
            Box::new(tls)
        } else {
            Box::new(tcp)
        };
        tokio_tungstenite::client_async(request, io)
            .await
            .map_err(|e| format!("the WebSocket handshake failed: {e}"))
    };
    let (mut ws, answer) = tokio::time::timeout(CONNECT, connect)
        .await
        .map_err(|_elapsed| format!("no connection within {} s", CONNECT.as_secs()))??;
    if !tell(Event::Open {
        status: answer.status().as_u16(),
    }) {
        ws.close(None).await.ok();
        return Ok("closed by you".into());
    }
    loop {
        tokio::select! {
            frame = ws.next() => {
                let event = match frame {
                    None => return Ok("closed by the server".into()),
                    Some(Err(e)) => return Err(format!("the connection failed: {e}")),
                    Some(Ok(Message::Text(t))) => Event::Received { name: None, data: t.to_string() },
                    Some(Ok(Message::Binary(b))) => Event::Received {
                        name: None,
                        data: format!("({} bytes of binary data)", b.len()),
                    },
                    Some(Ok(Message::Close(frame))) => {
                        return Ok(match frame {
                            Some(f) if !f.reason.is_empty() => {
                                format!("closed by the server: {} {}", u16::from(f.code), f.reason)
                            }
                            Some(f) => format!("closed by the server: {}", u16::from(f.code)),
                            None => "closed by the server".into(),
                        });
                    }
                    // Pings are answered by the library.
                    Some(Ok(_)) => continue,
                };
                if !tell(event) {
                    ws.close(None).await.ok();
                    return Ok("closed by you".into());
                }
            }
            message = outgoing.recv() => {
                let Some(text) = message else {
                    ws.close(None).await.ok();
                    return Ok("closed by you".into());
                };
                ws.send(Message::text(text.clone()))
                    .await
                    .map_err(|e| format!("cannot send: {e}"))?;
                if !tell(Event::Sent { data: text }) {
                    ws.close(None).await.ok();
                    return Ok("closed by you".into());
                }
            }
        }
    }
}

/// TLS that trusts what the OS trusts, as for every send.
pub(crate) fn tls_config() -> Result<Arc<rustls::ClientConfig>, String> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let verifier = rustls_platform_verifier::Verifier::new(provider.clone())
        .map_err(|e| format!("cannot load the OS certificates: {e}"))?;
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    Ok(Arc::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_events_split_on_blank_lines_across_chunks() {
        let mut p = SseParser::default();
        let mut got = p.push(b": a comment\nevent: greet\ndata: hel");
        assert!(got.is_empty());
        got.extend(p.push(b"lo\ndata: world\r\n\r\ndata:plain\rid: 7\r\r\n"));
        got.extend(p.push(b"retry: 5\n\nevent: empty\n\n"));
        assert_eq!(
            got,
            [
                (Some("greet".to_string()), "hello\nworld".to_string()),
                (None, "plain".to_string()),
            ]
        );
    }
}
