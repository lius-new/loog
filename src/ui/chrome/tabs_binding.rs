//! Read-only document/layout snapshot and tab command port.
use crate::domain::identity::{PaneId, ViewId};
use crate::ui::chrome::tab_state::UiState;
use std::path::PathBuf;
use std::sync::Arc;
pub type Core = crate::domain::workspace::Workspace<
    crate::features::git::diff_document::DiffDocument,
    crate::features::git::DiffRequest,
>;
#[derive(Clone)]
pub struct Pending {
    pub pane: PaneId,
    pub path: PathBuf,
    pub indicator: bool,
    pub generation: u64,
}
pub struct EditorFrame<'a> {
    pub tabs: &'a UiState,
    pub pending_file: Option<Pending>,
}
pub struct Frame<'a> {
    pub workspace: &'a Core,
    pub editor: EditorFrame<'a>,
    pub welcome_panes: Vec<PaneId>,
}
pub struct Snapshot {
    pub core: Core,
    pub tabs: UiState,
    pub pending: Option<Pending>,
    pub welcome_panes: Vec<PaneId>,
}
impl Snapshot {
    pub fn frame(&self) -> Frame<'_> {
        Frame {
            workspace: &self.core,
            editor: EditorFrame {
                tabs: &self.tabs,
                pending_file: self.pending.clone(),
            },
            welcome_panes: self.welcome_panes.clone(),
        }
    }
}
pub enum Command {
    Wheel(PaneId, f32, f32),
    Press(PaneId, ViewId, usize, f32, f32),
    Menu(PaneId, ViewId, (f32, f32)),
    Close(PaneId, ViewId),
    CancelPending(u64),
}
#[derive(Clone)]
pub struct Binding {
    read: Arc<dyn Fn() -> Snapshot + Send + Sync>,
    run: Arc<dyn Fn(Command) + Send + Sync>,
}
impl Binding {
    pub fn new(
        read: impl Fn() -> Snapshot + Send + Sync + 'static,
        run: impl Fn(Command) + Send + Sync + 'static,
    ) -> Self {
        Self {
            read: Arc::new(read),
            run: Arc::new(run),
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        (self.read)()
    }
    pub fn run(&self, c: Command) {
        (self.run)(c)
    }
}
