//! GraphQL schema introspection. The schema prints as SDL, so a reader sees
//! each type's fields and arguments while writing a query.

use reqlite_format::{Body, Method, Request};
use serde_json::Value;

/// The standard introspection query, cut down to what [`schema`] prints.
pub const INTROSPECTION: &str = "query IntrospectionQuery { __schema { \
queryType { name } mutationType { name } subscriptionType { name } \
types { kind name \
fields(includeDeprecated: true) { name args { name type { ...TypeRef } } type { ...TypeRef } } \
inputFields { name type { ...TypeRef } } \
interfaces { name } enumValues(includeDeprecated: true) { name } possibleTypes { name } } } } \
fragment TypeRef on __Type { kind name ofType { kind name ofType { kind name ofType { kind name \
ofType { kind name ofType { kind name ofType { kind name } } } } } } }";

/// `req` with its body swapped for the introspection query. The URL, headers
/// and auth stay, so the schema comes from the same server as the request.
pub fn introspection(req: &Request) -> Request {
    Request {
        method: Method::try_from("POST".to_string()).unwrap_or_default(),
        body: Some(Body::Graphql {
            query: INTROSPECTION.to_string(),
            variables: None,
        }),
        assert: Vec::new(),
        capture: Default::default(),
        grpc: None,
        version: 2,
        ..req.clone()
    }
}

const BUILT_IN: [&str; 5] = ["String", "Int", "Float", "Boolean", "ID"];

/// The schema in an introspection response, as SDL: root types first, then
/// the others by name. Built-in scalars and `__` types are left out.
pub fn schema(body: &[u8]) -> Result<String, String> {
    let json: Value =
        serde_json::from_slice(body).map_err(|e| format!("the answer is not JSON: {e}"))?;
    let Some(schema) = json.pointer("/data/__schema") else {
        let errors: Vec<&str> = json
            .get("errors")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|e| e.get("message").and_then(Value::as_str))
            .collect();
        return Err(match errors.as_slice() {
            [] => "the answer has no schema; does the server allow introspection?".into(),
            msgs => format!("the server refused introspection: {}", msgs.join("; ")),
        });
    };
    let root = |key: &str| {
        schema
            .pointer(&format!("/{key}/name"))
            .and_then(Value::as_str)
    };
    let roots: Vec<&str> = ["queryType", "mutationType", "subscriptionType"]
        .into_iter()
        .filter_map(root)
        .collect();
    let mut types: Vec<&Value> = schema
        .get("types")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|t| {
            let name = name(t);
            !name.starts_with("__") && !BUILT_IN.contains(&name)
        })
        .collect();
    let rank = |t: &&Value| {
        roots
            .iter()
            .position(|r| *r == name(t))
            .unwrap_or(roots.len())
    };
    types.sort_by(|a, b| rank(a).cmp(&rank(b)).then(name(a).cmp(name(b))));
    let mut out = String::new();
    for t in types {
        print_type(&mut out, t);
    }
    Ok(out)
}

fn name(v: &Value) -> &str {
    v.get("name").and_then(Value::as_str).unwrap_or_default()
}

fn list<'a>(v: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    v.get(key).and_then(Value::as_array).into_iter().flatten()
}

/// `[User!]!` and the like.
fn type_ref(v: &Value) -> String {
    let inner = || v.get("ofType").map(type_ref).unwrap_or_default();
    match v.get("kind").and_then(Value::as_str) {
        Some("NON_NULL") => format!("{}!", inner()),
        Some("LIST") => format!("[{}]", inner()),
        _ => name(v).to_string(),
    }
}

fn print_type(out: &mut String, t: &Value) {
    let n = name(t);
    match t.get("kind").and_then(Value::as_str).unwrap_or_default() {
        "SCALAR" => out.push_str(&format!("scalar {n}\n\n")),
        "UNION" => {
            let members: Vec<&str> = list(t, "possibleTypes").map(name).collect();
            out.push_str(&format!("union {n} = {}\n\n", members.join(" | ")));
        }
        "ENUM" => {
            out.push_str(&format!("enum {n} {{\n"));
            for v in list(t, "enumValues") {
                out.push_str(&format!("  {}\n", name(v)));
            }
            out.push_str("}\n\n");
        }
        kind => {
            let (word, key) = match kind {
                "INPUT_OBJECT" => ("input", "inputFields"),
                "INTERFACE" => ("interface", "fields"),
                _ => ("type", "fields"),
            };
            let interfaces: Vec<&str> = list(t, "interfaces").map(name).collect();
            match interfaces.as_slice() {
                [] => out.push_str(&format!("{word} {n} {{\n")),
                i => out.push_str(&format!("{word} {n} implements {} {{\n", i.join(" & "))),
            }
            for f in list(t, key) {
                let args: Vec<String> = list(f, "args")
                    .map(|a| {
                        format!(
                            "{}: {}",
                            name(a),
                            a.get("type").map(type_ref).unwrap_or_default()
                        )
                    })
                    .collect();
                let args = if args.is_empty() {
                    String::new()
                } else {
                    format!("({})", args.join(", "))
                };
                let ty = f.get("type").map(type_ref).unwrap_or_default();
                out.push_str(&format!("  {}{args}: {ty}\n", name(f)));
            }
            out.push_str("}\n\n");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ty(kind: &str, name: Option<&str>, of: Option<Value>) -> Value {
        serde_json::json!({"kind": kind, "name": name, "ofType": of})
    }

    #[test]
    fn the_schema_prints_as_sdl_with_the_roots_first() {
        let id = ty("NON_NULL", None, Some(ty("SCALAR", Some("ID"), None)));
        let users = ty(
            "NON_NULL",
            None,
            Some(ty(
                "LIST",
                None,
                Some(ty("NON_NULL", None, Some(ty("OBJECT", Some("User"), None)))),
            )),
        );
        let body = serde_json::json!({"data": {"__schema": {
        "queryType": {"name": "Query"},
        "mutationType": null,
        "subscriptionType": null,
        "types": [
            {"kind": "OBJECT", "name": "User", "interfaces": [{"name": "Node"}], "fields": [
                {"name": "id", "args": [], "type": id},
                {"name": "role", "args": [], "type": ty("ENUM", Some("Role"), None)}]},
            {"kind": "OBJECT", "name": "Query", "interfaces": [], "fields": [
                {"name": "user", "args": [{"name": "id", "type": id}],
                 "type": ty("OBJECT", Some("User"), None)},
                {"name": "users", "args": [], "type": users}]},
            {"kind": "ENUM", "name": "Role", "enumValues": [{"name": "ADMIN"}, {"name": "USER"}]},
            {"kind": "UNION", "name": "Result", "possibleTypes": [{"name": "User"}, {"name": "Query"}]},
            {"kind": "INPUT_OBJECT", "name": "NewUser", "inputFields": [
                {"name": "name", "type": ty("SCALAR", Some("String"), None)}]},
            {"kind": "SCALAR", "name": "String"},
            {"kind": "SCALAR", "name": "Date"},
            {"kind": "OBJECT", "name": "__Type", "fields": []}
        ]}}});
        let sdl = schema(body.to_string().as_bytes()).unwrap();
        assert_eq!(
            sdl,
            "type Query {\n  user(id: ID!): User\n  users: [User!]!\n}\n\n\
             scalar Date\n\n\
             input NewUser {\n  name: String\n}\n\n\
             union Result = User | Query\n\n\
             enum Role {\n  ADMIN\n  USER\n}\n\n\
             type User implements Node {\n  id: ID!\n  role: Role\n}\n\n"
        );
    }

    #[test]
    fn a_refusal_names_the_server_errors() {
        let refused = br#"{"errors": [{"message": "introspection is disabled"}]}"#;
        assert_eq!(
            schema(refused).unwrap_err(),
            "the server refused introspection: introspection is disabled"
        );
        assert!(
            schema(b"<html>")
                .unwrap_err()
                .starts_with("the answer is not JSON")
        );
    }

    #[test]
    fn introspection_keeps_the_url_headers_and_auth() {
        let req = reqlite_format::parse(
            "version = 3\nname = \"q\"\nmethod = \"GET\"\nurl = \"http://h/graphql\"\nassert = [\"status == 200\"]\n\n[headers]\nX-Team = \"a\"\n\n[auth]\ntype = \"bearer\"\ntoken = \"{{t}}\"\n",
        )
        .unwrap();
        let i = introspection(&req);
        assert_eq!(
            (i.method.as_str(), i.url.as_str()),
            ("POST", "http://h/graphql")
        );
        assert_eq!(i.headers, req.headers);
        assert_eq!(i.auth, req.auth);
        assert!(i.assert.is_empty());
        assert!(reqlite_format::validate(&i).is_ok());
    }
}
