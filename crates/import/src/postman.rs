//! Postman collections, schema v2.0 and v2.1. Each folder becomes a directory
//! and each request a file. Postman and Reqlite both write `{{name}}`, so
//! placeholders carry over unchanged.

use crate::{Warning, is_literal_credential};
use reqlite_format::{Auth as ReqAuth, Body as ReqBody, KeyIn, Method, Params, Part, Request};
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
    options: Value,
    #[serde(default)]
    urlencoded: Vec<Kv>,
    #[serde(default)]
    formdata: Vec<FormKv>,
    file: Option<FileSrc>,
    graphql: Option<Graphql>,
    #[serde(default)]
    disabled: bool,
}

#[derive(Deserialize)]
struct FormKv {
    #[serde(default)]
    key: String,
    #[serde(default)]
    value: Value,
    #[serde(rename = "type", default)]
    kind: String,
    /// A file path, or an array of them.
    #[serde(default)]
    src: Value,
    #[serde(rename = "contentType")]
    content_type: Option<String>,
    #[serde(default)]
    disabled: bool,
}

#[derive(Deserialize)]
struct FileSrc {
    #[serde(default)]
    src: Value,
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
    #[serde(default)]
    basic: Vec<AuthKv>,
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

    // A literal credential becomes a placeholder, so no secret is committed.
    let mut secret = |value: String, name: &str, what: &str| {
        if value.contains("{{") {
            return value;
        }
        warnings.push(Warning::CredentialPlaceholder {
            option: format!("{label}: {what}"),
            placeholder: name.into(),
        });
        format!("{{{{{name}}}}}")
    };
    let mut auth = match r.auth.as_ref().or(inherited) {
        None => None,
        Some(a) if a.kind == "noauth" || a.kind == "inherit" => None,
        Some(a) if a.kind == "bearer" => Some(ReqAuth::Bearer {
            token: secret(field(&a.bearer, "token"), "token", "bearer token"),
        }),
        Some(a) if a.kind == "apikey" => Some(ReqAuth::ApiKey {
            name: field(&a.apikey, "key"),
            value: secret(field(&a.apikey, "value"), "api_key", "API key"),
            location: if field(&a.apikey, "in") == "query" {
                KeyIn::Query
            } else {
                KeyIn::Header
            },
        }),
        Some(a) if a.kind == "basic" => Some(ReqAuth::Basic {
            username: field(&a.basic, "username"),
            password: secret(
                field(&a.basic, "password"),
                "password",
                "basic auth password",
            ),
        }),
        Some(a) => {
            warnings.push(Warning::Unsupported {
                option: format!("{} auth", a.kind),
                item: Some(label.into()),
            });
            None
        }
    };
    if let Some(header) = auth.as_ref().and_then(ReqAuth::header) {
        if has_header(&headers, header) {
            warnings.push(Warning::Unsupported {
                option: format!("auth, next to a {header} header (the header is kept)"),
                item: Some(label.into()),
            });
            auth = None;
        }
    }

    let body = match &r.body {
        None => None,
        Some(b) if b.disabled => None,
        Some(b) => match b.mode.as_str() {
            "raw" => {
                let language = b.options.pointer("/raw/language").and_then(Value::as_str);
                let implied = match language {
                    Some("json") => Some("application/json"),
                    Some("xml") => Some("application/xml"),
                    Some("html") => Some("text/html"),
                    Some("javascript") => Some("application/javascript"),
                    Some("text") => Some("text/plain"),
                    _ => None,
                };
                let raw = b.raw.clone().filter(|s| !s.is_empty());
                if let Some(ct) = implied.filter(|_| raw.is_some()) {
                    if !has_header(&headers, "Content-Type") {
                        headers.append("Content-Type", ct);
                    }
                }
                raw.map(ReqBody::Text)
            }
            "urlencoded" => {
                let mut fields = Params::default();
                for kv in &b.urlencoded {
                    if kv.disabled {
                        warnings.push(Warning::Unsupported {
                            option: format!("disabled form field {}", kv.key),
                            item: Some(label.into()),
                        });
                    } else {
                        fields.append(kv.key.clone(), text(&kv.value));
                    }
                }
                Some(ReqBody::Form(fields))
            }
            "formdata" => {
                let mut parts = Vec::new();
                for kv in &b.formdata {
                    if kv.disabled {
                        warnings.push(Warning::Unsupported {
                            option: format!("disabled form field {}", kv.key),
                            item: Some(label.into()),
                        });
                        continue;
                    }
                    if kv.kind == "file" {
                        let files = match &kv.src {
                            Value::Array(list) => list.iter().map(text).collect(),
                            Value::Null => Vec::new(),
                            v => vec![text(v)],
                        };
                        if files.is_empty() {
                            warnings.push(Warning::BodyFromFile {
                                path: format!("{label}: form file {} (no file chosen)", kv.key),
                            });
                        }
                        for file in files {
                            parts.push(Part {
                                name: kv.key.clone(),
                                text: None,
                                file: Some(file),
                                content_type: kv.content_type.clone(),
                            });
                        }
                    } else {
                        parts.push(Part {
                            name: kv.key.clone(),
                            text: Some(text(&kv.value)),
                            file: None,
                            content_type: None,
                        });
                    }
                }
                // An empty form sends no body.
                (!parts.is_empty()).then_some(ReqBody::Multipart(parts))
            }
            "graphql" => {
                let g = b.graphql.as_ref();
                let query = g.map_or(String::new(), |g| g.query.clone());
                let variables = g.and_then(|g| match &g.variables {
                    Value::Null => None,
                    Value::String(s) if s.trim().is_empty() => None,
                    Value::String(s) => Some(s.clone()),
                    v => Some(v.to_string()),
                });
                // An empty query sends no body.
                (!query.trim().is_empty()).then_some(ReqBody::Graphql { query, variables })
            }
            "file" => match b
                .file
                .as_ref()
                .map(|f| text(&f.src))
                .filter(|p| !p.is_empty())
            {
                Some(path) => Some(ReqBody::File { path }),
                None => {
                    warnings.push(Warning::BodyFromFile {
                        path: format!("{label} (a Postman file body with no file chosen)"),
                    });
                    None
                }
            },
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
    let mut req = Request {
        version: 1,
        name: label.into(),
        method,
        url,
        headers,
        query: Params::default(),
        body,
        auth,
        assert: Vec::new(),
        capture: Default::default(),
    };
    req.version = reqlite_format::needed_version(&req);
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

/// Header names compare without case, as HTTP defines them.
fn has_header(headers: &Params, name: &str) -> bool {
    headers.pairs().any(|(k, _)| k.eq_ignore_ascii_case(name))
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
        assert_eq!(
            list.auth,
            Some(ReqAuth::Bearer {
                token: "{{token}}".into()
            }),
            "inherited from the collection"
        );

        let login = &c.files[1].1;
        assert_eq!(login.method.as_str(), "POST");
        assert_eq!(login.auth, None, "noauth stops the inherited auth");
        let mut fields = Params::default();
        fields.append("user", "ada lovelace");
        assert_eq!(login.body, Some(ReqBody::Form(fields)));

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
    fn raw_body_language_sets_the_content_type_postman_would_send() {
        let c = import(r#"{"info": {"name": "r", "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"},
            "item": [
              {"name": "J", "request": {"method": "POST", "url": "http://h", "body": {"mode": "raw", "raw": "{}", "options": {"raw": {"language": "json"}}}}},
              {"name": "K", "request": {"method": "POST", "url": "http://h", "header": [{"key": "content-type", "value": "text/x-custom"}],
                "body": {"mode": "raw", "raw": "{}", "options": {"raw": {"language": "json"}}}}}
            ]}"#)
        .unwrap();
        assert_eq!(
            c.files[0].1.headers.get("Content-Type"),
            ["application/json"]
        );
        assert!(c.files[1].1.headers.get("Content-Type").is_empty());
        assert_eq!(c.files[1].1.headers.get("content-type"), ["text/x-custom"]);
    }

    #[test]
    fn graphql_bodies_stay_graphql() {
        let c = import(r#"{"info": {"name": "g", "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"},
            "item": [{"name": "Q", "request": {"method": "POST", "url": "http://h/graphql",
              "body": {"mode": "graphql", "graphql": {"query": "{ me { id } }", "variables": "{\"a\": 1}"}}}}]}"#)
        .unwrap();
        let req = &c.files[0].1;
        assert_eq!(
            req.body,
            Some(ReqBody::Graphql {
                query: "{ me { id } }".into(),
                variables: Some("{\"a\": 1}".into())
            })
        );
        assert!(req.headers.get("Content-Type").is_empty());
    }

    fn one(request: &str) -> (Request, Vec<String>) {
        let c = import(&format!(
            r#"{{"info": {{"name": "c", "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"}},
            "item": [{{"name": "R", "request": {request}}}]}}"#
        ))
        .unwrap();
        let warnings = c.warnings.iter().map(ToString::to_string).collect();
        (c.files.into_iter().next().unwrap().1, warnings)
    }

    #[test]
    fn form_data_and_file_bodies_keep_their_files() {
        let (req, warnings) = one(
            r#"{"method": "POST", "url": "http://h/up", "body": {"mode": "formdata", "formdata": [
                {"key": "note", "value": "hi", "type": "text"},
                {"key": "pics", "type": "file", "src": ["a.png", "b.png"], "contentType": "image/png"},
                {"key": "off", "value": "x", "type": "text", "disabled": true}]}}"#,
        );
        let file = |f: &str| Part {
            name: "pics".into(),
            text: None,
            file: Some(f.into()),
            content_type: Some("image/png".into()),
        };
        assert_eq!(
            req.body,
            Some(ReqBody::Multipart(vec![
                Part {
                    name: "note".into(),
                    text: Some("hi".into()),
                    file: None,
                    content_type: None
                },
                file("a.png"),
                file("b.png"),
            ]))
        );
        assert_eq!(
            warnings,
            ["R: disabled form field off is not supported and was skipped"]
        );

        let (bin, _) = one(
            r#"{"method": "PUT", "url": "http://h/p", "body": {"mode": "file", "file": {"src": "data/p.bin"}}}"#,
        );
        assert_eq!(
            bin.body,
            Some(ReqBody::File {
                path: "data/p.bin".into()
            })
        );
    }

    #[test]
    fn auth_maps_to_its_table_and_literal_secrets_become_placeholders() {
        let (basic, warnings) = one(r#"{"url": "http://h/", "auth": {"type": "basic", "basic": [
                {"key": "username", "value": "ada"}, {"key": "password", "value": "hunter2"}]}}"#);
        assert_eq!(
            basic.auth,
            Some(ReqAuth::Basic {
                username: "ada".into(),
                password: "{{password}}".into()
            })
        );
        assert_eq!(
            warnings,
            [
                "the credential from R: basic auth password became {{password}}; set password as a secret in the environment's .local.toml file"
            ]
        );

        let (key, _) = one(
            r#"{"url": "http://h/", "auth": {"type": "apikey", "apikey": [
                {"key": "key", "value": "X-Key"}, {"key": "value", "value": "{{k}}"}, {"key": "in", "value": "query"}]}}"#,
        );
        assert_eq!(
            key.auth,
            Some(ReqAuth::ApiKey {
                name: "X-Key".into(),
                value: "{{k}}".into(),
                location: KeyIn::Query
            })
        );

        let (both, warnings) = one(
            r#"{"url": "http://h/", "header": [{"key": "Authorization", "value": "Bearer {{t}}"}],
                "auth": {"type": "bearer", "bearer": [{"key": "token", "value": "{{token}}"}]}}"#,
        );
        assert_eq!(both.auth, None, "the hand-written header is kept");
        assert!(warnings[0].contains("the header is kept"), "{warnings:?}");
    }
}
