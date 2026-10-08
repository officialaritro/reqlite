//! Secret values from the OS keychain: macOS Keychain, Windows Credential
//! Manager, or the Secret Service on Linux.
//!
//! Lookup order for a secret an environment declares:
//! 1. the environment's `.local.toml` file;
//! 2. the OS keychain.
//!
//! A keychain entry is named by service [`SERVICE`] and an account of the
//! environment file's full path and the secret's name. So `dev.toml` in two
//! projects never share a value, and moving a project means setting its
//! secrets again.

use reqlite_format::{EnvError, Environment};
use std::path::Path;

/// The keychain service every Reqlite secret is stored under.
pub const SERVICE: &str = "reqlite";

/// Where secrets are kept. [`Keychain`] is the real one; tests use their own.
pub trait Store {
    /// `Ok(None)` when there is no entry. `Err` when the store failed to
    /// answer, with the reason.
    fn get(&self, account: &str) -> Result<Option<String>, String>;
    fn set(&self, account: &str, value: &str) -> Result<(), String>;
    /// `Ok(false)` when there was no entry to delete.
    fn delete(&self, account: &str) -> Result<bool, String>;
}

/// The OS keychain.
pub struct Keychain;

impl Store for Keychain {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        match entry(account)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn set(&self, account: &str, value: &str) -> Result<(), String> {
        entry(account)?
            .set_password(value)
            .map_err(|e| e.to_string())
    }

    fn delete(&self, account: &str) -> Result<bool, String> {
        match entry(account)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// OAuth 2.0 tokens in the OS keychain, under the same service. The account is
/// the client's cache key (token URL, client ID and scope), so tokens persist
/// across sends and runs. A keychain that fails behaves as empty: the token is
/// fetched again, and not kept.
pub struct KeychainTokens;

impl reqlite_engine::oauth::TokenCache for KeychainTokens {
    fn get(&self, key: &str) -> Option<String> {
        Keychain.get(key).ok().flatten()
    }

    fn put(&self, key: &str, value: &str) {
        // Not keeping a token only means the next send fetches a new one.
        Keychain.set(key, value).ok();
    }
}

fn entry(account: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, account).map_err(|e| e.to_string())
}

/// The keychain account for secret `name` of the environment file `env`.
pub fn account(env: &Path, name: &str) -> std::io::Result<String> {
    Ok(format!("{}#{name}", env.canonicalize()?.display()))
}

/// Reads an environment file and its `.local.toml`, then fills each declared
/// secret that has no local value from `store`. A secret the store cannot
/// read is kept as unavailable, with the reason, and fails only a send that
/// uses it.
pub fn load_env(path: &Path, store: &impl Store) -> Result<Environment, EnvError> {
    let mut env = reqlite_format::load_env(path)?;
    for name in env.missing_secrets() {
        // `load_env` just read this file, so its path resolves.
        let found = account(path, &name)
            .map_err(|e| e.to_string())
            .and_then(|acct| store.get(&acct));
        env.supply(&name, found);
    }
    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqlite_format::Var;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    /// A keychain in memory. `fail` makes every read fail with that reason.
    #[derive(Default)]
    struct Memory {
        entries: RefCell<BTreeMap<String, String>>,
        fail: Option<String>,
    }

    impl Store for Memory {
        fn get(&self, account: &str) -> Result<Option<String>, String> {
            match &self.fail {
                Some(reason) => Err(reason.clone()),
                None => Ok(self.entries.borrow().get(account).cloned()),
            }
        }
        fn set(&self, account: &str, value: &str) -> Result<(), String> {
            self.entries
                .borrow_mut()
                .insert(account.into(), value.into());
            Ok(())
        }
        fn delete(&self, account: &str) -> Result<bool, String> {
            Ok(self.entries.borrow_mut().remove(account).is_some())
        }
    }

    fn env_dir(local: Option<&str>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("dev.toml"),
            "version = 1\nsecrets = ['token', 'pw']\n[vars]\nbase = 'http://a'\n",
        )
        .unwrap();
        if let Some(local) = local {
            std::fs::write(dir.path().join("dev.local.toml"), local).unwrap();
        }
        dir
    }

    #[test]
    fn the_account_is_the_full_env_path_and_the_name() {
        let dir = env_dir(None);
        let path = dir.path().join("dev.toml");
        let full = path.canonicalize().unwrap();
        assert_eq!(
            account(&path, "token").unwrap(),
            format!("{}#token", full.display())
        );
        let other = tempfile::tempdir().unwrap();
        std::fs::write(other.path().join("dev.toml"), "version = 1\n").unwrap();
        assert_ne!(
            account(&other.path().join("dev.toml"), "token").unwrap(),
            account(&path, "token").unwrap(),
            "two projects never share a value"
        );
    }

    #[test]
    fn the_keychain_fills_secrets_the_local_file_lacks() {
        let dir = env_dir(Some("version = 1\n[vars]\npw = 'local'\n"));
        let path = dir.path().join("dev.toml");
        let store = Memory::default();
        store
            .set(&account(&path, "token").unwrap(), "from keychain")
            .unwrap();
        store
            .set(&account(&path, "pw").unwrap(), "ignored")
            .unwrap();

        let env = load_env(&path, &store).unwrap();
        assert_eq!(env.get("token"), Some(&Var::Secret("from keychain".into())));
        assert_eq!(
            env.get("pw"),
            Some(&Var::Secret("local".into())),
            "the local file wins"
        );
        assert_eq!(env.get("base"), Some(&Var::Plain("http://a".into())));
    }

    #[test]
    fn no_entry_and_a_failing_keychain_stay_apart() {
        let dir = env_dir(None);
        let path = dir.path().join("dev.toml");
        let empty = load_env(&path, &Memory::default()).unwrap();
        assert_eq!(empty.get("token"), Some(&Var::MissingSecret));

        let locked = Memory {
            fail: Some("the keychain is locked".into()),
            ..Memory::default()
        };
        let env = load_env(&path, &locked).unwrap();
        assert_eq!(
            env.get("token"),
            Some(&Var::Unavailable("the keychain is locked".into()))
        );
    }

    #[test]
    fn an_env_with_every_secret_local_never_asks_the_keychain() {
        let dir = env_dir(Some("version = 1\n[vars]\ntoken = 't'\npw = 'p'\n"));
        let failing = Memory {
            fail: Some("must not be asked".into()),
            ..Memory::default()
        };
        let env = load_env(&dir.path().join("dev.toml"), &failing).unwrap();
        assert_eq!(env.get("token"), Some(&Var::Secret("t".into())));
    }

    #[test]
    fn a_bad_env_file_is_still_an_env_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = load_env(&dir.path().join("missing.toml"), &Memory::default()).unwrap_err();
        assert!(matches!(err, EnvError::Read { .. }), "{err:?}");
    }
}
