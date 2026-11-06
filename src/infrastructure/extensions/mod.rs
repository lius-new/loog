//! Background executor for extension instances (design §5, §7.1, §8).
//!
//! One thread owns the Wasmtime runtime and every instance, so the UI thread
//! never waits on guest code. The application sends [`Job`]s and receives
//! [`Event`]s through a sink; nothing UI-shaped crosses this boundary, only
//! typed values and opaque ids (design §5, last layering rule).
pub(crate) mod document_queue;
pub mod documentation;
pub mod fileops;
pub mod http;
pub mod icon;
pub mod state;
// Used by the extensions page and on-demand installs, which are built on it next.
#[allow(dead_code)]
pub mod install;
pub mod package;
pub mod ports;
// Read by the extensions page and the installer, which are built on it next.
pub mod processes;
#[allow(dead_code)]
pub mod registry;
pub mod tasks;
pub mod trust_store;
pub mod watches;

use loom_extension_api::{ErrorKind, ExtensionError};
use loom_extension_host::{
    AcceptedEdit, ByteRange, CommandContext, CommittedReceipt, Completion, HostError, Inbound,
    Instance, Limits, Provided, ProviderKind, ProviderRequest, PublishedDiagnostics, RootRef,
    RoundStatus, Runtime, Selection, StartedFileOps, StartedTask, WitErrorKind,
};
use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

/// Rounds in a row the executor gives one instance when it keeps reporting
/// `runnable`, before other instances and controls get their turn.
const MAX_CONSECUTIVE_ROUNDS: usize = 8;

/// A request to a registered provider for one state of one document.
#[derive(Clone, Debug)]
pub struct ProviderJob {
    pub request: u64,
    pub provider: String,
    pub kind: ProviderKind,
    pub token: u64,
    pub text: Arc<String>,
    pub document: u64,
    pub version: u64,
    pub range: Option<Range<usize>>,
    /// Completion: the caret, the triggering character and, for a resolve request,
    /// the item to complete.
    pub cursor: Option<usize>,
    pub trigger: Option<String>,
    pub item: Option<loom_extension_host::WireCompletionItem>,
    pub context: Option<loom_extension_host::ProviderContext>,
    pub timeout_ms: u64,
}

/// What the application tells an instance happened to an operation.
#[derive(Clone, Debug)]
pub enum Outcome {
    Committed { transaction: u64, version: u64 },
    Failed(ExtensionError),
}

#[derive(Clone, Debug)]
pub struct Run {
    pub call: u64,
    pub command: String,
    /// Captured target text, readable by the guest through this token.
    pub snapshot: Option<(u64, Arc<String>)>,
    /// `(range, reversed)` for every selection, in document order.
    pub selections: Vec<(Range<usize>, bool)>,
    pub selection_precondition: Option<u64>,
}

// Jobs are moved through a channel once each; the size gap between loading a
// component and the small messages is not worth boxing every field.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Job {
    WorkspaceFiles {
        id: String,
        generation: u64,
        changes: Vec<loom_extension_host::WorkspaceFileChange>,
    },
    DocumentSaved {
        id: String,
        generation: u64,
        reference: loom_extension_host::DocumentRef,
        text: Arc<String>,
    },
    CancelProvider {
        id: String,
        generation: u64,
        request: u64,
    },
    Documents {
        id: String,
        generation: u64,
        changed: Vec<loom_extension_host::DocumentView>,
        closed: Vec<u64>,
    },
    DocumentEvents {
        id: String,
        generation: u64,
        references: Vec<loom_extension_host::DocumentRef>,
        messages: Vec<Inbound>,
    },
    DocumentBatch {
        id: String,
        generation: u64,
        queue: Arc<Mutex<document_queue::Queue>>,
    },
    UiEvents {
        id: String,
        generation: u64,
        events: UiEvents,
    },
    Load {
        id: String,
        generation: u64,
        component: Vec<u8>,
        package_dir: PathBuf,
        version: String,
        cache: PathBuf,
        limits: Limits,
        /// Where this extension's stored values live; `None` when storage is not granted.
        storage: Option<PathBuf>,
        /// Whether bounded file access is granted.
        files: bool,
        /// Hosts the extension may reach; empty when network access is not granted.
        domains: Vec<String>,
    },
    Run {
        id: String,
        generation: u64,
        run: Run,
    },
    Complete {
        id: String,
        generation: u64,
        operation: u64,
        outcome: Outcome,
    },
    Cancel {
        id: String,
        generation: u64,
        call: u64,
    },
    /// Ask a provider; the answer comes back as [`Event::Provided`].
    Provide {
        id: String,
        generation: u64,
        job: Box<ProviderJob>,
    },
    /// Lift the side-effect restriction of a save request that ended unanswered.
    EndSave {
        id: String,
        generation: u64,
        request: u64,
    },
    /// The workspace folders the extension may name.
    Roots {
        id: String,
        generation: u64,
        /// Handle, path and stable storage key of every folder.
        roots: Vec<(RootRef, PathBuf, String)>,
        /// Identifies the current set of folders for workspace-scoped storage.
        workspace_key: String,
    },
    /// Something happened to a task of the extension. `ended` frees its slot.
    Task {
        id: String,
        generation: u64,
        message: Inbound,
        ended: bool,
    },
    Process {
        id: String,
        generation: u64,
        message: Inbound,
        credit: Option<processes::Credit>,
    },
    Watch {
        id: String,
        generation: u64,
        message: Inbound,
        credit: Option<watches::Credit>,
    },
    /// New resolved setting values; `changed` lists the keys whose value differs.
    Settings {
        id: String,
        generation: u64,
        values: std::collections::BTreeMap<String, String>,
        changed: Vec<String>,
    },
    /// Document events, in order, for an instance that subscribed to them.
    Deliver {
        id: String,
        generation: u64,
        messages: Vec<Inbound>,
    },
    Stop {
        id: String,
    },
    Shutdown,
}

#[derive(Debug)]
pub enum Event {
    WorkspaceOperation {
        id: String,
        generation: u64,
        operation: u64,
        request: loom_extension_host::WorkspaceOperation,
    },
    ProviderExpired {
        id: String,
        generation: u64,
        request: u64,
    },
    ProvidersConfigured {
        id: String,
        generation: u64,
        configurations: Vec<loom_extension_host::ProviderConfiguration>,
    },
    UiChanged {
        id: String,
        generation: u64,
        snapshot: UiMailbox,
    },
    /// Everything the extension reports about its service, after a change.
    ServiceStatus {
        id: String,
        generation: u64,
        reports: Vec<loom_extension_host::ServiceReport>,
    },
    /// What the extension wrote to its Output channels.
    Output {
        id: String,
        generation: u64,
        changes: Vec<loom_extension_host::OutputChange>,
    },
    WatchControls {
        id: String,
        generation: u64,
        controls: Vec<loom_extension_host::WatchControl>,
    },
    ProcessControls {
        id: String,
        generation: u64,
        controls: Vec<loom_extension_host::ProcessControl>,
    },
    Loaded {
        id: String,
        generation: u64,
        cache_hit: bool,
        source: loom_extension_host::CodeSource,
        aot_rejection: Option<String>,
        result: Result<(), ExtensionError>,
    },
    EditRequested {
        id: String,
        generation: u64,
        edit: AcceptedEdit,
    },
    /// A provider answered. The application decides whether the answer is
    /// still current before using it.
    Provided {
        id: String,
        generation: u64,
        provided: Provided,
    },
    /// The guest asked to start a task. The application decides whether it may run.
    TaskRequested {
        id: String,
        generation: u64,
        task: StartedTask,
    },
    /// The guest asked to create, rename or delete files.
    FileOpsRequested {
        id: String,
        generation: u64,
        started: StartedFileOps,
    },
    /// The guest asked to cancel tasks (their local numbers).
    TasksCancelled {
        id: String,
        generation: u64,
        tasks: Vec<u64>,
    },
    /// The guest pushed findings for one state of a file.
    DiagnosticsPublished {
        id: String,
        generation: u64,
        published: PublishedDiagnostics,
    },
    /// The instance trapped or ran past a limit; it has been dropped.
    Failed {
        id: String,
        generation: u64,
        error: ExtensionError,
    },
    Log {
        id: String,
        level: u8,
        text: String,
        _credit: loom_extension_host::PublicationCredit,
    },
}

pub type Sink = Box<dyn Fn(Event) + Send + 'static>;
type DocumentQueues = HashMap<(String, u64), Arc<Mutex<document_queue::Queue>>>;

pub struct Executor {
    jobs: Sender<Job>,
    stopped: Arc<std::sync::atomic::AtomicBool>,
    documents: Mutex<DocumentQueues>,
    thread: Option<JoinHandle<()>>,
}

impl Executor {
    pub fn spawn(sink: Sink) -> Result<Self, String> {
        let (jobs, inbox) = channel();
        let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_stopped = stopped.clone();
        let thread = std::thread::Builder::new()
            .name("loom-extensions".to_owned())
            .spawn(move || {
                #[cfg(test)]
                let runtime = match std::env::var("LOOM_MIGRATION_AOT_PUBLIC_KEY") {
                    Ok(key) => Runtime::with_aot_authorities(vec![("benchmark-only".into(), key)]),
                    Err(_) => Runtime::new(),
                };
                #[cfg(not(test))]
                let runtime = Runtime::new();
                let Ok(runtime) = runtime else {
                    return;
                };
                Worker {
                    stopped: worker_stopped,
                    runtime,
                    sink,
                    instances: HashMap::new(),
                    inbox,
                }
                .run();
            })
            .map_err(|error| format!("could not start the extension executor: {error}"))?;
        Ok(Self {
            jobs,
            stopped,
            documents: Mutex::new(HashMap::new()),
            thread: Some(thread),
        })
    }

    pub fn submit(&self, job: Job) -> bool {
        if self.stopped.load(std::sync::atomic::Ordering::Acquire) {
            return false;
        }
        let mut queues = self.documents.lock().unwrap();
        let (id, generation) = match &job {
            Job::Documents { id, generation, .. }
            | Job::DocumentEvents { id, generation, .. }
            | Job::DocumentSaved { id, generation, .. }
            | Job::WorkspaceFiles { id, generation, .. } => (id.clone(), *generation),
            Job::Load { id, .. } | Job::Stop { id } => {
                queues.retain(|(owner, _), _| owner != id);
                return self.jobs.send(job).is_ok();
            }
            Job::Shutdown => {
                queues.clear();
                return self.jobs.send(job).is_ok();
            }
            _ => return self.jobs.send(job).is_ok(),
        };
        let queue = queues.entry((id.clone(), generation)).or_default().clone();
        let mut pending = queue.lock().unwrap();
        match job {
            Job::Documents {
                changed, closed, ..
            } => pending.documents(changed, closed),
            Job::DocumentEvents {
                references,
                messages,
                ..
            } => pending.events(references, messages),
            Job::DocumentSaved {
                reference, text, ..
            } => pending.saved(reference, text),
            Job::WorkspaceFiles { changes, .. } => pending.files(changes),
            _ => unreachable!(),
        }
        if pending.queued {
            return true;
        }
        pending.queued = true;
        self.jobs
            .send(Job::DocumentBatch {
                id,
                generation,
                queue: queue.clone(),
            })
            .is_ok()
    }

    /// Stop accepting work and wait for the executor to finish its queue.
    pub fn shutdown(&mut self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::Release);
        self.documents.lock().unwrap().clear();
        let _ = self.jobs.send(Job::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Executor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct Slot {
    generation: u64,
    instance: Instance,
    ui: UiMailbox,
    ui_published: std::time::Instant,
    requests: HashMap<u64, (u64, std::time::Instant)>,
    logs: loom_extension_host::PublicationBudget,
}
pub type UiMailbox = Arc<Mutex<Option<Vec<loom_extension_host::UiNode>>>>;
pub type UiEvents = Arc<Mutex<Vec<loom_extension_host::UiEvent>>>;

struct Worker {
    stopped: Arc<std::sync::atomic::AtomicBool>,
    runtime: Runtime,
    sink: Sink,
    instances: HashMap<String, Slot>,
    inbox: Receiver<Job>,
}

pub fn wire_error(error: &ExtensionError) -> HostError {
    wit_error(error)
}

fn wit_error(error: &ExtensionError) -> HostError {
    HostError {
        kind: match error.kind {
            ErrorKind::UnsupportedApi => WitErrorKind::UnsupportedApi,
            ErrorKind::PermissionDenied => WitErrorKind::PermissionDenied,
            ErrorKind::StaleSnapshot => WitErrorKind::StaleSnapshot,
            ErrorKind::ExpiredHandle => WitErrorKind::ExpiredHandle,
            ErrorKind::Cancelled => WitErrorKind::Cancelled,
            ErrorKind::DeadlineExceeded => WitErrorKind::DeadlineExceeded,
            ErrorKind::LimitExceeded => WitErrorKind::LimitExceeded,
            ErrorKind::Conflict => WitErrorKind::Conflict,
            ErrorKind::InvalidEdit => WitErrorKind::InvalidEdit,
            ErrorKind::ExtensionFailed => WitErrorKind::ExtensionFailed,
        },
        message: error.message.clone(),
        committed: error.committed,
    }
}

impl Worker {
    fn run(mut self) {
        loop {
            if self.stopped.load(std::sync::atomic::Ordering::Acquire) {
                break;
            }
            let pending = self.flush_ui();
            let provider_wait = self
                .expire_providers()
                .into_iter()
                .chain(self.poll_timers())
                .min();
            let wait = match (pending, provider_wait) {
                (true, Some(wait)) => Some(wait.min(std::time::Duration::from_millis(16))),
                (true, None) => Some(std::time::Duration::from_millis(16)),
                (false, wait) => wait,
            };
            let job = if let Some(wait) = wait {
                match self.inbox.recv_timeout(wait) {
                    Ok(job) => job,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(_) => break,
                }
            } else {
                match self.inbox.recv() {
                    Ok(job) => job,
                    Err(_) => break,
                }
            };
            match job {
                Job::CancelProvider {
                    id,
                    generation,
                    request,
                } => {
                    if let Some(slot) = self.slot(&id, generation)
                        && let Some((token, _)) = slot.requests.remove(&request)
                    {
                        slot.instance.end_provider(request, token);
                    }
                    self.deliver(&id, generation, vec![Inbound::ProviderCancelled(request)]);
                }
                Job::Documents { .. }
                | Job::DocumentEvents { .. }
                | Job::DocumentSaved { .. }
                | Job::WorkspaceFiles { .. } => unreachable!("document jobs must pass the outbox"),
                Job::DocumentBatch {
                    id,
                    generation,
                    queue,
                } => {
                    let batch = queue.lock().unwrap().drain();
                    if batch.port
                        && let Some(slot) = self.slot(&id, generation)
                    {
                        slot.instance.put_documents(batch.views, &batch.closed);
                    }
                    let mut messages = batch.events;
                    if !batch.files.is_empty() {
                        messages.push(Inbound::WorkspaceFilesChanged(batch.files));
                    }
                    if let Some(slot) = self.slot(&id, generation) {
                        for (reference, text) in batch.saved {
                            match slot.instance.saved_snapshot(reference, text) {
                                Ok(snapshot) => messages.push(Inbound::DocumentSaved(snapshot)),
                                Err(error) => {
                                    slot.instance.state_mut().logs.push((1, error.message))
                                }
                            }
                        }
                    }
                    self.deliver(&id, generation, messages);
                }
                Job::UiEvents {
                    id,
                    generation,
                    events,
                } => {
                    let messages = std::mem::take(&mut *events.lock().unwrap())
                        .into_iter()
                        .map(Inbound::UiEvent)
                        .collect();
                    self.deliver(&id, generation, messages);
                }
                Job::Shutdown => break,
                Job::Load {
                    id,
                    generation,
                    component,
                    package_dir,
                    version,
                    cache,
                    limits,
                    storage,
                    files,
                    domains,
                } => self.load(
                    id,
                    generation,
                    &component,
                    &package_dir,
                    &version,
                    &cache,
                    limits,
                    storage,
                    files,
                    domains,
                ),
                Job::Run {
                    id,
                    generation,
                    run,
                } => {
                    let message = Inbound::RunCommand(CommandContext {
                        call: run.call,
                        command: run.command,
                        snapshot: run.snapshot.as_ref().map(|(token, _)| *token),
                        selections: run
                            .selections
                            .iter()
                            .map(|(range, reversed)| Selection {
                                range: ByteRange {
                                    start: range.start as u64,
                                    end: range.end as u64,
                                },
                                reversed: *reversed,
                            })
                            .collect(),
                        selection_precondition: run.selection_precondition,
                    });
                    if let (Some(slot), Some((token, text))) =
                        (self.slot(&id, generation), run.snapshot)
                    {
                        slot.instance.put_snapshot(token, text);
                    }
                    self.deliver(&id, generation, vec![message]);
                }
                Job::Complete {
                    id,
                    generation,
                    operation,
                    outcome,
                } => {
                    let completion = match outcome {
                        Outcome::Committed {
                            transaction,
                            version,
                        } => Completion::Committed(CommittedReceipt {
                            transaction,
                            version,
                        }),
                        Outcome::Failed(error) => Completion::Failed(wit_error(&error)),
                    };
                    if let Some(slot) = self.slot(&id, generation) {
                        slot.instance.operation_settled();
                    }
                    self.deliver(
                        &id,
                        generation,
                        vec![Inbound::OperationComplete((operation, completion))],
                    );
                }
                Job::Cancel {
                    id,
                    generation,
                    call,
                } => {
                    self.deliver(&id, generation, vec![Inbound::Cancel(call)]);
                }
                Job::Provide {
                    id,
                    generation,
                    job,
                } => {
                    let job = *job;
                    if job.timeout_ms == 0 {
                        (self.sink)(Event::ProviderExpired {
                            id,
                            generation,
                            request: job.request,
                        });
                        continue;
                    }
                    let message = Inbound::ProviderRequest(ProviderRequest {
                        request: job.request,
                        provider: job.provider,
                        kind: job.kind,
                        snapshot: job.token,
                        document: job.document,
                        version: job.version,
                        length: job.text.len() as u64,
                        range: job.range.map(|range| ByteRange {
                            start: range.start as u64,
                            end: range.end as u64,
                        }),
                        cursor: job.cursor.map(|cursor| cursor as u64),
                        trigger: job.trigger,
                        item: job.item,
                        context: job.context,
                    });
                    if let Some(slot) = self.slot(&id, generation) {
                        slot.requests.insert(
                            job.request,
                            (
                                job.token,
                                std::time::Instant::now()
                                    + std::time::Duration::from_millis(job.timeout_ms),
                            ),
                        );
                        slot.instance.put_snapshot(job.token, job.text);
                        if matches!(job.kind, ProviderKind::Save) {
                            slot.instance.begin_save(job.request);
                        }
                    }
                    self.deliver(&id, generation, vec![message]);
                }
                Job::EndSave {
                    id,
                    generation,
                    request,
                } => {
                    if let Some(slot) = self.slot(&id, generation) {
                        if let Some((token, _)) = slot.requests.remove(&request) {
                            slot.instance.end_provider(request, token);
                        } else {
                            slot.instance.end_save(request);
                        }
                    }
                    self.deliver(&id, generation, vec![Inbound::ProviderCancelled(request)]);
                }
                Job::Roots {
                    id,
                    generation,
                    roots,
                    workspace_key,
                } => {
                    let message = Inbound::RootsChanged(
                        roots.iter().map(|(root, _, _)| root.clone()).collect(),
                    );
                    if let Some(slot) = self.slot(&id, generation) {
                        slot.instance.put_root_details(roots, workspace_key);
                    }
                    self.deliver(&id, generation, vec![message]);
                }
                Job::Watch {
                    id,
                    generation,
                    message,
                    credit,
                } => {
                    if let Inbound::WatchFailed(failure) = &message
                        && let Some(slot) = self.slot(&id, generation)
                    {
                        slot.instance.watch_ended(failure.watch);
                    }
                    self.deliver(&id, generation, vec![message]);
                    drop(credit);
                }
                Job::Process {
                    id,
                    generation,
                    message,
                    credit,
                } => {
                    if let Some(slot) = self.slot(&id, generation) {
                        match &message {
                            Inbound::ProcessExited(exit) => {
                                slot.instance.process_ended(exit.process)
                            }
                            Inbound::ProcessWritable(writable) => slot
                                .instance
                                .process_writable(writable.process, writable.bytes as usize),
                            _ => {}
                        }
                    }
                    self.deliver(&id, generation, vec![message]);
                    drop(credit);
                }
                Job::Task {
                    id,
                    generation,
                    message,
                    ended,
                } => {
                    if ended && let Some(slot) = self.slot(&id, generation) {
                        slot.instance.task_ended();
                    }
                    self.deliver(&id, generation, vec![message]);
                }
                Job::Settings {
                    id,
                    generation,
                    values,
                    changed,
                } => {
                    if let Some(slot) = self.slot(&id, generation) {
                        slot.instance.put_settings(values);
                    }
                    if !changed.is_empty() {
                        self.deliver(&id, generation, vec![Inbound::SettingsChanged(changed)]);
                    }
                }
                Job::Deliver {
                    id,
                    generation,
                    messages,
                } => self.deliver(&id, generation, messages),
                Job::Stop { id } => {
                    self.instances.remove(&id);
                }
            }
        }
    }

    fn slot(&mut self, id: &str, generation: u64) -> Option<&mut Slot> {
        self.instances
            .get_mut(id)
            .filter(|slot| slot.generation == generation)
    }
    fn poll_timers(&mut self) -> Option<std::time::Duration> {
        let mut due = Vec::new();
        for (id, slot) in &mut self.instances {
            let (events, _) = slot.instance.poll_timers();
            if !events.is_empty() {
                due.push((id.clone(), slot.generation, events));
            }
        }
        for (id, generation, events) in due {
            self.deliver(&id, generation, events);
        }
        // A callback may have installed an earlier timer during delivery.
        self.instances
            .values()
            .filter_map(|slot| slot.instance.state().timer_wait())
            .min()
    }

    fn expire_providers(&mut self) -> Option<std::time::Duration> {
        let now = std::time::Instant::now();
        let mut due = Vec::new();
        let mut next = None;
        for (id, slot) in &mut self.instances {
            let expired = slot
                .requests
                .iter()
                .filter(|(_, (_, deadline))| *deadline <= now)
                .map(|(request, _)| *request)
                .collect::<Vec<_>>();
            for request in expired {
                let (token, _) = slot.requests.remove(&request).unwrap();
                slot.instance.end_provider(request, token);
                due.push((id.clone(), slot.generation, request));
            }
            for (_, deadline) in slot.requests.values() {
                let wait = deadline.saturating_duration_since(now);
                next = Some(next.map_or(wait, |before: std::time::Duration| before.min(wait)));
            }
        }
        for (id, generation, request) in due {
            (self.sink)(Event::ProviderExpired {
                id: id.clone(),
                generation,
                request,
            });
            self.deliver(&id, generation, vec![Inbound::ProviderCancelled(request)]);
        }
        next
    }
    fn flush_ui(&mut self) -> bool {
        let mut pending = false;
        for (id, slot) in &mut self.instances {
            // A report is a few words: it goes as it is, without the pacing of a tree.
            if let Some(reports) = slot.instance.take_service_reports() {
                (self.sink)(Event::ServiceStatus {
                    id: id.clone(),
                    generation: slot.generation,
                    reports,
                });
            }
            // Lines go as they are too: the panel that shows them paces itself.
            if let Some(changes) = slot.instance.take_output() {
                (self.sink)(Event::Output {
                    id: id.clone(),
                    generation: slot.generation,
                    changes,
                });
            }
            if !slot.instance.ui_dirty() {
                continue;
            }
            if slot.ui_published.elapsed() < std::time::Duration::from_millis(16) {
                pending = true;
                continue;
            }
            if let Some(nodes) = slot.instance.take_ui_snapshot() {
                slot.ui_published = std::time::Instant::now();
                let notify = slot.ui.lock().unwrap().replace(nodes).is_none();
                if notify {
                    (self.sink)(Event::UiChanged {
                        id: id.clone(),
                        generation: slot.generation,
                        snapshot: slot.ui.clone(),
                    });
                }
            }
        }
        pending
    }

    fn load(
        &mut self,
        id: String,
        generation: u64,
        component: &[u8],
        package_dir: &std::path::Path,
        version: &str,
        cache: &std::path::Path,
        limits: Limits,
        storage: Option<PathBuf>,
        files: bool,
        domains: Vec<String>,
    ) {
        // A reload replaces the old instance first, so its callbacks cannot act on the new one.
        self.instances.remove(&id);
        let result = self
            .runtime
            .compile_package(component, cache, package_dir, &id, version)
            .and_then(|loaded| {
                let source = loaded.source;
                self.runtime
                    .instantiate(&loaded.component, limits)
                    .map(|instance| (instance, source, loaded.aot_rejection))
            });
        match result {
            Ok((mut instance, source, rejection)) => {
                // Capabilities are granted by giving the instance the port; without one
                // every call fails with permission-denied.
                if let Some(dir) = storage {
                    instance.set_storage(Box::new(ports::FsStorage::new(dir)));
                }
                if files {
                    instance.set_files(Box::new(ports::FsFiles));
                }
                if !domains.is_empty() {
                    instance.set_http(Box::new(http::BoundedHttp::new(domains)));
                }
                self.instances.insert(
                    id.clone(),
                    Slot {
                        logs: Default::default(),
                        requests: HashMap::new(),
                        generation,
                        instance,
                        ui: Arc::default(),
                        ui_published: std::time::Instant::now()
                            - std::time::Duration::from_millis(16),
                    },
                );
                (self.sink)(Event::Loaded {
                    id,
                    generation,
                    cache_hit: source == loom_extension_host::CodeSource::LocalCache,
                    source,
                    aot_rejection: rejection,
                    result: Ok(()),
                });
            }
            Err(error) => (self.sink)(Event::Loaded {
                id,
                generation,
                cache_hit: false,
                source: loom_extension_host::CodeSource::Compiled,
                aot_rejection: None,
                result: Err(error),
            }),
        }
    }

    /// Deliver one message, then keep running while the guest asks for more rounds.
    fn deliver(&mut self, id: &str, generation: u64, messages: Vec<Inbound>) {
        let mut batch = messages;
        for _ in 0..MAX_CONSECUTIVE_ROUNDS {
            if self.stopped.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            let Some(slot) = self.slot(id, generation) else {
                return;
            };
            let status = slot.instance.dispatch(&batch);
            let accepted = slot.instance.take_accepted();
            let logs = slot
                .instance
                .take_logs()
                .into_iter()
                .filter_map(|(level, text)| {
                    slot.logs
                        .reserve(text.len())
                        .map(|credit| (level, text, credit))
                })
                .collect::<Vec<_>>();
            let provided = slot
                .instance
                .take_provided()
                .into_iter()
                .filter(|answer| {
                    if let Some((token, _)) = slot.requests.remove(&answer.request()) {
                        slot.instance.end_provider(answer.request(), token);
                        true
                    } else {
                        false
                    }
                })
                .collect::<Vec<_>>();
            let started = slot.instance.take_started_tasks();
            let controls = slot.instance.take_process_controls();
            let watches = slot.instance.take_watch_controls();
            let file_ops = slot.instance.take_started_file_ops();
            let cancelled = slot.instance.take_cancelled_tasks();
            let configurations = slot.instance.take_provider_configurations();
            let published = slot.instance.take_published();
            let operations = slot.instance.take_workspace_operations();
            for (operation, request) in operations {
                (self.sink)(Event::WorkspaceOperation {
                    id: id.to_owned(),
                    generation,
                    operation,
                    request,
                });
            }
            if !configurations.is_empty() {
                (self.sink)(Event::ProvidersConfigured {
                    id: id.to_owned(),
                    generation,
                    configurations,
                });
            }
            if !watches.is_empty() {
                (self.sink)(Event::WatchControls {
                    id: id.to_owned(),
                    generation,
                    controls: watches,
                });
            }
            if !controls.is_empty() {
                (self.sink)(Event::ProcessControls {
                    id: id.to_owned(),
                    generation,
                    controls,
                });
            }
            for (level, text, credit) in logs {
                (self.sink)(Event::Log {
                    id: id.to_owned(),
                    level,
                    text,
                    _credit: credit,
                });
            }
            for started in file_ops {
                (self.sink)(Event::FileOpsRequested {
                    id: id.to_owned(),
                    generation,
                    started,
                });
            }
            for task in started {
                (self.sink)(Event::TaskRequested {
                    id: id.to_owned(),
                    generation,
                    task,
                });
            }
            if !cancelled.is_empty() {
                (self.sink)(Event::TasksCancelled {
                    id: id.to_owned(),
                    generation,
                    tasks: cancelled,
                });
            }
            for published in published {
                (self.sink)(Event::DiagnosticsPublished {
                    id: id.to_owned(),
                    generation,
                    published,
                });
            }
            for answer in provided {
                (self.sink)(Event::Provided {
                    id: id.to_owned(),
                    generation,
                    provided: answer,
                });
            }
            for edit in accepted {
                (self.sink)(Event::EditRequested {
                    id: id.to_owned(),
                    generation,
                    edit,
                });
            }
            match status {
                Ok(RoundStatus::Runnable) => batch = Vec::new(),
                Ok(_) => return,
                Err(error) => {
                    // A trap or limit fails the instance; the application reports it
                    // and a retry creates a new generation (design §8).
                    self.instances.remove(id);
                    (self.sink)(Event::Failed {
                        id: id.to_owned(),
                        generation,
                        error,
                    });
                    return;
                }
            }
        }
    }
}
