//! Tab pointer/drag/menu values, independent of the root application.
use crate::domain::identity::{PaneId, ViewId};
use crate::domain::pane_layout::Direction;
use std::{collections::HashMap, path::PathBuf, time::Instant};
/// Where a dragged tab lands outside its own strip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TabDrop {
    /// Into another pane's strip, before `index` (the tab count appends).
    Insert { pane: PaneId, index: usize },
    /// Onto a pane's surface: merge into it, or split it on one side.
    Pane {
        pane: PaneId,
        split: Option<Direction>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct TabDragState {
    pub pane: PaneId,
    pub source: ViewId,
    pub pointer_origin_x: f32,
    pub pointer_origin_y: f32,
    /// Final index while reordering within the source strip.
    pub target_index: usize,
    pub active: bool,
    /// Set while the pointer is outside the source strip over a drop target.
    pub drop: Option<TabDrop>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FileDragSource {
    Explorer {
        clicks: u8,
    },
    Changes {
        request: crate::features::git::DiffRequest,
        untracked: bool,
    },
}

/// A file pressed in a drawer: a click on release, or a drag into the
/// editor once the pointer moves far enough.
#[derive(Clone, Debug, PartialEq)]
pub struct FileDragState {
    pub path: PathBuf,
    pub source: FileDragSource,
    pub origin: (f32, f32),
    pub point: (f32, f32),
    pub active: bool,
    pub drop: Option<TabDrop>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TabContextMenuState {
    pub position: (f32, f32),
    pub pane: PaneId,
    pub target: ViewId,
    pub hovered: Option<usize>,
}
#[derive(Clone, Default)]
pub struct UiState {
    pub file_drag: Option<FileDragState>,
    pub tab_context_menu: Option<TabContextMenuState>,
    pub tab_drag: Option<TabDragState>,
    pub tab_hovered: Option<(PaneId, ViewId)>,
    pub tab_scroll: HashMap<PaneId, f32>,
    pub last_tab_click: Option<(Instant, ViewId, f32, f32, u8)>,
}
