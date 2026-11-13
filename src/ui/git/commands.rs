//! Narrow Source Control access and semantic requests to the application.
use super::state::GitPanelState;
use crate::features::git::actions::{BlockAction, Checkout, StashMode};
use crate::features::git::diff_document::ChangeBlock;
use crate::features::git::{ConflictSide, DiffRequest, RepositoryState};
use crate::ui::components::state_access::StateAccess;
use std::path::PathBuf;
use std::sync::Arc;

pub enum Command {
    Resource(crate::features::git::resources::Intent),
    ClosePeek,
    NavigatePeek {
        pane: crate::domain::pane_layout::PaneId,
        path: PathBuf,
        root: PathBuf,
        relative: PathBuf,
        line: usize,
        y: f32,
    },
    RequestDiscardBlock {
        root: PathBuf,
        path: PathBuf,
        block: ChangeBlock,
    },
    OpenCommitDiff {
        root: PathBuf,
        oid: String,
        path: PathBuf,
    },
    ResizeStart,
    Resize {
        width: f32,
    },
    ResizeEnd,
    ShowSourceControl,
    PressChange {
        request: DiffRequest,
        untracked: bool,
        point: (f32, f32),
        /// The row's section and that section's visible files in row order,
        /// for Ctrl/Shift multi-selection.
        section: u8,
        order: Arc<Vec<PathBuf>>,
    },
    OpenFile {
        path: PathBuf,
    },
    CopyPath {
        path: PathBuf,
        relative: bool,
        clipboard: Arc<dyn lgui::services::Clipboard>,
    },
    Reveal {
        path: PathBuf,
    },
    CloneRepository,

    HoverCommit {
        root: PathBuf,
        oid: String,
        anchor: lgui::prelude::UiRect,
    },
    HoverTag {
        root: PathBuf,
        name: String,
        anchor: lgui::prelude::UiRect,
    },
    Commit,
    CommitAndPush,
    RequestCreateTag {
        root: PathBuf,
        branch: Option<String>,
        target: String,
    },
    CreateTag {
        root: PathBuf,
        name: String,
        target: String,
    },
    RequestDeleteTag {
        root: PathBuf,
        name: String,
    },
    DeleteTag {
        root: PathBuf,
        name: String,
    },
    RequestPushTag {
        root: PathBuf,
        name: String,
        pos: (f32, f32),
    },
    PushTag {
        root: PathBuf,
        name: String,
        remote: String,
    },
    LoadMoreHistory {
        root: PathBuf,
    },
    /// Show the history of `name`, or of HEAD with `None`.
    SelectHistoryBranch {
        root: PathBuf,
        name: Option<String>,
    },
    /// Show a stash's files in the history's detail pane.
    SelectHistoryStash {
        root: PathBuf,
        oid: String,
    },
    ToggleCommit {
        root: PathBuf,
        oid: String,
    },
    OpenCommitChanges {
        root: PathBuf,
        oid: String,
        focus: Option<PathBuf>,
    },
    OpenWorktree {
        path: PathBuf,
    },
    AddWorktreeFolder {
        path: PathBuf,
    },
    RequestRemoveWorktree {
        repository: PathBuf,
        path: PathBuf,
        force: bool,
    },
    ApplyStash {
        oid: String,
        pop: bool,
    },
    RequestDropStash {
        oid: String,
    },
    RequestAbort,
    ContinueOperation,
    StagePaths {
        paths: Vec<PathBuf>,
    },
    /// Mark unmerged `paths` resolved although conflict markers remain.
    StageConflicted {
        paths: Vec<PathBuf>,
    },
    UnstagePaths {
        paths: Vec<PathBuf>,
    },
    RequestDiscard {
        tracked: Vec<PathBuf>,
        untracked: Vec<PathBuf>,
    },
    ResolveConflict {
        path: PathBuf,
        side: ConflictSide,
    },
    LoadBranches {
        repository: PathBuf,
        sequence: u64,
    },
    Checkout {
        root: PathBuf,
        checkout: Checkout,
        mode: StashMode,
    },
    FetchBranches,
    RequestCreateWorktree {
        repository: PathBuf,
        checkout: Checkout,
    },
    CreateWorktree {
        root: PathBuf,
        path: PathBuf,
        checkout: Checkout,
    },
    RemoveWorktree {
        root: PathBuf,
        path: PathBuf,
        force: bool,
    },
    DiscardPaths {
        root: PathBuf,
        tracked: Vec<PathBuf>,
        untracked: Vec<PathBuf>,
    },
    CommitAll,
    AbortOperation {
        root: PathBuf,
        operation: RepositoryState,
    },
    DropStash {
        root: PathBuf,
        oid: String,
    },
    ApplyBlock {
        root: PathBuf,
        path: PathBuf,
        target: crate::features::git::DiffTarget,
        block: ChangeBlock,
        action: BlockAction,
    },
    ChooseWorktreeLocation,
    OpenDiff {
        request: crate::features::git::DiffRequest,
        untracked: bool,
    },
    RevertCommit {
        root: PathBuf,
        oid: String,
    },
    UndoLastCommit,
    StashAll,
    SelectRepository {
        root: PathBuf,
    },
    Sync,
}

#[derive(Clone, Default)]
pub struct Frame {
    pub resizing: bool,
    pub resizable: bool,
    pub terminal_focused: bool,
    pub active_diff: Option<DiffRequest>,
    pub active_diff_focus: Option<DiffRequest>,
    /// The file in the active editor tab, for the Commits view's file scope.
    pub active_file: Option<PathBuf>,
}

#[derive(Clone)]
pub struct Binding {
    pub panel: StateAccess<GitPanelState>,
    dispatch: Arc<dyn Fn(Command) -> bool + Send + Sync>,
    frame: Arc<dyn Fn() -> Frame + Send + Sync>,
}
impl Binding {
    pub fn new(
        panel: StateAccess<GitPanelState>,
        frame: impl Fn() -> Frame + Send + Sync + 'static,
        dispatch: impl Fn(Command) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            panel,
            frame: Arc::new(frame),
            dispatch: Arc::new(dispatch),
        }
    }
    pub fn get(&self) -> GitPanelState {
        self.panel.get()
    }
    pub fn update(&self, update: impl FnOnce(&mut GitPanelState)) {
        self.panel.update(update)
    }
    pub fn try_update(&self, update: impl FnOnce(&mut GitPanelState) -> bool) -> bool {
        self.panel.try_update(update)
    }
    pub fn dispatch(&self, command: Command) -> bool {
        (self.dispatch)(command)
    }
    pub fn frame(&self) -> Frame {
        (self.frame)()
    }
}

/// A live, read-only Source Control snapshot. Views cannot publish scans or mutate operations.
#[derive(Clone)]
pub struct Snapshot(Arc<dyn Fn() -> crate::features::git::GitStoreSnapshot + Send + Sync>);
impl Snapshot {
    pub fn get(&self) -> crate::features::git::GitStoreSnapshot {
        (self.0)()
    }
}
impl From<lgui::prelude::State<crate::features::git::GitStoreSnapshot>> for Snapshot {
    fn from(state: lgui::prelude::State<crate::features::git::GitStoreSnapshot>) -> Self {
        Self(Arc::new(move || state.get()))
    }
}
