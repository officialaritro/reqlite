//! Converts other tools' request formats to and from Reqlite request files.
//! An importer never drops something silently: whatever it cannot map comes
//! back as a [`Warning`].

pub mod curl;
pub mod openapi;
pub mod postman;

use reqlite_format::Request;
use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;

/// Request files an importer made, with what it could not map.
#[derive(Debug)]
pub struct Collection {
    pub name: String,
    /// Paths relative to the output directory, in collection order.
    pub files: Vec<(PathBuf, Request)>,
    pub warnings: Vec<Warning>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Warning {
    /// An option with no Reqlite equivalent. It was skipped.
    Unsupported {
        option: String,
        item: Option<String>,
    },
    /// A body read from a file (`@file`). The file content was not imported.
    BodyFromFile { path: String },
    /// Credentials that would end up committed. They were not imported.
    CredentialsSkipped { option: String },
    /// A credential became a placeholder. Its value must go in the environment.
    CredentialPlaceholder { option: String, placeholder: String },
    /// A header holding what looks like a credential, written as is.
    LiteralCredential { header: String },
    /// A Postman pre-request or test script. Reqlite runs no scripts.
    Script { item: String, listen: String },
    /// A server URL with no scheme and host. It went in `base_url` as is.
    RelativeServer { url: String },
    /// A multipart part that sends a file. Its path is the part name.
    FilePart { item: String, part: String },
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Warning::Unsupported { option, item: None } => {
                write!(f, "{option} is not supported and was skipped")
            }
            Warning::Unsupported {
                option,
                item: Some(item),
            } => write!(f, "{item}: {option} is not supported and was skipped"),
            Warning::BodyFromFile { path } => write!(
                f,
                "the body is read from the file {path}; paste its content into the body"
            ),
            Warning::CredentialsSkipped { option } => write!(
                f,
                "credentials from {option} were not imported; add an Authorization header that uses a {{{{secret}}}}"
            ),
            Warning::CredentialPlaceholder {
                option,
                placeholder,
            } => write!(
                f,
                "the credential from {option} became {{{{{placeholder}}}}}; set {placeholder} as a secret in the environment's .local.toml file"
            ),
            Warning::Script { item, listen } => {
                write!(f, "{item}: the {listen} script was not imported")
            }
            Warning::RelativeServer { url } => write!(
                f,
                "the server URL {url} is relative; set base_url in the environment to the full URL"
            ),
            Warning::FilePart { item, part } => write!(
                f,
                "{item}: part {part} sends a file; set its path in the request file"
            ),
            Warning::LiteralCredential { header } => write!(
                f,
                "header {header} holds a literal credential; replace it with a {{{{secret}}}} before you commit"
            ),
        }
    }
}

const CREDENTIAL_HEADERS: [&str; 5] = [
    "authorization",
    "proxy-authorization",
    "cookie",
    "x-api-key",
    "api-key",
];

/// A header value that is a literal credential, not a `{{placeholder}}`.
fn is_literal_credential(name: &str, value: &str) -> bool {
    CREDENTIAL_HEADERS.contains(&name.to_ascii_lowercase().as_str()) && !value.contains("{{")
}

/// A name safe on Windows, macOS and Linux.
pub(crate) fn file_name(name: &str) -> String {
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
pub(crate) fn unique(taken: &mut BTreeSet<String>, base: &str, ext: &str) -> String {
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
