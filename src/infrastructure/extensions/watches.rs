//! Bounded recursive disk events; a slow guest receives an explicit rescan.
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use loog_extension_host::{FileChange, FileChangeKind};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};
#[derive(Clone, Debug)]
pub struct Spec {
    pub root: PathBuf,
    pub patterns: Vec<String>,
}
#[derive(Debug)]
pub struct Credit(Arc<AtomicBool>);
impl Drop for Credit {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
#[derive(Debug)]
pub enum WatchEvent {
    Ready {
        watch: u64,
    },
    Changed {
        watch: u64,
        changes: Vec<FileChange>,
        credit: Credit,
    },
    Failed {
        watch: u64,
        message: String,
    },
}
type Sink = Arc<dyn Fn(WatchEvent) + Send + Sync>;
pub struct Runner {
    sink: Sink,
    watches: Mutex<HashMap<u64, Arc<AtomicBool>>>,
}
impl Runner {
    pub fn new(sink: impl Fn(WatchEvent) + Send + Sync + 'static) -> Self {
        Self {
            sink: Arc::new(sink),
            watches: Mutex::default(),
        }
    }
    pub fn start(&self, watch: u64, spec: Spec) {
        let stop = Arc::new(AtomicBool::new(false));
        self.watches.lock().unwrap().insert(watch, stop.clone());
        let sink = self.sink.clone();
        let spawn = std::thread::Builder::new()
            .name("loog-extension-watch".into())
            .spawn({
                let sink = sink.clone();
                move || {
                    if let Err(message) = run(watch, spec, stop, &sink) {
                        sink(WatchEvent::Failed { watch, message });
                    }
                }
            });
        if let Err(error) = spawn {
            sink(WatchEvent::Failed {
                watch,
                message: error.to_string(),
            });
        }
    }
    pub fn stop(&self, watch: u64) {
        if let Some(stop) = self.watches.lock().unwrap().remove(&watch) {
            stop.store(true, Ordering::Release);
        }
    }
    pub fn cancel_all(&self) {
        for stop in self.watches.lock().unwrap().drain().map(|(_, stop)| stop) {
            stop.store(true, Ordering::Release);
        }
    }
}
impl Drop for Runner {
    fn drop(&mut self) {
        self.cancel_all();
    }
}
pub fn patterns(values: &[String]) -> Result<GlobSet, String> {
    let mut set = GlobSetBuilder::new();
    for value in values {
        if value.starts_with(['/', '\\']) || value.split('/').any(|s| s == "..") {
            return Err("watch globs must stay relative to the folder".into());
        }
        set.add(
            GlobBuilder::new(value)
                .case_insensitive(cfg!(windows))
                .literal_separator(true)
                .backslash_escape(false)
                .build()
                .map_err(|e| e.to_string())?,
        );
    }
    set.build().map_err(|e| e.to_string())
}
fn run(watch: u64, spec: Spec, stop: Arc<AtomicBool>, sink: &Sink) -> Result<(), String> {
    let root = spec.root.canonicalize().map_err(|e| e.to_string())?;
    let patterns = patterns(&spec.patterns)?;
    let (tx, rx) = mpsc::sync_channel(256);
    let overflow = Arc::new(AtomicBool::new(false));
    let callback = || {
        let tx = tx.clone();
        let overflow = overflow.clone();
        move |event: notify::Result<Event>| {
            if tx.try_send(event).is_err() {
                overflow.store(true, Ordering::Release);
            }
        }
    };
    let native = notify::recommended_watcher(callback());
    let mut backend: Box<dyn Watcher + Send> = match native {
        Ok(native) => Box::new(native),
        Err(_) => Box::new(
            notify::PollWatcher::new(
                callback(),
                notify::Config::default().with_poll_interval(Duration::from_secs(2)),
            )
            .map_err(|e| e.to_string())?,
        ),
    };
    backend
        .watch(&root, RecursiveMode::Recursive)
        .map_err(|e| e.to_string())?;
    sink(WatchEvent::Ready { watch });
    let pending = Arc::new(AtomicBool::new(false));
    let mut changes = BTreeMap::new();
    let mut rescan = false;
    while !stop.load(Ordering::Acquire) {
        let mut events = vec![];
        if let Ok(event) = rx.recv_timeout(Duration::from_millis(50)) {
            events.push(event);
        }
        events.extend(rx.try_iter().take(255));
        rescan |= overflow.swap(false, Ordering::AcqRel);
        for event in events {
            let Ok(event) = event else {
                rescan = true;
                continue;
            };
            if event.need_rescan() || matches!(event.kind, EventKind::Any | EventKind::Other) {
                rescan = true;
            }
            if matches!(event.kind, EventKind::Access(_)) {
                continue;
            }
            for path in event.paths.into_iter().take(4097) {
                let Ok(relative) = path.strip_prefix(&root) else {
                    continue;
                };
                let relative = relative.to_string_lossy().replace('\\', "/");
                if relative.is_empty() {
                    rescan = true;
                    continue;
                }
                if !patterns.is_match(&relative) {
                    continue;
                }
                if relative.len() > 4096 || changes.len() >= 4096 {
                    rescan = true;
                    changes.clear();
                    break;
                }
                let kind = match event.kind {
                    EventKind::Create(_) => FileChangeKind::Create,
                    EventKind::Remove(_) => FileChangeKind::Delete,
                    EventKind::Modify(notify::event::ModifyKind::Name(_)) => {
                        if path.exists() {
                            FileChangeKind::Create
                        } else {
                            FileChangeKind::Delete
                        }
                    }
                    EventKind::Modify(_) => FileChangeKind::Change,
                    _ => continue,
                };
                changes.insert(relative, kind);
            }
        }
        if (rescan || !changes.is_empty()) && !pending.swap(true, Ordering::AcqRel) {
            let mut batch = std::mem::take(&mut changes)
                .into_iter()
                .map(|(path, kind)| FileChange { path, kind })
                .collect::<Vec<_>>();
            if rescan {
                batch.clear();
                batch.push(FileChange {
                    path: String::new(),
                    kind: FileChangeKind::Rescan,
                });
                rescan = false;
            }
            sink(WatchEvent::Changed {
                watch,
                changes: batch,
                credit: Credit(pending.clone()),
            });
        }
    }
    Ok(())
}
