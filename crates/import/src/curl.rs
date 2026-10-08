//! `curl` command lines, as copied from a browser or a terminal (POSIX shell quoting).

use crate::{Warning, is_literal_credential};
use reqlite_format::{Auth, Body, KeyIn, Method, Params, Part, Request};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CurlError {
    #[error("the command does not start with curl")]
    NotCurl,
    #[error("a {0} quote is not closed")]
    Unclosed(char),
    #[error("{0} needs a value")]
    MissingValue(String),
    #[error("the command has no URL")]
    NoUrl,
    #[error("the command has more than one URL: {0:?} and {1:?}")]
    TwoUrls(String, String),
    #[error("header {0:?} has no colon")]
    BadHeader(String),
    #[error("the imported request is invalid: {0}")]
    Invalid(String),
}

#[derive(Clone, Copy)]
enum Opt {
    Method,
    Header,
    Data(Data),
    Json,
    Get,
    Head,
    UserAgent,
    Referer,
    Cookie,
    User,
    /// `-F`, and `--form-string` (`literal`: `@` and `<` are plain text).
    Form {
        literal: bool,
    },
    Url,
    /// Changes only what curl prints, or matches what Reqlite already does.
    NoEffect,
    Unsupported {
        takes_value: bool,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Data {
    /// `-d`, `--data`, `--data-ascii`: `@file` reads a file, without newlines.
    MaybeFile,
    /// `--data-binary`: `@file` sends the file as it is.
    Binary,
    /// `--data-raw`: `@` is literal.
    Raw,
    UrlEncode,
}

const OPTIONS: &[(Option<char>, &str, Opt)] = &[
    (Some('X'), "--request", Opt::Method),
    (Some('H'), "--header", Opt::Header),
    (Some('d'), "--data", Opt::Data(Data::MaybeFile)),
    (None, "--data-ascii", Opt::Data(Data::MaybeFile)),
    (None, "--data-binary", Opt::Data(Data::Binary)),
    (None, "--data-raw", Opt::Data(Data::Raw)),
    (None, "--data-urlencode", Opt::Data(Data::UrlEncode)),
    (None, "--json", Opt::Json),
    (Some('G'), "--get", Opt::Get),
    (Some('I'), "--head", Opt::Head),
    (Some('A'), "--user-agent", Opt::UserAgent),
    (Some('e'), "--referer", Opt::Referer),
    (Some('b'), "--cookie", Opt::Cookie),
    (Some('u'), "--user", Opt::User),
    (None, "--url", Opt::Url),
    (Some('s'), "--silent", Opt::NoEffect),
    (Some('S'), "--show-error", Opt::NoEffect),
    (Some('v'), "--verbose", Opt::NoEffect),
    (Some('i'), "--include", Opt::NoEffect),
    (Some('L'), "--location", Opt::NoEffect),
    (Some('g'), "--globoff", Opt::NoEffect),
    (None, "--compressed", Opt::NoEffect),
    (
        Some('k'),
        "--insecure",
        Opt::Unsupported { takes_value: false },
    ),
    (
        Some('o'),
        "--output",
        Opt::Unsupported { takes_value: true },
    ),
    (
        Some('m'),
        "--max-time",
        Opt::Unsupported { takes_value: true },
    ),
    (
        None,
        "--connect-timeout",
        Opt::Unsupported { takes_value: true },
    ),
    (Some('F'), "--form", Opt::Form { literal: false }),
    (None, "--form-string", Opt::Form { literal: true }),
    (Some('x'), "--proxy", Opt::Unsupported { takes_value: true }),
    (
        Some('T'),
        "--upload-file",
        Opt::Unsupported { takes_value: true },
    ),
    (Some('E'), "--cert", Opt::Unsupported { takes_value: true }),
    (None, "--http1.1", Opt::Unsupported { takes_value: false }),
    (None, "--http2", Opt::Unsupported { takes_value: false }),
];

fn takes_value(opt: Opt) -> bool {
    !matches!(
        opt,
        Opt::Get | Opt::Head | Opt::NoEffect | Opt::Unsupported { takes_value: false }
    )
}

#[derive(Default)]
struct Parsed {
    method: Option<String>,
    url: Option<String>,
    headers: Vec<(String, String)>,
    data: Vec<String>,
    /// `--data-binary @file`.
    file_body: Option<String>,
    parts: Vec<Part>,
    /// `-u name:password`.
    user: Option<String>,
    json: bool,
    get: bool,
    head: bool,
    warnings: Vec<Warning>,
}

pub fn import(command: &str) -> Result<(Request, Vec<Warning>), CurlError> {
    let words = words(command)?;
    let mut words = words.into_iter();
    match words.next() {
        Some(w) if w == "curl" || w.ends_with("/curl") || w.eq_ignore_ascii_case("curl.exe") => {}
        _ => return Err(CurlError::NotCurl),
    }

    let mut p = Parsed::default();
    let mut positional_only = false;
    while let Some(word) = words.next() {
        if positional_only || !word.starts_with('-') || word == "-" {
            p.url_word(word)?;
        } else if word == "--" {
            positional_only = true;
        } else if word.starts_with("--") {
            let opt = OPTIONS
                .iter()
                .find(|(_, long, _)| *long == word)
                .map_or(Opt::Unsupported { takes_value: false }, |o| o.2);
            let value = if takes_value(opt) {
                Some(
                    words
                        .next()
                        .ok_or_else(|| CurlError::MissingValue(word.clone()))?,
                )
            } else {
                None
            };
            p.apply(opt, &word, value)?;
        } else {
            let cluster: Vec<char> = word.chars().skip(1).collect();
            let mut i = 0;
            while i < cluster.len() {
                let c = cluster[i];
                let flag = format!("-{c}");
                let opt = OPTIONS
                    .iter()
                    .find(|(short, _, _)| *short == Some(c))
                    .map_or(Opt::Unsupported { takes_value: false }, |o| o.2);
                if takes_value(opt) {
                    let attached: String = cluster[i + 1..].iter().collect();
                    let value = if attached.is_empty() {
                        words
                            .next()
                            .ok_or_else(|| CurlError::MissingValue(flag.clone()))?
                    } else {
                        attached
                    };
                    p.apply(opt, &flag, Some(value))?;
                    break;
                }
                p.apply(opt, &flag, None)?;
                i += 1;
            }
        }
    }
    p.finish()
}

impl Parsed {
    fn url_word(&mut self, word: String) -> Result<(), CurlError> {
        match self.url.take() {
            None => {
                self.url = Some(word);
                Ok(())
            }
            Some(first) => Err(CurlError::TwoUrls(first, word)),
        }
    }

    fn apply(&mut self, opt: Opt, flag: &str, value: Option<String>) -> Result<(), CurlError> {
        let need = || {
            value
                .clone()
                .ok_or_else(|| CurlError::MissingValue(flag.to_string()))
        };
        match opt {
            Opt::Method => self.method = Some(need()?),
            Opt::Header => {
                let h = need()?;
                if let Some(name) = h.strip_suffix(';') {
                    self.headers.push((name.trim().to_string(), String::new()));
                } else {
                    let (name, v) = h
                        .split_once(':')
                        .ok_or_else(|| CurlError::BadHeader(h.clone()))?;
                    let v = v.trim_start();
                    if v.is_empty() {
                        self.warnings.push(Warning::Unsupported {
                            option: format!("{flag} '{name}:' (removing a default header)"),
                            item: None,
                        });
                    } else {
                        self.headers.push((name.trim().to_string(), v.to_string()));
                    }
                }
            }
            Opt::Data(kind) => {
                let d = need()?;
                match kind {
                    Data::Binary if d.starts_with('@') && self.file_body.is_none() => {
                        self.file_body = Some(d[1..].to_string());
                    }
                    Data::MaybeFile | Data::Binary if d.starts_with('@') => {
                        self.warnings.push(Warning::BodyFromFile {
                            path: d[1..].to_string(),
                        });
                    }
                    Data::UrlEncode => match url_encode_arg(&d) {
                        Some(encoded) => self.data.push(encoded),
                        None => self.warnings.push(Warning::BodyFromFile { path: d }),
                    },
                    _ => self.data.push(d),
                }
            }
            Opt::Json => {
                self.data.push(need()?);
                self.json = true;
            }
            Opt::Get => self.get = true,
            Opt::Head => self.head = true,
            Opt::UserAgent => self.headers.push(("User-Agent".into(), need()?)),
            Opt::Referer => self.headers.push(("Referer".into(), need()?)),
            Opt::Cookie => {
                let c = need()?;
                if c.contains('=') {
                    self.headers.push(("Cookie".into(), c));
                } else {
                    self.warnings.push(Warning::Unsupported {
                        option: format!("{flag} {c} (a cookie file)"),
                        item: None,
                    });
                }
            }
            Opt::User => self.user = Some(need()?),
            Opt::Form { literal } => {
                let f = need()?;
                let (name, value) = f.split_once('=').ok_or_else(|| {
                    CurlError::Invalid(format!("{flag} {f}: write it as name=value"))
                })?;
                let part = match value.strip_prefix('@') {
                    Some(file) if !literal => {
                        let (path, ct) = match file.split_once(";type=") {
                            Some((p, t)) => (p, Some(t.to_string())),
                            None => (file, None),
                        };
                        Part {
                            name: name.into(),
                            text: None,
                            file: Some(path.into()),
                            content_type: ct,
                        }
                    }
                    _ if !literal && value.starts_with('<') => {
                        self.warnings.push(Warning::BodyFromFile {
                            path: value[1..].to_string(),
                        });
                        return Ok(());
                    }
                    _ => Part {
                        name: name.into(),
                        text: Some(value.into()),
                        file: None,
                        content_type: None,
                    },
                };
                self.parts.push(part);
            }
            Opt::Url => self.url_word(need()?)?,
            Opt::NoEffect => {}
            Opt::Unsupported { .. } => self.warnings.push(Warning::Unsupported {
                option: match value {
                    Some(v) => format!("{flag} {v}"),
                    None => flag.to_string(),
                },
                item: None,
            }),
        }
        Ok(())
    }

    fn finish(mut self) -> Result<(Request, Vec<Warning>), CurlError> {
        let mut url = self.url.take().ok_or(CurlError::NoUrl)?;
        let has = |headers: &[(String, String)], name: &str| {
            headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name))
        };
        let mut body = None;
        if !self.parts.is_empty() && (!self.data.is_empty() || self.file_body.is_some()) {
            return Err(CurlError::Invalid("-F cannot be mixed with -d".into()));
        }
        if !self.parts.is_empty() {
            body = Some(Body::Multipart(std::mem::take(&mut self.parts)));
        } else if let Some(path) = self.file_body.take() {
            if !self.data.is_empty() {
                return Err(CurlError::Invalid(
                    "--data-binary @file cannot be mixed with other -d data".into(),
                ));
            }
            body = Some(Body::File { path });
        } else if !self.data.is_empty() {
            let joined = self.data.join("&");
            if self.get {
                url.push(if url.contains('?') { '&' } else { '?' });
                url.push_str(&joined);
            } else {
                if self.json {
                    // A JSON body adds its own Content-Type when sent.
                    if !has(&self.headers, "Accept") {
                        self.headers
                            .push(("Accept".into(), "application/json".into()));
                    }
                    body = Some(if has(&self.headers, "Content-Type") {
                        Body::Text(joined)
                    } else {
                        Body::Json(joined)
                    });
                } else {
                    if !has(&self.headers, "Content-Type") {
                        self.headers.push((
                            "Content-Type".into(),
                            "application/x-www-form-urlencoded".into(),
                        ));
                    }
                    body = Some(Body::Text(joined));
                }
            }
        }
        let method = match (self.method, self.head, body.is_some()) {
            (Some(m), _, _) => m,
            (None, true, _) => "HEAD".into(),
            (None, false, true) => "POST".into(),
            (None, false, false) => "GET".into(),
        };
        let method = Method::try_from(method).map_err(CurlError::Invalid)?;

        let auth = match self.user.take() {
            None => None,
            Some(_) if has(&self.headers, "Authorization") => {
                self.warnings.push(Warning::Unsupported {
                    option: "-u, next to an Authorization header".into(),
                    item: None,
                });
                None
            }
            Some(user) => {
                let name = user.split_once(':').map_or(user.as_str(), |(n, _)| n);
                self.warnings.push(Warning::CredentialPlaceholder {
                    option: "-u".into(),
                    placeholder: "password".into(),
                });
                Some(Auth::Basic {
                    username: name.to_string(),
                    password: "{{password}}".into(),
                })
            }
        };
        let mut headers = Params::default();
        for (name, value) in self.headers {
            if is_literal_credential(&name, &value) {
                self.warnings.push(Warning::LiteralCredential {
                    header: name.clone(),
                });
            }
            headers.append(name, value);
        }
        let mut req = Request {
            version: 1,
            name: format!("{} {}", method.as_str(), path_of(&url)),
            method,
            url,
            headers,
            query: Params::default(),
            body,
            auth,
            assert: Vec::new(),
            capture: Default::default(),
        };
        req.version = reqlite_format::needed_version(&req);
        reqlite_format::validate(&req).map_err(|e| CurlError::Invalid(e.to_string()))?;
        Ok((req, self.warnings))
    }
}

/// `curl --data-urlencode` forms: `content`, `=content`, `name=content`.
/// `@file` and `name@file` read a file, which returns `None`.
fn url_encode_arg(arg: &str) -> Option<String> {
    if let Some(content) = arg.strip_prefix('=') {
        return Some(encode(content));
    }
    match (arg.find('='), arg.find('@')) {
        (Some(eq), Some(at)) if at < eq => None,
        (Some(eq), _) => Some(format!("{}={}", &arg[..eq], encode(&arg[eq + 1..]))),
        (None, Some(_)) => None,
        (None, None) => Some(encode(arg)),
    }
}

fn path_of(url: &str) -> &str {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path = after_scheme.find('/').map_or("/", |i| &after_scheme[i..]);
    path.split(['?', '#']).next().unwrap_or(path)
}

/// Splits a command line the way a POSIX shell does, for the quoting curl
/// commands use: `'...'`, `"..."`, `$'...'`, backslash escapes and `\` line ends.
fn words(command: &str) -> Result<Vec<String>, CurlError> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' | '\r' => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '\\' => match chars.next() {
                Some('\n') => {}
                Some('\r') if chars.peek() == Some(&'\n') => {
                    chars.next();
                }
                Some(x) => {
                    word.push(x);
                    in_word = true;
                }
                None => {}
            },
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(x) => word.push(x),
                        None => return Err(CurlError::Unclosed('\'')),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(x @ ('"' | '\\' | '$' | '`')) => word.push(x),
                            Some('\n') => {}
                            Some(x) => {
                                word.push('\\');
                                word.push(x);
                            }
                            None => return Err(CurlError::Unclosed('"')),
                        },
                        Some(x) => word.push(x),
                        None => return Err(CurlError::Unclosed('"')),
                    }
                }
            }
            '$' if chars.peek() == Some(&'\'') => {
                chars.next();
                in_word = true;
                ansi_c(&mut chars, &mut word)?;
            }
            x => {
                word.push(x);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(word);
    }
    Ok(words)
}

/// The body of a `$'...'` string, which browsers use for bodies with special characters.
fn ansi_c(
    chars: &mut std::iter::Peekable<std::str::Chars>,
    word: &mut String,
) -> Result<(), CurlError> {
    loop {
        match chars.next() {
            Some('\'') => return Ok(()),
            Some('\\') => match chars.next() {
                Some('n') => word.push('\n'),
                Some('t') => word.push('\t'),
                Some('r') => word.push('\r'),
                Some('0') => word.push('\0'),
                Some(x @ ('\\' | '\'' | '"' | '?')) => word.push(x),
                Some(kind @ ('x' | 'u' | 'U')) => {
                    let max = match kind {
                        'x' => 2,
                        'u' => 4,
                        _ => 8,
                    };
                    let mut hex = String::new();
                    while hex.len() < max && chars.peek().is_some_and(char::is_ascii_hexdigit) {
                        hex.extend(chars.next());
                    }
                    match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        Some(ch) => word.push(ch),
                        None => {
                            word.push('\\');
                            word.push(kind);
                            word.push_str(&hex);
                        }
                    }
                }
                Some(x) => {
                    word.push('\\');
                    word.push(x);
                }
                None => return Err(CurlError::Unclosed('\'')),
            },
            Some(x) => word.push(x),
            None => return Err(CurlError::Unclosed('\'')),
        }
    }
}

/// A curl command that sends `req`. `{{placeholders}}` stay as written.
pub fn export(req: &Request) -> String {
    let mut url = req.url.clone();
    let mut query: Vec<(&str, &str)> = req.query.pairs().collect();
    if let Some(Auth::ApiKey {
        name,
        value,
        location: KeyIn::Query,
    }) = &req.auth
    {
        query.push((name, value));
    }
    for (name, value) in query {
        url.push(if url.contains('?') { '&' } else { '?' });
        url.push_str(&encode(name));
        url.push('=');
        url.push_str(&encode(value));
    }
    let mut parts = vec!["curl".to_string()];
    if req.method.as_str() != "GET" {
        parts.push(format!("-X {}", req.method.as_str()));
    }
    parts.push(quote(&url));
    let header = |name: &str, value: &str| format!("-H {}", quote(&format!("{name}: {value}")));
    for (name, value) in req.headers.pairs() {
        parts.push(header(name, value));
    }
    match &req.auth {
        Some(Auth::Bearer { token }) => {
            parts.push(header("Authorization", &format!("Bearer {token}")))
        }
        Some(Auth::Basic { username, password }) => {
            parts.push(format!("-u {}", quote(&format!("{username}:{password}"))));
        }
        Some(Auth::ApiKey {
            name,
            value,
            location: KeyIn::Header,
        }) => parts.push(header(name, value)),
        // curl cannot sign in, so the token is left for the user to fill.
        Some(Auth::Oauth2(_)) => parts.push(header("Authorization", "Bearer {{oauth_token}}")),
        _ => {}
    }
    let has_type = req
        .headers
        .pairs()
        .any(|(n, _)| n.eq_ignore_ascii_case("Content-Type"));
    match &req.body {
        None => {}
        Some(Body::Text(text)) => parts.push(format!("--data-raw {}", quote(text))),
        Some(Body::Json(text)) if !has_type => parts.push(format!("--json {}", quote(text))),
        Some(Body::Json(text)) => parts.push(format!("--data-raw {}", quote(text))),
        Some(Body::Form(fields)) => {
            for (name, value) in fields.pairs() {
                parts.push(format!(
                    "--data-urlencode {}",
                    quote(&format!("{name}={value}"))
                ));
            }
        }
        Some(Body::Multipart(list)) => {
            for p in list {
                let value = match (&p.text, &p.file) {
                    (Some(text), _) => text.clone(),
                    (None, Some(file)) => match &p.content_type {
                        Some(ct) => format!("@{file};type={ct}"),
                        None => format!("@{file}"),
                    },
                    (None, None) => String::new(),
                };
                let flag = if p.text.is_some() {
                    "--form-string"
                } else {
                    "-F"
                };
                parts.push(format!("{flag} {}", quote(&format!("{}={value}", p.name))));
            }
        }
        Some(Body::File { path }) => {
            parts.push(format!("--data-binary {}", quote(&format!("@{path}"))))
        }
    }
    parts.join(" \\\n  ")
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Percent-encodes everything but unreserved characters. Braces stay, so
/// `{{placeholders}}` remain readable.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~{}".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn import_ok(cmd: &str) -> (Request, Vec<Warning>) {
        import(cmd).unwrap_or_else(|e| panic!("{cmd}: {e}"))
    }

    #[test]
    fn imports_a_browser_copy_as_curl_command() {
        let (req, warnings) = import_ok(
            r#"curl 'https://api.example.com/v1/users?page=2' \
  -H 'accept: application/json' \
  -H 'authorization: Bearer abc.def' \
  -H 'content-type: application/json' \
  -b 'session=xyz' \
  --data-raw $'{"name":"O\'Brien","note":"line\nbreak"}' \
  --compressed"#,
        );
        assert_eq!(req.method.as_str(), "POST");
        assert_eq!(req.url, "https://api.example.com/v1/users?page=2");
        assert_eq!(req.name, "POST /v1/users");
        assert_eq!(req.headers.get("accept"), ["application/json"]);
        assert_eq!(req.headers.get("Cookie"), ["session=xyz"]);
        assert_eq!(
            req.body,
            Some(Body::Text(
                "{\"name\":\"O'Brien\",\"note\":\"line\nbreak\"}".into()
            ))
        );
        assert_eq!(
            warnings,
            [
                Warning::LiteralCredential {
                    header: "authorization".into()
                },
                Warning::LiteralCredential {
                    header: "Cookie".into()
                },
            ]
        );
    }

    #[test]
    fn follows_curl_rules_for_method_body_and_content_type() {
        let (form, _) = import_ok("curl -d a=1 -d b=2 http://h/f");
        assert_eq!(form.method.as_str(), "POST");
        assert_eq!(form.body, Some(Body::Text("a=1&b=2".into())));
        assert_eq!(
            form.headers.get("Content-Type"),
            ["application/x-www-form-urlencoded"]
        );

        let (get, _) = import_ok("curl -G -d q=rust --data-urlencode 'n=a b&c' http://h/s?x=1");
        assert_eq!(get.method.as_str(), "GET");
        assert_eq!(get.url, "http://h/s?x=1&q=rust&n=a%20b%26c");
        assert_eq!(get.body, None);

        let (json, _) = import_ok(r#"curl --json '{"a":1}' http://h/j"#);
        assert_eq!(json.method.as_str(), "POST");
        assert_eq!(json.body, Some(Body::Json("{\"a\":1}".into())));
        assert!(
            json.headers.get("Content-Type").is_empty(),
            "added at send time"
        );
        assert_eq!(json.headers.get("Accept"), ["application/json"]);

        let (head, _) = import_ok("curl -sSLI http://h/");
        assert_eq!(head.method.as_str(), "HEAD");
        let (put, _) = import_ok("curl -XPUT --url http://h/p");
        assert_eq!(put.method.as_str(), "PUT");
    }

    #[test]
    fn warns_about_everything_it_does_not_import() {
        let (req, warnings) =
            import_ok("curl -k -u ada:pw -o out.json -d @body.json --frobnicate http://h/");
        assert_eq!(req.body, None);
        assert_eq!(
            req.auth,
            Some(Auth::Basic {
                username: "ada".into(),
                password: "{{password}}".into()
            }),
            "the password never reaches the file"
        );
        assert_eq!(
            warnings,
            [
                Warning::Unsupported {
                    option: "-k".into(),
                    item: None
                },
                Warning::Unsupported {
                    option: "-o out.json".into(),
                    item: None
                },
                Warning::BodyFromFile {
                    path: "body.json".into()
                },
                Warning::Unsupported {
                    option: "--frobnicate".into(),
                    item: None
                },
                Warning::CredentialPlaceholder {
                    option: "-u".into(),
                    placeholder: "password".into()
                },
            ]
        );
    }

    #[test]
    fn forms_and_binary_files_become_body_tables() {
        let (up, warnings) = import_ok(
            "curl -F note=hi -F 'avatar=@img/ada.png;type=image/png' --form-string 'raw=@kept' http://h/up",
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(up.method.as_str(), "POST");
        let part = |name: &str, text: Option<&str>, file: Option<&str>, ct: Option<&str>| Part {
            name: name.into(),
            text: text.map(Into::into),
            file: file.map(Into::into),
            content_type: ct.map(Into::into),
        };
        assert_eq!(
            up.body,
            Some(Body::Multipart(vec![
                part("note", Some("hi"), None, None),
                part("avatar", None, Some("img/ada.png"), Some("image/png")),
                part("raw", Some("@kept"), None, None),
            ]))
        );
        assert_eq!(up.version, 2);

        let (bin, warnings) = import_ok("curl --data-binary @payload.bin http://h/p");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            bin.body,
            Some(Body::File {
                path: "payload.bin".into()
            })
        );
    }

    #[test]
    fn a_form_cannot_be_mixed_with_data() {
        assert!(matches!(
            import("curl -F a=1 -d b=2 http://h/"),
            Err(CurlError::Invalid(_))
        ));
    }

    #[test]
    fn oauth2_exports_as_a_bearer_placeholder() {
        let req = reqlite_format::parse(
            "version = 2\nname = \"x\"\nmethod = \"GET\"\nurl = \"http://h/\"\n\n[auth]\ntype = \"oauth2\"\ngrant = \"device_code\"\ntoken_url = \"https://id/t\"\ndevice_url = \"https://id/d\"\nclient_id = \"app\"\n",
        )
        .unwrap();
        assert_eq!(
            export(&req),
            "curl \\\n  'http://h/' \\\n  -H 'Authorization: Bearer {{oauth_token}}'"
        );
    }

    #[test]
    fn every_body_type_and_auth_exports_and_imports_back() {
        for tail in [
            "\n[body]\ntype = \"json\"\ntext = '{\"a\": 1}'\n",
            "\n[body]\ntype = \"multipart\"\n\n[[body.parts]]\nname = \"note\"\ntext = \"hi\"\n\n[[body.parts]]\nname = \"f\"\nfile = \"a.png\"\ncontent_type = \"image/png\"\n",
            "\n[body]\ntype = \"file\"\npath = \"p.bin\"\n",
            "\n[auth]\ntype = \"basic\"\nusername = \"ada\"\npassword = \"{{password}}\"\n",
        ] {
            let text = format!(
                "version = 2\nname = \"x\"\nmethod = \"POST\"\nurl = \"http://h/x\"\n{tail}"
            );
            let req = reqlite_format::parse(&text).unwrap();
            let cmd = export(&req);
            let (back, _) = import(&cmd).unwrap_or_else(|e| panic!("{cmd}: {e}"));
            assert_eq!(back.body, req.body, "{cmd}");
            assert_eq!(back.auth, req.auth, "{cmd}");
        }
    }

    #[test]
    fn rejects_commands_it_cannot_read() {
        let cases = [
            ("wget http://h", CurlError::NotCurl),
            ("curl 'http://h", CurlError::Unclosed('\'')),
            ("curl -H", CurlError::MissingValue("-H".into())),
            ("curl -s", CurlError::NoUrl),
            (
                "curl http://a http://b",
                CurlError::TwoUrls("http://a".into(), "http://b".into()),
            ),
            (
                "curl -H nocolon http://h",
                CurlError::BadHeader("nocolon".into()),
            ),
        ];
        for (cmd, want) in cases {
            assert_eq!(import(cmd).unwrap_err(), want, "{cmd}");
        }
        assert!(matches!(
            import("curl -X 'BAD METHOD' http://h"),
            Err(CurlError::Invalid(_))
        ));
    }

    #[test]
    fn export_then_import_gives_the_same_request() {
        let text = r#"version = 1
name = "PATCH /users/{{id}}"
method = "PATCH"
url = "{{base}}/users/{{id}}"
body = """
{"note": "it's \"quoted\""}
"""

[headers]
Authorization = "Bearer {{token}}"
Content-Type = "application/json"
X-Tag = ["a", "b"]
"#;
        let req = reqlite_format::parse(text).unwrap();
        let (back, warnings) = import_ok(&export(&req));
        assert_eq!(back, req);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn export_puts_query_params_in_the_url() {
        let req = reqlite_format::parse(
            "version = 1\nname = 'q'\nurl = 'http://h/s'\n[query]\nq = 'a b'\ntag = ['x', 'y']\n",
        )
        .unwrap();
        assert_eq!(export(&req), "curl \\\n  'http://h/s?q=a%20b&tag=x&tag=y'");
    }
}
