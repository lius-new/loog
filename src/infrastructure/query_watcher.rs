//! Watches a folder tree for edits to query files and extension manifests and
//! calls back once a burst of changes has settled.
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use notify::{RecursiveMode, Watcher};

/// Editors often save in several steps; wait this long for them to settle.
const QUIET_WINDOW: Duration = Duration::from_millis(250);

enum Message {
    Changed,
    Stop,
}

/// Stops watching when dropped.
pub struct DirWatcher {
    _watcher: notify::RecommendedWatcher,
    sender: Sender<Message>,
    worker: Option<JoinHandle<()>>,
}

impl Drop for DirWatcher {
    fn drop(&mut self) {
        let _ = self.sender.send(Message::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Whether a change to `path` can alter the stacked queries: a query file, an
/// extension's manifest, or the record of what is installed and turned off.
pub fn relevant(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension == "scm")
        || path
            .file_name()
            .is_some_and(|name| name == "manifest.json" || name == "extensions.json")
}

/// Call `reload` on a background thread whenever a file under `directory` that
/// `relevant` accepts changes. The folder is created if it is missing.
pub fn watch(
    directory: &Path,
    relevant: fn(&Path) -> bool,
    reload: impl Fn() + Send + 'static,
) -> Option<DirWatcher> {
    std::fs::create_dir_all(directory).ok()?;
    let (sender, receiver) = mpsc::channel();
    let event_sender = sender.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event
            && event.paths.iter().any(|changed| relevant(changed))
        {
            let _ = event_sender.send(Message::Changed);
        }
    })
    .ok()?;
    watcher.watch(directory, RecursiveMode::Recursive).ok()?;
    let worker = thread::Builder::new()
        .name("loom-watch-queries".into())
        .spawn(move || {
            while let Ok(Message::Changed) = receiver.recv() {
                loop {
                    match receiver.recv_timeout(QUIET_WINDOW) {
                        Ok(Message::Changed) => continue,
                        Ok(Message::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                        Err(RecvTimeoutError::Timeout) => break,
                    }
                }
                reload();
            }
        })
        .ok()?;
    Some(DirWatcher {
        _watcher: watcher,
        sender,
        worker: Some(worker),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_query_files_and_extension_manifests_matter() {
        assert!(relevant(Path::new("queries/rust/highlights.scm")));
        assert!(relevant(Path::new("extensions/acme/1.0.0/manifest.json")));
        assert!(relevant(Path::new("extensions/extensions.json")));
        assert!(!relevant(Path::new("extensions/.cache/x.bin")));
        assert!(!relevant(Path::new("queries/rust/notes.txt")));
    }
}
