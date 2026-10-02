//! Resolves and sends requests. No UI knowledge lives here.

use std::time::{Duration, Instant};

mod resolve;
pub use resolve::{Parts, ResolveError, Resolved, resolve};
#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub elapsed: Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("invalid HTTP method {method:?}: {reason}")]
    Method { method: String, reason: String },
    #[error("{0}")]
    Http(#[from] reqwest::Error),
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
    let resp = builder.send().await?;
    let status = resp.status().as_u16();
    let headers = resp
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.to_string(),
                String::from_utf8_lossy(v.as_bytes()).into_owned(),
            )
        })
        .collect();
    // SHORTCUT: whole body is buffered in memory. Stream to a temp file above a size
    // threshold once the 50 MB response budget is measured in the GUI (v0.1).
    let body = resp.bytes().await?.to_vec();
    Ok(Response {
        status,
        headers,
        body,
        elapsed: start.elapsed(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqlite_format::{Environment, Request};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// One-shot HTTP server: returns the raw request it received via the channel.
    async fn serve_once(reply: &'static str) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = sock.read(&mut buf).await.unwrap();
            sock.write_all(reply.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&buf[..n]).into_owned()
        });
        (url, handle)
    }

    fn request(url: &str) -> Request {
        reqlite_format::parse(&format!(
            "version = 1\nname = \"t\"\nurl = \"{url}/users\"\n"
        ))
        .unwrap()
    }

    #[tokio::test]
    async fn returns_status_and_body() {
        let (url, _srv) =
            serve_once("HTTP/1.1 201 Created\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await;
        let resp = send(
            &client().unwrap(),
            &resolve(&request(&url), &Environment::default()).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(resp.status, 201);
        assert_eq!(resp.body, b"ok");
    }

    #[tokio::test]
    async fn sends_method_query_headers_and_body() {
        let (url, srv) =
            serve_once("HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
        let mut req = request(&url);
        req.method = "post".to_string().try_into().unwrap();
        req.query.append("dry_run", "true");
        req.query.append("tag", "a");
        req.query.append("tag", "b");
        req.headers.append("X-Test", "yes");
        req.body = Some("hello".into());
        let env = reqlite_format::parse_env(
            std::path::Path::new("dev.toml"),
            "version = 1\nsecrets = ['key']\n",
            Some("version = 1\n[vars]\nkey = 's3cret'\n"),
        )
        .unwrap();
        req.headers.append("X-Key", "{{key}}");
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
}
