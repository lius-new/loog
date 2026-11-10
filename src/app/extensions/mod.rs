//! Orchestration of extension packages (design §5.1, §6.1, §7.2, §8).
//!
//! The service is plain state plus rules: it takes application state in and
//! returns [`Effect`]s (jobs for the executor, messages for the user). It
//! owns no thread, so every rule here is testable without a UI; `host.rs`
//! connects it to the executor and the UI thread.
use crate::app::state::AppState;
use crate::app::workspace_edits::{self, DocumentChange, TextTransaction};
use crate::domain::document::DocumentStamp;
use crate::domain::identity::FileId;
use crate::features::extensions::activation::{PendingCommand, PendingTriggers};
use crate::features::extensions::lifecycle::{FailureStage, Lifecycle, State};
use crate::features::extensions::manifest::Manifest;
use crate::features::extensions::manifest::ProviderKind as DeclKind;
use crate::features::extensions::providers::{self, Reported};
use crate::features::extensions::trust::RootTrust;
use crate::features::extensions::trust::{Denial, Grants, PackageIdentity};
use crate::features::language_service::sync::Stamp;
use crate::infrastructure::extensions::ProviderJob;
use crate::infrastructure::extensions::tasks;
use crate::infrastructure::extensions::{Event, Job, Outcome, Run};
use loog_extension_api::admission::EditLimits;
use loog_extension_api::budget::{CallBudget, InteractionLimits};
use loog_extension_api::events::{Delivery, DocumentStream};
use loog_extension_api::ledger::{Begin, OperationLedger};
use loog_extension_api::{ErrorKind, ExtensionError};
use loog_extension_host::{
    AcceptedEdit, DocumentChange as WireChange, DocumentRef, Inbound, Limits,
};
use loog_extension_host::{Provided, ProviderKind as WireKind};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::path::{Path, PathBuf};

pub(crate) mod bundled;
mod documents;
mod files;
pub(crate) mod host;
mod language;
mod processes;
mod save;
mod ui;
mod watches;

const PROVIDER_DEADLINE_MS: u64 = 500;
const FORMAT_DEADLINE_MS: u64 = 5_000;
/// Requests one extension may have open at once, over all its providers and documents.
const MAX_PROVIDER_CALLS: usize = 32;
const MAX_BACKGROUND_CALLS: usize = 16;
/// Extension candidates list after every language server's.
const COMPLETION_RANK: usize = crate::features::providers::EXTERNAL_RANK;
const EXTERNAL_SOURCE: u64 = crate::features::providers::EXTERNAL_SOURCE;

/// The state of a document a provider is asked about.
struct Target {
    file: FileId,
    stamp: DocumentStamp,
    path: PathBuf,
    text: std::sync::Arc<String>,
    doc_id: u64,
    version: u64,
    range: Option<Range<usize>>,
    /// Completion: where the word being completed starts, the caret, the
    /// triggering character and (for a resolve request) the item.
    word_start: usize,
    cursor: Option<usize>,
    trigger: Option<String>,
    item: Option<loog_extension_host::WireCompletionItem>,
    item_raw: Option<serde_json::Value>,
}

/// A running task and who it belongs to.
struct TaskRecord {
    extension: String,
    generation: u64,
    /// The number the guest knows it by.
    local: u64,
    root_id: u64,
    cwd: PathBuf,
}

/// A provider request in flight, with the state it was asked about.
struct ProviderCall {
    extension: String,
    generation: u64,
    provider: String,
    file: FileId,
    stamp: DocumentStamp,
    path: PathBuf,
    text: std::sync::Arc<String>,
    doc_id: u64,
    deadline: u64,
    range: Option<Range<usize>>,
    word_start: usize,
    item_raw: Option<serde_json::Value>,
    job: ProviderJob,
    rank: usize,
}

/// Start the extension subsystem and load the packages installed for this user.
pub(crate) fn start(
    state: &crate::app::context::AppContext,
    application: &lgui::ApplicationHandle,
    runtime: &std::sync::Arc<crate::app::runtime::AppRuntime>,
) {
    let Some(root) = crate::infrastructure::language_packs::user_resource_directory("extensions")
    else {
        return;
    };
    let host = match host::ExtensionHost::start(
        state.clone(),
        application.clone(),
        root.join(".cache"),
        runtime.trust().clone(),
        Some(root.join("settings.json")),
    ) {
        Ok(host) => host,
        Err(error) => {
            state.update(|app| app.show_error(error));
            return;
        }
    };
    let mut problems = Vec::new();
    for dir in bundled::directories() {
        if let Err(error) = host.load_package(&dir) {
            problems.push(format!("{}: {error}", dir.display()));
        }
    }
    // The version in use of every installed extension; a turned-off one loads and is stopped below.
    for dir in crate::infrastructure::extensions::state::folders(&root, false) {
        if let Err(error) = host.load_package(&dir) {
            problems.push(format!("{}: {error}", dir.display()));
        }
    }
    state.update(|app| {
        host.restore_disabled(app);
        host.start_language_resources(app);
        host.refresh_settings(app);
    });
    runtime.install_extensions(host);
    if !problems.is_empty() {
        state.update(|app| app.show_error(format!("Extensions: {}", problems.join("; "))));
    }
}

/// Logs kept per package for the diagnostics entry (design §8.2).
const LOG_LIMIT: usize = 200;
const RECEIPTS: usize = 256;
const PENDING_COMMANDS: usize = 16;

/// What the caller must do after the service handled something.
// Effects carry bounded jobs by value; avoid a second allocation for each dispatch.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Effect {
    WorkspaceOperation {
        id: String,
        generation: u64,
        operation: u64,
        action: language::WorkspaceAction,
    },
    ClearUi {
        session: u64,
        owner: String,
    },
    StartWatch {
        watch: u64,
        spec: crate::infrastructure::extensions::watches::Spec,
    },
    StopWatch {
        watch: u64,
    },
    StartProcess {
        process: u64,
        spec: crate::infrastructure::extensions::processes::Spec,
    },
    WriteProcess {
        process: u64,
        bytes: Vec<u8>,
    },
    CloseProcess {
        process: u64,
    },
    KillProcess {
        process: u64,
    },
    Submit(Job),
    PublishDiagnostics {
        source: u64,
        path: PathBuf,
        stamp: DocumentStamp,
        text: std::sync::Arc<String>,
        items: Vec<crate::features::language_service::diagnostics::Diagnostic>,
    },
    PublishHighlights {
        source: u64,
        path: PathBuf,
        stamp: DocumentStamp,
        length: usize,
        items: Vec<crate::features::syntax::TokenSpan>,
    },
    ClearDiagnostics {
        source: u64,
    },
    /// What a service found in a file that is not open; none removes the file.
    PublishFindings {
        source: u64,
        path: PathBuf,
        items: Vec<crate::features::problems::Finding>,
    },
    /// Everything one session of a service found in files that are not open.
    ClearFindings {
        source: u64,
    },
    /// Run these file operations on a background thread, then call `file_ops_done`.
    RunFileOps {
        key: u64,
        ops: Vec<crate::infrastructure::extensions::fileops::Op>,
    },
    /// Files were created, renamed or deleted: the language servers are told.
    FilesChanged(Vec<crate::features::providers::FileChange>),
    /// A save chain ended. `ok` means the files may be written now.
    SaveReady {
        chain: u64,
        ok: bool,
        message: Option<String>,
    },
    StartTask {
        task: u64,
        spec: tasks::Spec,
    },
    CancelTask {
        task: u64,
    },
    /// A completion provider answered (or could not in time: `items` is `None`).
    OfferCompletion {
        file: FileId,
        incarnation: u64,
        word_start: usize,
        source: u64,
        rank: usize,
        items: Option<(Vec<serde_json::Value>, bool)>,
    },
    /// Snippets of a package for the language being completed. Unlike a
    /// provider's answer nothing is awaited, so the session does not count them.
    OfferSnippets {
        file: FileId,
        incarnation: u64,
        word_start: usize,
        source: u64,
        rank: usize,
        items: Vec<serde_json::Value>,
    },
    /// A completion item was resolved by the extension that offered it.
    ResolvedCompletion {
        source: u64,
        raw: serde_json::Value,
        item: serde_json::Value,
    },
    /// A provider answered a language request (`None`: it had nothing, or could
    /// not answer in time). `token` names the request the application waits on.
    LanguageAnswer {
        token: u64,
        answer: Option<LanguageAnswer>,
    },
    Toast(String),
    Error(String),
}

/// What a provider said, as the reply a language server would give.
#[derive(Clone, Debug)]
pub struct LanguageAnswer {
    /// The provider's source id (`EXTERNAL_SOURCE` set) and its declared id.
    pub source: u64,
    pub provider: String,
    pub value: serde_json::Value,
    pub rank: usize,
}

/// A hover, definition, outline, signature or inlay request of the language UI.
#[derive(Clone, Debug)]
pub struct LanguageAsk {
    pub feature: crate::features::language_service::request::Feature,
    pub path: PathBuf,
    pub stamp: Stamp,
    /// Byte offsets of the state `stamp` names.
    pub start: usize,
    pub end: usize,
    pub extra: serde_json::Value,
    pub target: Option<u64>,
    pub server: Option<String>,
    pub timeout: Option<std::time::Duration>,
}

/// The kinds of provider that answer a language feature.
fn language_kinds(
    feature: crate::features::language_service::request::Feature,
) -> Option<(WireKind, DeclKind)> {
    use crate::features::language_service::request::Feature;
    Some(match feature {
        Feature::Hover => (WireKind::Hover, DeclKind::Hover),
        Feature::Completion => (WireKind::Completion, DeclKind::Completion),
        Feature::CompletionResolve => (WireKind::CompletionResolve, DeclKind::Completion),
        Feature::Definition
        | Feature::Declaration
        | Feature::TypeDefinition
        | Feature::Implementation => (WireKind::Definition, DeclKind::Definition),
        Feature::DocumentSymbol => (WireKind::DocumentSymbols, DeclKind::DocumentSymbols),
        Feature::SignatureHelp => (WireKind::SignatureHelp, DeclKind::SignatureHelp),
        Feature::InlayHint => (WireKind::InlayHints, DeclKind::InlayHints),
        Feature::CodeAction => (WireKind::CodeActions, DeclKind::CodeActions),
        Feature::CodeActionResolve => (WireKind::CodeActionResolve, DeclKind::CodeActions),
        Feature::References => (WireKind::References, DeclKind::References),
        Feature::WorkspaceSymbol => (WireKind::WorkspaceSymbols, DeclKind::WorkspaceSymbols),
        Feature::DocumentHighlight => (WireKind::DocumentHighlights, DeclKind::DocumentHighlights),
        Feature::PrepareRename => (WireKind::PrepareRename, DeclKind::Rename),
        Feature::Rename => (WireKind::Rename, DeclKind::Rename),
        Feature::FoldingRange => (WireKind::FoldingRanges, DeclKind::FoldingRanges),
        Feature::SelectionRange => (WireKind::SelectionRanges, DeclKind::SelectionRanges),
        Feature::PrepareCallHierarchy | Feature::IncomingCalls | Feature::OutgoingCalls => {
            (WireKind::CallHierarchy, DeclKind::CallHierarchy)
        }
        Feature::PrepareTypeHierarchy | Feature::Supertypes | Feature::Subtypes => {
            (WireKind::TypeHierarchy, DeclKind::TypeHierarchy)
        }
        Feature::CodeLens => (WireKind::CodeLens, DeclKind::CodeLens),
        Feature::CodeLensResolve => (WireKind::CodeLensResolve, DeclKind::CodeLens),
        Feature::Formatting | Feature::RangeFormatting => {
            (WireKind::Formatting, DeclKind::Formatting)
        }
        Feature::OnTypeFormatting => (WireKind::FormatOnType, DeclKind::FormatOnType),
        Feature::ExecuteCommand => (WireKind::ExecuteCommand, DeclKind::ExecuteCommand),
    })
}

/// The target a command call captured when it was triggered.
struct Call {
    extension: String,
    generation: u64,
    /// Snapshot token -> the document and state it was read from.
    snapshots: HashMap<u64, (FileId, DocumentStamp)>,
    budget: CallBudget,
    title: String,
}

/// What one package has been told about one document.
struct Feed {
    stream: DocumentStream,
    path: String,
    language: String,
    variant: Option<String>,
    language_generation: u64,
    version: u64,
    incarnation: u64,
    synchronized: bool,
}

/// Whether a manifest asks for the named event (design §6).
fn wants(manifest: &Manifest, event: &str) -> bool {
    manifest.activation.iter().any(|a| {
        matches!(a, crate::features::extensions::manifest::Activation::Event { name } if name == event)
    })
}

/// The languages a package says it is for: those its providers select and those its
/// document activations name. `None` when it names none, so it is not about any language.
fn language_scope(manifest: &Manifest) -> Option<Vec<&str>> {
    use crate::features::extensions::manifest::Activation;
    let mut languages: Vec<&str> = manifest
        .contributes
        .language_service
        .providers
        .iter()
        .flat_map(|provider| provider.selector.languages.iter().map(String::as_str))
        .collect();
    for activation in &manifest.activation {
        if let Activation::Document { selector } = activation {
            languages.extend(selector.languages.iter().map(String::as_str));
        }
    }
    (!languages.is_empty()).then_some(languages)
}

/// Whether the documents of `language` are the package's business. A package that names
/// no language is about every document, as before.
fn serves(manifest: &Manifest, language: &str) -> bool {
    language_scope(manifest).is_none_or(|languages| languages.contains(&language))
}

fn subscribed(manifest: &Manifest) -> bool {
    manifest.has_behaviour()
        && ["document.open", "document.change", "document.close"]
            .iter()
            .any(|event| wants(manifest, event))
}

fn document_events_of(grants: &Grants, package: &Package) -> bool {
    subscribed(&package.manifest)
        && !matches!(
            package.lifecycle.state(),
            State::Stopped | State::Failed | State::Stopping
        )
        && package
            .manifest
            .capabilities
            .iter()
            .any(|cap| cap == "events")
        && grants.authorize(&package.identity, "events", None).is_ok()
}

const FEED_MAX_EDITS: usize = 4096;
const FEED_MAX_MESSAGES: usize = 64;

/// Data resources of a package after each one was checked on its own. A bad
/// resource is dropped without touching the others or the behaviour (design §6.6).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Accepted {
    pub languages: Vec<String>,
    pub themes: Vec<String>,
    pub highlight: Vec<String>,
    pub snippets: Vec<String>,
    /// `(resource id, why it was rejected)`
    pub rejected: Vec<(String, String)>,
}

/// Check a data resource: inside the package after resolving links, readable,
/// and valid JSON. Contents are checked again by the subsystem that loads them.
fn read_resource(dir: &Path, path: &str) -> Result<String, String> {
    let base = dir.canonicalize().map_err(|e| e.to_string())?;
    let file = dir
        .join(path)
        .canonicalize()
        .map_err(|_| "the file does not exist".to_owned())?;
    if !file.starts_with(&base) {
        return Err("the file is outside the package".to_owned());
    }
    if std::fs::metadata(&file).map_err(|e| e.to_string())?.len() > MAX_RESOURCE_BYTES {
        return Err("the file is too large".to_owned());
    }
    let text = std::fs::read_to_string(&file).map_err(|e| e.to_string())?;
    serde_json::from_str::<serde_json::Value>(&text).map_err(|e| format!("invalid JSON: {e}"))?;
    Ok(text)
}

/// Largest data resource a package may contribute.
const MAX_RESOURCE_BYTES: u64 = 1024 * 1024;

/// A theme a package contributes: id, name and the finished palette.
pub(crate) type PackageTheme = (String, String, crate::ui::theme::Palette);

/// Check every data resource on its own. Themes are read completely, so one that
/// names a color wrongly is rejected here with the reason rather than drawn wrong.
fn accept_resources(
    dir: &Path,
    manifest: &Manifest,
) -> (Accepted, Vec<PackageTheme>, Vec<PackageSnippets>) {
    let mut accepted = Accepted::default();
    let mut themes = Vec::new();
    let mut snippets = Vec::new();
    let c = &manifest.contributes;
    for resource in &c.themes {
        match read_resource(dir, &resource.path)
            .and_then(|text| crate::ui::theme_package::parse(&text))
        {
            Ok(parsed) => {
                accepted.themes.push(resource.id.clone());
                let name = if parsed.name.is_empty() {
                    resource.id.clone()
                } else {
                    parsed.name
                };
                themes.push((resource.id.clone(), name, parsed.palette));
            }
            Err(reason) => accepted.rejected.push((resource.id.clone(), reason)),
        }
    }
    for resource in &c.snippets {
        match read_resource(dir, &resource.path)
            .and_then(|text| crate::features::extensions::snippets::parse(&text))
        {
            Ok(file) => {
                accepted.snippets.push(resource.id.clone());
                snippets.push((resource.id.clone(), file));
            }
            Err(reason) => accepted.rejected.push((resource.id.clone(), reason)),
        }
    }
    for resource in &c.languages {
        match crate::infrastructure::language_packs::check_extension_identity(dir, &resource.path) {
            Ok(()) => accepted.languages.push(resource.id.clone()),
            Err(reason) => accepted.rejected.push((resource.id.clone(), reason)),
        }
    }
    for resource in &c.highlight {
        // A highlight resource is a language pack; live discovery loads it
        // (design §6.6.1), so it is held to the rules of a pack here.
        match crate::infrastructure::language_packs::check_extension_pack(dir, &resource.path) {
            Ok(()) => accepted.highlight.push(resource.id.clone()),
            Err(reason) => accepted.rejected.push((resource.id.clone(), reason)),
        }
    }
    (accepted, themes, snippets)
}

/// A snippet file a package contributes, under its resource id.
pub(crate) type PackageSnippets = (String, crate::features::extensions::snippets::File);

pub struct Package {
    pub manifest: Manifest,
    pub icon: Option<std::sync::Arc<crate::domain::media::Image>>,
    pub readme: Option<crate::features::markdown::content::Markup>,
    pub changelog: Option<crate::features::markdown::content::Markup>,
    pub resources: Accepted,
    disabled: bool,
    resource_revision: u64,
    /// The accepted themes, ready to draw with.
    themes: Vec<PackageTheme>,
    /// The accepted snippet files.
    snippets: Vec<PackageSnippets>,
    feeds: HashMap<u64, Feed>,
    document_views: HashMap<u64, documents::ViewStamp>,
    documents_initialized: bool,
    provider_configurations: HashMap<String, loog_extension_host::ProviderConfiguration>,
    provider_revision: u64,
    pending_saved: HashMap<FileId, crate::domain::document::SaveSnapshot>,
    pending_files: Vec<loog_extension_host::WorkspaceFileChange>,
    /// Resolved setting values (JSON text per key) and what could not be used.
    resolved: crate::features::extensions::settings::Resolved,
    pub dir: PathBuf,
    pub identity: PackageIdentity,
    pub lifecycle: Lifecycle,
    ledger: OperationLedger,
    pending: PendingTriggers,
    pub logs: Vec<(u8, String)>,
    pub last_error: Option<String>,
    component: Vec<u8>,
    code_source: Option<loog_extension_host::CodeSource>,
}

impl Package {
    /// Whether it ships with Loog rather than being installed by the user.
    pub fn is_bundled(&self) -> bool {
        bundled::is_bundled(&self.dir)
    }
}

pub struct ExtensionService {
    packages: HashMap<String, Package>,
    calls: HashMap<u64, Call>,
    grants: Grants,
    next_call: u64,
    next_token: u64,
    limits: EditLimits,
    guest_limits: Limits,
    clock: Box<dyn Fn() -> u64 + Send>,
    cache: PathBuf,
    /// Commands waiting for a cold activation, with what they captured.
    waiting_runs: HashMap<u64, Run>,
    /// Document identities handed to extensions, stable while the document lives.
    seen: HashMap<FileId, u64>,
    document_languages: HashMap<PathBuf, String>,
    language_preferences:
        std::collections::BTreeMap<String, crate::features::settings::language::LanguageSetting>,
    next_document: u64,
    settings: SettingSources,
    provider_calls: HashMap<u64, ProviderCall>,
    position_indices: crate::features::providers::position::IndexCache,
    /// Last document version each (extension, provider, document) was asked about.
    provider_seen: HashMap<(String, String, u64), DocumentStamp>,
    /// Requests waiting for a cold activation to finish.
    queued: Vec<u64>,
    next_request: u64,
    sources: HashMap<(String, String), u64>,
    /// Folders extensions may name, by handle.
    roots: Vec<(u64, PathBuf)>,
    next_root: u64,
    tasks: HashMap<u64, TaskRecord>,
    next_task: u64,
    processes: HashMap<u64, TaskRecord>,
    next_process: u64,
    watches: HashMap<u64, watches::WatchRecord>,
    next_watch: u64,
    ui_events: HashMap<String, (u64, crate::infrastructure::extensions::UiEvents)>,
    ui_session: u64,
    /// What each extension reports about its service, with the generation it came from.
    service_status: HashMap<String, (u64, Vec<loog_extension_host::ServiceReport>)>,
    file_jobs: HashMap<u64, files::FileJob>,
    next_file_job: u64,
    saves: HashMap<u64, save::Chain>,
    /// Open save-participant requests and the chain each belongs to.
    save_calls: HashMap<u64, u64>,
    next_save: u64,
    storage_dir: PathBuf,
    trust: Option<std::sync::Arc<crate::infrastructure::extensions::trust_store::TrustStore>>,
    /// Provider request -> the language request it answers.
    language_calls: HashMap<u64, u64>,
    /// Advances whenever the enabled set of package resources may have changed.
    resources_revision: u64,
    last_retired_generation: u64,
}

/// The setting files as last read.
#[derive(Default)]
struct SettingSources {
    user: serde_json::Value,
    project: Option<serde_json::Value>,
    trusted: bool,
}

/// Whether a package may keep stored values / read files in folders. Both are
/// plain package grants: declared in the manifest and approved by the user.
fn storage_of(grants: &Grants, package: &Package) -> bool {
    package.manifest.capabilities.iter().any(|c| c == "storage")
        && grants.authorize(&package.identity, "storage", None).is_ok()
}

fn files_of(grants: &Grants, package: &Package) -> bool {
    package
        .manifest
        .capabilities
        .iter()
        .any(|c| c == "workspace.files.read")
        && grants
            .authorize(&package.identity, "workspace.files.read", None)
            .is_ok()
}

/// Hosts a package may reach: its declared list, once the user granted `network`.
fn domains_of(grants: &Grants, package: &Package) -> Vec<String> {
    let granted = package.manifest.capabilities.iter().any(|c| c == "network")
        && grants.authorize(&package.identity, "network", None).is_ok();
    match (&package.manifest.network, granted) {
        (Some(network), true) => network.domains.clone(),
        _ => Vec::new(),
    }
}

/// What a person is told when local execution is refused, and how to change it.
fn execution_denial(denial: Denial) -> &'static str {
    match denial {
        Denial::PackageNotGranted => "this extension has not been granted local execution",
        Denial::RootNotTrusted => {
            "the folder is not trusted; trust it when Loog asks, or run workspace::TrustWorkspace"
        }
        Denial::NoTargetRoot => "a task needs a folder",
    }
}

/// A short stable name for a path: the first bytes of its SHA-256.
fn stable_key(text: &str) -> String {
    use std::fmt::Write;
    let hash = ring::digest::digest(&ring::digest::SHA256, text.as_bytes());
    hash.as_ref()
        .iter()
        .take(10)
        .fold(String::new(), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

fn fingerprint(edit: &AcceptedEdit) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    edit.title.hash(&mut hasher);
    edit.call.hash(&mut hasher);
    for document in &edit.documents {
        document.snapshot.hash(&mut hasher);
        for text_edit in &document.edits {
            (text_edit.range.start, text_edit.range.end, &text_edit.text).hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn digest(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let hash = ring::digest::digest(&ring::digest::SHA256, bytes);
    hash.as_ref()
        .iter()
        .fold(String::from("sha256:"), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

impl ExtensionService {
    pub fn new(cache: PathBuf) -> Self {
        static UI_SESSIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let start = std::time::Instant::now();
        let storage_dir = cache.with_file_name("storage");
        Self {
            packages: HashMap::new(),
            calls: HashMap::new(),
            grants: Grants::default(),
            next_call: 0,
            next_token: 0,
            limits: EditLimits::default(),
            guest_limits: Limits::default(),
            clock: Box::new(move || start.elapsed().as_millis() as u64),
            cache,
            waiting_runs: HashMap::new(),
            seen: HashMap::new(),
            document_languages: HashMap::new(),
            language_preferences: Default::default(),
            next_document: 0,
            settings: SettingSources::default(),
            provider_calls: HashMap::new(),
            position_indices: Default::default(),
            provider_seen: HashMap::new(),
            queued: Vec::new(),
            next_request: 0,
            sources: HashMap::new(),
            roots: Vec::new(),
            next_root: 0,
            tasks: HashMap::new(),
            next_task: 0,
            processes: HashMap::new(),
            next_process: 0,
            watches: HashMap::new(),
            next_watch: 0,
            ui_events: HashMap::new(),
            ui_session: UI_SESSIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            service_status: HashMap::new(),
            file_jobs: HashMap::new(),
            next_file_job: 0,
            saves: HashMap::new(),
            save_calls: HashMap::new(),
            next_save: 0,
            storage_dir,
            trust: None,
            language_calls: HashMap::new(),
            resources_revision: 0,
            last_retired_generation: 0,
        }
    }

    pub(crate) fn resources_revision(&self) -> u64 {
        self.resources_revision
    }

    pub fn set_clock(&mut self, clock: impl Fn() -> u64 + Send + 'static) {
        self.clock = Box::new(clock);
    }

    /// Replace the clock (milliseconds); tests drive time by hand.
    pub fn with_clock(mut self, clock: impl Fn() -> u64 + Send + 'static) -> Self {
        self.clock = Box::new(clock);
        self
    }

    pub fn with_guest_limits(mut self, limits: Limits) -> Self {
        self.guest_limits = limits;
        self
    }

    pub fn set_storage_dir(&mut self, dir: PathBuf) {
        self.storage_dir = dir;
    }

    pub fn set_guest_limits(&mut self, limits: Limits) {
        self.guest_limits = limits;
    }

    /// What the extension reports about its service that is about `document`, or about every
    /// document it serves; empty when it reports nothing, or is not running.
    pub fn service_reports(
        &self,
        id: &str,
        document: Option<&str>,
    ) -> Vec<&loog_extension_host::ServiceReport> {
        let running = self
            .packages
            .get(id)
            .is_some_and(|package| package.lifecycle.state() == State::Active);
        let Some((_, reports)) = self.service_status.get(id).filter(|_| running) else {
            return Vec::new();
        };
        reports
            .iter()
            .filter(|report| match (&report.contexts, document) {
                (None, _) => true,
                (Some(contexts), Some(document)) => contexts.iter().any(|path| path == document),
                (Some(_), None) => false,
            })
            .collect()
    }

    pub fn package(&self, id: &str) -> Option<&Package> {
        self.packages.get(id)
    }

    pub fn packages(&self) -> impl Iterator<Item = &Package> + Clone {
        self.packages.values()
    }

    fn read_package(dir: &Path) -> Result<(Manifest, Vec<u8>, PackageIdentity), String> {
        let source = std::fs::read_to_string(dir.join("manifest.json"))
            .map_err(|e| format!("manifest.json: {e}"))?;
        let manifest = Manifest::parse(&source).map_err(|e| e.to_string())?;
        let component = match &manifest.component {
            Some(path) => std::fs::read(dir.join(path)).map_err(|e| format!("{path}: {e}"))?,
            None => Vec::new(),
        };
        let identity = PackageIdentity {
            id: manifest.id.clone(),
            digest: digest(&[source.as_bytes(), &component].concat()),
        };
        Ok((manifest, component, identity))
    }

    /// Read and validate a package from a user-selected directory. Nothing runs.
    pub fn load_package(&mut self, dir: &Path) -> Result<String, String> {
        let package = Self::read_package(dir)?;
        Ok(self.replace_package(dir, package))
    }

    fn replace_package(
        &mut self,
        dir: &Path,
        (manifest, component, identity): (Manifest, Vec<u8>, PackageIdentity),
    ) -> String {
        let id = manifest.id.clone();
        let disabled = self.packages.get(&id).is_some_and(|old| old.disabled);
        let mut lifecycle = self
            .packages
            .get(&id)
            .map(|old| Lifecycle::after(old.lifecycle.generation()))
            .unwrap_or_else(|| Lifecycle::after(self.last_retired_generation));
        lifecycle.validated().expect("discovered package");
        lifecycle.dormant().expect("validated package");
        if disabled {
            lifecycle.stopping().expect("dormant package");
            lifecycle.stopped().expect("stopping package");
        }
        // A reload replaces the old package: its generation moves on first.
        if let Some(old) = self.packages.remove(&id) {
            self.calls
                .retain(|_, call| call.extension != old.manifest.id);
        }
        let (mut resources, themes, snippets) = accept_resources(dir, &manifest);
        let icon = manifest.icon.as_deref().and_then(|path| {
            match crate::infrastructure::extensions::icon::load(dir, path) {
                Ok(image) => Some(std::sync::Arc::new(image)),
                Err(error) => {
                    resources.rejected.push(("icon".to_owned(), error));
                    None
                }
            }
        });
        let mut document = |field: &str, path: Option<&str>| {
            path.and_then(|path| {
                match crate::infrastructure::extensions::documentation::load(dir, path) {
                    Ok(content) => Some(content),
                    Err(error) => {
                        resources.rejected.push((field.to_owned(), error));
                        None
                    }
                }
            })
        };
        let readme = document("readme", manifest.readme.as_deref());
        let changelog = document("changelog", manifest.changelog.as_deref());
        self.resources_revision += 1;
        let resolved = crate::features::extensions::settings::resolve(
            &manifest.contributes.settings,
            &self.settings.user,
            self.settings.project.as_ref(),
            self.settings.trusted,
        );
        self.packages.insert(
            id.clone(),
            Package {
                manifest,
                icon,
                readme,
                changelog,
                resources,
                disabled,
                resource_revision: self.resources_revision,
                themes,
                snippets,
                dir: dir.to_path_buf(),
                identity,
                lifecycle,
                feeds: HashMap::new(),
                document_views: HashMap::new(),
                documents_initialized: false,
                provider_configurations: HashMap::new(),
                provider_revision: 0,
                pending_saved: HashMap::new(),
                pending_files: Vec::new(),
                resolved,
                ledger: OperationLedger::new(RECEIPTS),
                pending: PendingTriggers::new(PENDING_COMMANDS),
                logs: Vec::new(),
                last_error: None,
                component,
                code_source: None,
            },
        );
        id
    }

    /// Re-read a package from its directory (development reload, design §8.2).
    ///
    /// The old instance is stopped first and the generation moves on, so its
    /// unconfirmed operations are never replayed and its callbacks cannot act
    /// on the new instance. Grants follow the content digest: an unchanged
    /// package keeps what it was granted, a changed one must be granted again.
    pub fn reload(&mut self, id: &str) -> Result<Vec<Effect>, String> {
        let dir = self
            .packages
            .get(id)
            .ok_or("unknown extension")?
            .dir
            .clone();
        let package = Self::read_package(&dir)?;
        if package.0.id != id {
            return Err(format!("manifest now identifies {}", package.0.id));
        }
        let old_digest = self.packages[id].identity.digest.clone();
        let mut effects = self.stop(id);
        let new_id = self.replace_package(&dir, package);
        if self.packages[&new_id].identity.digest == old_digest {
            self.grant_declared(&new_id);
        } else {
            effects.push(Effect::Toast(format!(
                "{new_id} changed on disk; its permissions must be granted again"
            )));
        }
        Ok(effects)
    }

    /// Start using the package folder `dir`, a new install or a new version of one
    /// that is loaded: what ran before from another folder is stopped, and the
    /// capabilities the package declares are granted, because the user asked for
    /// this install (and confirmed what it asks for). Returns its id.
    pub fn adopt(&mut self, dir: &Path) -> Result<(String, Vec<Effect>), String> {
        let package = Self::read_package(dir)?;
        let effects = if self.packages.contains_key(&package.0.id) {
            self.stop(&package.0.id)
        } else {
            Vec::new()
        };
        let id = self.replace_package(dir, package);
        self.grant_declared(&id);
        Ok((id, effects))
    }

    /// The user approves the package's declared capabilities.
    pub fn grant_declared(&mut self, id: &str) {
        if let Some(package) = self.packages.get(id) {
            for capability in &package.manifest.capabilities {
                self.grants.grant_package(&package.identity, capability);
            }
        }
    }

    fn authorize(&self, package: &Package, capability: &str) -> Result<(), ExtensionError> {
        if !package
            .manifest
            .capabilities
            .iter()
            .any(|c| c == capability)
        {
            return Err(ExtensionError::new(
                ErrorKind::PermissionDenied,
                format!("`{capability}` is not declared by {}", package.manifest.id),
            ));
        }
        self.grants
            .authorize(&package.identity, capability, None)
            .map_err(|denial| {
                ExtensionError::new(
                    ErrorKind::PermissionDenied,
                    match denial {
                        Denial::PackageNotGranted => format!(
                            "`{capability}` has not been granted to {}",
                            package.manifest.id
                        ),
                        other => format!("{other:?}"),
                    },
                )
            })
    }

    fn owner_of(&self, command: &str) -> Option<&str> {
        self.packages
            .values()
            .find(|package| {
                package
                    .manifest
                    .contributes
                    .commands
                    .iter()
                    .any(|c| c.id == command)
            })
            .map(|package| package.manifest.id.as_str())
    }

    /// New contents of the settings files (design §6). Each package resolves
    /// its own keys; running ones are told which values changed.
    pub fn set_setting_sources(
        &mut self,
        user: serde_json::Value,
        project: Option<serde_json::Value>,
        trusted: bool,
    ) -> Vec<Effect> {
        self.settings = SettingSources {
            user,
            project,
            trusted,
        };
        let mut effects = Vec::new();
        for (id, package) in self.packages.iter_mut() {
            let resolved = crate::features::extensions::settings::resolve(
                &package.manifest.contributes.settings,
                &self.settings.user,
                self.settings.project.as_ref(),
                self.settings.trusted,
            );
            let changed = crate::features::extensions::settings::changed(
                &package.resolved.values,
                &resolved.values,
            );
            package.resolved = resolved;
            if package.lifecycle.state() == State::Active {
                effects.push(Effect::Submit(Job::Settings {
                    id: id.clone(),
                    generation: package.lifecycle.generation(),
                    values: package.resolved.values.clone(),
                    changed,
                }));
            }
        }
        effects
    }

    /// One block per package for the diagnostics entry (design §8.2): identity,
    /// state, why it failed, what it may do, and its latest log lines. Document
    /// text and command arguments are never included.
    pub fn report(&self) -> String {
        if self.packages.is_empty() {
            return "No extensions installed".to_owned();
        }
        let mut ids: Vec<_> = self.packages.keys().collect();
        ids.sort();
        ids.into_iter()
            .map(|id| {
                let package = &self.packages[id];
                let granted: Vec<_> = package
                    .manifest
                    .capabilities
                    .iter()
                    .filter(|c| self.authorize(package, c).is_ok())
                    .map(String::as_str)
                    .collect();
                let mut block = format!(
                    "{} {} [{:?}] generation {} api {}\n  permissions: {}",
                    id,
                    package.manifest.version,
                    package.lifecycle.state(),
                    package.lifecycle.generation(),
                    package.manifest.api.as_deref().unwrap_or("resources only"),
                    if granted.is_empty() {
                        "none granted".to_owned()
                    } else {
                        granted.join(", ")
                    },
                );
                if let Some(stage) = package.lifecycle.failure() {
                    block.push_str(&format!("\n  failed at: {stage:?}"));
                }
                if let Some(error) = &package.last_error {
                    block.push_str(&format!("\n  error: {error}"));
                }
                if package.resources != Accepted::default() {
                    let r = &package.resources;
                    block.push_str(&format!(
                        "\n  resources: {} theme(s), {} highlight pack(s), {} snippet file(s)",
                        r.themes.len(),
                        r.highlight.len(),
                        r.snippets.len()
                    ));
                    for (id, reason) in &r.rejected {
                        block.push_str(&format!("\n  resource rejected: {id} ({reason})"));
                    }
                }
                for ignored in &package.resolved.ignored {
                    block.push_str(&format!(
                        "\n  setting ignored: {} ({})",
                        ignored.key, ignored.reason
                    ));
                }
                for (level, line) in package.logs.iter().rev().take(3).rev() {
                    block.push_str(&format!("\n  log[{level}]: {line}"));
                }
                block
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Commands the user can run, with titles, for menus and keymap checks.
    pub fn commands(&self) -> Vec<(String, String)> {
        let mut commands: Vec<_> = self
            .packages
            .values()
            .flat_map(|p| {
                p.manifest
                    .contributes
                    .commands
                    .iter()
                    .map(|c| (c.id.clone(), c.title.clone()))
            })
            .collect();
        commands.sort();
        commands
    }

    /// Trigger `command` on the active document. The target is captured now
    /// and never re-resolved to whatever is active later.
    pub fn run_command(&mut self, app: &AppState, command: &str) -> Vec<Effect> {
        let Some(owner) = self.owner_of(command).map(str::to_owned) else {
            return vec![Effect::Error(format!(
                "unknown extension command {command}"
            ))];
        };
        let package = &self.packages[&owner];
        if let Err(error) = self
            .authorize(package, "documents.read")
            .and_then(|_| self.authorize(package, "workspace.edit"))
        {
            return vec![Effect::Error(error.to_string())];
        }
        let Some(file) = app.workspace.active_document() else {
            return vec![Effect::Error(format!(
                "{command}: there is no active document"
            ))];
        };
        let Some(editor) = app.workspace.active_editor() else {
            return vec![Effect::Error(format!(
                "{command}: the active tab is not a text file"
            ))];
        };
        let document = &app.workspace.documents[&file];
        let stamp = document.stamp(file);
        let text = document.buffer.text_snapshot();
        let selections = editor
            .selections()
            .into_iter()
            .filter_map(|s| s.anchor.map(|anchor| (s.range(), s.cursor < anchor)))
            .filter_map(|(range, reversed)| range.map(|r| (r, reversed)))
            .collect::<Vec<_>>();

        self.next_call += 1;
        self.next_token += 1;
        let (call, token) = (self.next_call, self.next_token);
        let title = self.packages[&owner]
            .manifest
            .contributes
            .commands
            .iter()
            .find(|c| c.id == command)
            .map(|c| c.title.clone())
            .unwrap_or_default();
        let package = self.packages.get_mut(&owner).expect("owner exists");
        let generation = package.lifecycle.generation();
        let now = (self.clock)();
        self.calls.insert(
            call,
            Call {
                extension: owner.clone(),
                generation,
                snapshots: HashMap::from([(token, (file, stamp))]),
                budget: CallBudget::new(now, CallBudget::COMMAND_MS, InteractionLimits::default()),
                title,
            },
        );
        let run = Run {
            call,
            command: command.to_owned(),
            snapshot: Some((token, text)),
            selections,
            selection_precondition: None,
        };
        match package.lifecycle.state() {
            State::Active => vec![Effect::Submit(Job::Run {
                id: owner,
                generation,
                run,
            })],
            State::Dormant => {
                // Cold activation: queue the command, then ask for the instance once.
                let queued = package.pending.push_command(PendingCommand {
                    command: command.to_owned(),
                    call,
                });
                self.calls_run_store(call, run);
                if !queued {
                    self.calls.remove(&call);
                    return vec![Effect::Error(format!(
                        "{command}: too many commands waiting for activation"
                    ))];
                }
                let package = self.packages.get_mut(&owner).expect("owner exists");
                let generation = package.lifecycle.generation();
                let _ = package.lifecycle.activating();
                vec![Effect::Submit(Job::Load {
                    id: owner,
                    generation,
                    component: package.component.clone(),
                    package_dir: package.dir.clone(),
                    version: package.manifest.version.clone(),
                    cache: self.cache.clone(),
                    limits: self.guest_limits,
                    storage: storage_of(&self.grants, package)
                        .then(|| self.storage_dir.join(&package.manifest.id)),
                    files: files_of(&self.grants, package),
                    domains: domains_of(&self.grants, package),
                })]
            }
            State::Activating => {
                let queued = package.pending.push_command(PendingCommand {
                    command: command.to_owned(),
                    call,
                });
                if queued {
                    self.calls_run_store(call, run);
                    vec![]
                } else {
                    self.calls.remove(&call);
                    vec![Effect::Error(format!(
                        "{command}: too many commands waiting for activation"
                    ))]
                }
            }
            other => {
                self.calls.remove(&call);
                vec![Effect::Error(format!(
                    "{command}: the extension is {other:?}; retry it first"
                ))]
            }
        }
    }

    // Commands waiting for activation keep the captured run next to their call.
    fn calls_run_store(&mut self, call: u64, run: Run) {
        self.waiting_runs.insert(call, run);
    }

    /// Explicit retry after a failure: a new generation, back to dormant.
    pub fn retry(&mut self, id: &str) -> Result<(), String> {
        let package = self.packages.get_mut(id).ok_or("unknown extension")?;
        package.lifecycle.retry().map_err(|e| e.to_string())?;
        package.last_error = None;
        Ok(())
    }

    /// Turn an extension off: it runs nothing and contributes nothing until
    /// it is enabled again. A failed one is made dormant first so it can stop.
    pub fn disable(&mut self, id: &str) -> Result<Vec<Effect>, String> {
        let package = self.packages.get(id).ok_or("unknown extension")?;
        if package.lifecycle.state() == State::Failed {
            self.retry(id)?;
        }
        self.packages.get_mut(id).expect("known package").disabled = true;
        Ok(self.stop(id))
    }

    /// Turn a disabled extension back on; it starts when it is next needed.
    pub fn enable(&mut self, id: &str) -> Result<(), String> {
        let package = self.packages.get(id).ok_or("unknown extension")?;
        if package.lifecycle.state() == State::Stopped {
            self.retry(id)?;
            self.resources_revision += 1;
        }
        self.packages.get_mut(id).expect("known package").disabled = false;
        Ok(())
    }

    /// Stop an extension and forget it; returns the folder it was loaded from.
    pub fn uninstall(&mut self, id: &str) -> Result<(PathBuf, Vec<Effect>), String> {
        let package = self.packages.get(id).ok_or("unknown extension")?;
        if package.is_bundled() {
            return Err("it ships with Loog; disable it instead".to_owned());
        }
        let effects = self.stop(id);
        let package = self.packages.remove(id).ok_or("unknown extension")?;
        self.last_retired_generation = self
            .last_retired_generation
            .max(package.lifecycle.generation());
        Ok((package.dir, effects))
    }

    /// Stop an extension: the generation advances, so late work is refused.
    pub fn stop(&mut self, id: &str) -> Vec<Effect> {
        let Some(package) = self.packages.get_mut(id) else {
            return vec![];
        };
        if package.lifecycle.stopping().is_ok() {
            let _ = package.lifecycle.stopped();
        }
        self.resources_revision += 1;
        self.service_status.remove(id);
        self.calls.retain(|_, call| call.extension != id);
        let mut effects = self.clear_sources(id);
        effects.extend(self.cancel_tasks_of(id));
        effects.push(Effect::Submit(Job::Stop { id: id.to_owned() }));
        effects
    }

    pub fn handle_event(&mut self, app: &mut AppState, event: Event) -> Vec<Effect> {
        match event {
            Event::WorkspaceOperation {
                id,
                generation,
                operation,
                request,
            } => self.workspace_operation(app, &id, generation, operation, request),
            Event::ProviderExpired {
                id,
                generation,
                request,
            } => {
                let mut effects = Vec::new();
                if self
                    .provider_calls
                    .get(&request)
                    .is_some_and(|c| c.extension == id && c.generation == generation)
                {
                    self.cancel_provider(request, &mut effects);
                }
                effects
            }
            Event::ProvidersConfigured {
                id,
                generation,
                configurations,
            } => {
                let mut changed = Vec::new();
                let mut disabled = Vec::new();
                let mut invalidate = Vec::new();
                if let Some(package) = self
                    .packages
                    .get_mut(&id)
                    .filter(|p| p.lifecycle.accepts(generation))
                {
                    for configuration in configurations {
                        if package
                            .manifest
                            .contributes
                            .language_service
                            .providers
                            .iter()
                            .any(|p| p.id == configuration.provider)
                        {
                            if package
                                .provider_configurations
                                .get(&configuration.provider)
                                .is_some_and(|before| {
                                    language::same_configuration(before, &configuration)
                                })
                            {
                                continue;
                            }
                            if package
                                .provider_configurations
                                .contains_key(&configuration.provider)
                                || !configuration.enabled
                            {
                                invalidate.push(configuration.provider.clone());
                            }
                            changed.push(configuration.provider.clone());
                            if !configuration.enabled {
                                disabled.push(configuration.provider.clone());
                            }
                            package
                                .provider_configurations
                                .insert(configuration.provider.clone(), configuration);
                            package.provider_revision += 1;
                        } else {
                            if package.logs.len() >= LOG_LIMIT {
                                package.logs.remove(0);
                            }
                            package
                                .logs
                                .push((1, "undeclared provider configuration refused".into()));
                        }
                    }
                    self.provider_seen.retain(|(owner, provider, _), _| {
                        owner != &id || !changed.contains(provider)
                    });
                }
                let mut effects = Vec::new();
                let pending = self
                    .provider_calls
                    .iter()
                    .filter(|(_, call)| call.extension == id && invalidate.contains(&call.provider))
                    .map(|(request, _)| *request)
                    .collect::<Vec<_>>();
                for request in pending {
                    self.cancel_provider(request, &mut effects);
                }
                for provider in disabled {
                    if let Some(source) = self.sources.get(&(id.clone(), provider)) {
                        effects.push(Effect::ClearDiagnostics { source: *source });
                    }
                }
                effects.extend(self.observe(app));
                effects
            }
            Event::UiChanged {
                id,
                generation,
                snapshot,
            } => {
                let snapshot = snapshot.lock().unwrap().take();
                match snapshot {
                    Some(snapshot) => self.ui_changed(app, &id, generation, snapshot),
                    None => vec![],
                }
            }
            Event::ServiceStatus {
                id,
                generation,
                reports,
            } => {
                if self
                    .packages
                    .get(&id)
                    .is_some_and(|package| package.lifecycle.accepts(generation))
                {
                    self.service_status.insert(id, (generation, reports));
                }
                vec![]
            }
            Event::Findings {
                id,
                generation,
                changes,
            } => {
                if !self
                    .packages
                    .get(&id)
                    .is_some_and(|package| package.lifecycle.accepts(generation))
                {
                    return vec![];
                }
                let mut effects = Vec::new();
                for key in changes.withdrawn {
                    let source = self.source_for(&id, &format!("findings:{key}"));
                    effects.push(Effect::ClearFindings { source });
                }
                for file in changes.files {
                    let source = self.source_for(&id, &format!("findings:{}", file.key));
                    effects.push(Effect::PublishFindings {
                        source,
                        path: PathBuf::from(&file.path),
                        items: processes::closed_findings(&file),
                    });
                }
                effects
            }
            Event::WatchControls {
                id,
                generation,
                controls,
            } => self.watch_controls(&id, generation, controls),
            Event::ProcessControls {
                id,
                generation,
                controls,
            } => self.process_controls(&id, generation, controls),
            Event::Loaded {
                id,
                generation,
                result,
                source,
                aot_rejection,
                ..
            } => {
                if let Some(package) = self
                    .packages
                    .get_mut(&id)
                    .filter(|p| p.lifecycle.generation() == generation)
                {
                    package.code_source = Some(source);
                    if let Some(reason) = aot_rejection {
                        package
                            .logs
                            .push((1, format!("Published AOT not used: {reason}")));
                    }
                }
                let effects = self.loaded(&id, generation, result);
                let (mut setup, work): (Vec<_>, Vec<_>) = effects.into_iter().partition(|effect| {
                    matches!(
                        effect,
                        Effect::Submit(Job::Settings { .. } | Job::Roots { .. })
                    )
                });
                // Populate document views and deliver lifecycle boundaries before
                // replaying commands/provider calls that waited for activation.
                setup.extend(self.observe(app));
                setup.extend(self.flush_saved(app, &id));
                setup.extend(self.flush_files(&id));
                setup.extend(work);
                setup
            }
            Event::FileOpsRequested {
                id,
                generation,
                started,
            } => self.file_ops_requested(app, &id, generation, started),
            Event::TaskRequested {
                id,
                generation,
                task,
            } => self.task_requested(&id, generation, task),
            Event::TasksCancelled {
                id,
                generation,
                tasks,
            } => self.tasks_cancelled(&id, generation, &tasks),
            Event::DiagnosticsPublished {
                id,
                generation,
                published,
            } => self.diagnostics_published(app, &id, generation, published),
            Event::Provided {
                id,
                generation,
                provided,
            } => self.provided(app, &id, generation, provided),
            Event::EditRequested {
                id,
                generation,
                edit,
            } => self.edit_requested(app, &id, generation, edit),
            Event::Failed {
                id,
                generation,
                error,
            } => self.failed(&id, generation, error),
            Event::Log {
                id, level, text, ..
            } => {
                if let Some(package) = self.packages.get_mut(&id) {
                    // What an extension logs is what the person reads in Output.
                    // Levels are the host's: 0 information, 1 warning, 2 error.
                    crate::features::output::extension_log(
                        &id,
                        match level {
                            0 => crate::features::output::Level::Info,
                            1 => crate::features::output::Level::Warn,
                            _ => crate::features::output::Level::Error,
                        },
                        &text,
                    );
                    if package.logs.len() >= LOG_LIMIT {
                        package.logs.remove(0);
                    }
                    package.logs.push((level, text));
                }
                vec![]
            }
        }
    }

    fn loaded(
        &mut self,
        id: &str,
        generation: u64,
        result: Result<(), ExtensionError>,
    ) -> Vec<Effect> {
        let Some(package) = self.packages.get_mut(id) else {
            return vec![];
        };
        if package.lifecycle.generation() != generation {
            return vec![];
        }
        match result {
            Ok(()) => {
                let _ = package.lifecycle.active();
                crate::features::output::extension_log(
                    id,
                    crate::features::output::Level::Info,
                    "activated",
                );
                let (commands, _, _) = package.pending.drain();
                // The guest can read its settings before it handles anything.
                let initial = Effect::Submit(Job::Settings {
                    id: id.to_owned(),
                    generation,
                    values: package.resolved.values.clone(),
                    changed: Vec::new(),
                });
                let runs: Vec<Effect> = commands
                    .into_iter()
                    .filter_map(|pending| {
                        self.waiting_runs.remove(&pending.call).map(|run| {
                            Effect::Submit(Job::Run {
                                id: id.to_owned(),
                                generation,
                                run,
                            })
                        })
                    })
                    .collect();
                let mut effects: Vec<Effect> = std::iter::once(initial).collect();
                effects.push(self.roots_job(id, generation));
                effects.extend(runs);
                // Provider requests that waited for the start keep the deadline they
                // were given; one that already ran out is answered as too late.
                let ready: Vec<u64> = self
                    .queued
                    .iter()
                    .copied()
                    .filter(|request| {
                        self.provider_calls
                            .get(request)
                            .is_some_and(|call| call.extension == id)
                    })
                    .collect();
                self.queued.retain(|request| !ready.contains(request));
                for request in ready {
                    let call = &self.provider_calls[&request];
                    let mut job = Box::new(call.job.clone());
                    job.timeout_ms = call.deadline.saturating_sub((self.clock)());
                    effects.push(Effect::Submit(Job::Provide {
                        id: id.to_owned(),
                        generation,
                        job,
                    }));
                }
                effects
            }
            Err(error) => {
                package.lifecycle.fail(FailureStage::Activation);
                package.last_error = Some(error.to_string());
                crate::features::output::extension_log(
                    id,
                    crate::features::output::Level::Error,
                    &format!("could not start: {error}"),
                );
                let waiting = package.pending.fail_all();
                for pending in &waiting {
                    self.calls.remove(&pending.call);
                    self.waiting_runs.remove(&pending.call);
                }
                let mut effects = self.clear_sources(id);
                effects.push(Effect::Error(format!("{id}: could not start: {error}")));
                effects
            }
        }
    }

    fn failed(&mut self, id: &str, generation: u64, error: ExtensionError) -> Vec<Effect> {
        let Some(package) = self.packages.get_mut(id) else {
            return vec![];
        };
        if package.lifecycle.generation() != generation {
            return vec![];
        }
        let stage = match error.kind {
            ErrorKind::DeadlineExceeded | ErrorKind::LimitExceeded => FailureStage::BudgetExceeded,
            _ => FailureStage::Execution,
        };
        package.lifecycle.fail(stage);
        package.last_error = Some(error.to_string());
        crate::features::output::extension_log(
            id,
            crate::features::output::Level::Error,
            &format!("stopped: {error}"),
        );
        self.calls.retain(|_, call| call.extension != id);
        let mut effects = self.clear_sources(id);
        effects.extend(self.cancel_tasks_of(id));
        effects.push(Effect::Error(format!("{id}: stopped: {error}")));
        effects
    }

    fn edit_requested(
        &mut self,
        app: &mut AppState,
        id: &str,
        generation: u64,
        edit: AcceptedEdit,
    ) -> Vec<Effect> {
        let operation = edit.operation;
        let complete = |outcome: Outcome| {
            Effect::Submit(Job::Complete {
                id: id.to_owned(),
                generation,
                operation,
                outcome,
            })
        };
        let Some(package) = self.packages.get_mut(id) else {
            return vec![];
        };
        // Stop, reload and failure all move the generation: nothing from the old one may act.
        if !package.lifecycle.accepts(generation) {
            return vec![];
        }
        let print = fingerprint(&edit);
        match package.ledger.begin(edit.sequence, print) {
            Ok(Begin::Fresh) => {}
            Ok(Begin::Duplicate(state)) => {
                // Same sequence and input: report the existing outcome, never run it twice.
                use loog_extension_api::ledger::OperationState as S;
                let outcome = match state {
                    S::Committed {
                        transaction,
                        version,
                    }
                    | S::Settled {
                        transaction: Some(transaction),
                        version,
                    } => Outcome::Committed {
                        transaction,
                        version,
                    },
                    other => Outcome::Failed(ExtensionError::new(
                        ErrorKind::Conflict,
                        format!("the request is still {other:?}"),
                    )),
                };
                return vec![complete(outcome)];
            }
            Err(error) => return vec![complete(Outcome::Failed(error))],
        }
        let _ = package.ledger.validating(edit.sequence);

        let Some(call) = self
            .calls
            .get_mut(&edit.call)
            .filter(|c| c.extension == id && c.generation == generation)
        else {
            let _ = package.ledger.reject(edit.sequence);
            return vec![complete(Outcome::Failed(ExtensionError::new(
                ErrorKind::ExpiredHandle,
                "the command call has ended",
            )))];
        };
        let now = (self.clock)();
        if let Err(error) = call.budget.check(now) {
            let _ = package.ledger.reject(edit.sequence);
            return vec![complete(Outcome::Failed(error))];
        }

        let mut documents = Vec::new();
        for document in &edit.documents {
            let Some((file, stamp)) = call.snapshots.get(&document.snapshot).copied() else {
                let _ = package.ledger.reject(edit.sequence);
                return vec![complete(Outcome::Failed(ExtensionError::new(
                    ErrorKind::ExpiredHandle,
                    "the snapshot does not belong to this call",
                )))];
            };
            documents.push(DocumentChange {
                file,
                stamp,
                edits: document
                    .edits
                    .iter()
                    .map(|e| (e.range.start as usize..e.range.end as usize, e.text.clone()))
                    .collect(),
            });
        }
        let transaction = TextTransaction {
            title: call.title.clone(),
            documents,
        };
        let result = workspace_edits::commit(app, &transaction, &self.limits);
        let package = self.packages.get_mut(id).expect("package exists");
        match result {
            Ok(receipt) => {
                let version = receipt.versions.first().map(|(_, v)| *v).unwrap_or(0);
                let _ = package
                    .ledger
                    .commit(edit.sequence, receipt.transaction, version);
                let _ = package.ledger.settle(edit.sequence);
                vec![complete(Outcome::Committed {
                    transaction: receipt.transaction,
                    version,
                })]
            }
            Err(error) => {
                let _ = package.ledger.reject(edit.sequence);
                vec![complete(Outcome::Failed(error))]
            }
        }
    }

    /// Compare the open documents with what each subscribed package has been
    /// told and deliver the difference (design §7.3, §8.1). The continuity
    /// rules live in `DocumentStream`: contiguous changes merge, gaps become
    /// a resync, and open/close are boundaries.
    pub fn observe(&mut self, app: &AppState) -> Vec<Effect> {
        self.language_preferences = app.preferences.values().languages.clone();
        self.position_indices.prune();
        let mut effects = self.sync_roots(app);
        let stale = self
            .provider_calls
            .iter()
            .filter(|(_, call)| {
                app.workspace.documents.get(&call.file).is_none_or(|d| {
                    d.incarnation != call.stamp.incarnation
                        || d.meta.language_generation != call.stamp.language_generation
                        || (!matches!(
                            call.job.kind,
                            WireKind::Completion
                                | WireKind::CompletionResolve
                                | WireKind::Save
                                | WireKind::ExecuteCommand
                        ) && d.buffer.version() != call.stamp.version)
                })
            })
            .map(|(request, _)| *request)
            .collect::<Vec<_>>();
        for request in stale {
            self.cancel_provider(request, &mut effects);
        }
        let document_events = self.observe_documents(app);
        effects.extend(self.sync_document_views(app));
        effects.extend(document_events);
        effects
    }

    fn observe_documents(&mut self, app: &AppState) -> Vec<Effect> {
        let grants = &self.grants;
        let has_providers = self.packages.values().any(|p| {
            p.manifest.has_behaviour()
                && !p.manifest.contributes.language_service.providers.is_empty()
        });
        let has_readers = self
            .packages
            .values()
            .any(|p| p.lifecycle.state() == State::Active && documents::allowed(&self.grants, p));
        if !has_providers
            && !has_readers
            && !self
                .packages
                .values()
                .any(|p| document_events_of(grants, p))
        {
            return Vec::new();
        }
        let mut current = Vec::new();
        for (file, document) in app.workspace.documents.iter().filter(|(_, d)| d.is_text()) {
            current.push((
                *file,
                document.meta.path.display().to_string(),
                document.meta.lang.definition().name.to_string(),
                document.buffer.version(),
            ));
        }
        self.document_languages = current
            .iter()
            .map(|(_, path, language, _)| (PathBuf::from(path), language.clone()))
            .collect();
        let mut ids = Vec::new();
        for (file, path, language, version) in &current {
            let id = match self.seen.get(file) {
                Some(id) => *id,
                None => {
                    self.next_document += 1;
                    self.seen.insert(*file, self.next_document);
                    self.next_document
                }
            };
            ids.push(id);
            let document = &app.workspace.documents[file];
            let synchronized = document.buffer.len()
                <= app
                    .preferences
                    .values()
                    .language_features
                    .max_document_bytes;
            // A package for some languages hears only about their documents, so a document
            // of another language neither feeds it nor wakes it.
            for package in self
                .packages
                .values_mut()
                .filter(|p| document_events_of(grants, p) && serves(&p.manifest, language))
            {
                let feed = package.feeds.entry(id).or_insert_with(|| {
                    let mut stream = DocumentStream::new(FEED_MAX_EDITS, FEED_MAX_MESSAGES);
                    stream.open(*version);
                    Feed {
                        stream,
                        path: path.clone(),
                        language: language.clone(),
                        variant: Some(document.meta.lang.variant_name().to_owned()),
                        language_generation: document.meta.language_generation,
                        version: *version,
                        incarnation: document.incarnation,
                        synchronized,
                    }
                });
                if feed.incarnation != document.incarnation
                    || feed.path != *path
                    || feed.language != *language
                    || feed.language_generation != document.meta.language_generation
                    || feed.synchronized != synchronized
                {
                    feed.stream.open(*version);
                    feed.path.clone_from(path);
                    feed.language.clone_from(language);
                    feed.incarnation = document.incarnation;
                    feed.synchronized = synchronized;
                    feed.version = *version;
                    feed.language_generation = document.meta.language_generation;
                    feed.variant = Some(document.meta.lang.variant_name().to_owned());
                } else if feed.version != *version {
                    feed.stream.changed(feed.version, *version, 1);
                    feed.version = *version;
                }
                feed.language.clone_from(language);
            }
        }
        let open_files: Vec<FileId> = current.iter().map(|(file, ..)| *file).collect();
        let closed: Vec<u64> = self
            .seen
            .iter()
            .filter(|(file, _)| !open_files.contains(file))
            .map(|(_, id)| *id)
            .collect();
        self.seen.retain(|file, _| open_files.contains(file));
        self.provider_seen
            .retain(|(_, _, document), _| ids.contains(document));
        for id in closed {
            for package in self.packages.values_mut() {
                if let Some(feed) = package.feeds.get_mut(&id) {
                    feed.stream.close();
                }
            }
        }

        let mut effects = Vec::new();
        for (name, package) in self
            .packages
            .iter_mut()
            .filter(|(_, p)| document_events_of(grants, p))
        {
            let wants_open = wants(&package.manifest, "document.open");
            let wants_change = wants(&package.manifest, "document.change");
            let wants_close = wants(&package.manifest, "document.close");
            let relevant = |d: &Delivery| match d {
                Delivery::Opened { .. } => wants_open,
                Delivery::Changed { .. } | Delivery::ResyncRequired { .. } => wants_change,
                Delivery::Closed { .. } => wants_close,
            };
            match package.lifecycle.state() {
                State::Active => {
                    let generation = package.lifecycle.generation();
                    let mut messages = Vec::new();
                    let mut references = Vec::new();
                    let mut finished = Vec::new();
                    let mut keys: Vec<u64> = package.feeds.keys().copied().collect();
                    keys.sort_unstable();
                    for key in keys {
                        let feed = package.feeds.get_mut(&key).expect("key exists");
                        let drained = feed.stream.drain();
                        let closed = drained.iter().any(|d| matches!(d, Delivery::Closed { .. }));
                        for delivery in drained.into_iter().filter(|d| relevant(d)) {
                            let reference = |lifecycle: u64, version: u64| DocumentRef {
                                document: key,
                                lifecycle,
                                path: feed.path.clone(),
                                language: feed.language.clone(),
                                variant: feed.variant.clone(),
                                version,
                            };
                            references.push(reference(feed.stream.lifecycle(), feed.version));
                            messages.push(match delivery {
                                Delivery::Opened { lifecycle, version } => {
                                    Inbound::DocumentOpened(reference(lifecycle, version))
                                }
                                Delivery::Changed {
                                    lifecycle,
                                    from,
                                    to,
                                    edits,
                                } => Inbound::DocumentChanged(WireChange {
                                    document: key,
                                    lifecycle,
                                    from_version: from,
                                    to_version: to,
                                    edits: edits as u32,
                                }),
                                Delivery::ResyncRequired { lifecycle, version } => {
                                    Inbound::DocumentResync(reference(lifecycle, version))
                                }
                                Delivery::Closed { lifecycle } => {
                                    Inbound::DocumentClosed(reference(lifecycle, feed.version))
                                }
                            });
                        }
                        if closed && !ids.contains(&key) {
                            finished.push(key);
                        }
                    }
                    for key in finished {
                        package.feeds.remove(&key);
                    }
                    if !messages.is_empty() {
                        effects.push(Effect::Submit(Job::DocumentEvents {
                            id: name.clone(),
                            generation,
                            references,
                            messages,
                        }));
                    }
                }
                State::Dormant => {
                    // A declared event is a reason to wake the extension (design §8.1).
                    let due = package
                        .feeds
                        .values()
                        .any(|feed| feed.stream.peek().iter().any(&relevant));
                    if due {
                        let generation = package.lifecycle.generation();
                        let _ = package.lifecycle.activating();
                        effects.push(Effect::Submit(Job::Load {
                            id: name.clone(),
                            generation,
                            component: package.component.clone(),
                            package_dir: package.dir.clone(),
                            version: package.manifest.version.clone(),
                            cache: self.cache.clone(),
                            limits: self.guest_limits,
                            storage: storage_of(&self.grants, package)
                                .then(|| self.storage_dir.join(&package.manifest.id)),
                            files: files_of(&self.grants, package),
                            domains: domains_of(&self.grants, package),
                        }));
                    }
                }
                _ => {}
            }
        }
        if has_providers {
            self.request_diagnostics(app, &current, &mut effects);
        }
        effects
    }

    fn source_for(&mut self, extension: &str, provider: &str) -> u64 {
        static SOURCES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let key = (extension.to_owned(), provider.to_owned());
        if let Some(source) = self.sources.get(&key) {
            return *source;
        }
        let source = EXTERNAL_SOURCE | SOURCES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.sources.insert(key, source);
        source
    }

    /// Withdraw what an extension published; its provider calls are void.
    fn clear_sources(&mut self, extension: &str) -> Vec<Effect> {
        if let Some(package) = self.packages.get_mut(extension) {
            package.document_views.clear();
            package.feeds.clear();
            package.documents_initialized = false;
            package.provider_configurations.clear();
            package.provider_revision += 1;
            package.pending_saved.clear();
            package.pending_files.clear();
        }
        // Language requests waiting on this extension are told there is no answer.
        let mut gone = vec![];
        let chains = self
            .save_calls
            .iter()
            .filter(|(request, _)| {
                self.provider_calls
                    .get(request)
                    .is_some_and(|call| call.extension == extension)
            })
            .map(|(_, chain)| *chain)
            .collect::<Vec<_>>();
        for chain in chains {
            self.fail(chain, format!("{extension} is not running"), &mut gone);
        }
        if self.ui_events.remove(extension).is_some() {
            gone.push(Effect::ClearUi {
                session: self.ui_session,
                owner: extension.to_owned(),
            });
        }
        for (request, call) in &self.provider_calls {
            if call.extension == extension
                && let Some(token) = self.language_calls.get(request)
            {
                gone.push(Effect::LanguageAnswer {
                    token: *token,
                    answer: None,
                });
            }
        }
        self.language_calls.retain(|request, _| {
            self.provider_calls
                .get(request)
                .is_none_or(|c| c.extension != extension)
        });
        self.provider_calls
            .retain(|_, call| call.extension != extension);
        self.provider_seen
            .retain(|(owner, _, _), _| owner != extension);
        self.queued
            .retain(|request| self.provider_calls.contains_key(request));
        gone.extend(
            self.sources
                .iter()
                .filter(|((owner, _), _)| owner == extension)
                .map(|(_, source)| Effect::ClearDiagnostics { source: *source }),
        );
        self.sources.retain(|(owner, _), _| owner != extension);
        gone
    }

    /// The providers that answer `feature` for documents in `language`, as
    /// (extension, provider id). Only packages that may read documents and
    /// declared the language capability count.
    pub fn language_providers(
        &self,
        feature: crate::features::language_service::request::Feature,
        language: &str,
    ) -> Vec<(String, String)> {
        let Some((_, declared)) = language_kinds(feature) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for (owner, package) in &self.packages {
            let allowed = ["documents.read", "languages"]
                .iter()
                .all(|c| self.authorize(package, c).is_ok());
            let allowed = allowed
                && (!matches!(
                    feature,
                    crate::features::language_service::request::Feature::Formatting
                        | crate::features::language_service::request::Feature::RangeFormatting
                        | crate::features::language_service::request::Feature::OnTypeFormatting
                        | crate::features::language_service::request::Feature::Rename
                ) || self.authorize(package, "workspace.edit").is_ok());
            if !allowed || !package.manifest.has_behaviour() {
                continue;
            }
            for provider in &package.manifest.contributes.language_service.providers {
                if provider.kind == declared
                    && super::extensions::language::supports_mode(feature, package.provider_configurations.get(&provider.id))
                    && (feature == crate::features::language_service::request::Feature::WorkspaceSymbol || provider.selector.languages.iter().any(|l| l == language))
                    && !matches!(package.lifecycle.state(), State::Stopped | State::Failed)
                    && package.provider_configurations.get(&provider.id).is_none_or(|c| c.enabled && (feature != crate::features::language_service::request::Feature::PrepareRename || c.prepare) && (!matches!(feature, crate::features::language_service::request::Feature::CodeActionResolve | crate::features::language_service::request::Feature::CodeLensResolve) || c.resolve))
                {
                    found.push((owner.clone(), provider.id.clone()));
                }
            }
        }
        found.sort();
        found
    }

    pub(super) fn matching_language_providers(
        &self,
        feature: crate::features::language_service::request::Feature,
        language: &str,
        target: Option<u64>,
        server: Option<&str>,
    ) -> Vec<(String, String)> {
        let mut providers = self.language_providers(feature, language).into_iter().filter(|(owner, provider)| {
            target.is_none_or(|id| self.sources.iter().any(|((source_owner, source_provider), source)| *source == id && source_owner == owner && (source_provider == provider || feature == crate::features::language_service::request::Feature::ExecuteCommand))) && server.is_none_or(|name| name == owner || name == provider || self.packages[owner].provider_configurations.get(provider).is_some_and(|c|c.aliases.iter().any(|alias|alias == name)))
        }).collect::<Vec<_>>();
        if target.is_none()
            && server.is_none()
            && let Some((_, kind)) = language_kinds(feature)
            && let Ok(serde_json::Value::String(key)) = serde_json::to_value(kind)
            && let Some(order) = self
                .language_preferences
                .get(language)
                .and_then(|s| s.providers.get(&key))
            && !order.is_empty()
        {
            let mut arranged = Vec::new();
            for wanted in order {
                for candidate in &providers {
                    let named = |id: &str| id == candidate.0 || id == candidate.1;
                    if (named(wanted) || (wanted == "..." && !order.iter().any(|id| named(id))))
                        && !arranged.contains(candidate)
                    {
                        arranged.push(candidate.clone());
                    }
                }
            }
            providers = arranged;
        }
        if feature.merge() == crate::features::language_service::request::Merge::Primary {
            providers.truncate(1);
        }
        providers
    }

    /// Ask the providers for a language feature of one state of a document.
    /// Returns how many were asked; each of those ends in an
    /// [`Effect::LanguageAnswer`] with `token`, also when it fails or is cut off.
    pub fn request_language(
        &mut self,
        app: &AppState,
        ask: &LanguageAsk,
        token: u64,
        effects: &mut Vec<Effect>,
    ) -> usize {
        let Some((kind, _)) = language_kinds(ask.feature) else {
            return 0;
        };
        let found = app
            .workspace
            .documents
            .iter()
            .find(|(_, d)| d.meta.path == ask.path);
        let Some((file, document)) = found.map(|(file, d)| (*file, d)) else {
            return 0;
        };
        if document.buffer.len()
            > app
                .preferences
                .values()
                .language_features
                .max_document_bytes
        {
            return 0;
        }
        // Only the state the request was made for; a document that moved on is not asked.
        let now = document.stamp(file);
        if (now.incarnation, now.version) != (ask.stamp.incarnation, ask.stamp.version) {
            return 0;
        }
        let language = document.meta.lang.definition().name.to_string();
        let doc_id = match self.seen.get(&file) {
            Some(id) => *id,
            None => {
                self.next_document += 1;
                self.seen.insert(file, self.next_document);
                self.next_document
            }
        };
        let text = document.buffer.text_snapshot();
        let range = match ask.feature {
            crate::features::language_service::request::Feature::InlayHint
            | crate::features::language_service::request::Feature::CodeAction
            | crate::features::language_service::request::Feature::RangeFormatting => {
                let (start, end) = (ask.start.min(text.len()), ask.end.min(text.len()));
                Some(start.min(end)..start.max(end))
            }
            _ => None,
        };
        let mut asked = 0;
        for (rank, (owner, provider)) in self
            .matching_language_providers(ask.feature, &language, ask.target, ask.server.as_deref())
            .into_iter()
            .enumerate()
        {
            let target = Target {
                file,
                stamp: document.stamp(file),
                path: document.meta.path.clone(),
                text: text.clone(),
                doc_id,
                version: document.buffer.version(),
                range: range.clone(),
                word_start: crate::features::language_service::completion::word_start(
                    &text,
                    ask.start.min(text.len()),
                ),
                cursor: Some(ask.start),
                trigger: ask
                    .extra
                    .get("ch")
                    .or_else(|| ask.extra.pointer("/context/triggerCharacter"))
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
                item: (ask.feature
                    == crate::features::language_service::request::Feature::CompletionResolve)
                    .then(|| language::completion_item(&ask.extra))
                    .flatten(),
                item_raw: (ask.feature
                    == crate::features::language_service::request::Feature::CompletionResolve)
                    .then(|| ask.extra.clone()),
            };
            let request = self.next_request + 1;
            self.issue_context(
                &owner,
                &provider,
                kind,
                target,
                Some(language::context(ask, &text)),
                Some(
                    ask.timeout
                        .unwrap_or(crate::features::providers::DEFAULT_TIMEOUT)
                        .as_millis()
                        .min(u64::MAX as u128) as u64,
                ),
                effects,
            );
            if self.provider_calls.contains_key(&request) {
                self.provider_calls.get_mut(&request).unwrap().rank = COMPLETION_RANK + rank;
                self.language_calls.insert(request, token);
                asked += 1;
            }
        }
        asked
    }

    /// Turn a provider's answer to a language request into the reply a server
    /// would give. `None` when it is late, for another state, or empty.
    fn language_reply(
        &mut self,
        app: &AppState,
        id: &str,
        call: &ProviderCall,
        answer: Provided,
    ) -> Option<LanguageAnswer> {
        use providers::language as lang;
        let current = app
            .workspace
            .documents
            .get(&call.file)
            .map(|d| d.stamp(call.file));
        let completion = matches!(
            call.job.kind,
            WireKind::Completion | WireKind::CompletionResolve
        );
        let compatible = if matches!(call.job.kind, WireKind::ExecuteCommand) {
            current.is_some_and(|stamp| {
                stamp.incarnation == call.stamp.incarnation
                    && stamp.language_generation == call.stamp.language_generation
            })
        } else if completion {
            current.is_some_and(|stamp| stamp.incarnation == call.stamp.incarnation)
        } else {
            current == Some(call.stamp)
        };
        if (self.clock)() > call.deadline || !compatible {
            return None;
        }
        let text = call.text.as_str();
        let (value, dropped) = match answer {
            Provided::Completions {
                items, incomplete, ..
            } => {
                let index = self.position_indices.index(&call.text);
                let (items, dropped) =
                    language::completions(&call.extension, &call.provider, text, &index, items);
                (
                    if matches!(call.job.kind, WireKind::CompletionResolve) {
                        items.into_iter().next()?
                    } else {
                        let mut list = serde_json::Map::with_capacity(2);
                        list.insert("isIncomplete".into(), incomplete.into());
                        list.insert("items".into(), serde_json::Value::Array(items));
                        serde_json::Value::Object(list)
                    },
                    dropped,
                )
            }
            Provided::Language { answer, .. } => {
                self.validate_language_answer(app, self.packages.get(id)?, &answer)
                    .ok()?;
                (
                    language::answer(text, answer, call.job.context.as_ref())?,
                    0,
                )
            }
            Provided::Edits { mut edits, .. } => {
                if let Some(range) = &call.range {
                    edits.retain(|e| {
                        e.range.start >= range.start as u64 && e.range.end <= range.end as u64
                    });
                }
                (language::edits(text, edits)?, 0)
            }
            Provided::Hover { answer, .. } => (
                lang::hover_indexed(
                    text,
                    &lang::Hover {
                        contents: answer.as_ref()?.contents.clone(),
                        range: answer
                            .as_ref()?
                            .range
                            .map(|r| (r.start as usize, r.end as usize)),
                    },
                    &self.position_indices.index(&call.text),
                )?,
                0,
            ),
            Provided::Locations { items, .. } => {
                let places: Vec<lang::Place> = items
                    .into_iter()
                    .map(|l| lang::Place {
                        path: l.path,
                        line: l.line,
                        column: l.column,
                    })
                    .collect();
                let base = call.path.parent().map(PathBuf::from).unwrap_or_default();
                lang::locations(&base, &places)
            }
            Provided::Symbols { items, .. } => {
                let deprecated = items
                    .iter()
                    .map(|item| item.deprecated.unwrap_or(false))
                    .collect::<Vec<_>>();
                let symbols: Vec<lang::Symbol> = items
                    .into_iter()
                    .map(|s| lang::Symbol {
                        name: s.name,
                        detail: s.detail,
                        kind: s.kind,
                        range: (s.range.start as usize, s.range.end as usize),
                        selection: (s.selection.start as usize, s.selection.end as usize),
                        parent: s.parent.map(|p| p as usize),
                    })
                    .collect();
                lang::symbols_with_deprecated(text, &symbols, &deprecated)
            }
            Provided::Signature { answer, .. } => {
                let answer = answer?;
                (
                    lang::signature(&lang::Signature {
                        label: answer.label,
                        documentation: answer.documentation,
                        parameters: answer
                            .parameters
                            .into_iter()
                            .map(|p| (p.label, p.documentation))
                            .collect(),
                        active: answer.active_parameter,
                    })?,
                    0,
                )
            }
            Provided::InlayHints { items, .. } => {
                let hints: Vec<lang::Hint> = items
                    .into_iter()
                    .filter(|h| {
                        call.range.as_ref().is_none_or(|range| {
                            range.contains(&(h.offset as usize)) || range.end == h.offset as usize
                        })
                    })
                    .map(|h| lang::Hint {
                        offset: h.offset as usize,
                        label: h.label,
                        kind: h.kind,
                        padding_left: h.padding_left,
                        padding_right: h.padding_right,
                        tooltip: h.tooltip,
                    })
                    .collect();
                lang::hints(text, &hints)
            }
            Provided::CodeActions { items, .. } => {
                let actions: Vec<lang::Action> = items
                    .into_iter()
                    .map(|a| lang::Action {
                        title: a.title,
                        kind: a.kind,
                        preferred: a.preferred,
                        edits: a
                            .edits
                            .into_iter()
                            .map(|e| (e.range.start as usize, e.range.end as usize, e.text))
                            .collect(),
                    })
                    .collect();
                let uri = crate::features::language_service::uri::file_uri(&call.path)?;
                lang::code_actions(text, &uri, &actions)
            }
            _ => return None,
        };
        if dropped > 0
            && let Some(package) = self.packages.get_mut(id)
        {
            let note =
                format!("{dropped} entr(ies) of an answer could not be placed and were dropped");
            crate::features::output::extension_log(id, crate::features::output::Level::Warn, &note);
            package.logs.push((1, note));
        }
        let source = self.source_for(&call.extension, &call.provider);
        Some(LanguageAnswer {
            source,
            provider: call.provider.clone(),
            value,
            rank: call.rank,
        })
    }

    /// Ask one provider about one state of a document. A newer request for the
    /// same provider and document replaces an older one; waiting for a cold
    /// activation never extends the deadline fixed here (design §6.3).
    fn issue(
        &mut self,
        owner: &str,
        provider: &str,
        kind: WireKind,
        target: Target,
        effects: &mut Vec<Effect>,
    ) {
        self.issue_context(owner, provider, kind, target, None, None, effects);
    }

    fn issue_context(
        &mut self,
        owner: &str,
        provider: &str,
        kind: WireKind,
        target: Target,
        context: Option<loog_extension_host::ProviderContext>,
        timeout_ms: Option<u64>,
        effects: &mut Vec<Effect>,
    ) {
        self.next_request += 1;
        self.next_token += 1;
        let (request, token) = (self.next_request, self.next_token);
        let replaces = matches!(
            kind,
            WireKind::Diagnostics
                | WireKind::Highlights
                | WireKind::Completion
                | WireKind::Hover
                | WireKind::SignatureHelp
                | WireKind::DocumentHighlights
                | WireKind::CodeActions
                | WireKind::InlayHints
                | WireKind::FoldingRanges
                | WireKind::CodeLens
                | WireKind::DocumentSymbols
                | WireKind::PrepareRename
        );
        let superseded = self
            .provider_calls
            .iter()
            .filter(|(_, call)| {
                replaces
                    && call.extension == owner
                    && call.provider == provider
                    && call.file == target.file
                    && std::mem::discriminant(&call.job.kind) == std::mem::discriminant(&kind)
            })
            .map(|(request, _)| *request)
            .collect::<Vec<_>>();
        for request in superseded {
            self.cancel_provider(request, effects);
        }
        self.queued
            .retain(|queued| self.provider_calls.contains_key(queued));
        // One extension never has more than this many requests open at once; the
        // newest is refused, as if it had no answer, rather than queued without end.
        let open = self
            .provider_calls
            .values()
            .filter(|call| call.extension == owner)
            .count();
        if open >= MAX_PROVIDER_CALLS {
            if let Some(package) = self.packages.get_mut(owner) {
                let note = format!("too many open requests; {provider} was not asked");
                crate::features::output::extension_log(
                    owner,
                    crate::features::output::Level::Warn,
                    &note,
                );
                package.logs.push((1, note));
            }
            return;
        }
        let now = (self.clock)();
        let limit = timeout_ms.unwrap_or(
            if matches!(
                kind,
                WireKind::Formatting | WireKind::Save | WireKind::Highlights
            ) {
                FORMAT_DEADLINE_MS
            } else {
                PROVIDER_DEADLINE_MS
            },
        );
        let Some(package) = self.packages.get_mut(owner) else {
            return;
        };
        if package
            .provider_configurations
            .get(provider)
            .is_some_and(|c| !c.enabled)
        {
            return;
        }
        let generation = package.lifecycle.generation();
        let job = ProviderJob {
            request,
            provider: provider.to_owned(),
            kind,
            token,
            text: target.text.clone(),
            document: target.doc_id,
            version: target.version,
            range: target.range.clone(),
            cursor: target.cursor,
            trigger: target.trigger.clone(),
            item: target.item.clone(),
            context,
            timeout_ms: limit,
        };
        let mut load = None;
        match package.lifecycle.state() {
            State::Active => effects.push(Effect::Submit(Job::Provide {
                id: owner.to_owned(),
                generation,
                job: Box::new(job.clone()),
            })),
            State::Dormant => {
                let _ = package.lifecycle.activating();
                load = Some(Job::Load {
                    id: owner.to_owned(),
                    generation,
                    component: package.component.clone(),
                    package_dir: package.dir.clone(),
                    version: package.manifest.version.clone(),
                    cache: self.cache.clone(),
                    limits: self.guest_limits,
                    storage: storage_of(&self.grants, package)
                        .then(|| self.storage_dir.join(&package.manifest.id)),
                    files: files_of(&self.grants, package),
                    domains: domains_of(&self.grants, package),
                });
                self.queued.push(request);
            }
            State::Activating => self.queued.push(request),
            _ => return,
        }
        effects.extend(load.map(Effect::Submit));
        self.provider_calls.insert(
            request,
            ProviderCall {
                extension: owner.to_owned(),
                generation,
                provider: provider.to_owned(),
                file: target.file,
                stamp: target.stamp,
                path: target.path,
                text: target.text,
                doc_id: target.doc_id,
                deadline: now + limit,
                range: target.range,
                word_start: target.word_start,
                item_raw: target.item_raw,
                job,
                rank: COMPLETION_RANK,
            },
        );
    }

    /// Ask every completion provider registered for the active document's
    /// language. Returns the requests that were actually issued; the answers
    /// arrive as [`Effect::OfferCompletion`].
    #[cfg(test)]
    pub fn request_completion(
        &mut self,
        app: &AppState,
        word_start: usize,
        cursor: usize,
        trigger: Option<char>,
    ) -> (usize, Vec<Effect>) {
        let mut effects = Vec::new();
        let Some(file) = app.workspace.active_document() else {
            return (0, effects);
        };
        let Some(document) = app.workspace.documents.get(&file) else {
            return (0, effects);
        };
        let language = document.meta.lang.definition().name.to_string();
        let mut wanted: Vec<(String, String)> = Vec::new();
        for (owner, package) in &self.packages {
            let allowed = ["documents.read", "languages"]
                .iter()
                .all(|c| self.authorize(package, c).is_ok());
            if !allowed || !package.manifest.has_behaviour() {
                continue;
            }
            for provider in &package.manifest.contributes.language_service.providers {
                if provider.kind == DeclKind::Completion
                    && provider.selector.languages.contains(&language)
                {
                    wanted.push((owner.clone(), provider.id.clone()));
                }
            }
        }
        wanted.sort();
        let doc_id = match self.seen.get(&file) {
            Some(id) => *id,
            None => {
                self.next_document += 1;
                self.seen.insert(file, self.next_document);
                self.next_document
            }
        };
        let mut asked = 0;
        for (owner, provider) in wanted {
            let target = Target {
                file,
                stamp: document.stamp(file),
                path: document.meta.path.clone(),
                text: document.buffer.text_snapshot(),
                doc_id,
                version: document.buffer.version(),
                range: None,
                word_start,
                cursor: Some(cursor),
                trigger: trigger.map(|c| c.to_string()),
                item: None,
                item_raw: None,
            };
            let before = self.provider_calls.len();
            self.issue(
                &owner,
                &provider,
                WireKind::Completion,
                target,
                &mut effects,
            );
            if self.provider_calls.values().any(|c| {
                c.extension == owner
                    && c.provider == provider
                    && c.file == file
                    && matches!(c.job.kind, WireKind::Completion)
            }) {
                asked += 1;
            }
            let _ = before;
        }
        (asked, effects)
    }

    /// The snippets that packages contribute for the active document's language.
    /// Packages that were stopped offer nothing; snippets are data, so no grant
    /// is needed.
    pub fn snippet_offers(&mut self, app: &AppState, word_start: usize) -> Vec<Effect> {
        let Some(file) = app.workspace.active_document() else {
            return Vec::new();
        };
        let Some(document) = app.workspace.documents.get(&file) else {
            return Vec::new();
        };
        let language = document.meta.lang.definition().name.to_string();
        let incarnation = document.stamp(file).incarnation;
        let mut found: Vec<(String, String, Vec<serde_json::Value>)> = Vec::new();
        for (owner, package) in &self.packages {
            if package.lifecycle.state() == State::Stopped {
                continue;
            }
            for (id, snippets) in &package.snippets {
                if snippets.language == language {
                    found.push((
                        owner.clone(),
                        id.clone(),
                        crate::features::extensions::snippets::completion_items(snippets),
                    ));
                }
            }
        }
        found.sort_by(|a, b| a.1.cmp(&b.1));
        found
            .into_iter()
            .map(|(owner, id, items)| Effect::OfferSnippets {
                file,
                incarnation,
                word_start,
                source: self.source_for(&owner, &id),
                rank: COMPLETION_RANK,
                items,
            })
            .collect()
    }

    /// Execute an accepted item's command only in its owning provider instance.
    pub fn execute_provider_command(&self, source: u64, raw: &serde_json::Value) -> Vec<Effect> {
        let origin = &raw["x-loog"];
        let (Some(extension), Some(provider), Some(name)) = (
            origin["extension"].as_str(),
            origin["provider"].as_str(),
            raw["command"]["command"].as_str(),
        ) else {
            return vec![];
        };
        if self
            .sources
            .get(&(extension.to_owned(), provider.to_owned()))
            != Some(&source)
        {
            return vec![];
        }
        let Some(package) = self
            .packages
            .get(extension)
            .filter(|package| package.lifecycle.accepts(package.lifecycle.generation()))
        else {
            return vec![];
        };
        vec![Effect::Submit(Job::Deliver {
            id: extension.to_owned(),
            generation: package.lifecycle.generation(),
            messages: vec![Inbound::ProviderCommand(
                loog_extension_host::ProviderCommandCall {
                    provider: provider.to_owned(),
                    command: loog_extension_host::ProviderCommand {
                        name: name.to_owned(),
                        arguments: raw["command"]["arguments"].to_string(),
                    },
                },
            )],
        })]
    }

    /// Ask the extension that offered a completion item to complete it. The
    /// `raw` JSON carries the origin; an item whose origin does not match
    /// the source it is filed under is not resolved.
    #[cfg(test)]
    pub fn request_completion_resolve(
        &mut self,
        app: &AppState,
        source: u64,
        raw: &serde_json::Value,
    ) -> Vec<Effect> {
        let mut effects = Vec::new();
        let origin = &raw["x-loog"];
        let (Some(extension), Some(provider)) =
            (origin["extension"].as_str(), origin["provider"].as_str())
        else {
            return effects;
        };
        if self
            .sources
            .get(&(extension.to_owned(), provider.to_owned()))
            != Some(&source)
        {
            return effects;
        }
        let Some(file) = app.workspace.active_document() else {
            return effects;
        };
        let Some(document) = app.workspace.documents.get(&file) else {
            return effects;
        };
        let text_of = |key: &str| raw[key].as_str().map(str::to_owned);
        let edit_of = |value: &serde_json::Value| {
            Some(loog_extension_host::TextEdit {
                range: loog_extension_host::ByteRange {
                    start: value["start"].as_u64()?,
                    end: value["end"].as_u64()?,
                },
                text: value["text"].as_str()?.to_owned(),
            })
        };
        let item = loog_extension_host::WireCompletionItem {
            label_detail: raw
                .pointer("/labelDetails/detail")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            label_description: raw
                .pointer("/labelDetails/description")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            deprecated: raw["deprecated"].as_bool(),
            preselect: raw["preselect"].as_bool(),
            insert_text_mode: raw["insertTextMode"]
                .as_u64()
                .and_then(|value| u8::try_from(value).ok()),
            label: text_of("label").unwrap_or_default(),
            detail: text_of("detail"),
            documentation: text_of("documentation"),
            kind: raw["kind"].as_u64().map(|k| k as u32),
            insert_text: text_of("insertText"),
            text_edit: edit_of(&origin["text-edit"]),
            additional_text_edits: origin["additional-text-edits"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(&edit_of)
                .collect(),
            insert_text_format: raw["insertTextFormat"]
                .as_u64()
                .and_then(|v| u8::try_from(v).ok()),
            commit_characters: raw["commitCharacters"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| s.as_str().map(str::to_owned))
                .collect(),
            command: raw.get("command").and_then(|c| {
                Some(loog_extension_host::ProviderCommand {
                    name: c["command"].as_str()?.to_owned(),
                    arguments: c["arguments"].to_string(),
                })
            }),
            sort_text: text_of("sortText"),
            filter_text: text_of("filterText"),
            data: origin["data"].as_str().map(str::to_owned),
        };
        let doc_id = self.seen.get(&file).copied().unwrap_or(0);
        let target = Target {
            file,
            stamp: document.stamp(file),
            path: document.meta.path.clone(),
            text: document.buffer.text_snapshot(),
            doc_id,
            version: document.buffer.version(),
            range: None,
            word_start: 0,
            cursor: None,
            trigger: None,
            item: Some(item),
            item_raw: Some(raw.clone()),
        };
        self.issue(
            extension,
            provider,
            WireKind::CompletionResolve,
            target,
            &mut effects,
        );
        effects
    }

    /// Request formatting from an extension provider for the active document,
    /// if one is registered for its language. The extension provider wins over
    /// the language server for that language (one provider, never several).
    /// \`None\` means no extension provider applies.
    #[cfg(test)]
    pub fn request_format(&mut self, app: &AppState, selection_only: bool) -> Option<Vec<Effect>> {
        let file = app.workspace.active_document()?;
        let editor = app.workspace.active_editor()?;
        let document = app.workspace.documents.get(&file)?;
        let language = document.meta.lang.definition().name.to_string();
        let mut owners: Vec<&String> = self.packages.keys().collect();
        owners.sort();
        let (owner, provider) = owners.into_iter().find_map(|owner| {
            let package = &self.packages[owner];
            let allowed = ["documents.read", "workspace.edit", "languages"]
                .iter()
                .all(|c| self.authorize(package, c).is_ok());
            allowed
                .then(|| {
                    package
                        .manifest
                        .contributes
                        .language_service
                        .providers
                        .iter()
                        .find(|p| {
                            p.kind == DeclKind::Formatting
                                && p.selector.languages.contains(&language)
                        })
                        .map(|p| (owner.clone(), p.id.clone()))
                })
                .flatten()
        })?;
        let range = if selection_only {
            editor.selection()
        } else {
            None
        };
        let doc_id = match self.seen.get(&file) {
            Some(id) => *id,
            None => {
                self.next_document += 1;
                self.seen.insert(file, self.next_document);
                self.next_document
            }
        };
        let target = Target {
            file,
            stamp: document.stamp(file),
            path: document.meta.path.clone(),
            text: document.buffer.text_snapshot(),
            doc_id,
            version: document.buffer.version(),
            range,
            word_start: 0,
            cursor: None,
            trigger: None,
            item: None,
            item_raw: None,
        };
        let mut effects = Vec::new();
        self.issue(
            &owner,
            &provider,
            WireKind::Formatting,
            target,
            &mut effects,
        );
        Some(effects)
    }

    fn provided(
        &mut self,
        app: &mut AppState,
        id: &str,
        generation: u64,
        answer: Provided,
    ) -> Vec<Effect> {
        let request = answer.request();
        let Some(call) = self.provider_calls.get(&request) else {
            return Vec::new();
        };
        if call.extension != id || call.generation != generation {
            return Vec::new();
        }
        let call = self.provider_calls.remove(&request).expect("checked above");
        let language = self.language_calls.remove(&request);
        if !self
            .packages
            .get(id)
            .is_some_and(|p| p.lifecycle.accepts(generation))
        {
            return language
                .map(|token| Effect::LanguageAnswer {
                    token,
                    answer: None,
                })
                .into_iter()
                .collect();
        }
        if let Some(token) = language {
            // An answer of the wrong shape for its request is no answer.
            let fits = matches!(
                (&call.job.kind, &answer),
                (WireKind::Hover, Provided::Hover { .. })
                    | (
                        WireKind::Completion | WireKind::CompletionResolve,
                        Provided::Completions { .. }
                    )
                    | (WireKind::Definition, Provided::Locations { .. })
                    | (WireKind::DocumentSymbols, Provided::Symbols { .. })
                    | (WireKind::SignatureHelp, Provided::Signature { .. })
                    | (WireKind::InlayHints, Provided::InlayHints { .. })
                    | (WireKind::CodeActions, Provided::CodeActions { .. })
            ) || language::fits(&call.job.kind, &answer);
            let answer = if fits {
                self.language_reply(app, id, &call, answer)
            } else {
                None
            };
            return vec![Effect::LanguageAnswer { token, answer }];
        }
        if matches!(call.job.kind, WireKind::Save) {
            return match answer {
                Provided::Edits { edits, .. } => self.save_answered(app, request, edits),
                _ => {
                    let mut effects = Vec::new();
                    if let Some(chain) = self.save_calls.remove(&request) {
                        self.fail(
                            chain,
                            "unexpected answer from a save participant".into(),
                            &mut effects,
                        );
                    }
                    effects
                }
            };
        }
        let formatting = matches!(call.job.kind, WireKind::Formatting);
        let completion = matches!(
            call.job.kind,
            WireKind::Completion | WireKind::CompletionResolve
        );
        if completion {
            // Typing continues while a provider works, so the text may have moved on;
            // the answer still serves the session while the word being completed is
            // the same. A document that was closed and reopened is a different one.
            let same_life = app
                .workspace
                .documents
                .get(&call.file)
                .is_some_and(|d| d.stamp(call.file).incarnation == call.stamp.incarnation);
            let source = self.source_for(&call.extension, &call.provider);
            let offer = |items| Effect::OfferCompletion {
                file: call.file,
                incarnation: call.stamp.incarnation,
                word_start: call.word_start,
                source,
                rank: COMPLETION_RANK,
                items,
            };
            let on_time = (self.clock)() <= call.deadline && same_life;
            return match answer {
                Provided::Completions {
                    items, incomplete, ..
                } => {
                    let offered: Vec<providers::Offered> = items
                        .into_iter()
                        .map(|i| providers::Offered {
                            label: i.label,
                            label_detail: i.label_detail,
                            label_description: i.label_description,
                            deprecated: i.deprecated,
                            preselect: i.preselect,
                            insert_text_mode: i.insert_text_mode,
                            detail: i.detail,
                            documentation: i.documentation,
                            kind: i.kind,
                            insert_text: i.insert_text,
                            text_edit: i.text_edit.map(|edit| {
                                (
                                    edit.range.start as usize..edit.range.end as usize,
                                    edit.text,
                                )
                            }),
                            additional_text_edits: i
                                .additional_text_edits
                                .into_iter()
                                .map(|edit| {
                                    (
                                        edit.range.start as usize..edit.range.end as usize,
                                        edit.text,
                                    )
                                })
                                .collect(),
                            insert_text_format: i.insert_text_format,
                            commit_characters: i.commit_characters,
                            command: i.command.map(|command| (command.name, command.arguments)),
                            sort_text: i.sort_text,
                            filter_text: i.filter_text,
                            data: i.data,
                        })
                        .collect();
                    let (json, dropped) = providers::completion_json(
                        &call.extension,
                        &call.provider,
                        &call.text,
                        &offered,
                    );
                    if dropped > 0
                        && let Some(package) = self.packages.get_mut(id)
                    {
                        package
                            .logs
                            .push((1, format!("{dropped} completion item(s) were dropped")));
                    }
                    match (call.job.kind, call.item_raw.clone()) {
                        (WireKind::CompletionResolve, Some(raw)) if on_time => json
                            .into_iter()
                            .next()
                            .map(|item| vec![Effect::ResolvedCompletion { source, raw, item }])
                            .unwrap_or_default(),
                        (WireKind::CompletionResolve, _) => Vec::new(),
                        _ if on_time => vec![offer(Some((json, incomplete)))],
                        _ => vec![offer(None)],
                    }
                }
                _ => vec![offer(None)],
            };
        }
        // An answer that came too late, or describes a state that is gone, is
        // never shown. For diagnostics the current state is asked about again, so
        // a slow cold start still ends with findings for what is on screen.
        let too_late = (self.clock)() > call.deadline;
        let current = app
            .workspace
            .documents
            .get(&call.file)
            .map(|d| d.stamp(call.file));
        if too_late || current != Some(call.stamp) {
            self.provider_seen.remove(&(
                call.extension.clone(),
                call.provider.clone(),
                call.doc_id,
            ));
            if formatting {
                return vec![Effect::Error(if too_late {
                    "Format: the provider answered too late".into()
                } else {
                    "Format: the document changed while the provider worked".into()
                })];
            }
            let mut effects = Vec::new();
            if let Some(document) = app.workspace.documents.get(&call.file) {
                let version = document.buffer.version();
                self.provider_seen.insert(
                    (call.extension.clone(), call.provider.clone(), call.doc_id),
                    document.stamp(call.file),
                );
                let target = Target {
                    file: call.file,
                    stamp: document.stamp(call.file),
                    path: call.path.clone(),
                    text: document.buffer.text_snapshot(),
                    doc_id: call.doc_id,
                    version,
                    range: None,
                    word_start: 0,
                    cursor: None,
                    trigger: None,
                    item: None,
                    item_raw: None,
                };
                self.issue(
                    &call.extension,
                    &call.provider,
                    call.job.kind,
                    target,
                    &mut effects,
                );
            }
            return effects;
        }
        match answer {
            Provided::Language {
                answer: loog_extension_host::WireLanguageAnswer::Highlights(items),
                ..
            } if matches!(call.job.kind, WireKind::Highlights) => {
                let mut spans = items
                    .into_iter()
                    .filter_map(|item| {
                        let start = usize::try_from(item.range.start).ok()?;
                        let end = usize::try_from(item.range.end).ok()?;
                        if start >= end
                            || end > call.text.len()
                            || !call.text.is_char_boundary(start)
                            || !call.text.is_char_boundary(end)
                        {
                            return None;
                        }
                        Some(crate::features::syntax::TokenSpan {
                            range: start..end,
                            class: Some(crate::features::highlighting::HighlightId::from_name(
                                &item.theme,
                            )?),
                        })
                    })
                    .collect::<Vec<_>>();
                spans.sort_by_key(|s| (s.range.start, s.range.end));
                if spans
                    .windows(2)
                    .any(|pair| pair[0].range.end > pair[1].range.start)
                {
                    return vec![Effect::Error(format!("{id}: overlapping highlight ranges"))];
                }
                let source = self.source_for(&call.extension, &call.provider);
                vec![Effect::PublishHighlights {
                    source,
                    path: call.path,
                    stamp: call.stamp,
                    length: call.text.len(),
                    items: spans,
                }]
            }
            Provided::Diagnostics { items, .. } if !formatting => {
                let reported: Vec<Reported> = items
                    .into_iter()
                    .map(|d| Reported {
                        start: d.range.start as usize,
                        end: d.range.end as usize,
                        severity: d.severity,
                        message: d.message,
                        source: d.source,
                        code: d.code,
                        documentation_url: d.documentation_url,
                    })
                    .collect();
                let converted = providers::convert(&call.text, &reported, &call.provider);
                let source = self.source_for(&call.extension, &call.provider);
                let mut effects = vec![Effect::PublishDiagnostics {
                    source,
                    path: call.path.clone(),
                    stamp: call.stamp,
                    text: call.text.clone(),
                    items: converted.diagnostics,
                }];
                if converted.dropped > 0
                    && let Some(package) = self.packages.get_mut(id)
                {
                    let note = format!(
                        "{} diagnostic(s) could not be placed and were dropped",
                        converted.dropped
                    );
                    crate::features::output::extension_log(
                        id,
                        crate::features::output::Level::Warn,
                        &note,
                    );
                    package.logs.push((1, note));
                }
                effects.shrink_to_fit();
                effects
            }
            Provided::Edits { edits, .. } if formatting => {
                let mut changes: Vec<(Range<usize>, String)> = edits
                    .into_iter()
                    .map(|e| (e.range.start as usize..e.range.end as usize, e.text))
                    .collect();
                if let Some(range) = &call.range {
                    changes = providers::within(changes, range);
                }
                if changes.is_empty() {
                    return vec![Effect::Toast("Format: nothing to change".into())];
                }
                let transaction = TextTransaction {
                    title: "Format".into(),
                    documents: vec![DocumentChange {
                        file: call.file,
                        stamp: call.stamp,
                        edits: changes,
                    }],
                };
                match workspace_edits::commit(app, &transaction, &self.limits) {
                    Ok(_) => Vec::new(),
                    Err(error) => vec![Effect::Error(format!("Format: {error}"))],
                }
            }
            _ => vec![Effect::Error(format!(
                "{id}: the answer does not match the request"
            ))],
        }
    }

    /// Ask diagnostics providers about documents whose state they have not seen.
    fn request_diagnostics(
        &mut self,
        app: &AppState,
        current: &[(FileId, String, String, u64)],
        effects: &mut Vec<Effect>,
    ) {
        let owners: Vec<(String, String, Vec<String>, DeclKind)> = self
            .packages
            .iter()
            .filter(|(_, p)| {
                !matches!(
                    p.lifecycle.state(),
                    State::Stopped | State::Failed | State::Stopping
                ) && ["documents.read", "languages"]
                    .iter()
                    .all(|c| self.authorize(p, c).is_ok())
            })
            .flat_map(|(owner, p)| {
                p.manifest
                    .contributes
                    .language_service
                    .providers
                    .iter()
                    .filter(|d| {
                        matches!(d.kind, DeclKind::Diagnostics | DeclKind::Highlights)
                            && p.provider_configurations
                                .get(&d.id)
                                .is_none_or(|c| c.enabled)
                    })
                    .map(move |d| {
                        (
                            owner.clone(),
                            d.id.clone(),
                            d.selector.languages.clone(),
                            d.kind,
                        )
                    })
            })
            .collect();
        for (file, path, language, version) in current {
            if app.workspace.documents[file].buffer.len()
                > app
                    .preferences
                    .values()
                    .language_features
                    .max_document_bytes
            {
                continue;
            }
            let Some(doc_id) = self.seen.get(file).copied() else {
                continue;
            };
            for (owner, provider, languages, kind) in &owners {
                if !languages.contains(language) {
                    continue;
                }
                if self
                    .provider_calls
                    .values()
                    .filter(|call| &call.extension == owner)
                    .count()
                    >= MAX_PROVIDER_CALLS
                    || self
                        .provider_calls
                        .values()
                        .filter(|call| {
                            &call.extension == owner
                                && matches!(
                                    call.job.kind,
                                    WireKind::Diagnostics | WireKind::Highlights
                                )
                        })
                        .count()
                        >= MAX_BACKGROUND_CALLS
                {
                    continue;
                }
                let key = (owner.clone(), provider.clone(), doc_id);
                let document = &app.workspace.documents[file];
                let stamp = document.stamp(*file);
                if self.provider_seen.get(&key) == Some(&stamp) {
                    continue;
                }
                let target = Target {
                    file: *file,
                    stamp: document.stamp(*file),
                    path: PathBuf::from(path),
                    text: document.buffer.text_snapshot(),
                    doc_id,
                    version: *version,
                    range: None,
                    word_start: 0,
                    cursor: None,
                    trigger: None,
                    item: None,
                    item_raw: None,
                };
                self.issue(
                    owner,
                    provider,
                    if *kind == DeclKind::Highlights {
                        WireKind::Highlights
                    } else {
                        WireKind::Diagnostics
                    },
                    target,
                    effects,
                );
                if self.provider_calls.contains_key(&self.next_request) {
                    self.provider_seen.insert(key, stamp);
                }
            }
        }
    }

    pub fn set_trust(
        &mut self,
        trust: std::sync::Arc<crate::infrastructure::extensions::trust_store::TrustStore>,
    ) {
        self.trust = Some(trust);
    }

    fn root_key(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    pub(super) fn contributing_packages(&self) -> impl Iterator<Item = &Package> + Clone {
        self.packages()
            .filter(|p| p.lifecycle.state() != State::Stopped)
    }

    /// The themes of loaded packages that were not stopped, in id order. Themes
    /// are data, so they need no grant and no trusted folder.
    pub(crate) fn themes(&self) -> Vec<PackageTheme> {
        let mut themes: Vec<PackageTheme> = self
            .contributing_packages()
            .flat_map(|package| package.themes.iter().cloned())
            .collect();
        themes.sort_by(|a, b| a.0.cmp(&b.0));
        themes
    }

    /// Whether an execution-capable extension supplies providers for `name`.
    pub fn serves_language(&self, name: &str) -> bool {
        self.packages.values().any(|package| {
            !matches!(package.lifecycle.state(), State::Stopped | State::Failed)
                && package.manifest.has_behaviour()
                && package
                    .manifest
                    .capabilities
                    .iter()
                    .any(|cap| cap == "process.execute-local")
                && package
                    .manifest
                    .contributes
                    .language_service
                    .providers
                    .iter()
                    .any(|provider| {
                        provider
                            .selector
                            .languages
                            .iter()
                            .any(|language| language == name)
                    })
        })
    }

    /// Stop what extensions already run in `root`, for when its trust is taken away.
    pub fn stop_tools_in(&mut self, root: &Path) -> Vec<Effect> {
        // Tasks are found by the folder handle they were started for.
        let handles: Vec<u64> = self
            .roots
            .iter()
            .filter(|(_, path)| path.starts_with(root))
            .map(|(id, _)| *id)
            .collect();
        let mut effects: Vec<_> = self
            .tasks
            .iter()
            .filter(|(_, record)| handles.contains(&record.root_id) || record.cwd.starts_with(root))
            .map(|(task, _)| Effect::CancelTask { task: *task })
            .collect();
        effects.extend(
            self.processes
                .iter()
                .filter(|(_, record)| {
                    handles.contains(&record.root_id) || record.cwd.starts_with(root)
                })
                .map(|(process, _)| Effect::KillProcess { process: *process }),
        );
        effects
    }

    fn cancel_tasks_of(&mut self, extension: &str) -> Vec<Effect> {
        let mut effects: Vec<_> = self
            .tasks
            .iter()
            .filter(|(_, record)| record.extension == extension)
            .map(|(task, _)| Effect::CancelTask { task: *task })
            .collect();
        effects.extend(
            self.processes
                .iter()
                .filter(|(_, record)| record.extension == extension)
                .map(|(process, _)| Effect::KillProcess { process: *process }),
        );
        effects.extend(
            self.watches
                .iter()
                .filter(|(_, record)| record.extension == extension)
                .map(|(watch, _)| Effect::StopWatch { watch: *watch }),
        );
        self.watches
            .retain(|_, record| record.extension != extension);
        effects
    }

    fn root_refs(&self) -> Vec<loog_extension_host::RootRef> {
        self.roots
            .iter()
            .map(|(id, path)| loog_extension_host::RootRef {
                id: *id,
                path: path.to_string_lossy().into_owned(),
                name: path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string()),
            })
            .collect()
    }

    /// Folders with their paths and stable storage keys, and the key of the
    /// whole set (workspace-scoped storage follows the set of folders).
    fn root_details(&self) -> (Vec<(loog_extension_host::RootRef, PathBuf, String)>, String) {
        let refs = self.root_refs();
        let details: Vec<_> = refs
            .into_iter()
            .zip(&self.roots)
            .map(|(reference, (_, path))| {
                (reference, path.clone(), stable_key(&Self::root_key(path)))
            })
            .collect();
        let mut paths: Vec<String> = self.roots.iter().map(|(_, p)| Self::root_key(p)).collect();
        paths.sort();
        (details, stable_key(&paths.join("\n")))
    }

    fn roots_job(&self, id: &str, generation: u64) -> Effect {
        let (roots, workspace_key) = self.root_details();
        Effect::Submit(Job::Roots {
            id: id.to_owned(),
            generation,
            roots,
            workspace_key,
        })
    }

    /// Follow the workspace folders. A folder that is gone ends its tasks; a
    /// folder that is added again gets a new handle (design §5.1).
    fn sync_roots(&mut self, app: &AppState) -> Vec<Effect> {
        let current: Vec<PathBuf> = app.explorer.model.roots().to_vec();
        if self.roots.iter().map(|(_, path)| path).eq(current.iter()) {
            return Vec::new();
        }
        let mut effects = Vec::new();
        let gone: Vec<u64> = self
            .roots
            .iter()
            .filter(|(_, path)| !current.contains(path))
            .map(|(id, _)| *id)
            .collect();
        let gone_paths: Vec<_> = self
            .roots
            .iter()
            .filter(|(id, _)| gone.contains(id))
            .map(|(_, path)| path.clone())
            .collect();
        for (task, record) in &self.tasks {
            if gone.contains(&record.root_id)
                || gone_paths.iter().any(|path| record.cwd.starts_with(path))
            {
                effects.push(Effect::CancelTask { task: *task });
            }
        }
        let removed_watches: Vec<_> = self
            .watches
            .iter()
            .filter(|(_, record)| gone.contains(&record.root))
            .map(|(watch, _)| *watch)
            .collect();
        for watch in removed_watches {
            if let Some(record) = self.watches.remove(&watch) {
                effects.push(Effect::Submit(Job::Watch {
                    id: record.extension,
                    generation: record.generation,
                    credit: None,
                    message: Inbound::WatchFailed(loog_extension_host::WatchFailure {
                        watch: record.local,
                        error: crate::infrastructure::extensions::wire_error(&ExtensionError::new(
                            ErrorKind::ExpiredHandle,
                            "the watched folder is no longer open",
                        )),
                    }),
                }));
            }
            effects.push(Effect::StopWatch { watch });
        }
        effects.extend(
            self.processes
                .iter()
                .filter(|(_, record)| {
                    gone.contains(&record.root_id)
                        || gone_paths.iter().any(|path| record.cwd.starts_with(path))
                })
                .map(|(process, _)| Effect::KillProcess { process: *process }),
        );
        // Something an extension left half-done in a folder that was opened since is reported once.
        for path in current
            .iter()
            .filter(|p| !self.roots.iter().any(|(_, known)| known == *p))
        {
            for note in crate::infrastructure::extensions::fileops::unfinished_notes(path) {
                effects.push(Effect::Error(format!(
                    "An extension's file operation did not finish; see {}",
                    note.display()
                )));
            }
        }
        let mut next = Vec::new();
        for path in current {
            let id = match self.roots.iter().find(|(_, known)| *known == path) {
                Some((id, _)) => *id,
                None => {
                    self.next_root += 1;
                    self.next_root
                }
            };
            next.push((id, path));
        }
        self.roots = next;
        for (id, package) in &self.packages {
            if package.lifecycle.state() == State::Active {
                effects.push(self.roots_job(id, package.lifecycle.generation()));
            }
        }
        effects
    }

    /// Check a task request against everything that has to allow it and turn
    /// it into a run. Nothing here starts a process.
    fn plan_task(
        &self,
        extension: &str,
        request: &loog_extension_host::TaskRequest,
    ) -> Result<(u64, tasks::Spec), ExtensionError> {
        let deny = |kind, message: &str| Err(ExtensionError::new(kind, message));
        let package = self
            .packages
            .get(extension)
            .ok_or_else(|| ExtensionError::new(ErrorKind::ExpiredHandle, "unknown extension"))?;
        if !package
            .manifest
            .capabilities
            .iter()
            .any(|c| c == "process.execute-local")
        {
            return deny(
                ErrorKind::PermissionDenied,
                "process.execute-local is not declared by this extension",
            );
        }
        let Some((root_id, root)) = self.roots.iter().find(|(id, _)| *id == request.root) else {
            return deny(ErrorKind::ExpiredHandle, "the folder is no longer open");
        };
        let trust = match &self.trust {
            Some(trust) => trust.is_trusted(root),
            None => false,
        };
        self.grants
            .authorize(
                &package.identity,
                "process.execute-local",
                Some((
                    &Self::root_key(root),
                    if trust {
                        RootTrust::Trusted
                    } else {
                        RootTrust::Untrusted
                    },
                )),
            )
            .map_err(|denial| {
                ExtensionError::new(ErrorKind::PermissionDenied, execution_denial(denial))
            })?;

        let program = request.program.trim();
        let bad_text = |text: &str| text.contains('\0');
        if program.is_empty() || program.len() > 1024 || bad_text(program) {
            return deny(ErrorKind::InvalidEdit, "the program name is not valid");
        }
        if request.args.len() > 128 || request.args.iter().any(|a| a.len() > 8192 || bad_text(a)) {
            return deny(ErrorKind::LimitExceeded, "too many or too long arguments");
        }
        for (name, value) in &request.env {
            if !tasks::SETTABLE.contains(&name.as_str()) || value.len() > 1024 || bad_text(value) {
                return deny(
                    ErrorKind::PermissionDenied,
                    "that environment variable is not allowed",
                );
            }
        }
        let base = root.canonicalize().map_err(|_| {
            ExtensionError::new(ErrorKind::ExpiredHandle, "the folder cannot be read")
        })?;
        let cwd = match request.cwd.as_deref().filter(|c| !c.is_empty()) {
            None => base.clone(),
            Some(relative) => {
                if crate::features::extensions::manifest::check_package_path(relative).is_err() {
                    return deny(
                        ErrorKind::PermissionDenied,
                        "the working directory must stay inside the folder",
                    );
                }
                let joined = base.join(relative).canonicalize().map_err(|_| {
                    ExtensionError::new(
                        ErrorKind::InvalidEdit,
                        "the working directory does not exist",
                    )
                })?;
                if !joined.starts_with(&base) {
                    return deny(
                        ErrorKind::PermissionDenied,
                        "the working directory must stay inside the folder",
                    );
                }
                joined
            }
        };
        // Preserve the workspace's spelling after resolving links. On Windows
        // canonicalize adds a verbatim prefix, which is not necessarily how the
        // same root is stored by the workspace/trust service.
        let cwd = root.join(cwd.strip_prefix(&base).expect("cwd was confined to root"));
        if !self
            .trust
            .as_ref()
            .is_some_and(|trust| trust.is_trusted(&cwd))
        {
            return deny(
                ErrorKind::PermissionDenied,
                "the working directory is in an untrusted folder",
            );
        }
        // A relative path with a separator means "from the working directory";
        // a bare name is looked up on PATH by the operating system.
        let program = if program.contains(['/', '\\']) && Path::new(program).is_relative() {
            cwd.join(program).to_string_lossy().into_owned()
        } else {
            program.to_owned()
        };
        let timeout = request.timeout_ms.unwrap_or(120_000).clamp(100, 600_000);
        Ok((
            *root_id,
            tasks::Spec {
                program,
                args: request.args.clone(),
                cwd,
                env: request.env.clone(),
                timeout: std::time::Duration::from_millis(u64::from(timeout)),
            },
        ))
    }

    fn task_requested(
        &mut self,
        id: &str,
        generation: u64,
        started: loog_extension_host::StartedTask,
    ) -> Vec<Effect> {
        if !self
            .packages
            .get(id)
            .is_some_and(|p| p.lifecycle.accepts(generation))
        {
            return Vec::new();
        }
        let local = started.task;
        let refuse = |error: ExtensionError| {
            vec![Effect::Submit(Job::Task {
                id: id.to_owned(),
                generation,
                message: Inbound::TaskExited(loog_extension_host::TaskExit {
                    task: local,
                    code: None,
                    cancelled: false,
                    timed_out: false,
                    output_truncated: false,
                    error: Some(crate::infrastructure::extensions::wire_error(&error)),
                }),
                ended: true,
            })]
        };
        match self.plan_task(id, &started.request) {
            Err(error) => refuse(error),
            Ok((root_id, spec)) => {
                self.next_task += 1;
                let global = self.next_task;
                self.tasks.insert(
                    global,
                    TaskRecord {
                        extension: id.to_owned(),
                        generation,
                        local,
                        root_id,
                        cwd: spec.cwd.clone(),
                    },
                );
                vec![Effect::StartTask { task: global, spec }]
            }
        }
    }

    /// The guest asked to cancel tasks by their own numbers.
    fn tasks_cancelled(&self, id: &str, generation: u64, locals: &[u64]) -> Vec<Effect> {
        self.tasks
            .iter()
            .filter(|(_, r)| {
                r.extension == id && r.generation == generation && locals.contains(&r.local)
            })
            .map(|(task, _)| Effect::CancelTask { task: *task })
            .collect()
    }

    /// Output and exits of running tasks, on their way to the owning guest.
    pub fn handle_task_event(&mut self, event: tasks::TaskEvent) -> Vec<Effect> {
        use loog_extension_host::{OutputChannel, TaskExit, TaskOutput};
        let (task, ended) = match &event {
            tasks::TaskEvent::Output { task, .. } => (*task, false),
            tasks::TaskEvent::Exited { task, .. } => (*task, true),
        };
        let Some(record) = self.tasks.get(&task) else {
            return Vec::new();
        };
        let (extension, generation, local) =
            (record.extension.clone(), record.generation, record.local);
        if ended {
            self.tasks.remove(&task);
        }
        let message = match event {
            tasks::TaskEvent::Output {
                channel,
                text,
                truncated,
                ..
            } => Inbound::TaskOutput(TaskOutput {
                task: local,
                channel: match channel {
                    tasks::Channel::Stdout => OutputChannel::Stdout,
                    tasks::Channel::Stderr => OutputChannel::Stderr,
                },
                text,
                truncated,
            }),
            tasks::TaskEvent::Exited {
                code,
                cancelled,
                timed_out,
                output_truncated,
                error,
                ..
            } => Inbound::TaskExited(TaskExit {
                task: local,
                code,
                cancelled,
                timed_out,
                output_truncated,
                error: error.map(|message| {
                    crate::infrastructure::extensions::wire_error(&ExtensionError::new(
                        ErrorKind::ExtensionFailed,
                        message,
                    ))
                }),
            }),
        };
        vec![Effect::Submit(Job::Task {
            id: extension,
            generation,
            message,
            ended,
        })]
    }

    /// Findings an extension pushed for one state of an open file.
    fn diagnostics_published(
        &mut self,
        app: &AppState,
        id: &str,
        generation: u64,
        published: loog_extension_host::PublishedDiagnostics,
    ) -> Vec<Effect> {
        let allowed = self.packages.get(id).is_some_and(|package| {
            package.lifecycle.accepts(generation)
                && self.authorize(package, "documents.read").is_ok()
                && package
                    .provider_configurations
                    .get(&published.provider)
                    .is_none_or(|c| c.enabled)
        });
        // A source name must be the extension's own, so it cannot speak as another.
        if !allowed || !published.provider.starts_with(&format!("{id}.")) {
            return Vec::new();
        }
        let Some(file) = app.workspace.file_id_for_path(Path::new(&published.path)) else {
            return Vec::new();
        };
        let document = &app.workspace.documents[&file];
        if document.buffer.version() != published.version {
            // Describes a state that is gone: never shown.
            return Vec::new();
        }
        let text = document.buffer.text_snapshot();
        let reported: Vec<Reported> = published
            .items
            .into_iter()
            .map(|d| Reported {
                start: d.range.start as usize,
                end: d.range.end as usize,
                severity: d.severity,
                message: d.message,
                source: d.source,
                code: d.code,
                documentation_url: d.documentation_url,
            })
            .collect();
        let converted = providers::convert(&text, &reported, &published.provider);
        let stamp = document.stamp(file);
        let source = self.source_for(id, &published.provider);
        vec![Effect::PublishDiagnostics {
            source,
            path: document.meta.path.clone(),
            stamp,
            text,
            items: converted.diagnostics,
        }]
    }

    /// End of a command call: its snapshot leases are released.
    pub fn finish_call(&mut self, call: u64) {
        self.calls.remove(&call);
    }

    pub fn active_calls(&self) -> usize {
        self.calls.len()
    }
}

#[cfg(test)]
mod package_tests;
#[cfg(test)]
pub(crate) mod tests;
