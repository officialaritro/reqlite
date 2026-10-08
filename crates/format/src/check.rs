//! Assertions and captures, added in format version 3. Each is one line of
//! text, so a file stays easy to read and there is no script to run:
//!
//! ```toml
//! assert = ["status == 201", "json $.id exists", "time < 500"]
//!
//! [capture]
//! user_id = "json $.id"
//! ```

use serde::{Deserialize, Serialize};
use std::fmt;

/// One check on a response: a subject, an operator and, for most operators,
/// a value. The value may use `{{placeholders}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Assert {
    pub subject: Subject,
    pub op: Op,
    /// As written, `""` for `exists`. A JSON literal (`42`, `"ada"`, `true`)
    /// or bare text.
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    Status,
    Header(String),
    Json(JsonPath),
    Body,
    /// The response time in milliseconds.
    Time,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Contains,
    Exists,
    Lt,
    Gt,
}

impl Op {
    const ALL: [(Op, &'static str); 6] = [
        (Op::Eq, "=="),
        (Op::Ne, "!="),
        (Op::Contains, "contains"),
        (Op::Exists, "exists"),
        (Op::Lt, "<"),
        (Op::Gt, ">"),
    ];

    fn as_str(self) -> &'static str {
        Op::ALL
            .iter()
            .find(|(op, _)| *op == self)
            .map_or("", |(_, s)| s)
    }
}

/// Where a capture takes its value from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Source {
    Json(JsonPath),
    Header(String),
}

/// A path into a JSON value: `$`, then `.key`, `["key"]` or `[index]` steps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonPath {
    text: String,
    steps: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Key(String),
    Index(usize),
}

impl JsonPath {
    pub fn parse(text: &str) -> Result<JsonPath, String> {
        let bad = |why: &str| Err(format!("bad JSON path {text:?}: {why}"));
        let Some(mut rest) = text.strip_prefix('$') else {
            return bad("it starts with $");
        };
        let mut steps = Vec::new();
        while !rest.is_empty() {
            if let Some(r) = rest.strip_prefix('.') {
                let end = r.find(['.', '[']).unwrap_or(r.len());
                if end == 0 {
                    return bad("a . needs a key after it");
                }
                steps.push(Step::Key(r[..end].to_string()));
                rest = &r[end..];
            } else if let Some(r) = rest.strip_prefix("[\"") {
                let Some(end) = r.find("\"]") else {
                    return bad("a [\" needs a closing \"]");
                };
                steps.push(Step::Key(r[..end].to_string()));
                rest = &r[end + 2..];
            } else if let Some(r) = rest.strip_prefix('[') {
                let Some(end) = r.find(']') else {
                    return bad("a [ needs a closing ]");
                };
                let Ok(i) = r[..end].parse() else {
                    return bad("an index is a whole number, such as [0]");
                };
                steps.push(Step::Index(i));
                rest = &r[end + 1..];
            } else {
                return bad("steps are .key, [\"key\"] or [0]");
            }
        }
        Ok(JsonPath {
            text: text.to_string(),
            steps,
        })
    }

    pub fn steps(&self) -> &[Step] {
        &self.steps
    }
}

impl fmt::Display for JsonPath {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// Splits off the first word.
fn word(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    let end = s.find(char::is_whitespace).unwrap_or(s.len());
    (&s[..end], s[end..].trim_start())
}

/// Splits off a JSON path. A `["quoted key"]` step may hold spaces.
fn path_word(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    let mut i = 0;
    while i < s.len() {
        let r = &s[i..];
        if r.starts_with("[\"") {
            i += r.find("\"]").map_or(r.len(), |e| e + 2);
        } else if r.starts_with(char::is_whitespace) {
            break;
        } else {
            i += r.chars().next().map_or(1, char::len_utf8);
        }
    }
    (&s[..i], s[i..].trim_start())
}

/// `header NAME` or `json PATH`, and the text after it.
fn subject(line: &str) -> Result<(Subject, &str), String> {
    let (kind, rest) = word(line);
    let (arg, after) = if kind == "json" {
        path_word(rest)
    } else {
        word(rest)
    };
    let need = |what: &str| format!("{kind:?} needs {what} after it");
    Ok(match kind {
        "status" => (Subject::Status, rest),
        "body" => (Subject::Body, rest),
        "time" => (Subject::Time, rest),
        "header" if arg.is_empty() => return Err(need("a header name")),
        "header" => (Subject::Header(arg.to_string()), after),
        "json" if arg.is_empty() => return Err(need("a JSON path")),
        "json" => (Subject::Json(JsonPath::parse(arg)?), after),
        _ => {
            return Err(format!(
                "{kind:?} is not a subject; use status, header, json, body or time"
            ));
        }
    })
}

impl TryFrom<String> for Assert {
    type Error = String;

    fn try_from(line: String) -> Result<Assert, String> {
        let at = |e: String| format!("assert {line:?}: {e}");
        let (subject, rest) = subject(&line).map_err(at)?;
        let (op_text, value) = word(rest);
        let Some(&(op, _)) = Op::ALL.iter().find(|(_, s)| *s == op_text) else {
            return Err(at(format!(
                "{op_text:?} is not an operator; use ==, !=, contains, exists, < or >"
            )));
        };
        let allowed: &[Op] = match subject {
            Subject::Status => &[Op::Eq, Op::Ne, Op::Lt, Op::Gt],
            Subject::Time => &[Op::Lt, Op::Gt],
            Subject::Body => &[Op::Contains],
            Subject::Header(_) => &[Op::Eq, Op::Ne, Op::Contains, Op::Exists],
            Subject::Json(_) => &[Op::Eq, Op::Ne, Op::Contains, Op::Exists, Op::Lt, Op::Gt],
        };
        if !allowed.contains(&op) {
            return Err(at(format!("{op_text} does not apply here")));
        }
        match (op, value.is_empty()) {
            (Op::Exists, false) => return Err(at("exists takes no value".into())),
            (Op::Exists, true) => {}
            (_, true) => return Err(at(format!("{op_text} needs a value"))),
            (Op::Lt | Op::Gt, false) if !value.contains("{{") && value.parse::<f64>().is_err() => {
                return Err(at(format!("{op_text} needs a number")));
            }
            _ => {}
        }
        if subject == Subject::Status && !value.contains("{{") && value.parse::<u16>().is_err() {
            return Err(at("a status is a number, such as 200".into()));
        }
        Ok(Assert {
            subject,
            op,
            value: value.to_string(),
        })
    }
}

impl fmt::Display for Assert {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match &self.subject {
            Subject::Status => f.write_str("status")?,
            Subject::Header(name) => write!(f, "header {name}")?,
            Subject::Json(path) => write!(f, "json {path}")?,
            Subject::Body => f.write_str("body")?,
            Subject::Time => f.write_str("time")?,
        }
        write!(f, " {}", self.op.as_str())?;
        if !self.value.is_empty() {
            write!(f, " {}", self.value)?;
        }
        Ok(())
    }
}

impl From<Assert> for String {
    fn from(a: Assert) -> String {
        a.to_string()
    }
}

impl TryFrom<String> for Source {
    type Error = String;

    fn try_from(text: String) -> Result<Source, String> {
        let at = |e: String| format!("capture {text:?}: {e}");
        let (subject, rest) = subject(&text).map_err(at)?;
        if !rest.is_empty() {
            return Err(at(format!("{rest:?} is extra")));
        }
        match subject {
            Subject::Json(path) => Ok(Source::Json(path)),
            Subject::Header(name) => Ok(Source::Header(name)),
            _ => Err(at("a capture takes json PATH or header NAME".into())),
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Source::Json(path) => write!(f, "json {path}"),
            Source::Header(name) => write!(f, "header {name}"),
        }
    }
}

impl From<Source> for String {
    fn from(s: Source) -> String {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Result<Assert, String> {
        Assert::try_from(line.to_string())
    }

    #[test]
    fn each_subject_reads_and_writes_back() {
        for line in [
            "status == 201",
            "status < 400",
            "header Content-Type contains json",
            "header X-Id exists",
            "json $.user.id == 42",
            "json $.items[0][\"full name\"] == \"Ada Lovelace\"",
            "json $.tags contains admin",
            "json $.count > 2",
            "body contains ok",
            "time < 500",
            "json $.id == {{id}}",
        ] {
            assert_eq!(parse(line).unwrap().to_string(), line);
        }
        let a = parse("json $.items[0].name   ==   \"two  spaces\"").unwrap();
        assert_eq!(a.value, "\"two  spaces\"");
        let Subject::Json(path) = a.subject else {
            panic!()
        };
        assert_eq!(
            path.steps(),
            [
                Step::Key("items".into()),
                Step::Index(0),
                Step::Key("name".into())
            ]
        );
    }

    #[test]
    fn a_wrong_assert_says_what_is_wrong() {
        for (line, why) in [
            ("code == 200", "is not a subject"),
            ("status is 200", "is not an operator"),
            ("status == ok", "a status is a number"),
            ("status contains 2", "does not apply"),
            ("body == x", "does not apply"),
            ("time == 5", "does not apply"),
            ("time < soon", "needs a number"),
            ("header", "needs a header name"),
            ("json $.a exists 1", "takes no value"),
            ("json $.a ==", "needs a value"),
            ("json id == 1", "starts with $"),
            ("json $.a[x] == 1", "whole number"),
            ("json $..a exists", "needs a key"),
        ] {
            let err = parse(line).unwrap_err();
            assert!(err.contains(why), "{line}: {err}");
        }
    }

    #[test]
    fn a_capture_takes_a_json_path_or_a_header() {
        let src = |t: &str| Source::try_from(t.to_string());
        assert_eq!(src("header X-Token").unwrap().to_string(), "header X-Token");
        assert_eq!(
            src("json $.data.token").unwrap().to_string(),
            "json $.data.token"
        );
        assert!(
            src("status")
                .unwrap_err()
                .contains("json PATH or header NAME")
        );
        assert!(src("json $.a == 1").unwrap_err().contains("is extra"));
    }
}
