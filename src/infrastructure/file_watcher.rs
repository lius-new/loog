//! Recursive workspace file watching with quiet-window event batching.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use notify::event::ModifyKind;
use notify::{Config, Event, EventKind, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};

const QUIET_WINDOW: Duration = Duration::from_millis(150);
const MAX_BATCH_LATENCY: Duration = Duration::from_secs(1);
const POLL_INTERVAL: Duration = Duration::from_secs(2);
pub(crate) const MAX_BATCH_PATHS: usize = 4_096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileChangeKind {
    Create,
    Modify,
    Remove,
    Rescan,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileChange {
    pub path: PathBuf,
    pub kind: FileChangeKind,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FileChangeBatch {
    pub changes: Vec<FileChange>,
    pub requires_full_rescan: bool,
}

impl FileChangeBatch {
    pub(crate) fn paths(&self) -> impl Iterator<Item = &Path> {
        self.changes.iter().map(|change| change.path.as_path())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WatchBackendKind {
    Native,
    Polling,
}

pub(crate) struct FileWatchHandle {
    lifetime: Arc<AtomicBool>,
    stop: Sender<()>,
    worker: Option<JoinHandle<()>>,
    pub backend: WatchBackendKind,
}

impl Drop for FileWatchHandle {
    fn drop(&mut self) {
        self.lifetime.store(true, Ordering::Release);
        let _ = self.stop.send(());
        // A batch may be reading a slow disk. Never join it on the UI thread;
        // delayed notifications are validated before they are applied.
        self.worker.take();
    }
}

pub(crate) struct AsyncFileWatchHandle {
    stop: Sender<()>,
    stopped: Arc<AtomicBool>,
}

impl Drop for AsyncFileWatchHandle {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        let _ = self.stop.send(());
    }
}

/// Recursive watcher registration and polling fallback may traverse a large
/// tree. Set them up off the event loop too, not just their event callbacks.
pub(crate) fn start_async(
    roots: Vec<PathBuf>,
    on_batch: impl Fn(FileChangeBatch) + Send + 'static,
    on_error: impl Fn(String) + Send + 'static,
) -> std::io::Result<AsyncFileWatchHandle> {
    start_async_scoped(
        roots,
        move |batch, _| on_batch(batch),
        move |error, _| on_error(error),
    )
}
/// Recursive watcher registration and polling fallback may traverse a large
/// tree. Set them up off the event loop too, not just their event callbacks.
pub(crate) fn start_async_scoped(
    roots: Vec<PathBuf>,
    on_batch: impl Fn(FileChangeBatch, Lease) + Send + 'static,
    on_error: impl Fn(String, Lease) + Send + 'static,
) -> std::io::Result<AsyncFileWatchHandle> {
    let (stop, receiver) = mpsc::channel();
    let stopped = Arc::new(AtomicBool::new(false));
    let worker_stopped = stopped.clone();
    thread::Builder::new()
        .name("loog-watch-setup".into())
        .spawn(move || {
            let batch_stopped = worker_stopped.clone();
            match start(roots, move |batch| {
                if !batch_stopped.load(Ordering::Acquire) {
                    on_batch(
                        batch,
                        Lease {
                            stopped: Arc::downgrade(&batch_stopped),
                        },
                    );
                }
            }) {
                Ok(handle) => {
                    if !worker_stopped.load(Ordering::Acquire) {
                        let _ = receiver.recv();
                    }
                    drop(handle);
                }
                Err(error) => {
                    if !worker_stopped.load(Ordering::Acquire) {
                        on_error(
                            error.to_string(),
                            Lease {
                                stopped: Arc::downgrade(&worker_stopped),
                            },
                        );
                    }
                }
            }
        })?;
    Ok(AsyncFileWatchHandle { stop, stopped })
}

enum WatchBackend {
    Native(RecommendedWatcher),
    Polling(PollWatcher),
}

/// Start watching normalized workspace roots. Native notifications are used
/// when available; a polling watcher is the safe fallback for unsupported
/// filesystems and failed recursive registrations.
pub(crate) fn start(
    roots: Vec<PathBuf>,
    on_batch: impl Fn(FileChangeBatch) + Send + 'static,
) -> notify::Result<FileWatchHandle> {
    start_filtered(roots, on_batch, true)
}

/// Protocol consumers may explicitly watch Git metadata. The editor's reload
/// watcher keeps its existing exclusion; both consumers share the same source.
pub(crate) fn start_unfiltered(
    roots: Vec<PathBuf>,
    on_batch: impl Fn(FileChangeBatch) + Send + 'static,
) -> notify::Result<FileWatchHandle> {
    start_filtered(roots, on_batch, false)
}

fn start_filtered(
    roots: Vec<PathBuf>,
    on_batch: impl Fn(FileChangeBatch) + Send + 'static,
    exclude_git: bool,
) -> notify::Result<FileWatchHandle> {
    let roots = normalized_roots(roots);
    let (event_sender, event_receiver) = mpsc::channel::<notify::Result<Event>>();

    let native = build_native_watcher(&roots, event_sender.clone());
    let (backend, backend_kind) = match native {
        Ok(watcher) => (WatchBackend::Native(watcher), WatchBackendKind::Native),
        Err(error) => {
            eprintln!("native file watching unavailable ({error}); using polling");
            (
                WatchBackend::Polling(build_poll_watcher(&roots, event_sender)?),
                WatchBackendKind::Polling,
            )
        }
    };

    let (stop, stop_receiver) = mpsc::channel();
    let worker = thread::Builder::new()
        .name("loog-file-watcher".into())
        .spawn(move || {
            // Keep the watcher alive for the lifetime of the receiver loop.
            let _backend = backend;
            let mut pending = Vec::new();
            while stop_receiver.try_recv().is_err() {
                match event_receiver.recv_timeout(QUIET_WINDOW) {
                    Ok(event) => {
                        pending.push(event);
                        let started = Instant::now();
                        loop {
                            if stop_receiver.try_recv().is_ok() {
                                return;
                            }
                            let remaining = MAX_BATCH_LATENCY.saturating_sub(started.elapsed());
                            if remaining.is_zero() {
                                break;
                            }
                            match event_receiver.recv_timeout(QUIET_WINDOW.min(remaining)) {
                                Ok(event) => pending.push(event),
                                Err(RecvTimeoutError::Timeout) => break,
                                Err(RecvTimeoutError::Disconnected) => break,
                            }
                        }
                        let batch =
                            coalesce_filtered(std::mem::take(&mut pending), &roots, exclude_git);
                        if batch.requires_full_rescan || !batch.changes.is_empty() {
                            on_batch(batch);
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        })
        .map_err(notify::Error::io)?;

    Ok(FileWatchHandle {
        lifetime: Arc::new(AtomicBool::new(false)),
        stop,
        worker: Some(worker),
        backend: backend_kind,
    })
}

pub(crate) fn build_native_watcher(
    roots: &[PathBuf],
    sender: Sender<notify::Result<Event>>,
) -> notify::Result<RecommendedWatcher> {
    let mut watcher = RecommendedWatcher::new(
        move |event| {
            let _ = sender.send(event);
        },
        Config::default(),
    )?;
    watch_roots(&mut watcher, roots)?;
    Ok(watcher)
}

pub(crate) fn build_poll_watcher(
    roots: &[PathBuf],
    sender: Sender<notify::Result<Event>>,
) -> notify::Result<PollWatcher> {
    let mut watcher = PollWatcher::new(
        move |event| {
            let _ = sender.send(event);
        },
        Config::default().with_poll_interval(POLL_INTERVAL),
    )?;
    watch_roots(&mut watcher, roots)?;
    Ok(watcher)
}

pub(crate) fn watch_roots(watcher: &mut impl Watcher, roots: &[PathBuf]) -> notify::Result<()> {
    for root in roots {
        watcher.watch(root, RecursiveMode::Recursive)?;
    }
    Ok(())
}

pub(crate) fn normalized_roots(roots: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut roots = roots
        .into_iter()
        .filter(|root| root.is_dir())
        .map(|root| std::fs::canonicalize(&root).unwrap_or(root))
        .collect::<Vec<_>>();
    roots.sort();
    roots.dedup();

    let mut reduced = Vec::<PathBuf>::new();
    for root in roots {
        if !reduced.iter().any(|parent| root.starts_with(parent)) {
            reduced.push(root);
        }
    }
    reduced
}

pub(crate) fn coalesce_events(
    events: Vec<notify::Result<Event>>,
    roots: &[PathBuf],
) -> FileChangeBatch {
    coalesce_filtered(events, roots, true)
}

fn coalesce_filtered(
    events: Vec<notify::Result<Event>>,
    roots: &[PathBuf],
    exclude_git: bool,
) -> FileChangeBatch {
    let mut by_path = HashMap::<PathBuf, FileChangeKind>::new();
    let mut requires_full_rescan = false;

    for event in events {
        let Ok(event) = event else {
            requires_full_rescan = true;
            continue;
        };
        if event.need_rescan() {
            requires_full_rescan = true;
        }
        let Some(kind) = change_kind(&event.kind) else {
            continue;
        };
        if kind == FileChangeKind::Rescan {
            requires_full_rescan = true;
        }
        for path in event.paths {
            let path = absolute_event_path(path, roots);
            // Git metadata is polled by the Git worker and must not cause
            // directory/document reload storms during checkout or staging.
            if exclude_git
                && path
                    .components()
                    .any(|component| component.as_os_str() == ".git")
            {
                continue;
            }
            if !roots.iter().any(|root| path.starts_with(root)) {
                continue;
            }
            let entry = by_path.entry(path).or_insert(kind);
            if kind_priority(kind) > kind_priority(*entry) {
                *entry = kind;
            }
            if by_path.len() > MAX_BATCH_PATHS {
                requires_full_rescan = true;
                by_path.clear();
                break;
            }
        }
    }

    let mut changes = by_path
        .into_iter()
        .map(|(path, kind)| FileChange { path, kind })
        .collect::<Vec<_>>();
    changes.sort_by(|left, right| left.path.cmp(&right.path));
    FileChangeBatch {
        changes,
        requires_full_rescan,
    }
}

pub(crate) fn change_kind(kind: &EventKind) -> Option<FileChangeKind> {
    match kind {
        EventKind::Create(_) => Some(FileChangeKind::Create),
        // Rename notifications contain the old and/or new paths depending on
        // the platform. Treat both as structural hints so their parents are
        // reread without trying to pair the events.
        EventKind::Modify(ModifyKind::Name(_)) => Some(FileChangeKind::Create),
        EventKind::Modify(_) => Some(FileChangeKind::Modify),
        EventKind::Remove(_) => Some(FileChangeKind::Remove),
        EventKind::Any | EventKind::Other => Some(FileChangeKind::Rescan),
        EventKind::Access(_) => None,
    }
}

pub(crate) fn kind_priority(kind: FileChangeKind) -> u8 {
    match kind {
        FileChangeKind::Modify => 1,
        FileChangeKind::Create => 2,
        FileChangeKind::Remove => 3,
        FileChangeKind::Rescan => 4,
    }
}

pub(crate) fn absolute_event_path(path: PathBuf, roots: &[PathBuf]) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        roots.first().map_or(path.clone(), |root| root.join(path))
    }
}

#[derive(Clone)]
pub(crate) struct Lease {
    stopped: std::sync::Weak<AtomicBool>,
}
impl Lease {
    pub(crate) fn cancelled(&self) -> bool {
        self.stopped
            .upgrade()
            .is_none_or(|stopped| stopped.load(Ordering::Acquire))
    }
}

impl FileWatchHandle {
    pub(crate) fn lease(&self) -> Lease {
        Lease {
            stopped: Arc::downgrade(&self.lifetime),
        }
    }
}
