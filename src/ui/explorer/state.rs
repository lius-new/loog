//! Explorer pointer, menu, inline-edit input and viewport state; owned
//! separately from the business model.
use std::collections::HashSet;
use std::path::PathBuf;

use crate::features::explorer::ExplorerContextTarget;
use crate::features::explorer::model::ExplorerModel;
use crate::ui::components::input::InputState;

/// A tree row to bring into sight; the view scrolls to it once it knows the
/// viewport, and the request ends with the next scroll by the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExplorerReveal {
    /// Visible row index counted from the top of the tree.
    pub row: usize,
    /// Ancestor rows that stay pinned above the row while it is scrolled to.
    pub covered: usize,
}

#[derive(Clone, Default)]
pub struct ExplorerUiState {
    pub reveal: Option<ExplorerReveal>,
    /// Pointer is over the file drawer (shows its scrollbars).
    pub hovered: bool,
    /// Cache key of the tree row under the pointer.
    pub hovered_path: Option<String>,
    /// Open context menu anchor in window coordinates.
    pub menu: Option<(f32, f32)>,
    /// Row the menu was opened on; `None` acts on the containing root.
    pub menu_target: Option<ExplorerContextTarget>,
    pub resource_menu: Option<crate::features::resource_tree::Target>,
    pub pressed_resource: Option<ResourcePress>,
    pub menu_hover: Option<usize>,
    /// Text of the inline create/rename row.
    pub create_input: InputState,
    pub create_error: Option<String>,
    /// Files picked with ctrl/shift-click.
    pub selection: HashSet<PathBuf>,
    /// Last plain or ctrl-clicked file; shift-click extends from it.
    pub selection_anchor: Option<PathBuf>,
    pub scroll: f32,
    pub scroll_x: f32,
    pub scrollbar_dragging: bool,
    pub scrollbar_drag_offset: f32,
    pub horizontal_scrollbar_dragging: bool,
    pub horizontal_scrollbar_drag_offset: f32,
}

#[derive(Clone)]
pub struct ResourcePress {
    pub scope: loog_workspace_api::ScopeId,
    pub path: loog_workspace_api::ProviderPath,
    pub point: (f32, f32),
    pub clicks: u8,
    pub dragged: bool,
}

impl ExplorerUiState {
    pub fn dismiss_menu(&mut self) {
        self.menu = None;
        self.menu_target = None;
        self.resource_menu = None;
        self.menu_hover = None;
    }

    /// Reset the inline row after its request opened, closed or failed.
    pub fn clear_edit(&mut self) {
        self.create_input.clear();
        // The row can unmount before a blur event reaches it. Its stale focus
        // would keep editor shortcuts in the Input keymap context.
        self.create_input.focused = false;
        self.create_error = None;
    }

    pub fn reset_scroll(&mut self) {
        self.reveal = None;
        self.scroll = 0.0;
        self.scroll_x = 0.0;
    }
}

#[derive(Clone, Default)]
pub struct ExplorerPanelState {
    pub model: ExplorerModel,
    pub resources: crate::features::resource_tree::Tree,
    pub ui: ExplorerUiState,
}
