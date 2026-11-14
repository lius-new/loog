//! Compose the pane scene and application intents; the viewport never sees root state.
use crate::app::context::AppContext;
use lgui::prelude::{State, UiRect};
use std::sync::Arc;

use super::{editor, state::AppState};
use crate::domain::identity::PaneId;
use crate::features::git::GitStoreSnapshot;
use crate::ui::editor_binding::{Binding, DocumentMeta, Event, Preedit, ViewDocument, ViewState};

pub(crate) fn menu(
    viewport: UiRect,
    editor_rect: UiRect,
    pos: (f32, f32),
    state: AppContext,
    focus: lgui::core::UiFocusHandle,
) -> lgui::prelude::Element {
    use crate::ui::editor::menu::{self, Event, Item};
    use loog_editor_core::commands::Command;
    let app = state.get();
    let items = [
        ("Undo", Command::Undo),
        ("Redo", Command::Redo),
        ("Cut", Command::Cut),
        ("Copy", Command::Copy),
        ("Paste", Command::Paste),
        ("Select All", Command::SelectAll),
    ]
    .into_iter()
    .map(|(label, command)| Item {
        label,
        command,
        shortcut: crate::features::keymap::action::Action::for_editor_command(command)
            .and_then(|action| crate::app::key_actions::editor_shortcut_label(&app, action))
            .unwrap_or_default(),
        enabled: app
            .workspace
            .active_editor()
            .is_some_and(|buffer| match command {
                Command::Undo => buffer.can_undo(),
                Command::Redo => buffer.can_redo(),
                Command::Copy | Command::Cut => {
                    buffer.selections().iter().any(|s| s.range().is_some())
                        || matches!(
                            app.editor.vim.mode(),
                            loog_editor_core::vim::Mode::Visual(_)
                        )
                }
                _ => true,
            }),
    })
    .collect();
    menu::render(
        viewport,
        pos,
        items,
        app.editor.menu_hover,
        std::sync::Arc::new(move |event| match event {
            Event::Close => state.update(|app| app.editor.menu = None),
            Event::Hover(index) => {
                state.try_update(|app| {
                    let changed = app.editor.menu_hover != Some(index);
                    app.editor.menu_hover = Some(index);
                    changed
                });
            }
            Event::Execute(command, clipboard) => {
                editor::execute_command(&state, command, editor_rect, clipboard);
                state.update(|app| app.editor.menu = None);
            }
        }),
        focus,
    )
}

pub(crate) fn snapshot(
    app: AppState,
    git: GitStoreSnapshot,
    pane: PaneId,
    rect: UiRect,
) -> ViewState {
    let focused_pane = app.workspace.active_pane() == pane;
    let mut document = app
        .workspace
        .pane(pane)
        .and_then(|value| value.active())
        .and_then(|id| {
            let document = app.workspace.view_document(id)?;
            let meta = app.workspace.meta(document)?;
            let value = app.workspace.editor(pane)?;
            Some(ViewDocument {
                folds: Arc::from([]),
                id,
                stamp: app.workspace.documents.get(&document)?.stamp(document),
                meta: DocumentMeta {
                    resource: meta.resource.clone(),
                    lang: meta.lang.clone(),
                },
                projection: super::editing::projection(&app, id, rect),
                buffer: value.buffer().clone(),
                selection: value.selection_state(),
                secondary: value.secondary().to_vec(),
                scroll: app.workspace.scroll(pane),
            })
        });
    let has_repository = document.as_ref().is_some_and(|doc| {
        crate::features::git::location::git_path(&doc.meta.resource).is_some_and(|path| {
            git.repository_for_path(&path)
                .is_some_and(|value| value.relative_path(&path).is_some())
        })
    });
    let visible_lines = document
        .as_ref()
        .map(|doc| {
            let left = rect.left
                + crate::ui::fonts::editor().gutter_w_for_lines(doc.buffer.line_count())
                + crate::ui::theme::CODE_PAD;
            let metrics =
                crate::ui::editor::projection::metrics(rect, left, &doc.buffer, &doc.projection);
            let rows = crate::ui::editor::view::visible_line_range(
                doc.scroll.1.clamp(0.0, metrics.max_y),
                rect.height(),
                doc.projection.len(),
            );
            if rows.is_empty() {
                0..0
            } else {
                doc.projection.source_line(rows.start)..doc.projection.source_line(rows.end - 1) + 1
            }
        })
        .unwrap_or(0..0);
    if let Some(doc) = document.as_mut() {
        doc.folds = super::folding::markers(&app, doc.id, &visible_lines);
    }
    let state = app.editor;
    let vim_view = state.vim.is_enabled()
        && document.as_ref().is_some_and(|doc| {
            state.vim.view()
                == Some(loog_editor_core::vim::ViewKey {
                    pane,
                    document: doc.stamp.document,
                    view: doc.id,
                    input_generation: app.workspace.view(doc.id).map_or(0, |v| v.input_generation),
                })
        });
    let selections = document
        .as_ref()
        .map(|doc| {
            vim_view
                .then(|| state.vim.highlights(&doc.buffer, doc.selection))
                .flatten()
                .unwrap_or_else(|| {
                    doc.editor()
                        .selections()
                        .into_iter()
                        .filter_map(|s| s.range())
                        .collect()
                })
        })
        .unwrap_or_default();
    let search_matches = document
        .as_ref()
        .filter(|doc| {
            state.vim.is_enabled()
                && doc.buffer.len() <= 4 * 1024 * 1024
                && doc.buffer.max_line_display_width() <= 4096
        })
        .map(|doc| state.vim.search_matches(&doc.buffer, visible_lines.clone()))
        .unwrap_or_default();
    // Other places the selected text appears, tinted as the symbol under the
    // caret is; one selection only, and only the lines on screen.
    let selection_occurrences: Arc<[std::ops::Range<usize>]> = document
        .as_ref()
        .filter(|_| selections.len() == 1)
        .map(|doc| {
            doc.buffer
                .other_occurrences(selections[0].clone(), visible_lines.clone())
        })
        .unwrap_or_default()
        .into();
    let changes = document
        .as_ref()
        .and_then(|doc| git.line_changes.get(&crate::features::git::location::git_path(&doc.meta.resource)?));
    let line_changes = changes
        .map(|changes| {
            changes
                .range(visible_lines.start + 1..visible_lines.end + 1)
                .map(|(line, change)| (*line, *change))
                .collect()
        })
        .unwrap_or_default();
    let overview_marks = document
        .as_ref()
        .zip(changes)
        .filter(|(_, changes)| changes.len() <= 10_000)
        .map(|(doc, changes)| {
            crate::ui::editor::view::change_marks(changes, doc.buffer.line_count())
        })
        .unwrap_or_default();
    // Git read the saved file, so unsaved edits move the conflicts it found. A
    // saved edit changes the file before Git has read it again, so keep only
    // the conflicts whose markers are still where Git said.
    let conflicts = document
        .as_ref()
        .and_then(|doc| {
            let regions = git.conflicts.get(&crate::features::git::location::git_path(&doc.meta.resource)?)?;
            let saved = app.workspace.documents.get(&doc.stamp.document)?;
            let found = if saved.is_dirty() {
                crate::features::git::conflict::shifted(
                    regions,
                    &saved.saved_text,
                    &doc.buffer.text_snapshot(),
                )
            } else {
                regions.to_vec()
            };
            let lines = doc.buffer.line_count();
            Some(
                found
                    .into_iter()
                    .filter(|region| {
                        crate::features::git::conflict::markers_in_place(region, |n| {
                            (1..=lines).contains(&n).then(|| doc.buffer.line(n - 1))
                        })
                    })
                    .collect::<Vec<_>>()
                    .into(),
            )
        })
        .unwrap_or_default();
    let peek_visible = state.editor_hunk_peek.as_ref().is_some_and(|peek| {
        document.as_ref().is_some_and(|doc| {
            peek.pane == pane
                && crate::features::git::location::git_path(&doc.meta.resource).as_deref()
                    == Some(peek.path.as_path())
        })
    });
    ViewState {
        pane,
        document,
        focused_pane,
        focused: state.focused,
        accepts_text: !state.vim.is_enabled() || state.vim.accepts_text(),
        cursor_shape: if vim_view {
            state.vim.cursor_shape()
        } else {
            loog_editor_core::vim::CursorShape::Bar
        },
        selections,
        search_matches,
        diagnostics: Arc::from(Vec::new()),
        smooth_caret: app.preferences.values().smooth_caret,
        dragging: state.drag.is_some(),
        caret_dragging: state.caret_dragging,
        preedit: state
            .ime
            .filter(|value| {
                app.workspace.pane_of(value.document) == Some(pane)
                    && app.workspace.view(value.document).is_some_and(|v| {
                        v.accepts_text() && v.input_generation == value.input_generation
                    })
            })
            .map(|value| Preedit {
                pane,
                document: value.document,
                text: value.text,
                cursor: value.cursor,
            }),
        editor_gutter_hover: state.editor_gutter_hover,
        editor_diagnostic_hover: state.editor_diagnostic_hover.clone(),
        editor_hovered: state.editor_hovered,
        editor_vertical_scrollbar_dragging: state.editor_vertical_scrollbar_dragging,
        editor_horizontal_scrollbar_dragging: state.editor_horizontal_scrollbar_dragging,
        line_changes,
        overview_marks,
        conflicts,
        has_repository,
        peek_visible,
        hover: None,
        popup: None,
        snippet_choices: None,
        lightbulb: None,
        quickfix: None,
        language_peek: None,
        signature: None,
        symbol_highlights: selection_occurrences,
        link: None,
        inlay_hints: Arc::from(Vec::new()),
    }
}

pub(crate) fn binding(
    state: AppContext,
    git: State<GitStoreSnapshot>,
    pane: PaneId,
    rect: UiRect,
) -> Binding {
    let input_owner = state
        .get()
        .workspace
        .pane(pane)
        .and_then(|p| p.active())
        .and_then(|id| {
            state
                .get()
                .workspace
                .view(id)
                .map(|v| (id, v.input_generation))
        });
    let reader = state.clone();
    let read_git = git.clone();
    let dispatcher = state.clone();
    let overlay_git = git.clone();
    let syntax_state = state.clone();
    Binding::new(
        move || {
            let mut view = snapshot(reader.get(), read_git.get(), pane, rect);
            if let Some(document) = &view.document {
                view.hover = reader
                    .get()
                    .editor
                    .language
                    .hover
                    .card_for(pane, &document.stamp)
                    .map(|card| crate::ui::editor_binding::HoverView {
                        anchor: card.anchor.clone(),
                        docs: card.documentation.clone().into(),
                    });
            }
            if let Some(document) = &view.document {
                view.link =
                    super::language_ui::link::word_for(&reader.get(), pane, &document.stamp);
            }
            if let Some(document) = &view.document {
                let app = reader.get();
                view.popup = super::language_ui::popup_view(&app, pane, &document.stamp);
                view.snippet_choices = super::language_ui::snippet_session::choice_view(&app, pane);
                view.signature = super::language_ui::signature_view(&app, pane);
                view.lightbulb = super::language_ui::lightbulb::for_view(&reader, &app, pane);
                view.quickfix = super::language_ui::quickfix::view(&reader, &app, pane);
                view.language_peek = super::language_ui::peek::view(&app, pane);
                view.inlay_hints = app
                    .editor
                    .language
                    .inlay
                    .for_view(pane, &document.stamp)
                    .unwrap_or_else(|| Arc::from(Vec::new()));
                // The servers' tint only exists with no selection; a selection
                // keeps its own occurrences.
                if let Some(found) = app
                    .editor
                    .language
                    .highlights
                    .for_view(pane, &document.stamp)
                {
                    view.symbol_highlights = found;
                }
            }
            if let Some(document) = &view.document
                && let Some(services) = reader.services().and_then(|r| r.language_services())
                && let Some(found) = services
                    .results(&document.meta.resource, &document.stamp)
                    .diagnostics
            {
                view.diagnostics = found;
            }
            if let Some(document) = &view.document {
                if let Some(service) = reader.services().and_then(|runtime| runtime.markdown()) {
                    let generation = reader
                        .get()
                        .workspace
                        .view(document.id)
                        .map_or(0, |view| view.input_generation);
                    let mut slots = std::collections::HashSet::new();
                    if let Some(hover) = view.hover.as_mut() {
                        slots.insert(1);
                        hover.docs =
                            service.bind_card(document.id, generation, 1, hover.docs.clone());
                    }
                    if let Some(popup) = view.popup.as_mut() {
                        if let Some(docs) = popup.docs.take() {
                            slots.insert(2);
                            popup.docs = Some(service.bind_card(document.id, generation, 2, docs));
                        }
                    }
                    if let Some((_, signature)) = view.signature.as_mut() {
                        if let Some(docs) = signature.docs.take() {
                            slots.insert(3);
                            signature.docs =
                                Some(service.bind_card(document.id, generation, 3, docs));
                        }
                    }
                    service.retain_cards(document.id, &slots);
                }
            }
            view
        },
        move |event| {
            let current = dispatcher
                .get()
                .workspace
                .pane(pane)
                .and_then(|p| p.active())
                .and_then(|id| {
                    dispatcher
                        .get()
                        .workspace
                        .view(id)
                        .filter(|v| v.presentation == crate::domain::view::Presentation::Source)
                        .map(|v| (id, v.input_generation))
                });
            if current != input_owner || current.is_none() {
                return;
            }
            if let Event::Peek { line } = event {
                let app = dispatcher.get();
                if let Some(meta) = app
                    .workspace
                    .pane(pane)
                    .and_then(|value| value.active())
                    .and_then(|id| app.workspace.view_meta(id))
                {
                    let store = git.get();
                    if let Some(path) = crate::features::git::location::git_path(&meta.resource)
                        && let Some(repo) = store.repository_for_path(&path)
                        && let Some(relative) = repo.relative_path(&path)
                    {
                        super::git_peek::toggle(
                            &dispatcher,
                            pane,
                            path.clone(),
                            repo.worktree_root.clone(),
                            relative,
                            line,
                        );
                    }
                }
                return;
            }
            // A click on the word shown as a link goes to where it is defined.
            if let Event::PointerDown {
                point,
                button: lgui::core::PointerButton::Left,
                code_left,
                metrics,
                ..
            } = &event
            {
                let app = dispatcher.get();
                if let Some(offset) = hover_offset(&app, pane, rect, *code_left, *metrics, *point)
                    && let Some(goal) = super::language_ui::link::click_target(&app, offset)
                {
                    drop(app);
                    super::language_ui::link::follow(&dispatcher, goal, offset, rect);
                    return;
                }
            }
            if let Event::HoverMove {
                point,
                code_left,
                metrics,
            } = event
            {
                // Selecting in the card: moving off it must not take it away.
                if dispatcher.get().editor.language.hover.pressing {
                    return;
                }
                let mut armed = None;
                dispatcher.try_update(|app| {
                    let offset = hover_offset(app, pane, rect, code_left, metrics, point);
                    let had = !app.editor.language.hover.is_idle();
                    armed = super::language_ui::hover::pointer_over(app, pane, offset);
                    had != !app.editor.language.hover.is_idle() || armed.is_some()
                });
                if let Some(rest) = armed {
                    super::language_ui::hover::arm(&dispatcher, rest);
                }
                return;
            }
            if let Event::Popup(popup_event) = event {
                super::language_ui::popup_event(&dispatcher, popup_event, rect);
                return;
            }
            if let Event::SnippetChoice { revision, event } = event {
                dispatcher.try_update(|app| {
                    super::language_ui::snippet_session::choice_event(app, revision, event)
                });
                return;
            }
            if let Event::ResolveConflict { region, resolution } = event {
                super::git_conflict::resolve(&dispatcher, pane, rect, &region, resolution);
                return;
            }
            if let Event::CodeActions { revision } = event {
                dispatcher.try_update(|app| {
                    super::language_ui::lightbulb::open(&dispatcher, app, pane, revision)
                });
                return;
            }
            if let Event::QuickFix { revision, index } = event {
                super::language_ui::quickfix::choose(&dispatcher, pane, revision, index, rect);
                return;
            }
            if let Event::PeekOpen { revision } = event {
                super::language_ui::peek::jump(&dispatcher, revision, rect);
                return;
            }
            if let Event::DiagnosticLink(url) = event {
                let app = dispatcher.get();
                let valid = super::language_ui::target(&app, pane)
                    .and_then(|t| {
                        super::language_ui::services(&dispatcher)
                            .and_then(|s| s.results(&t.resource, &t.stamp).diagnostics)
                    })
                    .is_some_and(|ds| {
                        ds.iter()
                            .any(|d| d.documentation_url.as_deref() == Some(url.as_str()))
                    });
                if valid
                    && (url.starts_with("https://") || url.starts_with("http://"))
                    && !url.contains('\0')
                {
                    if let Err(error) = crate::infrastructure::update::open_page(&url) {
                        dispatcher.update(|app| {
                            app.show_error(format!(
                                "Could not open diagnostic documentation: {error}"
                            ))
                        });
                    }
                }
                return;
            }
            if let Event::HoverPress(pressing) = event {
                dispatcher.try_update(|app| {
                    let hover = &mut app.editor.language.hover;
                    let changed = hover.pressing != pressing;
                    hover.pressing = pressing;
                    changed
                });
                return;
            }
            if let Event::HoverSelect(text) = event {
                dispatcher.try_update(|app| {
                    let hover = &mut app.editor.language.hover;
                    let changed = hover.selection != text;
                    hover.selection = text;
                    changed
                });
                return;
            }
            if let Event::SignatureSelect { revision, text } = event {
                dispatcher.try_update(|app| {
                    let next = text.map(|text| (revision, text));
                    let changed = app.editor.language.signature_selection != next;
                    app.editor.language.signature_selection = next;
                    changed
                });
                return;
            }
            if let Event::OpenLink(url) = event {
                super::language_ui::open_link(&dispatcher, url);
                return;
            }
            if let Event::PeekClose { revision } = event {
                dispatcher.update(|app| {
                    if super::language_ui::peek::view(app, pane)
                        .is_some_and(|v| v.revision == revision)
                    {
                        super::language_ui::peek::close(app);
                    }
                });
                return;
            }
            if let Event::Text(text) = &event
                && super::language_ui::keys::takes_text(&dispatcher.get())
            {
                super::language_ui::keys::type_text(&dispatcher, text, rect);
                return;
            }
            if matches!(event, Event::HoverLeave) {
                if dispatcher.get().editor.language.hover.pressing {
                    return;
                }
                dispatcher.try_update(super::language_ui::hover::clear);
                return;
            }
            dispatcher.try_update(|app| dispatch(app, pane, rect, event));
        },
        {
            let git = overlay_git;
            // Git loading and navigation stay in application composition.
            move |placement| {
                let app = state.get();
                let meta = app
                    .workspace
                    .pane(pane)
                    .and_then(|value| value.active())
                    .and_then(|id| app.workspace.view_meta(id))?;
                let path = &crate::features::git::location::git_path(&meta.resource)?;
                let peek = app
                    .editor
                    .editor_hunk_peek
                    .as_ref()
                    .filter(|peek| peek.pane == pane && peek.path == *path)?;
                let store = git.get();
                let repo = store.repository_for_path(path)?;
                let relative = repo.relative_path(path)?;
                Some(crate::ui::git::hunk_peek::render(
                    peek,
                    crate::ui::git::hunk_peek::PeekContext {
                        pane,
                        rect: placement.rect,
                        code_left: placement.code_left,
                        code_right: placement.rect.right - crate::ui::editor::view::SCROLLBAR_SIZE,
                        scroll_y: placement.scroll_y,
                        max_y: placement.metrics.max_y,
                        viewport_h: placement.metrics.viewport_h,
                        path,
                        root: &repo.worktree_root,
                        relative: &relative,
                        changes: store.line_changes.get(path),
                    },
                    &super::git_view::binding(&state, &git),
                ))
            }
        },
    )
    .with_highlights(move |document, windows| {
        syntax_state
            .services()
            .and_then(|runtime| runtime.highlighter())
            .map_or_else(Vec::new, |highlighter| {
                let mut tokens = highlighter.worker.query(
                    pane,
                    document.stamp,
                    crate::features::syntax::Source::new(&document.buffer),
                    document.meta.lang.clone(),
                    windows,
                );
                // The server's semantic tokens, when they describe exactly this text,
                // are drawn over the syntax tree's classes.
                if syntax_state
                    .get()
                    .preferences
                    .values()
                    .language_features
                    .semantic_highlighting
                    == "combined"
                    && let Some(semantic) = syntax_state
                        .services()
                        .and_then(|runtime| runtime.language_services())
                        .and_then(|services| {
                            services
                                .results(&document.meta.resource, &document.stamp)
                                .semantic
                        })
                        .filter(|spans| !spans.is_empty())
                {
                    for window in &mut tokens {
                        let composed = crate::features::language_service::semantic::compose(
                            &window.spans,
                            &semantic,
                            window.range.clone(),
                        );
                        window.spans = composed.into();
                    }
                }
                tokens
            })
    })
}

pub(crate) fn dispatch(app: &mut AppState, pane: PaneId, rect: UiRect, event: Event) -> bool {
    match event {
        Event::Fold { view, stamp, start } => {
            return super::folding::toggle(app, view, stamp, start, rect);
        }
        Event::Signature { revision, forward } => {
            return super::language_ui::signature::step(app, forward, Some(revision));
        }
        Event::PointerDown {
            document,
            point,
            button,
            code_left,
            metrics,
        } => {
            if app.workspace.pane(pane).and_then(|value| value.active()) != Some(document) {
                return false;
            }
            if let Some(popup) = app
                .editor
                .language
                .popup
                .as_mut()
                .filter(|p| p.kind == super::language_ui::popup::Kind::References)
            {
                popup.panel_focused = false;
            }
            editor::pointer_down(app, pane, document, rect, code_left, metrics, point, button);
        }
        Event::PointerDrag(point) => {
            if let Some(drag) = app.editor.drag.as_mut() {
                app.editor.caret_dragging = true;
                drag.point = point;
            }
            editor::update_drag_selection(app, rect);
        }
        Event::PointerUp => app.editor.drag = None,
        Event::Wheel { x, y, metrics } => {
            // The card belongs to where the pointer was; scrolling moves the text.
            app.editor.editor_diagnostic_hover = None;
            super::language_ui::hover::clear(app);
            let (x, y) = if app.editor.modifiers.shift() {
                (x + y, 0.0)
            } else {
                (x, y)
            };
            let (sx, sy) = app.workspace.scroll(pane);
            app.workspace.set_scroll(
                pane,
                (sx - x).clamp(0.0, metrics.max_x),
                (sy - y).clamp(0.0, metrics.max_y),
            );
        }
        Event::GutterHover(hovered) => {
            let changed = app.editor.editor_gutter_hover != hovered;
            app.editor.editor_gutter_hover = hovered;
            return changed;
        }
        Event::DiagnosticHover(hovered) => {
            // A selection started in the card is finished first.
            if app.editor.language.hover.pressing {
                return false;
            }
            let changed = app.editor.editor_diagnostic_hover != hovered;
            if changed {
                app.editor.language.hover.selection = None;
            }
            app.editor.editor_diagnostic_hover = hovered;
            return changed;
        }
        Event::Text(text) => {
            app.editor.editor_diagnostic_hover = None;
            super::language_ui::hover::clear(app);
            editor::insert_text(app, &text, rect)
        }
        Event::Composition { text, cursor } => editor::update_composition(app, &text, cursor),
        Event::CompositionEnd => app.editor.ime = None,
        Event::Focus => app.editor.focused = true,
        Event::Blur => {
            app.editor.focused = false;
            app.editor.drag = None;
            app.editor.ime = None;
            if let Some(mut value) = app.workspace.active_editor_mut() {
                value.break_undo_group();
            }
        }
        Event::ScrollbarDown {
            vertical,
            offset,
            position,
        } => {
            app.workspace.activate_pane(pane);
            if vertical {
                app.editor.editor_vertical_scrollbar_dragging = true;
                app.editor.editor_vertical_scrollbar_drag_offset = offset;
            } else {
                app.editor.editor_horizontal_scrollbar_dragging = true;
                app.editor.editor_horizontal_scrollbar_drag_offset = offset;
            }
            if let Some(value) = position {
                let (x, y) = app.workspace.scroll(pane);
                app.workspace.set_scroll(
                    pane,
                    if vertical { x } else { value },
                    if vertical { value } else { y },
                );
            }
        }
        Event::ScrollbarDrag {
            vertical,
            pointer,
            start,
            travel,
            max,
        } => {
            let offset = if vertical {
                app.editor.editor_vertical_scrollbar_drag_offset
            } else {
                app.editor.editor_horizontal_scrollbar_drag_offset
            };
            let value = crate::ui::editor::view::scroll_from_thumb(
                (pointer - start - offset).clamp(0.0, travel),
                travel,
                max,
            );
            let (x, y) = app.workspace.scroll(pane);
            app.workspace.set_scroll(
                pane,
                if vertical { x } else { value },
                if vertical { value } else { y },
            );
        }
        Event::ScrollbarUp { vertical } => {
            if vertical {
                app.editor.editor_vertical_scrollbar_dragging = false;
            } else {
                app.editor.editor_horizontal_scrollbar_dragging = false;
            }
        }
        Event::Peek { .. }
        | Event::HoverMove { .. }
        | Event::HoverLeave
        | Event::Popup(_)
        | Event::SnippetChoice { .. }
        | Event::CodeActions { .. }
        | Event::ResolveConflict { .. }
        | Event::QuickFix { .. }
        | Event::PeekOpen { .. }
        | Event::PeekClose { .. }
        | Event::DiagnosticLink(_)
        | Event::OpenLink(_)
        | Event::HoverSelect(_)
        | Event::SignatureSelect { .. }
        | Event::HoverPress(_) => {
            return false;
        }
    }
    true
}

/// The text offset under the pointer, if it is over the text of a line and not
/// the empty space beyond its end.
fn hover_offset(
    app: &AppState,
    pane: PaneId,
    rect: UiRect,
    code_left: f32,
    metrics: crate::ui::editor::view::ScrollMetrics,
    point: (f32, f32),
) -> Option<usize> {
    use crate::ui::editor::projection as projected;
    if point.0 < code_left {
        return None;
    }
    let (sx, sy) = app.workspace.scroll(pane);
    let buffer = app.workspace.editor(pane)?;
    let view = app.workspace.pane(pane)?.active()?;
    let map = super::editing::projection(app, view, rect);
    let (offset, _) = projected::hit(
        &buffer, &map, rect, code_left, metrics, sx, sy, point.0, point.1,
    );
    let row = projected::point_row(&map, rect, sy, point.1);
    let end = map.row(&buffer, row).range.end;
    if offset >= end {
        let edge = projected::caret(
            &buffer,
            &map,
            loog_editor_core::text::Selection {
                cursor: end,
                wrap_affinity: loog_editor_core::text::Assoc::Before,
                ..Default::default()
            },
            code_left,
            rect.top,
            sx,
            metrics.viewport_w,
        )
        .0 - sx;
        if point.0 > edge + 1.0 {
            return None;
        }
    }
    Some(offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::line_change::LineChange;
    use crate::model::pane_layout::Direction;
    use lgui::core::PointerButton;
    use std::collections::BTreeMap;

    const RECT: UiRect = UiRect {
        left: 0.0,
        top: 0.0,
        right: 500.0,
        bottom: 120.0,
    };
    fn document(contents: &str) -> AppState {
        let mut app = AppState::new();
        app.workspace.open_path("first.txt".into(), contents.into());
        app
    }

    #[test]
    fn inactive_pane_snapshot_keeps_its_own_selection_scroll_and_ime() {
        let mut app = document(&"some text\n".repeat(120));
        let left = app.workspace.active_pane();
        let id = app.workspace.active().unwrap();
        app.workspace.editor_mut(left).unwrap().select_range(0..3);
        app.workspace.set_scroll(left, 17.0, 48.0);
        let right = app.workspace.split(left, Direction::Right).unwrap();
        let right_view = app.workspace.active().unwrap();
        app.workspace.editor_mut(right).unwrap().select_range(5..9);
        app.workspace.set_scroll(right, 30.0, 200.0);
        dispatch(
            &mut app,
            right,
            RECT,
            Event::Composition {
                text: "ni".into(),
                cursor: Some(0..0),
            },
        );
        let scene = snapshot(app.clone(), GitStoreSnapshot::default(), left, RECT);
        assert!(!scene.focused_pane);
        let doc = scene.document.as_ref().unwrap();
        assert_eq!(doc.id, id);
        assert_eq!(doc.editor().selection(), Some(0..3));
        assert_eq!(doc.scroll, (17.0, 48.0));
        assert!(scene.preedit_for(left, id).is_none());
        let scene = snapshot(app, GitStoreSnapshot::default(), right, RECT);
        assert!(scene.focused_pane);
        assert_eq!(
            scene.document.as_ref().unwrap().editor().selection(),
            Some(5..9)
        );
        assert!(scene.preedit_for(right, right_view).is_some());
    }

    #[test]
    fn snapshot_bounds_gutter_changes_but_keeps_the_full_overview() {
        let app = document(&"line\n".repeat(400));
        let pane = app.workspace.active_pane();
        let git = GitStoreSnapshot {
            line_changes: BTreeMap::from([
                (
                    "first.txt".into(),
                    (1..=400).map(|line| (line, LineChange::Added)).collect(),
                ),
                (
                    "unrelated.txt".into(),
                    BTreeMap::from([(1, LineChange::Deleted)]),
                ),
            ])
            .into(),
            ..Default::default()
        };
        let scene = snapshot(app, git, pane, RECT);
        assert!(!scene.line_changes.is_empty());
        assert!(scene.line_changes.len() < 20);
        assert!(
            scene
                .line_changes
                .values()
                .all(|value| *value == LineChange::Added)
        );
        assert_eq!(scene.overview_marks.len(), 1);
        assert_eq!(scene.overview_marks[0].lines, 400);
    }

    #[test]
    fn snapshot_moves_git_conflicts_for_unsaved_edits() {
        use crate::features::git::conflict::ConflictRegion;
        let mut app = document(
            "a
<<<<<<< x
L
=======
R
>>>>>>> y
",
        );
        let pane = app.workspace.active_pane();
        let region = ConflictRegion {
            start: 2,
            current: 3..4,
            separator: 4,
            incoming: 5..6,
            end: 6,
        };
        let git = || GitStoreSnapshot {
            conflicts: BTreeMap::from([(
                "first.txt".into(),
                std::sync::Arc::from(vec![region.clone()]),
            )])
            .into(),
            ..Default::default()
        };
        let saved = snapshot(app.clone(), git(), pane, RECT);
        assert_eq!(saved.conflicts.as_ref(), std::slice::from_ref(&region));

        // A line typed above moves the conflict down; one typed inside drops it.
        app.workspace
            .editor_mut(pane)
            .unwrap()
            .replace(
                0..0,
                "new
",
                loog_editor_core::text::EditKind::Other,
            )
            .unwrap();
        let above = snapshot(app.clone(), git(), pane, RECT);
        assert_eq!(above.conflicts[0].start, 3);
        let inside = app.workspace.editor(pane).unwrap().buffer().line_start(4);
        app.workspace
            .editor_mut(pane)
            .unwrap()
            .replace(inside..inside, "x", loog_editor_core::text::EditKind::Other)
            .unwrap();
        assert!(snapshot(app, git(), pane, RECT).conflicts.is_empty());
    }

    #[test]
    fn a_saved_resolution_hides_the_conflict_before_git_has_looked_again() {
        use crate::features::git::conflict::ConflictRegion;
        let mut app = document("x\n<<<<<<< a\nL\n=======\nR\n>>>>>>> b\nz\n");
        let pane = app.workspace.active_pane();
        let id = app.workspace.active().unwrap();
        let git = || GitStoreSnapshot {
            conflicts: BTreeMap::from([(
                "first.txt".into(),
                std::sync::Arc::from(vec![ConflictRegion {
                    start: 2,
                    current: 3..4,
                    separator: 4,
                    incoming: 5..6,
                    end: 6,
                }]),
            )])
            .into(),
            ..Default::default()
        };
        assert_eq!(snapshot(app.clone(), git(), pane, RECT).conflicts.len(), 1);

        // Accepting the current side and saving: the text matches the disk
        // again, but the snapshot still holds the positions Git found before.
        let resolved = "x\nL\nz\n";
        let length = app.workspace.editor(pane).unwrap().buffer().len();
        app.workspace
            .editor_mut(pane)
            .unwrap()
            .replace(0..length, resolved, loog_editor_core::text::EditKind::Other)
            .unwrap();
        let document = app.workspace.view_document(id).unwrap();
        let saved = app.workspace.editor(pane).unwrap().buffer().text_snapshot();
        app.workspace
            .documents
            .get_mut(&document)
            .unwrap()
            .saved_text = saved;
        assert!(snapshot(app, git(), pane, RECT).conflicts.is_empty());
    }

    #[test]
    fn a_pointer_event_from_a_replaced_document_cannot_move_the_new_selection() {
        let mut app = document("first");
        let pane = app.workspace.active_pane();
        let old = app.workspace.active().unwrap();
        app.workspace
            .open_path("second.txt".into(), "second".into());
        app.workspace
            .active_editor_mut()
            .unwrap()
            .select_range(1..4);
        let before = app.workspace.active_editor().unwrap().selection_state();
        let metrics = crate::ui::editor::view::scroll_metrics(
            RECT,
            80.0,
            app.workspace.active_editor().unwrap().buffer(),
        );
        assert!(!dispatch(
            &mut app,
            pane,
            RECT,
            Event::PointerDown {
                document: old,
                point: (400.0, 100.0),
                button: PointerButton::Left,
                code_left: 80.0,
                metrics,
            }
        ));
        assert_eq!(
            app.workspace.active_editor().unwrap().selection_state(),
            before
        );
        assert!(app.editor.drag.is_none());
    }

    #[test]
    fn a_late_ime_event_commits_to_its_original_pane_and_undo_history() {
        let mut app = document("first");
        let left = app.workspace.active_pane();
        dispatch(
            &mut app,
            left,
            RECT,
            Event::Composition {
                text: "ni".into(),
                cursor: None,
            },
        );
        let right = app.workspace.split(left, Direction::Right).unwrap();
        app.workspace
            .open_path("second.txt".into(), "second".into());
        dispatch(&mut app, right, RECT, Event::Text("你".into()));
        assert_eq!(app.workspace.active_pane(), right);
        assert_eq!(app.workspace.editor(right).unwrap().text(), "second");
        assert_eq!(app.workspace.editor(left).unwrap().text(), "你first");
        assert!(app.editor.ime.is_none());
        app.workspace.editor_mut(left).unwrap().undo();
        assert_eq!(app.workspace.editor(left).unwrap().text(), "first");
        assert_eq!(app.workspace.editor(right).unwrap().text(), "second");
    }

    #[test]
    fn blur_cancels_transients_and_separates_typing_undo_groups() {
        let mut app = document("");
        let pane = app.workspace.active_pane();
        dispatch(&mut app, pane, RECT, Event::Text("ab".into()));
        dispatch(
            &mut app,
            pane,
            RECT,
            Event::Composition {
                text: "ni".into(),
                cursor: None,
            },
        );
        dispatch(&mut app, pane, RECT, Event::Blur);
        assert!(!app.editor.focused);
        assert!(app.editor.ime.is_none());
        dispatch(&mut app, pane, RECT, Event::Focus);
        dispatch(&mut app, pane, RECT, Event::Text("c".into()));
        app.workspace.active_editor_mut().unwrap().undo();
        assert_eq!(app.workspace.active_editor().unwrap().text(), "ab");
        app.workspace.active_editor_mut().unwrap().undo();
        assert_eq!(app.workspace.active_editor().unwrap().text(), "");
    }
}
