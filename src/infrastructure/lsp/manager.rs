//! Runs the language servers for the open documents.
//!
//! One coordinator thread owns every server instance. The application hands it
//! snapshots of the open documents (cheap: text is shared) and gets back a
//! status list; nothing here runs on the UI thread, and a server that is slow,
//! missing or crashing never delays an edit.
//!
//! The coordinator
//! - decides which instance each document belongs to (see `plan`), reading the
//!   disk for project markers only when a document is new or the settings changed;
//! - starts instances, keeps their documents in step, and stops them when idle;
//! - restarts a crashed server after 1, 2 and 4 seconds, then reports it failed
//!   until the user restarts it;
//! - reports a program that cannot be started as unavailable, once, quietly.
#[path = "manager_registration.rs"]
mod registration;
#[path = "manager_requests.rs"]
mod requests;
pub(crate) use requests::{
    Failure, Part, Progress, Reply, RequestHandle, RequestSpec, ServerAction,
};

use super::client::{Client, Launch, Timeouts};
use crate::domain::language::{Registry, ResolvedLanguage};
use crate::features::language_service::{
    capabilities::ServerCapabilities,
    diagnostics::{self, Diagnostic},
    dynamic,
    expand::{self, Installed},
    plan::{self, Assignment, InstanceKey},
    semantic,
    server_set::ServerSet,
    session::{Event, ProgressUpdate, SessionOptions, State},
    sync::{DesiredDocument, DocumentSync, Notification, Stamp},
    uri,
};
use crate::features::settings::servers::{
    InstallPolicy, LanguageServerSetting, LanguageSetting, install_policy, registered,
    server_settings,
};
use crate::features::syntax::TokenSpan;
use crate::infrastructure::lsp_install::{Installer, PackStatus};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// A change to files the application made on disk.
#[derive(Clone, Debug)]
pub(crate) enum FileChange {
    Created(PathBuf),
    Renamed { from: PathBuf, to: PathBuf },
    Deleted(PathBuf),
}

/// Largest message accepted from a server.
const MAX_MESSAGE: usize = 64 * 1024 * 1024;

/// Set in the source id of findings that extensions published; server instance
/// ids never have it.
pub(crate) const EXTERNAL_SOURCE: u64 = 1 << 63;

#[derive(Clone, Debug)]
pub(crate) struct Options {
    pub timeouts: Timeouts,
    /// How long an instance with no documents keeps running.
    pub idle_stop: Duration,
    /// Waits before each automatic restart; once used up the instance is failed.
    pub backoff: Vec<Duration>,
    /// A server that stayed ready this long starts counting failures afresh.
    pub stable_after: Duration,
    /// Stop displaying progress that has not been refreshed for this long.
    pub progress_timeout: Duration,
    /// Wait for an end report after cancellation, then stop tracking the task.
    pub cancel_grace: Duration,
    /// Asked before a server process is started for a project root; an `Err`
    /// keeps the instance unavailable with that reason (design §9.1).
    pub start_gate: Option<StartGate>,
}

/// Decides whether servers may be started for a root. Detecting servers and
/// reading their configuration never starts anything; only this gate-guarded
pub(crate) type GateCheck = dyn Fn(&std::path::Path) -> Result<(), String> + Send + Sync;

/// Decides whether servers may be started for a root. Detecting servers and
/// reading their configuration never starts anything; only this gate-guarded
/// start does.
#[derive(Clone)]
pub(crate) struct StartGate(pub Arc<GateCheck>);

impl std::fmt::Debug for StartGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StartGate")
    }
}

impl Default for Options {
    fn default() -> Self {
        Self {
            timeouts: Timeouts::default(),
            idle_stop: Duration::from_secs(60),
            backoff: vec![
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
            ],
            stable_after: Duration::from_secs(60),
            progress_timeout: Duration::from_secs(300),
            cancel_grace: Duration::from_secs(5),
            start_gate: None,
        }
    }
}

/// An open document, as the application hands it over.
#[derive(Clone, Debug)]
pub(crate) struct SnapshotDocument {
    pub path: PathBuf,
    pub language: ResolvedLanguage,
    pub stamp: Stamp,
    pub text: Arc<String>,
}

/// Everything the coordinator needs to know about the application. Each snapshot
/// replaces the previous one.
#[derive(Clone, Debug, Default)]
pub(crate) struct Snapshot {
    pub documents: Vec<SnapshotDocument>,
    pub workspace_roots: Vec<PathBuf>,
    pub servers: Vec<LanguageServerSetting>,
    /// Which servers each language uses, and in what order.
    pub languages: BTreeMap<String, LanguageSetting>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    /// The process is starting or answering `initialize`.
    Starting,
    Ready,
    /// It stopped unexpectedly and will be started again.
    Restarting {
        attempt: usize,
        reason: String,
    },
    /// It stopped repeatedly; it stays down until restarted by the user.
    Failed(String),
    /// The program could not be started at all.
    Unavailable(String),
    /// The server's pack is not installed and nothing is installing it.
    NotInstalled,
    /// Its pack is being installed; the text says how far.
    Installing(String),
    /// Installing its pack failed.
    InstallFailed(String),
    InstallCancelling,
    InstallCancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InstanceInfo {
    /// What installing the server would download, when it is not installed.
    pub download_bytes: Option<u64>,
    pub id: u64,
    pub run: u64,
    pub registration_revision: u64,
    /// The server's name as people see it.
    pub server: String,
    /// The server definition's own name, as settings and commands name it.
    pub server_id: String,
    pub root: PathBuf,
    pub status: Status,
    pub install_generation: Option<u64>,
    pub documents: usize,
    /// The program and its arguments as started.
    pub command: String,
    pub process_id: Option<u32>,
    /// Background work the server reports, such as loading the project.
    pub tasks: Vec<TaskInfo>,
    /// Recent messages the server logged or showed, oldest first.
    pub log: Vec<String>,
    /// The last lines the server wrote to its error output.
    pub stderr: Vec<String>,
    pub diagnostics: DiagnosticCounts,
    /// What the server offers, once it has said.
    pub providers: Option<crate::features::language_service::providers::Providers>,
    pub registrations: Vec<String>,
}

/// What became of the diagnostics a server published, so a missing underline can
/// be told apart: nothing published, published for another version of the text,
/// or published and shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DiagnosticCounts {
    pub published: usize,
    pub shown: usize,
    /// Dropped because they described an older state of the document.
    pub outdated: usize,
    /// Dropped because the document is not open in this server any more.
    pub unknown: usize,
    /// Shown after being moved along the edits the server had not seen yet.
    pub carried: usize,
}

/// One running background task of a server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskInfo {
    pub id: u64,
    pub title: String,
    pub message: Option<String>,
    pub percentage: Option<u8>,
    pub cancellable: bool,
    pub cancelling: bool,
}

struct Task {
    token: crate::features::language_service::message::Id,
    info: TaskInfo,
    updated: Instant,
    cancelled: Option<Instant>,
}

/// Messages and tasks kept per instance.
const MAX_LOG: usize = 100;
const MAX_TASKS: usize = 32;

/// Tags from here up to the request channel's belong to pull diagnostics.
const PULL_BASE: u64 = 1 << 32;
/// How long after the last edit a document's diagnostics are pulled.
const PULL_DEBOUNCE: Duration = Duration::from_millis(200);

/// Pull diagnostics (`textDocument/diagnostic`) of one instance.
#[derive(Default)]
struct PullState {
    noted: HashMap<String, Stamp>,
    due: HashMap<String, Instant>,
    /// Requests sent: tag to (document key, the state asked about, the protocol
    /// version that state was sent as).
    in_flight: HashMap<u64, (String, Stamp, i32)>,
    next_tag: u64,
    /// The last `resultId` and findings of each document.
    last: HashMap<String, (String, Value)>,
}

/// How long after the last edit a document's semantic tokens are asked for.
const SEMANTIC_DEBOUNCE: Duration = Duration::from_millis(75);

/// What the servers said about one document, each part with the editor state it
/// describes. A part is only meant to be shown for exactly that state. Each
/// server instance keeps its own part, so one server's findings are replaced or
/// cleared without touching another's.
#[derive(Default)]
struct Entry {
    /// By instance.
    diagnostics: BTreeMap<u64, Findings>,
    /// By instance. Only the preferred matching one is shown.
    semantic: BTreeMap<u64, Tokens>,
}

/// The state some diagnostics describe, the diagnostics, and the text of that
/// state so they can be moved along later edits.
type Findings = (Stamp, Arc<[Diagnostic]>, Arc<String>);
/// The server's listing rank (lower is preferred), the state the tokens describe,
/// and the tokens.
type Tokens = (usize, Stamp, Arc<[TokenSpan]>);

impl Entry {
    fn is_empty(&self) -> bool {
        self.diagnostics.is_empty() && self.semantic.is_empty()
    }

    fn parts(&self) -> usize {
        self.diagnostics.len() + self.semantic.len()
    }
}

type Results = HashMap<String, Entry>;
type Store = Arc<Mutex<Results>>;

/// Drops what `instance` said about the document `key`.
fn forget(store: &mut Results, key: &str, instance: u64) {
    if let Some(entry) = store.get_mut(key) {
        entry.diagnostics.remove(&instance);
        entry.semantic.remove(&instance);
        if entry.is_empty() {
            store.remove(key);
        }
    }
}

/// The server results that apply to a document in a given state.
#[derive(Clone, Debug, Default)]
pub(crate) struct DocumentResults {
    pub diagnostics: Option<Arc<[Diagnostic]>>,
    pub semantic: Option<Arc<[TokenSpan]>>,
}

/// Pins diagnostic publications so equal-looking replacements (including new
/// server data) and restarted or re-registered providers invalidate action lists.
#[derive(Clone, Default)]
pub(crate) struct CodeActionContext {
    sources: Vec<(u64, u64, u64)>,
    diagnostics: Vec<Arc<[Diagnostic]>>,
}

impl PartialEq for CodeActionContext {
    fn eq(&self, other: &Self) -> bool {
        self.sources == other.sources
            && self.diagnostics.len() == other.diagnostics.len()
            && self
                .diagnostics
                .iter()
                .zip(&other.diagnostics)
                .all(|(a, b)| Arc::ptr_eq(a, b))
    }
}
impl Eq for CodeActionContext {}

impl CodeActionContext {
    pub fn has_sources(&self) -> bool {
        !self.sources.is_empty()
    }
}

enum Msg {
    Snapshot(Box<Snapshot>),
    Saved {
        path: PathBuf,
        text: Arc<String>,
    },
    Restart,
    /// Results were changed from outside the coordinator (extension findings).
    ResultsChanged,
    RestartInstance(u64),
    /// Install the pack of the named server.
    Install(String),
    CancelInstall {
        server: String,
        generation: u64,
    },
    RetryInstalls,
    CancelTask {
        instance: u64,
        run: u64,
        task: u64,
    },
    /// Some pack's installation moved on.
    InstallChanged,
    Request {
        id: u64,
        spec: Box<RequestSpec>,
        shared: Arc<Mutex<Progress>>,
    },
    Cancel(u64),
    /// Files were created, renamed or deleted on disk by the application.
    Files(FileChange),
    /// The application answers a request a server made.
    Respond {
        instance: u64,
        run: u64,
        id: crate::features::language_service::message::Id,
        outcome: Result<Value, crate::features::language_service::message::ResponseError>,
    },
    Event {
        instance: u64,
        run: u64,
        event: Box<Event>,
    },
    RegistrationReady {
        instance: u64,
        run: u64,
        generation: u64,
        outcome: Result<(), String>,
    },
    WatchedFiles {
        instance: u64,
        run: u64,
        generation: u64,
        changes: Vec<Value>,
    },
    WatchError {
        instance: u64,
        run: u64,
        generation: u64,
        error: String,
    },
    #[cfg(test)]
    ProgressTime {
        now: Instant,
        done: Sender<()>,
    },
    Stop,
}

pub(crate) struct Manager {
    tx: Sender<Msg>,
    actions: Arc<Mutex<Vec<ServerAction>>>,
    next_request: std::sync::atomic::AtomicU64,
    infos: Arc<Mutex<Vec<InstanceInfo>>>,
    results: Store,
    thread: Option<JoinHandle<()>>,
}

impl Manager {
    /// `on_change` runs on the coordinator thread whenever the status list
    /// changes; it should only request a repaint.
    pub fn start(
        registry: &'static Registry,
        servers: Arc<ServerSet>,
        installer: Option<Arc<Installer>>,
        options: Options,
        on_change: impl Fn() + Send + 'static,
    ) -> Self {
        let (tx, inbox) = mpsc::channel();
        // The installer holds the listener weakly; the coordinator keeps it alive.
        let install_listener: Option<Arc<dyn Fn() + Send + Sync>> =
            installer.as_ref().map(|installer| {
                let wake = tx.clone();
                let listener: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                    let _ = wake.send(Msg::InstallChanged);
                });
                installer.subscribe(listener.clone());
                listener
            });
        let infos = Arc::new(Mutex::new(Vec::new()));
        let actions: Arc<Mutex<Vec<ServerAction>>> = Arc::default();
        let results: Store = Arc::default();
        let coordinator = Coordinator {
            registry,
            base_servers: servers.clone(),
            servers,
            installer,
            _install_listener: install_listener,
            options,
            tx: tx.clone(),
            inbox,
            infos: infos.clone(),
            results: results.clone(),
            results_changed: false,
            book: requests::Book::default(),
            actions: actions.clone(),
            on_change: Box::new(on_change),
            instances: Vec::new(),
            next_id: 1,
            snapshot: Snapshot::default(),
            assignments: HashMap::new(),
            published: Vec::new(),
            #[cfg(test)]
            progress_time: None,
        };
        let thread = thread::Builder::new()
            .name("loom-lsp-manager".into())
            .spawn(move || coordinator.run())
            .expect("the coordinator thread starts");
        Self {
            tx,
            actions,
            next_request: std::sync::atomic::AtomicU64::new(1),
            infos,
            results,
            thread: Some(thread),
        }
    }

    /// Freeze/advance only progress expiry; real transport deadlines stay live.
    #[cfg(test)]
    pub(crate) fn set_progress_time(&self, now: Instant) {
        let (done, ready) = mpsc::channel();
        self.tx.send(Msg::ProgressTime { now, done }).unwrap();
        ready.recv_timeout(Duration::from_secs(15)).unwrap();
    }

    pub fn update(&self, snapshot: Snapshot) {
        let _ = self.tx.send(Msg::Snapshot(Box::new(snapshot)));
    }

    /// A document was written to disk.
    pub fn saved(&self, path: PathBuf, text: Arc<String>) {
        let _ = self.tx.send(Msg::Saved { path, text });
    }

    /// Show findings from a source that is not a language server, for exactly
    /// the state of the document they describe (design §6.3). `source` must have
    /// `EXTERNAL_SOURCE` set; each source keeps its own part, which replaces its
    /// previous findings for the document and never touches a server's.
    pub fn publish_external(
        &self,
        source: u64,
        path: &Path,
        stamp: Stamp,
        text: Arc<String>,
        diagnostics: Arc<[Diagnostic]>,
    ) {
        debug_assert!(source & EXTERNAL_SOURCE != 0);
        let Some(uri) = uri::file_uri(path) else {
            return;
        };
        self.results
            .lock()
            .unwrap()
            .entry(uri::key(&uri))
            .or_default()
            .diagnostics
            .insert(source, (stamp, diagnostics, text));
        let _ = self.tx.send(Msg::ResultsChanged);
    }

    /// Remove everything `source` published (it stopped or was unloaded).
    pub fn clear_external(&self, source: u64) {
        {
            let mut store = self.results.lock().unwrap();
            for entry in store.values_mut() {
                entry.diagnostics.remove(&source);
            }
            store.retain(|_, entry| !entry.is_empty());
        }
        let _ = self.tx.send(Msg::ResultsChanged);
    }

    /// Starts again every server that is down, and restarts the running ones.
    pub fn restart(&self) {
        let _ = self.tx.send(Msg::Restart);
    }

    /// Restarts one instance, leaving the others alone.
    pub fn restart_instance(&self, id: u64) {
        let _ = self.tx.send(Msg::RestartInstance(id));
    }

    /// Tells the servers that want to know that files changed on disk.
    pub fn files_changed(&self, change: FileChange) {
        let _ = self.tx.send(Msg::Files(change));
    }

    /// Installs the pack of the named server, now, whatever its install setting says.
    pub fn install(&self, server: &str) {
        let _ = self.tx.send(Msg::Install(server.to_owned()));
    }

    pub fn cancel_install(&self, server: &str, generation: u64) {
        let _ = self.tx.send(Msg::CancelInstall {
            server: server.into(),
            generation,
        });
    }

    /// Asks again for every pack whose installation failed.
    pub fn retry_installs(&self) {
        let _ = self.tx.send(Msg::RetryInstalls);
    }

    pub fn cancel_task(&self, instance: u64, run: u64, task: u64) {
        let _ = self.tx.send(Msg::CancelTask {
            instance,
            run,
            task,
        });
    }

    pub fn infos(&self) -> Vec<InstanceInfo> {
        self.infos.lock().unwrap().clone()
    }

    pub fn code_action_context(&self, path: &Path, stamp: Stamp) -> CodeActionContext {
        let sources = self
            .infos
            .lock()
            .unwrap()
            .iter()
            .filter(|info| {
                info.status == Status::Ready
                    && info.documents > 0
                    && path.starts_with(&info.root)
                    && info
                        .providers
                        .as_ref()
                        .is_some_and(|p| p.code_action.is_some())
            })
            .map(|info| (info.id, info.run, info.registration_revision))
            .collect();
        let diagnostics = uri::file_uri(path)
            .and_then(|uri| {
                self.results
                    .lock()
                    .unwrap()
                    .get(&uri::key(&uri))
                    .map(|entry| {
                        entry
                            .diagnostics
                            .values()
                            .filter(|(at, ..)| *at == stamp)
                            .map(|(_, found, _)| found.clone())
                            .collect()
                    })
            })
            .unwrap_or_default();
        CodeActionContext {
            sources,
            diagnostics,
        }
    }

    /// What the servers know about `path` in the editor state `stamp`. Results
    /// computed for any other state are not returned.
    pub fn results(&self, path: &Path, stamp: Stamp) -> DocumentResults {
        let Some(uri) = uri::file_uri(path) else {
            return DocumentResults::default();
        };
        let store = self.results.lock().unwrap();
        let Some(entry) = store.get(&uri::key(&uri)) else {
            return DocumentResults::default();
        };
        let mut matching = entry
            .diagnostics
            .values()
            .filter(|(at, ..)| *at == stamp)
            .map(|(_, found, _)| found);
        let diagnostics = match (matching.next(), matching.next()) {
            (None, _) => None,
            (Some(only), None) => Some(only.clone()),
            (Some(first), Some(second)) => {
                // Every server's findings, in text order; equal positions keep the
                // servers' order.
                let mut all: Vec<Diagnostic> = first.iter().chain(second.iter()).cloned().collect();
                for more in matching {
                    all.extend(more.iter().cloned());
                }
                all.sort_by_key(|d| (d.range.start, d.range.end));
                Some(all.into())
            }
        };
        DocumentResults {
            diagnostics,
            semantic: entry
                .semantic
                .values()
                .filter(|(_, at, _)| *at == stamp)
                .min_by_key(|(rank, ..)| *rank)
                .map(|(_, _, spans)| spans.clone()),
        }
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Running {
    /// Present once `initialize` has been answered.
    /// Drop leases before waiting for the process to exit.
    ready: Option<Box<Ready>>,
    client: Client,
}

struct Ready {
    sync: DocumentSync,
    since: Instant,
    capabilities: ServerCapabilities,
    semantic: SemanticState,
    pull: PullState,
    /// Work-done progress tokens in the order they began.
    tasks: Vec<Task>,
    next_task: u64,
    registrations: dynamic::Registry,
    watch: Option<crate::infrastructure::lsp_watch::Handle>,
    watch_generation: u64,
    next_registration: u64,
    registration_pending: Option<registration::Pending>,
    registration_queue: std::collections::VecDeque<(
        crate::features::language_service::message::Id,
        String,
        Value,
    )>,
}

/// When to ask for a document's semantic tokens and which answers still count.
#[derive(Default)]
struct SemanticState {
    /// Raised when the server says its tokens changed; answers asked for before
    /// that no longer count.
    epoch: u64,
    /// The state of each document (by URI key) last noticed.
    noted: HashMap<String, Stamp>,
    /// Documents to ask about, and when.
    due: HashMap<String, Instant>,
    /// Requests sent: tag to (document key, state asked about, epoch, whether
    /// only the difference from the previous result was asked for).
    in_flight: HashMap<u64, (String, Stamp, u64, bool)>,
    next_tag: u64,
    /// The last result for each document, to ask for differences from.
    previous: HashMap<String, (String, Vec<u32>)>,
}

impl Ready {
    fn wants_semantic_tokens(&self) -> bool {
        self.capabilities
            .semantic_tokens
            .as_ref()
            .is_some_and(|tokens| tokens.full || tokens.range)
    }
}

enum Phase {
    Running(Running),
    Waiting {
        retry_at: Instant,
        reason: String,
    },
    Failed(String),
    Unavailable(String),
    /// Waiting for the server's pack to be installed.
    NotInstalled,
}

struct Instance {
    id: u64,
    run: u64,
    registration_revision: u64,
    key: InstanceKey,
    phase: Phase,
    failures: usize,
    documents: Vec<DesiredDocument>,
    idle_since: Option<Instant>,
    /// What the server was told its settings are.
    configuration: BTreeMap<String, Value>,
    /// What the server said recently, for the status panel.
    log: std::collections::VecDeque<String>,
    diagnostics: DiagnosticCounts,
}

struct Coordinator {
    registry: &'static Registry,
    /// The catalogued servers.
    base_servers: Arc<ServerSet>,
    /// The catalogued servers and those the person registered.
    servers: Arc<ServerSet>,
    installer: Option<Arc<Installer>>,
    _install_listener: Option<Arc<dyn Fn() + Send + Sync>>,
    options: Options,
    tx: Sender<Msg>,
    inbox: Receiver<Msg>,
    infos: Arc<Mutex<Vec<InstanceInfo>>>,
    results: Store,
    /// Results were stored since the last announcement.
    results_changed: bool,
    /// Requests the application is waiting on.
    book: requests::Book,
    /// What servers asked of the application, until it takes it.
    actions: Arc<Mutex<Vec<ServerAction>>>,
    on_change: Box<dyn Fn() + Send>,
    instances: Vec<Instance>,
    next_id: u64,
    snapshot: Snapshot,
    /// Where each document went last time. Text-only edits reuse it, so typing
    /// never touches the disk to look for project markers.
    assignments: HashMap<PathBuf, (ResolvedLanguage, Vec<Assignment>)>,
    published: Vec<InstanceInfo>,
    #[cfg(test)]
    progress_time: Option<Instant>,
}

/// The settings the person gave `server`, by their top-level names.
fn configuration_of(servers: &[LanguageServerSetting], server: &str) -> BTreeMap<String, Value> {
    match server_settings(servers, server) {
        Value::Object(map) => map.into_iter().collect(),
        _ => BTreeMap::new(),
    }
}

fn has_entry(dir: &Path, name: &str) -> bool {
    dir.join(name).exists()
}

impl Coordinator {
    fn run(mut self) {
        loop {
            let message = match self.next_deadline() {
                Some(deadline) => match self
                    .inbox
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                {
                    Ok(message) => Some(message),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => break,
                },
                None => match self.inbox.recv() {
                    Ok(message) => Some(message),
                    Err(_) => break,
                },
            };
            if let Some(message) = message
                && !self.handle(message)
            {
                break;
            }
            self.timers();
            self.publish();
        }
        self.shutdown();
    }

    /// Returns false to stop.
    fn handle(&mut self, message: Msg) -> bool {
        match message {
            #[cfg(test)]
            Msg::ProgressTime { now, done } => {
                assert!(self.progress_time.is_none_or(|previous| now >= previous));
                self.progress_time = Some(now);
                self.expire_progress(now);
                self.publish();
                let _ = done.send(());
            }
            Msg::Stop => return false,
            Msg::Snapshot(snapshot) => {
                // Only the newest snapshot matters.
                let mut latest = *snapshot;
                let mut pending = Vec::new();
                while let Ok(next) = self.inbox.try_recv() {
                    match next {
                        Msg::Snapshot(newer) => latest = *newer,
                        Msg::Stop => return false,
                        other => pending.push(other),
                    }
                }
                self.apply_snapshot(latest);
                for message in pending {
                    if !self.handle(message) {
                        return false;
                    }
                }
            }
            Msg::Saved { path, text } => self.saved(&path, &text),
            Msg::Restart => self.restart(),
            Msg::ResultsChanged => self.results_changed = true,
            Msg::RestartInstance(id) => self.restart_where(|instance| instance.id == id),
            Msg::Install(server) => {
                let pack = self.servers.get(&server).and_then(|d| d.pack.clone());
                if let (Some(installer), Some(pack)) = (&self.installer, pack) {
                    let _ = installer.resume(&pack);
                }
            }
            Msg::CancelInstall { server, generation } => {
                let pack = self.servers.get(&server).and_then(|d| d.pack.clone());
                if let (Some(installer), Some(pack)) = (&self.installer, pack) {
                    installer.cancel(&super::super::lsp_install::InstallTarget {
                        pack: pack.to_string(),
                        generation,
                    });
                }
            }
            Msg::RetryInstalls => {
                if let Some(installer) = &self.installer {
                    installer.retry();
                }
            }
            Msg::CancelTask {
                instance,
                run,
                task,
            } => self.cancel_task(instance, run, task),
            Msg::InstallChanged => self.install_changed(),
            Msg::Request { id, spec, shared } => {
                self.request(id, *spec, shared);
                self.results_changed = true;
            }
            Msg::Cancel(id) => self.cancel_request(id),
            Msg::Files(change) => self.files_changed(&change),
            Msg::Respond {
                instance,
                run,
                id,
                outcome,
            } => {
                if let Some(Phase::Running(Running { client, .. })) = self
                    .instances
                    .iter()
                    .find(|i| i.id == instance && i.run == run)
                    .map(|i| &i.phase)
                {
                    client.respond(id, outcome);
                }
            }
            Msg::Event {
                instance,
                run,
                event,
            } => self.event(instance, run, *event),
            Msg::RegistrationReady {
                instance,
                run,
                generation,
                outcome,
            } => self.registration_ready(instance, run, generation, outcome),
            Msg::WatchedFiles {
                instance,
                run,
                generation,
                changes,
            } => self.watched_files(instance, run, generation, changes),
            Msg::WatchError {
                instance,
                run,
                generation,
                error,
            } => {
                if let Some(at) = self.watch_instance(instance, run, generation) {
                    self.remember(at, 2, &error);
                }
            }
        }
        true
    }

    fn progress_now(&self) -> Instant {
        #[cfg(test)]
        if let Some(now) = self.progress_time {
            return now;
        }
        Instant::now()
    }

    fn progress_deadline(&self, deadline: Option<Instant>) -> Option<Instant> {
        #[cfg(test)]
        if self.progress_time.is_some() {
            return None;
        }
        deadline
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.instances
            .iter()
            .filter_map(|instance| {
                let idle = instance
                    .idle_since
                    .map(|since| since + self.options.idle_stop);
                let retry = match &instance.phase {
                    Phase::Waiting { retry_at, .. } => Some(*retry_at),
                    _ => None,
                };
                let (semantic, pull, progress) = match &instance.phase {
                    Phase::Running(Running {
                        ready: Some(ready), ..
                    }) => (
                        ready.semantic.due.values().min().copied(),
                        ready.pull.due.values().min().copied(),
                        ready
                            .tasks
                            .iter()
                            .map(|task| {
                                task.cancelled
                                    .map_or(task.updated + self.options.progress_timeout, |when| {
                                        when + self.options.cancel_grace
                                    })
                            })
                            .min(),
                    ),
                    _ => (None, None, None),
                };
                let registration = match &instance.phase {
                    Phase::Running(Running {
                        ready: Some(ready), ..
                    }) => ready
                        .registration_pending
                        .as_ref()
                        .map(|pending| pending.deadline),
                    _ => None,
                };
                [
                    idle,
                    retry,
                    semantic,
                    pull,
                    self.progress_deadline(progress),
                    registration,
                ]
                .into_iter()
                .flatten()
                .min()
            })
            .chain(self.book.deadline())
            .min()
    }

    fn timers(&mut self) {
        let now = Instant::now();
        self.expire_requests(now);
        self.request_semantic_tokens(now);
        self.request_pulls(now);
        self.expire_progress(self.progress_now());
        self.expire_registrations(now);
        let idle_stop = self.options.idle_stop;
        let mut stopping = Vec::new();
        let mut lost = Vec::new();
        self.instances.retain_mut(|instance| {
            if instance.idle_since.is_some_and(|s| now >= s + idle_stop) {
                lost.push(instance.id);
                stopping.push(std::mem::replace(
                    &mut instance.phase,
                    Phase::Failed(String::new()),
                ));
                return false;
            }
            true
        });
        for phase in stopping {
            stop_in_background(phase);
        }
        for id in lost {
            self.requests_lost(id);
        }
        let due: Vec<usize> = self
            .instances
            .iter()
            .enumerate()
            .filter(
                |(_, i)| matches!(&i.phase, Phase::Waiting { retry_at, .. } if now >= *retry_at),
            )
            .map(|(at, _)| at)
            .collect();
        for at in due {
            self.start(at);
        }
    }

    /// The instances a document belongs to, one for each server its language is
    /// bound to.
    fn assign(&mut self, document: &SnapshotDocument) -> Vec<Assignment> {
        if let Some((language, assignments)) = self.assignments.get(&document.path)
            && *language == document.language
        {
            return assignments.clone();
        }
        let assignments = plan::assign(
            &self.servers,
            self.registry,
            document.language,
            &document.path,
            &self.snapshot.workspace_roots,
            &self.snapshot.servers,
            &self.snapshot.languages,
            has_entry,
        );
        self.assignments.insert(
            document.path.clone(),
            (document.language, assignments.clone()),
        );
        assignments
    }

    fn apply_snapshot(&mut self, snapshot: Snapshot) {
        // New roots or settings may move every document, so look again.
        if snapshot.servers != self.snapshot.servers {
            // Servers the person registered join the catalogued ones.
            let (extra, _) = registered(&snapshot.servers, &self.base_servers);
            self.servers = Arc::new(self.base_servers.extended(&extra).0);
        }
        if snapshot.workspace_roots != self.snapshot.workspace_roots
            || snapshot.servers != self.snapshot.servers
            || snapshot.languages != self.snapshot.languages
        {
            self.assignments.clear();
        }
        let open: std::collections::HashSet<&PathBuf> =
            snapshot.documents.iter().map(|d| &d.path).collect();
        self.assignments.retain(|path, _| open.contains(path));
        let documents = snapshot.documents.clone();
        self.snapshot = snapshot;

        let mut wanted: Vec<(InstanceKey, Vec<DesiredDocument>)> = Vec::new();
        for document in &documents {
            let Some(uri) = uri::file_uri(&document.path) else {
                continue;
            };
            for assignment in self.assign(document) {
                let desired = DesiredDocument {
                    uri: uri.clone(),
                    language_id: assignment.language_id,
                    stamp: document.stamp,
                    text: document.text.clone(),
                };
                match wanted.iter_mut().find(|(key, _)| *key == assignment.key) {
                    Some((_, list)) => list.push(desired),
                    None => wanted.push((assignment.key, vec![desired])),
                }
            }
        }

        let now = Instant::now();
        for instance in &mut self.instances {
            match wanted.iter().position(|(key, _)| *key == instance.key) {
                Some(at) => {
                    instance.documents = std::mem::take(&mut wanted[at].1);
                    instance.idle_since = None;
                }
                None => {
                    instance.documents.clear();
                    instance.idle_since.get_or_insert(now);
                }
            }
        }
        let mut created = Vec::new();
        for (key, documents) in wanted {
            if self.instances.iter().any(|i| i.key == key) {
                continue;
            }
            let id = self.next_id;
            self.next_id += 1;
            self.instances.push(Instance {
                id,
                run: 0,
                registration_revision: 0,
                key,
                phase: Phase::Failed(String::new()),
                failures: 0,
                configuration: BTreeMap::new(),
                log: Default::default(),
                diagnostics: DiagnosticCounts::default(),
                documents,
                idle_since: None,
            });
            created.push(self.instances.len() - 1);
        }
        for at in created {
            self.start(at);
        }
        self.install_changed();
        self.reconfigure();
        self.follow_edits();
        for at in 0..self.instances.len() {
            self.sync(at);
        }
    }

    /// Sends `workspace/didCreateFiles` and its relatives to the servers whose
    /// filters accept the files.
    fn files_changed(&mut self, change: &FileChange) {
        let (path, is_folder) = match change {
            FileChange::Created(path) | FileChange::Deleted(path) => (path, path.is_dir()),
            FileChange::Renamed { to, .. } => (to, to.is_dir()),
        };
        for instance in &self.instances {
            let Phase::Running(Running {
                client,
                ready: Some(ready),
            }) = &instance.phase
            else {
                continue;
            };
            let operations = &ready.capabilities.providers.file_operations;
            let (method, filters, params) = match change {
                FileChange::Created(path) => (
                    "workspace/didCreateFiles",
                    &operations.created,
                    uri::file_uri(path).map(|uri| json!({ "files": [{ "uri": uri }] })),
                ),
                FileChange::Deleted(path) => (
                    "workspace/didDeleteFiles",
                    &operations.deleted,
                    uri::file_uri(path).map(|uri| json!({ "files": [{ "uri": uri }] })),
                ),
                FileChange::Renamed { from, to } => (
                    "workspace/didRenameFiles",
                    &operations.renamed,
                    uri::file_uri(from)
                        .zip(uri::file_uri(to))
                        .map(|(old, new)| json!({ "files": [{ "oldUri": old, "newUri": new }] })),
                ),
            };
            let (Some(filters), Some(params)) = (filters, params) else {
                continue;
            };
            if filters.iter().any(|f| f.accepts(path, is_folder)) {
                client.notify(method, params);
            }
        }
    }

    /// Tells running servers whose settings changed; they are not restarted.
    fn reconfigure(&mut self) {
        for instance in &mut self.instances {
            let wanted = configuration_of(&self.snapshot.servers, &instance.key.server);
            if wanted == instance.configuration {
                continue;
            }
            if let Phase::Running(Running {
                client,
                ready: Some(_),
            }) = &instance.phase
            {
                client.configure(wanted.clone());
                instance.configuration = wanted;
            }
        }
    }

    /// Looks again at the servers waiting for their pack: starts those whose pack is
    /// now installed, and asks for the packs of those set to install on demand.
    fn install_changed(&mut self) {
        let Some(installer) = self.installer.clone() else {
            return;
        };
        for at in 0..self.instances.len() {
            if !matches!(self.instances[at].phase, Phase::NotInstalled) {
                continue;
            }
            let server = self.instances[at].key.server.clone();
            let Some(pack) = self.servers.get(&server).and_then(|d| d.pack.clone()) else {
                continue;
            };
            if installer.installed_dir(&pack).is_some() {
                self.start(at);
            } else if install_policy(&self.snapshot.servers, &server) == InstallPolicy::Auto {
                let _ = installer.request(&pack);
            }
        }
    }

    /// The status of a server whose pack is not there yet.
    fn install_status(&self, server: &str) -> Status {
        let status = self
            .servers
            .get(server)
            .and_then(|d| d.pack.as_deref())
            .zip(self.installer.as_ref())
            .map(|(pack, installer)| installer.status(pack));
        match status {
            None | Some(PackStatus::NotInstalled) => Status::NotInstalled,
            Some(PackStatus::Unavailable(reason)) => Status::Unavailable(reason),
            Some(PackStatus::Queued) => Status::Installing("queued".to_owned()),
            Some(PackStatus::Downloading { received, total }) => Status::Installing(format!(
                "downloading {}%",
                received.saturating_mul(100).checked_div(total).unwrap_or(0)
            )),
            Some(PackStatus::Verifying) => Status::Installing("verifying".to_owned()),
            Some(PackStatus::Starting) => Status::Installing("starting".to_owned()),
            Some(PackStatus::Cancelling) => Status::InstallCancelling,
            Some(PackStatus::Cancelled) => Status::InstallCancelled,
            // Installed a moment ago: the instance starts as soon as the message arrives.
            Some(PackStatus::Installed(_)) => Status::Starting,
            Some(PackStatus::Failed(error)) => Status::InstallFailed(error),
        }
    }

    /// Moves each document's diagnostics along the edits made since they were
    /// computed, so underlines stay on their text while the server catches up (and
    /// when it has nothing new to say, because its findings did not change).
    fn follow_edits(&mut self) {
        let mut store = self.results.lock().unwrap();
        for instance in &self.instances {
            for document in &instance.documents {
                let Some(entry) = store.get_mut(&uri::key(&document.uri)) else {
                    continue;
                };
                let Some((stamp, found, text)) = entry.diagnostics.get(&instance.id) else {
                    continue;
                };
                if stamp.incarnation != document.stamp.incarnation {
                    // Following edits is only valid within one document life.
                    // Identical text in a reopened file is still a new document.
                    entry.diagnostics.remove(&instance.id);
                    self.results_changed = true;
                    continue;
                }
                if *stamp == document.stamp {
                    continue;
                }
                let carried = diagnostics::carry(text, &document.text, found.to_vec());
                entry.diagnostics.insert(
                    instance.id,
                    (document.stamp, carried.into(), document.text.clone()),
                );
                self.results_changed = true;
            }
        }
    }

    fn start(&mut self, at: usize) {
        let servers = self.servers.clone();
        let installer = self.installer.clone();
        let timeouts = self.options.timeouts.clone();
        let tx = self.tx.clone();
        let server = self.instances[at].key.server.clone();
        let auto = install_policy(&self.snapshot.servers, &server) == InstallPolicy::Auto;
        let configuration = configuration_of(&self.snapshot.servers, &server);
        let gate = self.options.start_gate.clone();
        let instance = &mut self.instances[at];
        instance.run += 1;
        instance.log.clear();
        instance.diagnostics = DiagnosticCounts::default();
        let (id, run) = (instance.id, instance.run);
        let key = &instance.key;
        let name = servers
            .get(&key.server)
            .map_or_else(|| key.server.to_string(), |d| d.display_name.to_string());
        let Some(root_uri) = uri::file_uri(&key.root) else {
            instance.phase = Phase::Unavailable("the project folder has no usable path".into());
            return;
        };
        if let Some(StartGate(gate)) = &gate
            && let Err(reason) = gate(&key.root)
        {
            instance.phase = Phase::Unavailable(reason);
            return;
        }
        let mut command = key.launch.command.clone();
        let mut args = key.launch.args.clone();
        let mut options = key
            .launch
            .initialization_options
            .as_deref()
            .and_then(|text| serde_json::from_str::<Value>(text).ok());
        // A catalog server starts from files in its installed pack: expand the
        // placeholders, or wait for the pack to be installed.
        let templated = command.contains("${")
            || args.iter().any(|arg| arg.contains("${"))
            || key
                .launch
                .initialization_options
                .as_deref()
                .is_some_and(|text| text.contains("${"));
        if templated {
            let pack = servers.get(&key.server).and_then(|d| d.pack.clone());
            let (Some(installer), Some(pack)) = (installer, pack) else {
                instance.phase = Phase::Unavailable(format!("{name}: is not installed"));
                return;
            };
            let waiting = |instance: &mut Instance| {
                if auto {
                    let _ = installer.request(&pack);
                }
                instance.phase = Phase::NotInstalled;
            };
            let Some(directory) = installer.installed_dir(&pack) else {
                waiting(instance);
                return;
            };
            let runtime = |id: &str| installer.installed_dir(id);
            let installed = Installed {
                pack: &directory,
                runtime: &runtime,
            };
            let expanded = (|| -> Result<_, String> {
                let command = expand::expand(&command, &installed)?;
                let args = args
                    .iter()
                    .map(|arg| expand::expand(arg, &installed))
                    .collect::<Result<Vec<_>, _>>()?;
                let options = options
                    .as_ref()
                    .map(|options| expand::expand_json(options, &installed))
                    .transpose()?;
                Ok((command, args, options))
            })();
            match expanded {
                Ok((expanded_command, expanded_args, expanded_options)) => {
                    command = expand::program(&expanded_command, cfg!(windows))
                        .to_string_lossy()
                        .into_owned();
                    args = expanded_args;
                    options = expanded_options;
                }
                // A runtime the pack needs is gone: install it along with the pack.
                Err(error) if error.contains("is not installed") => {
                    waiting(instance);
                    return;
                }
                Err(error) => {
                    instance.phase = Phase::Unavailable(format!("{name}: {error}"));
                    return;
                }
            }
        }
        instance.configuration = configuration.clone();
        let launch = Launch {
            command: command.clone(),
            args,
            env: key.launch.env.clone(),
            cwd: key.root.clone(),
            session: SessionOptions {
                root_uri,
                root_name: key
                    .root
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                process_id: Some(std::process::id()),
                client_version: env!("CARGO_PKG_VERSION").to_owned(),
                initialization_options: options,
                configuration,
            },
            timeouts,
            max_message: MAX_MESSAGE,
        };
        instance.phase = match Client::spawn(launch, move |event| {
            let _ = tx.send(Msg::Event {
                instance: id,
                run,
                event: Box::new(event),
            });
        }) {
            Ok(client) => Phase::Running(Running {
                client,
                ready: None,
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Phase::Unavailable(format!("{name}: `{command}` was not found"))
            }
            Err(error) => Phase::Unavailable(format!("{name}: cannot start `{command}`: {error}")),
        };
    }

    fn event(&mut self, instance_id: u64, run: u64, event: Event) {
        let Some(at) = self
            .instances
            .iter()
            .position(|i| i.id == instance_id && i.run == run)
        else {
            return; // from a run that has been replaced
        };
        match event {
            Event::Ready(capabilities) => {
                if let Phase::Running(running) = &mut self.instances[at].phase {
                    running.ready = Some(Box::new(Ready {
                        sync: DocumentSync::with_encoding(
                            capabilities.text_sync,
                            capabilities.position_encoding,
                        ),
                        since: Instant::now(),
                        capabilities,
                        semantic: SemanticState::default(),
                        pull: PullState::default(),
                        tasks: Vec::new(),
                        next_task: 0,
                        registrations: dynamic::Registry::default(),
                        watch: None,
                        watch_generation: 0,
                        next_registration: 0,
                        registration_pending: None,
                        registration_queue: Default::default(),
                    }));
                }
                self.sync(at);
            }
            Event::Notification { method, params }
                if method == "textDocument/publishDiagnostics" =>
            {
                self.diagnostics(at, &params);
            }
            Event::SemanticTokensRefresh => {
                let instance = &mut self.instances[at];
                if let Phase::Running(Running {
                    ready: Some(ready), ..
                }) = &mut instance.phase
                {
                    ready.semantic.epoch += 1;
                    ready.semantic.previous.clear();
                    let now = Instant::now();
                    for document in &instance.documents {
                        ready.semantic.due.insert(uri::key(&document.uri), now);
                    }
                }
            }
            Event::DiagnosticsRefresh => {
                let instance = &mut self.instances[at];
                if let Phase::Running(Running {
                    ready: Some(ready), ..
                }) = &mut instance.phase
                {
                    if ready.capabilities.providers.pull_diagnostics {
                        let now = Instant::now();
                        for document in &instance.documents {
                            ready.pull.due.insert(uri::key(&document.uri), now);
                        }
                    }
                }
            }
            Event::Response { tag, outcome, .. } if tag >= requests::TAG_BASE => {
                self.request_response(at, tag, outcome)
            }
            Event::Response { tag, outcome, .. } if tag >= PULL_BASE => {
                self.pull_response(at, tag, outcome)
            }
            Event::Response { tag, outcome, .. } => self.semantic_response(at, tag, outcome),
            Event::Progress { token, update } => self.progress(at, token, update),
            Event::ServerRequest { id, method, params } => {
                self.server_request(at, id, &method, &params)
            }
            Event::Log { level, message } => self.remember(at, level, &message),
            Event::ShowMessage { level, message } => self.remember(at, level, &message),
            Event::StateChanged(State::Failed(reason)) => self.failed(at, reason),
            Event::StateChanged(State::Stopped) => {
                // Nobody asked it to stop while it was running here.
                self.failed(at, "the server exited".to_owned());
            }
            _ => {}
        }
    }

    fn remember(&mut self, at: usize, level: i64, message: &str) {
        let label = match level {
            1 => "error",
            2 => "warning",
            3 => "info",
            _ => "log",
        };
        let log = &mut self.instances[at].log;
        if log.len() == MAX_LOG {
            log.pop_front();
        }
        log.push_back(format!("{label}: {message}"));
    }

    fn progress(
        &mut self,
        at: usize,
        token: crate::features::language_service::message::Id,
        update: ProgressUpdate,
    ) {
        let now = self.progress_now();
        let Phase::Running(Running {
            ready: Some(ready), ..
        }) = &mut self.instances[at].phase
        else {
            return;
        };
        let tasks = &mut ready.tasks;
        match update {
            ProgressUpdate::Begin {
                title,
                message,
                percentage,
                cancellable,
            } => {
                tasks.retain(|task| task.token != token);
                if tasks.len() < MAX_TASKS {
                    ready.next_task += 1;
                    tasks.push(Task {
                        token,
                        info: TaskInfo {
                            id: ready.next_task,
                            title,
                            message,
                            percentage,
                            cancellable: cancellable.unwrap_or(false),
                            cancelling: false,
                        },
                        updated: now,
                        cancelled: None,
                    });
                }
            }
            ProgressUpdate::Report {
                message,
                percentage,
                cancellable,
            } => {
                if let Some(task) = tasks.iter_mut().find(|task| task.token == token) {
                    if message.is_some() {
                        task.info.message = message;
                    }
                    if percentage.is_some() {
                        task.info.percentage = percentage;
                    }
                    if let Some(cancellable) = cancellable {
                        task.info.cancellable = cancellable;
                    }
                    task.updated = now;
                }
            }
            ProgressUpdate::End { .. } => tasks.retain(|task| task.token != token),
        }
    }

    fn cancel_task(&mut self, instance: u64, run: u64, task_id: u64) {
        let now = self.progress_now();
        let Some(instance) = self
            .instances
            .iter_mut()
            .find(|i| i.id == instance && i.run == run)
        else {
            return;
        };
        let Phase::Running(Running {
            client,
            ready: Some(ready),
        }) = &mut instance.phase
        else {
            return;
        };
        let Some(task) = ready.tasks.iter_mut().find(|t| t.info.id == task_id) else {
            return;
        };
        if task.info.cancellable
            && !task.info.cancelling
            && client.notify(
                "window/workDoneProgress/cancel",
                json!({ "token": task.token.to_json() }),
            )
        {
            task.info.cancelling = true;
            task.cancelled = Some(now);
        }
    }

    fn expire_progress(&mut self, now: Instant) {
        let mut expired = Vec::new();
        for (at, instance) in self.instances.iter_mut().enumerate() {
            if let Phase::Running(Running {
                ready: Some(ready), ..
            }) = &mut instance.phase
            {
                ready.tasks.retain(|task| {
                    let due = task
                        .cancelled
                        .map_or(task.updated + self.options.progress_timeout, |when| {
                            when + self.options.cancel_grace
                        });
                    if due <= now {
                        expired.push((
                            at,
                            format!(
                                "Stopped tracking {}: {}",
                                task.info.title,
                                if task.cancelled.is_some() {
                                    "cancellation was not acknowledged"
                                } else {
                                    "no progress report before timeout"
                                }
                            ),
                        ));
                        false
                    } else {
                        true
                    }
                });
            }
        }
        for (at, message) in expired {
            self.remember(at, 2, &message);
        }
    }

    fn failed(&mut self, at: usize, reason: String) {
        let stable_after = self.options.stable_after;
        let backoff = self.options.backoff.clone();
        let lost = self.instances[at].id;
        self.requests_lost(lost);
        let instance = &mut self.instances[at];
        {
            let mut store = self.results.lock().unwrap();
            for document in &instance.documents {
                forget(&mut store, &uri::key(&document.uri), instance.id);
            }
            self.results_changed = true;
        }
        let stable = matches!(
            &instance.phase,
            Phase::Running(Running { ready: Some(ready), .. }) if ready.since.elapsed() >= stable_after
        );
        // Dropping the client reaps the process; it has already ended.
        instance.phase = Phase::Failed(String::new());
        if stable {
            instance.failures = 0;
        }
        instance.failures += 1;
        instance.phase = match backoff.get(instance.failures - 1) {
            Some(wait) => Phase::Waiting {
                retry_at: Instant::now() + *wait,
                reason,
            },
            None => Phase::Failed(reason),
        };
    }

    /// Sends the server whatever it is missing about the documents.
    fn sync(&mut self, at: usize) {
        let instance = &mut self.instances[at];
        let Phase::Running(Running {
            client,
            ready: Some(ready),
        }) = &mut instance.phase
        else {
            return;
        };
        let notes = match ready.sync.reconcile(&instance.documents) {
            Ok(notes) => notes,
            Err(_) => {
                // Never wrap versions in the same connection: a late publication
                // could otherwise match a new document's version. A new run also
                // invalidates all old semantic responses and queued events.
                let instance = &mut self.instances[at];
                {
                    let mut store = self.results.lock().unwrap();
                    for document in &instance.documents {
                        forget(&mut store, &uri::key(&document.uri), instance.id);
                    }
                    self.results_changed = true;
                }
                let old = std::mem::replace(&mut instance.phase, Phase::Failed(String::new()));
                instance.failures = 0;
                stop_in_background(old);
                let lost = self.instances[at].id;
                self.requests_lost(lost);
                self.start(at);
                return;
            }
        };
        for Notification { method, params } in notes {
            client.notify(method, params);
        }
        if ready.capabilities.providers.pull_diagnostics {
            let due = Instant::now() + PULL_DEBOUNCE;
            for document in &instance.documents {
                let key = uri::key(&document.uri);
                if ready.pull.noted.get(&key) != Some(&document.stamp) {
                    ready.pull.noted.insert(key.clone(), document.stamp);
                    ready.pull.due.insert(key, due);
                }
            }
        }
        if ready.wants_semantic_tokens() {
            let due = Instant::now() + SEMANTIC_DEBOUNCE;
            for document in &instance.documents {
                let key = uri::key(&document.uri);
                if ready.semantic.noted.get(&key) != Some(&document.stamp) {
                    ready.semantic.noted.insert(key.clone(), document.stamp);
                    ready.semantic.due.insert(key, due);
                }
            }
        }
    }

    /// Stores a server's diagnostics when they describe the document as it is now.
    fn diagnostics(&mut self, at: usize, params: &Value) {
        let Some(published) = diagnostics::parse(params) else {
            return;
        };
        enum Outcome {
            Shown(String, Stamp, Vec<Diagnostic>, Arc<String>),
            Carried(String, Stamp, Vec<Diagnostic>, Arc<String>),
            Outdated,
            Unknown,
        }
        let instance = &self.instances[at];
        let Phase::Running(Running {
            ready: Some(ready), ..
        }) = &instance.phase
        else {
            return;
        };
        let key = uri::key(&published.uri);
        let outcome = match instance.documents.iter().find(|d| uri::key(&d.uri) == key) {
            None => Outcome::Unknown,
            Some(document) => {
                // With a version the server says which text it looked at; without
                // one we can only assume it is the latest, which the next edit
                // invalidates.
                let describes_now = match published.version {
                    Some(version) => {
                        i32::try_from(version)
                            .ok()
                            .and_then(|version| ready.sync.stamp_of(&document.uri, version))
                            == Some(document.stamp)
                    }
                    None => true,
                };
                let encoding = ready.capabilities.position_encoding;
                if describes_now {
                    Outcome::Shown(
                        key,
                        document.stamp,
                        published.resolve(&document.text, encoding),
                        document.text.clone(),
                    )
                } else {
                    // The server is behind the typing. Keep what it found, moved
                    // along the edits it has not seen, so a diagnostic that is still
                    // true does not vanish when the server has nothing new to say.
                    let old = published
                        .version
                        .and_then(|v| i32::try_from(v).ok())
                        .and_then(|v| ready.sync.text_of(&document.uri, v));
                    match old {
                        Some(old) => {
                            let resolved = published.resolve(&old, encoding);
                            Outcome::Carried(
                                key,
                                document.stamp,
                                diagnostics::carry(&old, &document.text, resolved),
                                document.text.clone(),
                            )
                        }
                        None => Outcome::Outdated,
                    }
                }
            }
        };
        // Say why a publication was not used, so a missing underline can be explained.
        let why = match &outcome {
            Outcome::Unknown => {
                let open: Vec<&str> = self.instances[at]
                    .documents
                    .iter()
                    .map(|d| d.uri.as_str())
                    .collect();
                Some(format!(
                    "diagnostics for {} dropped: not an open document (open: {})",
                    published.uri,
                    open.join(", ")
                ))
            }
            Outcome::Outdated => Some(format!(
                "diagnostics for {} dropped: server version {:?} is no longer remembered",
                published.uri, published.version
            )),
            _ => None,
        };
        if let Some(why) = why {
            self.remember(at, 3, &why);
        }
        let instance_id = self.instances[at].id;
        let counts = &mut self.instances[at].diagnostics;
        counts.published += 1;
        match outcome {
            Outcome::Unknown => counts.unknown += 1,
            Outcome::Carried(key, stamp, carried, text) => {
                counts.carried += 1;
                let mut store = self.results.lock().unwrap();
                store
                    .entry(key)
                    .or_default()
                    .diagnostics
                    .insert(instance_id, (stamp, carried.into(), text));
            }
            Outcome::Outdated => counts.outdated += 1,
            Outcome::Shown(key, stamp, resolved, text) => {
                counts.shown += 1;
                let mut store = self.results.lock().unwrap();
                store
                    .entry(key)
                    .or_default()
                    .diagnostics
                    .insert(instance_id, (stamp, resolved.into(), text));
            }
        }
        self.results_changed = true;
    }

    fn semantic_response(
        &mut self,
        at: usize,
        tag: u64,
        outcome: Result<Value, crate::features::language_service::message::ResponseError>,
    ) {
        let rank = self
            .servers
            .rank(&self.instances[at].key.server)
            .unwrap_or(usize::MAX);
        let instance = &mut self.instances[at];
        let instance_id = instance.id;
        let Phase::Running(Running {
            ready: Some(ready), ..
        }) = &mut instance.phase
        else {
            return;
        };
        let Some((key, stamp, epoch, was_delta)) = ready.semantic.in_flight.remove(&tag) else {
            return;
        };
        let document = instance.documents.iter().find(|d| uri::key(&d.uri) == key);
        let current = document.is_some_and(|d| d.stamp == stamp) && epoch == ready.semantic.epoch;
        let now = Instant::now();
        match outcome {
            Ok(_) if !current => {
                if document.is_some() {
                    ready.semantic.due.insert(key, now);
                }
            }
            Ok(value) => {
                let Some(document) = document else { return };
                // The integers of the result: given whole, or a difference to apply
                // to the last result. Anything unusable drops what was remembered
                // and asks again for everything.
                let data = if value.is_null() {
                    Some(Vec::new())
                } else if let Some(edits) = semantic::parse_delta(&value) {
                    ready
                        .semantic
                        .previous
                        .get(&key)
                        .filter(|_| was_delta)
                        .and_then(|(_, old)| semantic::apply_delta(old, &edits).ok())
                } else {
                    value.get("data").and_then(semantic::integers)
                };
                let Some(data) = data else {
                    ready.semantic.previous.remove(&key);
                    ready.semantic.due.insert(key, now);
                    return;
                };
                match value.get("resultId").and_then(Value::as_str) {
                    Some(id) if !value.is_null() => {
                        ready
                            .semantic
                            .previous
                            .insert(key.clone(), (id.to_owned(), data.clone()));
                    }
                    _ => {
                        ready.semantic.previous.remove(&key);
                    }
                }
                let legend = ready
                    .capabilities
                    .semantic_tokens
                    .as_ref()
                    .map(|tokens| tokens.legend.clone())
                    .unwrap_or_default();
                // A null answer clears the layer, like an empty one.
                let decoded = if value.is_null() {
                    Ok(Vec::new())
                } else {
                    semantic::decode(
                        &Value::Array(data.iter().map(|n| Value::from(*n)).collect()),
                        &legend,
                        &document.text,
                        ready.capabilities.position_encoding,
                    )
                };
                if let Ok(spans) = decoded {
                    let mut store = self.results.lock().unwrap();
                    store
                        .entry(key)
                        .or_default()
                        .semantic
                        .insert(instance_id, (rank, stamp, spans.into()));
                    self.results_changed = true;
                }
            }
            Err(error) if crate::features::language_service::session::is_stale(&error) => {
                if document.is_some() {
                    ready.semantic.due.insert(key, now + SEMANTIC_DEBOUNCE);
                }
            }
            Err(_) => {}
        }
    }

    /// Asks the servers that answer pull requests for the problems of documents
    /// whose wait is over.
    fn request_pulls(&mut self, now: Instant) {
        for instance in &mut self.instances {
            let Phase::Running(Running {
                client,
                ready: Some(ready),
            }) = &mut instance.phase
            else {
                continue;
            };
            let due: Vec<String> = ready
                .pull
                .due
                .iter()
                .filter(|(_, when)| **when <= now)
                .map(|(key, _)| key.clone())
                .collect();
            for key in due {
                if ready.pull.in_flight.values().any(|(k, ..)| *k == key) {
                    continue;
                }
                ready.pull.due.remove(&key);
                let Some(document) = instance.documents.iter().find(|d| uri::key(&d.uri) == key)
                else {
                    continue;
                };
                let Some(version) = ready.sync.version_of(&document.uri) else {
                    // Not sent yet: asked again when it has been.
                    ready.pull.due.insert(key, now + PULL_DEBOUNCE);
                    continue;
                };
                ready.pull.next_tag += 1;
                let tag = PULL_BASE + ready.pull.next_tag;
                let mut params = json!({ "textDocument": { "uri": document.uri } });
                if let Some((id, _)) = ready.pull.last.get(&key) {
                    params["previousResultId"] = json!(id);
                }
                if client.request("textDocument/diagnostic", params, tag) {
                    ready
                        .pull
                        .in_flight
                        .insert(tag, (key, document.stamp, version));
                }
            }
        }
    }

    /// The findings a pull request returned, stored like pushed ones.
    fn pull_response(
        &mut self,
        at: usize,
        tag: u64,
        outcome: Result<Value, crate::features::language_service::message::ResponseError>,
    ) {
        let Phase::Running(Running {
            ready: Some(ready), ..
        }) = &mut self.instances[at].phase
        else {
            return;
        };
        let Some((key, _stamp, version)) = ready.pull.in_flight.remove(&tag) else {
            return;
        };
        let instance = &self.instances[at];
        let Some(document) = instance.documents.iter().find(|d| uri::key(&d.uri) == key) else {
            return;
        };
        let uri = document.uri.clone();
        let Ok(value) = outcome else {
            // The server could not say now; try again after the next edit.
            return;
        };
        let (items, id) = match value.get("kind").and_then(Value::as_str) {
            Some("unchanged") => {
                let Phase::Running(Running {
                    ready: Some(ready), ..
                }) = &self.instances[at].phase
                else {
                    return;
                };
                match ready.pull.last.get(&key) {
                    Some((id, items)) => (items.clone(), id.clone()),
                    None => return,
                }
            }
            Some("full") => (
                value.get("items").cloned().unwrap_or_else(|| json!([])),
                value
                    .get("resultId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            ),
            _ => return,
        };
        if let Phase::Running(Running {
            ready: Some(ready), ..
        }) = &mut self.instances[at].phase
        {
            if id.is_empty() {
                ready.pull.last.remove(&key);
            } else {
                ready.pull.last.insert(key, (id, items.clone()));
            }
        }
        // Handled like a publication for the state the request was sent for.
        self.diagnostics(
            at,
            &json!({ "uri": uri, "version": version, "diagnostics": items }),
        );
        self.results_changed = true;
    }

    /// Asks for the semantic tokens of documents whose wait is over.
    fn request_semantic_tokens(&mut self, now: Instant) {
        for instance in &mut self.instances {
            let Phase::Running(Running {
                client,
                ready: Some(ready),
            }) = &mut instance.phase
            else {
                continue;
            };
            let due: Vec<String> = ready
                .semantic
                .due
                .iter()
                .filter(|(_, when)| **when <= now)
                .map(|(key, _)| key.clone())
                .collect();
            for key in due {
                // One request per document at a time.
                if ready.semantic.in_flight.values().any(|(k, ..)| *k == key) {
                    continue;
                }
                ready.semantic.due.remove(&key);
                let Some(document) = instance.documents.iter().find(|d| uri::key(&d.uri) == key)
                else {
                    continue;
                };
                ready.semantic.next_tag += 1;
                let tag = ready.semantic.next_tag;
                let delta_ok = ready
                    .capabilities
                    .semantic_tokens
                    .as_ref()
                    .is_some_and(|tokens| tokens.delta);
                let previous = ready
                    .semantic
                    .previous
                    .get(&key)
                    .filter(|_| delta_ok)
                    .map(|(id, _)| id.clone());
                let (method, params) = match &previous {
                    Some(id) => (
                        "textDocument/semanticTokens/full/delta",
                        json!({ "textDocument": { "uri": document.uri }, "previousResultId": id }),
                    ),
                    None if ready
                        .capabilities
                        .semantic_tokens
                        .as_ref()
                        .is_some_and(|tokens| !tokens.full && tokens.range) =>
                    {
                        // One stamped range covers the whole open document. It
                        // cannot leave stale or uncovered viewport fragments.
                        let end = crate::features::language_service::position::LineIndex::new(
                            &document.text,
                        )
                        .position(document.text.len(), ready.capabilities.position_encoding);
                        (
                            "textDocument/semanticTokens/range",
                            json!({"textDocument":{"uri":document.uri},"range":{"start":{"line":0,"character":0},"end":{"line":end.line,"character":end.character}}}),
                        )
                    }
                    None => (
                        "textDocument/semanticTokens/full",
                        json!({ "textDocument": { "uri": document.uri } }),
                    ),
                };
                if client.request(method, params, tag) {
                    ready.semantic.in_flight.insert(
                        tag,
                        (
                            key,
                            document.stamp,
                            ready.semantic.epoch,
                            previous.is_some(),
                        ),
                    );
                }
            }
        }
    }

    fn saved(&mut self, path: &Path, text: &Arc<String>) {
        let Some(uri) = uri::file_uri(path) else {
            return;
        };
        for instance in &mut self.instances {
            if let Phase::Running(Running {
                client,
                ready: Some(ready),
            }) = &mut instance.phase
                && let Some(Notification { method, params }) = ready.sync.saved(&uri, text)
            {
                client.notify(method, params);
            }
        }
    }

    fn restart(&mut self) {
        self.restart_where(|_| true);
    }

    fn restart_where(&mut self, picked: impl Fn(&Instance) -> bool) {
        // An instance with no documents has nothing to restart for; it is simply
        // dropped and comes back when a document needs it.
        let mut stopping = Vec::new();
        let mut lost = Vec::new();
        self.instances.retain_mut(|instance| {
            if !picked(instance) {
                return true;
            }
            lost.push(instance.id);
            stopping.push(std::mem::replace(
                &mut instance.phase,
                Phase::Failed(String::new()),
            ));
            instance.failures = 0;
            !instance.documents.is_empty()
        });
        for id in lost {
            self.requests_lost(id);
        }
        for phase in stopping {
            stop_in_background(phase);
        }
        for at in 0..self.instances.len() {
            self.start(at);
            self.sync(at);
        }
    }

    fn publish(&mut self) {
        let infos: Vec<InstanceInfo> = self
            .instances
            .iter()
            .map(|instance| InstanceInfo {
                download_bytes: None,
                id: instance.id,
                run: instance.run,
                registration_revision: instance.registration_revision,
                server: self.servers.get(&instance.key.server).map_or_else(
                    || instance.key.server.to_string(),
                    |definition| definition.display_name.to_string(),
                ),
                server_id: instance.key.server.to_string(),
                install_generation: self
                    .servers
                    .get(&instance.key.server)
                    .and_then(|definition| definition.pack.as_deref())
                    .and_then(|pack| self.installer.as_ref()?.cancel_target(pack))
                    .map(|target| target.generation),
                providers: match &instance.phase {
                    Phase::Running(Running {
                        ready: Some(ready), ..
                    }) => {
                        let mut providers = ready
                            .registrations
                            .effective(&ready.capabilities.providers, None);
                        for document in &instance.documents {
                            providers.merge(
                                &ready
                                    .registrations
                                    .effective(&ready.capabilities.providers, Some(document)),
                            );
                        }
                        Some(providers)
                    }
                    _ => None,
                },
                registrations: match &instance.phase {
                    Phase::Running(Running {
                        ready: Some(ready), ..
                    }) => ready.registrations.ids(),
                    _ => Vec::new(),
                },
                root: instance.key.root.clone(),
                documents: instance.documents.len(),
                command: std::iter::once(instance.key.launch.command.as_str())
                    .chain(instance.key.launch.args.iter().map(String::as_str))
                    .collect::<Vec<_>>()
                    .join(" "),
                process_id: match &instance.phase {
                    Phase::Running(running) => Some(running.client.process_id()),
                    _ => None,
                },
                tasks: match &instance.phase {
                    Phase::Running(Running {
                        ready: Some(ready), ..
                    }) => ready.tasks.iter().map(|task| task.info.clone()).collect(),
                    _ => Vec::new(),
                },
                log: instance.log.iter().cloned().collect(),
                diagnostics: instance.diagnostics,
                stderr: match &instance.phase {
                    Phase::Running(running) => {
                        let tail = running.client.stderr_tail();
                        tail[tail.len().saturating_sub(30)..].to_vec()
                    }
                    _ => Vec::new(),
                },
                status: match &instance.phase {
                    Phase::NotInstalled => self.install_status(&instance.key.server),
                    Phase::Running(Running { ready: Some(_), .. }) => Status::Ready,
                    Phase::Running(_) => Status::Starting,
                    Phase::Waiting { reason, .. } if reason.is_empty() => Status::Starting,
                    Phase::Waiting { reason, .. } => Status::Restarting {
                        attempt: instance.failures,
                        reason: reason.clone(),
                    },
                    Phase::Failed(reason) => Status::Failed(reason.clone()),
                    Phase::Unavailable(reason) => Status::Unavailable(reason.clone()),
                },
            })
            .collect();
        let mut infos = infos;
        for info in infos
            .iter_mut()
            .filter(|info| info.status == Status::NotInstalled)
        {
            info.download_bytes = self
                .servers
                .get(&info.server_id)
                .and_then(|definition| definition.pack.as_deref())
                .zip(self.installer.as_ref())
                .and_then(|(pack, installer)| installer.missing_archives(pack).ok())
                .map(|archives| archives.values().sum());
        }
        {
            // Forget what an instance said about documents it does not hold any
            // more, and results of documents no server holds at all.
            let mut held: HashMap<String, Vec<u64>> = HashMap::new();
            for instance in &self.instances {
                for document in &instance.documents {
                    held.entry(uri::key(&document.uri))
                        .or_default()
                        .push(instance.id);
                }
            }
            let mut store = self.results.lock().unwrap();
            let before: usize = store.values().map(Entry::parts).sum();
            store.retain(|key, entry| {
                let holders = held.get(key);
                let holds = |id: &u64| holders.is_some_and(|holders| holders.contains(id));
                // Findings pushed by extensions are not tied to a server instance.
                entry
                    .diagnostics
                    .retain(|id, _| id & EXTERNAL_SOURCE != 0 || holds(id));
                entry.semantic.retain(|id, _| holds(id));
                !entry.is_empty()
            });
            let after: usize = store.values().map(Entry::parts).sum();
            self.results_changed |= after != before;
        }
        if std::mem::take(&mut self.results_changed) {
            (self.on_change)();
        }
        if infos != self.published {
            *self.infos.lock().unwrap() = infos.clone();
            self.published = infos;
            (self.on_change)();
        }
    }

    fn shutdown(&mut self) {
        let handles: Vec<JoinHandle<()>> = self
            .instances
            .drain(..)
            .filter_map(|instance| stop_in_background(instance.phase))
            .collect();
        for handle in handles {
            let _ = handle.join();
        }
        self.published.clear();
        self.infos.lock().unwrap().clear();
    }
}

/// Stops a running server without making the coordinator wait for it.
fn stop_in_background(phase: Phase) -> Option<JoinHandle<()>> {
    let Phase::Running(mut running) = phase else {
        return None;
    };
    if let Some(ready) = running.ready.as_mut() {
        ready.watch = None;
        ready.registration_pending = None;
        ready.registration_queue.clear();
        ready.registrations = dynamic::Registry::default();
    }
    thread::Builder::new()
        .name("loom-lsp-stop".into())
        .spawn(move || {
            if let Some(ready) = running.ready.as_mut() {
                for Notification { method, params } in ready.sync.close_all() {
                    running.client.notify(method, params);
                }
            }
            running.client.stop();
        })
        .ok()
}
