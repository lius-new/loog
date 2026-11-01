//! Chrome composition: views receive only their relevant frames and commands.
use crate::app::context::AppContext;
use crate::app::terminal_binding::TerminalStore;
use crate::ui::chrome::{HomeBinding, HomeCommand, HomeFrame, TitleBinding, TitleFrame};
use lgui::core::{UiFocusHandle, UiId};
use lgui::prelude::{Element, UiRect};
/// What the terminal toggle needs: the editor and terminal focus and the tabs.
pub type TerminalToggle = (UiFocusHandle, UiFocusHandle, TerminalStore);
pub fn title_binding(state: &AppContext, terminal: Option<TerminalToggle>) -> TitleBinding {
    use crate::features::keymap::action::Action;
    use crate::ui::chrome::PanelToggle;
    let reader = state.clone();
    let writer = state.clone();
    let extensions = state.clone();
    let updater = state.clone();
    let toggler = state.clone();
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
                drawer_open: app.drawer.visible,
                source_control_open: app.source_control.visible,
                terminal_open: app.default_terminal_open(),
            }
        },
        move || writer.update(|app| app.apply(Action::OpenSettings)),
        move || extensions.update(|app| app.apply(Action::OpenExtensions)),
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
    let reader = state.clone();
    let writer = state.clone();
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
    let reader = state.clone();
    let writer = state.clone();
    StatusBinding::new(
        move || {
            let app = reader.get();
            let (server, language) = language_status(&reader, &app);
            StatusFrame {
                pending: app.keymap.pending().to_vec(),
                server,
                language,
                vim: crate::app::editor_vim::is_active(&app)
                    .then(|| (app.editor.vim.mode(), app.editor.vim.status())),
                file: app.workspace.active().map(|id| FileStatus {
                    line_ending: app
                        .workspace
                        .active_editor()
                        .map(|b| b.line_ending_label().into())
                        .unwrap_or_default(),
                    indent: app
                        .workspace
                        .active_document()
                        .map(|doc| {
                            let settings = super::editing::effective(&app, doc);
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
                        .map(|d| crate::ui::encoding_picker::encoding_label(d.format()))
                        .unwrap_or_default(),
                    lang: app
                        .workspace
                        .view_meta(id)
                        .expect("active document metadata exists")
                        .lang,
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
            StatusCommand::LineEnding => {
                writer.update(|app| super::editing::open(app, super::editing::Menu::Ending))
            }
            StatusCommand::Indent => {
                writer.update(|app| super::editing::open(app, super::editing::Menu::Indent))
            }
            StatusCommand::Encoding => crate::app::encoding::open(&writer),
            StatusCommand::ServerAction(crate::ui::chrome::ChipAction::Details(id)) => writer
                .update(|app| {
                    app.chrome.language_panel = false;
                    app.chrome.language_details = match app.chrome.language_details {
                        Some(open) if open == id => None,
                        _ => Some(id),
                    };
                }),
            StatusCommand::ServerAction(action) => {
                use crate::ui::chrome::ChipAction;
                if let Some(services) = writer.services().and_then(|r| r.language_services()) {
                    match action {
                        ChipAction::Details(_) | ChipAction::Pending => {}
                        ChipAction::Restart(instance) => services.restart_instance(instance),
                    }
                }
            }
            StatusCommand::LanguagePanel => writer.update(|app| {
                app.chrome.language_panel = !app.chrome.language_panel;
                app.chrome.language_details = None;
            }),
        },
    )
}
use crate::app::extensions::host::Suggestion;

/// What the registry can do for the active file, when Loom has no syntax for it.
fn syntax_suggestion(state: &AppContext, app: &crate::app::state::AppState) -> Option<Suggestion> {
    let id = app.workspace.active_document()?;
    let document = app.workspace.documents.get(&id).filter(|d| d.is_text())?;
    if document.meta.lang.grammar.is_some() {
        return None;
    }
    let name = document
        .meta
        .path
        .file_name()?
        .to_string_lossy()
        .into_owned();
    state
        .services()
        .and_then(|runtime| runtime.extensions())?
        .suggest_for(&name)
}

/// Pending syntax resources and the active file's main language server.
fn language_status(
    state: &AppContext,
    app: &crate::app::state::AppState,
) -> (
    Option<crate::ui::chrome::ServerChip>,
    Vec<crate::ui::chrome::StatusNote>,
) {
    use crate::ui::chrome::{NoteLevel, ServerChip, StatusNote};
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
    let Some(id) = app.workspace.active_document() else {
        return (None, notes);
    };
    // A file with no syntax of its own, for which the registry has an extension.
    match syntax_suggestion(state, app) {
        Some(Suggestion::Install { name, .. }) => notes.push(StatusNote {
            text: format!("{name} syntax is available"),
            level: NoteLevel::Quiet,
        }),
        Some(Suggestion::Restart { name }) => notes.push(StatusNote {
            text: format!("Restart Loom to use {name} syntax"),
            level: NoteLevel::Quiet,
        }),
        None => {}
    }
    let Some(services) = state.services().and_then(|r| r.language_services()) else {
        return (None, notes);
    };
    let Some(document) = app.workspace.documents.get(&id).filter(|d| d.is_text()) else {
        return (None, notes);
    };
    let registry = crate::domain::language::Registry::builtin();
    let servers = crate::features::language_service::server_set::ServerSet::current();
    let infos = services.infos();
    // The language's main server is the chip; the others only add to its count, and
    // the panel lists them.
    let mut present = Vec::new();
    for (definition, _) in servers.for_language(registry, document.meta.lang) {
        let name = definition.display_name.to_string();
        let info = infos
            .iter()
            .filter(|i| i.server == name && document.meta.path.starts_with(&i.root))
            .max_by_key(|i| i.root.components().count());
        if let Some(info) = info {
            present.push(server_chip(&name, info));
        }
    }
    let chip = (!present.is_empty()).then(|| {
        let others = present.len() - 1;
        let (tone, text, progress, action) = present.remove(0);
        ServerChip {
            text: if others > 0 {
                format!("{text} +{others}")
            } else {
                text
            },
            tone,
            progress,
            action,
        }
    });
    (chip, notes)
}

/// The state, words and progress the status bar shows for one server instance.
fn server_chip(
    name: &str,
    info: &crate::infrastructure::lsp::manager::InstanceInfo,
) -> (
    crate::ui::language_panel::Tone,
    String,
    crate::ui::language_panel::Progress,
    Option<crate::ui::chrome::ChipAction>,
) {
    use crate::infrastructure::lsp::manager::Status;
    use crate::ui::chrome::ChipAction;
    use crate::ui::language_panel::{Progress, Tone};
    match &info.status {
        Status::Starting => (
            Tone::Busy,
            format!("{name} starting"),
            Progress::None,
            Some(ChipAction::Pending),
        ),
        Status::Ready => match info.tasks.first() {
            // The server is connected but still working: say what on.
            Some(task) => (
                Tone::Busy,
                match task.percentage {
                    Some(percent) => format!("{name}: {} {percent}%", task.title),
                    None => format!("{name}: {}", task.title),
                },
                Progress::Spinner,
                Some(ChipAction::Details(info.id)),
            ),
            None => (
                Tone::Ready,
                name.to_owned(),
                Progress::None,
                Some(ChipAction::Details(info.id)),
            ),
        },
        Status::Restarting { .. } => (
            Tone::Failed,
            format!("{name} restarting"),
            Progress::Spinner,
            Some(ChipAction::Restart(info.id)),
        ),
        Status::Failed(_) => (
            Tone::Failed,
            format!("{name} stopped"),
            Progress::None,
            Some(ChipAction::Restart(info.id)),
        ),
        Status::Unavailable(_) => (
            Tone::Off,
            format!("{name} not found"),
            Progress::None,
            Some(ChipAction::Restart(info.id)),
        ),
    }
}

/// The open details card's server, as the card shows it.
pub fn language_details_frame(state: &AppContext) -> Option<crate::ui::language_details::Frame> {
    use crate::features::language_service::diagnostics::Severity;
    use crate::infrastructure::lsp::manager::Status;
    use crate::ui::language_details::Frame;
    use crate::ui::language_panel::{Level, Tone};
    let services = state.services().and_then(|r| r.language_services())?;
    let app = state.get();
    let open = app.chrome.language_details?;
    let info = services.infos().into_iter().find(|info| info.id == open)?;
    let (tone, word) = match &info.status {
        Status::Starting => (Tone::Busy, "Starting".to_owned()),
        Status::Ready if info.tasks.is_empty() => (Tone::Ready, "Ready".to_owned()),
        Status::Ready => (Tone::Busy, "Working".to_owned()),
        Status::Restarting { attempt, .. } => (Tone::Failed, format!("Restarting ({attempt})")),
        Status::Failed(_) => (Tone::Failed, "Stopped".to_owned()),
        Status::Unavailable(_) => (Tone::Off, "Unavailable".to_owned()),
    };
    let provides = match &info.providers {
        Some(providers) => {
            let names = providers.names();
            if names.is_empty() {
                "diagnostics only".to_owned()
            } else {
                names.join(", ")
            }
        }
        None => "—".to_owned(),
    };
    // The active file's errors and warnings: the counts the status bar leaves out.
    let diagnostics = app
        .workspace
        .active_document()
        .and_then(|id| {
            let document = app.workspace.documents.get(&id).filter(|d| d.is_text())?;
            services
                .results(&document.meta.path, &document.stamp(id))
                .diagnostics
        })
        .map(|found| {
            let count = |severity| found.iter().filter(|d| d.severity == severity).count();
            let (errors, warnings) = (count(Severity::Error), count(Severity::Warning));
            if errors + warnings == 0 {
                "none".to_owned()
            } else {
                format!(
                    "{errors} error{}, {warnings} warning{}",
                    if errors == 1 { "" } else { "s" },
                    if warnings == 1 { "" } else { "s" }
                )
            }
        })
        .unwrap_or_else(|| "—".to_owned());
    let rows = vec![
        ("Provides".to_owned(), provides),
        ("Root".to_owned(), crate::ui::display_path(&info.root)),
        (
            "Documents".to_owned(),
            match info.process_id {
                Some(pid) => format!("{} · pid {pid}", info.documents),
                None => info.documents.to_string(),
            },
        ),
        ("Diagnostics".to_owned(), diagnostics),
    ];
    let mut log: Vec<(String, Level)> = info
        .log
        .iter()
        .rev()
        .take(6)
        .rev()
        .map(|line| (line.clone(), Level::Quiet))
        .collect();
    log.extend(
        info.stderr
            .iter()
            .rev()
            .take(4)
            .rev()
            .map(|line| (line.clone(), Level::Warning)),
    );
    Some(Frame {
        instance: info.id,
        name: info.server,
        state: word,
        tone,
        rows,
        log,
    })
}

/// The open details card's log as plain text, for copying.
pub fn language_details_log(state: &AppContext) -> Option<String> {
    let services = state.services().and_then(|r| r.language_services())?;
    let open = state.get().chrome.language_details?;
    let info = services.infos().into_iter().find(|info| info.id == open)?;
    let mut text = format!(
        "{} ({})\n",
        info.server,
        crate::ui::display_path(&info.root)
    );
    for line in info.log.iter().chain(info.stderr.iter()) {
        text.push_str(line);
        text.push('\n');
    }
    Some(text)
}

/// Language servers and syntax downloads: the active file's servers first.
pub fn language_panel_frame(state: &AppContext) -> crate::ui::language_panel::Frame {
    use crate::infrastructure::lsp::manager::Status;
    use crate::ui::language_panel::{Frame, Group, Level, Line, Progress, Section, Tone};
    let services = state.services().and_then(|r| r.language_services());
    let current = current_servers(state);
    let registry = crate::domain::language::Registry::builtin();
    let server_set = crate::features::language_service::server_set::ServerSet::current();
    let mut sections: Vec<Section> = services
        .map(|s| s.infos())
        .unwrap_or_default()
        .into_iter()
        .map(|info| {
            let group = match &current {
                Some((path, ids))
                    if ids.contains(&info.server_id) && path.starts_with(&info.root) =>
                {
                    Group::Current
                }
                _ => Group::Other,
            };
            // The languages it serves, such as "TypeScript, JavaScript".
            let mut names: Vec<String> = Vec::new();
            for binding in server_set
                .get(&info.server_id)
                .map(|definition| definition.languages.as_slice())
                .unwrap_or_default()
            {
                if let Some(id) = registry.language_by_name(&binding.language) {
                    let name = registry.definition(id).display_name.to_string();
                    if !names.contains(&name) {
                        names.push(name);
                    }
                }
            }
            let languages = match names.len() {
                0..=3 => names.join(", "),
                _ => format!("{}, …", names[..3].join(", ")),
            };
            let with = |detail: String| {
                if languages.is_empty() {
                    detail
                } else if detail.is_empty() {
                    languages.clone()
                } else {
                    format!("{languages} · {detail}")
                }
            };
            let mut lines = Vec::new();
            let mut subtitle_level = Level::Quiet;
            let mut progress = Progress::None;
            let mut restart = None;
            let (tone, state_text, subtitle) = match &info.status {
                Status::Starting => {
                    progress = Progress::Spinner;
                    (Tone::Busy, "Starting".to_owned(), languages.clone())
                }
                Status::Ready if info.tasks.is_empty() => {
                    restart = Some(info.id);
                    let detail = match &info.providers {
                        Some(providers) => {
                            let offered = providers.names();
                            if offered.is_empty() {
                                "diagnostics only".to_owned()
                            } else {
                                offered.join(", ")
                            }
                        }
                        None => String::new(),
                    };
                    (Tone::Ready, "Ready".to_owned(), with(detail))
                }
                Status::Ready => {
                    restart = Some(info.id);
                    progress = Progress::Spinner;
                    (Tone::Busy, "Working".to_owned(), languages.clone())
                }
                Status::Restarting { attempt, reason } => {
                    progress = Progress::Spinner;
                    subtitle_level = Level::Error;
                    (
                        Tone::Failed,
                        format!("Restarting ({attempt})"),
                        reason.clone(),
                    )
                }
                Status::Failed(reason) => {
                    restart = Some(info.id);
                    subtitle_level = Level::Error;
                    (Tone::Failed, "Stopped".to_owned(), reason.clone())
                }
                Status::Unavailable(reason) => {
                    lines.push(Line {
                        text: "Set another program with language_servers in settings.json."
                            .to_owned(),
                        level: Level::Quiet,
                        cancel: None,
                    });
                    (Tone::Off, "Unavailable".to_owned(), reason.clone())
                }
            };
            for task in &info.tasks {
                let mut text = task.title.clone();
                if let Some(percent) = task.percentage {
                    text.push_str(&format!(" {percent}%"));
                }
                if let Some(message) = &task.message {
                    text.push_str(&format!(" — {message}"));
                }
                if task.cancelling {
                    text.push_str(" (cancelling…)");
                }
                lines.push(Line {
                    text,
                    level: Level::Quiet,
                    cancel: (task.cancellable && !task.cancelling).then_some({
                        crate::ui::language_panel::TaskTarget {
                            instance: info.id,
                            run: info.run,
                            task: task.id,
                        }
                    }),
                });
            }
            Section {
                group,
                title: info.server.clone(),
                subtitle,
                subtitle_level,
                state: state_text,
                tone,
                progress,
                lines,
                install: None,
                restart,
            }
        })
        .collect();
    // The extension that would give the active file its syntax.
    let suggestion = syntax_suggestion(state, &state.get());
    if let Some(suggestion) = suggestion {
        let (title, subtitle, install, status) = match suggestion {
            Suggestion::Install { id, name } => (
                format!("{name} syntax"),
                "An extension adds it".to_owned(),
                Some((id, "Install")),
                "Available",
            ),
            Suggestion::Restart { name } => (
                format!("{name} syntax"),
                "Restart Loom to use it".to_owned(),
                None,
                "Installed",
            ),
        };
        sections.push(Section {
            group: Group::Download,
            title,
            subtitle,
            subtitle_level: Level::Quiet,
            state: status.to_owned(),
            tone: Tone::Missing,
            progress: Progress::None,
            lines: Vec::new(),
            install,
            restart: None,
        });
    }
    // The active file's servers first, then the others, then syntax to install.
    sections.sort_by_key(|section| section.group);
    Frame { sections }
}

/// The active text file's path and the ids of the servers its language is bound to.
fn current_servers(
    state: &AppContext,
) -> Option<(std::path::PathBuf, std::collections::BTreeSet<String>)> {
    let app = state.get();
    let id = app.workspace.active_document()?;
    let document = app.workspace.documents.get(&id).filter(|d| d.is_text())?;
    let registry = crate::domain::language::Registry::builtin();
    let servers = crate::features::language_service::server_set::ServerSet::current();
    let ids = servers
        .for_language(registry, document.meta.lang)
        .into_iter()
        .map(|(definition, _)| definition.name.to_string())
        .collect();
    Some((document.meta.path.clone(), ids))
}

pub fn status(rect: UiRect, state: AppContext) -> Element {
    let app = state.get();
    if let Some(view) = app.workspace.active() {
        if let Some(frame) = crate::app::image_preview::frame(&app, view) {
            return crate::ui::image_preview::status(
                rect,
                &frame,
                crate::app::image_preview::send(&state, view),
            );
        }
    }
    crate::ui::statusbar::render(rect, status_binding(&state))
}
