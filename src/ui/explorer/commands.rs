//! Semantic Explorer commands and the read-only per-frame inputs the
//! application supplies. The view never sees the root state.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lgui::core::{UiFocusHandle, UiId};

use crate::features::explorer::ExplorerContextTarget;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuAction {
    NewFile,
    NewFolder,
    RefreshExplorer,
    OpenTerminal,
    WorkspaceTrust,
    Cut,
    Copy,
    Rename,
    Delete,
    AddFolder,
    RemoveFolder,
    RevealInFileExplorer,
    CopyPath,
    CopyRelativePath,
    PreviewMarkdown,
    ToggleSvg,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    ResourceRecovery {
        resource: loog_workspace_api::ResourceRef,
        forget: bool,
    },
    ResourceMenu {
        point: (f32, f32),
        target: crate::features::resource_tree::Target,
    },
    ResourceAction {
        action: MenuAction,
        target: crate::features::resource_tree::Target,
    },
    ResourceDirectory {
        scope: loog_workspace_api::ScopeId,
        path: loog_workspace_api::ProviderPath,
        more: bool,
    },
    ResourceFile {
        scope: loog_workspace_api::ScopeId,
        file: loog_workspace_api::FileInfo,
    },
    PressResourceFile {
        scope: loog_workspace_api::ScopeId,
        file: loog_workspace_api::FileInfo,
        point: (f32, f32),
    },
    ReleaseResourceFile {
        scope: loog_workspace_api::ScopeId,
        file: loog_workspace_api::FileInfo,
        point: (f32, f32),
    },
    /// Expand or collapse a directory row (reads it on first expansion).
    ToggleDirectory(String),
    /// Open the context menu; `None` targets the drawer background.
    OpenMenu {
        point: (f32, f32),
        target: Option<ExplorerContextTarget>,
    },
    /// Left press on a file row: opens on release, or starts a drag.
    PressFile {
        path: PathBuf,
        point: (f32, f32),
    },
    /// Middle click on a file row: open it as a permanent tab.
    OpenPinned(PathBuf),
    ChooseFolder,
    CloneRepository,
    SubmitEdit,
    CancelEdit,
    ResizeStart,
    /// New drawer width in logical pixels.
    Resize(f32),
    ResizeEnd,
    /// A context-menu item; the menu has already closed itself.
    Menu {
        action: MenuAction,
        target: Option<ExplorerContextTarget>,
    },
}

#[derive(Clone)]
pub struct Commands(Arc<dyn Fn(Command) + Send + Sync>);

impl Commands {
    pub fn new(dispatch: impl Fn(Command) + Send + Sync + 'static) -> Self {
        Self(Arc::new(dispatch))
    }

    pub fn dispatch(&self, command: Command) {
        (self.0)(command);
    }
}

/// Source-control tone of a tree label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitTone {
    Conflict,
    Deleted,
    Modified,
    Added,
    Ignored,
}

/// Source-control decoration of one tree row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Decoration {
    pub tone: Option<GitTone>,
    /// Short status letter shown on file rows.
    pub indicator: &'static str,
}

/// Read-only inputs for one frame of the tree.
pub struct Frame<'a> {
    /// Path of the active editor tab, highlighted in the tree.
    pub active_path: Option<&'a Path>,
    pub active_resource: Option<&'a loog_workspace_api::ResourceRef>,
    /// Drawer resize drag in progress.
    pub resizing: bool,
    /// Whether the drawer edge shows a resize handle.
    pub resizable: bool,
    pub decorate: &'a dyn Fn(&Path) -> Decoration,
}

/// Focus and identity of the inline create/rename input.
#[derive(Clone)]
pub struct EditInput {
    pub id: UiId,
    pub focus: UiFocusHandle,
}
