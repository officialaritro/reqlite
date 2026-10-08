//! Fills `{{var}}` placeholders from an [`Environment`]. Pure: no IO, no clock.

use crate::oauth::OAuthConfig;
use reqlite_format::{
    Assert, Auth, Body, Environment, KeyIn, Method, Request, Source, Var, is_var_name,
};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// A request with every placeholder filled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parts {
    pub method: Method,
    pub url: String,
    /// Includes what the body type and the auth add: a Content-Type for JSON
    /// and forms, and the auth header.
    pub headers: Vec<(String, String)>,
    pub query: Vec<(String, String)>,
    pub body: Option<SendBody>,
}

/// A body ready to send. JSON and forms are text by now, with their
/// Content-Type in the headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendBody {
    Text(String),
    Multipart(Vec<SendPart>),
    /// Read from disk and streamed at send time.
    File(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendPart {
    pub name: String,
    pub value: PartValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartValue {
    Text(String),
    File {
        path: PathBuf,
        content_type: Option<String>,
    },
}

impl SendBody {
    /// The body as text, for history and display. Files show as `@path`.
    pub fn describe(&self) -> String {
        match self {
            SendBody::Text(t) => t.clone(),
            SendBody::File(path) => format!("@{}", path.display()),
            SendBody::Multipart(parts) => parts
                .iter()
                .map(|p| match &p.value {
                    PartValue::Text(t) => format!("{}: {t}\n", p.name),
                    PartValue::File { path, .. } => format!("{}: @{}\n", p.name, path.display()),
                })
                .collect(),
        }
    }
}

/// The only input [`crate::send`] accepts. The sent form holds secret values and
/// stays private. Callers see the redacted form, where secrets remain `{{name}}`.
#[derive(Clone)]
pub struct Resolved {
    sent: Parts,
    redacted: Parts,
    /// The OAuth 2.0 client, when the request's auth is `oauth2`. Its token is
    /// fetched at send time.
    oauth: Option<OAuthConfig>,
    /// Secret values this request uses, longest first, with their names.
    secrets: Vec<(String, String)>,
    /// Each assertion with its value filled, and its text to show, where
    /// secrets stay `{{name}}`.
    asserts: Vec<(Assert, String)>,
    capture: BTreeMap<String, Source>,
}

impl Resolved {
    pub fn redacted(&self) -> &Parts {
        &self.redacted
    }

    /// Replaces every secret value this request used with `{{name}}`. Servers
    /// echo headers back, so anything stored from a response goes through this.
    pub fn redact(&self, bytes: &[u8]) -> Vec<u8> {
        let mut out = bytes.to_vec();
        for (name, value) in &self.secrets {
            out = replace(&out, value.as_bytes(), format!("{{{{{name}}}}}").as_bytes());
        }
        out
    }

    /// The longest secret this request used, in bytes.
    pub fn longest_secret(&self) -> usize {
        self.secrets.first().map_or(0, |(_, v)| v.len())
    }

    pub(crate) fn sent(&self) -> &Parts {
        &self.sent
    }

    pub(crate) fn oauth(&self) -> Option<&OAuthConfig> {
        self.oauth.as_ref()
    }

    pub(crate) fn asserts(&self) -> &[(Assert, String)] {
        &self.asserts
    }

    pub(crate) fn capture(&self) -> &BTreeMap<String, Source> {
        &self.capture
    }
}

impl fmt::Debug for Resolved {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Resolved").field(&self.redacted).finish()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ResolveError {
    #[error("{{{{{name}}}}} in {field} is not defined in the environment")]
    Undefined { name: String, field: String },
    #[error(
        "secret {{{{{name}}}}} in {field} has no value; set it with `reqlite secret set` or in the environment's .local.toml file"
    )]
    MissingSecret { name: String, field: String },
    #[error("secret {{{{{name}}}}} in {field} cannot be read: {reason}")]
    SecretUnavailable {
        name: String,
        field: String,
        reason: String,
    },
    #[error("{{{{ in {field} is not closed with }}}}")]
    Unclosed { field: String },
    #[error("invalid variable name {name:?} in {field}")]
    InvalidName { name: String, field: String },
    #[error("header {name:?} has a control character after substitution")]
    HeaderValue { name: String },
}

/// `name=value` in `application/x-www-form-urlencoded`: unreserved bytes stay,
/// a space is `+`, everything else is `%XX`. The shown copy keeps braces, so a
/// `{{secret}}` reads as itself in history.
fn form_pair(name: &str, value: &str, shown: bool) -> String {
    format!("{}={}", form_encode(name, shown), form_encode(value, shown))
}

fn form_encode(s: &str, keep_braces: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'*' => {
                out.push(b as char);
            }
            b'{' | b'}' if keep_braces => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Standard base64 with padding (RFC 4648), for Basic auth.
fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.len();
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let v = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= n {
                out.push(ABC[((v >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub(crate) fn replace(haystack: &[u8], needle: &[u8], with: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(haystack.len());
    let mut i = 0;
    while i < haystack.len() {
        if haystack[i..].starts_with(needle) {
            out.extend_from_slice(with);
            i += needle.len();
        } else {
            out.push(haystack[i]);
            i += 1;
        }
    }
    out
}

/// [`resolve_in`] with body files relative to the current folder.
pub fn resolve(req: &Request, env: &Environment) -> Result<Resolved, ResolveError> {
    resolve_in(req, env, Path::new(""))
}

/// Fills every placeholder. Body file paths are relative to `dir`, the folder
/// of the request file.
pub fn resolve_in(req: &Request, env: &Environment, dir: &Path) -> Result<Resolved, ResolveError> {
    let mut used = BTreeMap::new();
    let mut fill = |text: &str, field: &str| fill(text, env, field, &mut used);
    let (url, url_r) = fill(&req.url, "url")?;
    let mut headers = (Vec::new(), Vec::new());
    for (name, value) in req.headers.pairs() {
        let (v, v_r) = fill(value, &format!("header {name}"))?;
        if v.chars().any(|c| c.is_control() && c != '\t') {
            return Err(ResolveError::HeaderValue { name: name.into() });
        }
        headers.0.push((name.to_string(), v));
        headers.1.push((name.to_string(), v_r));
    }
    let mut query = (Vec::new(), Vec::new());
    for (name, value) in req.query.pairs() {
        let (v, v_r) = fill(value, &format!("query {name}"))?;
        query.0.push((name.to_string(), v));
        query.1.push((name.to_string(), v_r));
    }
    let has_type = req
        .headers
        .pairs()
        .any(|(n, _)| n.eq_ignore_ascii_case("Content-Type"));
    let mut add_type = |ct: &str| {
        if !has_type {
            headers.0.push(("Content-Type".into(), ct.into()));
            headers.1.push(("Content-Type".into(), ct.into()));
        }
    };
    let path = |p: &str| dir.join(p);
    let (body, body_r) = match &req.body {
        None => (None, None),
        Some(Body::Text(t)) => {
            let (v, v_r) = fill(t, "body")?;
            (Some(SendBody::Text(v)), Some(SendBody::Text(v_r)))
        }
        Some(Body::Json(t)) => {
            add_type("application/json");
            let (v, v_r) = fill(t, "body")?;
            (Some(SendBody::Text(v)), Some(SendBody::Text(v_r)))
        }
        Some(Body::Form(fields)) => {
            add_type("application/x-www-form-urlencoded");
            let (mut sent, mut shown) = (Vec::new(), Vec::new());
            for (name, value) in fields.pairs() {
                let (v, v_r) = fill(value, &format!("form {name}"))?;
                sent.push(form_pair(name, &v, false));
                shown.push(form_pair(name, &v_r, true));
            }
            (
                Some(SendBody::Text(sent.join("&"))),
                Some(SendBody::Text(shown.join("&"))),
            )
        }
        Some(Body::Multipart(list)) => {
            let (mut sent, mut shown) = (Vec::new(), Vec::new());
            for p in list {
                let (v, v_r) = match (&p.text, &p.file) {
                    (Some(t), _) => {
                        let (v, v_r) = fill(t, &format!("part {}", p.name))?;
                        (PartValue::Text(v), PartValue::Text(v_r))
                    }
                    (None, file) => {
                        let v = PartValue::File {
                            path: path(file.as_deref().unwrap_or_default()),
                            content_type: p.content_type.clone(),
                        };
                        (v.clone(), v)
                    }
                };
                sent.push(SendPart {
                    name: p.name.clone(),
                    value: v,
                });
                shown.push(SendPart {
                    name: p.name.clone(),
                    value: v_r,
                });
            }
            (
                Some(SendBody::Multipart(sent)),
                Some(SendBody::Multipart(shown)),
            )
        }
        Some(Body::File { path: p }) => {
            (Some(SendBody::File(path(p))), Some(SendBody::File(path(p))))
        }
    };
    let mut extra_secret = None;
    let mut oauth = None;
    match &req.auth {
        None => {}
        Some(Auth::Oauth2(o)) => {
            let mut sent = |text: &str, field: &str| fill(text, field).map(|(v, _)| v);
            let opt =
                |v: &Option<String>,
                 field: &str,
                 sent: &mut dyn FnMut(&str, &str) -> Result<String, ResolveError>| {
                    v.as_deref().map(|t| sent(t, field)).transpose()
                };
            let config = OAuthConfig {
                grant: o.grant,
                token_url: sent(&o.token_url, "oauth2 token_url")?,
                auth_url: opt(&o.auth_url, "oauth2 auth_url", &mut sent)?,
                device_url: opt(&o.device_url, "oauth2 device_url", &mut sent)?,
                client_id: sent(&o.client_id, "oauth2 client_id")?,
                client_secret: opt(&o.client_secret, "oauth2 client_secret", &mut sent)?,
                scope: opt(&o.scope, "oauth2 scope", &mut sent)?,
                client_auth: o.client_auth,
            };
            // The token itself is added at send time. History shows this.
            headers
                .1
                .push(("Authorization".into(), "Bearer {{oauth_token}}".into()));
            oauth = Some(config);
        }
        Some(Auth::Bearer { token }) => {
            let (v, v_r) = fill(token, "auth token")?;
            headers
                .0
                .push(("Authorization".into(), format!("Bearer {v}")));
            headers
                .1
                .push(("Authorization".into(), format!("Bearer {v_r}")));
        }
        Some(Auth::Basic { username, password }) => {
            let (u, u_r) = fill(username, "auth username")?;
            let (p, p_r) = fill(password, "auth password")?;
            let sent = base64(format!("{u}:{p}").as_bytes());
            let shown = base64(format!("{u_r}:{p_r}").as_bytes());
            if sent != shown {
                // The encoded pair holds a secret; a server may echo it back.
                extra_secret = Some(("auth".to_string(), sent.clone()));
            }
            headers
                .0
                .push(("Authorization".into(), format!("Basic {sent}")));
            headers
                .1
                .push(("Authorization".into(), format!("Basic {shown}")));
        }
        Some(Auth::ApiKey {
            name,
            value,
            location,
        }) => {
            let (v, v_r) = fill(value, "auth key")?;
            let target = match location {
                KeyIn::Header => &mut headers,
                KeyIn::Query => &mut query,
            };
            target.0.push((name.clone(), v));
            target.1.push((name.clone(), v_r));
        }
    }
    let mut asserts = Vec::new();
    for a in &req.assert {
        let (v, v_r) = fill(&a.value, "assert")?;
        let shown = Assert {
            value: v_r,
            ..a.clone()
        };
        asserts.push((
            Assert {
                value: v,
                ..a.clone()
            },
            shown.to_string(),
        ));
    }
    for (name, value) in &headers.0 {
        if value.chars().any(|c| c.is_control() && c != '\t') {
            return Err(ResolveError::HeaderValue { name: name.clone() });
        }
    }
    let mut secrets: Vec<(String, String)> = used
        .into_iter()
        .filter(|(_, v): &(String, String)| !v.is_empty())
        .chain(extra_secret)
        .collect();
    secrets.sort_by_key(|(_, v)| std::cmp::Reverse(v.len()));
    Ok(Resolved {
        oauth,
        secrets,
        asserts,
        capture: req.capture.clone(),
        sent: Parts {
            method: req.method.clone(),
            url,
            headers: headers.0,
            query: query.0,
            body,
        },
        redacted: Parts {
            method: req.method.clone(),
            url: url_r,
            headers: headers.1,
            query: query.1,
            body: body_r,
        },
    })
}

/// Returns the text to send and the text to show or store. Substituted values
/// are not scanned again, so a value containing `{{` is sent as is.
fn fill(
    text: &str,
    env: &Environment,
    field: &str,
    secrets: &mut BTreeMap<String, String>,
) -> Result<(String, String), ResolveError> {
    let mut sent = String::with_capacity(text.len());
    let mut shown = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        sent.push_str(&rest[..start]);
        shown.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find("}}").ok_or_else(|| ResolveError::Unclosed {
            field: field.into(),
        })?;
        let name = after[..end].trim();
        if !is_var_name(name) {
            return Err(ResolveError::InvalidName {
                name: name.into(),
                field: field.into(),
            });
        }
        match env.get(name) {
            Some(Var::Plain(v)) => {
                sent.push_str(v);
                shown.push_str(v);
            }
            Some(Var::Secret(v)) => {
                secrets.insert(name.to_string(), v.clone());
                sent.push_str(v);
                shown.push_str("{{");
                shown.push_str(name);
                shown.push_str("}}");
            }
            Some(Var::Unavailable(reason)) => {
                return Err(ResolveError::SecretUnavailable {
                    name: name.into(),
                    field: field.into(),
                    reason: reason.clone(),
                });
            }
            Some(Var::MissingSecret) => {
                return Err(ResolveError::MissingSecret {
                    name: name.into(),
                    field: field.into(),
                });
            }
            None => {
                return Err(ResolveError::Undefined {
                    name: name.into(),
                    field: field.into(),
                });
            }
        }
        rest = &after[end + 2..];
    }
    sent.push_str(rest);
    shown.push_str(rest);
    Ok((sent, shown))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn env() -> Environment {
        reqlite_format::parse_env(
            Path::new("dev.toml"),
            "version = 1\nsecrets = ['token', 'unset']\n[vars]\nbase = 'http://api'\nloop = '{{base}}'\n",
            Some("version = 1\n[vars]\ntoken = 's3cret'\n"),
        )
        .unwrap()
    }

    fn req(extra: &str) -> Request {
        reqlite_format::parse(&format!("version = 1\nname = 't'\n{extra}")).unwrap()
    }

    #[test]
    fn fills_every_field_and_redacts_secrets() {
        let r = resolve(
            &req("url = '{{base}}/u'\nbody = 'k={{ token }}'\n[headers]\nAuthorization = 'Bearer {{token}}'\n[query]\nq = '{{loop}}'\n"),
            &env(),
        )
        .unwrap();
        assert_eq!(r.sent().url, "http://api/u");
        assert_eq!(
            r.sent().headers,
            [("Authorization".into(), "Bearer s3cret".into())]
        );
        assert_eq!(r.sent().body, Some(SendBody::Text("k=s3cret".into())));
        assert_eq!(r.sent().query, [("q".into(), "{{base}}".into())]);
        assert_eq!(
            r.redacted().headers,
            [("Authorization".into(), "Bearer {{token}}".into())]
        );
        assert_eq!(
            r.redacted().body,
            Some(SendBody::Text("k={{token}}".into()))
        );
        assert!(!format!("{r:?}").contains("s3cret"));
        assert_eq!(
            r.redact(b"echo: Bearer s3cret, s3cret"),
            b"echo: Bearer {{token}}, {{token}}"
        );
    }

    #[test]
    fn redact_leaves_bytes_alone_when_no_secret_is_used() {
        let r = resolve(&req("url = '{{base}}'\n"), &env()).unwrap();
        assert_eq!(r.redact(b"s3cret http://api"), b"s3cret http://api");
        assert_eq!(r.longest_secret(), 0);
    }

    #[test]
    fn reports_each_failure_with_the_name_and_field() {
        let cases = [
            (
                "url = '{{nope}}/x'\n",
                ResolveError::Undefined {
                    name: "nope".into(),
                    field: "url".into(),
                },
            ),
            (
                "url = 'http://a'\nbody = '{{unset}}'\n",
                ResolveError::MissingSecret {
                    name: "unset".into(),
                    field: "body".into(),
                },
            ),
            (
                "url = 'http://a/{{base'\n",
                ResolveError::Unclosed {
                    field: "url".into(),
                },
            ),
            (
                "url = 'http://a'\n[query]\nq = '{{a b}}'\n",
                ResolveError::InvalidName {
                    name: "a b".into(),
                    field: "query q".into(),
                },
            ),
        ];
        for (extra, want) in cases {
            assert_eq!(resolve(&req(extra), &env()).unwrap_err(), want, "{extra}");
        }
    }

    #[test]
    fn rejects_a_header_that_a_value_breaks() {
        let env = reqlite_format::parse_env(
            Path::new("d.toml"),
            "version = 1\n[vars]\nnl = \"a\\nb\"\n",
            None,
        )
        .unwrap();
        let err = resolve(&req("url = 'http://a'\n[headers]\nX = '{{nl}}'\n"), &env).unwrap_err();
        assert_eq!(err, ResolveError::HeaderValue { name: "X".into() });
    }

    #[test]
    fn error_text_shows_the_placeholder() {
        let err = resolve(&req("url = '{{nope}}'\n"), &env()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "{{nope}} in url is not defined in the environment"
        );
    }

    fn req2(extra: &str) -> Request {
        reqlite_format::parse(&format!(
            "version = 2\nname = 't'\nurl = 'http://a'\n{extra}"
        ))
        .unwrap()
    }

    #[test]
    fn base64_matches_the_rfc_vectors() {
        for (plain, coded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
            ("Aladdin:open sesame", "QWxhZGRpbjpvcGVuIHNlc2FtZQ=="),
        ] {
            assert_eq!(base64(plain.as_bytes()), coded, "{plain:?}");
        }
    }

    #[test]
    fn json_and_forms_add_their_content_type_unless_one_is_set() {
        let ct = |r: &Resolved| {
            r.sent()
                .headers
                .iter()
                .filter(|(n, _)| n.eq_ignore_ascii_case("content-type"))
                .map(|(_, v)| v.clone())
                .collect::<Vec<_>>()
        };
        let json = resolve(&req2("[body]\ntype = 'json'\ntext = '{}'\n"), &env()).unwrap();
        assert_eq!(ct(&json), ["application/json"]);
        let own = resolve(
            &req2("[headers]\ncontent-type = 'application/vnd.x+json'\n[body]\ntype = 'json'\ntext = '{}'\n"),
            &env(),
        )
        .unwrap();
        assert_eq!(ct(&own), ["application/vnd.x+json"]);
        let text = resolve(&req("url = 'http://a'\nbody = 'raw'\n"), &env()).unwrap();
        assert!(ct(&text).is_empty(), "plain text adds nothing");
    }

    #[test]
    fn form_values_fill_placeholders_and_hide_secrets() {
        let r = resolve(
            &req2("[body]\ntype = 'form'\n[body.fields]\nkey = '{{token}}'\nq = 'a b+c'\n"),
            &env(),
        )
        .unwrap();
        assert_eq!(
            r.sent().body,
            Some(SendBody::Text("key=s3cret&q=a+b%2Bc".into()))
        );
        assert_eq!(
            r.redacted().body,
            Some(SendBody::Text("key={{token}}&q=a+b%2Bc".into()))
        );
    }

    #[test]
    fn file_paths_are_relative_to_the_request_folder() {
        let r = resolve_in(
            &req2("[body]\ntype = 'multipart'\n[[body.parts]]\nname = 'a'\nfile = 'f/x.bin'\n[[body.parts]]\nname = 'b'\ntext = '{{base}}'\n"),
            &env(),
            Path::new("/work/api"),
        )
        .unwrap();
        let Some(SendBody::Multipart(parts)) = &r.sent().body else {
            panic!("{:?}", r.sent().body)
        };
        assert_eq!(
            parts[0].value,
            PartValue::File {
                path: PathBuf::from("/work/api/f/x.bin"),
                content_type: None
            }
        );
        assert_eq!(parts[1].value, PartValue::Text("http://api".into()));
        // The platform's own separator joins the folder and the file.
        let file = Path::new("/work/api").join("f/x.bin");
        assert_eq!(
            r.sent().body.as_ref().unwrap().describe(),
            format!("a: @{}\nb: http://api\n", file.display())
        );
    }

    #[test]
    fn auth_values_fill_placeholders_and_secrets_stay_hidden() {
        let bearer = resolve(
            &req2("[auth]\ntype = 'bearer'\ntoken = '{{token}}'\n"),
            &env(),
        )
        .unwrap();
        assert_eq!(
            bearer.sent().headers,
            [("Authorization".into(), "Bearer s3cret".into())]
        );
        assert_eq!(
            bearer.redacted().headers,
            [("Authorization".into(), "Bearer {{token}}".into())]
        );

        let basic = resolve(
            &req2("[auth]\ntype = 'basic'\nusername = 'ada'\npassword = '{{token}}'\n"),
            &env(),
        )
        .unwrap();
        let sent = format!("Basic {}", base64(b"ada:s3cret"));
        assert_eq!(basic.sent().headers, [("Authorization".into(), sent)]);
        assert_eq!(
            basic.redacted().headers,
            [(
                "Authorization".into(),
                format!("Basic {}", base64(b"ada:{{token}}"))
            )]
        );
        let echo = format!("seen {}", base64(b"ada:s3cret"));
        assert_eq!(
            basic.redact(echo.as_bytes()),
            b"seen {{auth}}",
            "an echoed header is hidden"
        );

        let key = resolve(
            &req2("[auth]\ntype = 'api_key'\nname = 'X-Key'\nvalue = '{{token}}'\n"),
            &env(),
        )
        .unwrap();
        assert_eq!(key.sent().headers, [("X-Key".into(), "s3cret".into())]);
    }

    #[test]
    fn a_secret_that_cannot_be_read_fails_only_where_it_is_used() {
        let mut env = env();
        env.supply("unset", Err("the keychain is locked".into()));
        let err = resolve(&req("url = 'http://a'\nbody = '{{unset}}'\n"), &env).unwrap_err();
        assert_eq!(
            err,
            ResolveError::SecretUnavailable {
                name: "unset".into(),
                field: "body".into(),
                reason: "the keychain is locked".into(),
            }
        );
        assert_eq!(
            err.to_string(),
            "secret {{unset}} in body cannot be read: the keychain is locked"
        );
        assert!(
            resolve(&req("url = '{{base}}'\n"), &env).is_ok(),
            "a request that does not use it still resolves"
        );
    }

    #[test]
    fn a_secret_with_no_value_names_both_ways_to_set_it() {
        let err = resolve(&req("url = 'http://a'\nbody = '{{unset}}'\n"), &env()).unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains("reqlite secret set") && text.contains(".local.toml"),
            "{text}"
        );
    }
}
