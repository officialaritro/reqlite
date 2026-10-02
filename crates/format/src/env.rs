//! Environment files. `dev.toml` is shared and committed. `dev.local.toml` sits
//! next to it, is gitignored, and holds secret values and personal overrides.

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::VERSION;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SharedFile {
    version: u32,
    #[serde(default)]
    vars: BTreeMap<String, String>,
    #[serde(default)]
    secrets: BTreeSet<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalFile {
    version: u32,
    #[serde(default)]
    vars: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Var {
    Plain(String),
    Secret(String),
    /// Declared secret in the shared file, with no value in the local file.
    MissingSecret,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Environment {
    vars: BTreeMap<String, Var>,
}

impl Environment {
    pub fn get(&self, name: &str) -> Option<&Var> {
        self.vars.get(name)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EnvError {
    #[error("cannot read environment {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid environment {path}")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("environment {path} has version {found}, this build reads version {VERSION}")]
    UnsupportedVersion { path: PathBuf, found: u32 },
    #[error("invalid variable name {name:?} in {path}")]
    InvalidName { path: PathBuf, name: String },
    #[error(
        "secret {name:?} has a value in the shared file {path}; move it to the .local.toml file"
    )]
    SecretInSharedFile { path: PathBuf, name: String },
}

/// The local file for `envs/dev.toml` is `envs/dev.local.toml`.
pub fn local_path(shared: &Path) -> PathBuf {
    shared.with_extension("local.toml")
}

/// Reads a shared environment file and its local file, if one exists.
pub fn load_env(shared: &Path) -> Result<Environment, EnvError> {
    let read = |path: &Path| {
        std::fs::read_to_string(path).map_err(|source| EnvError::Read {
            path: path.to_path_buf(),
            source,
        })
    };
    let local = local_path(shared);
    let local_text = match std::fs::read_to_string(&local) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(source) => {
            return Err(EnvError::Read {
                path: local,
                source,
            });
        }
    };
    parse_env(shared, &read(shared)?, local_text.as_deref())
}

/// `path` names the shared file in errors. The local file reuses its directory.
pub fn parse_env(path: &Path, shared: &str, local: Option<&str>) -> Result<Environment, EnvError> {
    let shared: SharedFile = parse_file(path, shared)?;
    check_version(path, shared.version)?;
    let local = match local {
        Some(text) => {
            let local_path = local_path(path);
            let file: LocalFile = parse_file(&local_path, text)?;
            check_version(&local_path, file.version)?;
            file.vars
        }
        None => BTreeMap::new(),
    };

    let names = shared
        .vars
        .keys()
        .chain(&shared.secrets)
        .chain(local.keys());
    if let Some(name) = names.into_iter().find(|n| !is_var_name(n)) {
        return Err(EnvError::InvalidName {
            path: path.to_path_buf(),
            name: name.clone(),
        });
    }
    if let Some(name) = shared.secrets.iter().find(|n| shared.vars.contains_key(*n)) {
        return Err(EnvError::SecretInSharedFile {
            path: path.to_path_buf(),
            name: name.clone(),
        });
    }

    let mut vars: BTreeMap<String, Var> = shared
        .vars
        .into_iter()
        .map(|(k, v)| (k, Var::Plain(v)))
        .collect();
    for name in &shared.secrets {
        vars.insert(name.clone(), Var::MissingSecret);
    }
    for (name, value) in local {
        let var = if shared.secrets.contains(&name) {
            Var::Secret(value)
        } else {
            Var::Plain(value)
        };
        vars.insert(name, var);
    }
    Ok(Environment { vars })
}

fn parse_file<T: serde::de::DeserializeOwned>(path: &Path, text: &str) -> Result<T, EnvError> {
    toml::from_str(text).map_err(|source| EnvError::Toml {
        path: path.to_path_buf(),
        source,
    })
}

fn check_version(path: &Path, found: u32) -> Result<(), EnvError> {
    if found == VERSION {
        Ok(())
    } else {
        Err(EnvError::UnsupportedVersion {
            path: path.to_path_buf(),
            found,
        })
    }
}

/// Variable names allowed inside `{{...}}`.
pub fn is_var_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(shared: &str, local: Option<&str>) -> Result<Environment, EnvError> {
        parse_env(Path::new("envs/dev.toml"), shared, local)
    }

    #[test]
    fn local_file_supplies_secrets_and_overrides_vars() {
        let e = env(
            "version = 1\nsecrets = ['token']\n[vars]\nbase = 'http://a'\nuser = 'ada'\n",
            Some("version = 1\n[vars]\ntoken = 's3cret'\nuser = 'me'\n"),
        )
        .unwrap();
        assert_eq!(e.get("base"), Some(&Var::Plain("http://a".into())));
        assert_eq!(e.get("user"), Some(&Var::Plain("me".into())));
        assert_eq!(e.get("token"), Some(&Var::Secret("s3cret".into())));
    }

    #[test]
    fn secret_without_local_value_is_marked_missing() {
        let e = env("version = 1\nsecrets = ['token']\n", None).unwrap();
        assert_eq!(e.get("token"), Some(&Var::MissingSecret));
    }

    #[test]
    fn rejects_a_secret_value_in_the_shared_file() {
        let err = env(
            "version = 1\nsecrets = ['token']\n[vars]\ntoken = 'leak'\n",
            None,
        )
        .unwrap_err();
        assert!(matches!(err, EnvError::SecretInSharedFile { ref name, .. } if name == "token"));
    }

    #[test]
    fn rejects_bad_names_versions_and_fields() {
        let bad_name = env("version = 1\n[vars]\n'a b' = '1'\n", None).unwrap_err();
        assert!(
            matches!(bad_name, EnvError::InvalidName { .. }),
            "{bad_name}"
        );
        let version = env("version = 2\n", None).unwrap_err();
        assert!(matches!(
            version,
            EnvError::UnsupportedVersion { found: 2, .. }
        ));
        let local_secrets =
            env("version = 1\n", Some("version = 1\nsecrets = ['x']\n")).unwrap_err();
        assert!(
            local_secrets.to_string().contains("dev.local.toml"),
            "{local_secrets}"
        );
    }

    #[test]
    fn load_env_reads_the_local_file_next_to_the_shared_one() {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("dev.toml");
        std::fs::write(&shared, "version = 1\nsecrets = ['token']\n").unwrap();
        assert_eq!(
            load_env(&shared).unwrap().get("token"),
            Some(&Var::MissingSecret)
        );
        std::fs::write(
            dir.path().join("dev.local.toml"),
            "version = 1\n[vars]\ntoken = 't'\n",
        )
        .unwrap();
        assert_eq!(
            load_env(&shared).unwrap().get("token"),
            Some(&Var::Secret("t".into()))
        );
    }
}
