//! GraphQL against a local server: the JSON a query sends, and the schema
//! that introspection prints.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
use common::{reqlite, serve, write};
use std::ffi::OsStr;

const SCHEMA: &str = r#"{"data": {"__schema": {
  "queryType": {"name": "Query"}, "mutationType": null, "subscriptionType": null,
  "types": [
    {"kind": "OBJECT", "name": "Query", "interfaces": [], "fields": [
      {"name": "user", "args": [{"name": "id", "type": {"kind": "NON_NULL", "name": null, "ofType": {"kind": "SCALAR", "name": "ID", "ofType": null}}}],
       "type": {"kind": "OBJECT", "name": "User", "ofType": null}}]},
    {"kind": "OBJECT", "name": "User", "interfaces": [], "fields": [
      {"name": "name", "args": [], "type": {"kind": "SCALAR", "name": "String", "ofType": null}}]}
  ]}}}"#;

/// Echoes each request back as `{"data": <the JSON it got>}`, and answers
/// introspection with a small schema. Only a JSON POST with a token counts.
fn server() -> String {
    serve(|seen| {
        let ok = seen.method == "POST"
            && seen.header("content-type") == "application/json"
            && seen.header("authorization") == "Bearer t-1";
        match () {
            () if !ok => (
                "400 Bad Request",
                r#"{"errors": [{"message": "bad request"}]}"#.into(),
            ),
            () if seen.body.contains("__schema") => ("200 OK", SCHEMA.into()),
            () => ("200 OK", format!(r#"{{"data": {}}}"#, seen.body)),
        }
    })
}

fn file(base: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "user.toml",
        &format!(
            "version = 2\nname = \"user\"\nmethod = \"POST\"\nurl = \"{base}/graphql\"\n\n[body]\ntype = \"graphql\"\nquery = \"query ($id: ID!) {{ user(id: $id) {{ name }} }}\"\nvariables = '{{\"id\": \"7\"}}'\n\n[auth]\ntype = \"bearer\"\ntoken = \"t-1\"\n"
        ),
    );
    dir
}

#[test]
fn a_query_goes_as_a_json_post_with_its_variables() {
    let dir = file(&server());
    let path = dir.path().join("user.toml");
    let (code, out, err) = reqlite(&[OsStr::new("send"), path.as_os_str()]);
    assert_eq!(code, Some(0), "{err}");
    let echoed: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        echoed,
        serde_json::json!({"data": {
            "query": "query ($id: ID!) { user(id: $id) { name } }",
            "variables": {"id": "7"}
        }})
    );
}

#[test]
fn the_schema_prints_as_sdl() {
    let dir = file(&server());
    let path = dir.path().join("user.toml");
    let (code, out, err) = reqlite(&[
        OsStr::new("graphql"),
        OsStr::new("schema"),
        path.as_os_str(),
    ]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(
        out,
        "type Query {\n  user(id: ID!): User\n}\n\ntype User {\n  name: String\n}\n\n"
    );
}

#[test]
fn a_refused_introspection_exits_1_with_the_server_message() {
    let base = server();
    let dir = file(&base);
    let path = dir.path().join("user.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("t-1", "wrong")).unwrap();
    let (code, out, err) = reqlite(&[
        OsStr::new("graphql"),
        OsStr::new("schema"),
        path.as_os_str(),
    ]);
    assert_eq!(code, Some(1), "{out}{err}");
    assert!(
        err.contains("the server refused introspection: bad request (status 400)"),
        "{err}"
    );
}
