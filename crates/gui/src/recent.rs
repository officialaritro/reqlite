//! Remembers the last workspace, so the app started from Finder or the Dock,
//! which gets no path, opens it again. One line of text in the data folder,
//! next to `history.db`.

use std::path::{Path, PathBuf};

const FILE: &str = "last-workspace.txt";

/// The last workspace, if it still is a folder.
pub fn last_workspace(data_dir: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(data_dir.join(FILE)).ok()?;
    let path = PathBuf::from(text.trim_end_matches(['\n', '\r']));
    path.is_dir().then_some(path)
}

/// Records `workspace` as the last one. A failure only means the next start
/// opens no folder, so the caller may ignore it.
pub fn remember(data_dir: &Path, workspace: &Path) -> std::io::Result<()> {
    let full = workspace.canonicalize()?;
    std::fs::create_dir_all(data_dir)?;
    std::fs::write(data_dir.join(FILE), format!("{}\n", full.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_folder_comes_back_on_the_next_start() {
        let data = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        assert_eq!(last_workspace(data.path()), None, "nothing yet");
        remember(data.path(), ws.path()).unwrap();
        let back = last_workspace(data.path()).unwrap();
        assert_eq!(back, ws.path().canonicalize().unwrap());
    }

    #[test]
    fn a_relative_path_is_stored_in_full() {
        let data = tempfile::tempdir().unwrap();
        remember(data.path(), Path::new(".")).unwrap();
        let back = last_workspace(data.path()).unwrap();
        assert!(back.is_absolute(), "{back:?}");
    }

    #[test]
    fn a_folder_that_is_gone_is_forgotten() {
        let data = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        remember(data.path(), ws.path()).unwrap();
        drop(ws);
        assert_eq!(last_workspace(data.path()), None);
    }

    #[test]
    fn the_data_folder_is_made_when_missing() {
        let data = tempfile::tempdir().unwrap();
        let nested = data.path().join("a/b");
        let ws = tempfile::tempdir().unwrap();
        remember(&nested, ws.path()).unwrap();
        assert!(last_workspace(&nested).is_some());
    }
}
