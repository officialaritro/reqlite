//! Converts other tools' request formats to and from Reqlite request files.
//! An importer never drops something silently: whatever it cannot map comes
//! back as a [`Warning`].

pub mod curl;
pub mod postman;

use std::fmt;

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
    /// A header holding what looks like a credential, written as is.
    LiteralCredential { header: String },
    /// A Postman pre-request or test script. Reqlite runs no scripts.
    Script { item: String, listen: String },
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
            Warning::Script { item, listen } => {
                write!(f, "{item}: the {listen} script was not imported")
            }
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
