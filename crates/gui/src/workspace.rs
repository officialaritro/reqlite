//! A workspace is a folder of request files. This module reads its tree and
//! its environments. No iced types, so every rule is a plain test.

use std::io;
use std::path::{Path, PathBuf};

/// Where environment files live, at the top of a workspace.
pub const ENVS: &str = "envs";

/// The scan stops after this many entries, so opening a huge folder by mistake
/// cannot stall the window.
pub const MAX_ENTRIES: usize = 5000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub name: String,
    pub path: PathBuf,
    pub kind: Kind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// Holds its children, folders first, each group by name.
    Folder(Vec<Node>),
    Request,
}

/// The folders and request files under `root`. Hidden entries, `envs/` at the
/// top and `*.local.toml` files are left out. Folders with no request files
/// in them are kept, so a new folder shows at once.
pub fn scan(root: &Path) -> io::Result<Vec<Node>> {
    let mut left = MAX_ENTRIES;
    read(root, true, &mut left)
}

fn read(dir: &Path, top: bool, left: &mut usize) -> io::Result<Vec<Node>> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<io::Result<_>>()?;
    entries.sort_by_key(|e| e.file_name().to_string_lossy().to_lowercase());
    let (mut folders, mut requests) = (Vec::new(), Vec::new());
    for entry in entries {
        if *left == 0 {
            break;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || (top && name == ENVS) {
            continue;
        }
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            *left -= 1;
            // A folder that cannot be read shows empty rather than hiding the rest.
            let children = read(&path, false, left).unwrap_or_default();
            folders.push(Node {
                name,
                path,
                kind: Kind::Folder(children),
            });
        } else if let Some(stem) = request_stem(&name) {
            *left -= 1;
            requests.push(Node {
                name: stem.to_string(),
                path,
                kind: Kind::Request,
            });
        }
    }
    folders.extend(requests);
    Ok(folders)
}

/// `get.toml` is the request `get`. `get.local.toml` holds secrets, not a request.
fn request_stem(file: &str) -> Option<&str> {
    file.strip_suffix(".toml")
        .filter(|stem| !stem.is_empty() && !stem.ends_with(".local"))
}

/// The environment files in `root/envs`, by name. Their `.local.toml` secret
/// files are left out.
pub fn environments(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root.join(ENVS)) else {
        return Vec::new();
    };
    let mut envs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter(|e| request_stem(&e.file_name().to_string_lossy()).is_some())
        .map(|e| e.path())
        .collect();
    envs.sort();
    envs
}

/// The file name for a new request or folder named `name`. Requests get a
/// `.toml` extension. The name must not leave the folder it is made in.
pub fn file_name(name: &str, request: bool) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("the name is empty".into());
    }
    if name.contains(['/', '\\']) || name.starts_with('.') {
        return Err("the name cannot contain / or \\, or start with a dot".into());
    }
    if !request {
        return Ok(name.to_string());
    }
    let file = if name.ends_with(".toml") {
        name.to_string()
    } else {
        format!("{name}.toml")
    };
    match request_stem(&file) {
        Some(_) => Ok(file),
        None => Err("a name ending in .local is kept for secret files".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for d in ["users", "users/admin", "empty", "envs", ".git", "target"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        for f in [
            "hello.toml",
            "Users list.toml",
            "users/get.toml",
            "users/admin/ban.toml",
            "users/notes.md",
            "envs/dev.toml",
            "envs/dev.local.toml",
            "secret.local.toml",
            ".hidden.toml",
            ".git/config.toml",
        ] {
            fs::write(root.join(f), "").unwrap();
        }
        dir
    }

    fn names(nodes: &[Node]) -> Vec<String> {
        nodes
            .iter()
            .map(|n| match &n.kind {
                Kind::Folder(c) => format!("{}/[{}]", n.name, names(c).join(",")),
                Kind::Request => n.name.clone(),
            })
            .collect()
    }

    #[test]
    fn the_tree_shows_folders_first_then_requests() {
        let dir = tree();
        let nodes = scan(dir.path()).unwrap();
        assert_eq!(
            names(&nodes),
            [
                "empty/[]",
                "target/[]",
                "users/[admin/[ban],get]",
                "hello",
                "Users list"
            ]
        );
        assert_eq!(nodes[3].path, dir.path().join("hello.toml"));
    }

    #[test]
    fn environments_are_the_shared_files_in_envs() {
        let dir = tree();
        assert_eq!(environments(dir.path()), [dir.path().join("envs/dev.toml")]);
        assert!(environments(&dir.path().join("users")).is_empty());
    }

    #[test]
    fn new_names_stay_in_their_folder() {
        assert_eq!(file_name(" Get user ", true).unwrap(), "Get user.toml");
        assert_eq!(file_name("admin", false).unwrap(), "admin");
        assert_eq!(file_name("ban.toml", true).unwrap(), "ban.toml");
        for bad in ["", "  ", "a/b", "a\\b", "..", ".hidden", "x.local.toml"] {
            assert!(file_name(bad, true).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_huge_folder_stops_at_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..MAX_ENTRIES + 10 {
            fs::write(dir.path().join(format!("{i}.toml")), "").unwrap();
        }
        assert_eq!(scan(dir.path()).unwrap().len(), MAX_ENTRIES);
    }
}
