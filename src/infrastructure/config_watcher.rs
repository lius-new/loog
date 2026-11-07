//! Watches one file in Loog's configuration directory (`keymap.json`,
//! `settings.json`) and calls back once a burst of changes has settled.

use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use notify::{RecursiveMode, Watcher};

use crate::infrastructure::session_filesystem::config_path;

/// Editors often save in several steps; wait this long for them to settle.
const RELOAD_QUIET_WINDOW: Duration = Duration::from_millis(200);

enum WatchMessage {
    Changed,
    Stop,
}

/// Stops watching when dropped.
pub struct ConfigWatcher {
    _watcher: notify::RecommendedWatcher,
    sender: Sender<WatchMessage>,
    worker: Option<JoinHandle<()>>,
}

impl Drop for ConfigWatcher {
    fn drop(&mut self) {
        let _ = self.sender.send(WatchMessage::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Call `reload` on a background thread whenever `file_name` changes.
pub fn watch(file_name: &'static str, reload: impl Fn() + Send + 'static) -> Option<ConfigWatcher> {
    let path = config_path(file_name)?;
    let directory = path.parent()?.to_path_buf();
    std::fs::create_dir_all(&directory).ok()?;
    let (sender, receiver) = mpsc::channel();
    let event_sender = sender.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event
            && event
                .paths
                .iter()
                .any(|changed| is_named(changed, file_name))
        {
            let _ = event_sender.send(WatchMessage::Changed);
        }
    })
    .ok()?;
    watcher
        .watch(&directory, RecursiveMode::NonRecursive)
        .ok()?;
    let worker = thread::Builder::new()
        .name(format!("loog-watch-{file_name}"))
        .spawn(move || {
            while let Ok(WatchMessage::Changed) = receiver.recv() {
                loop {
                    match receiver.recv_timeout(RELOAD_QUIET_WINDOW) {
                        Ok(WatchMessage::Changed) => continue,
                        Ok(WatchMessage::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                        Err(RecvTimeoutError::Timeout) => break,
                    }
                }
                reload();
            }
        })
        .ok()?;
    Some(ConfigWatcher {
        _watcher: watcher,
        sender,
        worker: Some(worker),
    })
}

fn is_named(path: &Path, file_name: &str) -> bool {
    path.file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case(file_name))
}
