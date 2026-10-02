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
    },
}

/// What failed decides the exit code.
enum Failure {
    /// A file is unreadable or invalid, or a placeholder cannot be filled. Exit 3.
    Input(Box<dyn Error>),
    /// The request did not complete. Exit 1.
    Send(reqlite_engine::SendError),
    /// Writing the response out failed. Exit 1.
    Output(std::io::Error),
}

impl Failure {
    fn input(e: impl Error + 'static) -> Self {
        Failure::Input(Box::new(e))
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Send { file, env } => send(&file, env.as_deref()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            let (code, err): (u8, &dyn Error) = match &failure {
                Failure::Input(e) => (3, e.as_ref()),
                Failure::Send(e) => (1, e),
                Failure::Output(e) => (1, e),
            };
            report(err);
            ExitCode::from(code)
        }
    }
}

fn report(err: &dyn Error) {
    let mut line = format!("error: {err}");
    let mut cause = err.source();
    while let Some(c) = cause {
        line.push_str(&format!(": {c}"));
        cause = c.source();
    }
    eprintln!("{line}");
}

#[derive(Debug, thiserror::Error)]
#[error("cannot read {path}")]
struct ReadError {
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

fn send(file: &Path, env: Option<&Path>) -> Result<(), Failure> {
    let text = std::fs::read_to_string(file).map_err(|source| {
        Failure::input(ReadError {
            path: file.to_path_buf(),
            source,
        })
    })?;
    let req = reqlite_format::parse(&text).map_err(|source| {
        Failure::input(InFile {
            path: file.to_path_buf(),
            source,
        })
    })?;
    let env = match env {
        Some(path) => reqlite_format::load_env(path).map_err(Failure::input)?,
        None => reqlite_format::Environment::default(),
    };
    let req = reqlite_engine::resolve(&req, &env).map_err(|source| {
        Failure::input(InFile {
            path: file.to_path_buf(),
            source,
        })
    })?;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(Failure::Output)?;
    let resp = rt.block_on(async {
        let client = reqlite_engine::client().map_err(reqlite_engine::SendError::from)?;
        reqlite_engine::send(&client, &req).await
    });
    let resp = resp.map_err(Failure::Send)?;

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
