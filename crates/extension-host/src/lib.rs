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
        world: "extension",
    });
}

pub use bindings::loom::extension::types::{
    ApplyEditRequest, ByteRange, CommandContext, CommittedReceipt, Completion, DocumentChange,
    DocumentEdits, DocumentRef, ErrorKind as WitErrorKind, HostError, Inbound, RoundStatus,
    Selection, TextEdit,
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

/// State behind the host imports.
pub struct HostState {
    limits: StoreLimits,
    pending_limit: usize,
    next_operation: u64,
    snapshots: BTreeMap<u64, Arc<String>>,
    outstanding: usize,
    settings: BTreeMap<String, String>,
    pub accepted: Vec<AcceptedEdit>,
    pub cancelled: Vec<u64>,
    pub logs: Vec<(u8, String)>,
}

impl HostState {
    fn new(limits: &Limits) -> Self {
        Self {
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

impl host::Host for HostState {
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
        self.snapshots.remove(&snapshot);
    }

    fn apply_edit(&mut self, request: ApplyEditRequest) -> Result<u64, HostError> {
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

    fn get_setting(&mut self, key: String) -> Option<String> {
        self.settings.get(&key).cloned()
    }

    fn cancel(&mut self, operation: u64) {
        self.cancelled.push(operation);
    }

    fn log(&mut self, level: u8, message: String) {
        if self.logs.len() < 256 {
            self.logs.push((level, message));
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
pub const KNOWN_IMPORTS: &[&str] = &["loom:extension/types@0.1.0", "loom:extension/host@0.1.0"];

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
        bindings::Extension::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state)
            .map_err(|error| failed(&error))?;
        linker
            .define_unknown_imports_as_traps(component)
            .map_err(|error| failed(&error))?;
        let mut store = Store::new(&self.engine, HostState::new(&limits));
        store.limiter(|state| &mut state.limits);
        store.set_epoch_deadline(limits.round_ticks);
        let bindings = bindings::Extension::instantiate(&mut store, component, &linker)
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
    bindings: bindings::Extension,
    limits: Limits,
}

impl Instance {
    /// Deliver `batch` and advance the guest by one bounded round.
    ///
    /// A trap fails the instance; the caller must discard it (no automatic
    /// resume), keeping committed operations as the real receipts.
    pub fn dispatch(&mut self, batch: &[Inbound]) -> RoundResult {
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

    /// Replace the resolved setting values the guest can read.
    pub fn put_settings(&mut self, values: BTreeMap<String, String>) {
        self.store.data_mut().settings = values;
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
