//! Tells the window when files in the workspace change on disk. It uses the
//! OS's own change events (FSEvents, inotify, ReadDirectoryChangesW), so an
//! idle window wakes for nothing.

use super::Msg;
use iced::futures::{SinkExt, StreamExt, channel::mpsc};
use notify::{RecursiveMode, Watcher};
use std::path::{Path, PathBuf};

/// A stream of [`Msg::FsChanged`], one per batch of changes under `root`.
/// Changes under hidden folders such as `.git` are left out.
// `Subscription::run_with` hands its data over as `&PathBuf`.
#[allow(clippy::ptr_arg)]
pub fn watch(root: &PathBuf) -> impl iced::futures::Stream<Item = Msg> + use<> {
    let root = root.clone();
    iced::stream::channel(16, async move |mut out| {
        let (tx, mut rx) = mpsc::unbounded();
        let base = root.canonicalize().unwrap_or_else(|_| root.clone());
        let handler = move |event: notify::Result<notify::Event>| {
            if let Ok(e) = event {
                if e.paths.iter().any(|p| visible(&base, p)) {
                    // A send fails only when the window has closed: nothing to do.
                    if tx.unbounded_send(()).is_err() {}
                }
            }
        };
        let mut watcher = match notify::recommended_watcher(handler) {
            Ok(w) => w,
            Err(e) => return report(&mut out, e).await,
        };
        if let Err(e) = watcher.watch(&root, RecursiveMode::Recursive) {
            return report(&mut out, e).await;
        }
        while rx.next().await.is_some() {
            // One message for a burst: a save writes and renames in one go.
            while rx.try_recv().is_ok() {}
            if out.send(Msg::FsChanged).await.is_err() {
                return;
            }
        }
    })
}

fn visible(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root).map_or(true, |rest| {
        !rest
            .components()
            .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
    })
}

async fn report(out: &mut mpsc::Sender<Msg>, e: notify::Error) {
    // If the window has closed there is no one to tell.
    if out.send(Msg::WatchFailed(e.to_string())).await.is_err() {}
}
