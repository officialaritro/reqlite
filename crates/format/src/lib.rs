//! The Reqlite request file format: one request per TOML file.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Current schema version written to and accepted from request files.
pub const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub name: String,
    #[serde(default = "default_method")]
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub query: BTreeMap<String, String>,
    #[serde(default)]
    pub body: Option<String>,
}

fn default_method() -> String {
    "GET".to_string()
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("invalid request file: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("unsupported request file version {found}, this build reads version {VERSION}")]
    UnsupportedVersion { found: u32 },
}

pub fn parse(text: &str) -> Result<Request, ParseError> {
    let req: Request = toml::from_str(text)?;
    if req.version != VERSION {
        return Err(ParseError::UnsupportedVersion { found: req.version });
    }
    Ok(req)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_request() {
        let req = parse(
            r#"
version = 1
name = "Create user"
method = "POST"
url = "http://localhost:3000/users"
body = '{"name":"ada"}'

[headers]
Content-Type = "application/json"

[query]
dry_run = "true"
"#,
        )
        .unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(req.headers["Content-Type"], "application/json");
        assert_eq!(req.query["dry_run"], "true");
        assert_eq!(req.body.as_deref(), Some(r#"{"name":"ada"}"#));
    }

    #[test]
    fn method_defaults_to_get() {
        let req = parse("version = 1\nname = \"x\"\nurl = \"http://a\"\n").unwrap();
        assert_eq!(req.method, "GET");
    }

    #[test]
    fn rejects_unknown_version() {
        let err = parse("version = 2\nname = \"x\"\nurl = \"http://a\"\n").unwrap_err();
        assert!(matches!(err, ParseError::UnsupportedVersion { found: 2 }));
    }

    #[test]
    fn rejects_misspelled_field() {
        assert!(parse("version = 1\nname = \"x\"\nurl = \"http://a\"\nheadres = {}\n").is_err());
    }
}
