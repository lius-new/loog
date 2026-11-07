//! Transient pointer and IME state; selections and history live in each buffer.
use super::CloseRequest;
use crate::domain::identity::ViewId;
use crate::model::pane_layout::PaneId;
use lgui::core::KeyModifiers;
use std::{
    ops::Range,
    path::{Path, PathBuf},
    time::Instant,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionUnit {
    Rectangle { row: usize, column: usize },
    Character,
    Word,
    Line,
}
#[derive(Clone)]
pub struct DragSelection {
    pub document: ViewId,
    pub origin: Range<usize>,
    pub unit: SelectionUnit,
    pub point: (f32, f32),
}
pub use crate::ui::chrome::diff_binding::DiffSplit;

#[derive(Clone, Default)]
pub struct EditorState {
    pub saving: crate::app::saving::State,
    pub editing: crate::app::editing::State,
    pub images: crate::app::image_preview::State,
    /// Current Markdown publications changed; no service handle lives here.
    pub markdown_revision: u64,
    pub markdown_views:
        std::collections::HashMap<ViewId, crate::features::markdown::reader::InteractionState>,
    /// Changes only when a current background highlight result becomes ready.
    pub syntax_revision: u64,
    pub tabs: crate::ui::chrome::tab_state::UiState,
    /// Pending close operation waiting for the user to resolve dirty files.
    pub close_request: Option<CloseRequest>,
    /// Side-by-side diff divider: the pane being dragged and the split.
    pub diff_split: DiffSplit,
    /// The 1-based line under the pointer in a pane's gutter, which
    /// thickens that line's Git change bar.
    pub editor_gutter_hover: Option<(PaneId, usize)>,
    /// The range of the diagnostic under the pointer in a pane's code, whose
    /// message shows in a hover card.
    pub editor_diagnostic_hover: Option<(PaneId, Range<usize>)>,
    /// What the language servers are being asked or have said, and are shown.
    pub language: crate::app::language_ui::LanguageUi,
    /// Pointer x offset from the editor horizontal thumb's left.
    pub editor_horizontal_scrollbar_drag_offset: f32,
    /// True while the editor's horizontal overlay scrollbar is being dragged.
    pub editor_horizontal_scrollbar_dragging: bool,
    /// Whether the pointer is currently inside the visible code editor.
    pub editor_hovered: Option<PaneId>,
    /// The Git change peeked at from its gutter bar.
    pub editor_hunk_peek: Option<crate::ui::git::hunk_peek::HunkPeek>,
    /// Pointer y offset from the editor vertical thumb's top.
    pub editor_vertical_scrollbar_drag_offset: f32,
    /// True while the editor's vertical overlay scrollbar is being dragged.
    pub editor_vertical_scrollbar_dragging: bool,

    /// Focus of the shared welcome/home/editor surface, retained across swaps.
    pub focused: bool,
    pub pending_file: Option<crate::app::file_loading::PendingFile>,
    /// The pane divider being dragged.
    pub sash_drag: Option<crate::model::pane_layout::Sash>,

    /// Vim editing behaviour, when enabled in Settings.
    pub vim: loog_editor_core::vim::Vim,

    pub modifiers: KeyModifiers,
    pub drag: Option<DragSelection>,
    /// Text selected in the diff view.
    pub diff_selection: Option<crate::features::git::diff_selection::DiffSelection>,
    /// A press can place the caret smoothly; only a real drag must follow immediately.
    pub caret_dragging: bool,
    pub last_click: Option<(Instant, ViewId, f32, f32, u8)>,
    pub last_tree_click: Option<(Instant, PathBuf, f32, f32, u8)>,

    pub menu: Option<(f32, f32)>,
    pub menu_hover: Option<usize>,
    /// The input method composition in progress, and the view it belongs to.
    pub ime: Option<ImeSession>,
}

/// An input method composition. It belongs to the view where it started, so a
/// commit that arrives after the focus moved still lands in that view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImeSession {
    pub pane: PaneId,
    pub input_generation: u64,
    pub document: ViewId,
    /// Composition text; it is not part of the document until committed.
    pub text: String,
    pub cursor: Option<Range<usize>>,
}

impl EditorState {
    /// The composition text to draw in `pane` showing `document`, if any.
    pub fn preedit_for(&self, pane: PaneId, document: ViewId) -> Option<&ImeSession> {
        self.ime.as_ref().filter(|session| {
            session.pane == pane && session.document == document && !session.text.is_empty()
        })
    }

    /// Whether a composition is showing text, which owns every key.
    pub fn is_composing(&self) -> bool {
        self.ime
            .as_ref()
            .is_some_and(|session| !session.text.is_empty())
    }
}
impl EditorState {
    pub fn click_count(&mut self, document: ViewId, x: f32, y: f32) -> u8 {
        next_click_count(&mut self.last_click, document, x, y)
    }

    pub fn tree_click_count(&mut self, path: &Path, x: f32, y: f32) -> u8 {
        next_click_count(&mut self.last_tree_click, path.to_path_buf(), x, y)
    }

    pub fn tab_click_count(&mut self, document: ViewId, x: f32, y: f32) -> u8 {
        next_click_count(&mut self.tabs.last_tab_click, document, x, y)
    }
}

fn next_click_count<K: PartialEq>(
    previous: &mut Option<(Instant, K, f32, f32, u8)>,
    key: K,
    x: f32,
    y: f32,
) -> u8 {
    let now = Instant::now();
    let count = previous
        .as_ref()
        .filter(|(time, previous_key, previous_x, previous_y, _)| {
            previous_key == &key
                && now.duration_since(*time).as_millis() < 500
                && (x - *previous_x).abs() < 5.0
                && (y - *previous_y).abs() < 5.0
        })
        .map_or(1, |(_, _, _, _, count)| count % 3 + 1);
    *previous = Some((now, key, x, y, count));
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_tree_and_tab_click_sequences_are_independent() {
        let mut interaction = EditorState::default();
        let document = ViewId::new(1);
        let path = Path::new("src/main.rs");

        assert_eq!(interaction.tree_click_count(path, 10.0, 20.0), 1);
        assert_eq!(interaction.tree_click_count(path, 10.0, 20.0), 2);
        assert_eq!(interaction.tab_click_count(document, 10.0, 20.0), 1);
        assert_eq!(interaction.click_count(document, 10.0, 20.0), 1);
        assert_eq!(interaction.tab_click_count(document, 10.0, 20.0), 2);
    }

    #[test]
    fn changing_the_click_target_starts_a_new_sequence() {
        let mut interaction = EditorState::default();

        assert_eq!(
            interaction.tree_click_count(Path::new("first.rs"), 10.0, 20.0),
            1
        );
        assert_eq!(
            interaction.tree_click_count(Path::new("second.rs"), 10.0, 20.0),
            1
        );
    }
}
