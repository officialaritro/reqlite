//! Request bodies and auth helpers, added in format version 2.
//!
//! A version 1 body is a string, sent as is. Version 2 also allows a table:
//!
//! ```toml
//! [body]
//! type = "json"            # "text", "json", "form", "multipart", "file" or "graphql"
//! text = '{"name": "ada"}'
//! ```
//!
//! File paths are relative to the request file. The file is read at send time.

use crate::Params;
use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::{Deserialize, Serialize, Serializer};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    /// Sent as is, with no Content-Type of its own (`body = "..."`).
    Text(String),
    /// Sent as is, with `Content-Type: application/json` unless a header sets one.
    Json(String),
    /// URL-encoded fields (`application/x-www-form-urlencoded`).
    Form(Params),
    /// `multipart/form-data`, text and file parts in order.
    Multipart(Vec<Part>),
    /// One file, sent as the whole body.
    File { path: String },
    /// A GraphQL operation, sent as the JSON `{"query": ..., "variables": ...}`.
    /// `variables` is JSON text, checked once its placeholders are filled.
    Graphql {
        query: String,
        variables: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Part {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// For a file part. Without it the part is `application/octet-stream`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
}

/// The table form of a body, as it is in the file.
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
enum Table {
    Text {
        text: String,
    },
    Json {
        text: String,
    },
    Form {
        #[serde(default)]
        fields: Params,
    },
    Multipart {
        #[serde(default)]
        parts: Vec<Part>,
    },
    File {
        path: String,
    },
    Graphql {
        query: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        variables: Option<String>,
    },
}

impl Serialize for Body {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let table = match self {
            // The version 1 form, so files that use nothing new stay as they are.
            Body::Text(text) => return s.serialize_str(text),
            Body::Json(text) => Table::Json { text: text.clone() },
            Body::Form(fields) => Table::Form {
                fields: fields.clone(),
            },
            Body::Multipart(parts) => Table::Multipart {
                parts: parts.clone(),
            },
            Body::File { path } => Table::File { path: path.clone() },
            Body::Graphql { query, variables } => Table::Graphql {
                query: query.clone(),
                variables: variables.clone(),
            },
        };
        table.serialize(s)
    }
}

impl<'de> Deserialize<'de> for Body {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Body;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a string, or a table with a type")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Body, E> {
                Ok(Body::Text(v.to_string()))
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Body, A::Error> {
                let table = Table::deserialize(de::value::MapAccessDeserializer::new(map))?;
                Ok(match table {
                    Table::Text { text } => Body::Text(text),
                    Table::Json { text } => Body::Json(text),
                    Table::Form { fields } => Body::Form(fields),
                    Table::Multipart { parts } => Body::Multipart(parts),
                    Table::File { path } => Body::File { path },
                    Table::Graphql { query, variables } => Body::Graphql { query, variables },
                })
            }
        }
        d.deserialize_any(V)
    }
}

/// Builds the auth header or query value from the fields, so no one writes
/// `Authorization: Basic ...` by hand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Auth {
    /// `Authorization: Bearer <token>`. A JWT goes here.
    Bearer { token: String },
    /// `Authorization: Basic base64(username:password)`.
    Basic { username: String, password: String },
    /// A key in a header, or in the query.
    ApiKey {
        name: String,
        value: String,
        #[serde(rename = "in", default)]
        location: KeyIn,
    },
    /// `Authorization: Bearer <token>`, with the token fetched, cached in the
    /// OS keychain and refreshed by Reqlite.
    Oauth2(OAuth2),
}

/// An OAuth 2.0 client. Every field may use `{{placeholders}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OAuth2 {
    pub grant: Grant,
    pub token_url: String,
    /// The authorization endpoint, for the `authorization_code` grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_url: Option<String>,
    /// The device authorization endpoint, for the `device_code` grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_url: Option<String>,
    pub client_id: String,
    /// Left out for a public client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// How the client proves itself to the token endpoint.
    #[serde(default, skip_serializing_if = "is_default")]
    pub client_auth: ClientAuth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Grant {
    #[default]
    ClientCredentials,
    /// With PKCE and a redirect to a loopback port on this machine.
    AuthorizationCode,
    /// RFC 8628: the user enters a code on another page.
    DeviceCode,
}

impl Grant {
    pub const ALL: [Grant; 3] = [
        Grant::ClientCredentials,
        Grant::AuthorizationCode,
        Grant::DeviceCode,
    ];
}

impl fmt::Display for Grant {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match self {
            Grant::ClientCredentials => "Client credentials",
            Grant::AuthorizationCode => "Authorization code",
            Grant::DeviceCode => "Device code",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientAuth {
    /// HTTP Basic with the client ID and secret, as RFC 6749 prefers.
    #[default]
    Basic,
    /// The client ID and secret as form fields in the body.
    Body,
}

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyIn {
    #[default]
    Header,
    Query,
}

/// The JSON a GraphQL body sends. `variables` goes in as written, so its
/// JSON is checked by the caller; empty variables are left out.
pub fn graphql_json(query: &str, variables: Option<&str>) -> String {
    let mut out = String::from("{\"query\":\"");
    for c in query.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    if let Some(v) = variables.map(str::trim).filter(|v| !v.is_empty()) {
        out.push_str(",\"variables\":");
        out.push_str(v);
    }
    out.push('}');
    out
}

impl Auth {
    /// The header this auth sets, if it sets one.
    pub fn header(&self) -> Option<&str> {
        match self {
            Auth::Bearer { .. } | Auth::Basic { .. } | Auth::Oauth2(_) => Some("Authorization"),
            Auth::ApiKey {
                name,
                location: KeyIn::Header,
                ..
            } => Some(name),
            Auth::ApiKey { .. } => None,
        }
    }
}
