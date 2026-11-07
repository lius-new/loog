//! Target-bound editor tab menu operations and projection.
use crate::app::context::AppContext;
use crate::app::state::AppState;
use crate::app::workspace_commands::{self as workspace_actions, TabCloseScope};
use crate::domain::identity::{PaneId, ViewId};
use crate::domain::pane_layout::Direction;
use crate::features::explorer::{ExplorerContextTarget, ExplorerTargetKind};
use crate::ui::chrome::menu_binding::{Binding, Frame};
use crate::ui::tab_context_menu::{self, Action, ENTRIES, Entry};
use lgui::core::UiEventContext;
use lgui::prelude::{Element, UiRect};
use lgui::services::ServicesContextExt;
fn close_scope(action: Action) -> Option<TabCloseScope> {
    match action {
        Action::Close => Some(TabCloseScope::Tab),
        Action::CloseOthers => Some(TabCloseScope::Others),
        Action::CloseRight => Some(TabCloseScope::Right),
        Action::CloseSaved => Some(TabCloseScope::Saved),
        Action::CloseAll => Some(TabCloseScope::All),
        _ => None,
    }
}
fn action_enabled(app: &AppState, pane: PaneId, target: ViewId, action: Action) -> bool {
    let Some(meta) = app.workspace.view_meta(target) else {
        return false;
    };
    if let Some(scope) = close_scope(action) {
        return !workspace_actions::tab_close_targets(app, pane, target, scope).is_empty();
    }
    // Built-in pages have no path to copy or reveal.
    if app.workspace.page(target).is_some() {
        return false;
    }
    match action {
        Action::ToggleMarkdown => crate::app::markdown_view::eligible(app, target),
        Action::ToggleSvg => crate::app::image_preview::eligible(app, target),
        // Only files can be shown in two panes at once.
        Action::SplitRight | Action::SplitDown => app.workspace.is_file(target),
        Action::KeepOpen => app
            .workspace
            .pane(pane)
            .is_some_and(|pane| pane.is_preview(target)),
        Action::ReloadFromDisk => {
            app.workspace
                .view_document(target)
                .is_some_and(|id| app.workspace.has_disk_conflict(id))
                && !app
                    .workspace
                    .view_document(target)
                    .is_some_and(|id| app.workspace.is_missing_on_disk(id))
        }
        Action::KeepEditorVersion => app
            .workspace
            .view_document(target)
            .is_some_and(|id| app.workspace.has_disk_conflict(id)),
        Action::CopyPath => true,
        Action::CopyRelativePath => app.explorer.model.root_for_path(&meta.path).is_some(),
        Action::RevealInTree => {
            !app.workspace.is_diff(target) && app.explorer.model.root_for_path(&meta.path).is_some()
        }
        Action::RevealInFileExplorer => !app.workspace.is_diff(target),
        _ => false,
    }
}
pub fn execute(
    state: &AppContext,
    pane: PaneId,
    target: ViewId,
    action: Action,
    cx: &mut UiEventContext,
) {
    let app = state.get();
    if !app
        .workspace
        .pane(pane)
        .is_some_and(|pane| pane.items().contains(&target))
        || !action_enabled(&app, pane, target, action)
    {
        return;
    }
    let Some(action_path) = app
        .workspace
        .view_meta(target)
        .map(|meta| meta.path.clone())
    else {
        return;
    };
    drop(app);
    state.update(|app| app.editor.tabs.tab_context_menu = None);
    if let Some(scope) = close_scope(action) {
        workspace_actions::request_close_tabs(state, pane, target, scope);
        return;
    }
    match action {
        Action::ToggleSvg => {
            state.update(|app| {
                crate::app::image_preview::run(
                    app,
                    target,
                    crate::ui::image_preview::Command::Toggle,
                );
            });
        }
        Action::ToggleMarkdown => {
            state.update(|app| {
                app.workspace.activate(pane, target);
                app.apply(crate::features::keymap::action::Action::ToggleMarkdown);
            });
        }
        Action::KeepOpen => {
            state.update(|app| {
                app.workspace.promote_preview(pane, target);
            });
        }
        Action::SplitRight | Action::SplitDown => {
            let direction = if action == Action::SplitRight {
                Direction::Right
            } else {
                Direction::Down
            };
            state.update(|app| {
                app.workspace.activate(pane, target);
                app.workspace.split(pane, direction);
            });
        }
        Action::ReloadFromDisk => {
            state.update(|app| {
                if let Err(error) = app
                    .workspace
                    .accept_disk_version(app.workspace.view_document(target).unwrap())
                {
                    app.show_error(error);
                }
            });
        }
        Action::KeepEditorVersion => {
            state.update(|app| {
                app.workspace
                    .keep_editor_version(app.workspace.view_document(target).unwrap());
            });
        }
        Action::CopyPath | Action::CopyRelativePath => {
            crate::app::explorer::copy_path(
                state,
                cx.application().clipboard().as_ref(),
                &action_path,
                action == Action::CopyRelativePath,
            );
        }
        Action::RevealInTree => {
            if let Some(id) = state.get().workspace.view_document(target) {
                crate::app::explorer::reveal_file(state, id);
            }
        }
        Action::RevealInFileExplorer => {
            crate::app::explorer::reveal_item(
                state,
                &ExplorerContextTarget {
                    path: action_path.clone(),
                    kind: ExplorerTargetKind::File,
                },
            );
        }
        _ => {}
    }
}
pub fn binding(state: &AppContext) -> Binding {
    let reader = state.clone();
    let hovering = state.clone();
    let dismissing = state.clone();
    let actions = state.clone();
    Binding::new(
        move || {
            let app = reader.get();
            let menu = app.editor.tabs.tab_context_menu.as_ref()?;
            app.workspace.view_meta(menu.target)?;
            Some(Frame {
                position: menu.position,
                pane: menu.pane,
                target: menu.target,
                hovered: menu.hovered,
                enabled: ENTRIES
                    .iter()
                    .filter_map(|entry| {
                        if let Entry::Item(_, action) = entry {
                            action_enabled(&app, menu.pane, menu.target, *action).then_some(*action)
                        } else {
                            None
                        }
                    })
                    .collect(),
            })
        },
        move |pane, target, next| {
            hovering.try_update(|app| {
                let Some(menu) = app
                    .editor
                    .tabs
                    .tab_context_menu
                    .as_mut()
                    .filter(|menu| menu.pane == pane && menu.target == target)
                else {
                    return false;
                };
                let changed = menu.hovered != next;
                menu.hovered = next;
                changed
            });
        },
        move |pane, target| {
            dismissing.try_update(|app| {
                if app
                    .editor
                    .tabs
                    .tab_context_menu
                    .as_ref()
                    .is_some_and(|menu| menu.pane == pane && menu.target == target)
                {
                    app.editor.tabs.tab_context_menu = None;
                    true
                } else {
                    false
                }
            });
        },
        move |pane, target, action, cx| execute(&actions, pane, target, action, cx),
    )
}
pub fn render(rect: UiRect, state: AppContext) -> Element {
    tab_context_menu::render(rect, binding(&state))
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::text::EditKind;
    use std::path::PathBuf;
    #[test]
    fn svg_toggle_is_enabled_for_svg_source_and_preview_only() {
        let mut app = AppState::new();
        let svg = app
            .workspace
            .test_open_view("logo.SVG".into(), "<svg/>".into());
        let plain = app
            .workspace
            .test_open_view("note.txt".into(), "text".into());
        let pane = app.workspace.active_pane();
        assert!(action_enabled(&app, pane, svg, Action::ToggleSvg));
        assert!(!action_enabled(&app, pane, plain, Action::ToggleSvg));
        crate::app::image_preview::run(&mut app, svg, crate::ui::image_preview::Command::Toggle);
        assert!(action_enabled(&app, pane, svg, Action::ToggleSvg));
        assert_eq!(app.workspace.active(), Some(svg));
    }

    #[test]
    fn close_actions_disable_when_their_scope_is_empty() {
        let mut app = AppState::new();
        let only = app
            .workspace
            .test_open_view(PathBuf::from("workspace/only.rs"), String::new());

        assert!(action_enabled(
            &app,
            app.workspace.active_pane(),
            only,
            Action::Close
        ));
        assert!(!action_enabled(
            &app,
            app.workspace.active_pane(),
            only,
            Action::CloseOthers
        ));
        assert!(!action_enabled(
            &app,
            app.workspace.active_pane(),
            only,
            Action::CloseRight
        ));
        assert!(action_enabled(
            &app,
            app.workspace.active_pane(),
            only,
            Action::CloseSaved
        ));
        assert!(action_enabled(
            &app,
            app.workspace.active_pane(),
            only,
            Action::CloseAll
        ));
    }

    #[test]
    fn keep_open_is_available_only_for_the_preview_tab() {
        let mut app = AppState::new();
        let permanent = app
            .workspace
            .test_open_view(PathBuf::from("workspace/permanent.rs"), String::new());
        let preview = app
            .workspace
            .test_preview_view(PathBuf::from("workspace/preview.rs"), String::new());

        assert!(!action_enabled(
            &app,
            app.workspace.active_pane(),
            permanent,
            Action::KeepOpen
        ));
        assert!(action_enabled(
            &app,
            app.workspace.active_pane(),
            preview,
            Action::KeepOpen
        ));
    }

    #[test]
    fn relative_path_and_tree_actions_require_a_containing_workspace() {
        let mut app = AppState::new();
        let file = app
            .workspace
            .test_open_view(PathBuf::from("workspace/src/main.rs"), String::new());

        assert!(!action_enabled(
            &app,
            app.workspace.active_pane(),
            file,
            Action::CopyRelativePath
        ));
        assert!(!action_enabled(
            &app,
            app.workspace.active_pane(),
            file,
            Action::RevealInTree
        ));

        app.explorer
            .model
            .restore(vec![PathBuf::from("workspace")], Vec::new());
        assert!(action_enabled(
            &app,
            app.workspace.active_pane(),
            file,
            Action::CopyRelativePath
        ));
        assert!(action_enabled(
            &app,
            app.workspace.active_pane(),
            file,
            Action::RevealInTree
        ));
    }

    #[test]
    fn disk_resolution_actions_enable_only_for_a_conflict() {
        let root = std::env::temp_dir().join(format!("loog-tab-conflict-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("note.txt");
        std::fs::write(&path, "before").unwrap();
        let mut app = AppState::new();
        let target = app.workspace.test_open_view(path.clone(), "before".into());
        assert!(!action_enabled(
            &app,
            app.workspace.active_pane(),
            target,
            Action::ReloadFromDisk
        ));
        assert!(!action_enabled(
            &app,
            app.workspace.active_pane(),
            target,
            Action::KeepEditorVersion
        ));

        app.workspace
            .active_editor_mut()
            .unwrap()
            .select_to(usize::MAX, false);
        app.workspace
            .active_editor_mut()
            .unwrap()
            .replace_selection(" editor", EditKind::Typing)
            .unwrap();
        std::fs::write(&path, "after and longer").unwrap();
        app.workspace
            .reconcile_document(app.workspace.test_document(target));
        assert!(action_enabled(
            &app,
            app.workspace.active_pane(),
            target,
            Action::ReloadFromDisk
        ));
        assert!(action_enabled(
            &app,
            app.workspace.active_pane(),
            target,
            Action::KeepEditorVersion
        ));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn every_menu_action_is_enabled_when_its_target_is_available() {
        let mut app = AppState::new();
        app.explorer
            .model
            .restore(vec![PathBuf::from("workspace")], Vec::new());
        let target = app
            .workspace
            .test_open_view(PathBuf::from("workspace/first.rs"), String::new());
        app.workspace
            .test_open_view(PathBuf::from("workspace/second.rs"), String::new());

        for action in [
            Action::Close,
            Action::CloseOthers,
            Action::CloseRight,
            Action::CloseSaved,
            Action::CloseAll,
            Action::CopyPath,
            Action::CopyRelativePath,
            Action::RevealInTree,
            Action::RevealInFileExplorer,
        ] {
            assert!(
                action_enabled(&app, app.workspace.active_pane(), target, action),
                "{action:?}"
            );
        }
    }
}
