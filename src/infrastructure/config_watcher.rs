//! Watches one file in Loog's configuration directory (`keymap.json`,
//! `settings.json`) and calls back once a burst of changes has settled.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use notify::{RecursiveMode, Watcher};

use crate::infrastructure::session_filesystem::config_path;

/// Editors often save in several steps; wait this long for them to settle.
const RELOAD_QUIET_WINDOW: Duration = Duration::from_millis(200);

enum WatchMessage {
    Changed(&'static str),
    Stop,
}

struct Entry {
    id: u64,
    file_name: &'static str,
    reload: Box<dyn Fn() + Send>,
}

/// One directory watcher and one worker thread serve every file watched in the directory.
struct Hub {
    directory: PathBuf,
    _watcher: notify::RecommendedWatcher,
    sender: Sender<WatchMessage>,
    worker: Mutex<Option<JoinHandle<()>>>,
    entries: Arc<Mutex<Vec<Entry>>>,
}

impl Drop for Hub {
    fn drop(&mut self) {
        let _ = self.sender.send(WatchMessage::Stop);
        if let Some(worker) = self.worker.lock().ok().and_then(|mut worker| worker.take()) {
            let _ = worker.join();
        }
    }
}

/// Stops watching when dropped.
pub struct ConfigWatcher {
    hub: Arc<Hub>,
    id: u64,
}

impl Drop for ConfigWatcher {
    fn drop(&mut self) {
        if let Ok(mut entries) = self.hub.entries.lock() {
            entries.retain(|entry| entry.id != self.id);
        }
    }
}

static HUBS: Mutex<Vec<Weak<Hub>>> = Mutex::new(Vec::new());
static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn hub_for(directory: &Path) -> Option<Arc<Hub>> {
    let mut hubs = HUBS.lock().ok()?;
    hubs.retain(|hub| hub.strong_count() > 0);
    if let Some(hub) = hubs
        .iter()
        .filter_map(Weak::upgrade)
        .find(|hub| hub.directory == directory)
    {
        return Some(hub);
    }
    let (sender, receiver) = mpsc::channel();
    let entries: Arc<Mutex<Vec<Entry>>> = Arc::new(Mutex::new(Vec::new()));
    let event_sender = sender.clone();
    let event_entries = Arc::clone(&entries);
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else {
            return;
        };
        let Ok(entries) = event_entries.lock() else {
            return;
        };
        for entry in entries.iter() {
            if event
                .paths
                .iter()
                .any(|changed| is_named(changed, entry.file_name))
            {
                let _ = event_sender.send(WatchMessage::Changed(entry.file_name));
            }
        }
    })
    .ok()?;
    watcher.watch(directory, RecursiveMode::NonRecursive).ok()?;
    let worker_entries = Arc::clone(&entries);
    let worker = thread::Builder::new()
        .name("loog-watch-config".into())
        .spawn(move || {
            while let Ok(first) = receiver.recv() {
                let mut changed = BTreeSet::new();
                match first {
                    WatchMessage::Changed(name) => changed.insert(name),
                    WatchMessage::Stop => return,
                };
                loop {
                    match receiver.recv_timeout(RELOAD_QUIET_WINDOW) {
                        Ok(WatchMessage::Changed(name)) => {
                            changed.insert(name);
                        }
                        Ok(WatchMessage::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                        Err(RecvTimeoutError::Timeout) => break,
                    }
                }
                let Ok(entries) = worker_entries.lock() else {
                    return;
                };
                for entry in entries
                    .iter()
                    .filter(|entry| changed.contains(entry.file_name))
                {
                    (entry.reload)();
                }
            }
        })
        .ok()?;
    let hub = Arc::new(Hub {
        directory: directory.to_path_buf(),
        _watcher: watcher,
        sender,
        worker: Mutex::new(Some(worker)),
        entries,
    });
    hubs.push(Arc::downgrade(&hub));
    Some(hub)
}

/// Calls `reload` on a background thread whenever `file_name` changes.
pub fn watch(file_name: &'static str, reload: impl Fn() + Send + 'static) -> Option<ConfigWatcher> {
    let path = config_path(file_name)?;
    let directory = path.parent()?.to_path_buf();
    std::fs::create_dir_all(&directory).ok()?;
    let hub = hub_for(&directory)?;
    let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    hub.entries.lock().ok()?.push(Entry {
        id,
        file_name,
        reload: Box::new(reload),
    });
    Some(ConfigWatcher { hub, id })
}

fn is_named(path: &Path, file_name: &str) -> bool {
    path.file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case(file_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn register(
        hub: &Arc<Hub>,
        file_name: &'static str,
        reload: impl Fn() + Send + 'static,
    ) -> ConfigWatcher {
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        hub.entries.lock().unwrap().push(Entry {
            id,
            file_name,
            reload: Box::new(reload),
        });
        ConfigWatcher {
            hub: Arc::clone(hub),
            id,
        }
    }

    #[test]
    fn one_watcher_serves_both_files_and_stops_when_dropped() {
        let directory =
            std::env::temp_dir().join(format!("loog-config-watch-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let hub = hub_for(&directory).expect("watcher");
        let again = hub_for(&directory).expect("watcher");
        assert!(Arc::ptr_eq(&hub, &again), "a directory is watched once");
        drop(again);

        let (keymap_tx, keymap_rx) = channel();
        let (settings_tx, settings_rx) = channel();
        let keymap = register(&hub, "keymap.json", move || {
            let _ = keymap_tx.send(());
        });
        let settings = register(&hub, "settings.json", move || {
            let _ = settings_tx.send(());
        });

        std::fs::write(directory.join("keymap.json"), "{}").unwrap();
        keymap_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("keymap.json change reaches its callback");
        assert!(
            settings_rx.try_recv().is_err(),
            "settings.json was not touched"
        );

        std::fs::write(directory.join("settings.json"), "{}").unwrap();
        settings_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("settings.json change reaches its callback");

        drop(keymap);
        std::fs::write(directory.join("keymap.json"), "{\"a\":1}").unwrap();
        assert!(
            keymap_rx.recv_timeout(Duration::from_millis(700)).is_err(),
            "a dropped watcher is not called"
        );
        drop(settings);
        drop(hub);
        let _ = std::fs::remove_dir_all(&directory);
    }
}
