//! The Reqlite request file format: one request per TOML file.

use serde::de::{self, Deserializer, SeqAccess, Visitor};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};

mod body;
mod env;
pub use body::{Auth, Body, ClientAuth, Grant, KeyIn, OAuth2, Part};
pub use env::{EnvError, Environment, Var, is_var_name, load_env, local_path, parse_env};

/// The newest request file version this build reads and writes. Files use the
/// lowest version that holds them: see [`needed_version`].
pub const VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub name: String,
    #[serde(default)]
    pub method: Method,
    pub url: String,
    #[serde(default, skip_serializing_if = "Params::is_empty")]
    pub headers: Params,
    #[serde(default, skip_serializing_if = "Params::is_empty")]
    pub query: Params,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Body>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<Auth>,
}

/// Version 1 holds a request whose body is plain text and that has no auth.
/// Anything else needs version 2.
pub fn needed_version(req: &Request) -> u32 {
    match (&req.body, &req.auth) {
        (None | Some(Body::Text(_)), None) => 1,
        _ => 2,
    }
}

/// An HTTP method token, stored upper case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Method(String);

impl Method {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for Method {
    fn default() -> Self {
        Method("GET".to_string())
    }
}

impl TryFrom<String> for Method {
    type Error = String;

    fn try_from(s: String) -> Result<Self, String> {
        if is_token(&s) {
            Ok(Method(s.to_ascii_uppercase()))
        } else {
            Err(format!("invalid HTTP method {s:?}"))
        }
    }
}

impl From<Method> for String {
    fn from(m: Method) -> String {
        m.0
    }
}

/// Names mapped to one or more values, so `?tag=a&tag=b` is representable.
/// A file writes a single value as a string and several as an array.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Params(BTreeMap<String, Vec<String>>);

impl Params {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, name: &str) -> &[String] {
        self.0.get(name).map_or(&[], Vec::as_slice)
    }

    pub fn append(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.0.entry(name.into()).or_default().push(value.into());
    }

    /// Every name and value pair, names in sorted order, values in file order.
    pub fn pairs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0
            .iter()
            .flat_map(|(k, vs)| vs.iter().map(move |v| (k.as_str(), v.as_str())))
    }
}

impl Serialize for Params {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(self.0.len()))?;
        for (k, vs) in &self.0 {
            match vs.as_slice() {
                [one] => map.serialize_entry(k, one)?,
                many => map.serialize_entry(k, many)?,
            }
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Params {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = BTreeMap::<String, OneOrMany>::deserialize(d)?;
        let mut out = BTreeMap::new();
        for (k, OneOrMany(vs)) in raw {
            if vs.is_empty() {
                return Err(de::Error::custom(format!("{k:?} has no values")));
            }
            out.insert(k, vs);
        }
        Ok(Params(out))
    }
}

struct OneOrMany(Vec<String>);

impl<'de> Deserialize<'de> for OneOrMany {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = OneOrMany;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a string or an array of strings")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<OneOrMany, E> {
                Ok(OneOrMany(vec![v.to_string()]))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<OneOrMany, A::Error> {
                let mut vs = Vec::new();
                while let Some(v) = seq.next_element::<String>()? {
                    vs.push(v);
                }
                Ok(OneOrMany(vs))
            }
        }
        d.deserialize_any(V)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("invalid request file")]
    Toml(#[from] toml::de::Error),
    #[error("unsupported request file version {found}, this build reads versions 1 to {VERSION}")]
    UnsupportedVersion { found: u32 },
    #[error("invalid request file: {0}")]
    Invalid(String),
}

pub fn parse(text: &str) -> Result<Request, ParseError> {
    let req: Request = toml::from_str(text)?;
    if !(1..=VERSION).contains(&req.version) {
        return Err(ParseError::UnsupportedVersion { found: req.version });
    }
    if req.version < needed_version(&req) {
        return Err(ParseError::Invalid(
            "a [body] table or [auth] needs version = 2".to_string(),
        ));
    }
    validate(&req)?;
    Ok(req)
}

/// The rules every request file meets. [`parse`] applies them; code that builds
/// a [`Request`] another way calls this before saving it.
pub fn validate(req: &Request) -> Result<(), ParseError> {
    let invalid = |msg: String| Err(ParseError::Invalid(msg));
    if req.url.trim().is_empty() {
        return invalid("url is empty".to_string());
    }
    for (name, value) in req.headers.pairs() {
        if !is_token(name) {
            return invalid(format!("invalid header name {name:?}"));
        }
        if value.chars().any(|c| c.is_control() && c != '\t') {
            return invalid(format!("header {name:?} has a control character"));
        }
    }
    match &req.body {
        Some(Body::Form(fields)) => {
            if fields.pairs().any(|(name, _)| name.is_empty()) {
                return invalid("a form field has an empty name".to_string());
            }
        }
        Some(Body::Multipart(parts)) => {
            for p in parts {
                if p.name.is_empty() {
                    return invalid("a multipart part has an empty name".to_string());
                }
                match (&p.text, &p.file) {
                    (Some(_), None) => {
                        if p.content_type.is_some() {
                            return invalid(format!(
                                "part {:?}: content_type is for file parts",
                                p.name
                            ));
                        }
                    }
                    (None, Some(f)) if !f.trim().is_empty() => {}
                    (None, Some(_)) => {
                        return invalid(format!("part {:?} has an empty file path", p.name));
                    }
                    _ => {
                        return invalid(format!(
                            "part {:?} needs exactly one of text or file",
                            p.name
                        ));
                    }
                }
            }
        }
        Some(Body::File { path }) if path.trim().is_empty() => {
            return invalid("the body file path is empty".to_string());
        }
        _ => {}
    }
    if let Some(auth) = &req.auth {
        if let Auth::ApiKey { name, .. } = auth {
            if !is_token(name) {
                return invalid(format!("invalid API key name {name:?}"));
            }
        }
        if let Auth::Oauth2(o) = auth {
            let empty = |v: &Option<String>| v.as_deref().is_none_or(|s| s.trim().is_empty());
            if o.token_url.trim().is_empty() || o.client_id.trim().is_empty() {
                return invalid("oauth2 needs token_url and client_id".to_string());
            }
            if o.grant == Grant::AuthorizationCode && empty(&o.auth_url) {
                return invalid("the authorization_code grant needs auth_url".to_string());
            }
            if o.grant == Grant::DeviceCode && empty(&o.device_url) {
                return invalid("the device_code grant needs device_url".to_string());
            }
            if o.grant == Grant::ClientCredentials && empty(&o.client_secret) {
                return invalid("the client_credentials grant needs client_secret".to_string());
            }
        }
        if let Some(header) = auth.header() {
            if req
                .headers
                .pairs()
                .any(|(name, _)| name.eq_ignore_ascii_case(header))
            {
                return invalid(format!(
                    "{header} is set both in [headers] and by [auth]; keep one"
                ));
            }
        }
    }
    Ok(())
}

/// Canonical text for a request. The same request always gives the same bytes,
/// so a save that changes nothing makes no Git diff.
// SHORTCUT: comments and layout in a hand-edited file are lost on save. Switch to
// toml_edit if users report it.
pub fn to_string(req: &Request) -> Result<String, toml::ser::Error> {
    let version = needed_version(req);
    if req.version == version {
        return toml::to_string(req);
    }
    toml::to_string(&Request {
        version,
        ..req.clone()
    })
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("cannot serialize request")]
    Serialize(#[from] toml::ser::Error),
    #[error("cannot save {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Writes `req` to `path` through a temp file in the same directory, so a crash
/// leaves either the old file or the new one, never a partial file.
pub fn save(path: &Path, req: &Request) -> Result<(), SaveError> {
    let text = to_string(req)?;
    let io = |source| SaveError::Io {
        path: path.to_path_buf(),
        source,
    };
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(io)?;
    tmp.write_all(text.as_bytes()).map_err(io)?;
    tmp.as_file().sync_all().map_err(io)?;
    tmp.persist(path).map_err(|e| io(e.error))?;
    #[cfg(unix)]
    std::fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(io)?;
    Ok(())
}

/// RFC 9110 `token`: the shape of method and header names.
fn is_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: &str = "version = 1\nname = \"x\"\nurl = \"http://a\"\n";

    #[test]
    fn parses_full_request() {
        let req = parse(
            r#"
version = 1
name = "Create user"
method = "post"
url = "http://localhost:3000/users"
body = '{"name":"ada"}'

[headers]
Content-Type = "application/json"

[query]
dry_run = "true"
tag = ["a", "b"]
"#,
        )
        .unwrap();
        assert_eq!(req.method.as_str(), "POST");
        assert_eq!(req.headers.get("Content-Type"), ["application/json"]);
        let pairs: Vec<_> = req.query.pairs().collect();
        assert_eq!(pairs, [("dry_run", "true"), ("tag", "a"), ("tag", "b")]);
        assert_eq!(req.body, Some(Body::Text(r#"{"name":"ada"}"#.into())));
    }

    #[test]
    fn method_defaults_to_get() {
        assert_eq!(parse(MIN).unwrap().method.as_str(), "GET");
    }

    #[test]
    fn rejects_unknown_version() {
        let err = parse(&MIN.replace("version = 1", "version = 3")).unwrap_err();
        assert!(matches!(err, ParseError::UnsupportedVersion { found: 3 }));
    }

    #[test]
    fn rejects_invalid_files_with_a_reason() {
        let cases = [
            (format!("{MIN}headres = {{}}\n"), "headres"),
            (format!("{MIN}method = \"GE T\"\n"), "invalid HTTP method"),
            (MIN.replace("http://a", " "), "url is empty"),
            (format!("{MIN}[query]\ntag = []\n"), "has no values"),
            (
                format!("{MIN}[headers]\n\"X Bad\" = \"1\"\n"),
                "invalid header name",
            ),
            (
                format!("{MIN}[headers]\nX = \"a\\nb\"\n"),
                "control character",
            ),
        ];
        for (text, reason) in cases {
            let err = parse(&text).unwrap_err();
            let mut msg = err.to_string();
            if let Some(source) = std::error::Error::source(&err) {
                msg = format!("{msg}: {source}");
            }
            assert!(msg.contains(reason), "{text:?} gave {msg:?}");
        }
    }

    const CANONICAL: &str = r#"version = 1
name = "Search"
method = "GET"
url = "http://a/search"
body = """
{"q": 1}
"""

[headers]
Accept = "application/json"

[query]
tag = ["a", "b"]
"#;

    #[test]
    fn writes_canonical_text_whatever_the_input_layout() {
        let shuffled = "[query]\ntag = ['a', 'b']\n\n[headers]\nAccept = 'application/json'\n";
        let top = "url = 'http://a/search'\nversion = 1\nbody = \"\"\"\n{\"q\": 1}\n\"\"\"\nname = 'Search'\n";
        let req = parse(&format!("{top}{shuffled}")).unwrap();
        assert_eq!(to_string(&req).unwrap(), CANONICAL);
    }

    #[test]
    fn examples_round_trip_byte_for_byte() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples");
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let req = parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let once = to_string(&req).unwrap();
            assert_eq!(parse(&once).unwrap(), req, "{path:?}");
            assert_eq!(to_string(&parse(&once).unwrap()).unwrap(), once, "{path:?}");
        }
    }

    #[test]
    fn save_writes_a_file_that_parses_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.toml");
        let req = parse(CANONICAL).unwrap();
        save(&path, &req).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), CANONICAL);
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "temp file left behind"
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_save_keeps_the_old_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.toml");
        std::fs::write(&path, MIN).unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        let result = save(&path, &parse(CANONICAL).unwrap());
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(result, Err(SaveError::Io { .. })), "{result:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), MIN);
    }

    const V2: &str = "version = 2\nname = \"x\"\nmethod = \"POST\"\nurl = \"http://a\"\n";

    #[test]
    fn each_body_type_reads_and_writes_back_the_same() {
        let cases = [
            "\n[body]\ntype = \"json\"\ntext = '{\"a\": 1}'\n",
            "\n[body]\ntype = \"form\"\n\n[body.fields]\nname = \"ada\"\ntag = [\"a\", \"b\"]\n",
            "\n[body]\ntype = \"multipart\"\n\n[[body.parts]]\nname = \"note\"\ntext = \"hi\"\n\n[[body.parts]]\nname = \"avatar\"\nfile = \"img/ada.png\"\ncontent_type = \"image/png\"\n",
            "\n[body]\ntype = \"file\"\npath = \"payload.bin\"\n",
            "\n[auth]\ntype = \"bearer\"\ntoken = \"{{token}}\"\n",
            "\n[auth]\ntype = \"basic\"\nusername = \"ada\"\npassword = \"{{pw}}\"\n",
            "\n[auth]\ntype = \"api_key\"\nname = \"key\"\nvalue = \"{{key}}\"\nin = \"query\"\n",
        ];
        for tail in cases {
            let text = format!("{V2}{tail}");
            let req = parse(&text).unwrap_or_else(|e| panic!("{tail}: {e}"));
            assert_eq!(to_string(&req).unwrap(), text, "{tail}");
        }
    }

    #[test]
    fn a_v2_file_that_needs_nothing_new_is_written_as_v1() {
        let req = parse(&format!("{V2}body = \"raw\"\n")).unwrap();
        assert_eq!(req.body, Some(Body::Text("raw".into())));
        let out = to_string(&req).unwrap();
        assert!(out.starts_with("version = 1\n"), "{out}");
        let typed_text =
            parse(&format!("{V2}\n[body]\ntype = \"text\"\ntext = \"raw\"\n")).unwrap();
        assert_eq!(to_string(&typed_text).unwrap(), out);
    }

    #[test]
    fn new_tables_need_version_2() {
        let v1 = V2.replace("version = 2", "version = 1");
        for tail in [
            "\n[body]\ntype = \"json\"\ntext = \"{}\"\n",
            "\n[auth]\ntype = \"bearer\"\ntoken = \"t\"\n",
        ] {
            let err = parse(&format!("{v1}{tail}")).unwrap_err();
            assert!(err.to_string().contains("needs version = 2"), "{err}");
        }
    }

    #[test]
    fn new_fields_are_checked_with_a_reason() {
        let cases = [
            (
                "\n[body]\ntype = \"multipart\"\n\n[[body.parts]]\nname = \"a\"\ntext = \"x\"\nfile = \"f\"\n",
                "exactly one of text or file",
            ),
            (
                "\n[body]\ntype = \"multipart\"\n\n[[body.parts]]\nname = \"a\"\n",
                "exactly one of text or file",
            ),
            (
                "\n[body]\ntype = \"multipart\"\n\n[[body.parts]]\nname = \"\"\ntext = \"x\"\n",
                "empty name",
            ),
            (
                "\n[body]\ntype = \"multipart\"\n\n[[body.parts]]\nname = \"a\"\ntext = \"x\"\ncontent_type = \"text/plain\"\n",
                "content_type is for file parts",
            ),
            ("\n[body]\ntype = \"file\"\npath = \" \"\n", "path is empty"),
            (
                "\n[body]\ntype = \"xml\"\ntext = \"<a/>\"\n",
                "unknown variant",
            ),
            (
                "\n[headers]\nauthorization = \"x\"\n\n[auth]\ntype = \"bearer\"\ntoken = \"t\"\n",
                "set both in [headers] and by [auth]",
            ),
            (
                "\n[auth]\ntype = \"api_key\"\nname = \"bad key\"\nvalue = \"v\"\n",
                "invalid API key name",
            ),
        ];
        for (tail, reason) in cases {
            let err = parse(&format!("{V2}{tail}")).unwrap_err();
            let mut msg = err.to_string();
            if let Some(source) = std::error::Error::source(&err) {
                msg = format!("{msg}: {source}");
            }
            assert!(msg.contains(reason), "{tail:?} gave {msg:?}");
        }
    }

    #[test]
    fn an_api_key_in_the_query_does_not_clash_with_a_header() {
        let text = format!(
            "{V2}\n[headers]\nkey = \"h\"\n\n[auth]\ntype = \"api_key\"\nname = \"key\"\nvalue = \"v\"\nin = \"query\"\n"
        );
        assert!(parse(&text).is_ok());
    }

    #[test]
    fn oauth2_reads_and_writes_back_the_same() {
        for tail in [
            "\n[auth]\ntype = \"oauth2\"\ngrant = \"client_credentials\"\ntoken_url = \"https://id/token\"\nclient_id = \"app\"\nclient_secret = \"{{secret}}\"\nscope = \"read write\"\n",
            "\n[auth]\ntype = \"oauth2\"\ngrant = \"authorization_code\"\ntoken_url = \"https://id/token\"\nauth_url = \"https://id/authorize\"\nclient_id = \"app\"\nclient_auth = \"body\"\n",
            "\n[auth]\ntype = \"oauth2\"\ngrant = \"device_code\"\ntoken_url = \"https://id/token\"\ndevice_url = \"https://id/device\"\nclient_id = \"app\"\n",
        ] {
            let text = format!("{V2}{tail}");
            let req = parse(&text).unwrap_or_else(|e| panic!("{tail}: {e}"));
            assert_eq!(to_string(&req).unwrap(), text, "{tail}");
        }
    }

    #[test]
    fn each_oauth2_grant_needs_its_own_fields() {
        let base =
            "\n[auth]\ntype = \"oauth2\"\ntoken_url = \"https://id/token\"\nclient_id = \"app\"\n";
        for (extra, reason) in [
            ("grant = \"authorization_code\"\n", "needs auth_url"),
            ("grant = \"device_code\"\n", "needs device_url"),
            ("grant = \"client_credentials\"\n", "needs client_secret"),
            ("grant = \"password\"\n", "unknown variant"),
        ] {
            let err = parse(&format!("{V2}{base}{extra}")).unwrap_err();
            let mut msg = err.to_string();
            if let Some(source) = std::error::Error::source(&err) {
                msg = format!("{msg}: {source}");
            }
            assert!(msg.contains(reason), "{extra} gave {msg}");
        }
        let clash = format!(
            "{V2}\n[headers]\nAuthorization = \"x\"\n{base}grant = \"device_code\"\ndevice_url = \"https://id/d\"\n"
        );
        assert!(parse(&clash).unwrap_err().to_string().contains("set both"));
    }
}
