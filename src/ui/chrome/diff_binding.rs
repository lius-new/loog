//! Diff view snapshot and semantic command port.
use crate::domain::identity::{PaneId, ViewId};
use crate::features::git::DiffRequest;
use crate::features::git::diff_document::DiffDocument;
use crate::features::git::diff_selection::DiffSelection;
use crate::ui::chrome::tabs_binding::Core;
use lgui::prelude::UiRect;
use std::{
    path::PathBuf,
    sync::{Arc, Weak},
};
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DiffSplit {
    pub drag: Option<PaneId>,
    pub ratio: f32,
}
impl Default for DiffSplit {
    fn default() -> Self {
        Self {
            drag: None,
            ratio: 0.5,
        }
    }
}
#[derive(Clone)]
pub struct ChangeFocus {
    pub view: ViewId,
    pub document: Weak<DiffDocument>,
    pub block: usize,
}

impl ChangeFocus {
    pub fn is_in(&self, view: ViewId, document: &Arc<DiffDocument>) -> bool {
        self.view == view && std::ptr::eq(self.document.as_ptr(), Arc::as_ptr(document))
    }

    pub fn is_current(&self, view: ViewId, document: &Arc<DiffDocument>) -> bool {
        self.is_in(view, document)
            || document.sections.as_ref().is_some_and(|sections| {
                sections
                    .iter()
                    .any(|section| self.is_in(view, &section.document))
            })
    }
}

pub struct EditorFrame {
    pub diff_split: DiffSplit,
    pub editor_hovered: Option<PaneId>,
    pub editor_vertical_scrollbar_dragging: bool,
    pub editor_horizontal_scrollbar_dragging: bool,
    pub selection: Option<DiffSelection>,
    pub change_focus: Option<ChangeFocus>,
}
pub struct Snapshot {
    pub workspace: Core,
    pub split: bool,
    pub change_highlight: bool,
    pub editor: EditorFrame,
}
pub enum Command {
    Wheel(PaneId, f32, f32, f32, f32),
    Activate(PaneId),
    SplitStart(PaneId),
    SplitDrag(PaneId, UiRect, f32),
    SplitEnd,
    Open(PathBuf),
    OpenResource(loog_workspace_api::ResourceRef),
    Layout(PaneId, bool),
    ToggleChangeHighlight,
    Navigate(PaneId, Arc<Vec<usize>>, bool),
    FocusChange(PaneId, ChangeFocus, Option<f32>),
    DismissChangeFocus(ViewId),
    Toggle(PaneId, DiffRequest),
    ToggleAll(PaneId),
    SelectStart(PaneId, UiRect, (f32, f32)),
    SelectDrag(PaneId, UiRect, (f32, f32)),
    SelectEnd,
    Focus(bool),
}
#[derive(Clone)]
pub struct Binding {
    read: Arc<dyn Fn() -> Snapshot + Send + Sync>,
    run: Arc<dyn Fn(Command) -> bool + Send + Sync>,
}
impl Binding {
    pub fn new(
        read: impl Fn() -> Snapshot + Send + Sync + 'static,
        run: impl Fn(Command) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            read: Arc::new(read),
            run: Arc::new(run),
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        (self.read)()
    }
    pub fn run(&self, c: Command) -> bool {
        (self.run)(c)
    }
}
