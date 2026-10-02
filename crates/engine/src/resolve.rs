//! Fills `{{var}}` placeholders from an [`Environment`]. Pure: no IO, no clock.

use reqlite_format::{Environment, Method, Request, Var, is_var_name};
use std::collections::BTreeMap;
use std::fmt;

/// A request with every placeholder filled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parts {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub query: Vec<(String, String)>,
    pub body: Option<String>,
}

/// The only input [`crate::send`] accepts. The sent form holds secret values and
/// stays private. Callers see the redacted form, where secrets remain `{{name}}`.
#[derive(Clone)]
pub struct Resolved {
    sent: Parts,
    redacted: Parts,
    /// Secret values this request uses, longest first, with their names.
    secrets: Vec<(String, String)>,
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
        "secret {{{{{name}}}}} in {field} has no value; set it in the environment's .local.toml file"
    )]
    MissingSecret { name: String, field: String },
    #[error("{{{{ in {field} is not closed with }}}}")]
    Unclosed { field: String },
    #[error("invalid variable name {name:?} in {field}")]
    InvalidName { name: String, field: String },
    #[error("header {name:?} has a control character after substitution")]
    HeaderValue { name: String },
}

fn replace(haystack: &[u8], needle: &[u8], with: &[u8]) -> Vec<u8> {
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

pub fn resolve(req: &Request, env: &Environment) -> Result<Resolved, ResolveError> {
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
    let (body, body_r) = match &req.body {
        Some(b) => {
            let (v, v_r) = fill(b, "body")?;
            (Some(v), Some(v_r))
        }
        None => (None, None),
    };
    let mut secrets: Vec<(String, String)> = used
        .into_iter()
        .filter(|(_, v): &(String, String)| !v.is_empty())
        .collect();
    secrets.sort_by_key(|(_, v)| std::cmp::Reverse(v.len()));
    Ok(Resolved {
        secrets,
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
        assert_eq!(r.sent().body.as_deref(), Some("k=s3cret"));
        assert_eq!(r.sent().query, [("q".into(), "{{base}}".into())]);
        assert_eq!(
            r.redacted().headers,
            [("Authorization".into(), "Bearer {{token}}".into())]
        );
        assert_eq!(r.redacted().body.as_deref(), Some("k={{token}}"));
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
}
