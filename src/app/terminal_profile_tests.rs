#[cfg(test)]
mod tests {
    use crate::app::context::AppContext;
    use crate::app::state::AppState;
    use crate::app::terminal_profiles::*;
    use crate::features::terminal::{ShellKind, profiles::ShellProfile};
    use crate::ui::terminal::settings::render;
    use crate::ui::theme;
    use lgui::prelude::{UiRect, VisualStyle, panel};

    #[test]
    fn profile_editor_renders_at_normal_and_minimized_sizes() {
        use lgui::application::AppView;
        use lgui::core::{PhysicalRect, UiScale};
        use lgui::render_skia::SkiaSoftwareSurface;
        use lgui::renderer::{FrameInfo, FrameReason};
        use lgui::session::UiSession;
        use std::sync::Arc;
        for (width, height) in [(720.0, 650.0), (80.0, 80.0)] {
            let viewport = UiRect::new(0.0, 0.0, width, height);
            let view: AppView = Arc::new(move |cx| {
                let state = AppContext::detached(cx.state_with(|| {
                    let mut app = AppState::new();
                    let profile = ShellProfile {
                        id: 99,
                        name: "Developer shell".into(),
                        program: std::env::current_exe().unwrap(),
                        args: vec!["--login".into()],
                    };
                    app.preferences.values_mut().terminal_profiles.push(profile);
                    app.refresh_shells();
                    edit(&mut app, Some(99));
                    app
                }));
                panel(viewport, VisualStyle::filled(theme::c().bg)).child(render(
                    UiRect::new(
                        12.0,
                        0.0,
                        (width - 12.0).max(12.0),
                        crate::ui::terminal::settings::height(
                            &settings_frame(&state.get()),
                            &state.get().terminal.ui,
                        ),
                    ),
                    binding(&state),
                ))
            });
            let mut session = UiSession::new();
            session.render_view(&view, viewport, UiScale::ONE);
            let physical = PhysicalRect::new(0, 0, width as i32, height as i32);
            let dirty = [physical];
            let frame = FrameInfo::new(
                physical,
                &dirty,
                UiScale::ONE,
                FrameReason::SceneChange,
                true,
            );
            let mut surface = SkiaSoftwareSurface::new(16 * 1024 * 1024);
            surface
                .draw(
                    &session.tree().scene().project_to_physical(UiScale::ONE),
                    &frame,
                )
                .unwrap();
            if width == 720.0
                && let Some(path) = std::env::var_os("LOOG_PROFILE_PREVIEW")
            {
                let mut rgba = surface.pixels().to_vec();
                for pixel in rgba.as_chunks_mut::<4>().0 {
                    pixel.swap(0, 2);
                }
                image::save_buffer(
                    path,
                    &rgba,
                    width as u32,
                    height as u32,
                    image::ColorType::Rgba8,
                )
                .unwrap();
            }
        }
    }

    #[test]
    fn profiles_can_be_added_edited_selected_removed_and_repaired() {
        let mut app = AppState::new();
        edit(&mut app, None);
        assert!(!save_editor(&mut app));
        let editor = app.terminal.ui.shell_editor.as_mut().unwrap();
        editor.name.set_text("Custom shell");
        editor
            .program
            .set_text(std::env::current_exe().unwrap().display().to_string());
        editor.args.set_text(r#"--login "argument with spaces" """#);
        assert!(save_editor(&mut app));
        let original = app.preferences.values().terminal_profiles[0].clone();
        assert_eq!(original.args, vec!["--login", "argument with spaces", ""]);
        let id = original.id;
        app.preferences.values_mut().default_shell = ShellKind::Custom(id);
        edit(&mut app, Some(id));
        app.terminal
            .ui
            .shell_editor
            .as_mut()
            .unwrap()
            .name
            .set_text("Renamed shell");
        assert!(save_editor(&mut app));
        assert_eq!(app.preferences.values().terminal_profiles.len(), 1);
        assert_eq!(app.preferences.values().terminal_profiles[0].id, id);
        assert_eq!(
            app.preferences.values().default_shell,
            ShellKind::Custom(id)
        );
        app.preferences.values_mut().terminal_profiles[0].program = "/missing-loog-shell".into();
        app.refresh_shells();
        assert!(
            app.preferences
                .values()
                .shell_options
                .iter()
                .any(|p| p.kind == ShellKind::Custom(id) && !p.available())
        );
        // The saved choice survives while another shell stands in for it.
        assert_eq!(
            app.preferences.values().default_shell,
            ShellKind::Custom(id)
        );
        assert_ne!(app.effective_shell(), ShellKind::Custom(id));
        assert!(app.terminal.availability.fallback().is_some());
        edit(&mut app, Some(id));
        app.terminal
            .ui
            .shell_editor
            .as_mut()
            .unwrap()
            .program
            .set_text(std::env::current_exe().unwrap().display().to_string());
        assert!(save_editor(&mut app));
        assert_eq!(app.effective_shell(), ShellKind::Custom(id));
        assert_eq!(app.terminal.availability.fallback(), None);
        remove(&mut app, id);
        assert_eq!(app.preferences.values().default_shell, ShellKind::default());
        assert!(app.preferences.values().terminal_profiles.is_empty());
        assert!(!app.terminal.ui.shell_editor.as_ref().unwrap().open);
    }

    #[test]
    fn profile_inputs_save_and_unmount_without_losing_focus_state_or_bubbling_delete() {
        use lgui::application::{AppView, ApplicationContext};
        use lgui::core::{
            InputEvent, Point, PointerButton, PointerData, UiScale, dispatch_runtime_output,
        };
        use lgui::session::UiSession;
        use std::sync::{Arc, Mutex};
        let exposed = Arc::new(Mutex::new(None::<AppContext>));
        let output = exposed.clone();
        let viewport = UiRect::new(0.0, 0.0, 720.0, 600.0);
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(|| {
                let mut app = AppState::new();
                app.preferences.values_mut().shell_options.clear();
                edit(&mut app, None);
                app
            }));
            *output.lock().unwrap() = Some(state.clone());
            render(
                UiRect::new(
                    12.0,
                    0.0,
                    708.0,
                    crate::ui::terminal::settings::height(
                        &settings_frame(&state.get()),
                        &state.get().terminal.ui,
                    ),
                ),
                binding(&state),
            )
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        session.runtime().run_effects();
        let state = exposed.lock().unwrap().clone().unwrap();
        let context = ApplicationContext::empty(Default::default());
        fn send(
            session: &mut UiSession,
            context: &ApplicationContext,
            view: &AppView,
            viewport: UiRect,
            event: InputEvent,
        ) {
            let events = session.handle_input(event);
            dispatch_runtime_output(
                events,
                context,
                &lgui::window::WindowId::new("terminal-settings-test"),
                |action| session.handle_default_action(action),
                |_| {},
            );
            session.render_view(view, viewport, UiScale::ONE);
            session.runtime().run_effects();
        }
        fn click(
            session: &mut UiSession,
            context: &ApplicationContext,
            view: &AppView,
            viewport: UiRect,
            label: &str,
        ) {
            let node = session
                .tree()
                .nodes()
                .iter()
                .find(|n| n.text.as_deref() == Some(label))
                .unwrap_or_else(|| panic!("missing label {label}"));
            let point = Point::new(
                (node.layout_rect.left + node.layout_rect.right) * 0.5,
                (node.layout_rect.top + node.layout_rect.bottom) * 0.5,
            );
            for event in [
                InputEvent::PointerDown {
                    pointer: PointerData::mouse(point),
                    button: PointerButton::Left,
                },
                InputEvent::PointerUp {
                    pointer: PointerData::mouse(point),
                    button: PointerButton::Left,
                },
            ] {
                send(session, context, view, viewport, event);
            }
        }
        click(&mut session, &context, &view, viewport, "My shell");
        send(
            &mut session,
            &context,
            &view,
            viewport,
            InputEvent::TextInput("UI shell".into()),
        );
        assert_eq!(
            state
                .get()
                .terminal
                .ui
                .shell_editor
                .as_ref()
                .unwrap()
                .name
                .text(),
            "UI shell"
        );
        state.update(|app| {
            let draft = app.terminal.ui.shell_editor.as_mut().unwrap();
            draft
                .program
                .set_text(std::env::current_exe().unwrap().display().to_string());
            draft.args.set_text("--login");
        });
        session.render_view(&view, viewport, UiScale::ONE);
        click(&mut session, &context, &view, viewport, "Save");
        assert_eq!(state.get().preferences.values().terminal_profiles.len(), 1);
        let id = state.get().preferences.values().terminal_profiles[0].id;
        state.update(|app| app.preferences.values_mut().default_shell = ShellKind::Custom(id));
        session.render_view(&view, viewport, UiScale::ONE);
        // Editing replaces the row with its form; the form's title confirms it.
        click(&mut session, &context, &view, viewport, "Edit");
        assert!(
            session
                .tree()
                .nodes()
                .iter()
                .any(|n| n.text.as_deref() == Some("Edit terminal"))
        );
        click(&mut session, &context, &view, viewport, "Cancel");
        click(&mut session, &context, &view, viewport, "Remove");
        assert!(
            state
                .get()
                .preferences
                .values()
                .terminal_profiles
                .is_empty()
        );
        assert_ne!(
            state.get().preferences.values().default_shell,
            ShellKind::Custom(id)
        );
    }
}
