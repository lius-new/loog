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
use crate::features::extensions::trust::{Denial, Grants, PackageIdentity};
use crate::features::language_service::sync::Stamp;
use crate::infrastructure::extensions::ProviderJob;
use crate::infrastructure::extensions::{Event, Job, Outcome, Run};
use loom_extension_api::admission::EditLimits;
use loom_extension_api::budget::{CallBudget, InteractionLimits};
use loom_extension_api::events::{Delivery, DocumentStream};
use loom_extension_api::ledger::{Begin, OperationLedger};
use loom_extension_api::{ErrorKind, ExtensionError};
use loom_extension_host::{
    AcceptedEdit, DocumentChange as WireChange, DocumentRef, Inbound, Limits,
};
use loom_extension_host::{Provided, ProviderKind as WireKind};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::path::{Path, PathBuf};

pub(crate) mod host;

const PROVIDER_DEADLINE_MS: u64 = 500;
const FORMAT_DEADLINE_MS: u64 = 5_000;
/// Extension candidates list after every language server's.
const COMPLETION_RANK: usize = 1000;
const EXTERNAL_SOURCE: u64 = crate::infrastructure::lsp::manager::EXTERNAL_SOURCE;

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
    item: Option<loom_extension_host::WireCompletionItem>,
    item_raw: Option<serde_json::Value>,
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
    if let Ok(entries) = std::fs::read_dir(&root) {
        let mut dirs: Vec<_> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.join("manifest.json").is_file())
            .collect();
        dirs.sort();
        for dir in dirs {
            if let Err(error) = host.load_package(&dir) {
                problems.push(format!("{}: {error}", dir.display()));
            }
        }
    }
    state.update(|app| host.refresh_settings(app));
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
#[derive(Debug)]
pub enum Effect {
    Submit(Job),
    PublishDiagnostics {
        source: u64,
        path: PathBuf,
        stamp: Stamp,
        text: std::sync::Arc<String>,
        items: Vec<crate::features::language_service::diagnostics::Diagnostic>,
    },
    ClearDiagnostics {
        source: u64,
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
    /// A completion item was resolved by the extension that offered it.
    ResolvedCompletion {
        source: u64,
        raw: serde_json::Value,
        item: serde_json::Value,
    },
    Toast(String),
    Error(String),
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
    version: u64,
}

/// Whether a manifest asks for the named event (design §6).
fn wants(manifest: &Manifest, event: &str) -> bool {
    manifest.activation.iter().any(|a| {
        matches!(a, crate::features::extensions::manifest::Activation::Event { name } if name == event)
    })
}

fn subscribed(manifest: &Manifest) -> bool {
    manifest.has_behaviour()
        && ["document.open", "document.change", "document.close"]
            .iter()
            .any(|event| wants(manifest, event))
}

const FEED_MAX_EDITS: usize = 4096;
const FEED_MAX_MESSAGES: usize = 64;

/// Data resources of a package after each one was checked on its own. A bad
/// resource is dropped without touching the others or the behaviour (design §6.6).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Accepted {
    pub themes: Vec<String>,
    pub languages: Vec<String>,
    pub snippets: Vec<String>,
    /// `(resource id, why it was rejected)`
    pub rejected: Vec<(String, String)>,
}

/// Check a data resource: inside the package after resolving links, readable,
/// and valid JSON. Contents are checked again by the subsystem that loads them.
fn check_resource(dir: &Path, path: &str) -> Result<(), String> {
    let base = dir.canonicalize().map_err(|e| e.to_string())?;
    let file = dir
        .join(path)
        .canonicalize()
        .map_err(|_| "the file does not exist".to_owned())?;
    if !file.starts_with(&base) {
        return Err("the file is outside the package".to_owned());
    }
    let text = std::fs::read_to_string(&file).map_err(|e| e.to_string())?;
    serde_json::from_str::<serde_json::Value>(&text).map_err(|e| format!("invalid JSON: {e}"))?;
    Ok(())
}

fn accept_resources(dir: &Path, manifest: &Manifest) -> Accepted {
    let mut accepted = Accepted::default();
    let c = &manifest.contributes;
    for (list, kind) in [(&c.themes, 0), (&c.languages, 1), (&c.snippets, 2)] {
        for resource in list {
            match check_resource(dir, &resource.path) {
                Ok(()) => match kind {
                    0 => accepted.themes.push(resource.id.clone()),
                    1 => accepted.languages.push(resource.id.clone()),
                    _ => accepted.snippets.push(resource.id.clone()),
                },
                Err(reason) => accepted.rejected.push((resource.id.clone(), reason)),
            }
        }
    }
    accepted
}

pub struct Package {
    pub manifest: Manifest,
    pub resources: Accepted,
    feeds: HashMap<u64, Feed>,
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
    next_document: u64,
    settings: SettingSources,
    provider_calls: HashMap<u64, ProviderCall>,
    /// Last document version each (extension, provider, document) was asked about.
    provider_seen: HashMap<(String, String, u64), u64>,
    /// Requests waiting for a cold activation to finish.
    queued: Vec<u64>,
    next_request: u64,
    sources: HashMap<(String, String), u64>,
    next_source: u64,
}

/// The setting files as last read.
#[derive(Default)]
struct SettingSources {
    user: serde_json::Value,
    project: Option<serde_json::Value>,
    trusted: bool,
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
        let start = std::time::Instant::now();
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
            next_document: 0,
            settings: SettingSources::default(),
            provider_calls: HashMap::new(),
            provider_seen: HashMap::new(),
            queued: Vec::new(),
            next_request: 0,
            sources: HashMap::new(),
            next_source: 0,
        }
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

    pub fn set_guest_limits(&mut self, limits: Limits) {
        self.guest_limits = limits;
    }

    pub fn package(&self, id: &str) -> Option<&Package> {
        self.packages.get(id)
    }

    pub fn packages(&self) -> impl Iterator<Item = &Package> {
        self.packages.values()
    }

    /// Read and validate a package from a user-selected directory. Nothing runs.
    pub fn load_package(&mut self, dir: &Path) -> Result<String, String> {
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
        let id = manifest.id.clone();
        let mut lifecycle = self
            .packages
            .get(&id)
            .map(|old| Lifecycle::after(old.lifecycle.generation()))
            .unwrap_or_default();
        lifecycle.validated().map_err(|e| e.to_string())?;
        lifecycle.dormant().map_err(|e| e.to_string())?;
        // A reload replaces the old package: its generation moves on first.
        if let Some(old) = self.packages.remove(&id) {
            self.calls
                .retain(|_, call| call.extension != old.manifest.id);
        }
        let resources = accept_resources(dir, &manifest);
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
                resources,
                dir: dir.to_path_buf(),
                identity,
                lifecycle,
                feeds: HashMap::new(),
                resolved,
                ledger: OperationLedger::new(RECEIPTS),
                pending: PendingTriggers::new(PENDING_COMMANDS),
                logs: Vec::new(),
                last_error: None,
                component,
            },
        );
        Ok(id)
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
        let old_digest = self.packages[id].identity.digest.clone();
        let mut effects = self.stop(id);
        let new_id = self.load_package(&dir)?;
        if self.packages[&new_id].identity.digest == old_digest {
            self.grant_declared(&new_id);
        } else {
            effects.push(Effect::Toast(format!(
                "{new_id} changed on disk; its permissions must be granted again"
            )));
        }
        Ok(effects)
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
                        "\n  resources: {} theme(s), {} language(s), {} snippet file(s)",
                        r.themes.len(),
                        r.languages.len(),
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
                    cache: self.cache.clone(),
                    limits: self.guest_limits,
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

    /// Stop an extension: the generation advances, so late work is refused.
    pub fn stop(&mut self, id: &str) -> Vec<Effect> {
        let Some(package) = self.packages.get_mut(id) else {
            return vec![];
        };
        if package.lifecycle.stopping().is_ok() {
            let _ = package.lifecycle.stopped();
        }
        self.calls.retain(|_, call| call.extension != id);
        let mut effects = self.clear_sources(id);
        effects.push(Effect::Submit(Job::Stop { id: id.to_owned() }));
        effects
    }

    pub fn handle_event(&mut self, app: &mut AppState, event: Event) -> Vec<Effect> {
        match event {
            Event::Loaded {
                id,
                generation,
                result,
                ..
            } => {
                let mut effects = self.loaded(&id, generation, result);
                // Documents that opened or changed while it started are delivered now.
                effects.extend(self.observe(app));
                effects
            }
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
            Event::Log { id, level, text } => {
                if let Some(package) = self.packages.get_mut(&id) {
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
                let (commands, _, _) = package.pending.drain();
                // The guest can read its settings before it handles anything.
                let initial = Effect::Submit(Job::Settings {
                    id: id.to_owned(),
                    generation,
                    values: package.resolved.values.clone(),
                    changed: Vec::new(),
                });
                let runs = commands.into_iter().filter_map(|pending| {
                    self.waiting_runs.remove(&pending.call).map(|run| {
                        Effect::Submit(Job::Run {
                            id: id.to_owned(),
                            generation,
                            run,
                        })
                    })
                });
                let mut effects: Vec<Effect> = std::iter::once(initial).chain(runs).collect();
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
                    let job = Box::new(self.provider_calls[&request].job.clone());
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
                let waiting = package.pending.fail_all();
                for pending in &waiting {
                    self.calls.remove(&pending.call);
                    self.waiting_runs.remove(&pending.call);
                }
                vec![Effect::Error(format!("{id}: could not start: {error}"))]
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
        self.calls.retain(|_, call| call.extension != id);
        let mut effects = self.clear_sources(id);
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
                use loom_extension_api::ledger::OperationState as S;
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
        let has_providers = self
            .packages
            .values()
            .any(|p| p.manifest.has_behaviour() && !p.manifest.contributes.providers.is_empty());
        if !has_providers && !self.packages.values().any(|p| subscribed(&p.manifest)) {
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
            for package in self
                .packages
                .values_mut()
                .filter(|p| subscribed(&p.manifest))
            {
                let feed = package.feeds.entry(id).or_insert_with(|| {
                    let mut stream = DocumentStream::new(FEED_MAX_EDITS, FEED_MAX_MESSAGES);
                    stream.open(*version);
                    Feed {
                        stream,
                        path: path.clone(),
                        language: language.clone(),
                        version: *version,
                    }
                });
                if feed.version != *version {
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
            .filter(|(_, p)| subscribed(&p.manifest))
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
                                version,
                            };
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
                        effects.push(Effect::Submit(Job::Deliver {
                            id: name.clone(),
                            generation,
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
                            cache: self.cache.clone(),
                            limits: self.guest_limits,
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
        let key = (extension.to_owned(), provider.to_owned());
        if let Some(source) = self.sources.get(&key) {
            return *source;
        }
        self.next_source += 1;
        let source = EXTERNAL_SOURCE | self.next_source;
        self.sources.insert(key, source);
        source
    }

    /// Withdraw what an extension published; its provider calls are void.
    fn clear_sources(&mut self, extension: &str) -> Vec<Effect> {
        self.provider_calls
            .retain(|_, call| call.extension != extension);
        self.provider_seen
            .retain(|(owner, _, _), _| owner != extension);
        self.queued
            .retain(|request| self.provider_calls.contains_key(request));
        self.sources
            .iter()
            .filter(|((owner, _), _)| owner == extension)
            .map(|(_, source)| Effect::ClearDiagnostics { source: *source })
            .collect()
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
        self.next_request += 1;
        self.next_token += 1;
        let (request, token) = (self.next_request, self.next_token);
        self.provider_calls.retain(|_, call| {
            !(call.extension == owner
                && call.provider == provider
                && call.file == target.file
                && std::mem::discriminant(&call.job.kind) == std::mem::discriminant(&kind))
        });
        self.queued
            .retain(|queued| self.provider_calls.contains_key(queued));
        let now = (self.clock)();
        let limit = if matches!(kind, WireKind::Formatting) {
            FORMAT_DEADLINE_MS
        } else {
            PROVIDER_DEADLINE_MS
        };
        let Some(package) = self.packages.get_mut(owner) else {
            return;
        };
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
                    cache: self.cache.clone(),
                    limits: self.guest_limits,
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
            },
        );
    }

    /// Ask every completion provider registered for the active document's
    /// language. Returns the requests that were actually issued; the answers
    /// arrive as [`Effect::OfferCompletion`].
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
            for provider in &package.manifest.contributes.providers {
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

    /// Ask the extension that offered a completion item to complete it. The
    /// `raw` JSON carries the origin; an item whose origin does not match
    /// the source it is filed under is not resolved.
    pub fn request_completion_resolve(
        &mut self,
        app: &AppState,
        source: u64,
        raw: &serde_json::Value,
    ) -> Vec<Effect> {
        let mut effects = Vec::new();
        let origin = &raw["x-loom"];
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
        let item = loom_extension_host::WireCompletionItem {
            label: text_of("label").unwrap_or_default(),
            detail: text_of("detail"),
            documentation: text_of("documentation"),
            kind: raw["kind"].as_u64().map(|k| k as u32),
            insert_text: text_of("insertText"),
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
        let request = match &answer {
            Provided::Diagnostics { request, .. }
            | Provided::Edits { request, .. }
            | Provided::Completions { request, .. } => *request,
        };
        let Some(call) = self.provider_calls.get(&request) else {
            return Vec::new();
        };
        if call.extension != id || call.generation != generation {
            return Vec::new();
        }
        let call = self.provider_calls.remove(&request).expect("checked above");
        if !self
            .packages
            .get(id)
            .is_some_and(|p| p.lifecycle.accepts(generation))
        {
            return Vec::new();
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
                            detail: i.detail,
                            documentation: i.documentation,
                            kind: i.kind,
                            insert_text: i.insert_text,
                            sort_text: i.sort_text,
                            filter_text: i.filter_text,
                            data: i.data,
                        })
                        .collect();
                    let (json, dropped) =
                        providers::completion_json(&call.extension, &call.provider, &offered);
                    if dropped > 0 {
                        if let Some(package) = self.packages.get_mut(id) {
                            package
                                .logs
                                .push((1, format!("{dropped} completion item(s) were dropped")));
                        }
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
                    version,
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
                    WireKind::Diagnostics,
                    target,
                    &mut effects,
                );
            }
            return effects;
        }
        match answer {
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
                    })
                    .collect();
                let converted = providers::convert(&call.text, &reported, &call.provider);
                let source = self.source_for(&call.extension, &call.provider);
                let mut effects = vec![Effect::PublishDiagnostics {
                    source,
                    path: call.path.clone(),
                    stamp: Stamp {
                        incarnation: call.stamp.incarnation,
                        version: call.stamp.version,
                    },
                    text: call.text.clone(),
                    items: converted.diagnostics,
                }];
                if converted.dropped > 0 {
                    if let Some(package) = self.packages.get_mut(id) {
                        package.logs.push((
                            1,
                            format!(
                                "{} diagnostic(s) could not be placed and were dropped",
                                converted.dropped
                            ),
                        ));
                    }
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
        let owners: Vec<(String, String, Vec<String>)> = self
            .packages
            .iter()
            .filter(|(_, p)| {
                ["documents.read", "languages"]
                    .iter()
                    .all(|c| self.authorize(p, c).is_ok())
            })
            .flat_map(|(owner, p)| {
                p.manifest
                    .contributes
                    .providers
                    .iter()
                    .filter(|d| d.kind == DeclKind::Diagnostics)
                    .map(move |d| (owner.clone(), d.id.clone(), d.selector.languages.clone()))
            })
            .collect();
        for (file, path, language, version) in current {
            let Some(doc_id) = self.seen.get(file).copied() else {
                continue;
            };
            for (owner, provider, languages) in &owners {
                if !languages.contains(language) {
                    continue;
                }
                let key = (owner.clone(), provider.clone(), doc_id);
                if self.provider_seen.get(&key) == Some(version) {
                    continue;
                }
                self.provider_seen.insert(key, *version);
                let document = &app.workspace.documents[file];
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
                self.issue(owner, provider, WireKind::Diagnostics, target, effects);
            }
        }
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
pub(crate) mod tests;
