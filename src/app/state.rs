//! Application state — the single reactive root owned by the UI.
pub(crate) mod chrome;
pub(crate) mod editor;
pub(crate) mod layout;

#[derive(Clone, Default)]
pub struct TerminalPanelState {
    pub availability: crate::features::terminal::availability::TerminalAvailability,
    pub ui: crate::ui::terminal::state::TerminalUiState,
}

use std::path::{Path, PathBuf};

use crate::app::workspace::{AppPage, Workspace};
use crate::features::keymap::action::Action;
use crate::features::terminal::ShellKind;
use crate::model::pane_layout::{Direction, PaneId};
use crate::ui::theme::ThemeId;

pub use crate::ui::drawer::DrawerMode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseContinuation {
    CloseTabs,
    ExitApplication,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloseRequest {
    /// Tabs to close, as (pane, document).
    pub targets: Vec<(PaneId, crate::domain::identity::ViewId)>,
    pub continuation: CloseContinuation,
}

pub(crate) use crate::domain::notification::{Toast, ToastKind};

/// Top-level content shown in the editor region.
///
/// This is derived from the workspace rather than persisted, so opening or
/// closing a project/document cannot leave the chrome and content out of sync.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MainSurface {
    Welcome,
    WorkspaceHome,
    /// An empty pane next to other panes.
    Empty,
    Editor,
    Markdown,
    Image,
    Diff,
    Settings,
    Keymap,
    Extensions,
}

pub use chrome::{DrawerChrome, SidebarChrome};

#[derive(Clone)]
pub struct AppState {
    pub chrome: chrome::ChromeState,
    pub preferences: crate::features::settings::model::Model,
    pub editor: editor::EditorState,
    pub layout: layout::ApplicationLayoutState,
    pub terminal_chrome: chrome::TerminalChrome,
    pub terminal: TerminalPanelState,
    /// Right-hand drawer shared by Explorer and Search.
    pub drawer: DrawerChrome,
    /// Explorer business model and view state.
    pub explorer: crate::ui::explorer::state::ExplorerPanelState,
    /// Left-hand Source Control drawer.
    pub source_control: SidebarChrome,
    /// Source Control business model and view state.
    pub git: crate::ui::git::state::GitPanelState,
    pub workspace: Workspace,
    pub search: crate::ui::search::state::SearchPanelState,
    /// Font family fields on the Settings page; committed on Enter or blur.
    pub settings_ui: crate::ui::settings_binding::SettingsUiState,
    /// Keystrokes of a key sequence waiting for its next key.
    pub keymap: crate::features::keymap::model::Model,
    /// Filter typed into the Keymap page.
    pub keymap_ui: crate::ui::keymap_binding::KeymapUiState,
    /// The command palette opened by `workspace::OpenCommandPalette`.
    pub palette: crate::app::palette::PaletteState,
    /// Search, source, selection and tab of the Extensions page.
    pub extensions_ui: crate::ui::extensions_binding::ExtensionsUiState,
    /// Release checks and the title bar update button.
    pub update: crate::features::update::Model,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            chrome: Default::default(),
            preferences: crate::features::settings::model::Model::new(
                crate::features::settings::model::Values {
                    theme: ThemeId::default(),
                    ui_scale: None,
                    default_shell: ShellKind::default(),
                    terminal_profiles: Vec::new(),
                    shell_options: crate::infrastructure::terminal::profiles::options(&[]),
                    terminal_cursor_blink: true,
                    smooth_caret: true,
                    explorer_auto_reveal: true,
                    markdown: Default::default(),
                    check_for_updates: true,
                    editor_font: crate::ui::fonts::FontSettings::EDITOR,
                    terminal_font: crate::ui::fonts::FontSettings::TERMINAL,
                    file_associations: Vec::new(),
                    editor: Default::default(),
                    editor_languages: Default::default(),
                    encoding: Default::default(),
                    language_servers: Vec::new(),
                    languages: Default::default(),
                    lsp: Default::default(),
                    project_settings: Vec::new(),
                },
            ),
            drawer: DrawerChrome::default(),
            explorer: Default::default(),
            source_control: SidebarChrome::default(),
            git: Default::default(),
            editor: Default::default(),
            layout: Default::default(),
            terminal_chrome: Default::default(),
            terminal: Default::default(),
            workspace: Workspace::new(),
            search: Default::default(),

            settings_ui: Default::default(),

            keymap: Default::default(),
            keymap_ui: Default::default(),
            palette: Default::default(),
            extensions_ui: Default::default(),
            update: Default::default(),
        }
    }

    /// Build the application state and restore the last file-tree session.
    pub fn restored() -> Self {
        let mut app = Self::new();
        let session = match crate::app::workspace_session::load_checked() {
            Ok(session) => session,
            Err(error) => {
                app.show_error(format!("Could not restore workspace session: {error}"));
                Default::default()
            }
        };
        app.terminal.ui.terminal_workspace_root = session.terminal_workspace_root;
        app.terminal_chrome.show_terminal =
            session.show_terminal && !session.terminal_tabs.is_empty();
        if let Some(height) = session.terminal_height.filter(|height| height.is_finite()) {
            app.terminal_chrome.terminal_h = height;
        }
        app.explorer
            .model
            .restore(session.open_folders, session.recent_folders);
        app.source_control.visible = session.source_control_open;
        app.apply_settings(crate::infrastructure::settings_filesystem::load());
        app.refresh_shells();
        crate::app::explorer::hydrate_workspace_folders(&mut app);
        crate::app::workspace_commands::hydrate_editor_layout(&mut app, session.editor_layout);
        crate::app::explorer::open_launch_paths(
            &mut app,
            crate::infrastructure::launch::take_paths(),
            None,
        );
        app
    }

    /// The preferences stored in `settings.json`.
    pub fn settings(&self) -> crate::infrastructure::settings_filesystem::Settings {
        crate::infrastructure::settings_filesystem::Settings {
            theme: self.preferences.values().theme,
            editor: self.preferences.values().editor,
            editor_languages: self.preferences.values().editor_languages.clone(),
            encoding: self.preferences.values().encoding.clone(),
            ui_scale: self.preferences.values().ui_scale,
            default_shell: self.preferences.values().default_shell,
            terminal_profiles: self.preferences.values().terminal_profiles.clone(),
            terminal_cursor_blink: self.preferences.values().terminal_cursor_blink,
            smooth_caret: self.preferences.values().smooth_caret,
            markdown: self.preferences.values().markdown,
            explorer_auto_reveal: self.preferences.values().explorer_auto_reveal,
            check_for_updates: self.preferences.values().check_for_updates,
            git_inline_blame: self.git.ui.inline_blame,
            git_split_diff: self.git.ui.split_diff,
            git_tree_view: self.git.ui.tree_view,
            vim: self.editor.vim.options.into(),
            editor_font: self.preferences.values().editor_font.clone(),
            terminal_font: self.preferences.values().terminal_font.clone(),
            file_associations: self.preferences.values().file_associations.clone(),
            language_servers: self.preferences.values().language_servers.clone(),
            languages: self.preferences.values().languages.clone(),
            lsp: self.preferences.values().lsp.clone(),
        }
    }

    /// Applies preferences read from `settings.json`; returns whether
    /// anything changed.
    pub fn apply_settings(
        &mut self,
        settings: crate::infrastructure::settings_filesystem::Settings,
    ) -> bool {
        if settings == self.settings() {
            return false;
        }
        let associations_changed =
            settings.file_associations != self.preferences.values().file_associations;
        let shells_changed = settings.default_shell != self.preferences.values().default_shell
            || settings.terminal_profiles != self.preferences.values().terminal_profiles;
        self.preferences.values_mut().theme = settings.theme;
        self.preferences.values_mut().encoding = settings.encoding.clone();
        self.preferences.values_mut().editor = settings.editor;
        self.preferences.values_mut().editor_languages = settings.editor_languages;
        self.preferences.values_mut().ui_scale =
            crate::features::settings::scale::clamped(settings.ui_scale);
        self.preferences.values_mut().default_shell = settings.default_shell;
        self.preferences.values_mut().terminal_profiles = settings.terminal_profiles;
        self.preferences.values_mut().terminal_cursor_blink = settings.terminal_cursor_blink;
        self.preferences.values_mut().smooth_caret = settings.smooth_caret;
        self.preferences.values_mut().markdown = settings.markdown;
        self.preferences.values_mut().explorer_auto_reveal = settings.explorer_auto_reveal;
        self.preferences.values_mut().check_for_updates = settings.check_for_updates;
        self.preferences.values_mut().file_associations = settings.file_associations;
        self.preferences.values_mut().language_servers = settings.language_servers;
        self.preferences.values_mut().languages = settings.languages;
        self.preferences.values_mut().lsp = settings.lsp;
        self.git.ui.inline_blame = settings.git_inline_blame;
        self.git.ui.split_diff = settings.git_split_diff;
        self.git.ui.tree_view = settings.git_tree_view;
        self.set_editor_font(settings.editor_font);
        self.set_terminal_font(settings.terminal_font);
        let vim: loom_editor_core::vim::Options = settings.vim.into();
        if vim.enabled != self.editor.vim.options.enabled {
            crate::app::editor_vim::set_enabled(self, vim.enabled);
        }
        self.editor.vim.options = vim;
        if shells_changed {
            self.refresh_shells();
        }
        if associations_changed {
            self.refresh_language_rules();
        }
        true
    }

    /// Applies an editor font preference, keeping open views on the same line.
    pub fn set_editor_font(&mut self, font: crate::ui::fonts::FontSettings) {
        use crate::features::git::diff_document::row_height_for;
        let font = font.clamped();
        if font.family != self.preferences.values().editor_font.family {
            self.settings_ui
                .editor_font_input
                .set_text(font.family.clone());
        }
        if font == self.preferences.values().editor_font {
            return;
        }
        let (old_size, new_size) = (
            f32::from(self.preferences.values().editor_font.size),
            f32::from(font.size),
        );
        self.workspace.rescale_code_scroll(
            font.line_px() / self.preferences.values().editor_font.line_px(),
            row_height_for(new_size) / row_height_for(old_size),
        );
        self.preferences.values_mut().editor_font = font;
    }

    pub fn set_terminal_font(&mut self, font: crate::ui::fonts::FontSettings) {
        let font = font.clamped();
        if font.family != self.preferences.values().terminal_font.family {
            self.settings_ui
                .terminal_font_input
                .set_text(font.family.clone());
        }
        self.preferences.values_mut().terminal_font = font;
    }

    pub fn refresh_shells(&mut self) {
        crate::app::terminal_profiles::refresh_shells(self);
    }

    /// The shell new terminals launch: the saved default when available,
    /// otherwise the first available option.
    pub fn effective_shell(&self) -> ShellKind {
        crate::features::terminal::profiles::effective_default(
            self.preferences.values().default_shell,
            &self.preferences.values().shell_options,
        )
        .unwrap_or(self.preferences.values().default_shell)
    }

    /// `effective_shell` for a terminal starting in `cwd`, honoring the
    /// project settings of the workspace folder that holds it.
    pub fn effective_shell_at(&self, cwd: Option<&Path>) -> ShellKind {
        let preferred = self.settings_at(cwd).default_shell;
        crate::features::terminal::profiles::effective_default(
            preferred,
            &self.preferences.values().shell_options,
        )
        .unwrap_or(preferred)
    }

    /// The workspace folder holding `path`; the innermost when folders nest.
    pub fn workspace_folder_of(&self, path: &Path) -> Option<&Path> {
        self.explorer
            .model
            .roots()
            .iter()
            .filter(|root| path.starts_with(root))
            .max_by_key(|root| root.components().count())
            .map(PathBuf::as_path)
    }

    /// The user's settings with the project settings for `path` applied.
    pub fn settings_at(
        &self,
        path: Option<&Path>,
    ) -> crate::infrastructure::settings_filesystem::Settings {
        let settings = self.settings();
        let project = path
            .and_then(|path| self.workspace_folder_of(path))
            .and_then(|root| {
                self.preferences
                    .values()
                    .project_settings
                    .iter()
                    .find(|(r, _)| r == root)
            });
        match project {
            Some((_, project)) => {
                crate::infrastructure::settings_filesystem::overlay(&settings, &project.overrides)
            }
            None => settings,
        }
    }

    /// Re-reads every workspace folder's `.loom/settings.json`, reporting
    /// problems that are new since the last read.
    pub fn refresh_project_settings(&mut self) {
        use crate::infrastructure::project_settings::{self as project_settings, ProjectSettings};
        let mut fresh = Vec::new();
        for root in self.explorer.model.roots() {
            let project = match project_settings::read(root) {
                Ok(Some(project)) => project,
                Ok(None) => continue,
                Err(error) => ProjectSettings {
                    errors: vec![format!("not applied: {error}")],
                    ..Default::default()
                },
            };
            fresh.push((root.clone(), project));
        }
        let mut messages = Vec::new();
        for (root, project) in &fresh {
            let known = self
                .preferences
                .values()
                .project_settings
                .iter()
                .any(|(r, p)| r == root && p == project);
            if !known && let Some(first) = project.errors.first() {
                let more = match project.errors.len() {
                    1 => String::new(),
                    count => format!(" (and {} more)", count - 1),
                };
                messages.push(format!(
                    "{}: {first}{more}",
                    project_settings::path(root).display()
                ));
            }
        }
        self.preferences.values_mut().project_settings = fresh;
        // Also runs when only the workspace folders changed: patterns that name
        // directories are relative to the folder that holds the file. Files whose
        // language does not change keep their stamp, so this costs no re-lexing.
        self.refresh_language_rules();
        for message in messages {
            self.show_error(message);
        }
    }

    /// Rebuilds the file associations from the user's and every project's
    /// settings and the workspace folders, and re-recognizes the open files.
    /// Returns whether any open file changed language.
    pub fn refresh_language_rules(&mut self) -> bool {
        use crate::domain::language::{LanguageRules, PATH_CASE_INSENSITIVE, Registry};
        use crate::features::settings::rules::FileAssociation;
        let projects: Vec<(PathBuf, Vec<FileAssociation>)> = self
            .preferences
            .values()
            .project_settings
            .iter()
            .map(|(root, project)| {
                let rules = project
                    .overrides
                    .get("file_associations")
                    .and_then(|value| serde_json::from_value(value.clone()).ok())
                    .unwrap_or_default();
                (root.clone(), rules)
            })
            .collect();
        let project_specs: Vec<_> = projects
            .iter()
            .map(|(root, rules)| {
                (
                    root.clone(),
                    rules.iter().map(FileAssociation::spec).collect(),
                )
            })
            .collect();
        let user: Vec<_> = self
            .preferences
            .values()
            .file_associations
            .iter()
            .map(FileAssociation::spec)
            .collect();
        // Settings validate rules when they are read, so compiling reports nothing
        // new; a rule that still fails is skipped and the rest apply.
        let (rules, _) = LanguageRules::compile(
            Registry::builtin(),
            PATH_CASE_INSENSITIVE,
            self.explorer.model.roots(),
            &user,
            &project_specs,
        );
        self.workspace
            .set_language_rules(std::sync::Arc::new(rules))
    }

    pub fn shell_name(&self, kind: ShellKind) -> String {
        self.preferences
            .values()
            .shell_options
            .iter()
            .find(|s| s.kind == kind)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| kind.label().into())
    }

    /// Whether the shared default dock is open with terminal or commit content.
    pub fn default_terminal_open(&self) -> bool {
        self.terminal_chrome.show_terminal
            && (self.terminal.ui.panel_tab == crate::ui::terminal::state::PanelTab::Commits
                || self
                    .layout
                    .application_layout
                    .as_ref()
                    .is_none_or(|layout| layout.default_terminal_active().is_some()))
    }

    /// Apply a `workspace::`, `pane::` or `menu::` action. Actions that need a
    /// view or a platform service are performed by `key_actions` instead.
    pub fn apply(&mut self, action: Action) {
        match action {
            Action::ToggleSoftWrap => crate::app::editing::toggle_wrap(self),
            Action::ToggleMarkdown => {
                crate::app::markdown_view::toggle(self);
            }
            Action::MarkdownPreviewToSide => {
                crate::app::markdown_view::side(self);
            }
            Action::MarkdownOpenSource => {
                crate::app::markdown_view::source(self, None);
            }
            Action::CloneRepository => {
                if !self.git.model.cloning() {
                    self.git.ui.clone_visible = true;
                    self.git.ui.clone_error = None;
                }
            }
            Action::OpenSourceControl => {
                self.source_control.visible = true;
            }
            Action::ToggleSourceControl => {
                self.source_control.visible = !self.source_control.visible;
                if !self.source_control.visible {
                    self.git.ui.hovered = false;
                }
            }
            Action::ToggleGitPanel => {
                let opening = !self.git_panel_visible();
                self.terminal.ui.panel_tab = crate::ui::terminal::state::PanelTab::Commits;
                self.terminal_chrome.show_terminal = opening;
                self.terminal.ui.terminal_focused = false;
                self.terminal.ui.terminal_tab_context_menu = None;
                self.terminal.ui.terminal_shell_menu = false;
                self.terminal.ui.terminal_workspace_picker = None;
                self.git.ui.menu = None;
                self.git.ui.hover = None;
                self.git.ui.scroll_hover = None;
                self.git.ui.clear_hover_cards();
                if self.git.ui.panel_tab == crate::ui::git::panel::HISTORY {
                    self.git.ui.set_panel(true);
                } else {
                    self.git.ui.select_panel_tab(crate::ui::git::panel::HISTORY);
                }
            }
            Action::MenuSelectNext if self.palette.visible => self.palette.move_selection(1),
            Action::MenuSelectPrevious if self.palette.visible => self.palette.move_selection(-1),
            Action::MenuPageDown if self.palette.visible => {
                self.palette.move_selection(crate::app::palette::VISIBLE_ROWS as isize)
            }
            Action::MenuPageUp if self.palette.visible => {
                self.palette.move_selection(-(crate::app::palette::VISIBLE_ROWS as isize))
            }
            Action::MenuSelectNext => {
                crate::ui::git::branch_picker::move_selection(&mut self.git, 1)
            }
            Action::MenuSelectPrevious => {
                crate::ui::git::branch_picker::move_selection(&mut self.git, -1)
            }
            Action::OpenExplorer => {
                self.drawer.visible = true;
                self.drawer.mode = DrawerMode::Files;
                self.search.blur();
            }
            Action::OpenSearch => {
                self.drawer.visible = true;
                self.drawer.mode = DrawerMode::Search;
                self.search.focus_request += 1;
                self.search.results_focus_request = 0;
                self.close_menus();
            }
            Action::OpenOutline => {
                self.drawer.visible = true;
                self.drawer.mode = DrawerMode::Outline;
                self.search.blur();
                self.close_menus();
            }
            Action::ToggleDrawer => {
                self.drawer.visible = !self.drawer.visible;
                if !self.drawer.visible {
                    self.explorer.ui.hovered = false;
                    self.search.blur();
                }
            }
            Action::ToggleTerminal => {
                self.terminal_chrome.show_terminal = !self.default_terminal_open();
                self.terminal.ui.terminal_tab_context_menu = None;
            }
            Action::CloseOverlay => {
                self.close_menus();
                self.editor.close_request = None;
                self.editor.tabs.tab_drag = None;
                self.editor.tabs.file_drag = None;
                self.editor.sash_drag = None;
                self.layout.application_sash_drag = None;
                self.editor.tabs.tab_scrollbar_dragging = None;
                if !self.git.model.cloning() {
                    self.git.ui.clone_visible = false;
                    self.git.ui.clone_input_focused = false;
                }
            }
            Action::MenuCancel => self.close_menus(),
            Action::ActivateNextItem => self.workspace.next(),
            Action::ActivatePrevItem => self.workspace.prev(),
            Action::ActivateItem(index) => {
                self.workspace.activate_at(index);
            }
            Action::ActivateLastItem => {
                self.workspace.activate_last();
            }
            Action::OpenSettings => {
                self.refresh_shells();
                self.refresh_project_settings();
                self.workspace.open_settings();
            }
            Action::OpenKeymap => {
                self.workspace.open_keymap();
            }
            Action::OpenExtensions => {
                self.workspace.open_extensions();
            }
            Action::IncreaseFontSize | Action::DecreaseFontSize | Action::ResetFontSize => {
                let terminal = self.terminal.ui.terminal_focused;
                let (mut font, default_size) = if terminal {
                    (
                        self.preferences.values().terminal_font.clone(),
                        crate::ui::fonts::FontSettings::TERMINAL.size,
                    )
                } else {
                    (
                        self.preferences.values().editor_font.clone(),
                        crate::ui::fonts::FontSettings::EDITOR.size,
                    )
                };
                font.size = match action {
                    Action::IncreaseFontSize => font.size.saturating_add(1),
                    Action::DecreaseFontSize => font.size.saturating_sub(1),
                    _ => default_size,
                };
                if terminal {
                    self.set_terminal_font(font);
                } else {
                    self.set_editor_font(font);
                }
            }
            Action::SplitRight => self.split_focused(Direction::Right),
            Action::SplitLeft => self.split_focused(Direction::Left),
            Action::SplitUp => self.split_focused(Direction::Up),
            Action::SplitDown => self.split_focused(Direction::Down),
            Action::ActivatePaneLeft => {
                self.workspace.activate_neighbor(Direction::Left);
            }
            Action::ActivatePaneRight => {
                self.workspace.activate_neighbor(Direction::Right);
            }
            Action::ActivatePaneUp => {
                self.workspace.activate_neighbor(Direction::Up);
            }
            Action::ActivatePaneDown => {
                self.workspace.activate_neighbor(Direction::Down);
            }
            Action::ActivateNextPane => {
                self.workspace.activate_next_pane(true);
            }
            Action::ActivatePrevPane => {
                self.workspace.activate_next_pane(false);
            }
            Action::MoveItemToPaneLeft => {
                self.workspace.move_active_item(Direction::Left);
            }
            Action::MoveItemToPaneRight => {
                self.workspace.move_active_item(Direction::Right);
            }
            Action::MoveItemToPaneUp => {
                self.workspace.move_active_item(Direction::Up);
            }
            Action::MoveItemToPaneDown => {
                self.workspace.move_active_item(Direction::Down);
            }
            Action::ClosePane => {
                if self.editor.close_request.is_none() {
                    let pane = self.workspace.active_pane();
                    crate::app::workspace_commands::request_close_pane_in(self, pane);
                }
            }
            Action::CloseActiveItem => {
                // A pending (possibly batch) request must be resolved first.
                if self.editor.close_request.is_none()
                    && let Some(id) = self.workspace.active()
                {
                    let pane = self.workspace.active_pane();
                    crate::app::workspace_commands::request_close_tab_in(self, pane, id);
                }
            }
            _ => {}
        }
    }

    /// Dismiss every menu and picker so the palette is the only overlay.
    pub(crate) fn close_overlays_for_palette(&mut self) {
        self.close_menus();
        self.git.ui.stash_switch = None;
        self.git.ui.confirm = None;
    }

    fn close_menus(&mut self) {
        self.palette.close();
        self.terminal.ui.terminal_workspace_picker = None;
        self.editor.menu = None;
        self.explorer.ui.menu = None;
        self.explorer.ui.menu_target = None;
        self.explorer.ui.menu_hover = None;
        self.editor.tabs.tab_context_menu = None;
        self.terminal.ui.terminal_tab_context_menu = None;
        self.git.ui.branch_picker = None;
        self.git.ui.menu = None;
    }

    /// Split the focused pane, dropping pointer gestures tied to the old layout.
    fn split_focused(&mut self, direction: Direction) {
        let pane = self.workspace.active_pane();
        if self.workspace.split(pane, direction).is_some() {
            self.editor.tabs.tab_drag = None;
            self.editor.drag = None;
            self.editor.menu = None;
        }
    }

    /// What the focused pane shows.
    pub fn main_surface(&self) -> MainSurface {
        self.pane_surface(self.workspace.active_pane())
    }

    /// Commit history shares the default bottom panel with terminals.
    pub fn git_panel_visible(&self) -> bool {
        self.terminal_chrome.show_terminal
            && self.terminal.ui.panel_tab == crate::ui::terminal::state::PanelTab::Commits
    }

    /// What one pane shows. Welcome and Workspace Home fill the editor area
    /// only while it is a single empty pane.
    pub fn pane_surface(&self, pane: PaneId) -> MainSurface {
        if let Some(page) = self.workspace.pane_page(pane) {
            match page {
                AppPage::Settings => MainSurface::Settings,
                AppPage::Keymap => MainSurface::Keymap,
                AppPage::Extensions => MainSurface::Extensions,
            }
        } else if self.workspace.diff(pane).is_some() {
            MainSurface::Diff
        } else if self.workspace.pane_open_error(pane).is_some() {
            MainSurface::Empty
        } else if self
            .workspace
            .pane(pane)
            .and_then(|p| p.active())
            .and_then(|id| self.workspace.view(id))
            .is_some_and(|v| v.presentation == crate::domain::view::Presentation::Image)
        {
            MainSurface::Image
        } else if self
            .workspace
            .pane(pane)
            .and_then(|pane| pane.active())
            .and_then(|id| self.workspace.view(id))
            .is_some_and(|view| view.presentation == crate::domain::view::Presentation::Markdown)
        {
            MainSurface::Markdown
        } else if self.workspace.editor(pane).is_some() {
            MainSurface::Editor
        } else if self
            .workspace
            .pane(pane)
            .and_then(|p| p.active())
            .is_some_and(|id| self.workspace.is_file(id))
            || self.workspace.pane_count() > 1
        {
            MainSurface::Empty
        } else if self.explorer.model.roots().is_empty() {
            MainSurface::Welcome
        } else {
            MainSurface::WorkspaceHome
        }
    }

    /// Neutral guidance, e.g. a missing commit message.
    pub fn show_toast(&mut self, msg: impl Into<String>) {
        self.push_toast(ToastKind::Info, msg.into());
    }

    /// Confirms a completed action, e.g. a copied path.
    pub fn show_success(&mut self, msg: impl Into<String>) {
        self.push_toast(ToastKind::Success, msg.into());
    }

    /// Reports a failed operation.
    pub fn show_error(&mut self, msg: impl Into<String>) {
        self.push_toast(ToastKind::Error, msg.into());
    }

    fn push_toast(&mut self, kind: ToastKind, message: String) {
        self.chrome.toast_seq += 1;
        self.chrome.toast = Some(Toast {
            id: self.chrome.toast_seq,
            kind,
            message,
        });
    }

    /// Clears the toast only if it is still `id`, so a stale timer cannot
    /// dismiss a newer message. Returns whether anything changed.
    pub fn dismiss_toast(&mut self, id: u64) -> bool {
        let matches = self
            .chrome
            .toast
            .as_ref()
            .is_some_and(|toast| toast.id == id);
        if matches {
            self.chrome.toast = None;
        }
        matches
    }
}

pub use crate::ui::chrome::tab_state::{
    FileDragSource, FileDragState, TabContextMenuState, TabDragState, TabDrop,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::git::diff_document::DiffDocument;
    use crate::features::git::{DiffTarget, diff::UnifiedDiff};
    use std::path::PathBuf;

    #[test]
    fn main_surface_is_derived_from_projects_and_documents() {
        let mut app = AppState::new();
        assert_eq!(app.main_surface(), MainSurface::Welcome);

        app.explorer
            .model
            .restore(vec![PathBuf::from("project")], Vec::new());
        assert_eq!(app.main_surface(), MainSurface::WorkspaceHome);

        let file = app
            .workspace
            .open_path(PathBuf::from("project/main.rs"), String::new());
        assert_eq!(app.main_surface(), MainSurface::Editor);

        app.workspace.close(file);
        assert_eq!(app.main_surface(), MainSurface::WorkspaceHome);

        app.workspace.open_settings();
        assert_eq!(app.main_surface(), MainSurface::Settings);

        let settings = app.workspace.active().unwrap();
        app.workspace.test_close_document(settings);
        assert_eq!(app.main_surface(), MainSurface::WorkspaceHome);

        let diff = app.workspace.open_diff(DiffDocument::from_unified(
            PathBuf::from("project"),
            PathBuf::from("main.rs"),
            DiffTarget::IndexToWorktree,
            UnifiedDiff::default(),
        ));
        assert_eq!(app.main_surface(), MainSurface::Diff);

        app.workspace.test_close_document(diff);
        app.explorer.model.restore(Vec::new(), Vec::new());
        assert_eq!(app.main_surface(), MainSurface::Welcome);
    }

    #[test]
    fn commits_are_a_bottom_category_independent_of_the_editor() {
        let mut app = AppState::new();
        assert!(!app.git_panel_visible());
        app.workspace
            .open_path("project/main.rs".into(), String::new());
        app.source_control.visible = true;
        assert!(!app.git_panel_visible());
        app.apply(Action::ToggleGitPanel);
        assert!(app.git_panel_visible());
        assert!(app.default_terminal_open());
        app.workspace.open_settings();
        assert!(app.git_panel_visible());
        app.apply(Action::ToggleGitPanel);
        assert!(!app.git_panel_visible());
        app.terminal.ui.panel_tab = crate::ui::terminal::state::PanelTab::Terminal;
        app.terminal_chrome.show_terminal = true;
        assert!(!app.git_panel_visible());
    }

    #[test]
    fn standalone_file_uses_editor_then_returns_to_welcome() {
        let mut app = AppState::new();
        let file = app
            .workspace
            .open_path(PathBuf::from("notes.txt"), String::new());
        assert_eq!(app.main_surface(), MainSurface::Editor);

        app.workspace.close(file);
        assert_eq!(app.main_surface(), MainSurface::Welcome);
    }

    #[test]
    fn stale_toast_timer_does_not_dismiss_a_newer_toast() {
        let mut app = AppState::new();
        app.show_success("Copied path.");
        let first = app.chrome.toast.as_ref().unwrap().id;
        app.show_error("Could not save.");

        assert!(!app.dismiss_toast(first));
        assert_eq!(app.chrome.toast.as_ref().unwrap().kind, ToastKind::Error);

        let second = app.chrome.toast.as_ref().unwrap().id;
        assert!(app.dismiss_toast(second));
        assert!(app.chrome.toast.is_none());
    }

    #[test]
    fn git_view_preferences_round_trip_through_settings() {
        let mut app = AppState::new();
        let mut settings = app.settings();
        settings.git_inline_blame = true;
        settings.git_split_diff = true;
        settings.git_tree_view = true;

        assert!(app.apply_settings(settings.clone()));
        assert!(app.git.ui.inline_blame && app.git.ui.split_diff && app.git.ui.tree_view);
        assert_eq!(app.settings(), settings);
        assert!(!app.apply_settings(settings));
    }

    #[test]
    fn source_control_chrome_and_diff_split_keep_their_defaults() {
        let app = AppState::new();
        assert!(!app.source_control.visible);
        assert_eq!(app.source_control.width, crate::ui::theme::SIDEBAR_W);
        assert_eq!(app.editor.diff_split.ratio, 0.5);
        assert!(app.editor.diff_split.drag.is_none());
        assert!(
            !app.git
                .ui
                .collapsed_sections
                .contains(&crate::ui::git::panel::HISTORY)
        );
        assert!(app.git.ui.panel_open);
        assert!(!app.git_panel_visible());
    }
}
