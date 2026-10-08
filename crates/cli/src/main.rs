use clap::{Parser, Subcommand};
use std::error::Error;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// A lean, local-first API client.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Send one request file and print the response body to stdout.
    Send {
        file: PathBuf,
        /// Environment file that fills `{{var}}` placeholders. A `.local.toml`
        /// file next to it supplies secrets.
        #[arg(long, short)]
        env: Option<PathBuf>,
        /// Stop when the whole send takes longer than SECS. Without it there is
        /// no total limit, but a send still stops after 10 s without a
        /// connection or 30 s without data.
        #[arg(long, value_name = "SECS", value_parser = clap::value_parser!(u64).range(1..))]
        timeout: Option<u64>,
    },
    /// Send every request file under DIR, in the order the app lists them,
    /// and check each response against its `assert` lines. Values from
    /// `[capture]` fill placeholders in the requests after it.
    Run {
        dir: PathBuf,
        /// Environment file that fills `{{var}}` placeholders.
        #[arg(long, short)]
        env: Option<PathBuf>,
        /// Stop a send that takes longer than SECS.
        #[arg(long, value_name = "SECS", value_parser = clap::value_parser!(u64).range(1..))]
        timeout: Option<u64>,
    },
    /// GraphQL helpers.
    Graphql {
        #[command(subcommand)]
        action: GraphqlAction,
    },
    /// List recent sends, newest first. Set REQLITE_DATA_DIR to move the history file.
    History {
        #[arg(long, short = 'n', default_value_t = 20)]
        limit: usize,
    },
    /// Turn another tool's request into a Reqlite request file.
    Import {
        #[command(subcommand)]
        from: ImportFrom,
    },
    /// Turn a request file into another tool's format, printed to stdout.
    Export {
        #[command(subcommand)]
        to: ExportTo,
    },
    /// Keep an environment's secret in the OS keychain, out of every file.
    Secret {
        #[command(subcommand)]
        action: SecretAction,
    },
}

#[derive(Subcommand)]
enum GraphqlAction {
    /// Ask the server of a request file for its schema, and print it as SDL.
    /// The URL, headers and auth come from the file.
    Schema {
        file: PathBuf,
        #[arg(long, short)]
        env: Option<PathBuf>,
        #[arg(long, value_name = "SECS", value_parser = clap::value_parser!(u64).range(1..))]
        timeout: Option<u64>,
    },
}

#[derive(Subcommand)]
enum SecretAction {
    /// Store a value. It is read from stdin, or typed without being shown.
    /// A value in the environment's .local.toml file still comes first.
    Set {
        /// The environment file that declares the secret.
        #[arg(long, short)]
        env: PathBuf,
        name: String,
    },
    /// Remove a stored value.
    Delete {
        #[arg(long, short)]
        env: PathBuf,
        name: String,
    },
}

#[derive(Subcommand)]
enum ImportFrom {
    /// A curl command line. Reads stdin when COMMAND is not given.
    Curl {
        command: Option<String>,
        /// Write the request file here instead of printing it.
        #[arg(long, short)]
        out: Option<PathBuf>,
        /// Replace OUT if it already exists.
        #[arg(long)]
        force: bool,
    },
    /// A Postman v2.1 collection file. Writes one request file per request,
    /// one directory per folder.
    Postman {
        collection: PathBuf,
        /// Directory to write into.
        #[arg(long, short)]
        out: PathBuf,
        /// Replace request files that already exist.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum ExportTo {
    /// A curl command line. Placeholders stay as `{{name}}`.
    Curl { file: PathBuf },
}

/// What failed decides the exit code.
enum Failure {
    /// A file is unreadable or invalid, or a placeholder cannot be filled. Exit 3.
    Input(Box<dyn Error>),
    /// The request did not complete. Exit 1.
    Send(reqlite_engine::SendError),
    /// The request passed one of these limits. Exit 1.
    Timeout(reqlite_engine::SendError, reqlite_engine::Timeouts),
    /// Writing the response out failed. Exit 1.
    Output(std::io::Error),
    /// The OS keychain failed to answer. Exit 1.
    Keychain(KeychainError),
    /// History could not be read. Exit 1.
    Store(reqlite_store::StoreError),
    /// Assertions failed, or a request in a run did not complete. Exit 4.
    Checks(String),
    /// The server answered, but not with what was asked for. Exit 1.
    Answer(String),
}

impl Failure {
    fn input(e: impl Error + 'static) -> Self {
        Failure::Input(Box::new(e))
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Send { file, env, timeout } => send(&file, env.as_deref(), timeout),
        Command::Run { dir, env, timeout } => run(&dir, env.as_deref(), timeout),
        Command::Graphql {
            action: GraphqlAction::Schema { file, env, timeout },
        } => graphql_schema(&file, env.as_deref(), timeout),
        Command::History { limit } => history(limit),
        Command::Import {
            from:
                ImportFrom::Curl {
                    command,
                    out,
                    force,
                },
        } => import_curl(command, out.as_deref(), force),
        Command::Import {
            from:
                ImportFrom::Postman {
                    collection,
                    out,
                    force,
                },
        } => import_postman(&collection, &out, force),
        Command::Export {
            to: ExportTo::Curl { file },
        } => export_curl(&file),
        Command::Secret {
            action: SecretAction::Set { env, name },
        } => read_secret(&name)
            .and_then(|value| secret_set(&env, &name, &value, &reqlite_secrets::Keychain)),
        Command::Secret {
            action: SecretAction::Delete { env, name },
        } => secret_delete(&env, &name, &reqlite_secrets::Keychain),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            let (code, err): (u8, &dyn Error) = match &failure {
                Failure::Input(e) => (3, e.as_ref()),
                Failure::Send(e) => (1, e),
                Failure::Timeout(e, limits) => {
                    let cause = e
                        .source()
                        .map(|c| format!(": {}", chain(c)))
                        .unwrap_or_default();
                    eprintln!("error: {e} ({limits}){cause}");
                    return ExitCode::from(1);
                }
                Failure::Output(e) => (1, e),
                Failure::Store(e) => (1, e),
                Failure::Keychain(e) => (1, e),
                Failure::Checks(what) => {
                    eprintln!("{what}");
                    return ExitCode::from(4);
                }
                Failure::Answer(what) => {
                    eprintln!("error: {what}");
                    return ExitCode::from(1);
                }
            };
            report(err);
            ExitCode::from(code)
        }
    }
}

fn report(err: &dyn Error) {
    eprintln!("error: {}", chain(err));
}

fn chain(err: &dyn Error) -> String {
    let mut line = err.to_string();
    let mut cause = err.source();
    while let Some(c) = cause {
        line.push_str(&format!(": {c}"));
        cause = c.source();
    }
    line
}

fn runtime() -> Result<tokio::runtime::Runtime, Failure> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(Failure::Output)
}

/// History is a convenience. When it cannot open, say so and keep sending.
fn open_history() -> Option<reqlite_store::Store> {
    let Some(path) = reqlite_store::default_path() else {
        eprintln!("warning: history is off: no data directory found; set REQLITE_DATA_DIR");
        return None;
    };
    match reqlite_store::open(&path) {
        Ok(opened) => {
            if let Some(aside) = opened.moved_aside {
                eprintln!(
                    "warning: the history file was damaged; it was moved to {} and a new one started",
                    aside.display()
                );
            }
            Some(opened.store)
        }
        Err(e) => {
            eprintln!("warning: history is off: {}", chain(&e));
            None
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("cannot read {path}")]
struct ReadError {
    path: PathBuf,
    #[source]
    source: std::io::Error,
}

#[derive(Debug, thiserror::Error)]
#[error("cannot create {path}")]
struct CreateDirError {
    path: PathBuf,
    #[source]
    source: std::io::Error,
}

#[derive(Debug, thiserror::Error)]
#[error("the OS keychain failed: {0}")]
struct KeychainError(String);

/// Tells the user how to finish an OAuth 2.0 sign-in. The browser opens only
/// when a person is at the terminal, so scripts and CI never start one.
fn sign_in_prompt(p: reqlite_engine::oauth::Prompt) {
    use std::io::IsTerminal;
    eprintln!("{p}");
    if std::io::stderr().is_terminal() && reqlite_engine::oauth::open_browser(p.url()).is_err() {
        eprintln!("(could not open a browser; open the address above yourself)");
    }
}

#[derive(Debug, thiserror::Error)]
enum SecretInput {
    #[error("{name} is not a secret in {path}; add it to `secrets` there first")]
    NotDeclared { path: PathBuf, name: String },
    #[error("the value is empty")]
    Empty,
    #[error("cannot read the value")]
    Read(#[source] std::io::Error),
}

/// Typed without being shown in a terminal, or read whole from a pipe. One
/// line ending is dropped, so `echo VALUE |` works.
fn read_secret(name: &str) -> Result<String, Failure> {
    use std::io::{IsTerminal, Read};
    let value = if std::io::stdin().is_terminal() {
        rpassword::prompt_password(format!("Value for {{{{{name}}}}}: "))
            .map_err(|e| Failure::input(SecretInput::Read(e)))?
    } else {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|e| Failure::input(SecretInput::Read(e)))?;
        let trimmed = text.strip_suffix('\n').unwrap_or(&text);
        trimmed.strip_suffix('\r').unwrap_or(trimmed).to_string()
    };
    Ok(value)
}

/// The keychain account for `name`, once the environment is known to declare it.
fn declared(env: &Path, name: &str) -> Result<String, Failure> {
    let parsed = reqlite_format::load_env(env).map_err(Failure::input)?;
    let declared = matches!(
        parsed.get(name),
        Some(reqlite_format::Var::Secret(_) | reqlite_format::Var::MissingSecret)
    );
    if !declared {
        return Err(Failure::input(SecretInput::NotDeclared {
            path: env.to_path_buf(),
            name: name.to_string(),
        }));
    }
    reqlite_secrets::account(env, name).map_err(|e| Failure::input(SecretInput::Read(e)))
}

fn secret_set(
    env: &Path,
    name: &str,
    value: &str,
    store: &impl reqlite_secrets::Store,
) -> Result<(), Failure> {
    if value.is_empty() {
        return Err(Failure::input(SecretInput::Empty));
    }
    let account = declared(env, name)?;
    store
        .set(&account, value)
        .map_err(|e| Failure::Keychain(KeychainError(e)))?;
    eprintln!(
        "stored {{{{{name}}}}} for {} in the OS keychain",
        env.display()
    );
    let local = reqlite_format::local_path(env);
    let shadowed = reqlite_format::load_env(env)
        .ok()
        .and_then(|e| e.get(name).cloned())
        .is_some_and(|v| matches!(v, reqlite_format::Var::Secret(_)));
    if shadowed {
        eprintln!(
            "note: {} also sets {name}, and that value comes first",
            local.display()
        );
    }
    Ok(())
}

fn secret_delete(
    env: &Path,
    name: &str,
    store: &impl reqlite_secrets::Store,
) -> Result<(), Failure> {
    let account = declared(env, name)?;
    let removed = store
        .delete(&account)
        .map_err(|e| Failure::Keychain(KeychainError(e)))?;
    if removed {
        eprintln!(
            "removed {{{{{name}}}}} for {} from the OS keychain",
            env.display()
        );
    } else {
        eprintln!(
            "the OS keychain held no {{{{{name}}}}} for {}",
            env.display()
        );
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
#[error("{path}")]
struct InFile<E: Error + 'static> {
    path: PathBuf,
    #[source]
    source: E,
}

fn read_request(file: &Path) -> Result<reqlite_format::Request, Failure> {
    let text = std::fs::read_to_string(file).map_err(|source| {
        Failure::input(ReadError {
            path: file.to_path_buf(),
            source,
        })
    })?;
    reqlite_format::parse(&text).map_err(|source| {
        Failure::input(InFile {
            path: file.to_path_buf(),
            source,
        })
    })
}

fn load_env(env_path: Option<&Path>) -> Result<reqlite_format::Environment, Failure> {
    match env_path {
        Some(path) => {
            reqlite_secrets::load_env(path, &reqlite_secrets::Keychain).map_err(Failure::input)
        }
        None => Ok(reqlite_format::Environment::default()),
    }
}

/// Reads a request file and fills its placeholders.
fn prepare(
    file: &Path,
    env: &reqlite_format::Environment,
) -> Result<reqlite_engine::Resolved, Failure> {
    let req = read_request(file)?;
    // Body files are relative to the request file.
    let dir = file.parent().unwrap_or(Path::new(""));
    reqlite_engine::resolve_in(&req, env, dir).map_err(|source| {
        Failure::input(InFile {
            path: file.to_path_buf(),
            source,
        })
    })
}

/// What every send needs, made once per command.
struct Sender {
    rt: tokio::runtime::Runtime,
    client: reqwest::Client,
    history: Option<reqlite_store::Store>,
    limits: reqlite_engine::Timeouts,
}

impl Sender {
    fn new(timeout: Option<u64>) -> Result<Sender, Failure> {
        let limits = reqlite_engine::Timeouts {
            total: timeout.map(std::time::Duration::from_secs),
            ..reqlite_engine::Timeouts::default()
        };
        let client = reqlite_engine::client_with(limits)
            .map_err(|e| Failure::Send(reqlite_engine::SendError::from(e)))?;
        Ok(Sender {
            rt: runtime()?,
            client,
            history: open_history(),
            limits,
        })
    }

    /// Sends `req` and records it in history.
    fn send(
        &self,
        file: &Path,
        env_path: Option<&Path>,
        req: &reqlite_engine::Resolved,
    ) -> Result<reqlite_engine::Response, Failure> {
        let auth = reqlite_engine::oauth::Authorizer {
            cache: &reqlite_secrets::KeychainTokens,
            prompt: &sign_in_prompt,
            wait: std::time::Duration::from_secs(300),
        };
        let resp = self
            .rt
            .block_on(reqlite_engine::send_with(&self.client, req, Some(&auth)));
        if let Some(store) = &self.history {
            let entry = reqlite_store::Entry::from_send(
                Some(file.display().to_string()),
                env_path.map(|p| p.display().to_string()),
                req,
                resp.as_ref(),
            )
            .map_err(Failure::Output)?;
            if let Err(e) = self.rt.block_on(store.record(entry)) {
                eprintln!("warning: this send was not saved to history: {}", chain(&e));
            }
        }
        let limits = self.limits;
        resp.map_err(|e| match e {
            reqlite_engine::SendError::Timeout(_) => Failure::Timeout(e, limits),
            // A missing body file is a problem with the input, like a bad request file.
            e @ reqlite_engine::SendError::BodyFile { .. } => Failure::input(e),
            e => Failure::Send(e),
        })
    }
}

/// One line per assertion and capture.
fn print_checks(
    out: &mut dyn Write,
    checked: &reqlite_engine::check::Checked,
) -> std::io::Result<()> {
    for o in &checked.outcomes {
        if o.pass {
            writeln!(out, "  pass  {}", o.text)?;
        } else {
            writeln!(out, "  FAIL  {}  ({})", o.text, o.detail)?;
        }
    }
    Ok(())
}

fn send(file: &Path, env_path: Option<&Path>, timeout: Option<u64>) -> Result<(), Failure> {
    let env = load_env(env_path)?;
    let req = prepare(file, &env)?;
    let resp = Sender::new(timeout)?.send(file, env_path, &req)?;
    print_response(&resp)?;
    let checked = reqlite_engine::check::check(&req, &resp);
    print_checks(&mut std::io::stderr().lock(), &checked).map_err(Failure::Output)?;
    match checked.failed() {
        0 => Ok(()),
        n => Err(Failure::Checks(format!(
            "{n} of {} checks failed",
            checked.outcomes.len()
        ))),
    }
}

fn run(dir: &Path, env_path: Option<&Path>, timeout: Option<u64>) -> Result<(), Failure> {
    let files = reqlite_format::workspace::requests(dir).map_err(|source| {
        Failure::input(ReadError {
            path: dir.to_path_buf(),
            source,
        })
    })?;
    let mut env = load_env(env_path)?;
    let sender = Sender::new(timeout)?;
    let mut out = std::io::stdout().lock();
    let (mut checks, mut failed, mut broken) = (0, 0, 0);
    for file in &files {
        let label = file.strip_prefix(dir).unwrap_or(file).display().to_string();
        let sent = prepare(file, &env).and_then(|req| {
            let resp = sender.send(file, env_path, &req)?;
            Ok((req, resp))
        });
        let (req, resp) = match sent {
            Ok(done) => done,
            Err(failure) => {
                broken += 1;
                let why = match &failure {
                    Failure::Input(e) => chain(e.as_ref()),
                    Failure::Send(e) => chain(e),
                    Failure::Timeout(e, limits) => format!("{} ({limits})", chain(e)),
                    Failure::Output(e) => chain(e),
                    _ => return Err(failure),
                };
                writeln!(out, "{label}  FAIL  {why}").map_err(Failure::Output)?;
                continue;
            }
        };
        let checked = reqlite_engine::check::check(&req, &resp);
        writeln!(
            out,
            "{label}  {} · {} ms",
            resp.status,
            resp.elapsed.as_millis()
        )
        .and_then(|()| print_checks(&mut out, &checked))
        .map_err(Failure::Output)?;
        checks += checked.outcomes.len();
        failed += checked.failed();
        for (name, value) in checked.captured {
            env.capture(&name, value);
        }
    }
    let summary = format!(
        "{} requests, {} did not complete; {checks} checks, {failed} failed",
        files.len(),
        broken
    );
    if failed + broken > 0 {
        return Err(Failure::Checks(summary));
    }
    writeln!(out, "{summary}").map_err(Failure::Output)
}

fn graphql_schema(
    file: &Path,
    env_path: Option<&Path>,
    timeout: Option<u64>,
) -> Result<(), Failure> {
    let env = load_env(env_path)?;
    let req = reqlite_engine::graphql::introspection(&read_request(file)?);
    let dir = file.parent().unwrap_or(Path::new(""));
    let req = reqlite_engine::resolve_in(&req, &env, dir).map_err(|source| {
        Failure::input(InFile {
            path: file.to_path_buf(),
            source,
        })
    })?;
    let resp = Sender::new(timeout)?.send(file, env_path, &req)?;
    let mut body = Vec::new();
    resp.body
        .reader()
        .and_then(|mut r| std::io::Read::read_to_end(&mut r, &mut body))
        .map_err(Failure::Output)?;
    let sdl = reqlite_engine::graphql::schema(&body)
        .map_err(|e| Failure::Answer(format!("{e} (status {})", resp.status)))?;
    let mut out = std::io::stdout().lock();
    out.write_all(sdl.as_bytes())
        .and_then(|()| out.flush())
        .map_err(Failure::Output)
}

fn print_response(resp: &reqlite_engine::Response) -> Result<(), Failure> {
    eprintln!(
        "{} · {} ms · {} bytes",
        resp.status,
        resp.elapsed.as_millis(),
        resp.body.len()
    );
    for (k, v) in &resp.headers {
        eprintln!("{k}: {}", String::from_utf8_lossy(v));
    }
    let mut out = std::io::stdout().lock();
    let mut body = resp.body.reader().map_err(Failure::Output)?;
    std::io::copy(&mut body, &mut out).map_err(Failure::Output)?;
    out.flush().map_err(Failure::Output)
}

fn history(limit: usize) -> Result<(), Failure> {
    let path = reqlite_store::default_path().ok_or_else(|| {
        Failure::Output(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no data directory found; set REQLITE_DATA_DIR",
        ))
    })?;
    let opened = reqlite_store::open(&path).map_err(Failure::Store)?;
    if let Some(aside) = &opened.moved_aside {
        eprintln!(
            "warning: the history file was damaged; it was moved to {} and a new one started",
            aside.display()
        );
    }
    let rows = runtime()?
        .block_on(opened.store.recent(limit))
        .map_err(Failure::Store)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let mut out = std::io::stdout().lock();
    for r in rows {
        let result = match (r.status, &r.error) {
            (Some(status), _) => status.to_string(),
            (None, Some(_)) => "failed".to_string(),
            (None, None) => "-".to_string(),
        };
        let took = r
            .elapsed_ms
            .map(|ms| format!(" {ms} ms"))
            .unwrap_or_default();
        writeln!(
            out,
            "{:>5}  {:>8}  {:<6}  {:<6} {}{took}",
            r.id,
            ago(now - r.at_ms),
            result,
            r.method,
            r.url
        )
        .map_err(Failure::Output)?;
    }
    Ok(())
}

fn ago(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    match s {
        0..60 => format!("{s}s ago"),
        60..3600 => format!("{}m ago", s / 60),
        3600..86400 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86400),
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{0} already exists; pass --force to replace it")]
struct Exists(String);

fn import_curl(command: Option<String>, out: Option<&Path>, force: bool) -> Result<(), Failure> {
    let command = match command {
        Some(c) => c,
        None => std::io::read_to_string(std::io::stdin()).map_err(|source| {
            Failure::input(ReadError {
                path: PathBuf::from("stdin"),
                source,
            })
        })?,
    };
    let (req, warnings) = reqlite_import::curl::import(&command).map_err(Failure::input)?;
    for w in &warnings {
        eprintln!("warning: {w}");
    }
    match out {
        Some(path) => {
            if path.exists() && !force {
                return Err(Failure::input(Exists(path.display().to_string())));
            }
            reqlite_format::save(path, &req).map_err(Failure::input)?;
            eprintln!("wrote {}", path.display());
            Ok(())
        }
        None => {
            let text = reqlite_format::to_string(&req).map_err(Failure::input)?;
            let mut out = std::io::stdout().lock();
            out.write_all(text.as_bytes()).map_err(Failure::Output)
        }
    }
}

fn export_curl(file: &Path) -> Result<(), Failure> {
    let req = read_request(file)?;
    let mut out = std::io::stdout().lock();
    writeln!(out, "{}", reqlite_import::curl::export(&req)).map_err(Failure::Output)
}

fn import_postman(collection: &Path, out: &Path, force: bool) -> Result<(), Failure> {
    let json = std::fs::read_to_string(collection).map_err(|source| {
        Failure::input(ReadError {
            path: collection.to_path_buf(),
            source,
        })
    })?;
    let imported = reqlite_import::postman::import(&json).map_err(|source| {
        Failure::input(InFile {
            path: collection.to_path_buf(),
            source,
        })
    })?;
    for w in &imported.warnings {
        eprintln!("warning: {w}");
    }
    let targets: Vec<(PathBuf, &reqlite_format::Request)> = imported
        .files
        .iter()
        .map(|(rel, req)| (out.join(rel), req))
        .collect();
    if !force {
        if let Some((path, _)) = targets.iter().find(|(p, _)| p.exists()) {
            return Err(Failure::input(Exists(path.display().to_string())));
        }
    }
    for (path, req) in &targets {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|source| {
                Failure::input(CreateDirError {
                    path: dir.to_path_buf(),
                    source,
                })
            })?;
        }
        reqlite_format::save(path, req).map_err(Failure::input)?;
    }
    eprintln!(
        "wrote {} requests from {:?} to {}",
        targets.len(),
        imported.name,
        out.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Memory(RefCell<BTreeMap<String, String>>);

    impl reqlite_secrets::Store for Memory {
        fn get(&self, account: &str) -> Result<Option<String>, String> {
            Ok(self.0.borrow().get(account).cloned())
        }
        fn set(&self, account: &str, value: &str) -> Result<(), String> {
            self.0.borrow_mut().insert(account.into(), value.into());
            Ok(())
        }
        fn delete(&self, account: &str) -> Result<bool, String> {
            Ok(self.0.borrow_mut().remove(account).is_some())
        }
    }

    struct Broken;

    impl reqlite_secrets::Store for Broken {
        fn get(&self, _: &str) -> Result<Option<String>, String> {
            Err("locked".into())
        }
        fn set(&self, _: &str, _: &str) -> Result<(), String> {
            Err("locked".into())
        }
        fn delete(&self, _: &str) -> Result<bool, String> {
            Err("locked".into())
        }
    }

    fn env() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dev.toml");
        std::fs::write(
            &path,
            "version = 1\nsecrets = ['token']\n[vars]\nbase = 'x'\n",
        )
        .unwrap();
        (dir, path)
    }

    fn code(f: &Failure) -> u8 {
        match f {
            Failure::Input(_) => 3,
            Failure::Keychain(_) => 1,
            _ => 0,
        }
    }

    #[test]
    fn a_stored_secret_is_used_by_the_next_send_and_can_be_removed() {
        let (_dir, path) = env();
        let store = Memory::default();
        assert!(secret_set(&path, "token", "s3cret", &store).is_ok());
        let loaded = reqlite_secrets::load_env(&path, &store).unwrap();
        assert_eq!(
            loaded.get("token"),
            Some(&reqlite_format::Var::Secret("s3cret".into()))
        );
        assert!(secret_delete(&path, "token", &store).is_ok());
        let loaded = reqlite_secrets::load_env(&path, &store).unwrap();
        assert_eq!(
            loaded.get("token"),
            Some(&reqlite_format::Var::MissingSecret)
        );
        assert!(
            secret_delete(&path, "token", &store).is_ok(),
            "deleting again is not an error"
        );
    }

    #[test]
    fn only_a_declared_secret_with_a_value_is_stored() {
        let (_dir, path) = env();
        let store = Memory::default();
        for (name, value) in [("base", "v"), ("nope", "v"), ("token", "")] {
            let err = secret_set(&path, name, value, &store).unwrap_err();
            assert_eq!(code(&err), 3, "{name}");
        }
        assert!(store.0.borrow().is_empty(), "nothing was stored");
    }

    #[test]
    fn a_failing_keychain_is_its_own_error() {
        let (_dir, path) = env();
        let err = secret_set(&path, "token", "v", &Broken).unwrap_err();
        assert_eq!(code(&err), 1);
        let Failure::Keychain(e) = err else { panic!() };
        assert_eq!(e.to_string(), "the OS keychain failed: locked");
    }
}
