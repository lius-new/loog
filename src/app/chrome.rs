//! Chrome composition: views receive only their relevant frames and commands.
use crate::app::context::AppContext;
use crate::app::terminal_binding::TerminalStore;
use crate::ui::chrome::{HomeBinding, HomeCommand, HomeFrame, TitleBinding, TitleFrame};
use lgui::core::{UiFocusHandle, UiId};
use lgui::prelude::{Element, UiRect};
pub type TerminalToggle = (UiFocusHandle, UiFocusHandle, TerminalStore);

pub fn title_binding(state: &AppContext, terminal: Option<TerminalToggle>) -> TitleBinding {
    use crate::features::keymap::action::Action;
    use crate::ui::chrome::PanelToggle;
    let (reader, writer, extensions, updater, toggler, reviewer) = (
        state.clone(),
        state.clone(),
        state.clone(),
        state.clone(),
        state.clone(),
        state.clone(),
    );
    TitleBinding::new(
        move || {
            let app = reader.get();
            TitleFrame {
                settings_open: app.workspace.active_is_settings(),
                extensions_open: app.workspace.active_page()
                    == Some(crate::app::workspace::AppPage::Extensions),
                extensions_attention: crate::app::extensions_view::needs_attention(&reader),
                maximized: crate::app::window::is_maximized(),
                update: app.update.indicator(),
                restricted: reader
                    .services()
                    .and_then(|runtime| crate::app::trust_prompt::restricted(&app, runtime.trust()))
                    .map(|(count, label)| crate::ui::chrome::Restricted { count, label }),
                drawer_open: app.drawer.visible,
                source_control_open: app.source_control.visible,
                terminal_open: app.default_terminal_open(),
            }
        },
        move || writer.update(|app| app.apply(Action::OpenSettings)),
        move || {
            extensions.update(|app| app.apply(Action::OpenExtensions));
            crate::app::extensions_view::opened(&extensions);
        },
        move || crate::app::update::activate(&updater),
        move |panel| match panel {
            PanelToggle::Drawer => toggler.update(|app| app.apply(Action::ToggleDrawer)),
            PanelToggle::SourceControl => {
                toggler.update(|app| app.apply(Action::ToggleSourceControl))
            }
            PanelToggle::Terminal => {
                if let Some((editor, terminal, tabs)) = &terminal {
                    crate::app::terminal::toggle_panel(&toggler, tabs, editor, terminal);
                }
            }
        },
        move || crate::app::trust_prompt::review(&reviewer),
    )
}
pub fn title(
    rect: UiRect,
    state: AppContext,
    git: Element,
    terminal: Option<TerminalToggle>,
) -> Element {
    crate::ui::titlebar::render(rect, title_binding(&state, terminal), git)
}
pub fn home_binding(
    state: &AppContext,
    workspace: bool,
    editor: UiFocusHandle,
    terminal: Option<UiFocusHandle>,
    tabs: Option<TerminalStore>,
) -> HomeBinding {
    let (reader, writer) = (state.clone(), state.clone());
    HomeBinding::new(
        move || {
            let app = reader.get();
            HomeFrame {
                paths: if workspace {
                    app.explorer.model.roots().to_vec()
                } else {
                    app.explorer.model.recents().to_vec()
                },
                hover: if workspace {
                    app.chrome.workspace_home_hover
                } else {
                    app.chrome.welcome_hover
                },
            }
        },
        move |command| match command {
            HomeCommand::Hover(value) => writer.try_update(|app| {
                let hover = if workspace {
                    &mut app.chrome.workspace_home_hover
                } else {
                    &mut app.chrome.welcome_hover
                };
                let changed = *hover != value;
                *hover = value;
                changed
            }),
            HomeCommand::Focus(focused) => {
                writer.update(|app| {
                    app.editor.focused = focused;
                    if !focused {
                        app.editor.drag = None;
                        app.editor.ime = None;
                        if let Some(mut buffer) = app.workspace.active_editor_mut() {
                            buffer.break_undo_group();
                        }
                    }
                });
                true
            }
            HomeCommand::OpenFile => {
                let opened = crate::app::workspace_commands::choose_file(&writer);
                if opened && workspace {
                    editor.focus();
                }
                opened
            }
            HomeCommand::OpenFolder => {
                crate::app::explorer::choose_folder(&writer);
                true
            }
            HomeCommand::OpenRecent(path) => {
                crate::app::explorer::open_recent_folder(&writer, path);
                true
            }
            HomeCommand::ToggleTerminal => {
                if let (Some(tabs), Some(terminal)) = (&tabs, &terminal) {
                    crate::app::terminal::toggle_panel(&writer, tabs, &editor, terminal);
                    true
                } else {
                    false
                }
            }
            HomeCommand::OpenSourceControl => {
                writer.update(|app| {
                    app.source_control.visible = true;
                    app.chrome.workspace_home_hover = None;
                });
                true
            }
        },
    )
}
pub fn welcome(rect: UiRect, state: AppContext, id: UiId, editor: UiFocusHandle) -> Element {
    crate::ui::welcome::render(
        rect,
        home_binding(&state, false, editor.clone(), None, None),
        id,
        editor,
    )
}
pub fn workspace_home(
    rect: UiRect,
    state: AppContext,
    id: UiId,
    editor: UiFocusHandle,
    terminal: UiFocusHandle,
    tabs: TerminalStore,
) -> Element {
    crate::ui::workspace_home::render(
        rect,
        home_binding(&state, true, editor, Some(terminal), Some(tabs)),
        id,
    )
}

pub fn status_binding(state: &AppContext) -> crate::ui::chrome::StatusBinding {
    use crate::ui::chrome::{FileStatus, StatusBinding, StatusCommand, StatusFrame};
    let (reader, writer) = (state.clone(), state.clone());
    StatusBinding::new(
        move || {
            let app = reader.get();
            StatusFrame {
                extension_widgets: app.extension_widgets.roots_for(
                    app.workspace
                        .active()
                        .and_then(|view| app.workspace.view_meta(view))
                        .map(|meta| meta.path.display().to_string())
                        .as_deref(),
                ),
                pending: app.keymap.pending().to_vec(),
                language: syntax_notes(),
                language_services: crate::app::language_services_menu::status(&reader, &app),
                problems: {
                    let counts = crate::features::problems::counts();
                    Some(crate::ui::chrome::ProblemCounts {
                        errors: counts.errors,
                        warnings: counts.warnings,
                        infos: counts.infos,
                    })
                },
                vim: crate::app::editor_vim::is_active(&app)
                    .then(|| (app.editor.vim.mode(), app.editor.vim.status())),
                file: app.workspace.active().map(|id| FileStatus {
                    line_ending: app
                        .workspace
                        .active_editor()
                        .map(|buffer| buffer.line_ending_label().into())
                        .unwrap_or_default(),
                    indent: app
                        .workspace
                        .active_document()
                        .map(|document| {
                            let settings = super::editing::effective(&app, document);
                            if settings.use_tabs {
                                format!("Tabs: {}", settings.tab_width)
                            } else {
                                format!("Spaces: {}", settings.indent_size)
                            }
                        })
                        .unwrap_or_default(),
                    encoding: app
                        .workspace
                        .view_document(id)
                        .and_then(|id| app.workspace.documents.get(&id))
                        .map(|document| {
                            crate::ui::encoding_picker::encoding_label(document.format())
                        })
                        .unwrap_or_default(),
                    lang: app
                        .workspace
                        .view_meta(id)
                        .expect("active document metadata exists")
                        .lang
                        .clone(),
                    missing: app
                        .workspace
                        .view_document(id)
                        .is_some_and(|id| app.workspace.is_missing_on_disk(id)),
                    conflict: app
                        .workspace
                        .view_document(id)
                        .is_some_and(|id| app.workspace.has_disk_conflict(id)),
                    cursor: app
                        .workspace
                        .active_editor()
                        .map(|editor| editor.line_col()),
                    is_diff: app.workspace.is_diff(id),
                    is_file: app.workspace.is_file(id),
                }),
            }
        },
        move |command| match command {
            StatusCommand::ExtensionWidget { origin, payload } => {
                if let Some(host) = writer.services().and_then(|runtime| runtime.extensions()) {
                    host.ui_event(&writer.get(), origin, payload);
                }
            }
            StatusCommand::LineEnding => {
                writer.update(|app| super::editing::open(app, super::editing::Menu::Ending))
            }
            StatusCommand::Indent => {
                writer.update(|app| super::editing::open(app, super::editing::Menu::Indent))
            }
            StatusCommand::Encoding => crate::app::encoding::open(&writer),
            StatusCommand::HighlightMenu => {
                if writer.get().highlight_menu.visible {
                    writer.update(|app| app.highlight_menu.close());
                } else {
                    crate::app::highlight_menu::open(&writer);
                }
            }
            StatusCommand::Problems => writer
                .update(|app| app.toggle_log_panel(crate::ui::terminal::state::PanelTab::Problems)),
            StatusCommand::LanguageServices => {
                if writer.get().language_services_menu.visible {
                    writer.update(|app| app.language_services_menu.close());
                } else {
                    crate::app::language_services_menu::open(&writer);
                }
            }
        },
    )
}

fn syntax_notes() -> Vec<crate::ui::chrome::StatusNote> {
    use crate::ui::chrome::{NoteLevel, StatusNote};
    let mut notes = Vec::new();
    let left_out = crate::infrastructure::language_packs::resources().query_problem_count();
    if left_out > 0 {
        notes.push(StatusNote {
            text: format!(
                "{left_out} highlight {} left out",
                if left_out == 1 {
                    "query was"
                } else {
                    "queries were"
                }
            ),
            level: NoteLevel::Warning,
        });
    }
    notes
}

pub fn status(rect: UiRect, state: AppContext) -> Element {
    let app = state.get();
    if let Some(view) = app.workspace.active()
        && let Some(frame) = crate::app::image_preview::frame(&app, view)
    {
        return crate::ui::image_preview::status(
            rect,
            &frame,
            crate::app::image_preview::send(&state, view),
        );
    }
    crate::ui::statusbar::render(rect, status_binding(&state))
}
