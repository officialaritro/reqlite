//! OpenAPI 3.0 and 3.1 documents, in JSON or YAML. Each operation becomes a
//! request file, and its first tag a directory. The server URL, path
//! parameters and credentials become variables of one environment, so no
//! credential is ever written to a request file.

use crate::{Collection, Warning, file_name, unique};
use reqlite_format::{Auth, Body, Grant, KeyIn, Method, OAuth2, Params, Part, Request};
use saphyr::{LoadableYamlNode, Scalar, Yaml};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

const METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];
/// Schema samples stop this deep, so a schema that refers to itself ends.
const MAX_DEPTH: usize = 5;
/// A `$ref` chain longer than this is taken as a loop.
const MAX_HOPS: usize = 32;
/// OpenAPI ignores header parameters with these names.
const IGNORED_HEADERS: [&str; 3] = ["accept", "content-type", "authorization"];

#[derive(Debug, thiserror::Error)]
pub enum OpenApiError {
    #[error("not valid JSON")]
    Json(#[source] serde_json::Error),
    #[error("not valid YAML")]
    Yaml(#[source] saphyr::ScanError),
    #[error("the YAML holds {0}, which has no JSON equivalent")]
    YamlValue(String),
    #[error(
        "this is Swagger {0}; Reqlite reads OpenAPI 3.0 and 3.1, so convert the file to OpenAPI 3 first"
    )]
    Swagger(String),
    #[error("not an OpenAPI document: it has no openapi field")]
    NotOpenApi,
    #[error("OpenAPI {0} is not supported; Reqlite reads 3.0 and 3.1")]
    Version(String),
    #[error("{item}: {reason}")]
    Invalid { item: String, reason: String },
}

#[derive(Debug)]
pub struct Api {
    pub collection: Collection,
    /// The environment's name. Its file is `envs/<env>.toml`.
    pub env: String,
    pub vars: BTreeMap<String, String>,
    /// Declared by name only. Their values go in the `.local.toml` file or the
    /// OS keychain.
    pub secrets: BTreeSet<String>,
}

/// `origin` is the URL the document came from. A relative server URL is
/// resolved against it.
pub fn import(text: &str, origin: Option<&str>) -> Result<Api, OpenApiError> {
    let root = parse(text)?;
    if let Some(v) = root.get("swagger") {
        return Err(OpenApiError::Swagger(text_of(v)));
    }
    let version = root
        .get("openapi")
        .map(text_of)
        .ok_or(OpenApiError::NotOpenApi)?;
    if !(version.starts_with("3.0") || version.starts_with("3.1")) {
        return Err(OpenApiError::Version(version));
    }
    let title = root
        .pointer("/info/title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or("API")
        .to_string();

    let mut d = Doc {
        root: &root,
        warnings: Vec::new(),
        vars: BTreeMap::new(),
        secrets: BTreeSet::new(),
    };
    let base = d.base_url(origin);
    d.vars.insert("base_url".into(), base);
    if root.get("webhooks").is_some_and(non_empty) {
        d.warn(Warning::Unsupported {
            option: "webhooks".into(),
            item: None,
        });
    }

    let mut files = Vec::new();
    // Names taken in each directory. `envs` is reserved at the top.
    let mut taken: BTreeMap<PathBuf, BTreeSet<String>> = BTreeMap::new();
    taken
        .entry(PathBuf::new())
        .or_default()
        .insert(reqlite_format::workspace::ENVS.into());
    let mut folders: BTreeMap<String, PathBuf> = BTreeMap::new();
    let paths = root.get("paths").and_then(Value::as_object);
    for (path, item) in paths.into_iter().flatten() {
        let Some(item) = d.deref(item, path) else {
            continue;
        };
        for method in METHODS {
            let Some(op) = item.get(method) else {
                continue;
            };
            let upper = method.to_ascii_uppercase();
            let id = format!("{upper} {path}");
            let label = ["summary", "operationId"]
                .iter()
                .filter_map(|k| op.get(*k).and_then(Value::as_str))
                .map(str::trim)
                .find(|s| !s.is_empty())
                .unwrap_or(&id)
                .to_string();
            let req = d.request(path, &upper, item, op, &id, &label)?;

            let tag = op
                .pointer("/tags/0")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|t| !t.is_empty());
            let dir = match tag {
                None => PathBuf::new(),
                Some(tag) => folders
                    .entry(tag.to_string())
                    .or_insert_with(|| {
                        let top = taken.entry(PathBuf::new()).or_default();
                        PathBuf::from(unique(top, &file_name(tag), ""))
                    })
                    .clone(),
            };
            let name = unique(
                taken.entry(dir.clone()).or_default(),
                &file_name(&label),
                ".toml",
            );
            files.push((dir.join(name), req));
        }
    }

    let Doc {
        warnings,
        mut vars,
        secrets,
        ..
    } = d;
    vars.retain(|k, _| !secrets.contains(k));
    Ok(Api {
        env: slug(&title),
        collection: Collection {
            name: title,
            files,
            warnings,
        },
        vars,
        secrets,
    })
}

fn parse(text: &str) -> Result<Value, OpenApiError> {
    let text = text.trim_start_matches('\u{feff}');
    if text.trim_start().starts_with('{') {
        return serde_json::from_str(text).map_err(OpenApiError::Json);
    }
    let docs = Yaml::load_from_str(text).map_err(OpenApiError::Yaml)?;
    docs.first().map_or(Ok(Value::Null), to_json)
}

fn to_json(y: &Yaml) -> Result<Value, OpenApiError> {
    Ok(match y {
        Yaml::Value(Scalar::Null) => Value::Null,
        Yaml::Value(Scalar::Boolean(b)) => Value::Bool(*b),
        Yaml::Value(Scalar::Integer(i)) => Value::from(*i),
        Yaml::Value(Scalar::FloatingPoint(f)) => serde_json::Number::from_f64(f.0)
            .map(Value::Number)
            .ok_or_else(|| OpenApiError::YamlValue(format!("the number {}", f.0)))?,
        Yaml::Value(Scalar::String(s)) => Value::String(s.to_string()),
        Yaml::Representation(s, _, _) => Value::String(s.to_string()),
        Yaml::Sequence(items) => Value::Array(items.iter().map(to_json).collect::<Result<_, _>>()?),
        Yaml::Mapping(m) => {
            let mut out = Map::new();
            for (k, v) in m {
                let key = match to_json(k)? {
                    Value::String(s) => s,
                    v @ (Value::Null | Value::Bool(_) | Value::Number(_)) => v.to_string(),
                    _ => return Err(OpenApiError::YamlValue("a key that is not a scalar".into())),
                };
                out.insert(key, to_json(v)?);
            }
            Value::Object(out)
        }
        Yaml::Tagged(_, inner) => to_json(inner)?,
        Yaml::Alias(_) | Yaml::BadValue => {
            return Err(OpenApiError::YamlValue(
                "an alias or value it cannot read".into(),
            ));
        }
    })
}

struct Doc<'a> {
    root: &'a Value,
    warnings: Vec<Warning>,
    vars: BTreeMap<String, String>,
    secrets: BTreeSet<String>,
}

impl<'a> Doc<'a> {
    fn warn(&mut self, w: Warning) {
        if !self.warnings.contains(&w) {
            self.warnings.push(w);
        }
    }

    /// Declares a variable. A value replaces an empty one, never another value.
    fn declare(&mut self, name: &str, value: String) {
        let slot = self.vars.entry(name.to_string()).or_default();
        if slot.is_empty() {
            *slot = value;
        }
    }

    /// Follows local `$ref`s. Anything else is a warning and `None`.
    fn deref(&mut self, mut v: &'a Value, item: &str) -> Option<&'a Value> {
        for _ in 0..MAX_HOPS {
            let Some(r) = v.get("$ref").and_then(Value::as_str) else {
                return Some(v);
            };
            match r.strip_prefix('#').and_then(|p| self.root.pointer(p)) {
                Some(next) => v = next,
                None => {
                    self.warn(Warning::Unsupported {
                        option: format!("the $ref {r}"),
                        item: Some(item.into()),
                    });
                    return None;
                }
            }
        }
        self.warn(Warning::Unsupported {
            option: "a $ref loop".into(),
            item: Some(item.into()),
        });
        None
    }

    fn base_url(&mut self, origin: Option<&str>) -> String {
        let server = self.root.pointer("/servers/0");
        let mut url = server
            .and_then(|s| s.get("url"))
            .and_then(Value::as_str)
            .unwrap_or("/")
            .to_string();
        let vars = server
            .and_then(|s| s.get("variables"))
            .and_then(Value::as_object);
        for (name, var) in vars.into_iter().flatten() {
            if let Some(default) = var.get("default") {
                url = url.replace(&format!("{{{name}}}"), &text_of(default));
            }
        }
        let url = match (url.contains("://"), origin) {
            (true, _) => url,
            (false, Some(origin)) => join(origin, &url),
            (false, None) => {
                self.warn(Warning::RelativeServer { url: url.clone() });
                url
            }
        };
        url.trim_end_matches('/').to_string()
    }

    fn request(
        &mut self,
        path: &str,
        method: &str,
        item: &'a Value,
        op: &'a Value,
        id: &str,
        label: &str,
    ) -> Result<Request, OpenApiError> {
        let invalid = |reason: String| OpenApiError::Invalid {
            item: id.into(),
            reason,
        };
        for key in ["callbacks", "servers"] {
            if op.get(key).is_some_and(non_empty) {
                self.warn(Warning::Unsupported {
                    option: key.into(),
                    item: Some(id.into()),
                });
            }
        }

        // Operation parameters replace path-item ones with the same name and place.
        let mut params: Vec<&'a Value> = Vec::new();
        for list in [item.get("parameters"), op.get("parameters")] {
            for p in list.and_then(Value::as_array).into_iter().flatten() {
                let Some(p) = self.deref(p, id) else { continue };
                let key = |p: &Value| (p.get("name").cloned(), p.get("in").cloned());
                params.retain(|q| key(q) != key(p));
                params.push(p);
            }
        }

        let (mut query, mut headers) = (Params::default(), Params::default());
        for p in params {
            let name = p.get("name").and_then(Value::as_str).unwrap_or_default();
            let place = p.get("in").and_then(Value::as_str).unwrap_or_default();
            let value = self.given_param(p, id);
            match place {
                "path" => self.declare(
                    &var_name(name),
                    value.as_ref().map(text_of).unwrap_or_default(),
                ),
                "query" | "header" => {
                    if place == "header"
                        && IGNORED_HEADERS.contains(&name.to_ascii_lowercase().as_str())
                    {
                        continue;
                    }
                    let values = match value {
                        Some(Value::Array(items)) => items.iter().map(text_of).collect(),
                        Some(v) => vec![text_of(&v)],
                        None => {
                            let var = var_name(name);
                            self.declare(&var, String::new());
                            vec![format!("{{{{{var}}}}}")]
                        }
                    };
                    let target = if place == "query" {
                        &mut query
                    } else {
                        &mut headers
                    };
                    for v in values {
                        target.append(name, v);
                    }
                }
                _ => self.warn(Warning::Unsupported {
                    option: format!("the {place} parameter {name}"),
                    item: Some(id.into()),
                }),
            }
        }

        let url = format!("{{{{base_url}}}}{}", self.template(path));
        let body = self.body(op, id, &mut headers);
        let auth = self.auth(op, id);
        let method = Method::try_from(method.to_string()).map_err(&invalid)?;
        let mut req = Request {
            version: 1,
            name: label.into(),
            method,
            url,
            headers,
            query,
            body,
            auth,
            assert: Vec::new(),
            capture: Default::default(),
            grpc: None,
        };
        req.version = reqlite_format::needed_version(&req);
        reqlite_format::validate(&req).map_err(|e| invalid(e.to_string()))?;
        Ok(req)
    }

    /// `/pets/{id}` becomes `/pets/{{id}}`, with `id` declared.
    fn template(&mut self, path: &str) -> String {
        let mut out = String::new();
        let mut rest = path;
        while let Some(open) = rest.find('{') {
            let Some(len) = rest[open..].find('}') else {
                break;
            };
            let var = var_name(&rest[open + 1..open + len]);
            self.declare(&var, String::new());
            out.push_str(&rest[..open]);
            out.push_str(&format!("{{{{{var}}}}}"));
            rest = &rest[open + len + 1..];
        }
        out.push_str(rest);
        out
    }

    /// A parameter's example or default, if the document gives one.
    fn given_param(&mut self, p: &'a Value, id: &str) -> Option<Value> {
        if let Some(v) = p.get("example") {
            return Some(v.clone());
        }
        if let Some(v) = self.first_example(p, id) {
            return Some(v);
        }
        let schema = self.deref(p.get("schema")?, id)?;
        given(schema).cloned()
    }

    /// The value of the first entry of an `examples` map.
    fn first_example(&mut self, v: &'a Value, id: &str) -> Option<Value> {
        let (_, first) = v.get("examples")?.as_object()?.iter().next()?;
        self.deref(first, id)?.get("value").cloned()
    }

    fn body(&mut self, op: &'a Value, id: &str, headers: &mut Params) -> Option<Body> {
        let rb = self.deref(op.get("requestBody")?, id)?;
        let content = rb.get("content")?.as_object()?;
        let rank = |t: &str| {
            if is_json(t) {
                0
            } else if t == "application/x-www-form-urlencoded" || t == "multipart/form-data" {
                1
            } else {
                2
            }
        };
        let (kind, media) = content.iter().min_by_key(|(t, _)| rank(t))?;
        for other in content.keys().filter(|t| *t != kind) {
            self.warn(Warning::Unsupported {
                option: format!("the request content type {other}, besides {kind},"),
                item: Some(id.into()),
            });
        }
        let schema = media.get("schema");
        let example = match media.get("example") {
            Some(v) => Some(v.clone()),
            None => self.first_example(media, id),
        };
        let set_type = |headers: &mut Params| {
            if headers.get("Content-Type").is_empty() {
                headers.append("Content-Type", kind.as_str());
            }
        };
        if is_json(kind) {
            let value =
                example.unwrap_or_else(|| schema.map_or(Value::Null, |s| self.sample(s, 0, id)));
            if kind != "application/json" {
                set_type(headers);
            }
            return serde_json::to_string_pretty(&value).ok().map(Body::Json);
        }
        if kind == "application/x-www-form-urlencoded" || kind == "multipart/form-data" {
            let props = schema
                .and_then(|s| self.deref(s, id))
                .and_then(|s| s.get("properties"))
                .and_then(Value::as_object);
            let mut fields = Params::default();
            let mut parts = Vec::new();
            for (name, prop) in props.into_iter().flatten() {
                let Some(prop) = self.deref(prop, id) else {
                    continue;
                };
                let file = prop
                    .get("format")
                    .and_then(Value::as_str)
                    .is_some_and(|f| f == "binary" || f == "base64")
                    || prop.get("contentMediaType").is_some();
                if kind.starts_with("multipart") && file {
                    self.warn(Warning::FilePart {
                        item: id.into(),
                        part: name.clone(),
                    });
                    parts.push(Part {
                        name: name.clone(),
                        text: None,
                        file: Some(name.clone()),
                        content_type: None,
                    });
                    continue;
                }
                let value = text_of(&self.sample(prop, 1, id));
                if kind.starts_with("multipart") {
                    parts.push(Part {
                        name: name.clone(),
                        text: Some(value),
                        file: None,
                        content_type: None,
                    });
                } else {
                    fields.append(name.as_str(), value);
                }
            }
            return Some(if kind.starts_with("multipart") {
                Body::Multipart(parts)
            } else {
                Body::Form(fields)
            });
        }
        if kind.starts_with("text/") || kind.ends_with("xml") {
            set_type(headers);
            return Some(Body::Text(
                example.as_ref().map(text_of).unwrap_or_default(),
            ));
        }
        self.warn(Warning::Unsupported {
            option: format!("the {kind} request body"),
            item: Some(id.into()),
        });
        None
    }

    /// A value that fits `schema`: its example, default or first enum value,
    /// else an empty value of its type.
    fn sample(&mut self, schema: &'a Value, depth: usize, id: &str) -> Value {
        if depth > MAX_DEPTH {
            return Value::Null;
        }
        let Some(s) = self.deref(schema, id) else {
            return Value::Null;
        };
        if let Some(v) = given(s) {
            return v.clone();
        }
        for key in ["oneOf", "anyOf"] {
            if let Some(first) = s.get(key).and_then(|v| v.get(0)) {
                return self.sample(first, depth + 1, id);
            }
        }
        let mut object = Map::new();
        for part in s
            .get("allOf")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Value::Object(o) = self.sample(part, depth + 1, id) {
                object.extend(o);
            }
        }
        if let Some(props) = s.get("properties").and_then(Value::as_object) {
            for (name, prop) in props {
                let v = self.sample(prop, depth + 1, id);
                object.insert(name.clone(), v);
            }
        }
        let ty = match s.get("type") {
            Some(Value::String(t)) => t.as_str(),
            Some(Value::Array(ts)) => ts
                .iter()
                .filter_map(Value::as_str)
                .find(|t| *t != "null")
                .unwrap_or("null"),
            _ => "",
        };
        match ty {
            "string" => Value::String(String::new()),
            "integer" | "number" => Value::from(0),
            "boolean" => Value::Bool(false),
            "array" => Value::Array(Vec::new()),
            "null" => Value::Null,
            "object" => Value::Object(object),
            _ if object.is_empty() => Value::Null,
            _ => Value::Object(object),
        }
    }

    fn auth(&mut self, op: &'a Value, id: &str) -> Option<Auth> {
        let reqs = op.get("security").or(self.root.get("security"))?;
        let first = reqs.get(0)?.as_object()?;
        let mut schemes = first.iter();
        let (name, scopes) = schemes.next()?;
        for (other, _) in schemes {
            self.warn(Warning::Unsupported {
                option: format!("the security scheme {other}, used with {name},"),
                item: Some(id.into()),
            });
        }
        let pointer = format!(
            "/components/securitySchemes/{}",
            name.replace('~', "~0").replace('/', "~1")
        );
        let Some(scheme) = self.root.pointer(&pointer).and_then(|s| self.deref(s, id)) else {
            self.warn(Warning::Unsupported {
                option: format!("the undefined security scheme {name}"),
                item: Some(id.into()),
            });
            return None;
        };
        let ty = scheme
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let base = var_name(name);
        let unsupported = |what: String| Warning::Unsupported {
            option: format!("{what} (security scheme {name})"),
            item: None,
        };
        match ty {
            "http" => {
                let kind = scheme
                    .get("scheme")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                match kind.to_ascii_lowercase().as_str() {
                    "bearer" => Some(Auth::Bearer {
                        token: self.secret(name, &base),
                    }),
                    "basic" => {
                        let user = format!("{base}_username");
                        self.declare(&user, String::new());
                        Some(Auth::Basic {
                            username: format!("{{{{{user}}}}}"),
                            password: self.secret(name, &format!("{base}_password")),
                        })
                    }
                    _ => {
                        self.warn(unsupported(format!("the HTTP {kind} scheme")));
                        None
                    }
                }
            }
            "apiKey" => {
                let location = match scheme.get("in").and_then(Value::as_str) {
                    Some("header") => KeyIn::Header,
                    Some("query") => KeyIn::Query,
                    other => {
                        self.warn(unsupported(format!(
                            "an API key in {}",
                            other.unwrap_or("an unknown place")
                        )));
                        return None;
                    }
                };
                Some(Auth::ApiKey {
                    name: scheme.get("name").map(text_of).unwrap_or_default(),
                    value: self.secret(name, &base),
                    location,
                })
            }
            "oauth2" => {
                let flows = scheme.get("flows").and_then(Value::as_object)?;
                for flow in flows.keys() {
                    if flow != "clientCredentials" && flow != "authorizationCode" {
                        self.warn(unsupported(format!("the OAuth 2.0 {flow} flow")));
                    }
                }
                let (grant, flow) = match (
                    flows.get("clientCredentials"),
                    flows.get("authorizationCode"),
                ) {
                    (Some(f), _) => (Grant::ClientCredentials, f),
                    (None, Some(f)) => (Grant::AuthorizationCode, f),
                    (None, None) => return None,
                };
                let url = |k: &str| flow.get(k).and_then(Value::as_str).map(String::from);
                let scope = scopes
                    .as_array()
                    .map(|s| s.iter().map(text_of).collect::<Vec<_>>().join(" "))
                    .filter(|s| !s.is_empty());
                let client_id = format!("{base}_client_id");
                self.declare(&client_id, String::new());
                Some(Auth::Oauth2(OAuth2 {
                    grant,
                    token_url: url("tokenUrl").unwrap_or_default(),
                    auth_url: (grant == Grant::AuthorizationCode)
                        .then(|| url("authorizationUrl"))
                        .flatten(),
                    device_url: None,
                    client_id: format!("{{{{{client_id}}}}}"),
                    client_secret: Some(self.secret(name, &format!("{base}_client_secret"))),
                    scope,
                    client_auth: Default::default(),
                }))
            }
            other => {
                self.warn(unsupported(format!("the {other} type")));
                None
            }
        }
    }

    /// Declares a secret and returns its placeholder.
    fn secret(&mut self, scheme: &str, name: &str) -> String {
        self.secrets.insert(name.into());
        self.warn(Warning::CredentialPlaceholder {
            option: format!("security scheme {scheme}"),
            placeholder: name.into(),
        });
        format!("{{{{{name}}}}}")
    }
}

/// The example, default, constant or first enum value a schema gives.
fn given(s: &Value) -> Option<&Value> {
    s.get("example")
        .or_else(|| s.get("examples").and_then(|e| e.get(0)))
        .or_else(|| s.get("default"))
        .or_else(|| s.get("const"))
        .or_else(|| s.get("enum").and_then(|e| e.get(0)))
}

fn is_json(media_type: &str) -> bool {
    let t = media_type.split(';').next().unwrap_or_default().trim();
    t == "application/json" || t.ends_with("+json")
}

fn non_empty(v: &Value) -> bool {
    match v {
        Value::Object(o) => !o.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Null => false,
        _ => true,
    }
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        v => v.to_string(),
    }
}

/// A name allowed inside `{{...}}`.
fn var_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "_-.".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "value".into()
    } else {
        cleaned
    }
}

/// `Pet Store API` becomes `pet-store-api`.
fn slug(title: &str) -> String {
    let lower = title.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() {
        "api".into()
    } else {
        words.join("-")
    }
}

/// Resolves a relative server URL against the document's own URL.
fn join(origin: &str, url: &str) -> String {
    let scheme_end = origin.find("://").map_or(0, |i| i + 3);
    let host_end = origin[scheme_end..]
        .find('/')
        .map_or(origin.len(), |i| scheme_end + i);
    if url.starts_with("//") {
        format!("{}{url}", &origin[..scheme_end.saturating_sub(2)])
    } else if url.starts_with('/') {
        format!("{}{url}", &origin[..host_end])
    } else {
        let dir = origin[host_end..]
            .rfind('/')
            .map_or(origin.len(), |i| host_end + i);
        format!("{}/{}", &origin[..dir], url.trim_start_matches("./"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHOP: &str = r##"
openapi: 3.0.3
info: { title: Pet Shop API, version: "1" }
servers:
  - url: https://{region}.shop.example/v{major}
    variables:
      region: { default: eu }
      major: { default: "2" }
security: [ { bearer: [] } ]
paths:
  /pets/{petId}:
    parameters:
      - { name: petId, in: path, required: true, example: 7, schema: { type: integer } }
      - { name: X-Trace, in: header, schema: { type: string, default: abc } }
    get:
      tags: [pets]
      operationId: getPet
      parameters:
        - { name: X-Trace, in: header, example: overridden }
        - { name: fields, in: query, schema: { type: array, items: { type: string } }, example: [name, tag] }
        - { name: page, in: query, schema: { type: integer } }
        - { name: Accept, in: header, schema: { type: string } }
        - { name: session, in: cookie, schema: { type: string } }
    delete:
      tags: [pets]
      security: []
  /pets:
    post:
      tags: [pets, admin]
      summary: Add a pet
      callbacks:
        done: { "{$url}": { post: { responses: { "200": { description: ok } } } } }
      requestBody:
        content:
          application/json:
            schema: { $ref: "#/components/schemas/Pet" }
          application/xml:
            schema: { $ref: "#/components/schemas/Pet" }
  /envs:
    get: { tags: [envs], summary: List } 
  /health:
    get: { summary: Health, security: [ { key: [] } ] }
  /login:
    post:
      summary: Log in
      security: [ { basic: [] } ]
      requestBody:
        content:
          application/x-www-form-urlencoded:
            schema:
              properties:
                user: { type: string, example: ada }
                remember: { type: boolean }
  /photos:
    post:
      summary: Upload
      security: [ { oauth: [photos.write, photos.read] } ]
      requestBody:
        content:
          multipart/form-data:
            schema:
              properties:
                caption: { type: string, default: hi }
                photo: { type: string, format: binary }
  /remote:
    get:
      summary: Remote
      parameters: [ { $ref: "other.yaml#/components/parameters/Id" } ]
webhooks:
  newPet: { post: { responses: { "200": { description: ok } } } }
components:
  securitySchemes:
    bearer: { type: http, scheme: Bearer }
    basic: { type: http, scheme: basic }
    key: { type: apiKey, in: query, name: api_key }
    oauth:
      type: oauth2
      flows:
        implicit: { authorizationUrl: https://auth.example/authorize, scopes: {} }
        clientCredentials: { tokenUrl: https://auth.example/token, scopes: {} }
  schemas:
    Pet:
      type: object
      required: [name]
      properties:
        name: { type: string, example: Rex }
        status: { type: string, enum: [available, sold] }
        age: { type: integer }
        tags: { type: array, items: { type: string } }
        owner: { $ref: "#/components/schemas/Owner" }
    Owner:
      allOf:
        - properties: { id: { type: integer, default: 1 } }
        - properties: { email: { type: string, format: email } }
"##;

    fn file<'a>(api: &'a Api, path: &str) -> &'a Request {
        let found = api
            .collection
            .files
            .iter()
            .find(|(p, _)| p == Path::new(path));
        match found {
            Some((_, req)) => req,
            None => panic!(
                "no {path} in {:?}",
                api.collection
                    .files
                    .iter()
                    .map(|(p, _)| p)
                    .collect::<Vec<_>>()
            ),
        }
    }

    use std::path::Path;

    #[test]
    fn each_operation_becomes_a_request_in_its_first_tag_folder() {
        let api = import(SHOP, None).unwrap();
        assert_eq!(api.collection.name, "Pet Shop API");
        assert_eq!(api.env, "pet-shop-api");
        assert_eq!(api.vars["base_url"], "https://eu.shop.example/v2");

        let get = file(&api, "pets/getPet.toml");
        assert_eq!(get.method.as_str(), "GET");
        assert_eq!(get.url, "{{base_url}}/pets/{{petId}}");
        assert_eq!(api.vars["petId"], "7");
        assert_eq!(get.headers.get("X-Trace"), ["overridden"]);
        assert!(get.headers.get("Accept").is_empty());
        assert_eq!(get.query.get("fields"), ["name", "tag"]);
        assert_eq!(get.query.get("page"), ["{{page}}"]);
        assert_eq!(api.vars["page"], "");
        assert_eq!(
            get.auth,
            Some(Auth::Bearer {
                token: "{{bearer}}".into()
            })
        );

        let delete = file(&api, "pets/DELETE -pets-{petId}.toml");
        assert_eq!(
            delete.headers.get("X-Trace"),
            ["abc"],
            "the path item's parameter"
        );
        assert_eq!(delete.auth, None, "security: [] turns auth off");

        assert_eq!(file(&api, "envs 2/List.toml").url, "{{base_url}}/envs");
        assert_eq!(file(&api, "Health.toml").method.as_str(), "GET");
        assert_eq!(api.collection.files.len(), 8);
    }

    #[test]
    fn a_json_body_is_sampled_from_its_schema() {
        let api = import(SHOP, None).unwrap();
        let post = file(&api, "pets/Add a pet.toml");
        let Some(Body::Json(text)) = &post.body else {
            panic!("{:?}", post.body)
        };
        let body: Value = serde_json::from_str(text).unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "name": "Rex",
                "status": "available",
                "age": 0,
                "tags": [],
                "owner": { "id": 1, "email": "" }
            })
        );
        assert!(post.headers.get("Content-Type").is_empty());
    }

    #[test]
    fn an_example_wins_over_the_schema_and_a_self_reference_ends() {
        let spec = r##"{
          "openapi": "3.1.0", "info": { "title": "T" },
          "paths": { "/n": { "post": {
            "requestBody": { "content": {
              "application/vnd.api+json": { "schema": { "$ref": "#/components/schemas/Node" } },
              "text/plain": { "examples": { "a": { "value": "hello" } } }
            } } },
            "put": { "requestBody": { "content": {
              "application/json": { "examples": { "first": { "value": { "x": 1 } } }, "schema": { "type": "object" } }
            } } } } },
          "components": { "schemas": { "Node": {
            "type": ["object", "null"],
            "properties": { "value": { "type": "string" }, "next": { "$ref": "#/components/schemas/Node" } }
          } } }
        }"##;
        let api = import(spec, None).unwrap();
        let post = file(&api, "POST -n.toml");
        let Some(Body::Json(text)) = &post.body else {
            panic!("{:?}", post.body)
        };
        let mut v: Value = serde_json::from_str(text).unwrap();
        let mut depth = 0;
        while v["next"].is_object() {
            assert_eq!(v["value"], "");
            v = v["next"].clone();
            depth += 1;
        }
        assert_eq!(v, serde_json::json!({ "value": null, "next": null }));
        assert_eq!(depth, MAX_DEPTH, "nested until the depth limit");
        assert_eq!(
            post.headers.get("Content-Type"),
            ["application/vnd.api+json"]
        );

        let put = file(&api, "PUT -n.toml");
        assert_eq!(put.body, Some(Body::Json("{\n  \"x\": 1\n}".into())));
    }

    #[test]
    fn credentials_become_declared_secrets_never_values() {
        let api = import(SHOP, None).unwrap();
        assert_eq!(
            file(&api, "Health.toml").auth,
            Some(Auth::ApiKey {
                name: "api_key".into(),
                value: "{{key}}".into(),
                location: KeyIn::Query
            })
        );
        assert_eq!(
            file(&api, "Log in.toml").auth,
            Some(Auth::Basic {
                username: "{{basic_username}}".into(),
                password: "{{basic_password}}".into()
            })
        );
        let Some(Auth::Oauth2(o)) = &file(&api, "Upload.toml").auth else {
            panic!()
        };
        assert_eq!(o.grant, Grant::ClientCredentials);
        assert_eq!(o.token_url, "https://auth.example/token");
        assert_eq!(o.scope.as_deref(), Some("photos.write photos.read"));
        assert_eq!(o.client_id, "{{oauth_client_id}}");
        assert_eq!(o.client_secret.as_deref(), Some("{{oauth_client_secret}}"));

        let secrets: Vec<&str> = api.secrets.iter().map(String::as_str).collect();
        assert_eq!(
            secrets,
            ["basic_password", "bearer", "key", "oauth_client_secret"]
        );
        assert!(api.secrets.iter().all(|s| !api.vars.contains_key(s)));
        let env = reqlite_format::env_to_string(&api.vars, &api.secrets).unwrap();
        reqlite_format::parse_env(Path::new("envs/x.toml"), &env, None).unwrap();
    }

    #[test]
    fn form_and_multipart_bodies_get_one_field_per_property() {
        let api = import(SHOP, None).unwrap();
        let Some(Body::Form(fields)) = &file(&api, "Log in.toml").body else {
            panic!()
        };
        assert_eq!(fields.get("user"), ["ada"]);
        assert_eq!(fields.get("remember"), ["false"]);
        assert_eq!(
            file(&api, "Upload.toml").body,
            Some(Body::Multipart(vec![
                Part {
                    name: "caption".into(),
                    text: Some("hi".into()),
                    file: None,
                    content_type: None
                },
                Part {
                    name: "photo".into(),
                    text: None,
                    file: Some("photo".into()),
                    content_type: None
                },
            ]))
        );
    }

    #[test]
    fn what_has_no_mapping_comes_back_as_a_warning() {
        let api = import(SHOP, None).unwrap();
        let text: Vec<String> = api
            .collection
            .warnings
            .iter()
            .map(|w| w.to_string())
            .collect();
        for want in [
            "webhooks is not supported",
            "POST /pets: callbacks is not supported",
            "GET /pets/{petId}: the cookie parameter session is not supported",
            "POST /pets: the request content type application/xml, besides application/json, is not supported",
            "GET /remote: the $ref other.yaml#/components/parameters/Id is not supported",
            "the OAuth 2.0 implicit flow (security scheme oauth) is not supported",
            "POST /photos: part photo sends a file",
            "the credential from security scheme bearer became {{bearer}}",
        ] {
            assert!(
                text.iter().any(|t| t.starts_with(want)),
                "no {want:?} in {text:#?}"
            );
        }
        let oauth = text.iter().filter(|t| t.contains("implicit")).count();
        assert_eq!(oauth, 1, "a warning shows once");
    }

    #[test]
    fn a_relative_server_resolves_against_where_the_document_came_from() {
        let spec = "openapi: 3.1.0\ninfo: {title: T}\nservers: [{url: /api/v3/}]\npaths: {}\n";
        let api = import(spec, Some("https://petstore.example/api/v3/openapi.json")).unwrap();
        assert_eq!(api.vars["base_url"], "https://petstore.example/api/v3");
        assert!(api.collection.warnings.is_empty());

        let no_servers = "openapi: 3.0.0\ninfo: {title: T}\npaths: {}\n";
        let api = import(no_servers, Some("http://localhost:8080/docs/spec.yaml")).unwrap();
        assert_eq!(api.vars["base_url"], "http://localhost:8080");
        let api = import(no_servers, None).unwrap();
        assert_eq!(
            api.collection.warnings,
            [Warning::RelativeServer { url: "/".into() }]
        );

        assert_eq!(
            join("https://a.example/x/spec.json", "v1"),
            "https://a.example/x/v1"
        );
        assert_eq!(join("https://a.example", "./v1"), "https://a.example/v1");
        assert_eq!(
            join("https://a.example/s", "//cdn.example/v1"),
            "https://cdn.example/v1"
        );
    }

    #[test]
    fn rejects_swagger_2_and_what_is_not_openapi_3() {
        let err = import("swagger: '2.0'\ninfo: {title: T}\n", None).unwrap_err();
        assert!(
            matches!(err, OpenApiError::Swagger(ref v) if v == "2.0"),
            "{err}"
        );
        assert!(
            err.to_string().contains("convert the file to OpenAPI 3"),
            "{err}"
        );
        let err = import("{\"openapi\": \"4.0.0\"}", None).unwrap_err();
        assert!(matches!(err, OpenApiError::Version(_)), "{err}");
        let err = import("name: not an API\n", None).unwrap_err();
        assert!(matches!(err, OpenApiError::NotOpenApi), "{err}");
        let err = import("openapi: [unclosed\n", None).unwrap_err();
        assert!(matches!(err, OpenApiError::Yaml(_)), "{err}");
        let err = import("{\"openapi\": ", None).unwrap_err();
        assert!(matches!(err, OpenApiError::Json(_)), "{err}");
    }
}
