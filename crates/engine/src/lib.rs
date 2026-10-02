//! Resolves and sends requests. No UI knowledge lives here.

use std::fs::File;
use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

mod resolve;
pub use resolve::{Parts, ResolveError, Resolved, resolve};

/// Bodies larger than this go to a temp file instead of memory.
pub const SPILL_AT: usize = 1 << 20;

#[derive(Debug)]
pub struct Response {
    pub status: u16,
    /// Values stay as received. Decode only for display.
    pub headers: Vec<(String, Vec<u8>)>,
    pub body: Body,
    pub elapsed: Duration,
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

/// Reuse one `Client` for many sends so connections and TLS sessions are shared.
pub fn client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder().build()
}

pub async fn send(client: &reqwest::Client, req: &Resolved) -> Result<Response, SendError> {
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
    if let Some(body) = &req.body {
        builder = builder.body(body.clone());
    }

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
    })
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
        req.body = Some("hello".into());
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
}
