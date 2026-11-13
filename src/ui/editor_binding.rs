//! A viewport reads its own immutable scene and emits application intents.
use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

use lgui::core::PointerButton;
use lgui::prelude::{Element, UiRect};
use loog_editor_core::identity::{PaneId, ViewId};
use loog_editor_core::text::{Editor, Selection, TextBuffer};
use loog_editor_core::vim::CursorShape;

use crate::domain::document::DocumentStamp;
use crate::domain::language::ResolvedLanguage;
use crate::domain::line_change::LineChange;

pub(crate) struct DocumentMeta {
    pub resource: loog_workspace_api::ResourceRef,
    pub lang: ResolvedLanguage,
}
#[derive(Clone)]
pub(crate) struct FoldMarker {
    pub line: usize,
    pub start: usize,
    pub end: usize,
    pub collapsed: bool,
}
pub(crate) struct ViewDocument {
    pub folds: Arc<[FoldMarker]>,
    pub id: ViewId,
    /// Names the exact document lifetime, text and language this view shows.
    pub stamp: DocumentStamp,
    pub meta: DocumentMeta,
    pub buffer: TextBuffer,
    pub projection: Arc<loog_editor_core::text::DisplayMap>,
    pub selection: Selection,
    pub secondary: Vec<Selection>,
    pub scroll: (f32, f32),
}
impl ViewDocument {
    pub fn editor(&self) -> Editor<'_> {
        Editor::new(&self.buffer, self.selection).with_secondary(&self.secondary)
    }
}
pub(crate) struct Preedit {
    pub pane: PaneId,
    pub document: ViewId,
    pub text: String,
    pub cursor: Option<Range<usize>>,
}
pub(crate) struct ViewState {
    pub pane: PaneId,
    pub document: Option<ViewDocument>,
    pub focused_pane: bool,
    pub focused: bool,
    pub accepts_text: bool,
    pub cursor_shape: CursorShape,
    pub selections: Vec<Range<usize>>,
    pub search_matches: Vec<Range<usize>>,
    /// What the language server reports for this exact text.
    pub diagnostics: Arc<[crate::features::language_service::diagnostics::Diagnostic]>,
    pub smooth_caret: bool,
    pub dragging: bool,
    pub caret_dragging: bool,
    pub preedit: Option<Preedit>,
    pub editor_gutter_hover: Option<(PaneId, usize)>,
    pub editor_diagnostic_hover: Option<(PaneId, Range<usize>)>,
    pub editor_hovered: Option<PaneId>,
    pub editor_vertical_scrollbar_dragging: bool,
    pub editor_horizontal_scrollbar_dragging: bool,
    pub line_changes: BTreeMap<usize, LineChange>,
    pub overview_marks: Vec<OverviewMark>,
    /// The conflicts Git reports in this file.
    pub conflicts: Arc<[crate::features::git::conflict::ConflictRegion]>,
    pub has_repository: bool,
    pub peek_visible: bool,
    /// What the language servers said about the word the pointer rests on.
    pub hover: Option<HoverView>,
    /// The list open over the code, if any.
    pub popup: Option<crate::ui::editor::popup::View>,
    pub snippet_choices: Option<(u64, crate::ui::editor::popup::View)>,
    /// Generation of the code-action list available at the current selection.
    pub lightbulb: Option<u64>,
    pub quickfix: Option<QuickFixView>,
    pub language_peek: Option<crate::ui::editor::peek::View>,
    /// The signature of the call being typed, with where it opened.
    pub signature: Option<(usize, crate::ui::editor::signature::View)>,
    /// Other occurrences of the symbol under the caret.
    pub symbol_highlights: Arc<[Range<usize>]>,
    /// The word to show as a link to its definition while the key is held.
    pub link: Option<Range<usize>>,
    /// Hints the servers want shown inside lines, sorted by offset.
    pub inlay_hints: Arc<[crate::ui::editor::hints::Hint]>,
}

/// A server hover text, anchored to the text it describes.
#[derive(Clone)]
pub(crate) struct HoverView {
    pub anchor: Range<usize>,
    pub docs: crate::ui::markdown::card::Document,
}
pub(crate) struct QuickFixView {
    pub revision: u64,
    pub range: Range<usize>,
    pub labels: Vec<String>,
}
/// One adjacent run of changes in the overview ruler, with a zero-based first line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OverviewMark {
    pub first: usize,
    pub lines: usize,
    pub change: LineChange,
}
impl ViewState {
    pub fn preedit_for(&self, pane: PaneId, document: ViewId) -> Option<&Preedit> {
        self.preedit.as_ref().filter(|value| {
            value.pane == pane && value.document == document && !value.text.is_empty()
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Metrics {
    pub viewport_w: f32,
    pub viewport_h: f32,
    pub content_w: f32,
    pub content_h: f32,
    pub max_x: f32,
    pub max_y: f32,
}
pub(crate) enum Event {
    Fold {
        view: ViewId,
        stamp: DocumentStamp,
        start: usize,
    },
    DiagnosticLink(String),
    /// A link in the hover documentation was clicked.
    OpenLink(String),
    /// The text selected in the hover documentation, or `None` once cleared.
    HoverSelect(Option<String>),
    /// A press on the hover card began or ended.
    HoverPress(bool),
    PeekOpen {
        revision: u64,
    },
    PeekClose {
        revision: u64,
    },
    QuickFix {
        revision: u64,
        index: usize,
    },
    CodeActions {
        revision: u64,
    },
    PointerDown {
        document: ViewId,
        point: (f32, f32),
        button: PointerButton,
        code_left: f32,
        metrics: Metrics,
    },
    PointerDrag((f32, f32)),
    PointerUp,
    Wheel {
        x: f32,
        y: f32,
        metrics: Metrics,
    },
    GutterHover(Option<(PaneId, usize)>),
    /// The range of the diagnostic now under the pointer, if any.
    DiagnosticHover(Option<(PaneId, Range<usize>)>),
    Peek {
        line: usize,
    },
    /// A conflict's action bar kept one side of it.
    ResolveConflict {
        region: crate::features::git::conflict::ConflictRegion,
        resolution: crate::features::git::conflict::Resolution,
    },
    /// The pointer moved over the code area.
    HoverMove {
        point: (f32, f32),
        code_left: f32,
        metrics: Metrics,
    },
    /// The pointer left the code area.
    HoverLeave,
    /// The mouse did something in the popup list.
    Popup(crate::ui::editor::popup::Event),
    SnippetChoice {
        revision: u64,
        event: crate::ui::editor::popup::Event,
    },
    Signature {
        revision: u64,
        forward: bool,
    },
    /// The text selected in signature help `revision`, or `None` once cleared.
    SignatureSelect {
        revision: u64,
        text: Option<String>,
    },
    Text(String),
    Composition {
        text: String,
        cursor: Option<Range<usize>>,
    },
    CompositionEnd,
    Focus,
    Blur,
    ScrollbarDown {
        vertical: bool,
        offset: f32,
        position: Option<f32>,
    },
    ScrollbarDrag {
        vertical: bool,
        pointer: f32,
        start: f32,
        travel: f32,
        max: f32,
    },
    ScrollbarUp {
        vertical: bool,
    },
}
pub(crate) struct PeekPlacement {
    pub rect: UiRect,
    pub code_left: f32,
    pub scroll_y: f32,
    pub metrics: Metrics,
}
type HighlightProvider = dyn Fn(
        &ViewDocument,
        Vec<crate::features::syntax::Window>,
    ) -> Vec<crate::features::syntax::WindowTokens>
    + Send
    + Sync;

#[derive(Clone)]
pub(crate) struct Binding {
    read: Arc<dyn Fn() -> ViewState + Send + Sync>,
    dispatch: Arc<dyn Fn(Event) + Send + Sync>,
    overlay: Arc<dyn Fn(PeekPlacement) -> Option<Element> + Send + Sync>,
    highlights: Arc<HighlightProvider>,
}
impl Binding {
    pub fn new(
        read: impl Fn() -> ViewState + Send + Sync + 'static,
        dispatch: impl Fn(Event) + Send + Sync + 'static,
        overlay: impl Fn(PeekPlacement) -> Option<Element> + Send + Sync + 'static,
    ) -> Self {
        Self {
            read: Arc::new(read),
            dispatch: Arc::new(dispatch),
            overlay: Arc::new(overlay),
            highlights: Arc::new(|_, _| Vec::new()),
        }
    }
    pub fn get(&self) -> ViewState {
        (self.read)()
    }
    pub fn with_highlights(
        mut self,
        provider: impl Fn(
            &ViewDocument,
            Vec<crate::features::syntax::Window>,
        ) -> Vec<crate::features::syntax::WindowTokens>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.highlights = Arc::new(provider);
        self
    }
    pub fn highlights(
        &self,
        document: &ViewDocument,
        windows: Vec<crate::features::syntax::Window>,
    ) -> Vec<crate::features::syntax::WindowTokens> {
        (self.highlights)(document, windows)
    }
    pub fn send(&self, event: Event) {
        (self.dispatch)(event);
    }
    pub fn overlay(&self, placement: PeekPlacement) -> Option<Element> {
        (self.overlay)(placement)
    }
}
