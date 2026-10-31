//! Presentation identity and pure per-view state, independent of renderers.
use crate::domain::identity::{DiffId, FileId, ViewId};
use loom_editor_core::text::Selection;

pub const SETTINGS_TITLE: &str = "Settings";
pub const KEYMAP_TITLE: &str = "Keymap";
pub const EXTENSIONS_TITLE: &str = "Extensions";

/// Built-in pages that open in a tab instead of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppPage {
    Settings,
    Keymap,
    Extensions,
}

impl AppPage {
    pub fn title(self) -> &'static str {
        match self {
            AppPage::Settings => SETTINGS_TITLE,
            AppPage::Keymap => KEYMAP_TITLE,
            AppPage::Extensions => EXTENSIONS_TITLE,
        }
    }
}

/// Source selection and scroll of one independent view.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewState {
    pub selection: Selection,
    pub secondary: Vec<Selection>,
    pub scroll_x: f32,
    pub scroll_y: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewTarget {
    Text(FileId),
    Diff(DiffId),
    Builtin(AppPage),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Presentation {
    #[default]
    Source,
    Markdown,
    Image,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ReaderPosition {
    pub source_byte: usize,
    pub block_fraction: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DocumentView {
    pub id: ViewId,
    pub target: ViewTarget,
    pub presentation: Presentation,
    /// Explicit side-preview association; it never owns the referenced view.
    pub associated_source: Option<ViewId>,
    pub source: ViewState,
    pub reader: ReaderPosition,
    pub image: crate::domain::media::Position,
    /// Invalidates input and delayed source interactions on presentation changes.
    pub input_generation: u64,
}

impl DocumentView {
    pub fn new(id: ViewId, target: ViewTarget, presentation: Presentation) -> Self {
        Self {
            id,
            target,
            presentation,
            associated_source: None,
            source: ViewState::default(),
            reader: ReaderPosition::default(),
            image: Default::default(),
            input_generation: 0,
        }
    }
    pub fn document(&self) -> Option<FileId> {
        match self.target {
            ViewTarget::Text(id) => Some(id),
            _ => None,
        }
    }
    pub fn accepts_text(&self) -> bool {
        matches!(self.target, ViewTarget::Text(_)) && self.presentation == Presentation::Source
    }
}
