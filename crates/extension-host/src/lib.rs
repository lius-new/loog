//! Wasmtime component host for Loom extensions (design §7, §8).
//!
//! One `Instance` owns one Store. The host drives the guest in short rounds:
//! `dispatch` delivers a batch and must return; host operations are accepted
//! synchronously and complete in a later round. Epoch deadlines bound every
//! round and `StoreLimits` bound guest memory, so a looping or hoarding guest
//! fails its instance instead of the application.
use loom_extension_api::{ErrorKind, ExtensionError};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder, Trap};

mod bindings {
    wasmtime::component::bindgen!({
        path: "../../wit/loom-extension",
        world: "extension-ui",
        additional_derives: [serde::Serialize],
    });
}

mod completion_batch;
mod documents;
mod processes;
pub use bindings::loom::extension::types::DocumentSnapshot;
pub use documents::DocumentView;
pub use processes::ProcessControl;
mod watches;
pub use bindings::loom::extension::types::{
    FileChange, FileChangeKind, FileChanged, WatchFailure, WatchRequest,
};
pub use watches::WatchControl;
mod publication_budget;
mod ui;
pub use bindings::loom::extension::types::{
    UiDescription, UiEvent, UiEventPayload, UiKind, UiParent,
};
pub use bindings::loom::ui::tree::{
    Placement as UiPlacement, Relation as UiRelation, Target as UiTarget,
};

// COMPAT(base-extension-world): Keep placement outside the original WIT ui-node record
// so existing components retain their ABI. New placement travels through loom:ui/tree.
// Revisit this split only when the base 0.1.0 ABI is retired; no retirement is scheduled.
#[derive(Clone, Debug, serde::Serialize)]
pub struct UiNode {
    pub node: u64,
    pub parent: UiParent,
    pub description: UiDescription,
    pub placement: Option<UiPlacement>,
}
pub use publication_budget::{Budget as PublicationBudget, Credit as PublicationCredit};

pub use bindings::loom::extension::types::ProviderConfiguration;
pub use bindings::loom::extension::types::WorkspaceChange;
pub use bindings::loom::extension::types::WorkspaceFileChange;
pub use bindings::loom::extension::types::WorkspaceOperation;
pub use bindings::loom::extension::types::{
    ApplyEditRequest, ByteRange, CommandContext, CommittedReceipt, Completion, DocumentChange,
    DocumentEdits, DocumentRef, ErrorKind as WitErrorKind, HostError, Inbound, OutputChannel,
    ProcessExit, ProcessFraming, ProcessOutput, ProcessRequest, ProcessWritable, ProviderCommand,
    ProviderCommandCall, ProviderKind, ProviderRequest, RootRef, RoundStatus, Selection, TaskExit,
    TaskOutput, TaskRequest, TextEdit,
};
pub use bindings::loom::extension::types::{
    ColumnUnit, FileLocation, FileRange, HierarchyItem as WireHierarchyItem,
    LanguageAnswer as WireLanguageAnswer, ProviderContext, ProviderMode,
    WorkspaceEdit as WireWorkspaceEdit,
};
pub use bindings::loom::extension::types::{
    SelectionRange as WireSelectionRange, SignatureEntry, SignatureParameter, SignatureSet,
    TextPosition,
};
use bindings::loom::extension::{host, types};

/// Resource limits of one instance.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Total linear memory the guest may hold.
    pub memory_bytes: usize,
    /// Wall-clock budget of one `dispatch` round, in epoch ticks.
    pub round_ticks: u64,
    pub instances: usize,
    pub memories: usize,
    pub tables: usize,
    /// Operations accepted but not yet completed.
    pub pending_operations: usize,
    /// Tasks one instance may have running at once.
    pub tasks: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            memory_bytes: 256 * 1024 * 1024,
            round_ticks: 20,
            instances: 8,
            memories: 4,
            tables: 8,
            pending_operations: 64,
            tasks: 4,
        }
    }
}

/// An operation the guest asked for; the application decides its outcome.
#[derive(Clone, Debug)]
pub struct AcceptedEdit {
    pub operation: u64,
    pub title: String,
    pub call: u64,
    pub sequence: u64,
    pub edit_count: usize,
    pub selection_precondition: Option<u64>,
    pub documents: Vec<DocumentEdits>,
}

pub use bindings::loom::extension::types::CompletionItem as WireCompletionItem;
pub use bindings::loom::extension::types::Diagnostic as WireDiagnostic;
pub use bindings::loom::extension::types::{
    CodeAction as WireCodeAction, DocumentSymbol as WireSymbol, HoverAnswer as WireHover,
    InlayHint as WireInlayHint, Location as WireLocation, SignatureAnswer as WireSignature,
};
pub use bindings::loom::extension::types::{FileContent, HttpResponse, StorageScope};

/// An answer a provider gave, in the order it was given.
#[derive(Clone, Debug, serde::Serialize)]
pub enum Provided {
    Language {
        request: u64,
        answer: WireLanguageAnswer,
    },
    Diagnostics {
        request: u64,
        items: Vec<WireDiagnostic>,
    },
    Edits {
        request: u64,
        edits: Vec<TextEdit>,
    },
    Completions {
        request: u64,
        items: Vec<WireCompletionItem>,
        incomplete: bool,
    },
    Hover {
        request: u64,
        answer: Option<WireHover>,
    },
    Locations {
        request: u64,
        items: Vec<WireLocation>,
    },
    Symbols {
        request: u64,
        items: Vec<WireSymbol>,
    },
    Signature {
        request: u64,
        answer: Option<WireSignature>,
    },
    InlayHints {
        request: u64,
        items: Vec<WireInlayHint>,
    },
    CodeActions {
        request: u64,
        items: Vec<WireCodeAction>,
    },
}

impl Provided {
    /// The request this answers.
    pub fn request(&self) -> u64 {
        match self {
            Self::Language { request, .. }
            | Self::Diagnostics { request, .. }
            | Self::Edits { request, .. }
            | Self::Completions { request, .. }
            | Self::Hover { request, .. }
            | Self::Locations { request, .. }
            | Self::Symbols { request, .. }
            | Self::Signature { request, .. }
            | Self::InlayHints { request, .. }
            | Self::CodeActions { request, .. } => *request,
        }
    }
}

/// Findings a guest pushed for one state of a file.
#[derive(Clone, Debug)]
pub struct PublishedDiagnostics {
    _credit: publication_budget::Credit,
    pub provider: String,
    pub path: String,
    pub version: u64,
    pub items: Vec<WireDiagnostic>,
}

/// A task the guest asked to start; the host numbers it, the application decides.
#[derive(Clone, Debug)]
pub struct StartedTask {
    pub task: u64,
    pub request: TaskRequest,
}

/// Where an extension's stored values live. The host enforces the limits;
/// a port only reads and writes whole scopes.
pub trait StoragePort: Send {
    fn load(&mut self, scope: &str) -> BTreeMap<String, String>;
    fn save(&mut self, scope: &str, values: &BTreeMap<String, String>) -> Result<(), String>;
}

/// Bounded access to files inside a folder. Implementations must keep every
/// path inside `root` after resolving links.
pub trait FilePort: Send {
    fn read(
        &self,
        root: &std::path::Path,
        relative: &str,
        max: usize,
    ) -> Result<(String, bool), ExtensionError>;
    fn find(
        &self,
        root: &std::path::Path,
        pattern: &str,
        limit: usize,
    ) -> Result<Vec<String>, ExtensionError>;
}

/// A bounded HTTP client. The port decides which hosts are reachable.
pub trait HttpPort: Send {
    fn get(&self, url: &str, max: usize) -> Result<HttpResponse, ExtensionError>;
}

/// Limits of one storage scope of one extension.
pub mod storage_limits {
    pub const KEY: usize = 256;
    pub const VALUE: usize = 64 * 1024;
    pub const KEYS: usize = 256;
    pub const TOTAL: usize = 512 * 1024;
}

/// Most bytes one `http-get` returns, whatever was asked.
pub const MAX_HTTP_BODY: usize = 2 * 1024 * 1024;

/// Most bytes one `read-file` returns, whatever was asked.
pub const MAX_READ: usize = 4 * 1024 * 1024;
/// Most paths one `find-files` returns.
pub const MAX_FOUND: usize = 1000;

pub use bindings::loom::extension::types::{CreateOp, DeleteOp, FileOp, FileOpsRequest, RenameOp};

/// File operations the guest asked for; the application decides and runs them.
#[derive(Clone, Debug)]
pub struct StartedFileOps {
    pub operation: u64,
    pub request: FileOpsRequest,
}

/// State behind the host imports.
pub struct HostState {
    clock_origin: std::time::Instant,
    timers: BTreeMap<u64, std::time::Instant>,
    next_timer: u64,
    processes: processes::Processes,
    watches: watches::Watches,
    ui: ui::Tree,
    documents: Option<documents::Documents>,
    limits: StoreLimits,
    pending_limit: usize,
    next_operation: u64,
    snapshots: BTreeMap<u64, Arc<String>>,
    outstanding: usize,
    settings: BTreeMap<String, String>,
    provided: Vec<Provided>,
    provided_bytes: usize,
    provider_configurations: BTreeMap<String, ProviderConfiguration>,
    roots: Vec<RootRef>,
    next_task: u64,
    started_tasks: Vec<StartedTask>,
    started_file_ops: Vec<StartedFileOps>,
    workspace_operations: Vec<(u64, WorkspaceOperation)>,
    workspace_operation_bytes: usize,
    cancelled_tasks: Vec<u64>,
    live_tasks: usize,
    task_limit: usize,
    published: Vec<PublishedDiagnostics>,
    publication_budget: publication_budget::Budget,
    /// Open save-participant requests. While any is open the instance may
    /// read but not cause side effects (design §6.5).
    save_requests: std::collections::BTreeSet<u64>,
    storage: Option<Box<dyn StoragePort>>,
    files: Option<Box<dyn FilePort>>,
    http: Option<Box<dyn HttpPort>>,
    scopes: BTreeMap<String, BTreeMap<String, String>>,
    root_paths: BTreeMap<u64, std::path::PathBuf>,
    root_keys: BTreeMap<u64, String>,
    workspace_key: String,
    pub accepted: Vec<AcceptedEdit>,
    pub cancelled: Vec<u64>,
    pub logs: Vec<(u8, String)>,
}

impl HostState {
    pub fn timer_wait(&self) -> Option<Duration> {
        let now = std::time::Instant::now();
        self.timers
            .values()
            .map(|deadline| deadline.saturating_duration_since(now))
            .min()
    }
    /// Queue an answer for the application, within the unconsumed-answer limit.
    fn push_provided(&mut self, answer: Provided) -> Result<(), HostError> {
        if self.provided.len() >= self.pending_limit {
            return Err(host_error(
                ErrorKind::LimitExceeded,
                "too many unconsumed provider answers",
            ));
        }
        // Count a closed serialization without allocating it. This includes
        // empty records and metadata but avoids Debug's per-character formatting
        // on large interactive answers. Stop at the same shared byte budget.
        struct Counter {
            bytes: usize,
            remaining: usize,
        }
        impl std::io::Write for Counter {
            fn write(&mut self, value: &[u8]) -> std::io::Result<usize> {
                if value.len() > self.remaining {
                    return Err(std::io::Error::other("provider answer budget exceeded"));
                }
                self.bytes += value.len();
                self.remaining -= value.len();
                Ok(value.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut count = Counter {
            bytes: 0,
            remaining: (32 * 1024 * 1024usize).saturating_sub(self.provided_bytes),
        };
        if serde_json::to_writer(&mut count, &answer).is_err() {
            return Err(host_error(
                ErrorKind::LimitExceeded,
                "unconsumed provider answer budget exceeded",
            ));
        }
        self.provided_bytes += count.bytes;
        self.provided.push(answer);
        Ok(())
    }

    /// Refuse an import that causes a side effect while a save participant runs.
    fn unrestricted(&self, what: &str) -> Result<(), HostError> {
        if self.save_requests.is_empty() {
            Ok(())
        } else {
            Err(host_error(
                ErrorKind::PermissionDenied,
                &format!("{what} is not allowed while a save participant runs"),
            ))
        }
    }

    fn scope_key(&self, scope: &StorageScope) -> Result<String, HostError> {
        match scope {
            StorageScope::User => Ok("user".to_owned()),
            StorageScope::Workspace => Ok(format!("workspace-{}", self.workspace_key)),
            StorageScope::Root(id) => self
                .root_keys
                .get(id)
                .map(|key| format!("root-{key}"))
                .ok_or_else(|| {
                    host_error(ErrorKind::ExpiredHandle, "the folder is no longer open")
                }),
        }
    }

    /// The values of a scope, read from the port once. `None` when this
    /// extension has no storage port (not granted).
    fn scope_values(&mut self, scope: &str) -> Option<&BTreeMap<String, String>> {
        let port = self.storage.as_mut()?;
        if !self.scopes.contains_key(scope) {
            let loaded = port.load(scope);
            self.scopes.insert(scope.to_owned(), loaded);
        }
        self.scopes.get(scope)
    }

    fn write_scope(
        &mut self,
        scope: &str,
        next: BTreeMap<String, String>,
    ) -> Result<(), HostError> {
        let port = self.storage.as_mut().ok_or_else(|| {
            host_error(
                ErrorKind::PermissionDenied,
                "storage is not granted to this extension",
            )
        })?;
        port.save(scope, &next)
            .map_err(|message| host_error(ErrorKind::ExtensionFailed, &message))?;
        self.scopes.insert(scope.to_owned(), next);
        Ok(())
    }

    fn file_access(&self, root: u64) -> Result<(&dyn FilePort, std::path::PathBuf), HostError> {
        let port = self.files.as_deref().ok_or_else(|| {
            host_error(
                ErrorKind::PermissionDenied,
                "file access is not granted to this extension",
            )
        })?;
        let path = self
            .root_paths
            .get(&root)
            .ok_or_else(|| host_error(ErrorKind::ExpiredHandle, "the folder is no longer open"))?;
        Ok((port, path.clone()))
    }

    fn new(limits: &Limits) -> Self {
        Self {
            clock_origin: std::time::Instant::now(),
            timers: BTreeMap::new(),
            next_timer: 1,
            limits: StoreLimitsBuilder::new()
                .memory_size(limits.memory_bytes)
                .instances(limits.instances)
                .memories(limits.memories)
                .tables(limits.tables)
                .trap_on_grow_failure(true)
                .build(),
            pending_limit: limits.pending_operations,
            next_operation: 1,
            snapshots: BTreeMap::new(),
            outstanding: 0,
            settings: BTreeMap::new(),
            provided: Vec::new(),
            provided_bytes: 0,
            provider_configurations: BTreeMap::new(),
            roots: Vec::new(),
            next_task: 1,
            processes: Default::default(),
            watches: Default::default(),
            ui: Default::default(),
            documents: None,
            started_tasks: Vec::new(),
            started_file_ops: Vec::new(),
            workspace_operations: Vec::new(),
            workspace_operation_bytes: 0,
            cancelled_tasks: Vec::new(),
            live_tasks: 0,
            task_limit: limits.tasks,
            published: Vec::new(),
            publication_budget: Default::default(),
            save_requests: std::collections::BTreeSet::new(),
            storage: None,
            files: None,
            http: None,
            scopes: BTreeMap::new(),
            root_paths: BTreeMap::new(),
            root_keys: BTreeMap::new(),
            workspace_key: String::new(),
            accepted: Vec::new(),
            cancelled: Vec::new(),
            logs: Vec::new(),
        }
    }
}

fn host_error(kind: ErrorKind, message: &str) -> HostError {
    HostError {
        kind: wit_kind(kind),
        message: message.to_owned(),
        committed: false,
    }
}

fn validate_launch(
    program: &str,
    args: &[String],
    cwd: Option<&str>,
    env: &[(String, String)],
) -> Result<(), HostError> {
    if program.is_empty()
        || program.len() > 1024
        || program.contains('\0')
        || args.len() > 128
        || args
            .iter()
            .any(|arg| arg.len() > 8192 || arg.contains('\0'))
        || cwd.is_some_and(|cwd| cwd.len() > 4096 || cwd.contains('\0'))
        || env.len() > 32
        || env.iter().any(|(key, value)| {
            key.len() > 64 || key.contains('\0') || value.len() > 1024 || value.contains('\0')
        })
    {
        return Err(host_error(
            ErrorKind::LimitExceeded,
            "process launch fields exceed their budget",
        ));
    }
    Ok(())
}

fn wit_kind(kind: ErrorKind) -> WitErrorKind {
    match kind {
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
    }
}

impl types::Host for HostState {}

impl bindings::loom::ui::tree::Host for HostState {
    fn place(&mut self, node: u64, placement: UiPlacement) -> Result<(), String> {
        self.unrestricted("placing UI nodes")
            .map_err(|error| error.message)?;
        self.ui
            .place(node, placement)
            .map_err(|error| error.message)
    }
}

impl host::Host for HostState {
    fn monotonic_nanoseconds(&mut self) -> u64 {
        self.clock_origin.elapsed().as_nanos().min(u64::MAX as u128) as u64
    }
    fn timer_start(&mut self, milliseconds: u32) -> Result<u64, HostError> {
        if self.timers.len() >= 64 || milliseconds > 86_400_000 {
            return Err(host_error(
                ErrorKind::LimitExceeded,
                "timer budget exceeded",
            ));
        }
        let timer = self.next_timer;
        self.next_timer += 1;
        self.timers.insert(
            timer,
            std::time::Instant::now() + Duration::from_millis(milliseconds as u64),
        );
        Ok(timer)
    }

    fn timer_cancel(&mut self, timer: u64) {
        self.timers.remove(&timer);
    }
    fn perform_workspace_operation(
        &mut self,
        request: WorkspaceOperation,
    ) -> Result<u64, HostError> {
        self.unrestricted("workspace operations")?;
        if self.outstanding >= self.pending_limit {
            return Err(host_error(
                ErrorKind::LimitExceeded,
                "too many pending operations",
            ));
        }
        // Limit recursively nested strings and edits before retaining the request.
        let size = match &request {
            WorkspaceOperation::ShowDocument(location) => location.path.len(),
            WorkspaceOperation::Edit(request) => {
                if request.title.len() > 4096 || request.edit.changes.len() > 1024 {
                    return Err(host_error(
                        ErrorKind::LimitExceeded,
                        "workspace edit budget exceeded",
                    ));
                }
                request.title.len()
                    + request
                        .edit
                        .changes
                        .iter()
                        .map(|change| match change {
                            WorkspaceChange::Text(d) => {
                                d.path.len()
                                    + d.edits
                                        .iter()
                                        .map(|e| e.text.len().saturating_add(128))
                                        .sum::<usize>()
                            }
                            WorkspaceChange::Create(op) => op.path.len(),
                            WorkspaceChange::Rename(op) => op.source.len() + op.target.len(),
                            WorkspaceChange::Delete(op) => op.path.len(),
                        })
                        .sum::<usize>()
            }
        };
        if size.saturating_add(self.workspace_operation_bytes) > 8 * 1024 * 1024
            || self
                .workspace_operations
                .iter()
                .map(|(_, op)| match op {
                    WorkspaceOperation::Edit(r) => r.edit.changes.len(),
                    _ => 1,
                })
                .sum::<usize>()
                > 1024
        {
            return Err(host_error(
                ErrorKind::LimitExceeded,
                "workspace operation budget exceeded",
            ));
        }
        self.workspace_operation_bytes += size;
        let operation = self.next_operation;
        self.next_operation += 1;
        self.outstanding += 1;
        self.workspace_operations.push((operation, request));
        Ok(operation)
    }
    fn configure_provider(
        &mut self,
        configuration: ProviderConfiguration,
    ) -> Result<(), HostError> {
        self.unrestricted("configuring providers")?;
        if configuration.provider.len() > 128
            || configuration
                .modes
                .as_ref()
                .is_some_and(|modes| modes.len() > 16)
            || configuration.provider.is_empty()
            || configuration.triggers.len() > 64
            || configuration.retriggers.len() > 64
            || configuration.commands.len() > 256
            || configuration.aliases.len() > 32
            || configuration
                .triggers
                .iter()
                .chain(&configuration.retriggers)
                .any(|s| s.is_empty() || s.len() > 32)
            || configuration
                .commands
                .iter()
                .chain(&configuration.aliases)
                .any(|s| s.is_empty() || s.len() > 256)
            || (!self
                .provider_configurations
                .contains_key(&configuration.provider)
                && self.provider_configurations.len() >= 64)
        {
            return Err(host_error(
                ErrorKind::LimitExceeded,
                "provider configuration budget exceeded",
            ));
        }
        self.provider_configurations
            .insert(configuration.provider.clone(), configuration);
        Ok(())
    }
    fn provide_language(
        &mut self,
        request: u64,
        answer: WireLanguageAnswer,
    ) -> Result<(), HostError> {
        self.push_provided(Provided::Language { request, answer })
    }
    fn capture_document(&mut self, document: u64) -> Result<DocumentSnapshot, HostError> {
        let documents = self.documents.as_mut().ok_or_else(|| {
            host_error(
                ErrorKind::PermissionDenied,
                "document reading is not granted",
            )
        })?;
        let (snapshot, text) = documents.capture(document)?;
        self.snapshots.insert(snapshot.snapshot, text);
        Ok(snapshot)
    }
    fn ui_create(
        &mut self,
        parent: UiParent,
        description: UiDescription,
    ) -> Result<u64, HostError> {
        self.unrestricted("creating UI nodes")?;
        self.ui.create(parent, description)
    }
    fn ui_update(&mut self, node: u64, description: UiDescription) -> Result<(), HostError> {
        self.unrestricted("updating UI nodes")?;
        self.ui.update(node, description)
    }
    fn ui_remove(&mut self, node: u64) -> Result<(), HostError> {
        self.unrestricted("removing UI nodes")?;
        self.ui.remove(node)
    }
    fn watch_files(&mut self, request: WatchRequest) -> Result<u64, HostError> {
        self.unrestricted("registering a file watch")?;
        self.watches.start(request)
    }
    fn unwatch_files(&mut self, watch: u64) {
        self.watches.stop(watch);
    }
    fn process_spawn(&mut self, request: ProcessRequest) -> Result<u64, HostError> {
        self.unrestricted("starting a process")?;
        validate_launch(
            &request.program,
            &request.args,
            request.cwd.as_deref(),
            &request.env,
        )?;
        self.processes.spawn(request, self.task_limit)
    }
    fn process_write(&mut self, process: u64, bytes: Vec<u8>) -> Result<u32, HostError> {
        self.unrestricted("writing a process")?;
        self.processes.write(process, bytes)
    }
    fn process_close_stdin(&mut self, process: u64) -> Result<(), HostError> {
        self.unrestricted("closing process input")?;
        self.processes.close(process)
    }
    fn process_write_text(&mut self, process: u64, text: String) -> Result<u32, HostError> {
        self.process_write(process, text.into_bytes())
    }
    fn process_kill(&mut self, process: u64) {
        self.processes.kill(process);
    }
    fn read_range(&mut self, snapshot: u64, range: ByteRange) -> Result<String, HostError> {
        let text = self
            .snapshots
            .get(&snapshot)
            .ok_or_else(|| host_error(ErrorKind::ExpiredHandle, "unknown snapshot"))?;
        let (start, end) = (range.start as usize, range.end as usize);
        if start > end
            || end > text.len()
            || !text.is_char_boundary(start)
            || !text.is_char_boundary(end)
        {
            return Err(host_error(
                ErrorKind::InvalidEdit,
                "range is outside the snapshot",
            ));
        }
        Ok(text[start..end].to_owned())
    }

    fn release_snapshot(&mut self, snapshot: u64) {
        if let Some(documents) = &mut self.documents {
            documents.release(snapshot);
        }
        self.snapshots.remove(&snapshot);
    }

    fn apply_edit(&mut self, request: ApplyEditRequest) -> Result<u64, HostError> {
        self.unrestricted("applying an edit")?;
        if self.outstanding >= self.pending_limit {
            return Err(host_error(
                ErrorKind::LimitExceeded,
                "too many pending operations",
            ));
        }
        let operation = self.next_operation;
        self.next_operation += 1;
        self.outstanding += 1;
        self.accepted.push(AcceptedEdit {
            operation,
            title: request.title,
            call: request.call,
            sequence: request.sequence,
            edit_count: request
                .documents
                .iter()
                .map(|document| document.edits.len())
                .sum(),
            selection_precondition: request.selection_precondition,
            documents: request.documents,
        });
        Ok(operation)
    }

    fn provide_diagnostics(
        &mut self,
        request: u64,
        items: Vec<WireDiagnostic>,
    ) -> Result<(), HostError> {
        self.push_provided(Provided::Diagnostics { request, items })
    }

    fn provide_completions(
        &mut self,
        request: u64,
        items: Vec<WireCompletionItem>,
        incomplete: bool,
    ) -> Result<(), HostError> {
        self.push_provided(Provided::Completions {
            request,
            items,
            incomplete,
        })
    }

    fn provide_completion_batch(
        &mut self,
        request: u64,
        items: Vec<u8>,
        incomplete: bool,
    ) -> Result<(), HostError> {
        let json = std::str::from_utf8(&items)
            .map_err(|_| host_error(ErrorKind::InvalidEdit, "completion batch is not UTF-8"))?;
        self.provide_completions(request, completion_batch::decode(json)?, incomplete)
    }

    fn provide_hover(&mut self, request: u64, answer: Option<WireHover>) -> Result<(), HostError> {
        self.push_provided(Provided::Hover { request, answer })
    }

    fn provide_locations(
        &mut self,
        request: u64,
        items: Vec<WireLocation>,
    ) -> Result<(), HostError> {
        self.push_provided(Provided::Locations { request, items })
    }

    fn provide_symbols(&mut self, request: u64, items: Vec<WireSymbol>) -> Result<(), HostError> {
        self.push_provided(Provided::Symbols { request, items })
    }

    fn provide_signature(
        &mut self,
        request: u64,
        answer: Option<WireSignature>,
    ) -> Result<(), HostError> {
        self.push_provided(Provided::Signature { request, answer })
    }

    fn provide_inlay_hints(
        &mut self,
        request: u64,
        items: Vec<WireInlayHint>,
    ) -> Result<(), HostError> {
        self.push_provided(Provided::InlayHints { request, items })
    }

    fn provide_code_actions(
        &mut self,
        request: u64,
        items: Vec<WireCodeAction>,
    ) -> Result<(), HostError> {
        self.push_provided(Provided::CodeActions { request, items })
    }

    fn provide_edits(&mut self, request: u64, edits: Vec<TextEdit>) -> Result<(), HostError> {
        self.push_provided(Provided::Edits { request, edits })?;
        // Answering ends the restriction of a save request.
        self.save_requests.remove(&request);
        Ok(())
    }

    fn storage_get(&mut self, scope: StorageScope, key: String) -> Option<String> {
        let scope = self.scope_key(&scope).ok()?;
        self.scope_values(&scope)?.get(&key).cloned()
    }

    fn storage_set(
        &mut self,
        scope: StorageScope,
        key: String,
        value: String,
    ) -> Result<(), HostError> {
        self.unrestricted("storing a value")?;
        let scope = self.scope_key(&scope)?;
        if key.is_empty() || key.len() > storage_limits::KEY {
            return Err(host_error(
                ErrorKind::LimitExceeded,
                "the key is empty or too long",
            ));
        }
        if value.len() > storage_limits::VALUE {
            return Err(host_error(
                ErrorKind::LimitExceeded,
                "the value is too large",
            ));
        }
        let Some(values) = self.scope_values(&scope) else {
            return Err(host_error(
                ErrorKind::PermissionDenied,
                "storage is not granted to this extension",
            ));
        };
        let mut next = values.clone();
        next.insert(key, value);
        let total: usize = next.iter().map(|(k, v)| k.len() + v.len()).sum();
        if next.len() > storage_limits::KEYS || total > storage_limits::TOTAL {
            return Err(host_error(
                ErrorKind::LimitExceeded,
                "this storage scope is full",
            ));
        }
        self.write_scope(&scope, next)
    }

    fn storage_delete(&mut self, scope: StorageScope, key: String) -> Result<(), HostError> {
        self.unrestricted("deleting a value")?;
        let scope = self.scope_key(&scope)?;
        let Some(values) = self.scope_values(&scope) else {
            return Err(host_error(
                ErrorKind::PermissionDenied,
                "storage is not granted to this extension",
            ));
        };
        let mut next = values.clone();
        if next.remove(&key).is_none() {
            return Ok(());
        }
        self.write_scope(&scope, next)
    }

    fn http_get(&mut self, url: String, max_bytes: u32) -> Result<HttpResponse, HostError> {
        self.unrestricted("a network request")?;
        let port = self.http.as_deref().ok_or_else(|| {
            host_error(
                ErrorKind::PermissionDenied,
                "network access is not granted to this extension",
            )
        })?;
        port.get(&url, (max_bytes as usize).min(MAX_HTTP_BODY))
            .map_err(|error| host_error(error.kind, &error.message))
    }

    fn read_file(
        &mut self,
        root: u64,
        path: String,
        max_bytes: u32,
    ) -> Result<FileContent, HostError> {
        let (port, root) = self.file_access(root)?;
        let max = (max_bytes as usize).min(MAX_READ);
        port.read(&root, &path, max)
            .map(|(text, truncated)| FileContent { text, truncated })
            .map_err(|error| host_error(error.kind, &error.message))
    }

    fn find_files(
        &mut self,
        root: u64,
        pattern: String,
        limit: u32,
    ) -> Result<Vec<String>, HostError> {
        let (port, root) = self.file_access(root)?;
        port.find(&root, &pattern, (limit as usize).min(MAX_FOUND))
            .map_err(|error| host_error(error.kind, &error.message))
    }

    fn apply_file_ops(&mut self, request: FileOpsRequest) -> Result<u64, HostError> {
        self.unrestricted("changing files")?;
        if self.outstanding >= self.pending_limit {
            return Err(host_error(
                ErrorKind::LimitExceeded,
                "too many pending operations",
            ));
        }
        let operation = self.next_operation;
        self.next_operation += 1;
        self.outstanding += 1;
        self.started_file_ops
            .push(StartedFileOps { operation, request });
        Ok(operation)
    }

    fn list_roots(&mut self) -> Vec<RootRef> {
        self.roots.clone()
    }

    fn task_start(&mut self, request: TaskRequest) -> Result<u64, HostError> {
        self.unrestricted("starting a task")?;
        validate_launch(
            &request.program,
            &request.args,
            request.cwd.as_deref(),
            &request.env,
        )?;
        if self.live_tasks >= self.task_limit {
            return Err(host_error(
                ErrorKind::LimitExceeded,
                "too many running tasks",
            ));
        }
        let task = self.next_task;
        self.next_task += 1;
        self.live_tasks += 1;
        self.started_tasks.push(StartedTask { task, request });
        Ok(task)
    }

    fn task_cancel(&mut self, task: u64) {
        self.cancelled_tasks.push(task);
    }

    fn publish_diagnostics(
        &mut self,
        provider: String,
        path: String,
        version: u64,
        items: Vec<WireDiagnostic>,
    ) -> Result<(), HostError> {
        self.unrestricted("publishing diagnostics")?;
        if self.published.len() >= self.pending_limit {
            return Err(host_error(
                ErrorKind::LimitExceeded,
                "too many unconsumed diagnostics",
            ));
        }
        if provider.len() > 128 || path.len() > 32768 {
            return Err(host_error(
                ErrorKind::LimitExceeded,
                "diagnostic identity budget exceeded",
            ));
        }
        let bytes = items
            .iter()
            .fold(provider.len().saturating_add(path.len()), |bytes, item| {
                bytes
                    .saturating_add(std::mem::size_of_val(item))
                    .saturating_add(item.message.len())
                    .saturating_add(item.source.as_ref().map_or(0, String::len))
                    .saturating_add(item.code.as_ref().map_or(0, String::len))
            });
        let credit = self.publication_budget.reserve(bytes).ok_or_else(|| {
            host_error(
                ErrorKind::LimitExceeded,
                "unconsumed diagnostic publication budget exceeded",
            )
        })?;
        self.published.push(PublishedDiagnostics {
            _credit: credit,
            provider,
            path,
            version,
            items,
        });
        Ok(())
    }

    fn get_setting(&mut self, key: String) -> Option<String> {
        self.settings.get(&key).cloned()
    }

    fn cancel(&mut self, operation: u64) {
        self.cancelled.push(operation);
    }

    fn log(&mut self, level: u8, message: String) {
        if self.logs.len() < 256 {
            let mut end = message.len().min(4096);
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            self.logs.push((level, message[..end].to_owned()));
        }
    }
}

/// Shared engine plus the thread that advances its epoch.
pub struct Runtime {
    engine: Engine,
    stop: Arc<AtomicBool>,
    ticker: Option<JoinHandle<()>>,
}

/// Loom interfaces a component may import. An import of another version of a
/// Loom interface is a structural mismatch and is refused before instantiation
/// (design §7.4); only toolchain WASI imports are stubbed.
pub const KNOWN_IMPORTS: &[&str] = &[
    "loom:extension/types@0.1.0",
    "loom:extension/host@0.1.0",
    "loom:ui/tree@0.1.0",
];

/// Length of one epoch tick.
pub const TICK: Duration = Duration::from_millis(10);

impl Runtime {
    pub fn new() -> Result<Self, ExtensionError> {
        let mut config = Config::new();
        config.epoch_interruption(true);
        let engine = Engine::new(&config).map_err(|error| failed(&error))?;
        let stop = Arc::new(AtomicBool::new(false));
        let ticker = {
            let (engine, stop) = (engine.clone(), stop.clone());
            std::thread::Builder::new()
                .name("loom-extension-epoch".into())
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        std::thread::sleep(TICK);
                        engine.increment_epoch();
                    }
                })
                .map_err(|error| {
                    ExtensionError::new(ErrorKind::ExtensionFailed, error.to_string())
                })?
        };
        Ok(Self {
            engine,
            stop,
            ticker: Some(ticker),
        })
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Compile a component (binary or WAT text) and check it against the world.
    pub fn compile(&self, bytes: &[u8]) -> Result<Component, ExtensionError> {
        Component::new(&self.engine, bytes).map_err(|error| failed(&error))
    }

    /// Compile through an on-disk cache of precompiled code.
    ///
    /// Large components (a JavaScript engine is ~12 MiB) take seconds to
    /// compile cold, so the compiled form is kept next to the package and only
    /// deserialized afterwards. The file name covers the component bytes and
    /// the engine's compatibility hash, so a different Wasmtime or engine
    /// configuration never loads stale code. Returns whether the cache hit.
    pub fn compile_cached(
        &self,
        bytes: &[u8],
        directory: &std::path::Path,
    ) -> Result<(Component, bool), ExtensionError> {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut hasher);
        self.engine
            .precompile_compatibility_hash()
            .hash(&mut hasher);
        let file = directory.join(format!("{:016x}.cwasm", hasher.finish()));
        if file.is_file() {
            // SAFETY: the file was produced by `precompile_component` of an
            // engine with the same compatibility hash (part of the name) and
            // lives in a directory only the user can write to.
            if let Ok(component) = unsafe { Component::deserialize_file(&self.engine, &file) } {
                return Ok((component, true));
            }
            let _ = std::fs::remove_file(&file);
        }
        let compiled = self
            .engine
            .precompile_component(bytes)
            .map_err(|error| failed(&error))?;
        std::fs::create_dir_all(directory)
            .and_then(|_| {
                // Unique per writer, so concurrent activations never share a temp file.
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let partial = file.with_extension(format!(
                    "{}-{}.partial",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                std::fs::write(&partial, &compiled)?;
                match std::fs::rename(&partial, &file) {
                    Ok(()) => Ok(()),
                    // Another activation finished first (Windows cannot replace a mapped file).
                    Err(_) if file.is_file() => std::fs::remove_file(&partial),
                    Err(error) => Err(error),
                }
            })
            .map_err(|error| ExtensionError::new(ErrorKind::ExtensionFailed, error.to_string()))?;
        // SAFETY: as above, just written by this process.
        let component = unsafe { Component::deserialize(&self.engine, &compiled) }
            .map_err(|error| failed(&error))?;
        Ok((component, false))
    }

    pub fn instantiate(
        &self,
        component: &Component,
        limits: Limits,
    ) -> Result<Instance, ExtensionError> {
        // Inspect the real imports (design §9): anything that is neither the Loom
        // contract nor a WASI interface is an unsupported API, rejected before
        // instantiation. Toolchain-required WASI imports are linked as denied
        // stubs that trap if they are ever called.
        for (name, _) in component.component_type().imports(&self.engine) {
            let known = KNOWN_IMPORTS.contains(&name);
            if !known && !name.starts_with("wasi:") {
                return Err(ExtensionError::new(
                    ErrorKind::UnsupportedApi,
                    format!(
                        "unsupported import `{name}`; this host provides {}",
                        KNOWN_IMPORTS.join(", ")
                    ),
                ));
            }
        }
        let mut linker = Linker::<HostState>::new(&self.engine);
        // COMPAT(base-extension-world): Register both interfaces; existing components
        // may import only the base host. New SDK components also import loom:ui/tree.
        // Require the new import only after base-only components lose support;
        // no removal version has been scheduled.
        bindings::ExtensionUi::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state)
            .map_err(|error| failed(&error))?;
        linker
            .define_unknown_imports_as_traps(component)
            .map_err(|error| failed(&error))?;
        let mut store = Store::new(&self.engine, HostState::new(&limits));
        store.limiter(|state| &mut state.limits);
        store.set_epoch_deadline(limits.round_ticks);
        let bindings = bindings::ExtensionUi::instantiate(&mut store, component, &linker)
            .map_err(|error| failed(&error))?;
        Ok(Instance {
            store,
            bindings,
            limits,
        })
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(ticker) = self.ticker.take() {
            let _ = ticker.join();
        }
    }
}

fn failed(error: &wasmtime::Error) -> ExtensionError {
    ExtensionError::new(ErrorKind::ExtensionFailed, format!("{error:#}"))
}

/// Map a trap from a round to the error category the application reports.
fn classify(error: wasmtime::Error) -> ExtensionError {
    match error.downcast_ref::<Trap>() {
        Some(Trap::Interrupt) => ExtensionError::new(
            ErrorKind::DeadlineExceeded,
            "the round used its whole time slice",
        ),
        Some(Trap::OutOfFuel) => {
            ExtensionError::new(ErrorKind::DeadlineExceeded, "the round ran out of fuel")
        }
        _ => {
            let text = format!("{error:#}");
            let kind = if text.contains("forcing a memory growth failure")
                || text.contains("exceeds memory limits")
            {
                ErrorKind::LimitExceeded
            } else {
                ErrorKind::ExtensionFailed
            };
            ExtensionError::new(kind, text)
        }
    }
}

/// Outcome of a round.
pub type RoundResult = Result<RoundStatus, ExtensionError>;

pub struct Instance {
    store: Store<HostState>,
    bindings: bindings::ExtensionUi,
    limits: Limits,
}

impl Instance {
    /// Remove due timers before dispatch so callbacks may schedule replacements.
    pub fn poll_timers(&mut self) -> (Vec<Inbound>, Option<Duration>) {
        let now = std::time::Instant::now();
        let state = self.store.data_mut();
        let due: Vec<_> = state
            .timers
            .iter()
            .filter(|(_, deadline)| **deadline <= now)
            .map(|(id, _)| *id)
            .collect();
        for id in &due {
            state.timers.remove(id);
        }
        let wait = state
            .timers
            .values()
            .map(|deadline| deadline.saturating_duration_since(now))
            .min();
        (due.into_iter().map(Inbound::TimerElapsed).collect(), wait)
    }
    /// Deliver `batch` and advance the guest by one bounded round.
    ///
    /// A trap fails the instance; the caller must discard it (no automatic
    /// resume), keeping committed operations as the real receipts.
    pub fn dispatch(&mut self, batch: &[Inbound]) -> RoundResult {
        self.store.data_mut().ui.begin_round();
        self.store.set_epoch_deadline(self.limits.round_ticks);
        let result = self
            .bindings
            .loom_extension_guest()
            .call_dispatch(&mut self.store, batch);
        match result {
            Ok(Ok(status)) => Ok(status),
            Ok(Err(error)) => Err(ExtensionError::new(
                ErrorKind::ExtensionFailed,
                error.message,
            )),
            Err(trap) => Err(classify(trap)),
        }
    }

    pub fn state(&self) -> &HostState {
        self.store.data()
    }

    pub fn state_mut(&mut self) -> &mut HostState {
        self.store.data_mut()
    }

    /// Provide snapshot text readable through `read-range`.
    pub fn put_snapshot(&mut self, token: u64, text: impl Into<Arc<String>>) {
        self.store.data_mut().snapshots.insert(token, text.into());
    }
    pub fn end_provider(&mut self, request: u64, token: u64) {
        self.store.data_mut().save_requests.remove(&request);
        self.store.data_mut().snapshots.remove(&token);
    }
    pub fn saved_snapshot(
        &mut self,
        reference: DocumentRef,
        text: Arc<String>,
    ) -> Result<DocumentSnapshot, HostError> {
        let state = self.store.data_mut();
        let documents = state.documents.as_mut().ok_or_else(|| {
            host_error(
                ErrorKind::PermissionDenied,
                "document reading is not granted",
            )
        })?;
        let (snapshot, text) = documents.capture_text(reference, text)?;
        state.snapshots.insert(snapshot.snapshot, text);
        Ok(snapshot)
    }
    /// Installed only for instances authorized to read documents. `None` text
    /// keeps metadata while enforcing the host's large-document policy.
    pub fn put_documents(&mut self, changed: Vec<DocumentView>, closed: &[u64]) {
        let documents = self
            .store
            .data_mut()
            .documents
            .get_or_insert_with(Default::default);
        documents.update(changed, closed);
    }

    /// A save-participant request is about to be delivered: restrict side effects.
    pub fn begin_save(&mut self, request: u64) {
        self.store.data_mut().save_requests.insert(request);
    }

    /// The chain ended without an answer (timeout, failure): lift the restriction.
    pub fn end_save(&mut self, request: u64) {
        self.store.data_mut().save_requests.remove(&request);
    }

    /// Replace the resolved setting values the guest can read.
    pub fn put_settings(&mut self, values: BTreeMap<String, String>) {
        self.store.data_mut().settings = values;
    }

    /// The folders the guest can name.
    pub fn put_roots(&mut self, roots: Vec<RootRef>) {
        self.store.data_mut().roots = roots;
    }

    /// The folders with their paths and stable storage keys, and the key of
    /// the whole set of folders. Cached storage of folders that went away is dropped.
    pub fn put_root_details(
        &mut self,
        roots: Vec<(RootRef, std::path::PathBuf, String)>,
        workspace_key: String,
    ) {
        let state = self.store.data_mut();
        state.root_paths = roots
            .iter()
            .map(|(r, path, _)| (r.id, path.clone()))
            .collect();
        state.root_keys = roots
            .iter()
            .map(|(r, _, key)| (r.id, key.clone()))
            .collect();
        state.roots = roots.into_iter().map(|(r, ..)| r).collect();
        state.workspace_key = workspace_key;
    }

    /// Give the guest persistent storage (a granted capability).
    pub fn set_storage(&mut self, port: Box<dyn StoragePort>) {
        self.store.data_mut().storage = Some(port);
    }

    /// Give the guest bounded network access (a granted capability).
    pub fn set_http(&mut self, port: Box<dyn HttpPort>) {
        self.store.data_mut().http = Some(port);
    }

    /// Give the guest bounded file access (a granted capability).
    pub fn set_files(&mut self, port: Box<dyn FilePort>) {
        self.store.data_mut().files = Some(port);
    }

    pub fn take_started_file_ops(&mut self) -> Vec<StartedFileOps> {
        std::mem::take(&mut self.store.data_mut().started_file_ops)
    }

    pub fn take_started_tasks(&mut self) -> Vec<StartedTask> {
        std::mem::take(&mut self.store.data_mut().started_tasks)
    }

    pub fn take_process_controls(&mut self) -> Vec<ProcessControl> {
        self.store.data_mut().processes.take()
    }
    pub fn take_watch_controls(&mut self) -> Vec<WatchControl> {
        self.store.data_mut().watches.take()
    }
    pub fn take_ui_snapshot(&mut self) -> Option<Vec<UiNode>> {
        self.store.data_mut().ui.take()
    }
    pub fn ui_dirty(&self) -> bool {
        self.store.data().ui.dirty()
    }
    pub fn watch_ended(&mut self, watch: u64) {
        self.store.data_mut().watches.ended(watch);
    }
    pub fn process_ended(&mut self, process: u64) {
        self.store.data_mut().processes.ended(process);
    }
    pub fn process_writable(&mut self, process: u64, bytes: usize) {
        self.store.data_mut().processes.writable(process, bytes);
    }

    pub fn take_cancelled_tasks(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.store.data_mut().cancelled_tasks)
    }

    pub fn take_published(&mut self) -> Vec<PublishedDiagnostics> {
        std::mem::take(&mut self.store.data_mut().published)
    }

    /// A task ended: its slot is free again.
    pub fn task_ended(&mut self) {
        let state = self.store.data_mut();
        state.live_tasks = state.live_tasks.saturating_sub(1);
    }

    /// Provider answers since the last call, in the order given.
    pub fn take_provided(&mut self) -> Vec<Provided> {
        self.store.data_mut().provided_bytes = 0;
        std::mem::take(&mut self.store.data_mut().provided)
    }
    pub fn take_provider_configurations(&mut self) -> Vec<ProviderConfiguration> {
        std::mem::take(&mut self.store.data_mut().provider_configurations)
            .into_values()
            .collect()
    }
    pub fn take_workspace_operations(&mut self) -> Vec<(u64, WorkspaceOperation)> {
        self.store.data_mut().workspace_operation_bytes = 0;
        std::mem::take(&mut self.store.data_mut().workspace_operations)
    }

    /// Edits accepted since the last call, in acceptance order.
    pub fn take_accepted(&mut self) -> Vec<AcceptedEdit> {
        std::mem::take(&mut self.store.data_mut().accepted)
    }

    /// The application settled `operation`; its pending slot is free again.
    pub fn operation_settled(&mut self) {
        let state = self.store.data_mut();
        state.outstanding = state.outstanding.saturating_sub(1);
    }

    pub fn take_logs(&mut self) -> Vec<(u8, String)> {
        std::mem::take(&mut self.store.data_mut().logs)
    }
}

#[cfg(test)]
mod timer_tests {
    use super::*;
    #[test]
    fn answer_budget_counts_empty_records_and_rejection_keeps_prior_credit() {
        let mut state = HostState::new(&Limits::default());
        state
            .push_provided(Provided::Hover {
                request: 1,
                answer: Some(WireHover {
                    contents: "small".into(),
                    range: None,
                }),
            })
            .unwrap();
        let before = state.provided_bytes;
        let error =
            host::Host::provide_completion_batch(&mut state, 2, vec![0xff], false).unwrap_err();
        assert!(matches!(error.kind, WitErrorKind::InvalidEdit));
        assert_eq!(state.provided_bytes, before);
        assert_eq!(state.provided.len(), 1);
        let json = format!("[{}]", vec![r#"{"label":""}"#; 100000].join(","));
        let items = completion_batch::decode(&json).unwrap();
        let error = state
            .push_provided(Provided::Completions {
                request: 2,
                items,
                incomplete: false,
            })
            .unwrap_err();
        assert!(matches!(error.kind, WitErrorKind::LimitExceeded));
        assert_eq!(state.provided_bytes, before);
        assert_eq!(state.provided.len(), 1);
    }
    #[test]
    fn text_writes_preserve_the_existing_save_participant_restriction() {
        let mut state = HostState::new(&Limits::default());
        state.save_requests.insert(7);
        let error = host::Host::process_write_text(&mut state, 99, "中😀".into()).unwrap_err();
        assert!(matches!(error.kind, WitErrorKind::PermissionDenied));
    }
    #[test]
    fn timers_are_bounded_cancellable_and_instance_owned() {
        let mut state = HostState::new(&Limits::default());
        let before = host::Host::monotonic_nanoseconds(&mut state);
        let first = host::Host::timer_start(&mut state, 0).unwrap();
        for _ in 1..64 {
            host::Host::timer_start(&mut state, 1000).unwrap();
        }
        assert!(host::Host::timer_start(&mut state, 0).is_err());
        host::Host::timer_cancel(&mut state, first);
        assert!(host::Host::timer_start(&mut state, 0).is_ok());
        assert!(host::Host::timer_start(&mut state, 86_400_001).is_err());
        assert!(host::Host::monotonic_nanoseconds(&mut state) >= before);
    }
}
