//! The form being edited and how it maps to a request file. No iced types live
//! here, so every rule is a plain unit test.

use reqlite_format::{Method, Params, Request, VERSION};

/// The text of each field, as the user typed it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Draft {
    pub name: String,
    pub method: String,
    pub url: String,
    /// One `name: value` per line.
    pub headers: String,
    /// One `name: value` per line.
    pub query: String,
    pub body: String,
}

impl Draft {
    pub fn empty(name: &str) -> Draft {
        Draft {
            name: name.to_string(),
            method: "GET".to_string(),
            ..Draft::default()
        }
    }

    pub fn from_request(req: &Request) -> Draft {
        Draft {
            name: req.name.clone(),
            method: req.method.as_str().to_string(),
            url: req.url.clone(),
            headers: to_lines(&req.headers),
            query: to_lines(&req.query),
            body: req.body.clone().unwrap_or_default(),
        }
    }

    /// The request this draft describes, checked by the same rules as a file.
    /// An empty body means no body.
    pub fn to_request(&self) -> Result<Request, String> {
        let req = Request {
            version: VERSION,
            name: self.name.clone(),
            method: Method::try_from(self.method.trim().to_string())?,
            url: self.url.trim().to_string(),
            headers: from_lines(&self.headers, "headers")?,
            query: from_lines(&self.query, "query")?,
            body: (!self.body.is_empty()).then(|| self.body.clone()),
        };
        reqlite_format::validate(&req).map_err(|e| e.to_string())?;
        Ok(req)
    }

    /// True when saving would change the file. `saved` is the file's canonical
    /// text, or `None` when the file does not exist yet.
    pub fn differs_from(&self, saved: Option<&str>) -> bool {
        match (self.to_request(), saved) {
            (Ok(req), Some(saved)) => reqlite_format::to_string(&req).map_or(true, |t| t != saved),
            _ => true,
        }
    }
}

fn to_lines(params: &Params) -> String {
    params.pairs().map(|(k, v)| format!("{k}: {v}\n")).collect()
}

/// Parses `name: value` lines. Blank lines are skipped. A name may repeat.
pub fn from_lines(text: &str, field: &str) -> Result<Params, String> {
    let mut params = Params::default();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(format!("{field} line {}: write it as `name: value`", i + 1));
        };
        let name = name.trim();
        if name.is_empty() {
            return Err(format!("{field} line {}: the name is empty", i + 1));
        }
        params.append(name, value.trim_start());
    }
    Ok(params)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = r#"version = 1
name = "Create user"
method = "POST"
url = "{{base}}/users"
body = """
{"name": "ada"}
"""

[headers]
Authorization = "Bearer {{token}}"
X-Tag = ["a", "b"]

[query]
redirect = "http://x/y"
"#;

    #[test]
    fn a_file_comes_back_byte_for_byte_through_the_form() {
        let req = reqlite_format::parse(FILE).unwrap();
        let draft = Draft::from_request(&req);
        assert_eq!(
            draft.headers,
            "Authorization: Bearer {{token}}\nX-Tag: a\nX-Tag: b\n"
        );
        assert_eq!(draft.query, "redirect: http://x/y\n");
        assert_eq!(draft.to_request().unwrap(), req);
        assert_eq!(
            reqlite_format::to_string(&draft.to_request().unwrap()).unwrap(),
            FILE
        );
        assert!(!draft.differs_from(Some(FILE)));
    }

    #[test]
    fn any_edit_or_a_new_file_counts_as_a_change() {
        let mut draft = Draft::from_request(&reqlite_format::parse(FILE).unwrap());
        draft.body.push(' ');
        assert!(draft.differs_from(Some(FILE)));
        assert!(Draft::empty("new").differs_from(None));
    }

    #[test]
    fn lines_allow_blank_lines_repeats_and_colons_in_values() {
        let params = from_lines("\nA: 1\n\nA: 2\nurl: http://h:8080/\n", "query").unwrap();
        let pairs: Vec<_> = params.pairs().collect();
        assert_eq!(pairs, [("A", "1"), ("A", "2"), ("url", "http://h:8080/")]);
    }

    #[test]
    fn reports_which_line_is_wrong() {
        assert_eq!(
            from_lines("A: 1\nbroken\n", "headers").unwrap_err(),
            "headers line 2: write it as `name: value`"
        );
        assert_eq!(
            from_lines(": 1", "query").unwrap_err(),
            "query line 1: the name is empty"
        );
        let mut draft = Draft::empty("x");
        draft.url = "http://h".into();
        draft.headers = "Bad Name: 1".into();
        assert!(
            draft
                .to_request()
                .unwrap_err()
                .contains("invalid header name")
        );
        draft.headers.clear();
        draft.method = "GE T".into();
        assert!(
            draft
                .to_request()
                .unwrap_err()
                .contains("invalid HTTP method")
        );
    }

    #[test]
    fn an_empty_body_means_no_body() {
        let mut draft = Draft::empty("x");
        draft.url = "http://h".into();
        assert_eq!(draft.to_request().unwrap().body, None);
        draft.body = "\n".into();
        assert_eq!(draft.to_request().unwrap().body.as_deref(), Some("\n"));
    }
}
