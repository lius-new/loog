//! Explorer view composition: projects the Explorer panel out of the root
//! state, supplies per-frame inputs, and runs the view's semantic commands.
//! Cross-feature effects (editor drags, terminals, clipboard, drawer chrome)
//! stay here so the view never sees the root state.
use crate::app::context::AppContext;

use std::path::Path;
use std::sync::Arc;

use lgui::application::ApplicationContext;
use lgui::core::UiFocusHandle;
use lgui::prelude::{Element, State, UiRect};
use lgui::services::ServicesContextExt;

use crate::app::state::{FileDragSource, FileDragState};
#[cfg(test)]
use crate::app::terminal_binding::TerminalTabs;
use crate::features::explorer::{ExplorerContextTarget, ExplorerCreateKind, ExplorerTargetKind};
use crate::features::git::{GitStoreSnapshot, PathDecoration};
use crate::ui::components::state_access::StateAccess;
use crate::ui::drawer::{self, DrawerMode};
use crate::ui::explorer::commands::{
    Command, Commands, Decoration, EditInput, Frame, GitTone, MenuAction,
};
use crate::ui::explorer::state::ExplorerPanelState;

/// Services the command handler needs besides the root state.
#[derive(Clone)]
pub(crate) struct Services {
    pub application: ApplicationContext,
    pub editor_focus: UiFocusHandle,
    pub terminal_tabs: crate::app::terminal_binding::TerminalStore,
}

fn projection(state: &AppContext) -> StateAccess<ExplorerPanelState> {
    StateAccess::project(state.model(), |app| &app.explorer, |app| &mut app.explorer)
}

fn commands(state: &AppContext, services: &Services) -> Commands {
    let state = state.clone();
    let services = services.clone();
    Commands::new(move |command| run(&state, &services, command))
}

/// The Explorer/Search drawer header, shared with the Search view.
pub(crate) fn drawer_header(rect: UiRect, state: &AppContext) -> Element {
    use crate::features::keymap::action::Action;
    let mode = state.get().drawer.mode;
    let select_state = state.clone();
    drawer::header(
        rect,
        mode,
        Arc::new(move |mode| {
            select_state.update(|app| {
                app.apply(match mode {
                    DrawerMode::Files => Action::OpenExplorer,
                    DrawerMode::Search => Action::OpenSearch,
                    DrawerMode::Outline => Action::OpenOutline,
                })
            })
        }),
    )
}

/// Source-control decoration of a tree row, from the current Git snapshot.
fn decoration(git: &GitStoreSnapshot, path: &Path) -> Decoration {
    Decoration {
        tone: match git.decoration_for_absolute_path(path) {
            Some(PathDecoration::Conflict) => Some(GitTone::Conflict),
            Some(PathDecoration::Deleted) => Some(GitTone::Deleted),
            Some(PathDecoration::Modified) => Some(GitTone::Modified),
            Some(PathDecoration::Created) => Some(GitTone::Added),
            Some(PathDecoration::Ignored) => Some(GitTone::Ignored),
            None => None,
        },
        indicator: git
            .repository_for_path(path)
            .and_then(|repository| repository.state_for_absolute_path(path))
            .map(|state| state.display_kind().indicator())
            .unwrap_or_default(),
    }
}

pub(crate) fn render(
    rect: UiRect,
    state: &AppContext,
    git_store: &State<GitStoreSnapshot>,
    services: &Services,
    input: EditInput,
) -> Element {
    let app = state.get();
    let git = git_store.get();
    let decorate = |path: &Path| decoration(&git, path);
    let frame = Frame {
        active_path: app.workspace.active_path(),
        resizing: app.drawer.resizing,
        resizable: app.layout.application_layout.is_none(),
        decorate: &decorate,
    };
    let header = drawer_header(
        UiRect::new(
            rect.left,
            rect.top,
            rect.right,
            rect.top + crate::ui::theme::TABS_H,
        ),
        state,
    );
    crate::ui::explorer::view::render(
        rect,
        projection(state),
        &frame,
        commands(state, services),
        header,
        input,
    )
}

pub(crate) fn render_menu(
    viewport: UiRect,
    position: (f32, f32),
    state: &AppContext,
    services: &Services,
) -> Element {
    let app = state.get();
    crate::ui::explorer::context_menu::render(
        viewport,
        position,
        projection(state),
        app.workspace.active_path(),
        commands(state, services),
    )
}

fn run(state: &AppContext, services: &Services, command: Command) {
    use super::explorer;
    match command {
        Command::ResourceRecovery { resource, forget } => {
            let Some(host) = state.services().and_then(|r| r.extensions()) else {
                return;
            };
            if forget {
                host.forget_resource(&resource);
            } else {
                let bookmark = state
                    .get()
                    .explorer
                    .resources
                    .bookmarks
                    .iter()
                    .find(|bookmark| bookmark.resource == resource)
                    .cloned();
                if let Some(bookmark) = bookmark {
                    host.restore_resource(bookmark);
                }
            }
        }
        Command::ResourceMenu { point, target } => state.update(|app| {
            app.editor.tabs.tab_context_menu = None;
            app.explorer.ui.dismiss_menu();
            app.explorer.ui.menu = Some(point);
            app.explorer.ui.resource_menu = Some(target);
        }),
        Command::ResourceAction { action, target } => {
            if action == MenuAction::OpenTerminal {
                let root = state
                    .get()
                    .explorer
                    .resources
                    .root(target.scope)
                    .map(|root| root.descriptor.clone());
                if let Some(root) = root {
                    super::terminal::new_provided(
                        state,
                        &services.terminal_tabs,
                        crate::features::terminal::model::ProvidedTerminal {
                            root: root.resource,
                            cwd: target.path,
                            name: root.name,
                        },
                        None,
                    );
                }
                return;
            }
            super::resource_edit::action(
                state,
                action,
                target,
                services.application.clipboard().as_ref(),
            );
        }
        Command::ResourceDirectory { scope, path, more } => {
            super::resources::directory(state, scope, path, more)
        }
        Command::ResourceFile { scope, file } => {
            super::resources::open(state, scope, file);
            services.editor_focus.focus();
        }
        Command::ToggleDirectory(key) => {
            state.update(move |app| explorer::toggle_directory(app, &key));
        }
        Command::OpenMenu { point, target } => state.update(move |app| {
            app.explorer.ui.resource_menu = None;
            app.editor.tabs.tab_context_menu = None;
            // Right-clicking outside the selection starts over; inside it the
            // menu applies to the whole selection.
            let keeps = target.as_ref().is_some_and(|target| {
                target.kind == ExplorerTargetKind::File
                    && app.explorer.ui.selection.contains(&target.path)
            });
            if !keeps {
                app.explorer.ui.selection.clear();
                app.explorer.ui.selection_anchor = None;
            }
            app.explorer.ui.menu = Some(point);
            app.explorer.ui.menu_target = target;
            app.explorer.ui.menu_hover = None;
        }),
        Command::PressFile { path, point } => state.update(move |app| {
            let ctrl = app.editor.modifiers.ctrl() || app.editor.modifiers.meta();
            let shift = app.editor.modifiers.shift();
            if ctrl || shift {
                // Multi-select only changes the selection; it never opens
                // the file or starts a drag.
                let ui = &mut app.explorer.ui;
                if shift {
                    let files = app.explorer.model.visible_files();
                    let from = ui
                        .selection_anchor
                        .as_ref()
                        .and_then(|anchor| files.iter().position(|p| p == anchor));
                    let to = files.iter().position(|p| *p == path);
                    if let (Some(from), Some(to)) = (from, to) {
                        let (lo, hi) = (from.min(to), from.max(to));
                        if !ctrl {
                            ui.selection.clear();
                        }
                        ui.selection.extend(files[lo..=hi].iter().cloned());
                    } else {
                        ui.selection.insert(path.clone());
                        ui.selection_anchor = Some(path);
                    }
                } else {
                    if !ui.selection.remove(&path) {
                        ui.selection.insert(path.clone());
                    }
                    ui.selection_anchor = Some(path);
                }
                return;
            }
            app.explorer.ui.selection.clear();
            app.explorer.ui.selection_anchor = Some(path.clone());
            let clicks = app.editor.tree_click_count(&path, point.0, point.1);
            app.editor.tabs.file_drag = Some(FileDragState {
                path,
                source: FileDragSource::Explorer { clicks },
                origin: point,
                point,
                active: false,
                drop: None,
            });
        }),
        Command::OpenPinned(path) => {
            let loader = state.services().and_then(|r| r.file_loader());
            state.update(move |app| match app.workspace.file_id_for_path(&path) {
                Some(id) => {
                    app.workspace.activate_document(id);
                    app.workspace.promote_active_preview();
                }
                None => {
                    explorer::open_tree_file(app, path, 2, loader.as_ref());
                }
            });
            services.editor_focus.focus();
        }
        Command::ChooseFolder => {
            explorer::choose_folder(state);
        }
        Command::CloneRepository => state.update(|app| {
            app.git.ui.clone_visible = true;
            app.git.ui.clone_error = None;
        }),
        Command::SubmitEdit => {
            if state.get().explorer.resources.edit.is_some() {
                super::resource_edit::submit(state);
            } else if state.get().explorer.model.create_request().is_some() {
                explorer::finish_create(state);
            } else {
                explorer::finish_rename(state);
            }
        }
        Command::CancelEdit => {
            state.update(|app| app.explorer.resources.edit = None);
            explorer::cancel_edit(state);
        }
        Command::ResizeStart => state.update(|app| app.drawer.resizing = true),
        Command::Resize(width) => state.update(move |app| {
            if app.drawer.resizing {
                app.drawer.width = width;
            }
        }),
        Command::ResizeEnd => state.update(|app| app.drawer.resizing = false),
        Command::Menu { action, target } => run_menu(state, services, action, target),
    }
}

/// The selected files, in tree order, when `target` is one of several.
fn batch_paths(
    state: &AppContext,
    target: &ExplorerContextTarget,
) -> Option<Vec<std::path::PathBuf>> {
    use std::path::PathBuf;
    let app = state.get();
    let selection = &app.explorer.ui.selection;
    if target.kind != ExplorerTargetKind::File
        || selection.len() < 2
        || !selection.contains(&target.path)
    {
        return None;
    }
    let ordered: Vec<PathBuf> = app
        .explorer
        .model
        .visible_files()
        .into_iter()
        .filter(|path| selection.contains(path))
        .collect();
    // Files hidden by a collapsed folder stay selected: append them last.
    let mut rest: Vec<PathBuf> = selection
        .iter()
        .filter(|path| !ordered.contains(path))
        .cloned()
        .collect();
    rest.sort();
    Some(ordered.into_iter().chain(rest).collect())
}

fn run_menu(
    state: &AppContext,
    services: &Services,
    action: MenuAction,
    target: Option<ExplorerContextTarget>,
) {
    use super::explorer;
    if action == MenuAction::AddFolder {
        explorer::choose_folder_to_add(state);
        return;
    }
    let Some(target) = target else {
        return;
    };
    // Acting on a member of a multi-selection applies to every selected file.
    let batch = batch_paths(state, &target);
    if let Some(paths) = batch {
        match action {
            MenuAction::Cut | MenuAction::Copy => {
                explorer::copy_items(state, &paths, action == MenuAction::Cut);
                return;
            }
            MenuAction::Delete => {
                state.update(|app| {
                    app.explorer.ui.selection.clear();
                    app.explorer.ui.selection_anchor = None;
                });
                explorer::delete_items(state, paths);
                return;
            }
            MenuAction::CopyPath | MenuAction::CopyRelativePath => {
                explorer::copy_paths(
                    state,
                    services.application.clipboard().as_ref(),
                    &paths,
                    action == MenuAction::CopyRelativePath,
                );
                return;
            }
            _ => {}
        }
    }
    match action {
        MenuAction::NewFile => explorer::begin_create(state, target.path, ExplorerCreateKind::File),
        MenuAction::NewFolder => {
            explorer::begin_create(state, target.path, ExplorerCreateKind::Folder)
        }
        MenuAction::RefreshExplorer => state.update(|app| {
            explorer::refresh(app);
            app.workspace.reconcile_disk();
        }),
        MenuAction::OpenTerminal => {
            let cwd = match target.kind {
                ExplorerTargetKind::File => target.path.parent().map(Path::to_path_buf),
                ExplorerTargetKind::Directory => Some(target.path),
            };
            if let Some(cwd) = cwd {
                let shell = state.get().effective_shell_at(Some(&cwd));
                services.terminal_tabs.update(move |tabs| {
                    tabs.add_at(shell, Some(cwd));
                });
                state.update(|app| {
                    app.terminal_chrome.show_terminal = true;
                    app.terminal.ui.terminal_shell_menu = false;
                });
            }
        }
        MenuAction::Cut | MenuAction::Copy => {
            explorer::copy_item(state, &target.path, action == MenuAction::Cut)
        }
        MenuAction::Rename => explorer::begin_rename(state, target.path, target.kind),
        MenuAction::Delete => explorer::delete_item(state, target.path),
        MenuAction::RemoveFolder => explorer::remove_root(state, target.path),
        MenuAction::RevealInFileExplorer => explorer::reveal_item(state, &target),
        MenuAction::CopyPath | MenuAction::CopyRelativePath => explorer::copy_path(
            state,
            services.application.clipboard().as_ref(),
            &target.path,
            action == MenuAction::CopyRelativePath,
        ),
        MenuAction::PreviewMarkdown => {
            let pane = state.get().workspace.active_pane();
            super::markdown_view::open_local(state, pane, target.path, None);
        }
        MenuAction::ToggleSvg => {
            if target.kind == ExplorerTargetKind::File
                && super::image_preview::toggle_path(state, target.path)
            {
                services.editor_focus.focus();
            }
        }
        MenuAction::AddFolder => unreachable!("handled before the target check"),
        MenuAction::WorkspaceTrust => {}
    }
}

#[cfg(test)]
mod tests {
    use crate::app::state::AppState;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use lgui::application::AppView;
    use lgui::core::{UiId, UiScale};
    use lgui::prelude::panel;
    use lgui::session::UiSession;

    use super::*;
    use crate::app::state::TabContextMenuState;
    use crate::app::workspace_commands::close_test_directory;

    struct Harness {
        state: AppContext,
        services: Services,
        session: UiSession,
        view: AppView,
    }

    const VIEWPORT: UiRect = UiRect::new(0.0, 0.0, 800.0, 600.0);

    #[cfg(windows)]
    fn process_memory() -> (usize, usize, usize, usize) {
        #[repr(C)]
        struct Counters {
            size: u32,
            faults: u32,
            values: [usize; 9],
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn K32GetProcessMemoryInfo(
                process: windows_sys::Win32::Foundation::HANDLE,
                counters: *mut Counters,
                size: u32,
            ) -> i32;
        }
        let mut counters = Counters {
            size: std::mem::size_of::<Counters>() as u32,
            faults: 0,
            values: [0; 9],
        };
        unsafe {
            assert_ne!(
                K32GetProcessMemoryInfo(
                    windows_sys::Win32::System::Threading::GetCurrentProcess(),
                    &mut counters,
                    std::mem::size_of::<Counters>() as u32,
                ),
                0,
            );
        }
        use windows_sys::Win32::System::Memory::{GetProcessHeaps, HEAP_SUMMARY, HeapSummary};
        let mut allocated = 0;
        let mut committed = 0;
        unsafe {
            let mut heaps =
                vec![std::ptr::null_mut(); GetProcessHeaps(0, std::ptr::null_mut()) as usize];
            let count = GetProcessHeaps(heaps.len() as u32, heaps.as_mut_ptr()) as usize;
            assert!(count <= heaps.len());
            for heap in heaps.into_iter().take(count) {
                let mut summary = HEAP_SUMMARY {
                    cb: std::mem::size_of::<HEAP_SUMMARY>() as u32,
                    ..Default::default()
                };
                if HeapSummary(heap, 0, &mut summary) != 0 {
                    allocated += summary.cbAllocated;
                    committed += summary.cbCommitted;
                }
            }
        }
        (counters.values[1], counters.values[8], allocated, committed)
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "isolated Explorer expansion/collapse memory investigation"]
    fn explorer_memory_profile() {
        use crate::features::explorer::DirEntry;
        use lgui::core::PhysicalRect;
        use lgui::icons::IconRegistration;
        use lgui::render_skia::SkiaSoftwareSurface;
        use lgui::renderer::{FrameInfo, FrameReason};

        fn report(stage: &str, h: &Harness, surface: &SkiaSoftwareSurface) {
            let (working, private, heap_allocated, heap_committed) = process_memory();
            let (components, host) = lgui::backend::session_memory_usage(&h.session);
            let app = h.state.get();
            let listing_bytes = app
                .explorer
                .model
                .listings()
                .map(|(key, entries)| {
                    key.capacity()
                        + entries.capacity() * std::mem::size_of::<DirEntry>()
                        + entries
                            .iter()
                            .map(|entry| entry.name.capacity())
                            .sum::<usize>()
                })
                .sum::<usize>();
            println!(
                "EXPLORER_MEMORY {}",
                serde_json::json!({
                    "stage": stage, "working": working, "private": private,
                    "heap_allocated": heap_allocated, "heap_committed": heap_committed,
                    "nodes": h.session.tree().nodes().len(),
                    "tree_bytes": h.session.tree().estimated_bytes(),
                    "components": format!("{components:?}"), "host": format!("{host:?}"),
                    "listing_bytes": listing_bytes,
                    "renderer": format!("{:?}", surface.cache_stats()),
                })
            );
        }
        fn draw(h: &Harness, surface: &mut SkiaSoftwareSurface) {
            let frame = FrameInfo::new(
                PhysicalRect::new(0, 0, 800, 600),
                &[],
                UiScale::ONE,
                FrameReason::SceneChange,
                true,
            );
            lgui::assets::backend::with_render_resources(
                &h.services.application,
                lgui::RenderResources::new(),
                || {
                    surface.draw(&h.session.tree().scene(), &frame).unwrap();
                },
            );
        }
        let roots = [PathBuf::from("project-a"), PathBuf::from("project-b")];
        let mut app = AppState::new();
        app.explorer.model.restore(roots.to_vec(), vec![]);
        for root in &roots {
            app.explorer.model.insert_listing(
                root.to_string_lossy().into_owned(),
                Arc::new(
                    (0..50)
                        .map(|i| {
                            DirEntry::new(
                                &Arc::from(root.as_path()),
                                format!("directory-{i:03}"),
                                true,
                            )
                        })
                        .collect(),
                ),
            );
            app.explorer
                .model
                .set_expanded(&root.to_string_lossy(), true);
        }
        let mut h = harness(app);
        h.services
            .application
            .resources()
            .provide(IconRegistration(Arc::new(crate::app::svg_icons())));
        // Preserve the production's unbounded rendering policy during diagnosis.
        let mut surface = SkiaSoftwareSurface::new(usize::MAX);
        draw(&h, &mut surface);
        report("initial", &h, &surface);
        for directory in 0..100 {
            let parent = roots[directory / 50].join(format!("directory-{:03}", directory % 50));
            h.state.update(|app| {
                app.explorer.model.insert_listing(
                    parent.to_string_lossy().into_owned(),
                    Arc::new(
                        (0..40)
                            .map(|i| {
                                DirEntry::new(
                                    &Arc::from(parent.as_path()),
                                    format!("source-file-{i:03}.rs"),
                                    false,
                                )
                            })
                            .collect(),
                    ),
                );
                app.explorer
                    .model
                    .set_expanded(&parent.to_string_lossy(), true);
            });
            h.render();
            draw(&h, &mut surface);
            if [9, 24, 49, 99].contains(&directory) {
                report(&format!("expanded-{}", directory + 1), &h, &surface);
            }
        }
        for cycle in 0..3 {
            for expanded in [false, true] {
                h.state.update(|app| {
                    for root in &roots {
                        app.explorer
                            .model
                            .set_expanded(&root.to_string_lossy(), expanded);
                    }
                });
                h.render();
                draw(&h, &mut surface);
                report(&format!("cycle-{cycle}-expanded-{expanded}"), &h, &surface);
            }
        }
        surface.trim(lgui::renderer::MemoryPressure::Critical);
        report("renderer-trimmed", &h, &surface);
        h.state.update(|app| {
            for root in &roots {
                app.explorer
                    .model
                    .set_expanded(&root.to_string_lossy(), false);
            }
        });
        h.render();
        draw(&h, &mut surface);
        report("collapsed-before-host-clear", &h, &surface);
        h.session.clear_host();
        h.render();
        draw(&h, &mut surface);
        report("collapsed-after-host-clear", &h, &surface);
        drop(surface);
        drop(h);
        println!("EXPLORER_MEMORY dropped {:?}", process_memory());
    }

    /// A live root state plus the services the Explorer commands use. Each
    /// frame renders the real drawer and, when open, its context menu.
    fn harness(app: AppState) -> Harness {
        let initial = Arc::new(Mutex::new(Some(app)));
        let exposed = Arc::new(Mutex::new(None));
        let output = exposed.clone();
        let context = ApplicationContext::empty(Default::default());
        let view: AppView = Arc::new(move |cx| {
            let state =
                AppContext::detached(cx.state_with(|| initial.lock().unwrap().take().unwrap()));
            let git = cx.state(GitStoreSnapshot::default());
            let tabs = crate::app::terminal_binding::test_store(
                cx,
                Some(state.clone()),
                TerminalTabs::new,
            );
            let id = UiId::new("explorer-edit-input");
            let services = Services {
                application: context.clone(),
                editor_focus: cx.focus_handle(UiId::new("editor")),
                terminal_tabs: tabs,
            };
            let drawer = render(
                UiRect::new(500.0, 0.0, 800.0, 600.0),
                &state,
                &git,
                &services,
                EditInput {
                    id: id.clone(),
                    focus: cx.focus_handle(id),
                },
            );
            let mut root = panel(VIEWPORT, Default::default()).child(drawer);
            if let Some(position) = state.get().explorer.ui.menu {
                root = root.child(render_menu(VIEWPORT, position, &state, &services));
            }
            *output.lock().unwrap() = Some((state, services));
            root
        });
        let mut session = UiSession::new();
        session.render_view(&view, VIEWPORT, UiScale::ONE);
        let (state, services) = exposed.lock().unwrap().take().unwrap();
        Harness {
            state,
            services,
            session,
            view,
        }
    }

    impl Harness {
        fn run(&self, command: Command) {
            run(&self.state, &self.services, command);
        }

        fn render(&mut self) {
            self.session.render_view(&self.view, VIEWPORT, UiScale::ONE);
        }
    }

    fn workspace(name: &str, files: &[&str]) -> (PathBuf, AppState) {
        let root = close_test_directory(name);
        for file in files {
            let path = root.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "").unwrap();
        }
        let mut app = AppState::new();
        crate::app::explorer::add_folder(&mut app, root);
        let root = app.explorer.model.roots()[0].clone();
        (root, app)
    }

    fn target(path: PathBuf, kind: ExplorerTargetKind) -> Option<ExplorerContextTarget> {
        Some(ExplorerContextTarget { path, kind })
    }

    #[test]
    fn opening_the_explorer_menu_replaces_the_tab_menu() {
        let (root, mut app) = workspace("explorer-view-menu", &["a.rs"]);
        let id = app
            .workspace
            .test_open_view(root.join("a.rs"), String::new());
        app.editor.tabs.tab_context_menu = Some(TabContextMenuState {
            position: (1.0, 1.0),
            pane: app.workspace.active_pane(),
            target: id,
            hovered: None,
        });
        let mut h = harness(app);
        let menu_target = target(root.join("a.rs"), ExplorerTargetKind::File);

        h.run(Command::OpenMenu {
            point: (600.0, 40.0),
            target: menu_target.clone(),
        });
        h.render();

        let app = h.state.get();
        assert!(app.editor.tabs.tab_context_menu.is_none());
        assert_eq!(app.explorer.ui.menu, Some((600.0, 40.0)));
        assert_eq!(app.explorer.ui.menu_target, menu_target);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn pressing_a_file_twice_starts_a_double_click_drag() {
        let (root, app) = workspace("explorer-view-press", &["a.rs"]);
        let h = harness(app);
        let path = root.join("a.rs");

        for _ in 0..2 {
            h.run(Command::PressFile {
                path: path.clone(),
                point: (520.0, 50.0),
            });
        }

        let drag = h.state.get().editor.tabs.file_drag.clone().unwrap();
        assert_eq!(drag.path, path);
        assert!(matches!(
            drag.source,
            FileDragSource::Explorer { clicks: 2 }
        ));
        assert!(!drag.active);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_goes_to_whichever_edit_row_is_open() {
        let (root, app) = workspace("explorer-view-submit", &["old.rs"]);
        let h = harness(app);

        h.run(Command::Menu {
            action: MenuAction::Rename,
            target: target(root.join("old.rs"), ExplorerTargetKind::File),
        });
        h.state
            .update(|app| app.explorer.ui.create_input.set_text("new.rs".to_owned()));
        h.run(Command::SubmitEdit);
        assert!(root.join("new.rs").is_file());

        h.run(Command::Menu {
            action: MenuAction::NewFolder,
            target: target(root.clone(), ExplorerTargetKind::Directory),
        });
        h.state
            .update(|app| app.explorer.ui.create_input.set_text("docs".to_owned()));
        h.run(Command::SubmitEdit);
        assert!(root.join("docs").is_dir());
        assert!(!h.state.get().explorer.model.editing());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn drawer_width_changes_only_during_a_resize_drag() {
        let (root, app) = workspace("explorer-view-resize", &[]);
        let h = harness(app);
        let before = h.state.get().drawer.width;

        h.run(Command::Resize(400.0));
        assert_eq!(h.state.get().drawer.width, before);
        h.run(Command::ResizeStart);
        h.run(Command::Resize(400.0));
        h.run(Command::ResizeEnd);
        h.run(Command::Resize(300.0));

        let app = h.state.get();
        assert_eq!(app.drawer.width, 400.0);
        assert!(!app.drawer.resizing);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn menu_commands_need_a_target_except_add_folder() {
        let (root, app) = workspace("explorer-view-targets", &[]);
        let h = harness(app);

        h.run(Command::Menu {
            action: MenuAction::RemoveFolder,
            target: None,
        });
        assert_eq!(
            h.state.get().explorer.model.roots(),
            std::slice::from_ref(&root)
        );

        h.run(Command::Menu {
            action: MenuAction::RemoveFolder,
            target: target(root.clone(), ExplorerTargetKind::Directory),
        });
        assert!(h.state.get().explorer.model.roots().is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn preview_markdown_turns_an_open_markdown_file_into_its_preview() {
        let (root, mut app) = workspace("explorer-view-preview", &["notes.md"]);
        let path = root.join("notes.md");
        let id = app.workspace.test_open_view(path.clone(), "# Notes".into());
        let h = harness(app);

        h.run(Command::Menu {
            action: MenuAction::PreviewMarkdown,
            target: target(path, ExplorerTargetKind::File),
        });

        let app = h.state.get();
        assert_eq!(
            app.workspace.view(id).map(|view| view.presentation),
            Some(crate::domain::view::Presentation::Markdown)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn toggling_and_collapsing_render_from_the_projection() {
        let (root, app) = workspace("explorer-view-toggle", &["src/main.rs"]);
        let mut h = harness(app);
        let src = crate::features::explorer::tree::key(&root.join("src"));

        h.run(Command::ToggleDirectory(src.clone()));
        h.render();
        assert!(h.state.get().explorer.model.is_expanded(&src));
        h.state.update(|app| app.explorer.ui.hovered = true);
        h.run(Command::OpenMenu {
            point: (600.0, 40.0),
            target: None,
        });
        h.render();

        h.run(Command::ToggleDirectory(src.clone()));
        h.render();
        assert!(!h.state.get().explorer.model.is_expanded(&src));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn multiple_project_roots_remain_stacked_and_clickable_after_scrolling() {
        use crate::features::explorer::DirEntry;
        use lgui::core::{
            InputEvent, Point, PointerButton, PointerData, WheelDelta, dispatch_runtime_output,
        };

        let roots = ["project-a", "project-b", "project-c"].map(PathBuf::from);
        let mut app = AppState::new();
        app.explorer.model.restore(roots.to_vec(), Vec::new());
        for root in &roots {
            let key = root.to_string_lossy().into_owned();
            app.explorer.model.set_expanded(&key, true);
            app.explorer.model.insert_listing(
                key,
                (0..60)
                    .map(|index| {
                        DirEntry::new(
                            &Arc::from(root.as_path()),
                            format!("file-{index}.rs"),
                            false,
                        )
                    })
                    .collect::<Vec<_>>()
                    .into(),
            );
        }
        let mut h = harness(app);
        let send = |h: &mut Harness, input| {
            let events = h.session.handle_input(input);
            dispatch_runtime_output(
                events,
                &h.services.application,
                &lgui::window::WindowId::new("stacked-projects-test"),
                |action| h.session.handle_default_action(action),
                |_| {},
            );
            h.render();
        };
        let wheel = |y| InputEvent::Wheel {
            point: Point::new(650.0, 400.0),
            delta: WheelDelta::pixels(0.0, y),
        };
        send(&mut h, wheel(-2500.0));
        assert!(h.state.get().explorer.ui.scroll > 2400.0);
        for (index, root) in roots.iter().enumerate() {
            let id = UiId::owned(format!("tree-sticky-{}", root.display()));
            let icon = h.session.tree().node(&id).expect("Pinned project root");
            assert_eq!(
                icon.layout_rect.top,
                crate::ui::theme::TABS_H + index as f32 * 20.0 + 2.0
            );
        }
        send(&mut h, wheel(2500.0));
        assert_eq!(h.state.get().explorer.ui.scroll, 0.0);
        assert!(
            h.session
                .tree()
                .node(&UiId::new("tree-sticky-project-c"))
                .is_none()
        );
        send(&mut h, wheel(-2500.0));
        for root in &roots {
            let key = root.to_string_lossy();
            let icon = h
                .session
                .tree()
                .node(&UiId::owned(format!("tree-sticky-{key}")))
                .expect("Earlier projects stay accessible")
                .layout_rect;
            let point = Point::new(icon.left + 8.0, icon.top + 8.0);
            send(
                &mut h,
                InputEvent::PointerDown {
                    pointer: PointerData::mouse(point),
                    button: PointerButton::Left,
                },
            );
            send(
                &mut h,
                InputEvent::PointerUp {
                    pointer: PointerData::mouse(point),
                    button: PointerButton::Left,
                },
            );
            assert!(!h.state.get().explorer.model.is_expanded(&key));
            assert!(h.state.get().editor.tabs.file_drag.is_none());
        }
    }
}
