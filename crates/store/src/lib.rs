//! Local request history in SQLite.
//!
//! History is the user's own data: a past response cannot be rebuilt from request
//! files. So this file is never dropped to "rebuild" anything. A damaged file is
//! moved aside, not deleted, and a fresh one takes its place.
//!
//! One thread owns the connection. Callers send commands over a channel and await
//! the reply, so no lock on the connection exists.

use reqlite_engine::Parts;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::oneshot;

const SCHEMA: i64 = 1;

/// Response bodies longer than this keep only their first bytes in history.
// SHORTCUT: fixed cap. Make it a setting, or keep large bodies as files next to
// the database, if users want full bodies of big responses in history.
pub const BODY_CAP: usize = 256 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("history database {path}")]
    Sqlite {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error("cannot move the damaged history file {path} aside")]
    MoveAside {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("history file {path} has schema {found}, newer than this build reads ({SCHEMA})")]
    NewerSchema { path: PathBuf, found: i64 },
    #[error("cannot use {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("the history thread has stopped")]
    Stopped,
}

/// What was sent, with secrets still as `{{name}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub query: Vec<(String, String)>,
    pub body: Option<String>,
}

impl From<&Parts> for SentRequest {
    fn from(p: &Parts) -> Self {
        SentRequest {
            method: p.method.as_str().to_string(),
            url: p.url.clone(),
            headers: p.headers.clone(),
            query: p.query.clone(),
            body: p.body.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Response {
        status: u16,
        headers: Vec<(String, Vec<u8>)>,
        /// At most [`BODY_CAP`] bytes.
        body: Vec<u8>,
        body_len: u64,
        elapsed_ms: u64,
    },
    Failed {
        error: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub file: Option<String>,
    pub env: Option<String>,
    pub request: SentRequest,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub id: i64,
    pub at_ms: i64,
    pub file: Option<String>,
    pub method: String,
    pub url: String,
    pub status: Option<u16>,
    pub error: Option<String>,
    pub elapsed_ms: Option<u64>,
}

enum Cmd {
    Record(Box<Entry>, oneshot::Sender<Result<i64, rusqlite::Error>>),
    Recent(
        usize,
        oneshot::Sender<Result<Vec<Summary>, rusqlite::Error>>,
    ),
}

/// A handle to the history thread. Clones share the thread. The thread ends
/// when the last handle is dropped.
#[derive(Clone)]
pub struct Store {
    tx: mpsc::Sender<Cmd>,
    path: PathBuf,
}

pub struct Opened {
    pub store: Store,
    /// Set when the old file was damaged. It was moved here and a fresh file created.
    pub moved_aside: Option<PathBuf>,
}

/// The default history file, `<data dir>/reqlite/history.db`.
pub fn default_path() -> Option<PathBuf> {
    std::env::var_os("REQLITE_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::data_dir().map(|d| d.join("reqlite")))
        .map(|d| d.join("history.db"))
}

pub fn open(path: &Path) -> Result<Opened, StoreError> {
    let sqlite = |source| StoreError::Sqlite {
        path: path.to_path_buf(),
        source,
    };
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|source| StoreError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
    }
    let (conn, moved_aside) = match prepare(path) {
        Ok(conn) => (conn, None),
        Err(Prepare::Newer(found)) => {
            return Err(StoreError::NewerSchema {
                path: path.to_path_buf(),
                found,
            });
        }
        Err(Prepare::Damaged(_)) => {
            let aside = move_aside(path)?;
            let conn = prepare(path).map_err(|e| match e {
                Prepare::Damaged(source) => sqlite(source),
                Prepare::Newer(found) => StoreError::NewerSchema {
                    path: path.to_path_buf(),
                    found,
                },
            })?;
            (conn, Some(aside))
        }
    };

    let (tx, rx) = mpsc::channel::<Cmd>();
    std::thread::Builder::new()
        .name("reqlite-history".into())
        .spawn(move || serve(conn, rx))
        .map_err(|source| StoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(Opened {
        store: Store {
            tx,
            path: path.to_path_buf(),
        },
        moved_aside,
    })
}

impl Store {
    pub async fn record(&self, entry: Entry) -> Result<i64, StoreError> {
        let (reply, rx) = oneshot::channel();
        self.call(Cmd::Record(Box::new(entry), reply), rx).await
    }

    /// Newest first.
    pub async fn recent(&self, limit: usize) -> Result<Vec<Summary>, StoreError> {
        let (reply, rx) = oneshot::channel();
        self.call(Cmd::Recent(limit, reply), rx).await
    }

    async fn call<T>(
        &self,
        cmd: Cmd,
        rx: oneshot::Receiver<Result<T, rusqlite::Error>>,
    ) -> Result<T, StoreError> {
        self.tx.send(cmd).map_err(|_closed| StoreError::Stopped)?;
        rx.await
            .map_err(|_closed| StoreError::Stopped)?
            .map_err(|source| StoreError::Sqlite {
                path: self.path.clone(),
                source,
            })
    }
}

enum Prepare {
    Damaged(rusqlite::Error),
    Newer(i64),
}

fn prepare(path: &Path) -> Result<Connection, Prepare> {
    let conn = Connection::open(path).map_err(Prepare::Damaged)?;
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(Prepare::Damaged)?;
    if version > SCHEMA {
        return Err(Prepare::Newer(version));
    }
    let check: String = conn
        .query_row("PRAGMA quick_check", [], |r| r.get(0))
        .map_err(Prepare::Damaged)?;
    if check != "ok" {
        return Err(Prepare::Damaged(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
            Some(check),
        )));
    }
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA busy_timeout = 5000;
         CREATE TABLE IF NOT EXISTS history (
             id INTEGER PRIMARY KEY,
             at_ms INTEGER NOT NULL,
             file TEXT,
             env TEXT,
             request TEXT NOT NULL,
             status INTEGER,
             response_headers BLOB,
             response_body BLOB,
             body_len INTEGER,
             elapsed_ms INTEGER,
             error TEXT
         );
         PRAGMA user_version = 1;",
    )
    .map_err(Prepare::Damaged)?;
    Ok(conn)
}

fn move_aside(path: &Path) -> Result<PathBuf, StoreError> {
    let stamp = now_ms();
    let mut aside = path.as_os_str().to_owned();
    aside.push(format!(".corrupt-{stamp}"));
    let aside = PathBuf::from(aside);
    std::fs::rename(path, &aside).map_err(|source| StoreError::MoveAside {
        path: path.to_path_buf(),
        source,
    })?;
    for suffix in ["-wal", "-shm"] {
        let mut side = path.as_os_str().to_owned();
        side.push(suffix);
        let mut side_aside = aside.as_os_str().to_owned();
        side_aside.push(suffix);
        match std::fs::rename(&side, &side_aside) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(StoreError::MoveAside {
                    path: PathBuf::from(side),
                    source,
                });
            }
        }
    }
    Ok(aside)
}

fn serve(conn: Connection, rx: mpsc::Receiver<Cmd>) {
    while let Ok(cmd) = rx.recv() {
        // A caller that stopped waiting has dropped its receiver. Nothing to tell it.
        match cmd {
            Cmd::Record(entry, reply) => drop(reply.send(insert(&conn, &entry))),
            Cmd::Recent(limit, reply) => drop(reply.send(recent(&conn, limit))),
        }
    }
}

fn insert(conn: &Connection, e: &Entry) -> Result<i64, rusqlite::Error> {
    let request = serde_json::to_string(&e.request)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
    let (status, headers, body, body_len, elapsed, error) = match &e.outcome {
        Outcome::Response {
            status,
            headers,
            body,
            body_len,
            elapsed_ms,
        } => (
            Some(i64::from(*status)),
            Some(wire_headers(headers)),
            Some(&body[..body.len().min(BODY_CAP)]),
            Some(to_i64(*body_len)),
            Some(to_i64(*elapsed_ms)),
            None,
        ),
        Outcome::Failed { error } => (None, None, None, None, None, Some(error.as_str())),
    };
    conn.execute(
        "INSERT INTO history (at_ms, file, env, request, status, response_headers,
             response_body, body_len, elapsed_ms, error)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            now_ms(),
            e.file,
            e.env,
            request,
            status,
            headers,
            body,
            body_len,
            elapsed,
            error
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn recent(conn: &Connection, limit: usize) -> Result<Vec<Summary>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT id, at_ms, file, request, status, error, elapsed_ms
         FROM history ORDER BY id DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![to_i64(limit as u64)], |r| {
        let request: String = r.get(3)?;
        let request: SentRequest = serde_json::from_str(&request).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(err))
        })?;
        let status: Option<i64> = r.get(4)?;
        let elapsed: Option<i64> = r.get(6)?;
        Ok(Summary {
            id: r.get(0)?,
            at_ms: r.get(1)?,
            file: r.get(2)?,
            method: request.method,
            url: request.url,
            status: status.and_then(|s| u16::try_from(s).ok()),
            error: r.get(5)?,
            elapsed_ms: elapsed.and_then(|e| u64::try_from(e).ok()),
        })
    })?;
    rows.collect()
}

/// Headers in HTTP wire form, `name: value\r\n`. Lossless for any value bytes.
fn wire_headers(headers: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, value) in headers {
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value);
        out.extend_from_slice(b"\r\n");
    }
    out
}

fn to_i64(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| to_i64(d.as_millis().try_into().unwrap_or(u64::MAX)))
}

/// The first [`BODY_CAP`] bytes of a body, the part history keeps.
pub fn body_prefix(reader: impl std::io::Read) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut reader.take(BODY_CAP as u64), &mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::OptionalExtension;

    fn entry(url: &str, outcome: Outcome) -> Entry {
        Entry {
            file: Some("users/list.toml".into()),
            env: Some("dev".into()),
            request: SentRequest {
                method: "GET".into(),
                url: url.into(),
                headers: vec![("Authorization".into(), "Bearer {{token}}".into())],
                query: vec![],
                body: None,
            },
            outcome,
        }
    }

    fn ok(status: u16) -> Outcome {
        Outcome::Response {
            status,
            headers: vec![("x-raw".into(), b"caf\xe9".to_vec())],
            body: b"{}".to_vec(),
            body_len: 2,
            elapsed_ms: 12,
        }
    }

    #[tokio::test]
    async fn records_and_lists_newest_first_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let opened = open(&path).unwrap();
        assert!(opened.moved_aside.is_none());
        opened
            .store
            .record(entry("http://a/1", ok(200)))
            .await
            .unwrap();
        let failed = Outcome::Failed {
            error: "cannot connect".into(),
        };
        opened
            .store
            .record(entry("http://a/2", failed))
            .await
            .unwrap();
        drop(opened);

        let store = open(&path).unwrap().store;
        let rows = store.recent(10).await.unwrap();
        let urls: Vec<_> = rows.iter().map(|r| r.url.as_str()).collect();
        assert_eq!(urls, ["http://a/2", "http://a/1"]);
        assert_eq!(rows[0].error.as_deref(), Some("cannot connect"));
        assert_eq!(rows[1].status, Some(200));
        assert_eq!(rows[1].elapsed_ms, Some(12));
        assert_eq!(store.recent(1).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn damaged_file_is_moved_aside_and_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        std::fs::write(&path, b"this is not a database, it is a notebook").unwrap();

        let opened = open(&path).unwrap();
        let aside = opened.moved_aside.clone().unwrap();
        assert_eq!(
            std::fs::read(&aside).unwrap(),
            b"this is not a database, it is a notebook"
        );
        opened
            .store
            .record(entry("http://a", ok(201)))
            .await
            .unwrap();
        assert_eq!(opened.store.recent(5).await.unwrap()[0].status, Some(201));
    }

    #[tokio::test]
    async fn newer_schema_is_refused_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        Connection::open(&path)
            .unwrap()
            .execute_batch("PRAGMA user_version = 99;")
            .unwrap();
        let before = std::fs::read(&path).unwrap();
        let err = open(&path).err().unwrap();
        assert!(
            matches!(err, StoreError::NewerSchema { found: 99, .. }),
            "{err}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn stores_only_the_capped_body_and_raw_header_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let store = open(&path).unwrap().store;
        let big = vec![b'x'; BODY_CAP * 3];
        let body = body_prefix(big.as_slice()).unwrap();
        let body_len = big.len() as u64;
        let outcome = Outcome::Response {
            status: 200,
            headers: vec![("x-raw".into(), b"caf\xe9".to_vec())],
            body,
            body_len,
            elapsed_ms: 1,
        };
        store.record(entry("http://a", outcome)).await.unwrap();
        drop(store);

        let conn = Connection::open(&path).unwrap();
        let (stored, len, headers): (Vec<u8>, i64, Vec<u8>) = conn
            .query_row(
                "SELECT response_body, body_len, response_headers FROM history",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(stored.len(), BODY_CAP);
        assert_eq!(len, (BODY_CAP * 3) as i64);
        assert_eq!(headers, b"x-raw: caf\xe9\r\n");
        let request: String = conn
            .query_row("SELECT request FROM history", [], |r| r.get(0))
            .optional()
            .unwrap()
            .unwrap();
        assert!(request.contains("Bearer {{token}}"), "{request}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn many_concurrent_writers_all_land_without_hanging() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(&dir.path().join("history.db")).unwrap().store;
        let tasks: Vec<_> = (0..100)
            .map(|i| {
                let store = store.clone();
                tokio::spawn(
                    async move { store.record(entry(&format!("http://a/{i}"), ok(200))).await },
                )
            })
            .collect();
        let all = async {
            for t in tasks {
                t.await.unwrap().unwrap();
            }
            store.recent(1000).await.unwrap()
        };
        let rows = tokio::time::timeout(std::time::Duration::from_secs(10), all)
            .await
            .expect("history writes hung");
        assert_eq!(rows.len(), 100);
    }
}
