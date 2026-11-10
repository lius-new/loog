//! What the editor does with language servers while the user works: hover,
//! navigation, completion and the rest. Each feature keeps its state in
//! [`LanguageUi`], asks the servers through the request channel, and is advanced
//! on the UI thread by [`tick`] when answers arrive.
pub(crate) mod actions;
pub(crate) mod completion;
pub(crate) mod edit;
pub(crate) mod expand;
pub(crate) mod highlight;
pub(crate) mod hover;
pub(crate) mod inlay;
pub(crate) mod keys;
pub(crate) mod lightbulb;
pub(crate) mod link;
pub(crate) mod lists;
pub(crate) mod navigate;
pub(crate) mod outline;
pub(crate) mod peek;
pub(crate) mod popup;
pub(crate) mod quickfix;
pub(crate) mod refactor;
pub(crate) mod save;
pub(crate) mod signature;
pub(crate) mod snippet_session;

use super::context::AppContext;
use super::state::AppState;
use crate::domain::document::DocumentStamp;
use crate::domain::identity::{FileId, PaneId, ViewId};
use crate::features::language_service::sync::Stamp;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

/// All transient language-server interaction state of the editor.
#[derive(Clone, Default)]
pub struct LanguageUi {
    /// Ownership of transient source interactions; document services remain shared.
    interaction_view: Option<(ViewId, u64)>,
    pub file_changes: Vec<crate::features::providers::FileChange>,
    pub peek: Option<peek::Peek>,
    pub peek_revision: u64,
    pub hover: hover::Hover,
    /// The word under the pointer as a link to its definition.
    pub link: link::Link,
    pub history: navigate::History,
    pub navigation_revision: u64,
    /// The list open over the code, if any.
    pub popup: Option<popup::Popup>,
    /// Text selected in the documentation beside the list, with the row whose
    /// documentation it came from.
    pub docs_selection: Option<(usize, String)>,
    /// Text selected in signature help, with the revision it came from.
    pub signature_selection: Option<(u64, String)>,
    /// A question whose answer the person is waiting for.
    pub pending: Option<actions::Pending>,
    pub signature: Option<signature::Session>,
    pub signature_timer: Option<Arc<crate::app::ui_timer::Timer>>,
    pub signature_revision: u64,
    pub highlights: highlight::Highlights,
    /// The Outline drawer's tree and view state.
    pub outline: outline::OutlineUi,
    pub lightbulb: lightbulb::Lightbulb,
    pub quickfix: quickfix::QuickFix,
    pub inlay: inlay::Inlay,
    pub expansion: expand::Expansion,
    /// A hierarchy or code lens request in flight.
    pub lists: Option<lists::Pending>,
    /// Steps taken before a file is written.
    pub save: Option<save::Run>,
    pub save_counter: u64,
    /// A rename, code action or formatting request in flight.
    pub refactor: Option<refactor::Pending>,
    /// The completion list, once asked for.
    pub completion: Option<completion::Session>,
    pub completion_commands: Vec<(u64, serde_json::Value)>,
    pub words_request: Option<Arc<()>>,
    pub completion_timer: Option<Arc<crate::app::ui_timer::Timer>>,
    /// The last character typed into the document, until the next update looks.
    pub typed: Option<char>,
    /// A snippet whose tab stops are being filled in.
    pub snippet: Option<snippet_session::Session>,
    pub snippet_revision: u64,
    /// The editor's rectangle as last seen, for revealing the caret.
    pub rect: Option<lgui::prelude::UiRect>,
    /// Changes that span several documents, by transaction, for undoing them
    /// together.
    pub transactions: std::collections::HashMap<u64, Vec<FileId>>,
    pub next_transaction: u64,
}

impl LanguageUi {
    /// Closes what is open and drops what is awaited.
    pub fn close_popups(&mut self) {
        self.popup = None;
        self.docs_selection = None;
        self.pending = None;
        self.refactor = None;
        self.lists = None;
        completion::close_words(self);
        self.completion = None;
    }
}

/// Opens a link clicked in documentation. The text comes from a language
/// server, so only web pages open, and only because the person clicked.
pub(crate) fn open_link(state: &AppContext, url: String) {
    if (url.to_ascii_lowercase().starts_with("https://")
        || url.to_ascii_lowercase().starts_with("http://"))
        && !url.contains(['\0', '\r', '\n'])
        && let Err(error) = crate::infrastructure::update::open_page(&url)
    {
        state.update(|app| app.show_error(format!("Could not open link: {error}")));
    }
}

/// The text selected in a card over the code, if there is one: in the list's
/// documentation (while it is still the row shown), a hover card or signature help.
pub(crate) fn selected_text(app: &AppState) -> Option<String> {
    app.editor
        .language
        .hover
        .selection
        .clone()
        .or_else(|| docs_selection(app))
        .or_else(|| {
            let (revision, text) = app.editor.language.signature_selection.as_ref()?;
            app.editor
                .language
                .signature
                .as_ref()
                .is_some_and(|session| session.revision == *revision)
                .then(|| text.clone())
        })
}

/// The text selected in the list's documentation, if it is still the row shown.
pub(crate) fn docs_selection(app: &AppState) -> Option<String> {
    let language = &app.editor.language;
    let (row, text) = language.docs_selection.as_ref()?;
    let selected = if completion_visible(app) {
        language.completion.as_ref()?.selected
    } else {
        language.popup.as_ref()?.selected
    };
    (*row == selected).then(|| text.clone())
}

/// The document a view shows, as the servers need to be told about it.
#[derive(Clone)]
pub(crate) struct Target {
    pub pane: PaneId,
    pub view: ViewId,
    pub input_generation: u64,
    pub document: FileId,
    pub path: PathBuf,
    pub stamp: DocumentStamp,
    pub text: Arc<String>,
    pub cursor: usize,
    pub selection: Option<Range<usize>>,
}

impl Target {
    pub fn same_view(&self, other: &Self) -> bool {
        self.view == other.view
            && self.input_generation == other.input_generation
            && self.pane == other.pane
    }
    /// The state the servers know the document in.
    pub fn lsp_stamp(&self) -> Stamp {
        lsp_stamp(&self.stamp)
    }
}

pub(crate) fn lsp_stamp(stamp: &DocumentStamp) -> Stamp {
    Stamp {
        incarnation: stamp.incarnation,
        version: stamp.version,
    }
}

/// The text document `pane` shows, if it is one a server can be asked about.
pub(crate) fn target(app: &AppState, pane: PaneId) -> Option<Target> {
    let view = app.workspace.pane(pane)?.active()?;
    let state = app.workspace.view(view).filter(|v| v.accepts_text())?;
    let id = state.document()?;
    let document = app.workspace.documents.get(&id).filter(|d| d.is_text())?;
    let editor = app.workspace.editor(pane)?;
    Some(Target {
        pane,
        view,
        input_generation: state.input_generation,
        document: id,
        path: document.meta.path.clone(),
        stamp: document.stamp(id),
        text: document.buffer.text_snapshot(),
        cursor: editor.cursor(),
        selection: editor.selection(),
    })
}

/// The indentation of `document` as a formatting request carries it: how wide one
/// indent level is and whether it is made of spaces. These describe the document;
/// what a formatter does with them is the extension's business.
pub(crate) fn indentation(app: &AppState, document: FileId) -> serde_json::Value {
    let settings = super::editing::effective(app, document);
    let (tab_size, insert_spaces) = if settings.use_tabs {
        (settings.tab_width, false)
    } else {
        (settings.indent_size, true)
    };
    serde_json::json!({ "tabSize": tab_size, "insertSpaces": insert_spaces })
}

/// The language services of the running application, if started.
pub(crate) fn services(
    state: &AppContext,
) -> Option<Arc<super::language_services::LanguageServices>> {
    state
        .services()
        .and_then(|runtime| runtime.language_services())
}

/// Runs after every state update: starts, follows or ends the features that
/// watch the document. It must stay cheap when nothing relevant happened.
pub(crate) fn observe(ctx: &AppContext, app: &mut AppState) {
    let owner = target(app, app.workspace.active_pane()).map(|t| (t.view, t.input_generation));
    let language = &mut app.editor.language;
    if language.interaction_view.is_some() && language.interaction_view != owner {
        completion::close_words(language);
        language.hover = Default::default();
        language.link = Default::default();
        language.highlights = Default::default();
        language.inlay = Default::default();
        language.expansion = Default::default();
        language.lightbulb = Default::default();
        language.quickfix = Default::default();
        language.completion = None;
        language.completion_timer = None;
        language.signature = None;
        language.signature_timer = None;
        language.signature_selection = None;
        language.snippet = None;
        if language.popup.as_ref().is_some_and(|p| !p.kind.is_panel()) {
            language.popup = None;
            language.docs_selection = None;
        }
    }
    language.interaction_view = owner;
    if let Some(panel) = app.editor.language.popup.as_mut()
        && panel.kind.is_panel()
        && panel.pane != app.workspace.active_pane()
    {
        panel.panel_focused = false;
    }
    if app.editor.language.popup.as_ref().is_some_and(|p| {
        p.kind.is_panel()
            && (app.workspace.pane(p.pane).is_none()
                || app
                    .workspace
                    .documents
                    .get(&p.stamp.document)
                    .is_none_or(|d| d.stamp(p.stamp.document) != p.stamp))
    }) {
        app.editor.language.popup = None;
    }
    let typed = app.editor.language.typed.take();
    hover::observe(app);
    link::observe(ctx, app);
    peek::observe(app);
    completion::observe(ctx, app, typed);
    signature::observe(ctx, app, typed);
    snippet_session::observe(app);
    highlight::observe(ctx, app);
    inlay::observe(ctx, app);
    lightbulb::observe(ctx, app);
    quickfix::observe(ctx, app);
    outline::observe(ctx, app);
    super::folding::observe(ctx, app);
}

/// Ends what Escape ends: the hover card, the signature, a grown selection.
/// Returns whether anything was open.
pub(crate) fn dismiss(app: &mut AppState) -> bool {
    let open = !app.editor.language.hover.is_idle()
        || app.editor.language.signature.is_some()
        || app.editor.language.popup.is_some()
        || app.editor.language.completion.is_some()
        || app.editor.language.snippet.is_some();
    let open = open || app.editor.language.peek.is_some();
    peek::close(app);
    hover::clear(app);
    signature::close(app);
    app.editor.language.close_popups();
    snippet_session::end(app);
    open
}

/// The signature card to draw in `pane`: where the call opened, and its text.
pub(crate) fn signature_view(
    app: &AppState,
    pane: PaneId,
) -> Option<(usize, crate::ui::editor::signature::View)> {
    let session = app
        .editor
        .language
        .signature
        .as_ref()
        .filter(|s| s.pane == pane)?;
    let view = signature::view(session)?;
    Some((
        view.anchor,
        crate::ui::editor::signature::View {
            label: view.label,
            active: view.active,
            docs: view.docs.map(Into::into),
            position: view.position,
            revision: session.revision,
        },
    ))
}

/// Whether the list over the code is the completion list.
pub(crate) fn completion_visible(app: &AppState) -> bool {
    app.editor
        .language
        .completion
        .as_ref()
        .is_some_and(|session| session.visible())
}

/// The list to draw over the code in `pane`, if any.
pub(crate) fn popup_view(
    app: &AppState,
    pane: PaneId,
    stamp: &DocumentStamp,
) -> Option<crate::ui::editor::popup::View> {
    if let Some(session) = app
        .editor
        .language
        .completion
        .as_ref()
        .filter(|s| s.pane == pane && s.visible())
    {
        return Some(completion::view(session));
    }
    app.editor
        .language
        .popup
        .as_ref()
        .filter(|popup| {
            popup.pane == pane
                && (popup.stamp == *stamp
                    || popup.kind.is_panel()
                        && app
                            .workspace
                            .documents
                            .get(&popup.stamp.document)
                            .is_some_and(|d| d.stamp(popup.stamp.document) == popup.stamp))
        })
        .map(popup::Popup::view)
}

/// The mouse did something in the list over the code.
pub(crate) fn popup_event(
    state: &AppContext,
    event: crate::ui::editor::popup::Event,
    rect: lgui::prelude::UiRect,
) {
    use crate::ui::editor::popup::Event;
    if let Event::OpenLink(url) = event {
        open_link(state, url);
        return;
    }
    if let Event::Select(text) = event {
        state.try_update(|app| {
            let selected = if completion_visible(app) {
                app.editor.language.completion.as_ref().map(|s| s.selected)
            } else {
                app.editor.language.popup.as_ref().map(|p| p.selected)
            };
            let next = selected.zip(text);
            let changed = app.editor.language.docs_selection != next;
            app.editor.language.docs_selection = next;
            changed
        });
        return;
    }
    let mut completing = false;
    state.try_update(|app| {
        completing = completion_visible(app);
        false
    });
    if completing {
        match event {
            Event::Select(_) | Event::OpenLink(_) => {}
            Event::Focus => {}
            Event::Hover(index) => {
                state.update(|app| {
                    if let Some(session) = app.editor.language.completion.as_mut() {
                        session.set_selected(index);
                    }
                });
                state.update(|app| completion::resolve_selected(state, app));
            }
            Event::Scroll(rows) => state.update(|app| {
                if let Some(session) = app.editor.language.completion.as_mut() {
                    session.scroll_by(rows);
                }
            }),
            Event::Pick(index) => state.update(|app| {
                if let Some(session) = app.editor.language.completion.as_mut() {
                    session.set_selected(index);
                }
                if completion::accept(app, None) {
                    crate::app::editor::reveal_cursor(app, rect);
                }
            }),
            Event::Dismiss => state.update(completion::close),
        }
        return;
    }
    state.update(|app| {
        if let Some(popup) = app.editor.language.popup.as_mut() {
            popup.resize(rect.height());
        }
    });
    match event {
        Event::Select(_) | Event::OpenLink(_) => {}
        Event::Focus => state.update(|app| {
            if let Some(popup) = app.editor.language.popup.as_mut() {
                popup.panel_focused = true;
            }
        }),
        Event::Hover(index) => {
            state.try_update(|app| {
                app.editor.language.popup.as_mut().is_some_and(|p| {
                    p.select(index);
                    true
                })
            });
        }
        Event::Scroll(rows) => {
            state.try_update(|app| {
                app.editor.language.popup.as_mut().is_some_and(|p| {
                    p.scroll_by(rows);
                    true
                })
            });
        }
        Event::Pick(index) => {
            state.update(|app| {
                if let Some(popup) = app.editor.language.popup.as_mut() {
                    popup.select(index);
                }
            });
            keys::choose(state, rect);
        }
        Event::Dismiss => state.update(|app| app.editor.language.close_popups()),
    }
}

/// Does what a popup row that is not a place stands for.
pub(crate) fn run_payload(
    state: &AppContext,
    payload: popup::Payload,
    rect: lgui::prelude::UiRect,
) {
    match payload {
        popup::Payload::Group(_) => {}
        popup::Payload::PeekPlace(place) => peek::open(state, place),
        popup::Payload::Action { instance, action } => {
            refactor::run_action(state, instance, action, rect)
        }
        popup::Payload::Command { instance, command } => {
            refactor::execute(state, instance, command, rect)
        }
        popup::Payload::Lens { instance, lens } => lists::resolve_lens(state, instance, lens),
        popup::Payload::Place(_) | popup::Payload::Hierarchy { .. } | popup::Payload::Nothing => {}
    }
}

/// Advances every feature that is waiting for a server's answer. Runs on the UI
/// thread whenever the servers report something new.
pub(crate) fn tick(state: &AppContext) {
    let mut effects = Vec::new();
    let mut changes = Vec::new();
    let mut proceed = None;
    state.try_update(|app| {
        app.editor.syntax_revision = app.editor.syntax_revision.wrapping_add(1);
        hover::tick(app);
        link::tick(app);
        completion::tick(app);
        signature::tick(app);
        highlight::tick(app);
        inlay::tick(app);
        super::folding::tick(app);
        lightbulb::tick(state, app);
        quickfix::tick(state, app);
        expand::tick(app);
        effects = actions::tick(app);
        changes = refactor::tick(app);
        lists::tick(state, app);
        outline::tick(state, app);
        proceed = save::tick(state, app);
        true
    });
    flush_files(state);
    if let Some(then) = proceed {
        then(state);
    }
    for effect in effects {
        match effect {
            actions::Effect::Go(place, rect) => navigate::go(state, place, rect, true),
            actions::Effect::Peek(place) => peek::open(state, place),
        }
    }
    for effect in changes {
        match effect {
            refactor::Effect::Apply(request) => edit::apply(state, request),
            refactor::Effect::ApplyThen {
                request,
                instance,
                command,
            } => edit::apply_then(state, request, instance, command),
            refactor::Effect::Execute {
                instance,
                command,
                rect,
            } => refactor::execute(state, instance, command, rect),
        }
    }
}

pub(super) fn flush_files(state: &AppContext) {
    if state.get().editor.language.file_changes.is_empty() {
        return;
    }
    let mut changes = Vec::new();
    state.update(|app| changes = std::mem::take(&mut app.editor.language.file_changes));
    if let Some(services) = services(state) {
        for change in changes {
            services.files_changed(change);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn indentation_of(configure: impl FnOnce(&mut AppState)) -> serde_json::Value {
        let mut app = AppState::new();
        configure(&mut app);
        let view = app
            .workspace
            .test_open_view(PathBuf::from("/w/a.txt"), "a".into());
        let id = app.workspace.view_document(view).unwrap();
        indentation(&app, id)
    }

    #[test]
    fn formatting_requests_report_the_documents_own_indentation() {
        let spaces = indentation_of(|app| {
            app.preferences.values_mut().editor.indent_size = 2;
            app.preferences.values_mut().editor.use_tabs = false;
        });
        assert_eq!(
            spaces,
            serde_json::json!({"tabSize": 2, "insertSpaces": true})
        );
        // With tabs the width of a tab is what a formatter needs.
        let tabs = indentation_of(|app| {
            app.preferences.values_mut().editor.tab_width = 8;
            app.preferences.values_mut().editor.use_tabs = true;
        });
        assert_eq!(
            tabs,
            serde_json::json!({"tabSize": 8, "insertSpaces": false})
        );
    }
}
