//! OAuth 2.0 access tokens: fetched at send time, cached, and refreshed.
//!
//! Grants: client credentials, authorization code with PKCE (S256) and a
//! redirect to a loopback port on this machine (RFC 8252), and device code
//! (RFC 8628). The engine never opens a browser or shows a code itself: it
//! hands a [`Prompt`] to the caller.
//!
//! A cached token is used until shortly before it expires. Then its refresh
//! token is used, or a new token is fetched. Tokens live only in the
//! [`TokenCache`] (the OS keychain in the apps), never in files or history.

use reqlite_format::{ClientAuth, Grant};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A token is renewed this long before it expires, so it cannot run out on the
/// way to the server.
const EARLY: u64 = 30;

/// A resolved OAuth 2.0 client: every placeholder is filled.
#[derive(Clone, PartialEq, Eq)]
pub struct OAuthConfig {
    pub grant: Grant,
    pub token_url: String,
    pub auth_url: Option<String>,
    pub device_url: Option<String>,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub scope: Option<String>,
    pub client_auth: ClientAuth,
}

impl fmt::Debug for OAuthConfig {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("OAuthConfig")
            .field("grant", &self.grant)
            .field("token_url", &self.token_url)
            .field("client_id", &self.client_id)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

impl OAuthConfig {
    /// Where its token is cached. Dev and prod differ in URL, client or scope,
    /// so they never share a token.
    pub fn cache_key(&self) -> String {
        format!(
            "oauth2:{}#{}#{}",
            self.token_url,
            self.client_id,
            self.scope.as_deref().unwrap_or("")
        )
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Token {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Seconds since the Unix epoch. `None` when the server gave no lifetime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Token")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

impl Token {
    fn usable(&self, now: u64) -> bool {
        self.expires_at.is_none_or(|at| at > now + EARLY)
    }
}

/// Keeps tokens between sends and between runs. The value is opaque text.
/// A store that fails behaves as empty: a token can always be fetched again.
pub trait TokenCache: Send + Sync {
    fn get(&self, key: &str) -> Option<String>;
    fn put(&self, key: &str, value: &str);
}

/// What the user must do to finish a sign-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    /// Open this page in a browser and sign in there.
    OpenUrl(String),
    /// Open `url` and enter `code` there.
    DeviceCode { url: String, code: String },
}

impl Prompt {
    /// The page the user must open.
    pub fn url(&self) -> &str {
        match self {
            Prompt::OpenUrl(url) | Prompt::DeviceCode { url, .. } => url,
        }
    }
}

impl std::fmt::Display for Prompt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Prompt::OpenUrl(url) => write!(f, "Sign in to continue: {url}"),
            Prompt::DeviceCode { url, code } => {
                write!(f, "Sign in at {url} and enter the code {code}")
            }
        }
    }
}

/// Opens `url` in the default browser.
pub fn open_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(drop)
}

/// What a send needs for a request with OAuth 2.0 auth.
pub struct Authorizer<'a> {
    pub cache: &'a dyn TokenCache,
    pub prompt: &'a (dyn Fn(Prompt) + Send + Sync),
    /// How long to wait for the user to finish a sign-in.
    pub wait: Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    #[error("this request uses OAuth 2.0, and this caller has no token store")]
    NoAuthorizer,
    #[error("the request to {url} failed")]
    Http {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("{url} answered {status}: {error}{}", description.as_deref().map(|d| format!(" ({d})")).unwrap_or_default())]
    Server {
        url: String,
        status: u16,
        error: String,
        description: Option<String>,
    },
    #[error("{url} sent no usable answer")]
    BadResponse { url: String },
    #[error("the sign-in was not finished in time")]
    TimedOut,
    #[error("the sign-in was refused: {0}")]
    Denied(String),
    #[error("the redirect does not belong to this sign-in")]
    StateMismatch,
    #[error("cannot listen for the sign-in redirect")]
    Listen(#[source] std::io::Error),
    #[error("cannot make a random value: {0}")]
    Random(String),
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A token for `cfg`: from the cache while it is good, else refreshed, else
/// fetched. The flag is true when the token is new, so a 401 is not retried
/// with a token the server has just issued.
pub(crate) async fn token(
    client: &reqwest::Client,
    cfg: &OAuthConfig,
    auth: &Authorizer<'_>,
) -> Result<(Token, bool), OAuthError> {
    let key = cfg.cache_key();
    let cached = auth
        .cache
        .get(&key)
        .and_then(|text| serde_json::from_str::<Token>(&text).ok());
    if let Some(t) = &cached {
        if t.usable(now()) {
            return Ok((t.clone(), false));
        }
    }
    Ok((renew(client, cfg, auth, cached.as_ref()).await?, true))
}

/// A new token: by the refresh token of `stale` when it has one and the server
/// accepts it, else by the grant. The new token is cached.
pub(crate) async fn renew(
    client: &reqwest::Client,
    cfg: &OAuthConfig,
    auth: &Authorizer<'_>,
    stale: Option<&Token>,
) -> Result<Token, OAuthError> {
    let refreshed = match stale.and_then(|t| t.refresh_token.clone()) {
        Some(refresh) => {
            let form = vec![
                ("grant_type", "refresh_token".to_string()),
                ("refresh_token", refresh.clone()),
            ];
            // A refused refresh token falls back to a full sign-in.
            post_token(client, cfg, &cfg.token_url, form)
                .await
                .ok()
                .map(|mut t| {
                    // A server may keep the old refresh token in use.
                    t.refresh_token.get_or_insert(refresh);
                    t
                })
        }
        None => None,
    };
    let token = match refreshed {
        Some(t) => t,
        None => match cfg.grant {
            Grant::ClientCredentials => client_credentials(client, cfg).await?,
            Grant::AuthorizationCode => authorization_code(client, cfg, auth).await?,
            Grant::DeviceCode => device_code(client, cfg, auth).await?,
        },
    };
    if let Ok(text) = serde_json::to_string(&token) {
        auth.cache.put(&cfg.cache_key(), &text);
    }
    Ok(token)
}

async fn client_credentials(
    client: &reqwest::Client,
    cfg: &OAuthConfig,
) -> Result<Token, OAuthError> {
    let mut form = vec![("grant_type", "client_credentials".to_string())];
    if let Some(scope) = &cfg.scope {
        form.push(("scope", scope.clone()));
    }
    post_token(client, cfg, &cfg.token_url, form).await
}

async fn authorization_code(
    client: &reqwest::Client,
    cfg: &OAuthConfig,
    auth: &Authorizer<'_>,
) -> Result<Token, OAuthError> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(OAuthError::Listen)?;
    let port = listener.local_addr().map_err(OAuthError::Listen)?.port();
    let redirect = format!("http://127.0.0.1:{port}/callback");
    let verifier = base64url(&random(32)?);
    let challenge = base64url(&Sha256::digest(verifier.as_bytes()));
    let state = base64url(&random(16)?);

    let auth_url = cfg.auth_url.as_deref().unwrap_or_default();
    let mut params = vec![
        ("response_type", "code"),
        ("client_id", cfg.client_id.as_str()),
        ("redirect_uri", redirect.as_str()),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("state", state.as_str()),
    ];
    if let Some(scope) = &cfg.scope {
        params.push(("scope", scope));
    }
    let url = reqwest::Url::parse_with_params(auth_url, &params).map_err(|_bad| {
        OAuthError::BadResponse {
            url: auth_url.to_string(),
        }
    })?;
    (auth.prompt)(Prompt::OpenUrl(url.to_string()));

    let code = tokio::time::timeout(auth.wait, redirect_code(&listener, &state))
        .await
        .map_err(|_elapsed| OAuthError::TimedOut)??;
    let form = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code),
        ("redirect_uri", redirect),
        ("code_verifier", verifier),
    ];
    post_token(client, cfg, &cfg.token_url, form).await
}

/// Waits on the loopback port for the browser's redirect and returns its code.
/// Requests for other paths (a browser asks for `/favicon.ico`) get a 404.
async fn redirect_code(
    listener: &tokio::net::TcpListener,
    state: &str,
) -> Result<String, OAuthError> {
    loop {
        let (mut sock, _) = listener.accept().await.map_err(OAuthError::Listen)?;
        let mut head = Vec::new();
        let mut buf = [0u8; 2048];
        while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 16 * 1024 {
            let n = sock.read(&mut buf).await.map_err(OAuthError::Listen)?;
            if n == 0 {
                break;
            }
            head.extend_from_slice(&buf[..n]);
        }
        let line = String::from_utf8_lossy(&head);
        let target = line.split_whitespace().nth(1).unwrap_or_default();
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        if path != "/callback" {
            // A failed write only means the browser closed the connection.
            sock.write_all(page(404, "Not found.").as_bytes())
                .await
                .ok();
            continue;
        }
        let params: Vec<(String, String)> = reqwest::Url::parse(&format!("http://h/?{query}"))
            .map(|u| u.query_pairs().into_owned().collect())
            .unwrap_or_default();
        let get = |k: &str| params.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        let (status, body, result) = if get("state").as_deref() != Some(state) {
            (
                400,
                "This sign-in link is not the one Reqlite opened.",
                Err(OAuthError::StateMismatch),
            )
        } else if let Some(err) = get("error") {
            (
                400,
                "The sign-in was refused. You can close this tab.",
                Err(OAuthError::Denied(err)),
            )
        } else if let Some(code) = get("code") {
            (
                200,
                "Signed in. You can close this tab and go back to Reqlite.",
                Ok(code),
            )
        } else {
            (
                400,
                "The redirect carried no code.",
                Err(OAuthError::Denied("no code".into())),
            )
        };
        sock.write_all(page(status, body).as_bytes()).await.ok();
        return result;
    }
}

fn page(status: u16, text: &str) -> String {
    let reason = if status == 200 { "OK" } else { "Bad Request" };
    let body = format!("<!doctype html><title>Reqlite</title><p>{text}</p>");
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[derive(Deserialize)]
struct DeviceAnswer {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    expires_in: Option<u64>,
    interval: Option<u64>,
}

async fn device_code(
    client: &reqwest::Client,
    cfg: &OAuthConfig,
    auth: &Authorizer<'_>,
) -> Result<Token, OAuthError> {
    let device_url = cfg.device_url.as_deref().unwrap_or_default();
    let mut form = vec![];
    if let Some(scope) = &cfg.scope {
        form.push(("scope", scope.clone()));
    }
    let answer: DeviceAnswer = post_json(client, cfg, device_url, form).await?;
    (auth.prompt)(Prompt::DeviceCode {
        url: answer
            .verification_uri_complete
            .unwrap_or(answer.verification_uri),
        code: answer.user_code,
    });
    let lifetime = Duration::from_secs(answer.expires_in.unwrap_or(600)).min(auth.wait);
    let deadline = tokio::time::Instant::now() + lifetime;
    let mut interval = answer.interval.unwrap_or(5);
    loop {
        tokio::time::sleep_until(
            (tokio::time::Instant::now() + Duration::from_secs(interval)).min(deadline),
        )
        .await;
        if tokio::time::Instant::now() >= deadline {
            return Err(OAuthError::TimedOut);
        }
        let form = vec![
            (
                "grant_type",
                "urn:ietf:params:oauth:grant-type:device_code".to_string(),
            ),
            ("device_code", answer.device_code.clone()),
        ];
        match post_token(client, cfg, &cfg.token_url, form).await {
            Ok(token) => return Ok(token),
            Err(OAuthError::Server { error, .. }) if error == "authorization_pending" => {}
            Err(OAuthError::Server { error, .. }) if error == "slow_down" => interval += 5,
            Err(OAuthError::Server { error, .. }) if error == "expired_token" => {
                return Err(OAuthError::TimedOut);
            }
            Err(OAuthError::Server { error, .. }) if error == "access_denied" => {
                return Err(OAuthError::Denied(error));
            }
            Err(e) => return Err(e),
        }
    }
}

#[derive(Deserialize)]
struct TokenAnswer {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

async fn post_token(
    client: &reqwest::Client,
    cfg: &OAuthConfig,
    url: &str,
    form: Vec<(&str, String)>,
) -> Result<Token, OAuthError> {
    let answer: TokenAnswer = post_json(client, cfg, url, form).await?;
    if answer.access_token.is_empty() {
        return Err(OAuthError::BadResponse { url: url.into() });
    }
    Ok(Token {
        access_token: answer.access_token,
        refresh_token: answer.refresh_token,
        expires_at: answer.expires_in.map(|s| now() + s),
    })
}

#[derive(Deserialize)]
struct ErrorAnswer {
    error: String,
    error_description: Option<String>,
}

/// POSTs a form with the client's credentials, and reads a JSON answer.
async fn post_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    cfg: &OAuthConfig,
    url: &str,
    mut form: Vec<(&str, String)>,
) -> Result<T, OAuthError> {
    let http = |source| OAuthError::Http {
        url: url.to_string(),
        source,
    };
    let mut builder = client.post(url).header("Accept", "application/json");
    match (&cfg.client_secret, cfg.client_auth) {
        (Some(secret), ClientAuth::Basic) => {
            builder = builder.basic_auth(&cfg.client_id, Some(secret));
        }
        (secret, _) => {
            form.push(("client_id", cfg.client_id.clone()));
            if let Some(secret) = secret {
                form.push(("client_secret", secret.clone()));
            }
        }
    }
    let resp = builder.form(&form).send().await.map_err(http)?;
    let status = resp.status();
    let bytes = resp.bytes().await.map_err(http)?;
    if status.is_success() {
        return serde_json::from_slice(&bytes)
            .map_err(|_bad| OAuthError::BadResponse { url: url.into() });
    }
    match serde_json::from_slice::<ErrorAnswer>(&bytes) {
        Ok(e) => Err(OAuthError::Server {
            url: url.into(),
            status: status.as_u16(),
            error: e.error,
            description: e.error_description,
        }),
        Err(_) => Err(OAuthError::Server {
            url: url.into(),
            status: status.as_u16(),
            error: status.canonical_reason().unwrap_or("error").to_string(),
            description: None,
        }),
    }
}

fn random(n: usize) -> Result<Vec<u8>, OAuthError> {
    let mut bytes = vec![0u8; n];
    getrandom::fill(&mut bytes).map_err(|e| OAuthError::Random(e.to_string()))?;
    Ok(bytes)
}

/// Base64 with the URL-safe alphabet and no padding (RFC 7636, appendix A).
fn base64url(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_the_rfc_7636_example() {
        // RFC 7636, appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            base64url(&Sha256::digest(verifier.as_bytes())),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"foob"), "Zm9vYg");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn a_token_is_renewed_before_it_runs_out() {
        let t = |at| Token {
            access_token: "a".into(),
            refresh_token: None,
            expires_at: at,
        };
        assert!(t(None).usable(1000), "no lifetime given");
        assert!(t(Some(1031)).usable(1000));
        assert!(!t(Some(1030)).usable(1000), "within 30 s of its end");
        assert!(!t(Some(900)).usable(1000));
    }

    #[test]
    fn debug_output_hides_tokens_and_secrets() {
        let token = Token {
            access_token: "at-secret".into(),
            refresh_token: Some("rt-secret".into()),
            expires_at: Some(5),
        };
        let cfg = OAuthConfig {
            grant: Grant::ClientCredentials,
            token_url: "https://id/token".into(),
            auth_url: None,
            device_url: None,
            client_id: "app".into(),
            client_secret: Some("cs-secret".into()),
            scope: None,
            client_auth: ClientAuth::Basic,
        };
        let shown = format!("{token:?} {cfg:?}");
        assert!(!shown.contains("secret"), "{shown}");
    }
}
