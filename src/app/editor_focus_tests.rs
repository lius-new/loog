//! Native key dispatch after a focused Explorer edit row is removed.
use super::*;
use crate::app::key_actions::{self, KeyEnv};
use crate::domain::document::DiskState;
use crate::domain::identity::PaneId;
use crate::domain::workspace::{OpenMode, PreparedTextFile};
use crate::features::git::GitStoreSnapshot;
use crate::ui::components::input::{self, AccessInputOptions, InputBinding, InputStyle};
use lgui::application::{AppView, ApplicationContext};
use lgui::core::{
    ImeEvent, InputEvent, KeyModifiers, KeyState, KeyboardEvent, LogicalKey, NamedKey, Point,
    PointerData, UiId, UiScale, dispatch_runtime_output,
};
use lgui::prelude::group;
use lgui::session::UiSession;
use loog_workspace_api::{EnvironmentKey, ResourceRef};
use std::sync::{Arc, Mutex};

#[test]
fn local_editor_keys_work_after_explorer_edit_unmounts() {
    editor_keys_after_explorer_edit(false);
}

#[test]
fn provided_editor_keys_work_after_explorer_edit_unmounts() {
    editor_keys_after_explorer_edit(true);
}

fn editor_keys_after_explorer_edit(provided: bool) {
    let exposed = Arc::new(Mutex::new(None));
    let output = exposed.clone();
    let application = ApplicationContext::empty(Default::default());
    let environment = application.clone();
    let viewport = UiRect::new(0.0, 0.0, 600.0, 360.0);
    let editor_rect = UiRect::new(0.0, 60.0, 600.0, 360.0);
    let view: AppView = Arc::new(move |cx| {
        let state = AppContext::detached(cx.state_with(|| {
            let mut app = AppState::new();
            let resource = if provided {
                ResourceRef::provided(
                    EnvironmentKey::new("fixture", "keyboard").unwrap(),
                    "/work/new.txt",
                )
                .unwrap()
            } else {
                ResourceRef::local("new.txt")
            };
            app.workspace.open_prepared_text(
                PreparedTextFile::with_resource(
                    resource,
                    "new.txt".into(),
                    String::new(),
                    DiskState::from_contents(""),
                ),
                OpenMode::Permanent,
            );
            app
        }));
        let editing = cx.state(true);
        let git = cx.state(GitStoreSnapshot::default());
        *output.lock().unwrap() = Some(state.clone());
        let id = UiId::new("focus-test-editor");
        let focus = cx.focus_handle(id.clone());
        let env = KeyEnv {
            state: state.clone(),
            git_store: git.clone(),
            application: environment.clone(),
            editor_focus: focus.clone(),
            editor_rect,
            terminal: None,
        };
        let pane = PaneId::new(1);
        let binding = crate::app::editor_view::binding(state.clone(), git, pane, editor_rect);
        let mut root = group(viewport).child(render(pane, editor_rect, binding, id, focus));
        if editing.get() {
            let submit_state = state.clone();
            let input_id = UiId::new("focus-test-explorer-input");
            root = root.child(input::render_access(
                UiRect::new(10.0, 10.0, 300.0, 40.0),
                input_id.clone(),
                cx.focus_handle(input_id),
                InputBinding::new_access(
                    (*state).clone().into(),
                    |app| &app.explorer.ui.create_input,
                    |app| &mut app.explorer.ui.create_input,
                ),
                AccessInputOptions {
                    label: "File name",
                    placeholder: "",
                    style: InputStyle {
                        text: theme::mono(theme::c().text, theme::UI_SIZE),
                        placeholder: theme::mono(theme::c().text_faint, theme::UI_SIZE),
                        caret: theme::c().text,
                        selection: theme::c().accent,
                    },
                    on_submit: Some(Arc::new(move || {
                        // Both local and provider operations clear the edit row
                        // before it disappears from the next UI tree.
                        submit_state.update(|app| app.explorer.ui.clear_edit());
                        editing.update(|visible| *visible = false);
                    })),
                    on_cancel: None,
                    on_blur: None,
                },
            ));
        }
        key_actions::attach(root, env)
    });
    let mut session = UiSession::new();
    session.render_view(&view, viewport, UiScale::ONE);
    let state = exposed.lock().unwrap().clone().unwrap();
    let mut send = |input| {
        dispatch_runtime_output(
            session.handle_input(input),
            &application,
            &lgui::window::WindowId::new("editor-focus-test"),
            |action| session.handle_default_action(action),
            |_| {},
        );
        session.render_view(&view, viewport, UiScale::ONE);
    };
    let key = |key, modifiers| {
        InputEvent::Keyboard(KeyboardEvent {
            state: KeyState::Down,
            key,
            modifiers,
            ..Default::default()
        })
    };
    let named = |name| key(LogicalKey::Named(name), KeyModifiers::empty());
    let text = || {
        state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .text()
            .to_owned()
    };
    for button in [true, false] {
        let pointer = PointerData::mouse(Point::new(30.0, 20.0));
        send(if button {
            InputEvent::PointerDown {
                pointer,
                button: PointerButton::Left,
            }
        } else {
            InputEvent::PointerUp {
                pointer,
                button: PointerButton::Left,
            }
        });
    }
    assert!(state.get().explorer.ui.create_input.focused);
    send(InputEvent::Ime(ImeEvent::Commit("new.txt".into())));
    send(named(NamedKey::Enter));
    for button in [true, false] {
        let pointer = PointerData::mouse(Point::new(
            fonts::editor().gutter_w() + theme::CODE_PAD + 1.0,
            65.0,
        ));
        send(if button {
            InputEvent::PointerDown {
                pointer,
                button: PointerButton::Left,
            }
        } else {
            InputEvent::PointerUp {
                pointer,
                button: PointerButton::Left,
            }
        });
    }
    assert!(state.get().editor.focused);
    send(InputEvent::Ime(ImeEvent::Commit("abc".into())));
    assert_eq!(text(), "abc");
    send(named(NamedKey::Backspace));
    assert_eq!(text(), "ab", "provided={provided}");
    send(named(NamedKey::ArrowLeft));
    send(named(NamedKey::Enter));
    assert_eq!(text(), "a\nb");
    send(named(NamedKey::ArrowUp));
    assert_eq!(state.get().workspace.active_editor().unwrap().cursor(), 0);
    send(named(NamedKey::ArrowDown));
    send(named(NamedKey::ArrowRight));
    assert_eq!(state.get().workspace.active_editor().unwrap().cursor(), 3);
    send(named(NamedKey::ArrowLeft));
    send(named(NamedKey::Delete));
    assert_eq!(text(), "a\n");
    let secondary = if cfg!(target_os = "macos") {
        KeyModifiers::META
    } else {
        KeyModifiers::CONTROL
    };
    send(key(LogicalKey::Character("z".into()), secondary));
    assert_eq!(text(), "a\nb");
    send(key(LogicalKey::Character("a".into()), secondary));
    send(named(NamedKey::Backspace));
    assert_eq!(text(), "");
}
