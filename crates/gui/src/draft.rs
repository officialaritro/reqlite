//! The form being edited and how it maps to a request file. No iced types live
//! here, so every rule is a plain unit test.

use reqlite_format::{
    Assert, Auth, Body, ClientAuth, Grant, KeyIn, Method, OAuth2, Params, Part, Request, Source,
};

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
    pub body_kind: BodyKind,
    /// Text and JSON as typed. Form fields and multipart parts as `name: value`
    /// lines; a multipart file part is `name: @path`, or `name: @path;type=TYPE`.
    pub body: String,
    /// The file a File body sends, relative to the request file.
    pub body_file: String,
    pub auth: AuthDraft,
    /// One assertion per line, such as `status == 200`, and one capture per
    /// line as `capture NAME = json PATH`.
    pub tests: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BodyKind {
    #[default]
    Text,
    Json,
    Form,
    Multipart,
    File,
}

impl BodyKind {
    pub const ALL: [BodyKind; 5] = [
        BodyKind::Text,
        BodyKind::Json,
        BodyKind::Form,
        BodyKind::Multipart,
        BodyKind::File,
    ];
}

impl std::fmt::Display for BodyKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            BodyKind::Text => "Text",
            BodyKind::Json => "JSON",
            BodyKind::Form => "Form",
            BodyKind::Multipart => "Multipart",
            BodyKind::File => "File",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AuthDraft {
    pub kind: AuthKind,
    pub token: String,
    pub username: String,
    pub password: String,
    pub key_name: String,
    pub key_value: String,
    pub key_in: KeyIn,
    pub grant: Grant,
    pub token_url: String,
    pub auth_url: String,
    pub device_url: String,
    pub client_id: String,
    pub client_secret: String,
    pub scope: String,
    pub client_auth: ClientAuth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthKind {
    #[default]
    None,
    Bearer,
    Basic,
    ApiKey,
    OAuth2,
}

impl AuthKind {
    pub const ALL: [AuthKind; 5] = [
        AuthKind::None,
        AuthKind::Bearer,
        AuthKind::Basic,
        AuthKind::ApiKey,
        AuthKind::OAuth2,
    ];
}

impl std::fmt::Display for AuthKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            AuthKind::None => "No auth",
            AuthKind::Bearer => "Bearer token",
            AuthKind::Basic => "Basic",
            AuthKind::ApiKey => "API key",
            AuthKind::OAuth2 => "OAuth 2.0",
        })
    }
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
        let mut d = Draft {
            name: req.name.clone(),
            method: req.method.as_str().to_string(),
            url: req.url.clone(),
            headers: to_lines(&req.headers),
            query: to_lines(&req.query),
            ..Draft::default()
        };
        match &req.body {
            None => {}
            Some(Body::Text(t)) => d.body.clone_from(t),
            Some(Body::Json(t)) => (d.body_kind, d.body) = (BodyKind::Json, t.clone()),
            Some(Body::Form(fields)) => (d.body_kind, d.body) = (BodyKind::Form, to_lines(fields)),
            Some(Body::Multipart(parts)) => {
                d.body_kind = BodyKind::Multipart;
                d.body = parts.iter().map(part_line).collect();
            }
            Some(Body::File { path }) => {
                (d.body_kind, d.body_file) = (BodyKind::File, path.clone())
            }
        }
        for a in &req.assert {
            d.tests.push_str(&format!("{a}\n"));
        }
        for (name, source) in &req.capture {
            d.tests.push_str(&format!("capture {name} = {source}\n"));
        }
        if let Some(auth) = &req.auth {
            let a = &mut d.auth;
            match auth {
                Auth::Bearer { token } => (a.kind, a.token) = (AuthKind::Bearer, token.clone()),
                Auth::Basic { username, password } => {
                    (a.kind, a.username, a.password) =
                        (AuthKind::Basic, username.clone(), password.clone());
                }
                Auth::ApiKey {
                    name,
                    value,
                    location,
                } => {
                    (a.kind, a.key_name, a.key_value, a.key_in) =
                        (AuthKind::ApiKey, name.clone(), value.clone(), *location);
                }
                Auth::Oauth2(o) => {
                    let text = |v: &Option<String>| v.clone().unwrap_or_default();
                    a.kind = AuthKind::OAuth2;
                    (a.grant, a.client_auth) = (o.grant, o.client_auth);
                    (a.token_url, a.client_id) = (o.token_url.clone(), o.client_id.clone());
                    (a.auth_url, a.device_url) = (text(&o.auth_url), text(&o.device_url));
                    (a.client_secret, a.scope) = (text(&o.client_secret), text(&o.scope));
                }
            }
        }
        d
    }

    /// The request this draft describes, checked by the same rules as a file.
    /// An empty body means no body.
    pub fn to_request(&self) -> Result<Request, String> {
        let mut req = Request {
            version: 1,
            name: self.name.clone(),
            method: Method::try_from(self.method.trim().to_string())?,
            url: self.url.trim().to_string(),
            headers: from_lines(&self.headers, "headers")?,
            query: from_lines(&self.query, "query")?,
            body: self.body()?,
            auth: self.auth()?,
            assert: Vec::new(),
            capture: Default::default(),
        };
        self.tests(&mut req)?;
        req.version = reqlite_format::needed_version(&req);
        reqlite_format::validate(&req).map_err(|e| e.to_string())?;
        Ok(req)
    }

    fn tests(&self, req: &mut Request) -> Result<(), String> {
        for line in self.tests.lines().map(str::trim).filter(|l| !l.is_empty()) {
            match line.strip_prefix("capture ") {
                Some(rest) => {
                    let Some((name, source)) = rest.split_once('=') else {
                        return Err(format!("{line:?}: a capture is capture NAME = SOURCE"));
                    };
                    let source = Source::try_from(source.trim().to_string())?;
                    req.capture.insert(name.trim().to_string(), source);
                }
                None => req.assert.push(Assert::try_from(line.to_string())?),
            }
        }
        Ok(())
    }

    fn body(&self) -> Result<Option<Body>, String> {
        if self.body_kind == BodyKind::File {
            let path = self.body_file.trim();
            return Ok((!path.is_empty()).then(|| Body::File { path: path.into() }));
        }
        if self.body.is_empty() {
            return Ok(None);
        }
        Ok(Some(match self.body_kind {
            BodyKind::Text => Body::Text(self.body.clone()),
            BodyKind::Json => Body::Json(self.body.clone()),
            BodyKind::Form => Body::Form(from_lines(&self.body, "form")?),
            BodyKind::Multipart => Body::Multipart(parts(&self.body)?),
            BodyKind::File => return Ok(None),
        }))
    }

    fn auth(&self) -> Result<Option<Auth>, String> {
        let a = &self.auth;
        Ok(match a.kind {
            AuthKind::None => None,
            AuthKind::Bearer => Some(Auth::Bearer {
                token: a.token.trim().to_string(),
            }),
            AuthKind::Basic => Some(Auth::Basic {
                username: a.username.clone(),
                password: a.password.clone(),
            }),
            AuthKind::ApiKey => Some(Auth::ApiKey {
                name: a.key_name.trim().to_string(),
                value: a.key_value.clone(),
                location: a.key_in,
            }),
            AuthKind::OAuth2 => {
                let opt = |v: &str| Some(v.trim().to_string()).filter(|v| !v.is_empty());
                // Only the endpoints the grant uses, so a grant changed in the
                // form leaves no stale field in the file.
                let only = |grant: Grant, v: &str| opt(v).filter(|_| a.grant == grant);
                Some(Auth::Oauth2(OAuth2 {
                    grant: a.grant,
                    token_url: a.token_url.trim().to_string(),
                    auth_url: only(Grant::AuthorizationCode, &a.auth_url),
                    device_url: only(Grant::DeviceCode, &a.device_url),
                    client_id: a.client_id.trim().to_string(),
                    client_secret: opt(&a.client_secret),
                    scope: opt(&a.scope),
                    client_auth: a.client_auth,
                }))
            }
        })
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

fn part_line(p: &Part) -> String {
    match (&p.text, &p.file, &p.content_type) {
        (Some(t), _, _) => format!("{}: {t}\n", p.name),
        (None, Some(f), Some(ct)) => format!("{}: @{f};type={ct}\n", p.name),
        (None, Some(f), None) => format!("{}: @{f}\n", p.name),
        (None, None, _) => format!("{}:\n", p.name),
    }
}

/// Multipart lines, in order: `name: text`, `name: @path`, `name: @path;type=TYPE`.
fn parts(text: &str) -> Result<Vec<Part>, String> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(format!(
                "multipart line {}: write it as `name: value` or `name: @file`",
                i + 1
            ));
        };
        let (name, value) = (name.trim(), value.trim_start());
        if name.is_empty() {
            return Err(format!("multipart line {}: the name is empty", i + 1));
        }
        out.push(match value.strip_prefix('@') {
            Some(file) => {
                let (file, ct) = match file.split_once(";type=") {
                    Some((f, t)) => (f, Some(t.trim().to_string())),
                    None => (file, None),
                };
                Part {
                    name: name.into(),
                    text: None,
                    file: Some(file.trim().into()),
                    content_type: ct,
                }
            }
            None => Part {
                name: name.into(),
                text: Some(value.into()),
                file: None,
                content_type: None,
            },
        });
    }
    Ok(out)
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
        assert_eq!(
            draft.to_request().unwrap().body,
            Some(Body::Text("\n".into()))
        );
    }

    const V2: &str = "version = 2\nname = \"x\"\nmethod = \"POST\"\nurl = \"http://a\"\n";

    #[test]
    fn every_body_type_and_auth_comes_back_through_the_form() {
        for tail in [
            "\n[body]\ntype = \"json\"\ntext = '{\"a\": 1}'\n",
            "\n[body]\ntype = \"form\"\n\n[body.fields]\nname = \"ada\"\ntag = [\"a\", \"b\"]\n",
            "\n[body]\ntype = \"multipart\"\n\n[[body.parts]]\nname = \"z\"\ntext = \"first\"\n\n[[body.parts]]\nname = \"a\"\nfile = \"img/ada.png\"\ncontent_type = \"image/png\"\n",
            "\n[body]\ntype = \"file\"\npath = \"payload.bin\"\n",
            "\n[auth]\ntype = \"bearer\"\ntoken = \"{{token}}\"\n",
            "\n[auth]\ntype = \"basic\"\nusername = \"ada\"\npassword = \"{{pw}}\"\n",
            "\n[auth]\ntype = \"api_key\"\nname = \"key\"\nvalue = \"{{key}}\"\nin = \"query\"\n",
            "\n[auth]\ntype = \"oauth2\"\ngrant = \"client_credentials\"\ntoken_url = \"https://id/token\"\nclient_id = \"app\"\nclient_secret = \"{{cs}}\"\nscope = \"read\"\nclient_auth = \"body\"\n",
            "\n[auth]\ntype = \"oauth2\"\ngrant = \"authorization_code\"\ntoken_url = \"https://id/token\"\nauth_url = \"https://id/auth\"\nclient_id = \"app\"\n",
            "\n[auth]\ntype = \"oauth2\"\ngrant = \"device_code\"\ntoken_url = \"https://id/token\"\ndevice_url = \"https://id/device\"\nclient_id = \"app\"\n",
        ] {
            let file = format!("{V2}{tail}");
            let req = reqlite_format::parse(&file).unwrap();
            let draft = Draft::from_request(&req);
            assert_eq!(draft.to_request().unwrap(), req, "{tail}");
            assert!(!draft.differs_from(Some(&file)), "{tail}");
        }
    }

    #[test]
    fn multipart_lines_keep_their_order_and_file_types() {
        let mut d = Draft::empty("x");
        d.url = "http://h".into();
        d.body_kind = BodyKind::Multipart;
        d.body = "note: hi there\navatar: @img/a.png;type=image/png\n\nraw: @data.bin\n".into();
        let Some(Body::Multipart(parts)) = d.to_request().unwrap().body else {
            panic!()
        };
        let summary: Vec<_> = parts
            .iter()
            .map(|p| {
                (
                    p.name.as_str(),
                    p.text.as_deref(),
                    p.file.as_deref(),
                    p.content_type.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("note", Some("hi there"), None, None),
                ("avatar", None, Some("img/a.png"), Some("image/png")),
                ("raw", None, Some("data.bin"), None),
            ]
        );
        d.body = "note hi".into();
        assert_eq!(
            d.to_request().unwrap_err(),
            "multipart line 1: write it as `name: value` or `name: @file`"
        );
    }

    #[test]
    fn changing_the_body_type_keeps_what_was_typed() {
        let mut d = Draft::empty("x");
        d.url = "http://h".into();
        d.body = "{\"a\": 1}".into();
        assert_eq!(
            d.to_request().unwrap().body,
            Some(Body::Text("{\"a\": 1}".into()))
        );
        d.body_kind = BodyKind::Json;
        assert_eq!(
            d.to_request().unwrap().body,
            Some(Body::Json("{\"a\": 1}".into()))
        );
        d.body_kind = BodyKind::File;
        assert_eq!(d.to_request().unwrap().body, None, "no file chosen yet");
        assert_eq!(d.body, "{\"a\": 1}", "the text is still there");
    }

    #[test]
    fn tests_come_back_through_the_form_one_per_line() {
        let file = "version = 3\nname = \"x\"\nmethod = \"GET\"\nurl = \"http://a\"\nassert = [\"status == 200\", \"json $.id exists\"]\n\n[capture]\nid = \"json $.id\"\n";
        let req = reqlite_format::parse(file).unwrap();
        let draft = Draft::from_request(&req);
        assert_eq!(
            draft.tests,
            "status == 200\njson $.id exists\ncapture id = json $.id\n"
        );
        assert_eq!(draft.to_request().unwrap(), req);
        assert!(!draft.differs_from(Some(file)));

        let mut d = draft.clone();
        d.tests = "status is 200\n".into();
        assert!(d.to_request().unwrap_err().contains("is not an operator"));
        d.tests = "capture id json $.id\n".into();
        assert!(
            d.to_request()
                .unwrap_err()
                .contains("capture NAME = SOURCE")
        );
    }

    #[test]
    fn a_changed_grant_keeps_only_the_endpoints_it_uses() {
        let mut d = Draft::empty("x");
        d.url = "http://a".into();
        let a = &mut d.auth;
        (a.kind, a.grant) = (AuthKind::OAuth2, Grant::AuthorizationCode);
        (a.token_url, a.auth_url, a.client_id) =
            ("https://id/t".into(), "https://id/a".into(), "app".into());
        a.device_url = "https://id/d".into();
        let Some(Auth::Oauth2(o)) = d.to_request().unwrap().auth else {
            panic!()
        };
        assert_eq!(
            (o.auth_url.as_deref(), o.device_url),
            (Some("https://id/a"), None)
        );
        d.auth.grant = Grant::DeviceCode;
        let Some(Auth::Oauth2(o)) = d.to_request().unwrap().auth else {
            panic!()
        };
        assert_eq!(
            (o.auth_url, o.device_url.as_deref()),
            (None, Some("https://id/d"))
        );
    }

    #[test]
    fn auth_that_clashes_with_a_header_is_refused() {
        let mut d = Draft::empty("x");
        d.url = "http://h".into();
        d.headers = "Authorization: Bearer x\n".into();
        d.auth.kind = AuthKind::Bearer;
        d.auth.token = "{{token}}".into();
        assert!(
            d.to_request()
                .unwrap_err()
                .contains("set both in [headers] and by [auth]")
        );
    }
}
