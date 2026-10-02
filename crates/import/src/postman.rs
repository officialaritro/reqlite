//! Postman collections, schema v2.0 and v2.1. Each folder becomes a directory
//! and each request a file. Postman and Reqlite both write `{{name}}`, so
//! placeholders carry over unchanged.

use crate::{Warning, is_literal_credential};
use reqlite_format::{Method, Params, Request, VERSION};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum PostmanError {
    #[error("not a Postman collection")]
    Json(#[source] serde_json::Error),
    #[error("unsupported Postman schema {0:?}; export the collection as v2.1")]
    Schema(String),
    #[error("{item}: {reason}")]
    Invalid { item: String, reason: String },
}

#[derive(Debug)]
pub struct Collection {
    pub name: String,
    /// Paths relative to the output directory, in collection order.
    pub files: Vec<(PathBuf, Request)>,
    pub warnings: Vec<Warning>,
}

#[derive(Deserialize)]
struct Root {
    info: Info,
    #[serde(default)]
    item: Vec<Item>,
    #[serde(default)]
    event: Vec<Event>,
    #[serde(default)]
    variable: Vec<Kv>,
    auth: Option<Auth>,
}

#[derive(Deserialize)]
struct Info {
    name: String,
    #[serde(default)]
    schema: String,
}

#[derive(Deserialize)]
struct Item {
    #[serde(default)]
    name: String,
    item: Option<Vec<Item>>,
    request: Option<RequestOrUrl>,
    #[serde(default)]
    event: Vec<Event>,
    auth: Option<Auth>,
    #[serde(default)]
    response: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RequestOrUrl {
    Url(String),
    Request(Box<PRequest>),
}

#[derive(Deserialize)]
struct PRequest {
    method: Option<String>,
    #[serde(default)]
    header: Vec<Kv>,
    url: Option<Url>,
    body: Option<Body>,
    auth: Option<Auth>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Url {
    Text(String),
    Parts(UrlParts),
}

#[derive(Deserialize)]
struct UrlParts {
    raw: Option<String>,
    protocol: Option<String>,
    #[serde(default)]
    host: Value,
    #[serde(default)]
    path: Value,
    #[serde(default)]
    query: Vec<Kv>,
}

#[derive(Deserialize)]
struct Kv {
    #[serde(default)]
    key: String,
    #[serde(default)]
    value: Value,
    #[serde(default)]
    disabled: bool,
}

#[derive(Deserialize)]
struct Body {
    #[serde(default)]
    mode: String,
    raw: Option<String>,
    #[serde(default)]
    urlencoded: Vec<Kv>,
    graphql: Option<Graphql>,
    #[serde(default)]
    disabled: bool,
}

#[derive(Deserialize)]
struct Graphql {
    #[serde(default)]
    query: String,
    #[serde(default)]
    variables: Value,
}

#[derive(Deserialize, Clone)]
struct Auth {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    bearer: Vec<AuthKv>,
    #[serde(default)]
    apikey: Vec<AuthKv>,
}

#[derive(Deserialize, Clone)]
struct AuthKv {
    key: String,
    #[serde(default)]
    value: Value,
}

#[derive(Deserialize)]
struct Event {
    #[serde(default)]
    listen: String,
    script: Option<Script>,
}

#[derive(Deserialize)]
struct Script {
    #[serde(default)]
    exec: Value,
}

pub fn import(json: &str) -> Result<Collection, PostmanError> {
    let root: Root = serde_json::from_str(json).map_err(PostmanError::Json)?;
    if !(root.info.schema.contains("/v2.1") || root.info.schema.contains("/v2.0")) {
        return Err(PostmanError::Schema(root.info.schema));
    }
    let mut out = Collection {
        name: root.info.name.clone(),
        files: Vec::new(),
        warnings: Vec::new(),
    };
    scripts(&root.event, &root.info.name, &mut out.warnings);
    if !root.variable.is_empty() {
        let names: Vec<_> = root.variable.iter().map(|v| v.key.as_str()).collect();
        out.warnings.push(Warning::Unsupported {
            option: format!(
                "collection variables ({}); put them in an environment file",
                names.join(", ")
            ),
            item: Some(root.info.name.clone()),
        });
    }
    walk(&root.item, Path::new(""), root.auth.as_ref(), &mut out)?;
    Ok(out)
}

fn walk(
    items: &[Item],
    dir: &Path,
    auth: Option<&Auth>,
    out: &mut Collection,
) -> Result<(), PostmanError> {
    let mut taken = BTreeSet::new();
    for item in items {
        let auth = item.auth.as_ref().or(auth);
        let label = if item.name.is_empty() {
            "untitled"
        } else {
            &item.name
        };
        scripts(&item.event, label, &mut out.warnings);
        if let Some(children) = &item.item {
            let sub = dir.join(unique(&mut taken, &file_name(label), ""));
            walk(children, &sub, auth, out)?;
        } else if let Some(request) = &item.request {
            if !item.response.is_empty() {
                out.warnings.push(Warning::Unsupported {
                    option: format!("{} saved example responses", item.response.len()),
                    item: Some(label.into()),
                });
            }
            let req = request_of(label, request, auth, &mut out.warnings)?;
            let path = dir.join(unique(&mut taken, &file_name(label), ".toml"));
            out.files.push((path, req));
        }
    }
    Ok(())
}

fn request_of(
    label: &str,
    r: &RequestOrUrl,
    inherited: Option<&Auth>,
    warnings: &mut Vec<Warning>,
) -> Result<Request, PostmanError> {
    let invalid = |reason: String| PostmanError::Invalid {
        item: label.into(),
        reason,
    };
    let empty = PRequest {
        method: None,
        header: Vec::new(),
        url: None,
        body: None,
        auth: None,
    };
    let (r, url) = match r {
        RequestOrUrl::Url(u) => (&empty, u.clone()),
        RequestOrUrl::Request(r) => (r.as_ref(), url_of(r.url.as_ref())),
    };
    if let Some(Url::Parts(parts)) = &r.url {
        for q in parts.query.iter().filter(|q| q.disabled) {
            warnings.push(Warning::Unsupported {
                option: format!("disabled query parameter {}", q.key),
                item: Some(label.into()),
            });
        }
    }
    if url.is_empty() {
        return Err(invalid("the request has no URL".into()));
    }
    let method =
        Method::try_from(r.method.clone().unwrap_or_else(|| "GET".into())).map_err(invalid)?;

    let mut headers = Params::default();
    for h in &r.header {
        if h.disabled {
            warnings.push(Warning::Unsupported {
                option: format!("disabled header {}", h.key),
                item: Some(label.into()),
            });
        } else {
            headers.append(h.key.clone(), text(&h.value));
        }
    }

    match r.auth.as_ref().or(inherited) {
        None => {}
        Some(a) if a.kind == "noauth" || a.kind == "inherit" => {}
        Some(a) if a.kind == "bearer" => {
            let token = field(&a.bearer, "token");
            headers.append("Authorization", format!("Bearer {token}"));
        }
        Some(a) if a.kind == "apikey" && field(&a.apikey, "in") != "query" => {
            headers.append(field(&a.apikey, "key"), field(&a.apikey, "value"));
        }
        Some(a) if a.kind == "basic" => warnings.push(Warning::CredentialsSkipped {
            option: format!("{label}: basic auth"),
        }),
        Some(a) => warnings.push(Warning::Unsupported {
            option: format!("{} auth", a.kind),
            item: Some(label.into()),
        }),
    }

    let body = match &r.body {
        None => None,
        Some(b) if b.disabled => None,
        Some(b) => match b.mode.as_str() {
            "raw" => b.raw.clone().filter(|s| !s.is_empty()),
            "urlencoded" => {
                if headers.get("Content-Type").is_empty() {
                    headers.append("Content-Type", "application/x-www-form-urlencoded");
                }
                let mut pairs = Vec::new();
                for kv in &b.urlencoded {
                    if kv.disabled {
                        warnings.push(Warning::Unsupported {
                            option: format!("disabled form field {}", kv.key),
                            item: Some(label.into()),
                        });
                    } else {
                        pairs.push(format!("{}={}", form(&kv.key), form(&text(&kv.value))));
                    }
                }
                Some(pairs.join("&"))
            }
            "graphql" => {
                if headers.get("Content-Type").is_empty() {
                    headers.append("Content-Type", "application/json");
                }
                let g = b.graphql.as_ref();
                let query = g.map_or(String::new(), |g| g.query.clone());
                let vars = g.map_or(Value::Null, |g| match &g.variables {
                    Value::String(s) => serde_json::from_str(s).unwrap_or(Value::String(s.clone())),
                    v => v.clone(),
                });
                Some(serde_json::json!({ "query": query, "variables": vars }).to_string())
            }
            "file" => {
                warnings.push(Warning::BodyFromFile {
                    path: format!("{label} (Postman file body)"),
                });
                None
            }
            other => {
                warnings.push(Warning::Unsupported {
                    option: format!("{other} body"),
                    item: Some(label.into()),
                });
                None
            }
        },
    };

    for (name, value) in headers.pairs() {
        if is_literal_credential(name, value) {
            warnings.push(Warning::LiteralCredential {
                header: format!("{name} in {label}"),
            });
        }
    }
    let req = Request {
        version: VERSION,
        name: label.into(),
        method,
        url,
        headers,
        query: Params::default(),
        body,
    };
    reqlite_format::validate(&req).map_err(|e| invalid(e.to_string()))?;
    Ok(req)
}

fn url_of(url: Option<&Url>) -> String {
    match url {
        None => String::new(),
        Some(Url::Text(t)) => t.clone(),
        Some(Url::Parts(p)) => match &p.raw {
            Some(raw) => raw.clone(),
            None => {
                let join = |v: &Value, sep: &str| match v {
                    Value::Array(parts) => parts.iter().map(text).collect::<Vec<_>>().join(sep),
                    v => text(v),
                };
                let mut url = String::new();
                if let Some(proto) = &p.protocol {
                    url.push_str(proto);
                    url.push_str("://");
                }
                url.push_str(&join(&p.host, "."));
                let path = join(&p.path, "/");
                if !path.is_empty() {
                    url.push('/');
                    url.push_str(&path);
                }
                let query: Vec<String> = p
                    .query
                    .iter()
                    .filter(|q| !q.disabled)
                    .map(|q| format!("{}={}", q.key, text(&q.value)))
                    .collect();
                if !query.is_empty() {
                    url.push('?');
                    url.push_str(&query.join("&"));
                }
                url
            }
        },
    }
}

fn scripts(events: &[Event], label: &str, warnings: &mut Vec<Warning>) {
    for e in events {
        let has_code = match e.script.as_ref().map(|s| &s.exec) {
            Some(Value::Array(lines)) => lines.iter().any(|l| !text(l).trim().is_empty()),
            Some(Value::String(s)) => !s.trim().is_empty(),
            _ => false,
        };
        if has_code {
            warnings.push(Warning::Script {
                item: label.into(),
                listen: e.listen.clone(),
            });
        }
    }
}

fn field(kvs: &[AuthKv], key: &str) -> String {
    kvs.iter()
        .find(|kv| kv.key == key)
        .map_or(String::new(), |kv| text(&kv.value))
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        v => v.to_string(),
    }
}

/// `application/x-www-form-urlencoded` encoding. Braces stay for `{{placeholders}}`.
fn form(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b' ' => out.push('+'),
            b if b.is_ascii_alphanumeric() || b"-._~*{}".contains(&b) => out.push(char::from(b)),
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// A name safe on Windows, macOS and Linux.
fn file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '-'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_end_matches('.').trim();
    if cleaned.is_empty() {
        "untitled".into()
    } else {
        cleaned.into()
    }
}

/// Appends ` 2`, ` 3` and so on when a sibling already has the name. Comparison
/// ignores case, because macOS and Windows file systems do.
fn unique(taken: &mut BTreeSet<String>, base: &str, ext: &str) -> String {
    let mut n = 1;
    loop {
        let name = if n == 1 {
            format!("{base}{ext}")
        } else {
            format!("{base} {n}{ext}")
        };
        if taken.insert(name.to_lowercase()) {
            return name;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COLLECTION: &str = r#"{
  "info": { "name": "Shop", "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json" },
  "auth": { "type": "bearer", "bearer": [{ "key": "token", "value": "{{token}}", "type": "string" }] },
  "event": [{ "listen": "prerequest", "script": { "exec": ["pm.environment.set('t', Date.now())"] } }],
  "variable": [{ "key": "base", "value": "http://localhost:3000" }],
  "item": [
    {
      "name": "Users / Admin",
      "item": [
        {
          "name": "List users",
          "request": {
            "method": "GET",
            "header": [
              { "key": "Accept", "value": "application/json" },
              { "key": "X-Debug", "value": "1", "disabled": true }
            ],
            "url": { "raw": "{{base}}/users?page=1", "host": ["{{base}}"], "path": ["users"] }
          },
          "event": [{ "listen": "test", "script": { "exec": ["pm.test('ok', () => {})"] } }],
          "response": [{ "name": "200" }]
        },
        {
          "name": "Login",
          "request": {
            "method": "POST",
            "auth": { "type": "noauth" },
            "url": "{{base}}/login",
            "body": { "mode": "urlencoded", "urlencoded": [
              { "key": "user", "value": "ada lovelace" },
              { "key": "remember", "value": "1", "disabled": true }
            ] }
          }
        },
        { "name": "list users", "request": "{{base}}/users" }
      ]
    },
    {
      "name": "Upload",
      "request": { "method": "POST", "url": { "protocol": "https", "host": ["api", "shop", "io"], "path": ["files"] },
        "body": { "mode": "formdata", "formdata": [] } }
    }
  ]
}"#;

    #[test]
    fn imports_folders_requests_and_inherited_auth() {
        let c = import(COLLECTION).unwrap();
        assert_eq!(c.name, "Shop");
        let paths: Vec<_> = c
            .files
            .iter()
            .map(|(p, _)| p.to_string_lossy().into_owned())
            .collect();
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(
            paths,
            [
                format!("Users - Admin{sep}List users.toml"),
                format!("Users - Admin{sep}Login.toml"),
                format!("Users - Admin{sep}list users 2.toml"),
                "Upload.toml".to_string(),
            ]
        );
        let list = &c.files[0].1;
        assert_eq!(list.url, "{{base}}/users?page=1");
        assert_eq!(list.headers.get("Accept"), ["application/json"]);
        assert!(list.headers.get("X-Debug").is_empty());
        assert_eq!(list.headers.get("Authorization"), ["Bearer {{token}}"]);

        let login = &c.files[1].1;
        assert_eq!(login.method.as_str(), "POST");
        assert!(login.headers.get("Authorization").is_empty());
        assert_eq!(login.body.as_deref(), Some("user=ada+lovelace"));
        assert_eq!(
            login.headers.get("Content-Type"),
            ["application/x-www-form-urlencoded"]
        );

        assert_eq!(c.files[2].1.url, "{{base}}/users");
        assert_eq!(c.files[3].1.url, "https://api.shop.io/files");
        assert_eq!(c.files[3].1.body, None);
    }

    #[test]
    fn warns_about_scripts_and_everything_else_left_behind() {
        let warnings: Vec<String> = import(COLLECTION)
            .unwrap()
            .warnings
            .iter()
            .map(ToString::to_string)
            .collect();
        let expect = [
            "Shop: the prerequest script was not imported",
            "Shop: collection variables (base); put them in an environment file is not supported and was skipped",
            "List users: the test script was not imported",
            "List users: 1 saved example responses is not supported and was skipped",
            "List users: disabled header X-Debug is not supported and was skipped",
            "Login: disabled form field remember is not supported and was skipped",
            "Upload: formdata body is not supported and was skipped",
        ];
        assert_eq!(warnings, expect);
    }

    #[test]
    fn rejects_other_schemas_and_bad_items() {
        let v1 = r#"{"info": {"name": "x", "schema": "https://schema.getpostman.com/json/collection/v1.0.0/collection.json"}}"#;
        assert!(matches!(import(v1), Err(PostmanError::Schema(_))));
        assert!(matches!(import("[1, 2]"), Err(PostmanError::Json(_))));
        let no_url = r#"{"info": {"name": "x", "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"},
            "item": [{"name": "Broken", "request": {"method": "GET"}}]}"#;
        let err = import(no_url).unwrap_err();
        assert_eq!(err.to_string(), "Broken: the request has no URL");
    }

    #[test]
    fn graphql_bodies_become_json() {
        let c = import(r#"{"info": {"name": "g", "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"},
            "item": [{"name": "Q", "request": {"method": "POST", "url": "http://h/graphql",
              "body": {"mode": "graphql", "graphql": {"query": "{ me { id } }", "variables": "{\"a\": 1}"}}}}]}"#)
        .unwrap();
        let req = &c.files[0].1;
        let body: Value = serde_json::from_str(req.body.as_deref().unwrap()).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"query": "{ me { id } }", "variables": {"a": 1}})
        );
        assert_eq!(req.headers.get("Content-Type"), ["application/json"]);
    }
}
