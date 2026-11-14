use crate::app::context::AppContext;
use crate::app::state::AppState;
use crate::app::terminal_runtime::TerminalDisplay;
use crate::features::terminal::actions::MenuAction;
// Application command regressions against the real model, layout and weak runtime binding.
use super::terminal::*;
use crate::app::terminal_binding::{TerminalStore, TerminalTabs};
use crate::domain::application_layout::{ApplicationLayout, Content, Placement};
use crate::features::terminal::ShellKind;
use lgui::application::AppView;
use lgui::core::UiScale;
use lgui::prelude::{UiRect, VisualStyle, panel};
use lgui::services::{Clipboard, ClipboardError};
use lgui::session::UiSession;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub(super) fn fixture() -> (UiSession, AppContext, TerminalStore) {
    let exposed = Arc::new(Mutex::new(None));
    let out = exposed.clone();
    let rect = UiRect::new(0.0, 0.0, 800.0, 600.0);
    let view: AppView = Arc::new(move |cx| {
        let state = AppContext::detached(cx.state_with(|| {
            let mut app = AppState::new();
            app.terminal_chrome.show_terminal = true;
            app.terminal.ui.terminal_focused = true;
            app.layout.application_layout =
                Some(ApplicationLayout::initial(rect.into(), 100.0, 100.0, 200.0));
            app
        }));
        let tabs =
            crate::app::terminal_binding::test_store(cx, Some(state.clone()), TerminalTabs::new);
        let snapshot = tabs.get();
        state.update(|app| snapshot.sync_layout(app.layout.application_layout.as_mut().unwrap()));
        *out.lock().unwrap() = Some((state, tabs));
        panel(rect, VisualStyle::default())
    });
    let mut session = UiSession::new();
    session.render_view(&view, rect, UiScale::ONE);
    let (state, tabs) = exposed.lock().unwrap().clone().unwrap();
    (session, state, tabs)
}
fn application() -> Arc<lgui::ApplicationHandle> {
    Arc::new(lgui::ApplicationHandle::new(|task| task(), || {}))
}
fn invariant(state: &AppContext, tabs: &TerminalStore) {
    let app = state.get();
    let model = tabs.get();
    let layout = app.layout.application_layout.as_ref().unwrap();
    let mut owned = layout
        .leaves()
        .into_iter()
        .flat_map(|(_, content)| match content {
            Content::Terminal { tabs, active } => {
                assert!(active.is_none_or(|id| tabs.contains(&id)));
                tabs
            }
            _ => vec![],
        })
        .collect::<Vec<_>>();
    owned.sort_unstable();
    let mut ids = model.tabs().iter().map(|t| t.id).collect::<Vec<_>>();
    ids.sort_unstable();
    assert_eq!(owned, ids);
    assert!(model.active_id().is_none_or(|id| ids.contains(&id)));
}

#[test]
fn provider_terminal_new_and_split_keep_identity_and_layout_ownership() {
    use crate::features::terminal::model::ProvidedTerminal;
    use loog_workspace_api::{EnvironmentKey, ProviderPath, ResourceRef};
    let (_session, state, tabs) = fixture();
    let resource = ProvidedTerminal {
        root: ResourceRef::provided(
            EnvironmentKey::new("example.provider", "one").unwrap(),
            "/work",
        )
        .unwrap(),
        cwd: ProviderPath::new("/work/中文").unwrap(),
        name: "Example".into(),
    };
    let first = new_provided(&state, &tabs, resource.clone(), None);
    let mut resource = resource;
    resource.cwd = ProviderPath::new("/work/changed").unwrap();
    target(&tabs, first)
        .unwrap()
        .controller
        .test_controller()
        .test_report_directory(Some(resource.cwd.clone()));
    let second = new_from_target(&state, &tabs, first, None).unwrap();
    let third = split(&state, &tabs, first).unwrap();
    for id in [first, second, third] {
        let snapshot = tabs.get();
        let tab = snapshot
            .model()
            .tabs()
            .iter()
            .find(|tab| tab.id() == id)
            .unwrap();
        assert_eq!(tab.provided(), Some(&resource));
        assert!(tab.cwd().is_none());
    }
    invariant(&state, &tabs);
    target(&tabs, first)
        .unwrap()
        .controller
        .test_controller()
        .test_report_directory(None);
    let fourth = new_from_target(&state, &tabs, first, None).unwrap();
    assert_eq!(
        target(&tabs, fourth)
            .unwrap()
            .resource_target()
            .unwrap()
            .cwd
            .as_str(),
        "/work"
    );
    close(&state, &tabs, first, MenuAction::All, &application());
    invariant(&state, &tabs);
}

#[test]
fn empty_terminal_panel_uses_the_active_resource_and_never_falls_back_locally_when_offline() {
    use crate::domain::workspace::{OpenMode, PreparedTextFile};
    use crate::ui::terminal::state::PanelTab;
    use loog_workspace_api::{Capability, EnvironmentKey, PresentedRoot, ResourceRef};
    let (_session, state, tabs) = fixture();
    let environment = EnvironmentKey::new("example.provider", "one").unwrap();
    let root = ResourceRef::provided(environment.clone(), "/work").unwrap();
    state.update(|app| {
        let prepared = PreparedTextFile::offline(
            ResourceRef::provided(environment, "/work/file.txt").unwrap(),
            "file.txt".into(),
            None,
        )
        .with_provider_root(Some(root.clone()));
        app.workspace
            .open_prepared_text(prepared, OpenMode::Permanent);
        app.explorer.resources.sync(vec![PresentedRoot {
            scope: 1,
            resource: root.clone(),
            name: "Example".into(),
            capabilities: vec![Capability::Terminal],
            badge: None,
        }]);
    });
    let initial = tabs.get().active_id().unwrap();
    close(&state, &tabs, initial, MenuAction::All, &application());
    select_panel(&state, &tabs, PanelTab::Terminal);
    let provided = tabs.get().active().unwrap();
    assert_eq!(provided.resource_target().unwrap().root, root);
    close(&state, &tabs, provided.id, MenuAction::All, &application());
    state.update(|app| app.explorer.resources.sync(vec![]));
    select_panel(&state, &tabs, PanelTab::Terminal);
    assert!(tabs.get().is_empty());
    invariant(&state, &tabs);
}

#[test]
fn bottom_categories_keep_sessions_and_allow_history_without_a_shell() {
    use crate::ui::terminal::state::PanelTab;
    let (_session, state, tabs) = fixture();
    let original = tabs.get().active_id().unwrap();
    let sibling = split(&state, &tabs, original).unwrap();
    let handle = target(&tabs, original)
        .unwrap()
        .controller
        .test_controller();
    handle.test_parser().process(b"preserved output\r\n");
    let groups = tabs.get().saved_groups();
    let generation = handle.test_generation();
    state.update(|app| app.git.ui.details_scroll = 44.0);
    select_panel(&state, &tabs, PanelTab::Commits);
    assert!(state.get().git_panel_visible());
    assert!(!state.get().terminal.ui.terminal_focused);
    assert_eq!(tabs.get().saved_groups(), groups);
    assert_eq!(state.get().git.ui.details_scroll, 44.0);
    handle.test_parser().process(b"background output\r\n");
    select_panel(&state, &tabs, PanelTab::Terminal);
    assert!(!state.get().git_panel_visible());
    assert_eq!(tabs.get().active_id(), Some(sibling));
    assert_eq!(handle.test_generation(), generation);
    assert!(
        handle
            .test_parser()
            .screen()
            .contents()
            .contains("background output")
    );
    assert!(close(
        &state,
        &tabs,
        original,
        MenuAction::All,
        &application()
    ));
    select_panel(&state, &tabs, PanelTab::Commits);
    assert!(tabs.get().is_empty());
    assert!(state.get().default_terminal_open());
    assert!(collapse_panel(&state, &tabs));
    assert!(!state.get().git_panel_visible());
    select_panel(&state, &tabs, PanelTab::Commits);
    assert!(tabs.get().is_empty());
    select_panel(&state, &tabs, PanelTab::Terminal);
    assert_eq!(tabs.get().tabs().len(), 1);
    invariant(&state, &tabs);
}

#[test]
fn menu_new_uses_workspace_root_while_split_inherits_target_live_directory() {
    let (_session, state, tabs) = fixture();
    state.update(|app| {
        app.explorer
            .model
            .restore(vec!["workspace-root".into()], vec![]);
    });
    let clicked = new(&state, &tabs, ShellKind::Cmd, Some("clicked-cwd".into()));
    let controller = target(&tabs, clicked).unwrap().controller.test_controller();
    *controller.test_cwd() = Some("live-cwd".into());
    let active = new(&state, &tabs, ShellKind::Bash, Some("active-cwd".into()));
    assert_eq!(tabs.get().active_id(), Some(active));
    assert!(menu_action(
        &state,
        &tabs,
        clicked,
        MenuAction::New,
        &application()
    ));
    let created = tabs.get().active().unwrap();
    assert_eq!(created.controller.shell(), ShellKind::Cmd);
    assert_eq!(created.cwd, Some("workspace-root".into()));
    select(&state, &tabs, active);
    assert!(menu_action(
        &state,
        &tabs,
        clicked,
        MenuAction::Split,
        &application()
    ));
    let sibling = tabs.get().active().unwrap();
    assert_eq!(sibling.group_id, target(&tabs, clicked).unwrap().group_id);
    {
        let app = state.get();
        let layout = app.layout.application_layout.as_ref().unwrap();
        let (beside, own) = (
            layout.terminal_region(clicked).unwrap(),
            layout.terminal_region(sibling.id).unwrap(),
        );
        assert_eq!(beside, own);
        let (ids, active) = layout.terminal_view(own).unwrap();
        let index = ids.iter().position(|id| *id == clicked).unwrap();
        assert_eq!(ids[index + 1], sibling.id);
        assert_eq!(tabs.get().region_view(&ids, active).active_group().len(), 2);
    }
    assert_eq!(sibling.cwd, Some("live-cwd".into()));
    assert!(menu_action(
        &state,
        &tabs,
        clicked,
        MenuAction::Others,
        &application()
    ));
    // "Close Others" preserves both sides of the shared tab.
    assert_eq!(
        tabs.get().tabs().iter().map(|t| t.id).collect::<Vec<_>>(),
        vec![clicked, sibling.id]
    );
    assert!(
        controller.test_same_runtime(&target(&tabs, clicked).unwrap().controller.test_controller())
    );
    assert!(close(
        &state,
        &tabs,
        clicked,
        MenuAction::Close,
        &application()
    ));
    assert_eq!(tabs.get().active_id(), Some(sibling.id));
    assert_eq!(tabs.get().active_group().len(), 1);
    invariant(&state, &tabs);
}

#[test]
fn closed_targets_cannot_select_create_split_close_or_send_input_to_survivor() {
    let (_session, state, tabs) = fixture();
    let closed = tabs.get().active_id().unwrap();
    let survivor = new(&state, &tabs, ShellKind::Cmd, None);
    let control = target(&tabs, survivor)
        .unwrap()
        .controller
        .test_controller();
    let app = application();
    assert!(close(&state, &tabs, closed, MenuAction::Close, &app));
    for action in [
        MenuAction::New,
        MenuAction::Split,
        MenuAction::Close,
        MenuAction::Others,
        MenuAction::Right,
        MenuAction::All,
    ] {
        assert!(!menu_action(&state, &tabs, closed, action, &app));
    }
    assert!(!select(&state, &tabs, closed));
    assert!(new_from_target(&state, &tabs, closed, Some(ShellKind::Bash)).is_none());
    assert!(split(&state, &tabs, closed).is_none());
    assert!(!write(&tabs, closed, b"stale"));
    let clip = TestClipboard::default();
    clipboard(&tabs, closed, ClipboardShortcut::CopyOrInterrupt, &clip).unwrap();
    assert_eq!(*clip.calls.lock().unwrap(), 0);
    assert!(control.test_writes().is_empty());
    assert_eq!(tabs.get().active_id(), Some(survivor));
    assert_eq!(tabs.get().tabs().len(), 1);
    invariant(&state, &tabs);
}

#[test]
fn ordinary_new_uses_current_workspace_without_overwriting_restored_directories() {
    let (_session, state, tabs) = fixture();
    let restored = tabs.get().active_id().unwrap();
    tabs.update(|tabs| tabs.restore_directories(&[Some("remembered-cwd".into())]));
    for roots in [
        vec![PathBuf::from("workspace-a")],
        vec![PathBuf::from("workspace-c")],
        vec![],
    ] {
        let expected = roots.first().cloned();
        state.update(|app| app.explorer.model.restore(roots, vec![]));
        let created = new_from_target(&state, &tabs, restored, Some(ShellKind::Bash)).unwrap();
        assert_eq!(target(&tabs, created).unwrap().cwd, expected);
        assert_eq!(
            target(&tabs, created).unwrap().controller.shell(),
            ShellKind::Bash
        );
        assert_eq!(
            target(&tabs, restored).unwrap().working_directory(),
            Some("remembered-cwd".into())
        );
        invariant(&state, &tabs);
    }
    let sibling = split(&state, &tabs, restored).unwrap();
    assert_eq!(
        target(&tabs, sibling).unwrap().cwd,
        Some("remembered-cwd".into())
    );
    invariant(&state, &tabs);
}

#[test]
fn active_file_selects_its_innermost_workspace_before_remembered_choice() {
    let (_session, state, tabs) = fixture();
    let source = tabs.get().active_id().unwrap();
    state.update(|app| {
        app.explorer.model.restore(
            vec![
                "project-a".into(),
                "project-b".into(),
                "project-b/nested".into(),
            ],
            vec![],
        );
        app.terminal.ui.terminal_workspace_root = Some("project-a".into());
    });
    for (file, root) in [
        ("project-b/src/main.rs", "project-b"),
        ("project-b/nested/main.rs", "project-b/nested"),
        ("project-a/lib.rs", "project-a"),
    ] {
        state.update(|app| {
            app.workspace.open_path(file.into(), String::new());
        });
        let id = new_from_target(&state, &tabs, source, None).unwrap();
        assert_eq!(target(&tabs, id).unwrap().cwd, Some(root.into()));
        assert!(state.get().terminal.ui.terminal_workspace_picker.is_none());
    }
    invariant(&state, &tabs);
}

#[test]
fn ambiguous_new_waits_for_choice_remembers_it_and_rejects_removed_roots() {
    let (_session, state, tabs) = fixture();
    let source = tabs.get().active_id().unwrap();
    state.update(|app| {
        app.explorer
            .model
            .restore(vec!["project-a".into(), "project-b".into()], vec![])
    });
    assert!(new_from_target(&state, &tabs, source, Some(ShellKind::Bash)).is_none());
    assert_eq!(tabs.get().tabs().len(), 1);
    assert!(choose_workspace(&state, &tabs, "removed".into()).is_none());
    let id = choose_workspace(&state, &tabs, "project-b".into()).unwrap();
    assert_eq!(target(&tabs, id).unwrap().cwd, Some("project-b".into()));
    assert_eq!(
        target(&tabs, id).unwrap().controller.shell(),
        ShellKind::Bash
    );
    assert!(state.get().terminal.ui.terminal_workspace_picker.is_none());
    let next = new_from_target(&state, &tabs, id, None).unwrap();
    assert_eq!(target(&tabs, next).unwrap().cwd, Some("project-b".into()));
    state.update(|app| {
        app.explorer
            .model
            .restore(vec!["project-a".into(), "project-c".into()], vec![])
    });
    assert!(new_from_target(&state, &tabs, next, None).is_none());
    assert!(state.get().terminal.ui.terminal_workspace_picker.is_some());
    assert!(choose_workspace(&state, &tabs, "project-b".into()).is_none());
    invariant(&state, &tabs);
}

#[test]
fn closing_last_session_prunes_layout_stops_runtime_and_hides_default_region() {
    let (_session, state, tabs) = fixture();
    let id = tabs.get().active_id().unwrap();
    let handle = target(&tabs, id).unwrap().controller;
    let control = handle.test_controller();
    assert!(close(&state, &tabs, id, MenuAction::Close, &application()));
    assert!(tabs.get().is_empty());
    assert!(!state.get().terminal_chrome.show_terminal);
    assert!(!state.get().terminal.ui.terminal_focused);
    assert!(matches!(
        control.snapshot().status,
        crate::features::terminal::TerminalStatus::Stopped
    ));
    assert!(matches!(
        handle.snapshot().status,
        crate::features::terminal::TerminalStatus::Stopped
    ));
    assert!(!close(&state, &tabs, id, MenuAction::All, &application()));
    invariant(&state, &tabs);
}

#[derive(Default)]
struct TestClipboard {
    text: Mutex<Option<String>>,
    calls: Mutex<usize>,
    fail: bool,
}
impl Clipboard for TestClipboard {
    fn read_text(&self) -> Result<Option<String>, ClipboardError> {
        *self.calls.lock().unwrap() += 1;
        if self.fail {
            Err(ClipboardError::AccessDenied)
        } else {
            Ok(self.text.lock().unwrap().clone())
        }
    }
    fn write_text(&self, text: &str) -> Result<(), ClipboardError> {
        *self.calls.lock().unwrap() += 1;
        if self.fail {
            return Err(ClipboardError::AccessDenied);
        }
        *self.text.lock().unwrap() = Some(text.into());
        Ok(())
    }
}

#[test]
fn clipboard_failure_preserves_selection_and_emits_neither_interrupt_nor_paste_bytes() {
    let (_session, state, tabs) = fixture();
    let selected = tabs.get().active_id().unwrap();
    let controller = target(&tabs, selected)
        .unwrap()
        .controller
        .test_controller();
    controller.test_parser().process(b"selected text");
    begin_selection(&tabs, selected, 0, 0);
    extend_selection(&tabs, selected, 0, 8);
    let before = controller.test_selection();
    let other = new(&state, &tabs, ShellKind::Cmd, None);
    let fail = TestClipboard {
        fail: true,
        ..Default::default()
    };
    for shortcut in [
        ClipboardShortcut::Copy,
        ClipboardShortcut::CopyOrInterrupt,
        ClipboardShortcut::CopyOrPaste,
        ClipboardShortcut::Paste,
    ] {
        assert!(clipboard(&tabs, selected, shortcut, &fail).is_err());
        assert_eq!(controller.test_selection(), before);
        assert!(controller.test_writes().is_empty());
    }
    let success = TestClipboard::default();
    clipboard(
        &tabs,
        selected,
        ClipboardShortcut::CopyOrInterrupt,
        &success,
    )
    .unwrap();
    assert_eq!(success.text.lock().unwrap().as_deref(), Some("selected"));
    assert!(controller.test_selection().is_none());
    assert!(controller.test_writes().is_empty());
    clipboard(
        &tabs,
        selected,
        ClipboardShortcut::CopyOrInterrupt,
        &success,
    )
    .unwrap();
    controller.test_parser().process(b"\x1b[?2004h");
    *success.text.lock().unwrap() = Some("literal\r\n$HOME".into());
    clipboard(&tabs, selected, ClipboardShortcut::Paste, &success).unwrap();
    write(&tabs, selected, b"replayed");
    assert_eq!(
        controller.test_writes(),
        vec![
            b"\x03".to_vec(),
            b"\x1b[200~literal\r$HOME\x1b[201~".to_vec(),
            b"replayed".to_vec()
        ]
    );
    assert!(
        target(&tabs, other)
            .unwrap()
            .controller
            .test_controller()
            .test_writes()
            .is_empty()
    );
}

#[test]
fn detached_session_selection_and_close_preserve_layout_ownership_and_runtime() {
    let (_session, state, tabs) = fixture();
    let id = tabs.get().active_id().unwrap();
    let other = new(&state, &tabs, ShellKind::Cmd, None);
    let before = target(&tabs, other).unwrap().controller.test_controller();
    state.update(|app| {
        let visible = visibility(app);
        let layout = app.layout.application_layout.as_mut().unwrap();
        let plan = layout
            .plan(
                other,
                &Placement::Side(1, crate::domain::pane_layout::Direction::Right),
                UiRect::new(0.0, 0.0, 800.0, 600.0).into(),
                visible,
                vec![id, other],
            )
            .unwrap();
        *layout = plan.layout;
    });
    assert!(select(&state, &tabs, other));
    let sibling = split(&state, &tabs, other).unwrap();
    {
        let app = state.get();
        let layout = app.layout.application_layout.as_ref().unwrap();
        assert_eq!(
            layout.terminal_region(other),
            layout.terminal_region(sibling)
        );
        assert_ne!(layout.terminal_region(sibling), layout.terminal_region(id));
    }
    assert!(close(
        &state,
        &tabs,
        sibling,
        MenuAction::Close,
        &application()
    ));
    assert_eq!(tabs.get().active_id(), Some(other));
    assert!(close(&state, &tabs, id, MenuAction::Close, &application()));
    invariant(&state, &tabs);
    assert!(before.test_same_runtime(&target(&tabs, other).unwrap().controller.test_controller()));
    assert!(
        state
            .get()
            .layout
            .application_layout
            .unwrap()
            .terminal_region(other)
            .is_some()
    );
}

#[test]
fn profile_binding_preserves_literal_arguments_project_default_and_closed_draft_for_blur() {
    use crate::app::terminal_profiles::{binding, edit, settings_frame};
    use crate::ui::terminal::settings_binding::Command;
    let (_session, state, _tabs) = fixture();
    let root = std::path::PathBuf::from("project-shell");
    state.update(|app| {
        app.explorer.model.restore(vec![root.clone()], vec![]);
        app.preferences.values_mut().default_shell = ShellKind::Cmd;
        app.preferences.values_mut().project_settings.push((
            root.clone(),
            crate::infrastructure::project_settings::ProjectSettings {
                overrides: [("default_shell".into(), serde_json::json!("bash"))]
                    .into_iter()
                    .collect(),
                errors: vec![],
            },
        ));
        edit(app, None);
        let draft = app.terminal.ui.shell_editor.as_mut().unwrap();
        draft.name.set_text("Literal profile");
        draft
            .program
            .set_text(std::env::current_exe().unwrap().display().to_string());
        draft
            .args
            .set_text(r#""$HOME" "%PATH%" "$(echo never-expanded)" "~" """#);
    });
    let port = binding(&state);
    assert_eq!(
        port.frame().project_shell.as_ref().map(|p| p.0.as_str()),
        Some("project-shell")
    );
    port.run(Command::Save);
    let snapshot = state.get();
    let saved = &snapshot.preferences.values().terminal_profiles[0];
    assert_eq!(
        saved.args,
        ["$HOME", "%PATH%", "$(echo never-expanded)", "~", ""]
    );
    let id = saved.id;
    assert!(!snapshot.terminal.ui.shell_editor.as_ref().unwrap().open);
    // A late blur still updates the retained draft, through the narrow projection.
    port.ui
        .update(|ui| ui.shell_editor.as_mut().unwrap().name.focused = false);
    assert_eq!(
        port.ui.get().shell_editor.unwrap().name.text(),
        "Literal profile"
    );
    port.run(Command::Select(ShellKind::Custom(id)));
    assert_eq!(
        state.get().preferences.values().default_shell,
        ShellKind::Custom(id)
    );
    assert_eq!(
        state.get().settings_at(Some(&root)).default_shell,
        ShellKind::Bash
    );
    assert_eq!(
        settings_frame(&state.get()).project_shell.unwrap().1,
        state.get().shell_name(ShellKind::Bash)
    );
    port.run(Command::Remove(id));
    port.run(Command::Select(ShellKind::Custom(id)));
    assert_eq!(
        state.get().preferences.values().default_shell,
        ShellKind::default()
    );
    assert_eq!(
        state.get().settings_at(Some(&root)).default_shell,
        ShellKind::Bash
    );
}
