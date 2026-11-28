//! Chrome composition: views receive only their relevant frames and commands.
use crate::app::context::AppContext;
use crate::app::terminal_binding::TerminalStore;
use crate::ui::chrome::{
    HomeBinding, HomeCommand, HomeFrame, PathSegment, Project, TitleBinding, TitleFrame,
};
use lgui::core::{UiFocusHandle, UiId};
use lgui::prelude::{Element, UiRect};

pub fn title_binding(state: &AppContext) -> TitleBinding {
    let (reader, writer, revealer, updater, toggler, reviewer) = (
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
            let (project, path) = title_context(&app);
            TitleFrame {
                maximized: crate::app::window::is_maximized(),
                update: app.update.indicator(),
                restricted: reader
                    .services()
                    .and_then(|runtime| crate::app::trust_prompt::restricted(&app, runtime.trust()))
                    .map(|(count, label)| crate::ui::chrome::Restricted { count, label }),
                layout_open: app.layout.visibility_popup.is_some(),
                project,
                path,
            }
        },
        move || {
            writer.update(|app| {
                crate::app::action_menu::toggle(app, crate::app::action_menu::Kind::Project)
            })
        },
        move |folder| crate::app::explorer::reveal_folder(&revealer, &folder),
        move || crate::app::update::activate(&updater),
        move || crate::app::layout_visibility::open(&toggler),
        move || crate::app::trust_prompt::review(&reviewer),
    )
}

/// The project the title bar names and the active file's path below it.
fn title_context(app: &crate::app::state::AppState) -> (Project, Vec<PathSegment>) {
    use std::path::Path;
    let file = app.workspace.active_path().map(Path::to_path_buf);
    let root = file
        .as_deref()
        .and_then(|path| app.workspace_folder_of(path))
        .map(Path::to_path_buf)
        .or_else(|| app.explorer.model.roots().first().cloned());
    let name = root
        .as_deref()
        .map(crate::app::action_menu::folder_name)
        .unwrap_or_default();
    let mut path = Vec::new();
    if let (Some(file), Some(root)) = (&file, &root)
        && let Ok(relative) = file.strip_prefix(root)
    {
        let parts: Vec<_> = relative.components().collect();
        let mut folder = root.clone();
        for (index, part) in parts.iter().enumerate() {
            let label = part.as_os_str().to_string_lossy().into_owned();
            if index + 1 == parts.len() {
                path.push(PathSegment {
                    label,
                    folder: None,
                });
            } else {
                folder.push(part);
                path.push(PathSegment {
                    label,
                    folder: Some(folder.clone()),
                });
            }
        }
    }
    let project = Project {
        initial: crate::app::action_menu::initial(&name),
        menu_open: app
            .chrome
            .menu
            .is_some_and(|open| open.kind == crate::app::action_menu::Kind::Project),
        name,
    };
    (project, path)
}
pub fn title(rect: UiRect, state: AppContext) -> Element {
    crate::ui::titlebar::render(rect, title_binding(&state))
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
    // The page is its own component and keeps hover as local state, so moving
    // over a row re-renders only this page instead of the whole application.
    let recents = state.get().explorer.model.recents().to_vec();
    lgui::prelude::component((rect, id, recents), move |cx, (rect, id, recents)| {
        // A signal, not state: moving over a row repaints that row in place instead of
        // re-rendering the page.
        let hover = cx.signal(None::<usize>);
        let app = home_binding(&state, false, editor.clone(), None, None);
        let (reader, runner, recents) = (hover.clone(), hover.clone(), recents.clone());
        let binding = HomeBinding::new(
            move || HomeFrame {
                paths: recents.clone(),
                hover: reader.get(),
            },
            move |command| match command {
                HomeCommand::Hover(value) => runner.try_update(|current| {
                    let changed = *current != value;
                    *current = value;
                    changed
                }),
                other => app.run(other),
            },
        )
        .with_hover_signal(hover);
        crate::ui::welcome::render(*rect, binding, id.clone(), editor.clone())
    })
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
                        .map(|meta| meta.context_key())
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
    status_with(rect, state, None::<fn((f32, f32)) -> Element>)
}

/// The status bar with `leading` at its left end: the manage button and the repository's
/// controls, from `ui::git::badge::status_leading`. It draws them from the origin it is
/// given, which is the bar's own origin where the bar docks them and the window's where it
/// does not.
pub fn status_with(
    rect: UiRect,
    state: AppContext,
    leading: Option<impl Fn((f32, f32)) -> Element>,
) -> Element {
    let app = state.get();
    if let Some(view) = app.workspace.active()
        && let Some(frame) = crate::app::image_preview::frame(&app, view)
    {
        let bar = crate::ui::image_preview::status(
            rect,
            &frame,
            crate::app::image_preview::send(&state, view),
        );
        return match leading {
            Some(leading) => {
                bar.child(leading((rect.left + crate::ui::git::badge::LEFT, rect.top)))
            }
            None => bar,
        };
    }
    crate::ui::statusbar::render_with(
        rect,
        status_binding(&state),
        leading.map(|leading| leading((0.0, 0.0))),
    )
}
