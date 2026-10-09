//! gRPC calls, added in format version 4:
//!
//! ```toml
//! url = "http://localhost:50051"   # https:// for TLS
//!
//! [grpc]
//! method = "users.v1.Users/GetUser"
//! proto = "protos/users.proto"     # optional: without it, server reflection
//! message = '{"id": "{{user_id}}"}'
//! ```

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grpc {
    /// `package.Service/Method`.
    pub method: String,
    /// A `.proto` file that defines the service, relative to the request
    /// file. Its imports are looked up next to it. Without it, the server is
    /// asked for its schema by reflection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proto: Option<String>,
    /// The request message as JSON, in the protobuf JSON mapping. Empty
    /// sends an empty message.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
}

impl Grpc {
    /// The service and the method, when `method` has that shape.
    pub fn parts(&self) -> Option<(&str, &str)> {
        let (service, method) = self.method.trim().split_once('/')?;
        let ok = |s: &str| !s.is_empty() && !s.contains(char::is_whitespace);
        (ok(service) && ok(method) && !method.contains('/')).then_some((service, method))
    }
}
