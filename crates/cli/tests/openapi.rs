//! `reqlite import openapi` from a URL, then `reqlite run` on what it wrote.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
use common::{reqlite, serve};
use std::ffi::OsStr;

/// The server URL is relative, so it resolves against where the spec came from.
const SPEC: &str = r##"{
  "openapi": "3.0.3",
  "info": { "title": "Tiny Pets", "version": "1" },
  "servers": [{ "url": "/api" }],
  "paths": {
    "/pets/{petId}": { "get": {
      "tags": ["pets"], "operationId": "getPet",
      "parameters": [{ "name": "petId", "in": "path", "required": true, "example": 7 }]
    } },
    "/pets": { "post": {
      "tags": ["pets"], "summary": "Add a pet",
      "requestBody": { "content": { "application/json": {
        "schema": { "$ref": "#/components/schemas/Pet" }
      } } }
    } }
  },
  "components": { "schemas": { "Pet": {
    "type": "object", "properties": { "name": { "type": "string", "example": "Rex" } }
  } } }
}"##;

#[test]
fn an_imported_spec_runs_against_its_server() {
    let base = serve(|seen| match (seen.method.as_str(), seen.path.as_str()) {
        ("GET", "/openapi.json") => ("200 OK", SPEC.into()),
        ("GET", "/api/pets/7") => ("200 OK", r#"{"name": "Rex"}"#.into()),
        ("POST", "/api/pets") if seen.body.contains("\"Rex\"") => {
            ("201 Created", r#"{"id": 8}"#.into())
        }
        _ => ("404 Not Found", "{}".into()),
    });
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("api");
    let url = format!("{base}/openapi.json");
    let import = |force: bool| {
        let mut args = vec![
            OsStr::new("import"),
            OsStr::new("openapi"),
            OsStr::new(&url),
            OsStr::new("-o"),
            out.as_os_str(),
        ];
        if force {
            args.push(OsStr::new("--force"));
        }
        reqlite(&args)
    };

    let (code, _, err) = import(false);
    assert_eq!(code, Some(0), "{err}");
    assert!(err.contains("wrote 2 requests from \"Tiny Pets\""), "{err}");
    let env = out.join("envs").join("tiny-pets.toml");
    let env_text = std::fs::read_to_string(&env).unwrap();
    assert!(
        env_text.contains(&format!("base_url = \"{base}/api\"")),
        "{env_text}"
    );
    assert!(env_text.contains("petId = \"7\""), "{env_text}");

    let (code, stdout, err) = reqlite(&[
        OsStr::new("run"),
        out.as_os_str(),
        OsStr::new("-e"),
        env.as_os_str(),
    ]);
    let all = stdout + &err;
    assert_eq!(code, Some(0), "{all}");
    assert!(all.contains("getPet.toml  200"), "{all}");
    assert!(all.contains("Add a pet.toml  201"), "{all}");

    let (code, _, err) = import(false);
    assert_eq!(code, Some(3), "{err}");
    assert!(err.contains("already exists; pass --force"), "{err}");
    let (code, _, err) = import(true);
    assert_eq!(code, Some(0), "{err}");
}

#[test]
fn a_swagger_2_file_is_refused_with_its_version() {
    let dir = tempfile::tempdir().unwrap();
    let spec = dir.path().join("old.yaml");
    std::fs::write(&spec, "swagger: '2.0'\ninfo: {title: Old}\npaths: {}\n").unwrap();
    let (code, _, err) = reqlite(&[
        OsStr::new("import"),
        OsStr::new("openapi"),
        spec.as_os_str(),
        OsStr::new("-o"),
        dir.path().join("out").as_os_str(),
    ]);
    assert_eq!(code, Some(3), "{err}");
    assert!(err.contains("this is Swagger 2.0"), "{err}");
    assert!(!dir.path().join("out").exists());
}
