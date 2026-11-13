//! Versioned typed envelopes shared by guests and the application broker.
use crate::*;
use serde::{Deserialize, Serialize};

/// The identity of one buffer revision, independent of any filesystem path.
/// A lifecycle changes on reopen, retarget, language change or synchronization
/// policy change. Consumers must compare all three fields.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentVersion {
    pub document: u64,
    pub lifecycle: u64,
    pub version: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentResource {
    pub stamp: DocumentVersion,
    pub resource: ResourceRef,
    pub language: String,
    pub variant: String,
    pub byte_length: u64,
    pub synchronized: bool,
}

/// Editor findings use UTF-8 byte offsets in a particular DocumentVersion.
/// Protocol positions must be converted by the language plugin.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceDiagnostic {
    pub start: u64,
    pub end: u64,
    /// 1 error, 2 warning, 3 information, 4 hint.
    pub severity: u8,
    pub message: String,
    pub source: Option<String>,
    pub code: Option<String>,
    pub documentation_url: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnEncoding {
    Utf8,
    Utf16,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourcePosition {
    pub line: u32,
    pub character: u32,
}

/// Positions in a possibly unopened resource, interpreted in its own text.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceRange {
    pub start: ResourcePosition,
    pub end: ResourcePosition,
    pub encoding: ColumnEncoding,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceLocation {
    pub resource: ResourceRef,
    pub range: ResourceRange,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceTextEdit {
    pub range: ResourceRange,
    pub new_text: String,
}

/// A text-only proposal. The host applies it to buffers; saving is separate.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceDocumentEdit {
    pub resource: ResourceRef,
    /// Required when the server supplies a version for an open document.
    pub version: Option<DocumentVersion>,
    pub edits: Vec<ResourceTextEdit>,
}

/// Opaque provider command arguments, never interpreted as host paths.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceCommand {
    pub name: String,
    pub arguments: Vec<serde_json::Value>,
}

/// A resource code action. Missing edits and command request lazy resolution
/// using `data` in the same provider and source document revision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceCodeAction {
    pub title: String,
    pub kind: Option<String>,
    pub preferred: bool,
    pub disabled: Option<String>,
    pub data: Option<String>,
    pub documents: Option<Vec<ResourceDocumentEdit>>,
    pub command: Option<ResourceCommand>,
}

/// General language query modes independent of any server protocol.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguageMode {
    Definition,
    Declaration,
    TypeDefinition,
    Implementation,
    Full,
    Range,
}

/// Availability of a declared provider in exact resource contexts. A batch
/// replaces this activation's resource configurations; omitted contexts are
/// unavailable after the first successful configuration batch.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceProviderConfiguration {
    pub provider: String,
    pub contexts: Vec<ResourceRef>,
    pub enabled: bool,
    pub modes: Option<Vec<LanguageMode>>,
    pub revision: u64,
    pub resolve: bool,
    pub prepare: bool,
    pub triggers: Vec<String>,
    pub retriggers: Vec<String>,
    pub commands: Vec<String>,
    pub aliases: Vec<String>,
}

/// Generic service state, with contexts independent of host path syntax.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    Stopped,
    Starting,
    Ready,
    Working,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceServiceReport {
    pub key: String,
    pub state: ServiceState,
    pub detail: Option<String>,
    /// None is global; an empty list is dormant; otherwise exact resource match.
    pub contexts: Option<Vec<ResourceRef>>,
}

/// A stable root and a provider-owned, non-secret recovery description.
/// Runtime handles and generations are never persisted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "BookmarkWire")]
pub struct ResourceBookmark {
    pub resource: ResourceRef,
    pub name: String,
    pub state: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BookmarkWire {
    resource: ResourceRef,
    name: String,
    state: String,
}
impl TryFrom<BookmarkWire> for ResourceBookmark {
    type Error = String;
    fn try_from(wire: BookmarkWire) -> Result<Self, Self::Error> {
        let bookmark = Self {
            resource: wire.resource,
            name: wire.name,
            state: wire.state,
        };
        bookmark.validate()?;
        Ok(bookmark)
    }
}
impl ResourceBookmark {
    pub fn validate(&self) -> Result<(), String> {
        if self.resource.environment().is_none()
            || self.name.is_empty()
            || self.name.len() > 256
            || self.name.contains('\0')
            || self.state.len() > 16 * 1024
            || self.state.contains('\0')
        {
            return Err("Invalid resource recovery record".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    /// Resolve this package's declared defaults and user settings, optionally
    /// overlaying project data read by the consumer in an authorized scope.
    /// Native-project settings are never inherited. Project values only apply
    /// in a trusted scope and cannot override user-only declarations.
    ResolveSettings {
        scope: Option<ScopeId>,
        project: Option<serde_json::Value>,
    },
    /// Replace at most 128 configurations, 128 contexts per configuration and
    /// 512 KiB total. Requires languages and documents.read. Native-path
    /// provider configuration remains independent of this resource interface.
    ConfigureLanguageProviders {
        configurations: Vec<ResourceProviderConfiguration>,
    },
    /// Apply a text-only buffer transaction, anchored to a current provided
    /// document. The Reply is deferred until application finishes. A successful
    /// receipt does not mean files were saved. Never replay after a lost reply.
    ApplyWorkspaceEdit {
        origin: DocumentVersion,
        title: String,
        documents: Vec<ResourceDocumentEdit>,
    },
    /// Prevent a pending edit from committing. Done means cancellation was
    /// recorded before commit; the original ticket still receives its outcome.
    /// A finished/unknown ticket is refused, never interpreted as rolled back.
    CancelWorkspaceEdit {
        ticket: u64,
    },
    /// Atomically replace this activation's resource-aware service reports.
    /// At most 16 reports, 128 contexts per report, 200 detail bytes, 256 KiB total.
    /// An empty list withdraws them; unloading also clears them.
    ReportServices {
        reports: Vec<ResourceServiceReport>,
    },
    /// Opt declared language providers into resource-aware request context for
    /// this activation. Replaces the previous list. The path-free base request
    /// and snapshot APIs are reused; native-path answers remain local-only.
    BindLanguageProviders {
        providers: Vec<String>,
    },
    /// Complete an outstanding resource definition/reference request. The
    /// provider resolves server URIs in their target environment before reply.
    /// Source revision, activation, request ownership and target environment
    /// must still match; opening a destination separately enforces file access.
    ProvideLocations {
        request: u64,
        stamp: DocumentVersion,
        locations: Vec<ResourceLocation>,
    },
    /// Complete a rename with text-only edits in the source environment.
    /// At most 16 resources, 1024 edits, 512 KiB replacement text and 1 MiB
    /// serialized data. Requires workspace.edit. Done acknowledges delivery,
    /// not application or disk persistence; the native edit flow reports those.
    ProvideWorkspaceEdit {
        request: u64,
        stamp: DocumentVersion,
        documents: Vec<ResourceDocumentEdit>,
    },
    /// Completes a code-action or code-action-resolve provider request. Delivery
    /// acknowledgment does not mean an offered edit has been applied.
    ProvideCodeActions {
        request: u64,
        stamp: DocumentVersion,
        actions: Vec<ResourceCodeAction>,
    },
    /// Explicit provider failure, or successful completion of an execute-command
    /// request. Commands may themselves change the source buffer version.
    FinishLanguageRequest {
        request: u64,
        stamp: DocumentVersion,
        error: Option<String>,
    },
    /// Replace one declared provider's findings for exactly this buffer revision.
    /// Requires documents.read and an enabled diagnostics provider whose
    /// language selector matches. An empty list clears that document/source.
    PublishDiagnostics {
        provider: String,
        stamp: DocumentVersion,
        diagnostics: Vec<ResourceDiagnostic>,
    },
    /// Discover open text buffers, including unsaved/offline provided resources.
    /// Requires documents.read. A first page subscribes to coalesced changes;
    /// later pages must supply the first page's revision.
    Documents {
        after: Option<u64>,
        revision: Option<u64>,
        limit: u32,
    },
    /// Read at most 64 KiB of UTF-8 from exactly this buffer revision. Offsets
    /// must be UTF-8 boundaries. A change between chunks fails with Conflict;
    /// discard partial text and obtain a fresh document stamp before retrying.
    ReadDocument {
        stamp: DocumentVersion,
        offset: u64,
        limit: u32,
    },
    /// Discover presented roots and the caller's effective capabilities.
    /// Requires workspace.files.read. Root changes invalidate pagination.
    Roots {
        after: Option<ScopeId>,
        limit: u32,
    },
    Register {
        descriptor: EnvironmentDescriptor,
    },
    Mount {
        environment: EnvironmentId,
        root: ProviderPath,
    },
    /// Make an owned scope available in the application's native workspace.
    Present {
        scope: ScopeId,
    },
    /// Preserve this presented root for a future application session.
    Remember {
        scope: ScopeId,
        state: String,
    },
    Forget {
        resource: ResourceRef,
    },
    Unregister {
        environment: EnvironmentId,
    },
    Request {
        scope: ScopeId,
        operation: Operation,
        timeout_ms: u32,
    },
    Complete {
        request: RequestId,
        result: Result<Answer, Failure>,
    },
    Notify {
        scope: ScopeId,
        event: Notification,
    },
    Cancel {
        request: RequestId,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reply {
    Settings {
        /// JSON text per declared key, matching the base settings value format.
        values: std::collections::BTreeMap<String, String>,
    },
    EditsApplied {
        transaction: u64,
        changed: u32,
    },
    Documents {
        documents: Vec<DocumentResource>,
        revision: u64,
        next: Option<u64>,
    },
    DocumentChunk {
        stamp: DocumentVersion,
        text: String,
        next: Option<u64>,
    },
    Roots {
        roots: Vec<PresentedRoot>,
        next: Option<ScopeId>,
    },
    Environment {
        id: EnvironmentId,
    },
    Scope {
        id: ScopeId,
    },
    Request {
        id: RequestId,
    },
    Done,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// Declared user-layer values changed, independent of native-project
    /// overrides. Coalesced until this activation resolves settings again.
    SettingsChanged,
    /// Delivered before the corresponding path-free ProviderRequest in the
    /// same guest dispatch. Only explicitly bound providers receive this.
    ProviderDocument {
        request: u64,
        document: DocumentResource,
    },
    /// Coalesced until the consumer requests another document page.
    DocumentsChanged {
        revision: u64,
    },
    /// Roots or effective grants changed; obtain a fresh page sequence.
    RootsChanged,
    Restore {
        bookmark: ResourceBookmark,
    },
    Reply {
        ticket: u64,
        result: Result<Reply, Failure>,
    },
    Delivery {
        event: Delivery,
    },
}

#[derive(Clone, Debug)]
pub struct Submitted {
    pub ticket: u64,
    pub command: Command,
}
