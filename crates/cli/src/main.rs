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
    /// History could not be read. Exit 1.
    Store(reqlite_store::StoreError),
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

fn send(file: &Path, env_path: Option<&Path>, timeout: Option<u64>) -> Result<(), Failure> {
    let req = read_request(file)?;
    let env = match env_path {
        Some(path) => reqlite_format::load_env(path).map_err(Failure::input)?,
        None => reqlite_format::Environment::default(),
    };
    let req = reqlite_engine::resolve(&req, &env).map_err(|source| {
        Failure::input(InFile {
            path: file.to_path_buf(),
            source,
        })
    })?;

    let limits = reqlite_engine::Timeouts {
        total: timeout.map(std::time::Duration::from_secs),
        ..reqlite_engine::Timeouts::default()
    };
    let rt = runtime()?;
    let history = open_history();
    let resp = rt.block_on(async {
        let client =
            reqlite_engine::client_with(limits).map_err(reqlite_engine::SendError::from)?;
        reqlite_engine::send(&client, &req).await
    });

    if let Some(store) = &history {
        let entry = reqlite_store::Entry::from_send(
            Some(file.display().to_string()),
            env_path.map(|p| p.display().to_string()),
            &req,
            resp.as_ref(),
        )
        .map_err(Failure::Output)?;
        if let Err(e) = rt.block_on(store.record(entry)) {
            eprintln!("warning: this send was not saved to history: {}", chain(&e));
        }
    }
    let resp = resp.map_err(|e| match e {
        reqlite_engine::SendError::Timeout(_) => Failure::Timeout(e, limits),
        e => Failure::Send(e),
    })?;

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
