//! Source Control view state: pointer, scrolling, the commit form, menus,
//! pickers and dialogs, and the user's view preferences. Business state
//! lives in `features::git::model`.
use std::collections::HashSet;

use std::path::PathBuf;

use crate::features::git::BranchInfo;
use crate::features::git::model::GitModel;
use crate::ui::components::input::InputState;
use crate::ui::git::branch_picker::BranchPicker;
use crate::ui::git::confirm::GitConfirm;
use crate::ui::git::menu::GitMenu;
use crate::ui::git::panel::{CommitCard, GitHover, TagCard};
use crate::ui::git::stash_switch::StashSwitchRequest;

/// Which kind of reference the history navigator lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NavView {
    #[default]
    Branches,
    Tags,
    Stashes,
}

/// A history pane whose width the user can drag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryPane {
    Branches,
    Detail,
}

#[derive(Clone)]
pub struct GitUiState {
    /// Pointer is inside the Source Control drawer.
    pub hovered: bool,
    /// The scrollable region under the pointer, the only one to show its bar.
    pub scroll_hover: Option<crate::ui::git::panel::ScrollRegion>,
    /// The drawer row under the pointer, which shows its action buttons.
    pub hover: Option<GitHover>,
    pub scroll: f32,
    /// Scroll of the Git panel's list.
    pub details_scroll: f32,
    /// Expand the Git panel's content below the editor; its header stays visible.
    pub panel_open: bool,
    /// The panel's tab: one of HISTORY, STASHES, WORKTREES or TAGS.
    pub panel_tab: u8,
    /// Manual header scroll; None keeps the selected tab visible.
    pub panel_tab_scroll: Option<f32>,
    /// User-selected height of the panel; None keeps the default.
    pub panel_height: Option<f32>,
    /// Pointer's offset from the panel's top edge while dragging it.
    pub panel_resize_offset: Option<f32>,
    pub scrollbar_dragging: bool,
    pub scrollbar_drag_offset: f32,
    pub details_scrollbar_dragging: bool,
    pub details_scrollbar_drag_offset: f32,
    pub navigator_scrollbar_dragging: bool,
    pub navigator_scrollbar_drag_offset: f32,
    pub commit_input: InputState,
    pub file_filter: InputState,
    /// Branches listed beside the history, for the repository they were read from.
    pub history_branches: Option<(PathBuf, Vec<BranchInfo>)>,
    /// The branch whose commits the history shows; None follows HEAD.
    pub history_scope: Option<(PathBuf, String)>,
    /// Lists only the commits that touched the active file.
    pub history_file_scope: bool,
    /// Text the commit list is narrowed to, matched against subject, author and hash.
    pub history_query: InputState,
    pub history_branch_scroll: f32,
    /// The stash whose files the history's detail pane shows.
    pub history_stash: Option<String>,
    /// The navigator shows one kind of reference at a time.
    pub history_nav: NavView,
    /// Scroll of the detail pane's changed-file tree, and its scrollbar drag.
    pub history_files_scroll: f32,
    pub files_scrollbar_dragging: bool,
    pub files_scrollbar_drag_offset: f32,
    /// Independent scroll and scrollbar drag of the compact commit summary.
    pub history_summary_scroll: f32,
    pub summary_scrollbar_dragging: bool,
    pub summary_scrollbar_drag_offset: f32,
    /// User-chosen height of the summary under the changed files, and the pointer's offset
    /// from its top edge while dragging.
    pub history_summary_height: Option<f32>,
    pub history_summary_resize: Option<f32>,
    /// Branch folders folded shut in the navigator, as `b:` or `r:` plus the path.
    pub history_collapsed: HashSet<String>,
    /// User-chosen widths of the history's navigator and detail pane.
    pub history_branches_width: Option<f32>,
    pub history_detail_width: Option<f32>,
    /// The pane edge being dragged, and the pointer's offset from it.
    pub history_pane_resize: Option<(HistoryPane, f32)>,
    pub commit_amend: bool,
    pub commit_signoff: bool,
    pub expanded_repositories: HashSet<u64>,
    pub collapsed_sections: HashSet<u8>,
    /// Group change sections by folder.
    pub tree_view: bool,
    /// Show diffs side by side.
    pub split_diff: bool,
    /// Highlight corresponding changes when interacting with a split diff.
    pub diff_change_highlight: bool,
    /// Blame the caret line in the editor.
    pub inline_blame: bool,
    /// Changed files picked with Ctrl/Shift-click, as `(section, path)`.
    pub selection: HashSet<(u8, PathBuf)>,
    /// Last plain or Ctrl-clicked row; Shift-click extends from it.
    pub selection_anchor: Option<(u8, PathBuf)>,
    pub menu: Option<GitMenu>,
    pub confirm: Option<GitConfirm>,
    pub tag_input: InputState,
    /// The hovered history commit's card.
    pub commit_card: Option<CommitCard>,
    pub tag_card: Option<TagCard>,
    pub hover_sequence: u64,
    pub branch_picker: Option<BranchPicker>,
    /// The picker's filter, kept outside `branch_picker` so the input's
    /// focus and IME state survive listing reloads.
    pub branch_picker_input: InputState,
    pub stash_switch: Option<StashSwitchRequest>,
    pub clone_visible: bool,
    pub clone_url: String,
    pub clone_error: Option<String>,
    pub clone_input_focused: bool,
}

impl Default for GitUiState {
    fn default() -> Self {
        Self {
            hovered: false,
            scroll_hover: None,
            hover: None,
            scroll: 0.0,
            details_scroll: 0.0,
            panel_open: true,
            panel_tab: crate::ui::git::panel::HISTORY,
            panel_tab_scroll: None,
            panel_height: None,
            panel_resize_offset: None,
            scrollbar_dragging: false,
            scrollbar_drag_offset: 0.0,
            details_scrollbar_dragging: false,
            details_scrollbar_drag_offset: 0.0,
            navigator_scrollbar_dragging: false,
            navigator_scrollbar_drag_offset: 0.0,
            commit_input: InputState::default(),
            file_filter: InputState::default(),
            history_branches: None,
            history_scope: None,
            history_file_scope: false,
            history_query: InputState::default(),
            history_branch_scroll: 0.0,
            history_stash: None,
            history_nav: NavView::default(),
            history_files_scroll: 0.0,
            files_scrollbar_dragging: false,
            files_scrollbar_drag_offset: 0.0,
            history_summary_scroll: 0.0,
            summary_scrollbar_dragging: false,
            summary_scrollbar_drag_offset: 0.0,
            history_summary_height: None,
            history_summary_resize: None,
            history_collapsed: HashSet::from([crate::ui::git::history_branches::REMOTE.to_owned()]),
            history_branches_width: None,
            history_detail_width: None,
            history_pane_resize: None,
            commit_amend: false,
            commit_signoff: false,
            expanded_repositories: HashSet::new(),
            collapsed_sections: [
                crate::ui::git::panel::WORKTREES,
                crate::ui::git::panel::STASHES,
                crate::ui::git::panel::TAGS,
            ]
            .into_iter()
            .collect(),
            tree_view: false,
            split_diff: false,
            diff_change_highlight: true,
            inline_blame: false,
            selection: HashSet::new(),
            selection_anchor: None,
            menu: None,
            confirm: None,
            tag_input: InputState::default(),
            commit_card: None,
            tag_card: None,
            hover_sequence: 0,
            branch_picker: None,
            branch_picker_input: InputState::default(),
            stash_switch: None,
            clone_visible: false,
            clone_url: String::new(),
            clone_error: None,
            clone_input_focused: false,
        }
    }
}

impl GitUiState {
    /// Expand or collapse the Git panel. Only the open tab's section counts as
    /// expanded, which is what loading history and tag cards look at.
    pub fn set_panel(&mut self, open: bool) {
        self.panel_open = open;
        if !open {
            self.panel_resize_offset = None;
            self.details_scrollbar_dragging = false;
            self.summary_scrollbar_dragging = false;
            self.hover = None;
            self.clear_hover_cards();
        }
        self.sync_panel_sections();
    }

    pub fn select_panel_tab(&mut self, tab: u8) {
        self.panel_tab = tab;
        self.panel_tab_scroll = None;
        self.panel_open = true;
        self.details_scroll = 0.0;
        self.hover = None;
        self.clear_hover_cards();
        self.sync_panel_sections();
    }

    fn sync_panel_sections(&mut self) {
        use crate::ui::git::panel::{HISTORY, STASHES, TAGS, WORKTREES};
        for section in [HISTORY, STASHES, TAGS, WORKTREES] {
            if self.panel_open && section == self.panel_tab {
                self.collapsed_sections.remove(&section);
            } else {
                self.collapsed_sections.insert(section);
            }
        }
    }

    pub fn reset_scroll(&mut self) {
        self.scroll = 0.0;
        self.details_scroll = 0.0;
        self.history_summary_scroll = 0.0;
        self.summary_scrollbar_dragging = false;
        self.selection.clear();
        self.selection_anchor = None;
        self.hover = None;
        self.clear_hover_cards();
    }

    pub fn clear_hover_cards(&mut self) {
        self.commit_card = None;
        self.tag_card = None;
        self.hover_sequence = self.hover_sequence.wrapping_add(1);
    }
}

#[derive(Clone, Default)]
pub struct GitPanelState {
    pub model: GitModel,
    pub ui: GitUiState,
}

