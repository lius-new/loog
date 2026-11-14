use crate::{EnvironmentKey, ProviderPath};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    FilesRead,
    FilesWrite,
    Watch,
    Search,
    Process,
    Terminal,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentDescriptor {
    pub key: EnvironmentKey,
    pub name: String,
    pub platform: String,
    pub capabilities: Vec<Capability>,
    /// A short note the provider wants shown beside the roots of this environment, such as
    /// the machine they are on. At most [`MAX_BADGE_BYTES`] bytes of plain text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<String>,
}

/// The longest badge an environment can carry.
pub const MAX_BADGE_BYTES: usize = 64;

/// An explicitly presented native root. Paths remain provider-owned tokens.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresentedRoot {
    pub scope: crate::ScopeId,
    pub resource: crate::ResourceRef,
    pub name: String,
    pub capabilities: Vec<Capability>,
    /// The environment's badge, shown beside the root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileInfo {
    pub path: ProviderPath,
    pub name: String,
    pub kind: FileKind,
    pub size: u64,
    pub revision: Option<String>,
    pub readonly: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    /// Resolve a parent using the provider's path rules; None at the scope root.
    ParentPath {
        path: ProviderPath,
    },
    /// A URI meaningful inside this environment, never a client filesystem path.
    ResourceUri {
        path: ProviderPath,
    },
    /// Resolve an environment-local URI within this scope using provider rules.
    ResolveUri {
        uri: String,
    },
    /// Resolve one child name using the provider's naming rules.
    ChildPath {
        parent: ProviderPath,
        name: String,
    },
    /// Interpret a path expression relative to a directory using provider rules.
    /// The result must remain inside the authorized scope, including symlinks.
    ResolvePath {
        base: ProviderPath,
        relative: String,
    },
    /// Provider-owned descendant names, or None when path is outside base.
    /// Names are data, not client filesystem path fragments.
    RelativePath {
        base: ProviderPath,
        path: ProviderPath,
    },
    /// Create an empty file exclusively; an existing entry is a conflict.
    CreateFile {
        path: ProviderPath,
    },
    /// Rename without replacement and report affected tracked resource identities.
    Relocate {
        source: ProviderPath,
        target: ProviderPath,
        tracked: Vec<ProviderPath>,
    },
    Stat {
        path: ProviderPath,
    },
    List {
        path: ProviderPath,
        cursor: Option<String>,
        limit: u32,
    },
    Read {
        path: ProviderPath,
        offset: u64,
        limit: u32,
    },
    Write {
        path: ProviderPath,
        bytes: Vec<u8>,
        expected_revision: Option<String>,
    },
    BeginWrite {
        path: ProviderPath,
        expected_revision: Option<String>,
        size: u64,
    },
    AppendWrite {
        write: u64,
        offset: u64,
        bytes: Vec<u8>,
    },
    CommitWrite {
        write: u64,
    },
    AbortWrite {
        write: u64,
    },
    CreateDirectory {
        path: ProviderPath,
    },
    Rename {
        source: ProviderPath,
        target: ProviderPath,
        overwrite: bool,
    },
    Remove {
        path: ProviderPath,
        recursive: bool,
    },
    Watch {
        path: ProviderPath,
    },
    Unwatch {
        watch: u64,
    },
    Search {
        path: ProviderPath,
        options: crate::SearchOptions,
    },
    /// Consume a bounded batch. Empty/incomplete is valid; callers back off.
    SearchNext {
        search: u64,
        limit: u32,
    },
    SearchClose {
        search: u64,
    },
    Spawn {
        program: String,
        args: Vec<String>,
        cwd: ProviderPath,
        env: Vec<(String, String)>,
    },
    Input {
        process: u64,
        bytes: Vec<u8>,
    },
    CloseInput {
        process: u64,
    },
    Kill {
        process: u64,
    },
    TerminalOpen {
        cwd: ProviderPath,
        program: Option<String>,
        args: Vec<String>,
        cols: u16,
        rows: u16,
    },
    TerminalInput {
        terminal: u64,
        bytes: Vec<u8>,
    },
    TerminalResize {
        terminal: u64,
        cols: u16,
        rows: u16,
    },
    TerminalClose {
        terminal: u64,
    },
}

impl Operation {
    pub fn handle(&self) -> Option<u64> {
        match self {
            Self::AppendWrite { write, .. }
            | Self::CommitWrite { write }
            | Self::AbortWrite { write } => Some(*write),
            Self::Unwatch { watch } => Some(*watch),
            Self::SearchNext { search, .. } | Self::SearchClose { search } => Some(*search),
            Self::Input { process, .. } | Self::CloseInput { process } | Self::Kill { process } => {
                Some(*process)
            }
            Self::TerminalInput { terminal, .. }
            | Self::TerminalResize { terminal, .. }
            | Self::TerminalClose { terminal } => Some(*terminal),
            _ => None,
        }
    }
    pub fn answer_kind(&self) -> AnswerKind {
        match self {
            Self::ChildPath { .. } | Self::ResolveUri { .. } | Self::ResolvePath { .. } => {
                AnswerKind::Path
            }
            Self::RelativePath { .. } => AnswerKind::RelativePath,
            Self::ParentPath { .. } => AnswerKind::Parent,
            Self::ResourceUri { .. } => AnswerKind::Uri,
            Self::CreateFile { .. } => AnswerKind::Info,
            Self::Relocate { .. } => AnswerKind::Relocated,
            Self::Stat { .. } => AnswerKind::Info,
            Self::List { .. } => AnswerKind::Directory,
            Self::Read { .. } => AnswerKind::Bytes,
            Self::Write { .. } | Self::CommitWrite { .. } => AnswerKind::Written,
            Self::BeginWrite { .. }
            | Self::Search { .. }
            | Self::Watch { .. }
            | Self::Spawn { .. }
            | Self::TerminalOpen { .. } => AnswerKind::Started,
            Self::SearchNext { .. } => AnswerKind::Search,
            _ => AnswerKind::Ok,
        }
    }
    pub fn capability(&self) -> Capability {
        match self {
            Self::ChildPath { .. }
            | Self::ResolvePath { .. }
            | Self::RelativePath { .. }
            | Self::ParentPath { .. }
            | Self::ResourceUri { .. }
            | Self::ResolveUri { .. }
            | Self::Stat { .. }
            | Self::List { .. }
            | Self::Read { .. } => Capability::FilesRead,
            Self::CreateFile { .. }
            | Self::Relocate { .. }
            | Self::Write { .. }
            | Self::BeginWrite { .. }
            | Self::AppendWrite { .. }
            | Self::CommitWrite { .. }
            | Self::AbortWrite { .. }
            | Self::CreateDirectory { .. }
            | Self::Rename { .. }
            | Self::Remove { .. } => Capability::FilesWrite,
            Self::Watch { .. } | Self::Unwatch { .. } => Capability::Watch,
            Self::Search { .. } | Self::SearchNext { .. } | Self::SearchClose { .. } => {
                Capability::Search
            }
            Self::Spawn { .. }
            | Self::Input { .. }
            | Self::CloseInput { .. }
            | Self::Kill { .. } => Capability::Process,
            Self::TerminalOpen { .. }
            | Self::TerminalInput { .. }
            | Self::TerminalResize { .. }
            | Self::TerminalClose { .. } => Capability::Terminal,
        }
    }
    /// A lost result from these requests must never trigger an automatic replay.
    pub fn changes_state(&self) -> bool {
        !matches!(
            self,
            Self::ChildPath { .. }
                | Self::ResolvePath { .. }
                | Self::RelativePath { .. }
                | Self::ParentPath { .. }
                | Self::ResourceUri { .. }
                | Self::ResolveUri { .. }
                | Self::Stat { .. }
                | Self::List { .. }
                | Self::Read { .. }
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    PermissionDenied,
    Unsupported,
    Unavailable,
    InvalidRequest,
    NotFound,
    Conflict,
    LimitExceeded,
    Cancelled,
    DeadlineExceeded,
    UnknownOutcome,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Failure {
    pub kind: FailureKind,
    pub message: String,
}
impl Failure {
    pub fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for Failure {}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Answer {
    RelativePath {
        /// An empty list names base itself. None is a non-descendant.
        components: Option<Vec<String>>,
    },
    Parent {
        path: Option<ProviderPath>,
    },
    Uri {
        uri: String,
    },
    Path {
        path: ProviderPath,
    },
    Relocated {
        entries: Vec<ResourceMove>,
    },
    Info {
        file: FileInfo,
    },
    Directory {
        entries: Vec<FileInfo>,
        cursor: Option<String>,
    },
    Bytes {
        bytes: Vec<u8>,
        revision: Option<String>,
        eof: bool,
    },
    Written {
        revision: Option<String>,
    },
    Started {
        handle: u64,
    },
    Ok,
    SearchMatches {
        page: crate::SearchPage,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnswerKind {
    RelativePath,
    Parent,
    Uri,
    Path,
    Relocated,
    Info,
    Directory,
    Bytes,
    Written,
    Started,
    Ok,
    Search,
}
impl Answer {
    pub fn kind(&self) -> AnswerKind {
        match self {
            Self::RelativePath { .. } => AnswerKind::RelativePath,
            Self::Parent { .. } => AnswerKind::Parent,
            Self::Uri { .. } => AnswerKind::Uri,
            Self::Path { .. } => AnswerKind::Path,
            Self::Relocated { .. } => AnswerKind::Relocated,
            Self::Info { .. } => AnswerKind::Info,
            Self::Directory { .. } => AnswerKind::Directory,
            Self::Bytes { .. } => AnswerKind::Bytes,
            Self::Written { .. } => AnswerKind::Written,
            Self::Started { .. } => AnswerKind::Started,
            Self::Ok => AnswerKind::Ok,
            Self::SearchMatches { .. } => AnswerKind::Search,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceMove {
    pub source: ProviderPath,
    pub target: ProviderPath,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Notification {
    /// Input credits: initial window, followed by bytes drained by the provider.
    Writable {
        handle: u64,
        bytes: u32,
    },
    TerminalWritable {
        handle: u64,
        bytes: u32,
    },
    WatchStopped {
        watch: u64,
        error: Failure,
    },
    Output {
        handle: u64,
        bytes: Vec<u8>,
        stderr: bool,
    },
    Exited {
        handle: u64,
        code: Option<i32>,
    },
    TerminalOutput {
        handle: u64,
        bytes: Vec<u8>,
    },
    TerminalDirectory {
        handle: u64,
        /// Provider-normalized cwd, or unavailable/outside this root.
        path: Option<ProviderPath>,
    },
    TerminalExited {
        handle: u64,
        code: Option<i32>,
    },
    Changed {
        watch: u64,
        sequence: u64,
        paths: Vec<ProviderPath>,
        rescan: bool,
    },
}

impl Notification {
    pub fn capability(&self) -> Capability {
        match self {
            Self::Writable { .. } | Self::Output { .. } | Self::Exited { .. } => {
                Capability::Process
            }
            Self::TerminalWritable { .. }
            | Self::TerminalDirectory { .. }
            | Self::TerminalOutput { .. }
            | Self::TerminalExited { .. } => Capability::Terminal,
            Self::WatchStopped { .. } | Self::Changed { .. } => Capability::Watch,
        }
    }
}
