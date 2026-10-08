//! OAuth 2.0 against a local identity provider: every grant, the cache, the
//! refresh, and the one retry after a 401.
// The mock server's helpers sit outside `#[test]` functions, where
// `allow-unwrap-in-tests` does not reach.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use reqlite_engine::oauth::{Authorizer, OAuthError, Prompt, TokenCache};
use reqlite_engine::{SendError, client, resolve, send, send_with};
use reqlite_format::Environment;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// What the provider has seen and issued.
#[derive(Default)]
struct Idp {
    /// Token endpoint calls, by grant type.
    calls: BTreeMap<String, usize>,
    /// The access tokens the API accepts.
    valid: Vec<String>,
    issued: usize,
    /// `Authorization` headers the token endpoint received.
    client_auth: Vec<String>,
    /// PKCE challenge from the authorize URL, checked at the code exchange.
    challenge: Option<String>,
    /// Device polls left that answer `authorization_pending`.
    pending: usize,
    /// When true, the API answers 401 to every token.
    reject_all: bool,
}

type Shared = Arc<Mutex<Idp>>;

fn lock(idp: &Shared) -> std::sync::MutexGuard<'_, Idp> {
    idp.lock().unwrap_or_else(|e| e.into_inner())
}

async fn read_request(
    sock: &mut tokio::net::TcpStream,
) -> (String, BTreeMap<String, String>, String) {
    let mut got = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = sock.read(&mut buf).await.unwrap();
        got.extend_from_slice(&buf[..n]);
        if let Some(end) = got.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&got[..end]).into_owned();
            let len = head
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .map(str::to_string)
                })
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            while got.len() < end + 4 + len {
                let n = sock.read(&mut buf).await.unwrap();
                got.extend_from_slice(&buf[..n]);
            }
            let body = String::from_utf8_lossy(&got[end + 4..end + 4 + len]).into_owned();
            let mut lines = head.lines();
            let first = lines.next().unwrap_or_default().to_string();
            let headers = lines
                .filter_map(|l| l.split_once(": "))
                .map(|(k, v)| (k.to_ascii_lowercase(), v.to_string()))
                .collect();
            return (first, headers, body);
        }
        if n == 0 {
            return (String::new(), BTreeMap::new(), String::new());
        }
    }
}

fn form(body: &str) -> BTreeMap<String, String> {
    reqwest::Url::parse(&format!("http://h/?{body}"))
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect()
}

fn reply(status: u16, json: &str) -> String {
    format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
        json.len()
    )
}

fn issue(idp: &mut Idp) -> String {
    idp.issued += 1;
    let n = idp.issued;
    idp.valid.push(format!("at-{n}"));
    format!(
        r#"{{"access_token":"at-{n}","token_type":"Bearer","expires_in":3600,"refresh_token":"rt-{n}"}}"#
    )
}

/// Starts the provider. Its base URL serves `/token`, `/device` and `/api`.
async fn provider() -> (String, Shared) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let idp: Shared = Arc::default();
    let shared = idp.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            let idp = shared.clone();
            tokio::spawn(async move {
                let (first, headers, body) = read_request(&mut sock).await;
                let path = first
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_string();
                let out = {
                    let mut idp = lock(&idp);
                    let f = form(&body);
                    match path.as_str() {
                        "/token" => {
                            let grant = f.get("grant_type").cloned().unwrap_or_default();
                            *idp.calls.entry(grant.clone()).or_default() += 1;
                            if let Some(a) = headers.get("authorization") {
                                idp.client_auth.push(a.clone());
                            }
                            match grant.as_str() {
                                "client_credentials" | "refresh_token" => {
                                    reply(200, &issue(&mut idp))
                                }
                                "authorization_code" => {
                                    let verifier =
                                        f.get("code_verifier").cloned().unwrap_or_default();
                                    let hashed = base64url(&Sha256::digest(verifier.as_bytes()));
                                    if f.get("code").map(String::as_str) == Some("code-1")
                                        && idp.challenge.as_deref() == Some(hashed.as_str())
                                    {
                                        reply(200, &issue(&mut idp))
                                    } else {
                                        reply(
                                            400,
                                            r#"{"error":"invalid_grant","error_description":"PKCE check failed"}"#,
                                        )
                                    }
                                }
                                "urn:ietf:params:oauth:grant-type:device_code" => {
                                    if idp.pending > 0 {
                                        idp.pending -= 1;
                                        reply(400, r#"{"error":"authorization_pending"}"#)
                                    } else {
                                        reply(200, &issue(&mut idp))
                                    }
                                }
                                _ => reply(400, r#"{"error":"unsupported_grant_type"}"#),
                            }
                        }
                        "/device" => reply(
                            200,
                            r#"{"device_code":"dc-1","user_code":"WDJB-MJHT","verification_uri":"https://id.example/device","expires_in":30,"interval":1}"#,
                        ),
                        "/api" => {
                            let bearer = headers
                                .get("authorization")
                                .and_then(|a| a.strip_prefix("Bearer "))
                                .unwrap_or_default()
                                .to_string();
                            if !idp.reject_all && idp.valid.contains(&bearer) {
                                reply(200, &format!(r#"{{"you_sent":"{bearer}"}}"#))
                            } else {
                                reply(401, r#"{"error":"invalid_token"}"#)
                            }
                        }
                        _ => reply(404, "{}"),
                    }
                };
                sock.write_all(out.as_bytes()).await.unwrap();
            });
        }
    });
    (base, idp)
}

fn base64url(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let v = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..=chunk.len() {
            out.push(ABC[((v >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

#[derive(Default)]
struct Memory(Mutex<BTreeMap<String, String>>);

impl TokenCache for Memory {
    fn get(&self, key: &str) -> Option<String> {
        self.0.lock().ok()?.get(key).cloned()
    }
    fn put(&self, key: &str, value: &str) {
        if let Ok(mut m) = self.0.lock() {
            m.insert(key.into(), value.into());
        }
    }
}

fn request(base: &str, auth: &str) -> reqlite_format::Request {
    reqlite_format::parse(&format!(
        "version = 2\nname = \"t\"\nurl = \"{base}/api\"\n\n[auth]\ntype = \"oauth2\"\ntoken_url = \"{base}/token\"\nclient_id = \"app\"\n{auth}"
    ))
    .unwrap()
}

const CLIENT_CREDENTIALS: &str =
    "grant = \"client_credentials\"\nclient_secret = \"cs\"\nscope = \"read\"\n";

fn no_prompt(p: Prompt) {
    panic!("no sign-in expected: {p:?}");
}

async fn body_of(resp: &reqlite_engine::Response) -> String {
    let mut out = String::new();
    std::io::Read::read_to_string(&mut resp.body.reader().unwrap(), &mut out).unwrap();
    out
}

#[tokio::test]
async fn client_credentials_fetches_once_then_uses_the_cache() {
    let (base, idp) = provider().await;
    let req = resolve(&request(&base, CLIENT_CREDENTIALS), &Environment::default()).unwrap();
    let cache = Memory::default();
    let auth = Authorizer {
        cache: &cache,
        prompt: &no_prompt,
        wait: Duration::from_secs(5),
    };
    let http = client().unwrap();
    for _ in 0..3 {
        let resp = send_with(&http, &req, Some(&auth)).await.unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(body_of(&resp).await, r#"{"you_sent":"at-1"}"#);
    }
    let idp = lock(&idp);
    assert_eq!(
        idp.calls.get("client_credentials"),
        Some(&1),
        "cached after the first"
    );
    // Basic base64("app:cs").
    assert_eq!(idp.client_auth, ["Basic YXBwOmNz"]);
}

#[tokio::test]
async fn a_401_with_a_cached_token_renews_it_once() {
    let (base, idp) = provider().await;
    let req = resolve(&request(&base, CLIENT_CREDENTIALS), &Environment::default()).unwrap();
    let cache = Memory::default();
    let auth = Authorizer {
        cache: &cache,
        prompt: &no_prompt,
        wait: Duration::from_secs(5),
    };
    let http = client().unwrap();
    send_with(&http, &req, Some(&auth)).await.unwrap();
    lock(&idp).valid.clear(); // the server revokes at-1

    let resp = send_with(&http, &req, Some(&auth)).await.unwrap();
    assert_eq!(resp.status, 200, "renewed and sent again");
    assert_eq!(body_of(&resp).await, r#"{"you_sent":"at-2"}"#);
    assert_eq!(
        lock(&idp).calls.get("refresh_token"),
        Some(&1),
        "by its refresh token"
    );

    lock(&idp).reject_all = true;
    let resp = send_with(&http, &req, Some(&auth)).await.unwrap();
    assert_eq!(resp.status, 401, "only one retry, then the 401 stands");
    assert_eq!(lock(&idp).calls.get("refresh_token"), Some(&2));
}

#[tokio::test]
async fn an_expired_token_is_refreshed_before_the_send() {
    let (base, idp) = provider().await;
    let req = resolve(&request(&base, CLIENT_CREDENTIALS), &Environment::default()).unwrap();
    let cache = Memory::default();
    let auth = Authorizer {
        cache: &cache,
        prompt: &no_prompt,
        wait: Duration::from_secs(5),
    };
    let http = client().unwrap();
    send_with(&http, &req, Some(&auth)).await.unwrap();
    let key = cache.0.lock().unwrap().keys().next().cloned().unwrap();
    let stale = cache
        .get(&key)
        .unwrap()
        .replace(r#""expires_at":"#, r#""expires_at":1,"was":"#);
    cache.put(&key, &stale);

    let resp = send_with(&http, &req, Some(&auth)).await.unwrap();
    assert_eq!(body_of(&resp).await, r#"{"you_sent":"at-2"}"#);
    let idp = lock(&idp);
    assert_eq!(idp.calls.get("refresh_token"), Some(&1));
    assert_eq!(idp.calls.get("client_credentials"), Some(&1));
}

#[tokio::test]
async fn authorization_code_checks_pkce_and_the_redirect() {
    let (base, idp) = provider().await;
    let req = resolve(
        &request(
            &base,
            &format!("grant = \"authorization_code\"\nauth_url = \"{base}/authorize\"\n"),
        ),
        &Environment::default(),
    )
    .unwrap();
    let cache = Memory::default();
    let opened = Arc::new(Mutex::new(Vec::new()));
    let (seen, idp2) = (opened.clone(), idp.clone());
    // The "browser": records the URL, notes the PKCE challenge, and follows
    // the redirect with a code, as a provider would.
    let prompt = move |p: Prompt| {
        let Prompt::OpenUrl(url) = p else {
            panic!("{p:?}")
        };
        seen.lock().unwrap().push(url.clone());
        let q: BTreeMap<String, String> = reqwest::Url::parse(&url)
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect();
        lock(&idp2).challenge = q.get("code_challenge").cloned();
        let back = format!("{}?code=code-1&state={}", q["redirect_uri"], q["state"]);
        tokio::spawn(async move {
            // A stray request on the loopback, like a browser's favicon,
            // is turned away without ending the sign-in.
            let stray = back.replacen("/callback", "/favicon.ico", 1);
            let stray = reqwest::get(stray.split('?').next().unwrap())
                .await
                .unwrap();
            assert_eq!(stray.status(), 404);
            reqwest::get(back).await.unwrap();
        });
    };
    let auth = Authorizer {
        cache: &cache,
        prompt: &prompt,
        wait: Duration::from_secs(10),
    };
    let resp = send_with(&client().unwrap(), &req, Some(&auth))
        .await
        .unwrap();
    assert_eq!(resp.status, 200);
    let url = opened.lock().unwrap()[0].clone();
    assert!(
        url.starts_with(&format!(
            "{base}/authorize?response_type=code&client_id=app"
        )),
        "{url}"
    );
    assert!(url.contains("code_challenge_method=S256"), "{url}");
    assert_eq!(lock(&idp).calls.get("authorization_code"), Some(&1));
}

#[tokio::test]
async fn a_redirect_with_the_wrong_state_is_refused() {
    let (base, _idp) = provider().await;
    let req = resolve(
        &request(
            &base,
            &format!("grant = \"authorization_code\"\nauth_url = \"{base}/authorize\"\n"),
        ),
        &Environment::default(),
    )
    .unwrap();
    let cache = Memory::default();
    let prompt = |p: Prompt| {
        let Prompt::OpenUrl(url) = p else { panic!() };
        let q: BTreeMap<String, String> = reqwest::Url::parse(&url)
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect();
        let back = format!("{}?code=code-1&state=forged", q["redirect_uri"]);
        tokio::spawn(async move {
            reqwest::get(back).await.ok();
        });
    };
    let auth = Authorizer {
        cache: &cache,
        prompt: &prompt,
        wait: Duration::from_secs(10),
    };
    let err = send_with(&client().unwrap(), &req, Some(&auth))
        .await
        .unwrap_err();
    assert!(
        matches!(err, SendError::OAuth(ref e) if matches!(**e, OAuthError::StateMismatch)),
        "{err:?}"
    );
    assert!(cache.0.lock().unwrap().is_empty(), "nothing cached");
}

#[tokio::test]
async fn device_code_shows_the_code_and_polls_until_approved() {
    let (base, idp) = provider().await;
    lock(&idp).pending = 1;
    let req = resolve(
        &request(
            &base,
            &format!("grant = \"device_code\"\ndevice_url = \"{base}/device\"\n"),
        ),
        &Environment::default(),
    )
    .unwrap();
    let cache = Memory::default();
    let shown = Arc::new(Mutex::new(None));
    let seen = shown.clone();
    let prompt = move |p: Prompt| *seen.lock().unwrap() = Some(p);
    let auth = Authorizer {
        cache: &cache,
        prompt: &prompt,
        wait: Duration::from_secs(20),
    };
    let resp = send_with(&client().unwrap(), &req, Some(&auth))
        .await
        .unwrap();
    assert_eq!(resp.status, 200);
    assert_eq!(
        shown.lock().unwrap().clone(),
        Some(Prompt::DeviceCode {
            url: "https://id.example/device".into(),
            code: "WDJB-MJHT".into()
        })
    );
    assert_eq!(
        lock(&idp)
            .calls
            .get("urn:ietf:params:oauth:grant-type:device_code"),
        Some(&2),
        "one pending answer, then the token"
    );
}

#[tokio::test]
async fn tokens_never_reach_what_is_shown_or_stored() {
    let (base, _idp) = provider().await;
    let req = resolve(&request(&base, CLIENT_CREDENTIALS), &Environment::default()).unwrap();
    assert_eq!(
        req.redacted().headers,
        [(
            "Authorization".to_string(),
            "Bearer {{oauth_token}}".to_string()
        )]
    );
    let cache = Memory::default();
    let auth = Authorizer {
        cache: &cache,
        prompt: &no_prompt,
        wait: Duration::from_secs(5),
    };
    let resp = send_with(&client().unwrap(), &req, Some(&auth))
        .await
        .unwrap();
    let echoed = body_of(&resp).await;
    assert_eq!(
        resp.redact_token(echoed.as_bytes()),
        br#"{"you_sent":"{{oauth_token}}"}"#
    );
    assert!(!format!("{resp:?} {req:?}").contains("at-1"));
}

#[tokio::test]
async fn a_caller_with_no_token_store_is_told_so() {
    let (base, _idp) = provider().await;
    let req = resolve(&request(&base, CLIENT_CREDENTIALS), &Environment::default()).unwrap();
    let err = send(&client().unwrap(), &req).await.unwrap_err();
    assert!(
        matches!(err, SendError::OAuth(ref e) if matches!(**e, OAuthError::NoAuthorizer)),
        "{err:?}"
    );
}
