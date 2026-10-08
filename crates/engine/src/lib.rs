//! Resolves and sends requests. No UI knowledge lives here.

use std::fs::File;
use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

pub mod check;
pub mod graphql;
pub mod oauth;
mod resolve;
pub mod stream;
pub use resolve::{
    PartValue, Parts, ResolveError, Resolved, SendBody, SendPart, resolve, resolve_in,
};

/// An error and every cause under it, on one line.
pub(crate) fn chain(err: &dyn std::error::Error) -> String {
    let mut line = err.to_string();
    let mut cause = err.source();
    while let Some(c) = cause {
        line.push_str(": ");
        line.push_str(&c.to_string());
        cause = c.source();
    }
    line
}

/// Bodies larger than this go to a temp file instead of memory.
pub const SPILL_AT: usize = 1 << 20;

pub struct Response {
    pub status: u16,
    /// Values stay as received. Decode only for display.
    pub headers: Vec<(String, Vec<u8>)>,
    pub body: Body,
    pub elapsed: Duration,
    /// The OAuth 2.0 access token the request carried, so anything stored
    /// from the response can hide it. Never stored itself.
    pub oauth_token: Option<String>,
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .field("body", &self.body)
            .field("elapsed", &self.elapsed)
            .field("oauth_token", &self.oauth_token.as_ref().map(|_| "hidden"))
            .finish()
    }
}

impl Response {
    /// Replaces the OAuth 2.0 token in `bytes` with `{{oauth_token}}`. Use it
    /// after [`Resolved::redact`] on anything stored from this response.
    pub fn redact_token(&self, bytes: &[u8]) -> Vec<u8> {
        match &self.oauth_token {
            Some(t) if !t.is_empty() => resolve::replace(bytes, t.as_bytes(), b"{{oauth_token}}"),
            _ => bytes.to_vec(),
        }
    }
}

/// A response body, held once: in memory when small, in a temp file when large.
/// The temp file has no name on disk, so the OS frees it when the body is
/// dropped or the process dies, even on a crash.
#[derive(Debug)]
pub struct Body(Store);

#[derive(Debug)]
enum Store {
    Memory(Vec<u8>),
    File { file: File, len: u64 },
}

impl Body {
    pub fn len(&self) -> u64 {
        match &self.0 {
            Store::Memory(b) => b.len() as u64,
            Store::File { len, .. } => *len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn reader(&self) -> io::Result<Box<dyn Read + '_>> {
        Ok(match &self.0 {
            Store::Memory(b) => Box::new(b.as_slice()),
            Store::File { file, .. } => Box::new(io::BufReader::new(ReadAt { file, pos: 0 })),
        })
    }
}

/// Reads from its own position, so readers of one file never move each other.
struct ReadAt<'a> {
    file: &'a File,
    pos: u64,
}

impl Read for ReadAt<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        #[cfg(unix)]
        let n = std::os::unix::fs::FileExt::read_at(self.file, buf, self.pos)?;
        #[cfg(windows)]
        let n = std::os::windows::fs::FileExt::seek_read(self.file, buf, self.pos)?;
        self.pos += n as u64;
        Ok(n)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("invalid HTTP method {method:?}: {reason}")]
    Method { method: String, reason: String },
    #[error("cannot build the request")]
    Build(#[source] reqwest::Error),
    #[error("cannot connect")]
    Connect(#[source] reqwest::Error),
    #[error("timed out")]
    Timeout(#[source] reqwest::Error),
    #[error("too many redirects")]
    Redirect(#[source] reqwest::Error),
    #[error("the response body was cut off or unreadable")]
    Body(#[source] reqwest::Error),
    #[error("the request failed")]
    Transport(#[source] reqwest::Error),
    #[error("cannot write the response body to a temp file")]
    Spill(#[source] io::Error),
    #[error("cannot read the body file {path}")]
    BodyFile {
        path: std::path::PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid content type {content_type:?} for part {part:?}")]
    PartType { part: String, content_type: String },
    /// Boxed: the sign-in errors carry the server's answer, which would make
    /// every `SendError` large.
    #[error("OAuth 2.0 sign-in failed")]
    OAuth(#[source] Box<oauth::OAuthError>),
}

impl From<oauth::OAuthError> for SendError {
    fn from(e: oauth::OAuthError) -> Self {
        SendError::OAuth(Box::new(e))
    }
}

impl From<reqwest::Error> for SendError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_builder() {
            SendError::Build(e)
        } else if e.is_timeout() {
            SendError::Timeout(e)
        } else if e.is_connect() {
            SendError::Connect(e)
        } else if e.is_redirect() {
            SendError::Redirect(e)
        } else if e.is_body() || e.is_decode() {
            SendError::Body(e)
        } else {
            SendError::Transport(e)
        }
    }
}

/// Limits on each send. A send that passes one fails with [`SendError::Timeout`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    /// The TCP and TLS connect phase.
    pub connect: Duration,
    /// The longest wait for the next bytes, headers or body. It resets on every
    /// read, so a large download that keeps moving never reaches it.
    pub idle: Duration,
    /// The whole send, from connect to the last body byte. `None` lets a moving
    /// download take as long as it needs.
    pub total: Option<Duration>,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            connect: Duration::from_secs(10),
            idle: Duration::from_secs(30),
            total: None,
        }
    }
}

impl std::fmt::Display for Timeouts {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let show = |d: Duration| {
            if d.subsec_millis() == 0 {
                format!("{} s", d.as_secs())
            } else {
                format!("{} ms", d.as_millis())
            }
        };
        write!(
            f,
            "connect limit {}, no-data limit {}",
            show(self.connect),
            show(self.idle)
        )?;
        if let Some(total) = self.total {
            write!(f, ", total limit {}", show(total))?;
        }
        Ok(())
    }
}

/// A client with the default [`Timeouts`]. Reuse one `Client` for many sends so
/// connections and TLS sessions are shared.
pub fn client() -> Result<reqwest::Client, reqwest::Error> {
    client_with(Timeouts::default())
}

pub fn client_with(limits: Timeouts) -> Result<reqwest::Client, reqwest::Error> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(limits.connect)
        .read_timeout(limits.idle);
    if let Some(total) = limits.total {
        builder = builder.timeout(total);
    }
    builder.build()
}

/// Sends a request whose auth is not OAuth 2.0.
pub async fn send(client: &reqwest::Client, req: &Resolved) -> Result<Response, SendError> {
    send_with(client, req, None).await
}

/// Sends a request. One with OAuth 2.0 auth gets its token from `auth` first,
/// and after a 401 with a cached token, gets a new one and is sent once more.
pub async fn send_with(
    client: &reqwest::Client,
    req: &Resolved,
    auth: Option<&oauth::Authorizer<'_>>,
) -> Result<Response, SendError> {
    let Some(cfg) = req.oauth() else {
        return send_once(client, req, None).await;
    };
    let auth = auth.ok_or(oauth::OAuthError::NoAuthorizer)?;
    let (token, fresh) = oauth::token(client, cfg, auth).await?;
    let resp = send_once(client, req, Some(&token.access_token)).await?;
    if resp.status != 401 || fresh {
        return Ok(resp);
    }
    let token = oauth::renew(client, cfg, auth, Some(&token)).await?;
    send_once(client, req, Some(&token.access_token)).await
}

async fn send_once(
    client: &reqwest::Client,
    req: &Resolved,
    bearer: Option<&str>,
) -> Result<Response, SendError> {
    let builder = build(client, req, bearer).await?;
    let start = Instant::now();
    let mut resp = builder.send().await?;
    let status = resp.status().as_u16();
    let headers = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.as_bytes().to_vec()))
        .collect();
    let mut body = Store::Memory(Vec::new());
    while let Some(chunk) = resp.chunk().await? {
        // SHORTCUT: blocking file writes on the async task. Move to spawn_blocking
        // if the GUI shows dropped frames while a large body downloads.
        body = append(body, &chunk).map_err(SendError::Spill)?;
    }
    if let Store::File { file, .. } = &mut body {
        file.flush().map_err(SendError::Spill)?;
    }
    Ok(Response {
        status,
        headers,
        body: Body(body),
        elapsed: start.elapsed(),
        oauth_token: bearer.map(str::to_string),
    })
}

/// The HTTP request for `req`, ready to send.
async fn build(
    client: &reqwest::Client,
    req: &Resolved,
    bearer: Option<&str>,
) -> Result<reqwest::RequestBuilder, SendError> {
    let req = req.sent();
    let method = reqwest::Method::from_bytes(req.method.as_str().as_bytes()).map_err(|e| {
        SendError::Method {
            method: req.method.as_str().to_string(),
            reason: e.to_string(),
        }
    })?;
    let mut builder = client.request(method, &req.url).query(&req.query);
    for (k, v) in &req.headers {
        builder = builder.header(k, v);
    }
    if let Some(token) = bearer {
        builder = builder.bearer_auth(token);
    }
    match &req.body {
        None => {}
        Some(SendBody::Text(text)) => builder = builder.body(text.clone()),
        Some(SendBody::File(path)) => {
            let (file, len) = open_body(path).await?;
            builder = builder
                .header(reqwest::header::CONTENT_LENGTH, len)
                .body(file);
        }
        Some(SendBody::Multipart(parts)) => {
            let mut form = reqwest::multipart::Form::new();
            for p in parts {
                form = match &p.value {
                    PartValue::Text(t) => form.text(p.name.clone(), t.clone()),
                    PartValue::File { path, content_type } => {
                        let (file, len) = open_body(path).await?;
                        let mut part = reqwest::multipart::Part::stream_with_length(file, len);
                        if let Some(name) = path.file_name() {
                            part = part.file_name(name.to_string_lossy().into_owned());
                        }
                        let ct = content_type
                            .as_deref()
                            .unwrap_or("application/octet-stream");
                        let part = part.mime_str(ct).map_err(|_bad| SendError::PartType {
                            part: p.name.clone(),
                            content_type: ct.to_string(),
                        })?;
                        form.part(p.name.clone(), part)
                    }
                };
            }
            builder = builder.multipart(form);
        }
    }
    Ok(builder)
}

/// A body file, opened for streaming, and its length.
async fn open_body(path: &std::path::Path) -> Result<(tokio::fs::File, u64), SendError> {
    let fail = |source| SendError::BodyFile {
        path: path.to_path_buf(),
        source,
    };
    let file = tokio::fs::File::open(path).await.map_err(fail)?;
    let len = file.metadata().await.map_err(fail)?.len();
    Ok((file, len))
}

fn append(store: Store, chunk: &[u8]) -> io::Result<Store> {
    match store {
        Store::Memory(mut buf) if buf.len() + chunk.len() <= SPILL_AT => {
            buf.extend_from_slice(chunk);
            Ok(Store::Memory(buf))
        }
        Store::Memory(buf) => {
            let mut file = tempfile::tempfile()?;
            file.write_all(&buf)?;
            file.write_all(chunk)?;
            let len = (buf.len() + chunk.len()) as u64;
            Ok(Store::File { file, len })
        }
        Store::File { mut file, len } => {
            file.write_all(chunk)?;
            Ok(Store::File {
                file,
                len: len + chunk.len() as u64,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqlite_format::{Environment, Request};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// One-shot HTTP server. The handle yields the raw request it received.
    async fn serve_once(reply: Vec<u8>) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = sock.read(&mut buf).await.unwrap();
            sock.write_all(&reply).await.unwrap();
            String::from_utf8_lossy(&buf[..n]).into_owned()
        });
        (url, handle)
    }

    /// Like [`serve_once`], but reads the whole request: by Content-Length, or
    /// to the last chunk of a chunked body.
    async fn serve_whole(reply: Vec<u8>) -> (String, tokio::task::JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut got = Vec::new();
            let mut buf = vec![0u8; 65536];
            loop {
                let n = sock.read(&mut buf).await.unwrap();
                got.extend_from_slice(&buf[..n]);
                let Some(end) = got.windows(4).position(|w| w == b"\r\n\r\n") else {
                    continue;
                };
                let head = String::from_utf8_lossy(&got[..end]).to_ascii_lowercase();
                let body = got.len() - end - 4;
                let len = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .and_then(|v| v.trim().parse::<usize>().ok());
                let done = match len {
                    Some(len) => body >= len,
                    None => !head.contains("chunked") || got.ends_with(b"0\r\n\r\n"),
                };
                if done || n == 0 {
                    break;
                }
            }
            sock.write_all(&reply).await.unwrap();
            got
        });
        (url, handle)
    }

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    fn ok_with(body: &[u8]) -> Vec<u8> {
        let mut r = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n", body.len()).into_bytes();
        r.extend_from_slice(b"X-Raw: caf\xe9\r\nConnection: close\r\n\r\n");
        r.extend_from_slice(body);
        r
    }

    fn request(url: &str) -> Request {
        reqlite_format::parse(&format!(
            "version = 1\nname = \"t\"\nurl = \"{url}/users\"\n"
        ))
        .unwrap()
    }

    async fn send_plain(req: &Request) -> Result<Response, SendError> {
        send(
            &client().unwrap(),
            &resolve(req, &Environment::default()).unwrap(),
        )
        .await
    }

    fn read_all(body: &Body) -> Vec<u8> {
        let mut out = Vec::new();
        body.reader().unwrap().read_to_end(&mut out).unwrap();
        out
    }

    #[tokio::test]
    async fn small_body_stays_in_memory() {
        let (url, _srv) = serve_once(ok_with(b"ok")).await;
        let resp = send_plain(&request(&url)).await.unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(read_all(&resp.body), b"ok");
    }

    #[tokio::test]
    async fn large_body_reads_back_whole_from_two_readers_at_once() {
        let big: Vec<u8> = (0..5 * SPILL_AT).map(|i| (i % 251) as u8).collect();
        let (url, _srv) = serve_once(ok_with(&big)).await;
        let resp = send_plain(&request(&url)).await.unwrap();
        assert_eq!(resp.body.len(), big.len() as u64);
        let mut a = resp.body.reader().unwrap();
        let mut b = resp.body.reader().unwrap();
        let mut head = [0u8; 1000];
        a.read_exact(&mut head).unwrap();
        assert!(
            read_all(&resp.body) == big,
            "second reader saw different bytes"
        );
        let mut rest = Vec::new();
        b.read_to_end(&mut rest).unwrap();
        assert!(rest == big, "readers moved each other");
        assert_eq!(head, big[..1000]);
    }

    #[tokio::test]
    async fn header_values_keep_their_bytes() {
        let (url, _srv) = serve_once(ok_with(b"")).await;
        let resp = send_plain(&request(&url)).await.unwrap();
        let raw = resp.headers.iter().find(|(k, _)| k == "x-raw").unwrap();
        assert_eq!(raw.1, b"caf\xe9");
    }

    #[tokio::test]
    async fn sends_method_query_headers_and_body() {
        let (url, srv) = serve_once(ok_with(b"")).await;
        let mut req = request(&url);
        req.method = "post".to_string().try_into().unwrap();
        req.query.append("dry_run", "true");
        req.query.append("tag", "a");
        req.query.append("tag", "b");
        req.headers.append("X-Test", "yes");
        req.headers.append("X-Key", "{{key}}");
        req.body = Some(reqlite_format::Body::Text("hello".into()));
        let env = reqlite_format::parse_env(
            std::path::Path::new("dev.toml"),
            "version = 1\nsecrets = ['key']\n",
            Some("version = 1\n[vars]\nkey = 's3cret'\n"),
        )
        .unwrap();
        send(&client().unwrap(), &resolve(&req, &env).unwrap())
            .await
            .unwrap();

        let raw = srv.await.unwrap();
        assert!(
            raw.starts_with("POST /users?dry_run=true&tag=a&tag=b HTTP/1.1"),
            "{raw}"
        );
        assert!(raw.to_ascii_lowercase().contains("x-test: yes"), "{raw}");
        assert!(raw.to_ascii_lowercase().contains("x-key: s3cret"), "{raw}");
        assert!(raw.ends_with("hello"), "{raw}");
    }

    #[tokio::test]
    async fn refused_connection_is_a_connect_error() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let err = send_plain(&request(&url)).await.unwrap_err();
        assert!(matches!(err, SendError::Connect(_)), "{err:?}");
    }

    #[tokio::test]
    async fn truncated_body_is_a_body_error() {
        let reply =
            b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nok".to_vec();
        let (url, _srv) = serve_once(reply).await;
        let err = send_plain(&request(&url)).await.unwrap_err();
        assert!(matches!(err, SendError::Body(_)), "{err:?}");
    }

    /// Accepts one connection, reads the request, writes `reply`, then keeps the
    /// socket open. The handle yields true when the client closes it within 3 s.
    async fn serve_and_hold(reply: Vec<u8>) -> (String, tokio::task::JoinHandle<bool>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _n = sock.read(&mut buf).await.unwrap();
            sock.write_all(&reply).await.unwrap();
            let closed = tokio::time::timeout(Duration::from_secs(3), sock.read(&mut buf)).await;
            matches!(closed, Ok(Ok(0)) | Ok(Err(_)))
        });
        (url, handle)
    }

    fn short(total: Option<u64>) -> reqwest::Client {
        client_with(Timeouts {
            connect: Duration::from_millis(200),
            idle: Duration::from_millis(200),
            total: total.map(Duration::from_millis),
        })
        .unwrap()
    }

    async fn send_with(client: &reqwest::Client, url: &str) -> Result<Response, SendError> {
        let req = resolve(&request(url), &Environment::default()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), send(client, &req))
            .await
            .expect("the send hung past the test's own 5 s guard")
    }

    #[tokio::test]
    async fn a_silent_server_times_out_at_the_no_data_limit() {
        let (url, _srv) = serve_and_hold(Vec::new()).await;
        let start = Instant::now();
        let err = send_with(&short(None), &url).await.unwrap_err();
        assert!(matches!(err, SendError::Timeout(_)), "{err:?}");
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "{:?}",
            start.elapsed()
        );
    }

    #[tokio::test]
    async fn a_body_that_stops_times_out_at_the_no_data_limit() {
        let reply = b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\nfirst 10 b".to_vec();
        let (url, _srv) = serve_and_hold(reply).await;
        let err = send_with(&short(None), &url).await.unwrap_err();
        assert!(matches!(err, SendError::Timeout(_)), "{err:?}");
    }

    #[tokio::test]
    async fn a_moving_download_stops_at_the_total_limit() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _n = sock.read(&mut buf).await.unwrap();
            let head = b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n";
            if sock.write_all(head).await.is_err() {
                return;
            }
            for _ in 0..1000 {
                tokio::time::sleep(Duration::from_millis(50)).await;
                if sock.write_all(b"x").await.is_err() {
                    return;
                }
            }
        });
        let start = Instant::now();
        let err = send_with(&short(Some(600)), &url).await.unwrap_err();
        assert!(matches!(err, SendError::Timeout(_)), "{err:?}");
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "{:?}",
            start.elapsed()
        );
    }

    #[tokio::test]
    async fn dropping_a_send_in_flight_closes_the_connection() {
        let (url, srv) = serve_and_hold(Vec::new()).await;
        let task = tokio::spawn(async move { send_with(&client().unwrap(), &url).await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        task.abort();
        assert!(
            srv.await.unwrap(),
            "the server still had an open connection"
        );
    }

    #[test]
    fn timeouts_describe_their_limits() {
        assert_eq!(
            Timeouts::default().to_string(),
            "connect limit 10 s, no-data limit 30 s"
        );
        let t = Timeouts {
            total: Some(Duration::from_millis(1500)),
            ..Timeouts::default()
        };
        assert_eq!(
            t.to_string(),
            "connect limit 10 s, no-data limit 30 s, total limit 1500 ms"
        );
    }

    fn v2(url: &str, extra: &str) -> Request {
        reqlite_format::parse(&format!(
            "version = 2\nname = \"t\"\nmethod = \"POST\"\nurl = \"{url}/up\"\n{extra}"
        ))
        .unwrap()
    }

    async fn sent_in(req: &Request, dir: &std::path::Path) -> Result<Vec<u8>, SendError> {
        let (url, srv) = serve_whole(ok_with(b"")).await;
        let mut req = req.clone();
        req.url = req.url.replace("URL", &url);
        let r = resolve_in(&req, &Environment::default(), dir).unwrap();
        send(&client().unwrap(), &r).await?;
        Ok(srv.await.unwrap())
    }

    #[tokio::test]
    async fn a_json_body_says_so_and_a_form_is_url_encoded() {
        let dir = tempfile::tempdir().unwrap();
        let json = v2("URL", "[body]\ntype = 'json'\ntext = '{\"a\": 1}'\n");
        let raw = sent_in(&json, dir.path()).await.unwrap();
        let text = String::from_utf8_lossy(&raw).to_ascii_lowercase();
        assert!(text.contains("content-type: application/json"), "{text}");
        assert!(raw.ends_with(b"{\"a\": 1}"));

        let form = v2(
            "URL",
            "[body]\ntype = 'form'\n[body.fields]\nname = 'Ada L'\nnote = 'a&b=c'\n",
        );
        let raw = sent_in(&form, dir.path()).await.unwrap();
        let text = String::from_utf8_lossy(&raw).to_ascii_lowercase();
        assert!(
            text.contains("content-type: application/x-www-form-urlencoded"),
            "{text}"
        );
        assert!(raw.ends_with(b"name=Ada+L&note=a%26b%3Dc"), "{text}");
    }

    #[tokio::test]
    async fn a_file_body_is_streamed_whole_with_its_length() {
        let dir = tempfile::tempdir().unwrap();
        let bytes: Vec<u8> = (0..200_000).map(|i| (i % 251) as u8).collect();
        std::fs::write(dir.path().join("blob.bin"), &bytes).unwrap();
        let req = v2("URL", "[body]\ntype = 'file'\npath = 'blob.bin'\n");
        let raw = sent_in(&req, dir.path()).await.unwrap();
        let text = String::from_utf8_lossy(&raw[..300]).to_ascii_lowercase();
        assert!(text.contains("content-length: 200000"), "{text}");
        assert!(raw.ends_with(&bytes));
    }

    #[tokio::test]
    async fn a_missing_body_file_is_named_in_the_error() {
        let dir = tempfile::tempdir().unwrap();
        let req = v2("URL", "[body]\ntype = 'file'\npath = 'gone.bin'\n");
        let err = sent_in(&req, dir.path()).await.unwrap_err();
        match err {
            SendError::BodyFile { path, .. } => assert_eq!(path, dir.path().join("gone.bin")),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn multipart_sends_text_and_file_parts_in_order() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ada.png"), b"\x89PNG-bytes").unwrap();
        let req = v2(
            "URL",
            "[body]\ntype = 'multipart'\n[[body.parts]]\nname = 'note'\ntext = 'hi'\n\
             [[body.parts]]\nname = 'avatar'\nfile = 'ada.png'\ncontent_type = 'image/png'\n",
        );
        let raw = sent_in(&req, dir.path()).await.unwrap();
        let text = String::from_utf8_lossy(&raw);
        assert!(
            text.to_ascii_lowercase()
                .contains("content-type: multipart/form-data; boundary=")
        );
        let note = text.find("name=\"note\"").unwrap();
        let avatar = text.find("name=\"avatar\"; filename=\"ada.png\"").unwrap();
        assert!(note < avatar, "{text}");
        assert!(text.contains("Content-Type: image/png"), "{text}");
        assert!(contains(&raw, b"\x89PNG-bytes"));
    }

    #[tokio::test]
    async fn auth_adds_its_header_or_query_value() {
        let dir = tempfile::tempdir().unwrap();
        let bearer = v2("URL", "[auth]\ntype = 'bearer'\ntoken = 'abc.def'\n");
        let raw =
            String::from_utf8_lossy(&sent_in(&bearer, dir.path()).await.unwrap()).into_owned();
        assert!(raw.contains("authorization: Bearer abc.def"), "{raw}");

        let key = v2(
            "URL",
            "[auth]\ntype = 'api_key'\nname = 'key'\nvalue = 'k1'\nin = 'query'\n",
        );
        let raw = String::from_utf8_lossy(&sent_in(&key, dir.path()).await.unwrap()).into_owned();
        assert!(raw.starts_with("POST /up?key=k1 HTTP/1.1"), "{raw}");
    }
}
