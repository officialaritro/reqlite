//! The Reqlite request file format: one request per TOML file.

use serde::de::{self, Deserializer, SeqAccess, Visitor};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};

mod env;
pub use env::{EnvError, Environment, Var, is_var_name, load_env, local_path, parse_env};

/// Current schema version written to and accepted from request files.
pub const VERSION: u32 = 1;

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
    pub body: Option<String>,
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
    #[error("invalid request file: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("unsupported request file version {found}, this build reads version {VERSION}")]
    UnsupportedVersion { found: u32 },
    #[error("invalid request file: {0}")]
    Invalid(String),
}

pub fn parse(text: &str) -> Result<Request, ParseError> {
    let req: Request = toml::from_str(text)?;
    if req.version != VERSION {
        return Err(ParseError::UnsupportedVersion { found: req.version });
    }
    validate(&req).map_err(ParseError::Invalid)?;
    Ok(req)
}

fn validate(req: &Request) -> Result<(), String> {
    if req.url.trim().is_empty() {
        return Err("url is empty".to_string());
    }
    for (name, value) in req.headers.pairs() {
        if !is_token(name) {
            return Err(format!("invalid header name {name:?}"));
        }
        if value.chars().any(|c| c.is_control() && c != '\t') {
            return Err(format!("header {name:?} has a control character"));
        }
    }
    Ok(())
}

/// Canonical text for a request. The same request always gives the same bytes,
/// so a save that changes nothing makes no Git diff.
// SHORTCUT: comments and layout in a hand-edited file are lost on save. Switch to
// toml_edit if users report it.
pub fn to_string(req: &Request) -> Result<String, toml::ser::Error> {
    toml::to_string(req)
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("cannot serialize request: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("cannot save {path}: {source}")]
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
        assert_eq!(req.body.as_deref(), Some(r#"{"name":"ada"}"#));
    }

    #[test]
    fn method_defaults_to_get() {
        assert_eq!(parse(MIN).unwrap().method.as_str(), "GET");
    }

    #[test]
    fn rejects_unknown_version() {
        let err = parse(&MIN.replace("version = 1", "version = 2")).unwrap_err();
        assert!(matches!(err, ParseError::UnsupportedVersion { found: 2 }));
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
            let err = parse(&text).unwrap_err().to_string();
            assert!(err.contains(reason), "{text:?} gave {err:?}");
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
}
