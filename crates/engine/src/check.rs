//! Checks a response against the request's assertions, and takes its captures.

use crate::{Resolved, Response};
use reqlite_format::{Op, Source, Step, Subject};
use serde_json::Value;
use std::io::Read;

/// The result of one assertion or capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The line as written, with secrets as `{{name}}`.
    pub text: String,
    pub pass: bool,
    /// Why it failed, such as `got 404`. Empty on a pass.
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Checked {
    pub outcomes: Vec<Outcome>,
    /// Captured names and values, in name order.
    pub captured: Vec<(String, String)>,
}

impl Checked {
    pub fn passed(&self) -> bool {
        self.outcomes.iter().all(|o| o.pass)
    }

    pub fn failed(&self) -> usize {
        self.outcomes.iter().filter(|o| !o.pass).count()
    }
}

/// Runs every assertion and capture of `req` on `resp`. The body is read only
/// when a line needs it.
// SHORTCUT: body and JSON checks hold the whole body in memory. Stream them if
// tests on bodies of hundreds of MB come up.
pub fn check(req: &Resolved, resp: &Response) -> Checked {
    let needs_body = req
        .asserts()
        .iter()
        .any(|(a, _)| matches!(a.subject, Subject::Body | Subject::Json(_)))
        || req.capture().values().any(|s| matches!(s, Source::Json(_)));
    let body = if needs_body {
        read(resp)
    } else {
        Ok(Vec::new())
    };
    let json: Result<Value, String> = match &body {
        Ok(bytes) if needs_body => {
            serde_json::from_slice(bytes).map_err(|e| format!("the body is not JSON: {e}"))
        }
        Ok(_) => Ok(Value::Null),
        Err(e) => Err(e.clone()),
    };
    // Details quote the response, which may echo a secret.
    let hide = |text: String| {
        String::from_utf8_lossy(&resp.redact_token(&req.redact(text.as_bytes()))).into_owned()
    };
    let mut out = Checked::default();
    for (a, shown) in req.asserts() {
        let result = match &a.subject {
            Subject::Status => number(f64::from(resp.status), a.op, &a.value),
            Subject::Time => number(resp.elapsed.as_millis() as f64, a.op, &a.value),
            Subject::Header(name) => header(resp, name, a.op, &a.value),
            Subject::Body => body
                .as_ref()
                .map_err(Clone::clone)
                .and_then(|b| contains_bytes(b, text_of(&a.value).as_bytes())),
            Subject::Json(path) => json
                .as_ref()
                .map_err(Clone::clone)
                .and_then(|j| json_check(get(j, path.steps()), a.op, &a.value)),
        };
        out.outcomes
            .push(outcome(shown.clone(), result.map_err(&hide)));
    }
    for (name, source) in req.capture() {
        let value = match source {
            Source::Header(h) => values(resp, h)
                .into_iter()
                .next()
                .ok_or_else(|| format!("no {h} header")),
            Source::Json(path) => {
                json.as_ref()
                    .map_err(Clone::clone)
                    .and_then(|j| match get(j, path.steps()) {
                        Some(Value::String(s)) => Ok(s.clone()),
                        Some(v) => Ok(v.to_string()),
                        None => Err("not found".to_string()),
                    })
            }
        };
        let text = format!("capture {name} = {source}");
        out.outcomes.push(outcome(
            text,
            value.as_ref().map(drop).map_err(|e| hide(e.clone())),
        ));
        if let Ok(v) = value {
            out.captured.push((name.clone(), v));
        }
    }
    out
}

fn outcome(text: String, result: Result<(), String>) -> Outcome {
    match result {
        Ok(()) => Outcome {
            text,
            pass: true,
            detail: String::new(),
        },
        Err(detail) => Outcome {
            text,
            pass: false,
            detail,
        },
    }
}

fn read(resp: &Response) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    resp.body
        .reader()
        .and_then(|mut r| r.read_to_end(&mut bytes))
        .map_err(|e| format!("cannot read the body: {e}"))?;
    Ok(bytes)
}

/// The expected value: a JSON literal when it is one, else the text itself.
fn expected(value: &str) -> Value {
    serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.to_string()))
}

/// The expected value as text: a quoted string loses its quotes.
fn text_of(value: &str) -> String {
    match expected(value) {
        Value::String(s) => s,
        _ => value.to_string(),
    }
}

fn number(actual: f64, op: Op, value: &str) -> Result<(), String> {
    let Ok(want) = value.trim().parse::<f64>() else {
        return Err(format!("{value:?} is not a number"));
    };
    let ok = match op {
        Op::Eq => actual == want,
        Op::Ne => actual != want,
        Op::Lt => actual < want,
        Op::Gt => actual > want,
        Op::Contains | Op::Exists => false,
    };
    if ok {
        Ok(())
    } else {
        Err(format!("got {actual}"))
    }
}

fn values(resp: &Response, name: &str) -> Vec<String> {
    resp.headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| String::from_utf8_lossy(v).into_owned())
        .collect()
}

fn header(resp: &Response, name: &str, op: Op, value: &str) -> Result<(), String> {
    let found = values(resp, name);
    let Some(first) = found.first() else {
        return Err(format!("no {name} header"));
    };
    let want = text_of(value);
    let ok = match op {
        Op::Exists => true,
        Op::Eq => found.contains(&want),
        Op::Ne => found.iter().all(|v| *v != want),
        Op::Contains => found.iter().any(|v| v.contains(&want)),
        Op::Lt | Op::Gt => false,
    };
    if ok {
        Ok(())
    } else {
        Err(format!("got {}", short(first)))
    }
}

fn contains_bytes(hay: &[u8], needle: &[u8]) -> Result<(), String> {
    if needle.is_empty() || hay.windows(needle.len()).any(|w| w == needle) {
        Ok(())
    } else {
        Err("not in the body".to_string())
    }
}

fn get<'a>(mut v: &'a Value, steps: &[Step]) -> Option<&'a Value> {
    for step in steps {
        v = match step {
            Step::Key(k) => v.as_object()?.get(k)?,
            Step::Index(i) => v.as_array()?.get(*i)?,
        };
    }
    Some(v)
}

/// JSON equality, where `42` and `42.0` are the same number.
fn same(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) if a.is_number() && b.is_number() => x == y,
        _ => a == b,
    }
}

fn json_check(actual: Option<&Value>, op: Op, value: &str) -> Result<(), String> {
    let Some(actual) = actual else {
        return Err("not found".to_string());
    };
    let want = expected(value);
    let ok = match op {
        Op::Exists => true,
        Op::Eq => same(actual, &want),
        Op::Ne => !same(actual, &want),
        Op::Contains => match actual {
            Value::String(s) => s.contains(&text_of(value)),
            Value::Array(items) => items.iter().any(|i| same(i, &want)),
            Value::Object(map) => map.contains_key(&text_of(value)),
            _ => false,
        },
        Op::Lt | Op::Gt => {
            return match actual.as_f64() {
                Some(n) => number(n, op, value),
                None => Err(format!("got {}, not a number", short(&actual.to_string()))),
            };
        }
    };
    if ok {
        Ok(())
    } else {
        Err(format!("got {}", short(&actual.to_string())))
    }
}

/// At most 80 characters, so one long value cannot flood a report.
fn short(s: &str) -> String {
    match s.char_indices().nth(80) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Body, Store, resolve};
    use reqlite_format::parse_env;
    use std::path::Path;
    use std::time::Duration;

    const BODY: &str =
        r#"{"id": 42, "name": "ada", "tags": ["admin", "ops"], "score": 7.0, "echo": "s3cret"}"#;

    fn run(lines: &[&str], capture: &str, body: &str) -> Checked {
        let asserts: Vec<String> = lines.iter().map(|l| format!("{l:?}")).collect();
        let file = format!(
            "version = 3\nname = \"x\"\nurl = \"http://h/\"\nassert = [{}]\n{capture}",
            asserts.join(", ")
        );
        let req = reqlite_format::parse(&file).unwrap();
        let env = parse_env(
            Path::new("envs/dev.toml"),
            "version = 1\nsecrets = ['key']\n",
            Some("version = 1\n[vars]\nkey = 's3cret'\n"),
        )
        .unwrap();
        let resp = Response {
            status: 201,
            headers: vec![
                ("Content-Type".into(), b"application/json".to_vec()),
                ("X-Token".into(), b"t-1".to_vec()),
            ],
            body: Body(Store::Memory(body.as_bytes().to_vec())),
            elapsed: Duration::from_millis(120),
            oauth_token: None,
        };
        check(&resolve(&req, &env).unwrap(), &resp)
    }

    fn fails(c: &Checked) -> Vec<(String, String)> {
        c.outcomes
            .iter()
            .filter(|o| !o.pass)
            .map(|o| (o.text.clone(), o.detail.clone()))
            .collect()
    }

    #[test]
    fn a_matching_response_passes_and_captures() {
        let c = run(
            &[
                "status == 201",
                "status < 300",
                "time < 500",
                "header content-type contains json",
                "header X-Token == t-1",
                "json $.id == 42",
                "json $.score == 7",
                "json $.name == \"ada\"",
                "json $.name != bob",
                "json $.tags contains admin",
                "json $.tags[1] == ops",
                "json $.id > 41",
                "body contains \"ada\"",
            ],
            "[capture]\nid = \"json $.id\"\nname = \"json $.name\"\ntoken = \"header X-Token\"\n",
            BODY,
        );
        assert_eq!(fails(&c), []);
        assert!(c.passed());
        assert_eq!(
            c.captured,
            [
                ("id".to_string(), "42".to_string()),
                ("name".to_string(), "ada".to_string()),
                ("token".to_string(), "t-1".to_string())
            ]
        );
    }

    #[test]
    fn a_failure_says_what_came_back() {
        let c = run(
            &[
                "status == 200",
                "time > 500",
                "header X-Missing exists",
                "header X-Token == t-2",
                "json $.name == \"bob\"",
                "json $.nope exists",
                "json $.tags contains root",
                "json $.name > 1",
                "body contains bob",
            ],
            "[capture]\ngone = \"json $.gone\"\n",
            BODY,
        );
        assert_eq!(
            fails(&c),
            [
                ("status == 200".into(), "got 201".into()),
                ("time > 500".into(), "got 120".into()),
                (
                    "header X-Missing exists".into(),
                    "no X-Missing header".into()
                ),
                ("header X-Token == t-2".into(), "got t-1".into()),
                ("json $.name == \"bob\"".into(), "got \"ada\"".into()),
                ("json $.nope exists".into(), "not found".into()),
                (
                    "json $.tags contains root".into(),
                    "got [\"admin\",\"ops\"]".into()
                ),
                ("json $.name > 1".into(), "got \"ada\", not a number".into()),
                ("body contains bob".into(), "not in the body".into()),
                ("capture gone = json $.gone".into(), "not found".into()),
            ]
        );
        assert_eq!(c.failed(), 10);
        assert!(c.captured.is_empty());
    }

    #[test]
    fn secrets_stay_hidden_in_the_report() {
        let c = run(&["json $.echo == \"{{key}}x\""], "", BODY);
        assert_eq!(
            fails(&c),
            [(
                "json $.echo == \"{{key}}x\"".into(),
                "got \"{{key}}\"".into()
            )]
        );
    }

    #[test]
    fn json_lines_fail_on_a_body_that_is_not_json() {
        let c = run(
            &["status == 201", "json $.id exists", "body contains plain"],
            "",
            "plain text",
        );
        let f = fails(&c);
        assert_eq!(f.len(), 1);
        assert!(f[0].1.starts_with("the body is not JSON"), "{f:?}");
    }
}
