#[cfg(test)]
mod tests {
    use crate::app::context::AppContext;
    use crate::app::state::AppState;
    use crate::app::terminal_binding::TerminalTabs;
    use crate::app::terminal_runtime::TerminalDisplay;
    use crate::app::terminal_view::{render, render_terminal_tabs, session_rects};
    use crate::features::terminal::{ShellKind, TerminalRun};
    use crate::ui::terminal::keys::key_bytes;
    use crate::ui::terminal::view::*;
    use crate::ui::{fonts, theme};
    use lgui::ApplicationHandle;
    use lgui::core::{EventPolicy, Point, PointerButton, UiElement, UiId};
    use lgui::core::{KeyState, KeyboardEvent, LogicalKey, NamedKey};
    use lgui::prelude::{Element, UiRect, VisualStyle, group};
    use std::sync::Arc;

    /// The center of the control with this accessible name, so tests follow the
    /// header's layout instead of hard-coding where a button sits.
    fn named_point(session: &lgui::session::UiSession, name: &str) -> Point {
        let node = session
            .tree()
            .nodes()
            .iter()
            .find(|n| n.semantics.as_ref().and_then(|s| s.name.as_deref()) == Some(name))
            .unwrap_or_else(|| panic!("missing control {name}"));
        Point::new(
            (node.layout_rect.left + node.layout_rect.right) * 0.5,
            (node.layout_rect.top + node.layout_rect.bottom) * 0.5,
        )
    }

    #[test]
    fn selection_drag_scrolls_only_outside_the_screen_and_speeds_up_with_distance() {
        let content = UiRect::new(10.0, 100.0, 400.0, 300.0);
        let line = terminal_line_h();
        assert_eq!(selection_scroll_rows(content, 100.0), 0);
        assert_eq!(selection_scroll_rows(content, 300.0), 0);
        assert_eq!(selection_scroll_rows(content, 99.0), 1);
        assert_eq!(selection_scroll_rows(content, 301.0), -1);
        assert_eq!(selection_scroll_rows(content, 100.0 - line * 3.0), 3);
        assert_eq!(selection_scroll_rows(content, 300.0 + line * 3.0), -3);
        assert_eq!(selection_scroll_rows(content, -10_000.0), 8);
    }

    #[test]
    fn terminal_header_rule_and_underline_are_crisp_at_fractional_scales() {
        use lgui::application::{AppView, ApplicationContext};
        use lgui::core::{PhysicalRect, UiScale};
        use lgui::prelude::{Color, panel};
        use lgui::renderer::{FrameInfo, FrameReason};
        use lgui::session::UiSession;

        let context = ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        for factor in [0.75_f32, 1.0, 1.25, 1.5, 2.0] {
            let scale = UiScale::new(factor);
            for fraction in [0.0, 0.25, 0.5, 0.75] {
                for width in [400.0, 640.0] {
                    let top = (80.0 + fraction) / factor;
                    let viewport = UiRect::new(0.0, 0.0, width / factor, top + 180.0);
                    let terminal = UiRect::new(0.0, top, viewport.right, viewport.bottom);
                    let view: AppView = Arc::new(move |cx| {
                        let state = AppContext::detached(cx.state_with(AppState::new));
                        let tabs = crate::app::terminal_binding::test_store(
                            cx,
                            Some(state.clone()),
                            TerminalTabs::new,
                        );
                        let controller = tabs.get().active().unwrap().controller.clone();
                        let id = cx.use_stable_id();
                        let focus = cx.focus_handle(id.clone());
                        let editor_id = cx.use_stable_id();
                        let editor_focus = cx.focus_handle(editor_id);
                        panel(viewport, VisualStyle::filled(theme::c().bg)).child(render(
                            terminal,
                            state,
                            editor_focus,
                            focus,
                            id,
                            controller,
                            tabs,
                            Arc::new(ApplicationHandle::new(|task| task(), || {})),
                            true,
                            viewport.height(),
                        ))
                    });
                    let mut session = UiSession::new();
                    session.render_view(&view, viewport, scale);
                    let physical = scale.physical_rect(viewport);
                    let bottom = scale.physical_value(top + theme::TABS_H);
                    let border_width = scale.physical_length(1.0);
                    let dirty = [PhysicalRect::new(0, bottom - 6, physical.right, bottom + 6)];
                    let mut surface = lgui::render_skia::SkiaSoftwareSurface::new(4 * 1024 * 1024);
                    for full in [true, false] {
                        let frame =
                            FrameInfo::new(physical, &dirty, scale, FrameReason::SceneChange, full);
                        lgui::assets::backend::with_render_resources(
                            &context,
                            lgui::RenderResources::new(),
                            || {
                                surface.draw(
                                    &session.tree().scene().project_to_physical(scale),
                                    &frame,
                                )
                            },
                        )
                        .unwrap();
                        let check = |x: usize, y: i32, color: Color| {
                            let offset = (y as usize * physical.width() as usize + x) * 4;
                            let expected = [
                                color.0 as u8,
                                (color.0 >> 8) as u8,
                                (color.0 >> 16) as u8,
                                255,
                            ];
                            assert_eq!(
                                &surface.pixels()[offset..offset + 4],
                                &expected,
                                "scale={factor}, fraction={fraction}, width={width}, full={full}, x={x}, y={y}"
                            );
                        };
                        for y in bottom - border_width..bottom {
                            check(physical.width() as usize - 4, y, theme::c().border);
                        }
                    }
                    // Decorations must not intercept selecting/dragging the tab at its bottom.
                    let select = session
                        .tree()
                        .node(&UiId::new("terminal-tab-select-1"))
                        .unwrap()
                        .hit_rect;
                    let point = Point::new(
                        select.left + 4.0, // inside the clipped strip, even when it is narrow
                        top + theme::TABS_H - 0.25 / factor,
                    );
                    let hit = session.tree().hit_test(point).unwrap();
                    assert!(hit.id.as_str().starts_with("terminal-tab-select-"));
                    let above = Point::new(point.x, point.y - 6.0 / factor);
                    assert_eq!(
                        session.tree().cursor_at(point),
                        session.tree().cursor_at(above)
                    );
                }
            }
        }
    }
    #[test]
    fn split_tab_rules_render_without_seams_across_themes_and_scales() {
        use lgui::application::{AppView, ApplicationContext};
        use lgui::core::UiScale;
        use lgui::prelude::panel;
        use lgui::renderer::{FrameInfo, FrameReason};
        use lgui::session::UiSession;
        let previous = theme::current();
        let context = ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        for theme_id in theme::ThemeId::all() {
            theme::set(theme_id);
            for factor in [0.75_f32, 1.0, 1.25, 1.5, 2.0] {
                let scale = UiScale::new(factor);
                for fraction in [0.25, 0.75] {
                    let viewport = UiRect::new(0.0, 0.0, 400.0, 64.0);
                    let left = (16.0 + fraction) / factor;
                    let header = UiRect::new(left, 2.0, left + 360.0, 2.0 + theme::TABS_H);
                    let view: AppView = Arc::new(move |cx| {
                        let state = AppContext::detached(cx.state_with(|| {
                            let mut app = AppState::new();
                            app.terminal.ui.terminal_focused = true;
                            app
                        }));
                        let tabs = crate::app::terminal_binding::test_store(
                            cx,
                            Some(state.clone()),
                            || {
                                let mut tabs = TerminalTabs::new();
                                let second = tabs.split(1).unwrap();
                                tabs.split(second);
                                tabs
                            },
                        );
                        let snapshot = tabs.get();
                        let id = cx.use_stable_id();
                        let focus = cx.focus_handle(id);
                        let editor_id = cx.use_stable_id();
                        let editor_focus = cx.focus_handle(editor_id);
                        panel(viewport, VisualStyle::filled(theme::c().sidebar)).child(
                            render_terminal_tabs(
                                header,
                                &snapshot.tabs(),
                                snapshot.active_id().unwrap(),
                                tabs,
                                state,
                                crate::app::layout_map::MapStore::default(),
                                focus,
                                editor_focus,
                                Arc::new(ApplicationHandle::new(|task| task(), || {})),
                            ),
                        )
                    });
                    let mut session = UiSession::new();
                    session.render_view(&view, viewport, scale);
                    let physical = scale.physical_rect(viewport);
                    let dirty = [physical];
                    let frame =
                        FrameInfo::new(physical, &dirty, scale, FrameReason::SceneChange, true);
                    let mut surface = lgui::render_skia::SkiaSoftwareSurface::new(4 * 1024 * 1024);
                    lgui::assets::backend::with_render_resources(
                        &context,
                        lgui::RenderResources::new(),
                        || surface.draw(&session.tree().scene().project_to_physical(scale), &frame),
                    )
                    .unwrap();
                    // The line that joins split members is one solid row of pixels: not
                    // blended with its neighbours at any scale, and with the strip's own
                    // background just above and below it.
                    let bgra = |color: lgui::prelude::Color| {
                        [color.0 as u8, (color.0 >> 8) as u8, (color.0 >> 16) as u8, 255]
                    };
                    let pixel = |x: i32, y: i32| {
                        let offset = (y as usize * physical.width() as usize + x as usize) * 4;
                        surface.pixels()[offset..offset + 4].to_vec()
                    };
                    for member in [1, 2] {
                        let link = session
                            .tree()
                            .node(&UiId::owned(format!("terminal-tab-link-{member}")))
                            .unwrap();
                        let x = scale
                            .physical_value((link.layout_rect.left + link.layout_rect.right) / 2.0);
                        let top = scale.physical_value(link.layout_rect.top);
                        let rows = scale.physical_length(1.0).max(1);
                        for row in 0..rows {
                            assert_eq!(
                                pixel(x, top + row),
                                bgra(theme::c().text_faint),
                                "{} line, scale={factor}, fraction={fraction}, member={member}, row={row}",
                                theme_id.id()
                            );
                        }
                        for y in [top - 1, top + rows] {
                            assert_eq!(
                                pixel(x, y),
                                bgra(theme::c().sidebar),
                                "{} beside the line, scale={factor}, fraction={fraction}, member={member}, y={y}",
                                theme_id.id()
                            );
                        }
                    }
                }
            }
        }
        theme::set(previous);
    }

    #[test]
    fn shell_menu_only_selects_available_profiles_and_plus_creates_selected_shell() {
        use crate::domain::application_layout::ApplicationLayout;
        use crate::features::terminal::profiles::ShellProfile;
        use lgui::application::{AppView, ApplicationContext};
        use lgui::core::{InputEvent, PointerData, UiScale, dispatch_runtime_output};
        use lgui::session::UiSession;
        use std::sync::Mutex;
        let viewport = UiRect::new(0.0, 0.0, 400.0, 400.0);
        let exposed = Arc::new(Mutex::new(
            None::<(AppContext, crate::app::terminal_binding::TerminalStore)>,
        ));
        let output = exposed.clone();
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(|| {
                let mut app = AppState::new();
                app.preferences.values_mut().terminal_profiles = (1..=10)
                    .map(|id| ShellProfile {
                        id,
                        name: format!("Custom {id}"),
                        program: std::env::current_exe().unwrap(),
                        args: vec![format!("--profile={id}")],
                    })
                    .collect();
                app.preferences.values_mut().shell_options = app
                    .preferences
                    .values_mut()
                    .terminal_profiles
                    .iter()
                    .map(crate::infrastructure::terminal::profiles::custom)
                    .collect();
                app.preferences.values_mut().shell_options[0].program = None;
                let mut layout = ApplicationLayout::initial(viewport.into(), 40.0, 40.0, 200.0);
                layout.attach(1);
                app.layout.application_layout = Some(layout);
                app.terminal.ui.terminal_shell_menu = true;
                app.explorer
                    .model
                    .restore(vec!["workspace-root".into()], vec![]);
                app
            }));
            let tabs = crate::app::terminal_binding::test_store(cx, Some(state.clone()), || {
                let mut tabs = TerminalTabs::new();
                tabs.set_profiles(&state.get().preferences.values().terminal_profiles);
                tabs.restore_directories(&[Some("remembered-cwd".into())]);
                tabs
            });
            *output.lock().unwrap() = Some((state.clone(), tabs.clone()));
            let id = cx.use_stable_id();
            let focus = cx.focus_handle(id.clone());
            let editor_id = cx.use_stable_id();
            let editor_focus = cx.focus_handle(editor_id);
            let controller = tabs.get().active().unwrap().controller.clone();
            group(viewport).child(render(
                viewport,
                state,
                editor_focus,
                focus,
                id,
                controller,
                tabs,
                Arc::new(ApplicationHandle::new(|task| task(), || {})),
                true,
                viewport.height(),
            ))
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let (state, tabs) = exposed.lock().unwrap().clone().unwrap();
        let context = ApplicationContext::empty(Default::default());
        fn click(
            session: &mut UiSession,
            context: &ApplicationContext,
            view: &AppView,
            viewport: UiRect,
            point: Point,
        ) {
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
                let events = session.handle_input(event);
                dispatch_runtime_output(
                    events,
                    context,
                    &lgui::window::WindowId::new("shell-menu-test"),
                    |action| session.handle_default_action(action),
                    |_| {},
                );
                session.render_view(view, viewport, UiScale::ONE);
            }
        }
        fn label_point(session: &UiSession, label: &str) -> Point {
            let node = session
                .tree()
                .nodes()
                .iter()
                .find(|n| {
                    n.text
                        .as_deref()
                        .is_some_and(|text| text.starts_with(label))
                })
                .unwrap_or_else(|| panic!("missing menu label {label}"));
            Point::new(
                (node.layout_rect.left + node.layout_rect.right) * 0.5,
                (node.layout_rect.top + node.layout_rect.bottom) * 0.5,
            )
        }
        let original = tabs.get().active().unwrap().clone();
        let point = label_point(&session, "Custom 1");
        click(&mut session, &context, &view, viewport, point);
        assert_eq!(tabs.get().tabs().len(), 1);
        assert!(state.get().terminal.ui.terminal_shell_selection.is_none());
        assert!(state.get().terminal.ui.terminal_shell_menu);
        state.update(|app| app.terminal.ui.terminal_shell_scroll = 9);
        session.render_view(&view, viewport, UiScale::ONE);
        let point = label_point(&session, "Custom 10");
        click(&mut session, &context, &view, viewport, point);
        assert_eq!(tabs.get().tabs().len(), 1);
        assert_eq!(tabs.get().active_id(), Some(original.id));
        assert_eq!(
            tabs.get().active().unwrap().controller.shell(),
            original.controller.shell()
        );
        assert_eq!(
            state.get().terminal.ui.terminal_shell_selection,
            Some(ShellKind::Custom(10))
        );
        assert!(!state.get().terminal.ui.terminal_shell_menu);
        // The header keeps no shell label; the + button names the chosen shell.
        let plus = named_point(&session, "New terminal: Custom 10");
        click(&mut session, &context, &view, viewport, plus);
        assert_eq!(tabs.get().tabs().len(), 2);
        let tab = tabs.get().active().unwrap().clone();
        assert_eq!(tab.controller.shell(), ShellKind::Custom(10));
        assert_eq!(tab.controller.short_label(), "Custom 10");
        assert_eq!(tab.cwd, Some("workspace-root".into()));
        assert_eq!(original.cwd, Some("remembered-cwd".into()));
        assert_eq!(
            state
                .get()
                .layout
                .application_layout
                .as_ref()
                .unwrap()
                .default_terminal_active(),
            Some(tab.id)
        );
        assert!(!state.get().terminal.ui.terminal_shell_menu);
    }

    #[test]
    fn scrollbars_follow_pane_hover_and_drag_history_outside_the_pane() {
        use lgui::application::{AppView, ApplicationContext};
        use lgui::core::{InputEvent, PointerData, UiScale, dispatch_runtime_output};
        use lgui::session::UiSession;
        use std::sync::Mutex;

        let exposed = Arc::new(Mutex::new(None));
        let output = exposed.clone();
        let viewport = UiRect::new(0.0, 0.0, 600.0, 320.0);
        let terminal_rect = UiRect::new(0.0, 0.0, 600.0, 260.0);
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(AppState::new));
            let tabs = crate::app::terminal_binding::test_store(cx, Some(state.clone()), || {
                let mut tabs = TerminalTabs::new();
                tabs.split(1);
                tabs
            });
            *output.lock().unwrap() = Some((state.clone(), tabs.clone()));
            let id = UiId::new("scrollbar-terminal-input");
            let focus = cx.focus_handle(id.clone());
            let editor_focus = cx.focus_handle(UiId::new("scrollbar-editor-input"));
            let application = Arc::new(ApplicationHandle::new(|task| task(), || {}));
            let values = tabs.get().active_group();
            let regions = values
                .iter()
                .zip(session_rects(terminal_rect, &values))
                .map(|(tab, rect)| (tab.id, rect))
                .collect();
            let binding = crate::app::terminal_view::binding(&state, &tabs, &application).0;
            track_hover(
                group(viewport).child(render(
                    terminal_rect,
                    state,
                    editor_focus,
                    focus,
                    id,
                    tabs.get().active().unwrap().controller,
                    tabs,
                    application,
                    true,
                    260.0,
                )),
                binding,
                regions,
            )
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let (state, tabs) = exposed.lock().unwrap().clone().unwrap();
        let values = tabs.get().tabs();
        let left = values[0].controller.test_controller();
        let right = values[1].controller.test_controller();
        let thumb = |session: &UiSession, id| {
            session
                .tree()
                .nodes()
                .iter()
                .find(|node| {
                    let rect = node.layout_rect;
                    node.style.fill.is_some()
                        && (rect.right - (id as f32 * 300.0 - 2.0)).abs() < 0.01
                        && (rect.width() - 8.0).abs() < 0.01
                })
                .map(|node| node.layout_rect)
        };
        let context = ApplicationContext::empty(Default::default());
        let send = |session: &mut UiSession, input| {
            dispatch_runtime_output(
                session.handle_input(input),
                &context,
                &lgui::window::WindowId::new("terminal-scrollbar-test"),
                |action| session.handle_default_action(action),
                |_| {},
            );
            session.render_view(&view, viewport, UiScale::ONE);
        };
        let inside = PointerData::mouse(Point::new(80.0, 90.0));
        send(&mut session, InputEvent::PointerMove(inside));
        assert!(
            thumb(&session, 1).is_none(),
            "empty history has no scrollbar"
        );
        for controller in [&left, &right] {
            controller
                .test_parser()
                .process("output\r\n".repeat(100).as_bytes());
        }
        tabs.notify();
        session.render_view(&view, viewport, UiScale::ONE);
        assert!(thumb(&session, 1).is_some());
        // History keeps the faint scrollbar visible in every pane.
        assert!(thumb(&session, 2).is_some());
        let bottom = thumb(&session, 1).unwrap();
        assert_eq!(
            session
                .tree()
                .cursor_at(Point::new(bottom.left + 3.0, bottom.top + 4.0)),
            None
        );
        assert_eq!(
            session.tree().cursor_at(inside.point),
            Some(lgui::core::CursorIcon::Text)
        );
        send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(Point::new(450.0, 90.0))),
        );
        assert!(thumb(&session, 1).is_some());
        assert!(thumb(&session, 2).is_some());
        send(&mut session, InputEvent::PointerMove(inside));
        left.scroll(20);
        tabs.notify();
        session.render_view(&view, viewport, UiScale::ONE);
        let scrolled = thumb(&session, 1).unwrap();
        assert!(scrolled.top < bottom.top);
        let start = PointerData::mouse(Point::new(
            (scrolled.left + scrolled.right) * 0.5,
            scrolled.top + 4.0,
        ));
        send(
            &mut session,
            InputEvent::PointerDown {
                pointer: start,
                button: PointerButton::Left,
            },
        );
        assert!(state.get().terminal.ui.terminal_scrollbar_drag.is_some());
        assert!(!state.get().terminal.ui.selecting_terminal);
        let above = PointerData::mouse(Point::new(350.0, 0.0));
        send(&mut session, InputEvent::PointerMove(above));
        {
            let parser = left.test_parser();
            assert_eq!(
                parser.screen().scrollback(),
                parser.screen().scrollback_rows()
            );
        }
        assert!(
            thumb(&session, 1).is_some(),
            "drag keeps thumb visible outside pane"
        );
        assert_eq!(right.test_parser().screen().scrollback(), 0);
        let below = PointerData::mouse(Point::new(350.0, 310.0));
        send(&mut session, InputEvent::PointerMove(below));
        assert_eq!(left.test_parser().screen().scrollback(), 0);
        send(
            &mut session,
            InputEvent::PointerUp {
                pointer: below,
                button: PointerButton::Left,
            },
        );
        assert!(state.get().terminal.ui.terminal_scrollbar_drag.is_none());
        assert!(thumb(&session, 1).is_some());
        assert!(
            left.test_selection().is_none(),
            "drag must not start text selection"
        );
        send(&mut session, InputEvent::PointerMove(inside));
        assert!(thumb(&session, 1).is_some());
        left.test_parser().process(b"\x1b[?1049h");
        tabs.notify();
        session.render_view(&view, viewport, UiScale::ONE);
        assert!(
            thumb(&session, 1).is_none(),
            "alternate screen has no history scrollbar"
        );
        left.test_parser().process(b"\x1b[?1049l");
        tabs.notify();
        session.render_view(&view, viewport, UiScale::ONE);
        assert!(thumb(&session, 1).is_some());

        let restored = thumb(&session, 1).unwrap();
        send(
            &mut session,
            InputEvent::PointerDown {
                pointer: PointerData::mouse(Point::new(restored.left + 3.0, restored.top + 4.0)),
                button: PointerButton::Left,
            },
        );
        assert!(state.get().terminal.ui.terminal_scrollbar_drag.is_some());
        left.test_parser().process(b"\x1b[?1049h");
        tabs.notify();
        session.render_view(&view, viewport, UiScale::ONE);
        send(
            &mut session,
            InputEvent::PointerUp {
                pointer: below,
                button: PointerButton::Left,
            },
        );
        assert!(state.get().terminal.ui.terminal_scrollbar_drag.is_none());
    }

    #[test]
    fn mouse_coordinates_use_cells_instead_of_selection_boundaries() {
        let content = UiRect::new(12.0, 6.0, 500.0, 400.0);
        let point = Point::new(
            content.left + terminal_cell_width() * 3.75,
            content.top + terminal_line_h() * 2.5,
        );
        assert_eq!(mouse_cell_at(content, point, 10, 20), (2, 3));
        assert_eq!(cell_at(content, point, 10, 20), (2, 4));
        assert_eq!(
            mouse_cell_at(content, Point::new(-10.0, -10.0), 10, 20),
            (0, 0)
        );
        assert_eq!(
            mouse_cell_at(content, Point::new(1000.0, 1000.0), 10, 20),
            (9, 19)
        );
    }

    #[test]
    fn workspace_picker_supports_cancel_keyboard_choice_and_first_terminal() {
        use lgui::application::{AppView, ApplicationContext};
        use lgui::core::{InputEvent, UiScale, dispatch_runtime_output};
        use lgui::session::UiSession;
        use std::sync::Mutex;
        let exposed = Arc::new(Mutex::new(None));
        let output = exposed.clone();
        let viewport = UiRect::new(0.0, 0.0, 600.0, 400.0);
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(|| {
                let mut app = AppState::new();
                app.explorer
                    .model
                    .restore(vec!["project-a".into(), "project-b".into()], vec![]);
                app.workspace
                    .open_path("outside-project.txt".into(), String::new());
                app.editor.focused = true;
                app
            }));
            let tabs = crate::app::terminal_binding::test_store(cx, Some(state.clone()), || {
                TerminalTabs::restored(vec![], None)
            });
            let terminal_id = cx.use_stable_id();
            let terminal_focus = cx.focus_handle(terminal_id);
            let editor_id = cx.use_stable_id();
            let editor_focus = cx.focus_handle(editor_id);
            *output.lock().unwrap() = Some((
                state.clone(),
                tabs.clone(),
                terminal_focus.clone(),
                editor_focus.clone(),
            ));
            let env = crate::app::key_actions::KeyEnv {
                state: state.clone(),
                git_store: cx.state_with(crate::features::git::GitStoreSnapshot::default),
                application: ApplicationContext::empty(Default::default()),
                editor_focus: editor_focus.clone(),
                editor_rect: viewport,
                terminal: None,
            };
            let menu = crate::app::terminal_view::menu(
                viewport,
                state,
                tabs,
                Arc::new(ApplicationHandle::new(|task| task(), || {})),
                terminal_focus,
                editor_focus,
            );
            crate::app::key_actions::attach(menu, env)
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let (state, tabs, terminal_focus, editor_focus) = exposed.lock().unwrap().clone().unwrap();
        let pending = || {
            state.update(|app| app.terminal_chrome.show_terminal = false);
            crate::app::terminal::toggle_panel(&state, &tabs, &editor_focus, &terminal_focus);
        };
        let context = ApplicationContext::empty(Default::default());
        fn press(
            session: &mut UiSession,
            context: &ApplicationContext,
            view: &AppView,
            viewport: UiRect,
            named: NamedKey,
        ) {
            let events = session.handle_input(InputEvent::Keyboard(key(
                LogicalKey::Named(named),
                Default::default(),
            )));
            dispatch_runtime_output(
                events,
                context,
                &lgui::window::WindowId::new("workspace-picker-test"),
                |action| session.handle_default_action(action),
                |_| {},
            );
            session.render_view(view, viewport, UiScale::ONE);
        }
        pending();
        session.render_view(&view, viewport, UiScale::ONE);
        assert!(
            session
                .tree()
                .nodes()
                .iter()
                .any(|n| n.text.as_deref() == Some("project-b"))
        );
        press(&mut session, &context, &view, viewport, NamedKey::Escape);
        assert!(state.get().terminal.ui.terminal_workspace_picker.is_none());
        assert!(tabs.get().is_empty());
        pending();
        session.render_view(&view, viewport, UiScale::ONE);
        press(&mut session, &context, &view, viewport, NamedKey::ArrowDown);
        press(&mut session, &context, &view, viewport, NamedKey::Enter);
        assert_eq!(tabs.get().tabs().len(), 1);
        assert_eq!(tabs.get().active().unwrap().cwd, Some("project-b".into()));
        assert_eq!(
            state.get().terminal.ui.terminal_workspace_root,
            Some("project-b".into())
        );
        assert!(state.get().terminal.ui.terminal_workspace_picker.is_none());
    }

    #[test]
    fn resizing_adjacent_panes_preserves_other_widths_and_fits_narrow_windows() {
        let mut tabs = TerminalTabs::new();
        let first = tabs.active_id().unwrap();
        let second = tabs.split(first).unwrap();
        tabs.split(second);
        let bounds = UiRect::new(20.0, 0.0, 1020.0, 300.0);
        let before = session_rects(bounds, &tabs.active_group());
        tabs.resize_pair(first, second, 0.8);
        let after = session_rects(bounds, &tabs.active_group());
        assert!(after[0].width() > before[0].width());
        assert!(after[1].width() < before[1].width());
        assert!((after[2].left - before[2].left).abs() < 0.001);
        assert!((after[2].width() - before[2].width()).abs() < 0.001);
        assert!(after.iter().all(|pane| pane.width() >= 199.0));
        let narrow = session_rects(UiRect::new(0.0, 0.0, 90.0, 300.0), &tabs.active_group());
        assert_eq!(narrow.last().unwrap().right, 90.0);
        assert!(narrow.iter().all(|pane| pane.width() >= 29.0));
    }

    #[test]
    fn split_panes_select_independently_preserve_drag_selection_and_route_input() {
        use lgui::application::{AppView, ApplicationContext};
        use lgui::core::{InputEvent, PointerData, UiScale, dispatch_runtime_output};
        use lgui::session::UiSession;
        use std::sync::Mutex;

        let exposed = Arc::new(Mutex::new(
            None::<(AppContext, crate::app::terminal_binding::TerminalStore)>,
        ));
        let output = exposed.clone();
        let application = Arc::new(ApplicationHandle::new(|task| task(), || {}));
        let viewport = UiRect::new(0.0, 0.0, 600.0, 300.0);
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(|| {
                let mut app = AppState::new();
                app.terminal.ui.terminal_focused = true;
                app
            }));
            let tabs = crate::app::terminal_binding::test_store(cx, Some(state.clone()), || {
                let mut tabs = TerminalTabs::new();
                tabs.split(1);
                tabs
            });
            *output.lock().unwrap() = Some((state.clone(), tabs.clone()));
            let id = UiId::new("split-terminal-input");
            let focus = cx.focus_handle(id.clone());
            let editor_id = cx.use_stable_id();
            let editor_focus = cx.focus_handle(editor_id.clone());
            let controller = tabs.get().active().unwrap().controller.clone();
            group(viewport)
                .child(
                    Element::new(move |cx| {
                        UiElement::panel(
                            editor_id,
                            UiRect::new(0.0, 260.0, 600.0, 300.0),
                            VisualStyle::default(),
                        )
                        .children(cx.children)
                    })
                    .event_policy(EventPolicy::INTERACTIVE),
                )
                .child(render(
                    UiRect::new(0.0, 0.0, 600.0, 260.0),
                    state,
                    editor_focus,
                    focus,
                    id,
                    controller,
                    tabs,
                    application.clone(),
                    true,
                    260.0,
                ))
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let (state, tabs) = exposed.lock().unwrap().clone().unwrap();
        // Each tab is a rounded block shorter than the strip: the selected one in the
        // strong fill, the other faintly. There is no group background or per-tab
        // separator.
        let active = tabs.get().active_id().unwrap();
        for id in [1, 2] {
            let tab = session
                .tree()
                .node(&UiId::owned(format!("terminal-tab-{id}")))
                .unwrap();
            assert_eq!(tab.style.radius, 0.0);
            assert_eq!(tab.style.fill, None);
            assert!(
                session
                    .tree()
                    .node(&UiId::owned(format!("terminal-tab-separator-{id}")))
                    .is_none()
            );
            let background = session
                .tree()
                .node(&UiId::owned(format!("terminal-tab-background-{id}")))
                .expect("every tab has a block");
            assert_eq!(background.style.radius, TAB_BLOCK_RADIUS);
            assert_eq!(
                background.style.fill,
                Some(if id == active {
                    theme::c().active_tab
                } else {
                    theme::c().active_line
                })
            );
            assert_eq!(background.layout_rect.left, tab.layout_rect.left);
            assert_eq!(background.layout_rect.right, tab.layout_rect.right);
            // Centred in the strip and shorter than it; an odd-height strip puts the
            // extra pixel on one side.
            assert!(background.layout_rect.height() <= TAB_BLOCK_H + 1.0);
            assert!(background.layout_rect.height() < tab.layout_rect.height());
            assert!(
                ((background.layout_rect.top - tab.layout_rect.top)
                    - (tab.layout_rect.bottom - background.layout_rect.bottom))
                    .abs()
                    <= 1.0
            );
        }
        assert!(
            session
                .tree()
                .node(&UiId::new("terminal-tab-group-1"))
                .is_none(),
            "no background joins a split group"
        );
        // A line across the gap joins the two members, at the blocks' middle.
        let first = session.tree().node(&UiId::new("terminal-tab-1")).unwrap();
        let second = session.tree().node(&UiId::new("terminal-tab-2")).unwrap();
        let link = session
            .tree()
            .node(&UiId::new("terminal-tab-link-1"))
            .expect("split members are joined by a line");
        assert_eq!(link.style.fill, Some(theme::c().text_faint));
        assert_eq!(link.layout_rect.left, first.layout_rect.right);
        assert_eq!(link.layout_rect.right, second.layout_rect.left);
        assert_eq!(link.layout_rect.height(), 1.0);
        let middle = (first.layout_rect.top + first.layout_rect.bottom) / 2.0;
        assert!((link.layout_rect.top - middle).abs() <= 1.0);
        assert!(
            session
                .tree()
                .node(&UiId::new("terminal-tab-link-2"))
                .is_none(),
            "the last member joins nothing"
        );
        assert_eq!(
            session
                .tree()
                .node(&UiId::new("split-terminal-input"))
                .unwrap()
                .ime_cursor_rect
                .unwrap()
                .left,
            313.0
        );
        assert_eq!(
            session
                .tree()
                .nodes()
                .iter()
                .filter(|node| node.text.as_deref() == Some(ShellKind::default().short_label()))
                .count(),
            2 // The two tab labels; the header has no shell selector label.
        );
        let context = ApplicationContext::empty(Default::default());
        // Both close controls stay visible without moving as the pointer crosses tabs.
        {
            let snapshot = tabs.get();
            let active = snapshot.active_id().unwrap();
            let inactive = snapshot
                .tabs()
                .iter()
                .find(|tab| tab.id != active)
                .unwrap()
                .id;
            let active_close = UiId::owned(format!("terminal-tab-close-{active}"));
            let inactive_close = UiId::owned(format!("terminal-tab-close-{inactive}"));
            let active_bounds = session.tree().node(&active_close).unwrap().hit_rect;
            let inactive_bounds = session.tree().node(&inactive_close).unwrap().hit_rect;

            let select = session
                .tree()
                .node(&UiId::owned(format!("terminal-tab-select-{inactive}")))
                .unwrap()
                .hit_rect;
            session.handle_input(InputEvent::PointerMove(lgui::core::PointerData::mouse(
                Point::new(
                    (select.left + select.right) / 2.0,
                    (select.top + select.bottom) / 2.0,
                ),
            )));
            session.render_view(&view, viewport, UiScale::ONE);
            let close_bounds = session.tree().node(&inactive_close).unwrap().hit_rect;
            session.handle_input(InputEvent::PointerMove(lgui::core::PointerData::mouse(
                Point::new((close_bounds.left + close_bounds.right) / 2.0, 10.0),
            )));
            session.render_view(&view, viewport, UiScale::ONE);
            assert!(session.tree().node(&inactive_close).is_some());

            session.handle_input(InputEvent::PointerMove(lgui::core::PointerData::mouse(
                Point::new(400.0, 240.0),
            )));
            session.render_view(&view, viewport, UiScale::ONE);
            assert_eq!(
                session.tree().node(&inactive_close).unwrap().hit_rect,
                inactive_bounds
            );
            assert_eq!(
                session.tree().node(&active_close).unwrap().hit_rect,
                active_bounds
            );
        }
        let add_point = named_point(&session, "New terminal");
        let mut send = |input| {
            let events = session.handle_input(input);
            dispatch_runtime_output(
                events,
                &context,
                &lgui::window::WindowId::new("terminal-split-test"),
                |action| session.handle_default_action(action),
                |_| {},
            );
            session.render_view(&view, viewport, UiScale::ONE);
            session
                .tree()
                .node(&UiId::new("split-terminal-input"))
                .unwrap()
                .ime_cursor_rect
                .unwrap()
        };
        let divider = PointerData::mouse(Point::new(300.5, 80.0));
        send(InputEvent::PointerDown {
            pointer: divider,
            button: PointerButton::Left,
        });
        assert_eq!(state.get().terminal.ui.terminal_split_drag, Some(1));
        let moved = PointerData::mouse(Point::new(350.0, 80.0));
        send(InputEvent::PointerMove(moved));
        assert_eq!(tabs.get().active_id(), Some(2));
        let layout = session_rects(
            UiRect::new(0.0, 0.0, 600.0, 260.0),
            &tabs.get().active_group(),
        );
        assert_eq!(layout[0].right, 350.0);
        assert!(pty_size_for_body(layout[0]).cols > pty_size_for_body(layout[1]).cols);
        send(InputEvent::PointerUp {
            pointer: moved,
            button: PointerButton::Left,
        });
        assert_eq!(state.get().terminal.ui.terminal_split_drag, None);
        let weights = tabs
            .get()
            .active_group()
            .iter()
            .map(|tab| tab.width_weight)
            .collect::<Vec<_>>();
        send(InputEvent::PointerMove(divider));
        assert_eq!(
            tabs.get()
                .active_group()
                .iter()
                .map(|tab| tab.width_weight)
                .collect::<Vec<_>>(),
            weights
        );

        let start = PointerData::mouse(Point::new(50.0, 80.0));
        let caret = send(InputEvent::PointerDown {
            pointer: start,
            button: PointerButton::Left,
        });
        assert_eq!(tabs.get().active_id(), Some(1));
        assert_eq!(caret.left, 12.0);
        let end = PointerData::mouse(Point::new(85.0, 80.0));
        send(InputEvent::PointerMove(end));
        send(InputEvent::PointerUp {
            pointer: end,
            button: PointerButton::Left,
        });
        assert!(!state.get().terminal.ui.selecting_terminal);
        let snapshot = tabs.get();
        let left = snapshot.tabs()[0].controller.clone();
        let right = snapshot.tabs()[1].controller.clone();
        assert!(!left.snapshot().selection.is_empty());
        assert!(right.snapshot().selection.is_empty());
        right.begin_selection(0, 0);
        right.extend_selection(0, 2);
        send(InputEvent::TextInput("left terminal input".into()));
        assert!(left.snapshot().selection.is_empty());
        assert!(!right.snapshot().selection.is_empty());

        let add = PointerData::mouse(add_point);
        send(InputEvent::PointerDown {
            pointer: add,
            button: PointerButton::Left,
        });
        send(InputEvent::PointerUp {
            pointer: add,
            button: PointerButton::Left,
        });
        assert_eq!(tabs.get().tabs().len(), 3);
        assert_eq!(tabs.get().active_group().len(), 1);
        assert_eq!(tabs.get().saved_groups(), vec![0, 0, 1]);
        let panes = pane_rects(UiRect::new(0.0, 0.0, 600.0, 260.0), 2);
        assert_eq!(panes.len(), 2);
        assert_eq!(panes[0].right + 1.0, panes[1].left);
        assert!(
            pty_size_for_body(panes[0]).cols < pty_size(UiRect::new(0.0, 0.0, 600.0, 260.0)).cols
        );
    }

    #[test]
    fn tab_context_menu_targets_inactive_tabs_and_blocks_shell_input_until_dismissed() {
        use lgui::application::{AppView, ApplicationContext};
        use lgui::core::{InputEvent, UiScale, dispatch_runtime_output};
        use lgui::session::UiSession;
        use std::sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        };

        let exposed = Arc::new(Mutex::new(
            None::<(AppContext, crate::app::terminal_binding::TerminalStore)>,
        ));
        let output = exposed.clone();
        let writes = Arc::new(AtomicUsize::new(0));
        let received = writes.clone();
        let application = Arc::new(ApplicationHandle::new(|task| task(), || {}));
        let viewport = UiRect::new(0.0, 0.0, 600.0, 300.0);
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(|| {
                let mut app = AppState::new();
                app.terminal_chrome.show_terminal = true;
                app
            }));
            let tabs = crate::app::terminal_binding::test_store(cx, Some(state.clone()), || {
                let mut tabs = TerminalTabs::new();
                tabs.add(ShellKind::Cmd);
                tabs.add(ShellKind::Bash);
                tabs
            });
            *output.lock().unwrap() = Some((state.clone(), tabs.clone()));
            let terminal_id = cx.use_stable_id();
            let terminal_focus = cx.focus_handle(terminal_id.clone());
            let editor_id = cx.use_stable_id();
            let editor_focus = cx.focus_handle(editor_id.clone());
            crate::ui::terminal::menu::restore_focus_on_dismiss(
                cx,
                state.get().terminal.ui.terminal_tab_context_menu.is_some(),
                state.get().terminal_chrome.show_terminal,
                terminal_focus.clone(),
                editor_focus.clone(),
            );
            let received = received.clone();
            let terminal = Element::new(move |cx| {
                UiElement::panel(
                    terminal_id,
                    UiRect::new(0.0, 40.0, 600.0, 200.0),
                    VisualStyle::default(),
                )
                .auto_focus()
                .children(cx.children)
            })
            .event_policy(EventPolicy::INTERACTIVE)
            .on_input(move |_, _| {
                received.fetch_add(1, Ordering::AcqRel);
            });
            let editor = Element::new(move |cx| {
                UiElement::panel(
                    editor_id,
                    UiRect::new(0.0, 200.0, 600.0, 300.0),
                    VisualStyle::default(),
                )
                .children(cx.children)
            })
            .event_policy(EventPolicy::INTERACTIVE);
            let snapshot = tabs.get();
            let mut root = group(viewport).child(editor);
            if state.get().terminal_chrome.show_terminal {
                root = root.child(terminal);
            }
            if let Some(active_id) = snapshot.active_id() {
                root = root.child(render_terminal_tabs(
                    UiRect::new(0.0, 0.0, 600.0, 34.0),
                    &snapshot.tabs(),
                    active_id,
                    tabs.clone(),
                    state.clone(),
                    crate::app::layout_map::MapStore::default(),
                    terminal_focus.clone(),
                    editor_focus.clone(),
                    application.clone(),
                ));
            }
            if state.get().terminal.ui.terminal_tab_context_menu.is_some() {
                root = root.child(crate::app::terminal_view::menu(
                    viewport,
                    state,
                    tabs,
                    application.clone(),
                    terminal_focus,
                    editor_focus,
                ));
            }
            root
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let (state, tabs) = exposed.lock().unwrap().clone().unwrap();
        let context = ApplicationContext::empty(Default::default());
        let mut send = |input| {
            let events = session.handle_input(input);
            dispatch_runtime_output(
                events,
                &context,
                &lgui::window::WindowId::new("terminal-menu-test"),
                |action| session.handle_default_action(action),
                |_| {},
            );
            session.render_view(&view, viewport, UiScale::ONE);
            session.runtime().run_effects();
            session.render_view(&view, viewport, UiScale::ONE);
        };
        let point = lgui::core::PointerData::mouse(Point::new(30.0, 10.0));
        send(InputEvent::PointerDown {
            pointer: point,
            button: PointerButton::Right,
        });
        send(InputEvent::PointerUp {
            pointer: point,
            button: PointerButton::Right,
        });
        assert_eq!(
            state
                .get()
                .terminal
                .ui
                .terminal_tab_context_menu
                .unwrap()
                .target,
            1
        );
        assert_eq!(tabs.get().active_id(), Some(3));
        send(InputEvent::TextInput("must not reach shell".into()));
        assert_eq!(writes.load(Ordering::Acquire), 0);
        send(InputEvent::Keyboard(key(
            LogicalKey::Named(NamedKey::Escape),
            Default::default(),
        )));
        assert!(state.get().terminal.ui.terminal_tab_context_menu.is_none());
        send(InputEvent::TextInput("shell input restored".into()));
        assert_eq!(writes.load(Ordering::Acquire), 1);

        send(InputEvent::PointerDown {
            pointer: point,
            button: PointerButton::Right,
        });
        send(InputEvent::PointerUp {
            pointer: point,
            button: PointerButton::Right,
        });
        let others = lgui::core::PointerData::mouse(Point::new(45.0, 97.0));
        send(InputEvent::PointerDown {
            pointer: others,
            button: PointerButton::Left,
        });
        send(InputEvent::PointerUp {
            pointer: others,
            button: PointerButton::Left,
        });
        assert!(state.get().terminal.ui.terminal_tab_context_menu.is_none());
        assert_eq!(tabs.get().tabs().len(), 1);
        assert_eq!(tabs.get().active_id(), Some(1));
        send(InputEvent::PointerDown {
            pointer: point,
            button: PointerButton::Right,
        });
        send(InputEvent::PointerUp {
            pointer: point,
            button: PointerButton::Right,
        });
        let outside = lgui::core::PointerData::mouse(Point::new(500.0, 250.0));
        send(InputEvent::PointerDown {
            pointer: outside,
            button: PointerButton::Left,
        });
        send(InputEvent::PointerUp {
            pointer: outside,
            button: PointerButton::Left,
        });
        assert!(state.get().terminal.ui.terminal_tab_context_menu.is_none());
        send(InputEvent::TextInput(
            "outside dismissal restores input".into(),
        ));
        assert_eq!(writes.load(Ordering::Acquire), 2);

        // Single click selects a terminal; double click opens its purpose menu.
        let label = lgui::core::PointerData::mouse(Point::new(30.0, 10.0));
        send(InputEvent::PointerDown {
            pointer: label,
            button: PointerButton::Left,
        });
        send(InputEvent::PointerUp {
            pointer: label,
            button: PointerButton::Left,
        });
        assert!(state.get().terminal.ui.terminal_tab_context_menu.is_none());
        // Double-clicking a tab no longer opens the purpose panel.
        send(InputEvent::PointerDown {
            pointer: label,
            button: PointerButton::Left,
        });
        send(InputEvent::PointerUp {
            pointer: label,
            button: PointerButton::Left,
        });
        assert!(state.get().terminal.ui.terminal_tab_context_menu.is_none());
        // The header's more menu reaches the same panel; that path is covered by
        // `more_menu_renames_by_hand_and_reaches_the_purpose_panel`.
        state.update(|app| {
            app.terminal.ui.terminal_tab_context_menu =
                Some(crate::ui::terminal::state::TerminalTabContextMenuState {
                    position: (30.0, 28.0),
                    target: 1,
                    kind: crate::ui::terminal::state::TerminalTabMenuKind::Purpose,
                })
        });
        send(InputEvent::PointerMove(lgui::core::PointerData::mouse(
            Point::new(60.0, 40.0),
        )));
        let purpose_menu = state.get().terminal.ui.terminal_tab_context_menu.unwrap();
        assert_eq!(
            purpose_menu.kind,
            crate::ui::terminal::state::TerminalTabMenuKind::Purpose
        );
        assert_eq!(purpose_menu.target, 1);
        let origin = purpose_menu.position;
        let testing = lgui::core::PointerData::mouse(Point::new(origin.0 + 20.0, origin.1 + 59.0));
        send(InputEvent::PointerDown {
            pointer: testing,
            button: PointerButton::Left,
        });
        send(InputEvent::PointerUp {
            pointer: testing,
            button: PointerButton::Left,
        });
        assert_eq!(
            tabs.get().tabs()[0].label(),
            format!("{} · Testing", ShellKind::default().short_label())
        );
        assert!(state.get().terminal.ui.terminal_tab_context_menu.is_none());
        send(InputEvent::TextInput(
            "typing resumes after purpose selection".into(),
        ));
        assert_eq!(writes.load(Ordering::Acquire), 3);

        // The general preset replaces free-form purpose entry.
        state.update(|app| {
            app.terminal.ui.terminal_tab_context_menu =
                Some(crate::ui::terminal::state::TerminalTabContextMenuState {
                    position: (20.0, 40.0),
                    target: 1,
                    kind: crate::ui::terminal::state::TerminalTabMenuKind::Purpose,
                })
        });
        send(InputEvent::PointerMove(lgui::core::PointerData::mouse(
            Point::new(40.0, 165.0),
        )));
        let other = lgui::core::PointerData::mouse(Point::new(40.0, 165.0));
        send(InputEvent::TextInput(
            "must not rename or reach shell".into(),
        ));
        assert_eq!(writes.load(Ordering::Acquire), 3);
        send(InputEvent::PointerDown {
            pointer: other,
            button: PointerButton::Left,
        });
        send(InputEvent::PointerUp {
            pointer: other,
            button: PointerButton::Left,
        });
        assert!(state.get().terminal.ui.terminal_tab_context_menu.is_none());
        assert_eq!(
            tabs.get().tabs()[0].label(),
            format!("{} · Other", ShellKind::default().short_label())
        );

        tabs.update(|tabs| tabs.update_programs(&[(1, Some("node".into()))]));
        state.update(|app| {
            app.terminal.ui.terminal_tab_context_menu =
                Some(crate::ui::terminal::state::TerminalTabContextMenuState {
                    position: (20.0, 40.0),
                    target: 1,
                    kind: crate::ui::terminal::state::TerminalTabMenuKind::Purpose,
                })
        });
        let automatic = lgui::core::PointerData::mouse(Point::new(40.0, 191.0));
        send(InputEvent::PointerMove(automatic));
        send(InputEvent::PointerDown {
            pointer: automatic,
            button: PointerButton::Left,
        });
        send(InputEvent::PointerUp {
            pointer: automatic,
            button: PointerButton::Left,
        });
        assert_eq!(tabs.get().tabs()[0].label(), "node");
        tabs.update(|tabs| tabs.update_programs(&[(1, None)]));

        send(InputEvent::PointerDown {
            pointer: point,
            button: PointerButton::Right,
        });
        send(InputEvent::PointerUp {
            pointer: point,
            button: PointerButton::Right,
        });
        let split = lgui::core::PointerData::mouse(Point::new(45.0, 47.0));
        send(InputEvent::PointerDown {
            pointer: split,
            button: PointerButton::Left,
        });
        send(InputEvent::PointerUp {
            pointer: split,
            button: PointerButton::Left,
        });
        assert_eq!(tabs.get().active_group().len(), 2);
        assert_eq!(tabs.get().saved_groups(), vec![0, 0]);

        send(InputEvent::PointerDown {
            pointer: point,
            button: PointerButton::Right,
        });
        send(InputEvent::PointerUp {
            pointer: point,
            button: PointerButton::Right,
        });
        let new = lgui::core::PointerData::mouse(Point::new(45.0, 25.0));
        send(InputEvent::PointerDown {
            pointer: new,
            button: PointerButton::Left,
        });
        send(InputEvent::PointerUp {
            pointer: new,
            button: PointerButton::Left,
        });
        assert_eq!(tabs.get().tabs().len(), 3);
        assert_eq!(tabs.get().active_id(), Some(5));
        assert_eq!(tabs.get().saved_groups(), vec![0, 0, 1]);
        assert_eq!(
            tabs.get().active().unwrap().controller.shell(),
            ShellKind::default()
        );

        send(InputEvent::PointerDown {
            pointer: point,
            button: PointerButton::Right,
        });
        send(InputEvent::PointerUp {
            pointer: point,
            button: PointerButton::Right,
        });
        let all = lgui::core::PointerData::mouse(Point::new(45.0, 140.0));
        send(InputEvent::PointerDown {
            pointer: all,
            button: PointerButton::Left,
        });
        send(InputEvent::PointerUp {
            pointer: all,
            button: PointerButton::Left,
        });
        assert!(tabs.get().is_empty());
        assert!(!state.get().terminal_chrome.show_terminal);
        assert!(state.get().terminal.ui.terminal_tab_context_menu.is_none());
    }

    #[test]
    fn measured_cell_width_drives_cursor_runs_hit_testing_and_pty_size() {
        // Simulate a renderer font advance that differs from the old 7.2px.
        let _restore = fonts::override_terminal(fonts::Metrics {
            char_w: 6.25,
            ..fonts::terminal()
        });
        let content = UiRect::new(10.0, 20.0, 2010.0, 100.0);
        let mut parser = vt100::Parser::new(4, 300, 0);
        for count in 1..=200 {
            parser.process(b"x");
            let cursor = terminal_cursor_rect(content, parser.screen().cursor_position(), 4, 300);
            assert_eq!(cursor.left, content.left + count as f32 * 6.25);
            let run = TerminalRun {
                start_col: 0,
                columns: count,
                text: "x".repeat(usize::from(count)),
                style: Default::default(),
            };
            assert_eq!(run_span(content, &run).1 - 2.0, cursor.left);
            assert_eq!(
                cell_at(content, Point::new(cursor.left, content.top), 4, 300),
                (0, count)
            );
        }
        assert_eq!(pty_size(UiRect::new(0.0, 0.0, 400.0, 160.0)).cols, 62);
    }

    #[test]
    fn cursor_remains_visible_while_a_full_line_waits_to_wrap() {
        let width = terminal_cell_width();
        let content = UiRect::new(0.0, 0.0, width * 8.0, terminal_line_h() * 2.0);
        let mut parser = vt100::Parser::new(2, 8, 0);
        parser.process(b"12345678");
        assert_eq!(parser.screen().cursor_position(), (0, 8));
        let cursor = terminal_cursor_rect(content, parser.screen().cursor_position(), 2, 8);
        assert!((cursor.right - content.right).abs() < 0.001);
        assert_eq!(cursor.top, 2.0);
        parser.process(b"9");
        let cursor = terminal_cursor_rect(content, parser.screen().cursor_position(), 2, 8);
        assert_eq!(cursor.left, width);
        assert_eq!(cursor.top, terminal_line_h() + 2.0);
    }

    #[test]
    fn more_menu_renames_by_hand_and_reaches_the_purpose_panel() {
        use crate::ui::terminal::state::TerminalTabMenuKind;
        use lgui::application::{AppView, ApplicationContext};
        use lgui::core::{InputEvent, PointerData, UiScale, dispatch_runtime_output};
        use lgui::session::UiSession;
        use std::sync::Mutex;

        let viewport = UiRect::new(0.0, 0.0, 700.0, 300.0);
        let application = Arc::new(ApplicationHandle::new(|task| task(), || {}));
        let exposed = Arc::new(Mutex::new(
            None::<(AppContext, crate::app::terminal_binding::TerminalStore)>,
        ));
        let output = exposed.clone();
        let view_application = application.clone();
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(AppState::new));
            let tabs = crate::app::terminal_binding::test_store(
                cx,
                Some(state.clone()),
                TerminalTabs::new,
            );
            *output.lock().unwrap() = Some((state.clone(), tabs.clone()));
            let id = cx.use_stable_id();
            let focus = cx.focus_handle(id.clone());
            let editor_id = cx.use_stable_id();
            let editor_focus = cx.focus_handle(editor_id);
            let controller = tabs.get().active().unwrap().controller.clone();
            group(viewport)
                .child(render(
                    viewport,
                    state.clone(),
                    editor_focus.clone(),
                    focus.clone(),
                    id,
                    controller,
                    tabs.clone(),
                    view_application.clone(),
                    true,
                    viewport.height(),
                ))
                .child(crate::app::terminal_view::menu(
                    viewport,
                    state,
                    tabs,
                    view_application.clone(),
                    focus,
                    editor_focus,
                ))
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let (state, tabs) = exposed.lock().unwrap().clone().unwrap();
        let context = ApplicationContext::empty(Default::default());
        let send = |session: &mut UiSession, input| {
            let events = session.handle_input(input);
            dispatch_runtime_output(
                events,
                &context,
                &lgui::window::WindowId::new("terminal-more-test"),
                |action| session.handle_default_action(action),
                |_| {},
            );
            session.render_view(&view, viewport, UiScale::ONE);
            session.runtime().run_effects();
            session.render_view(&view, viewport, UiScale::ONE);
        };
        let click = |session: &mut UiSession, point: Point| {
            send(
                session,
                InputEvent::PointerDown {
                    pointer: PointerData::mouse(point),
                    button: PointerButton::Left,
                },
            );
            send(
                session,
                InputEvent::PointerUp {
                    pointer: PointerData::mouse(point),
                    button: PointerButton::Left,
                },
            );
        };
        let text_point = |session: &UiSession, label: &str| {
            let node = session
                .tree()
                .nodes()
                .iter()
                .find(|n| n.text.as_deref().is_some_and(|t| t.trim_start() == label))
                .unwrap_or_else(|| panic!("missing label {label}"));
            Point::new(
                (node.layout_rect.left + node.layout_rect.right) * 0.5,
                (node.layout_rect.top + node.layout_rect.bottom) * 0.5,
            )
        };
        let click_named = |session: &mut UiSession, name: &str| {
            let point = named_point(session, name);
            click(session, point);
        };
        let click_text = |session: &mut UiSession, label: &str| {
            let point = text_point(session, label);
            click(session, point);
        };
        let shell = ShellKind::default().short_label();
        let target = tabs.get().active_id().unwrap();

        // More opens beneath its button, right-aligned to it.
        let more = named_point(&session, "More terminal actions");
        click(&mut session, more);
        let menu = state.get().terminal.ui.terminal_tab_context_menu.unwrap();
        assert_eq!(
            (menu.kind, menu.target),
            (TerminalTabMenuKind::More, target)
        );

        // The icon at the end of "Rename Session" opens the purpose presets.
        let purpose_button = named_point(&session, "Choose purpose");
        click(&mut session, purpose_button);
        let menu = state.get().terminal.ui.terminal_tab_context_menu.unwrap();
        assert_eq!(menu.kind, TerminalTabMenuKind::Purpose);
        let testing = text_point(&session, "Testing");
        click(&mut session, testing);
        assert_eq!(tabs.get().tabs()[0].label(), format!("{shell} · Testing"));
        assert!(state.get().terminal.ui.terminal_tab_context_menu.is_none());

        // The label itself renames by hand, starting from the current name.
        click_named(&mut session, "More terminal actions");
        click_text(&mut session, "Rename Session");
        let editor = state.get().terminal.ui.terminal_rename.unwrap();
        assert_eq!(editor.target, target);
        assert_eq!(editor.input.text(), shell);
        assert!(state.get().terminal.ui.terminal_tab_context_menu.is_none());
        // The field sits inside the tab, in the header row, with no popover.
        let field = named_point(&session, "Session name");
        assert!(
            field.y < 28.0,
            "the field is in the tab strip, not below it"
        );
        state.update(|app| {
            app.terminal
                .ui
                .terminal_rename
                .as_mut()
                .unwrap()
                .input
                .set_text("  API server ");
        });
        session.render_view(&view, viewport, UiScale::ONE);
        send(
            &mut session,
            InputEvent::Keyboard(key(LogicalKey::Named(NamedKey::Enter), Default::default())),
        );
        assert!(state.get().terminal.ui.terminal_rename.is_none());
        // The typed name replaces the shell label; the purpose still follows it.
        assert_eq!(tabs.get().tabs()[0].label(), "API server · Testing");
        assert_eq!(tabs.get().tabs()[0].title.as_deref(), Some("API server"));

        // Escape abandons a rename without changing the name.
        click_named(&mut session, "More terminal actions");
        click_text(&mut session, "Rename Session");
        state.update(|app| {
            app.terminal
                .ui
                .terminal_rename
                .as_mut()
                .unwrap()
                .input
                .set_text("discarded");
        });
        session.render_view(&view, viewport, UiScale::ONE);
        send(
            &mut session,
            InputEvent::Keyboard(key(LogicalKey::Named(NamedKey::Escape), Default::default())),
        );
        assert!(state.get().terminal.ui.terminal_rename.is_none());
        assert_eq!(tabs.get().tabs()[0].title.as_deref(), Some("API server"));

        // A blank name restores the automatic one.
        click_named(&mut session, "More terminal actions");
        click_text(&mut session, "Rename Session");
        state.update(|app| {
            app.terminal
                .ui
                .terminal_rename
                .as_mut()
                .unwrap()
                .input
                .set_text("   ");
        });
        session.render_view(&view, viewport, UiScale::ONE);
        send(
            &mut session,
            InputEvent::Keyboard(key(LogicalKey::Named(NamedKey::Enter), Default::default())),
        );
        assert_eq!(tabs.get().tabs()[0].title, None);
        assert_eq!(tabs.get().tabs()[0].label(), format!("{shell} · Testing"));

        // Clear Output closes the menu and leaves the session alive.
        click_named(&mut session, "More terminal actions");
        click_text(&mut session, "Clear Output");
        assert!(state.get().terminal.ui.terminal_tab_context_menu.is_none());
        assert_eq!(tabs.get().tabs().len(), 1);
    }

    #[test]
    fn maximize_button_fills_the_default_region_and_restores_it() {
        use crate::domain::application_layout::ApplicationLayout;
        use lgui::application::{AppView, ApplicationContext};
        use lgui::core::{InputEvent, PointerData, UiScale, dispatch_runtime_output};
        use lgui::session::UiSession;
        use std::sync::Mutex;

        let viewport = UiRect::new(0.0, 0.0, 700.0, 400.0);
        let application = Arc::new(ApplicationHandle::new(|task| task(), || {}));
        let exposed = Arc::new(Mutex::new(None::<AppContext>));
        let output = exposed.clone();
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(|| {
                let mut app = AppState::new();
                let mut layout = ApplicationLayout::initial(viewport.into(), 40.0, 40.0, 200.0);
                layout.attach(1);
                app.layout.application_layout = Some(layout);
                app.terminal_chrome.show_terminal = true;
                app
            }));
            let tabs = crate::app::terminal_binding::test_store(
                cx,
                Some(state.clone()),
                TerminalTabs::new,
            );
            *output.lock().unwrap() = Some(state.clone());
            let id = cx.use_stable_id();
            let focus = cx.focus_handle(id.clone());
            let editor_id = cx.use_stable_id();
            let editor_focus = cx.focus_handle(editor_id);
            let region = state
                .get()
                .layout
                .application_layout
                .as_ref()
                .unwrap()
                .default_terminal_region()
                .unwrap();
            group(viewport).child(crate::app::terminal_view::render_region(
                UiRect::new(0.0, 200.0, 700.0, 400.0),
                state,
                crate::app::layout_map::MapStore::default(),
                editor_focus,
                focus,
                id,
                tabs,
                application.clone(),
                true,
                region,
            ))
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let state = exposed.lock().unwrap().clone().unwrap();
        let context = ApplicationContext::empty(Default::default());
        let click = |session: &mut UiSession, name: &str| {
            let point = named_point(session, name);
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
                let events = session.handle_input(event);
                dispatch_runtime_output(
                    events,
                    &context,
                    &lgui::window::WindowId::new("terminal-maximize-test"),
                    |action| session.handle_default_action(action),
                    |_| {},
                );
                session.render_view(&view, viewport, UiScale::ONE);
            }
        };
        let maximized = |state: &AppContext| {
            let app = state.get();
            let layout = app.layout.application_layout.as_ref().unwrap();
            layout.is_maximized(layout.default_terminal_region().unwrap())
        };
        assert!(!maximized(&state));
        click(&mut session, "Maximize terminal");
        assert!(maximized(&state));
        // The same button now restores the previous split.
        click(&mut session, "Restore terminal size");
        assert!(!maximized(&state));
        click(&mut session, "Maximize terminal");
        assert!(maximized(&state));

        // Split keeps one header and adds a side-by-side group to its strip.
        let terminal_regions = |state: &AppContext| {
            let app = state.get();
            app.layout
                .application_layout
                .as_ref()
                .unwrap()
                .leaves()
                .iter()
                .filter(|(_, content)| {
                    matches!(
                        content,
                        crate::domain::application_layout::Content::Terminal { .. }
                    )
                })
                .count()
        };
        assert_eq!(terminal_regions(&state), 1);
        click(&mut session, "Split terminal");
        assert_eq!(terminal_regions(&state), 1);
        assert!(maximized(&state));
        let tree = session.tree();
        assert!(tree.node(&UiId::new("terminal-session-1")).is_some());
        assert!(tree.node(&UiId::new("terminal-session-2")).is_some());
        assert!(tree.node(&UiId::new("terminal-tab-link-1")).is_some());
        // The new split is the selected tab, drawn as a filled rounded block.
        assert_eq!(
            tree.node(&UiId::new("terminal-tab-background-2"))
                .unwrap()
                .style
                .fill,
            Some(theme::c().active_tab)
        );

        // The collapse icon at the end hides the panel, like the app's own toggle.
        assert!(state.get().default_terminal_open());
        click(&mut session, "Hide terminal panel");
        assert!(!state.get().default_terminal_open());
    }

    #[test]
    fn search_finds_output_selects_matches_and_keeps_keys_out_of_the_shell() {
        use lgui::application::{AppView, ApplicationContext};
        use lgui::core::{InputEvent, PointerData, UiScale, dispatch_runtime_output};
        use lgui::session::UiSession;
        use std::sync::Mutex;

        let viewport = UiRect::new(0.0, 0.0, 700.0, 300.0);
        let application = Arc::new(ApplicationHandle::new(|task| task(), || {}));
        let exposed = Arc::new(Mutex::new(
            None::<(AppContext, crate::app::terminal_binding::TerminalStore)>,
        ));
        let output = exposed.clone();
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(AppState::new));
            let tabs = crate::app::terminal_binding::test_store(
                cx,
                Some(state.clone()),
                TerminalTabs::new,
            );
            *output.lock().unwrap() = Some((state.clone(), tabs.clone()));
            let id = cx.use_stable_id();
            let focus = cx.focus_handle(id.clone());
            let editor_id = cx.use_stable_id();
            let editor_focus = cx.focus_handle(editor_id);
            let controller = tabs.get().active().unwrap().controller.clone();
            group(viewport).child(render(
                viewport,
                state,
                editor_focus,
                focus,
                id,
                controller,
                tabs,
                application.clone(),
                true,
                viewport.height(),
            ))
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let (state, tabs) = exposed.lock().unwrap().clone().unwrap();
        let controller = tabs.get().active().unwrap().controller.test_controller();
        controller
            .test_parser()
            .process(b"build ERROR one\r\nok\r\nerror two\r\nfine\r\nthe last Error");
        let context = ApplicationContext::empty(Default::default());
        let step = |session: &mut UiSession, input| {
            let events = session.handle_input(input);
            dispatch_runtime_output(
                events,
                &context,
                &lgui::window::WindowId::new("terminal-search-test"),
                |action| session.handle_default_action(action),
                |_| {},
            );
            session.render_view(&view, viewport, UiScale::ONE);
            session.runtime().run_effects();
            session.render_view(&view, viewport, UiScale::ONE);
        };
        let click = |session: &mut UiSession, name: &str| {
            let point = named_point(session, name);
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
                step(session, event);
            }
        };
        let write_count =
            |controller: &crate::infrastructure::terminal::session::TerminalController| {
                controller.test_writes().len()
            };

        // The header button opens the box; nothing is searched for yet.
        click(&mut session, "Search terminal output");
        let bar = state.get().terminal.ui.terminal_search.unwrap();
        assert_eq!(bar.target, tabs.get().active_id().unwrap());
        assert!(named_point(&session, "Find in terminal output").x > 0.0);
        assert_eq!(bar.status.total, 0);

        // Typing in the box searches, selects the last match and counts them.
        let before = write_count(&controller);
        state.update(|app| {
            app.terminal
                .ui
                .terminal_search
                .as_mut()
                .unwrap()
                .input
                .set_text("error");
        });
        session.render_view(&view, viewport, UiScale::ONE);
        session.runtime().run_effects();
        session.render_view(&view, viewport, UiScale::ONE);
        let status = state.get().terminal.ui.terminal_search.unwrap().status;
        assert_eq!((status.current, status.total), (3, 3));
        assert_eq!(controller.selected_text().as_deref(), Some("Error"));

        // The buttons step through matches, wrapping around.
        click(&mut session, "Previous match");
        assert_eq!(controller.selected_text().as_deref(), Some("error"));
        click(&mut session, "Next match");
        click(&mut session, "Next match");
        let status = state.get().terminal.ui.terminal_search.unwrap().status;
        assert_eq!(status.current, 1);
        assert_eq!(controller.selected_text().as_deref(), Some("ERROR"));
        // Enter in the box means next, and none of this reaches the shell.
        click(&mut session, "Find in terminal output");
        step(
            &mut session,
            InputEvent::Keyboard(key(LogicalKey::Named(NamedKey::Enter), Default::default())),
        );
        assert_eq!(
            state
                .get()
                .terminal
                .ui
                .terminal_search
                .unwrap()
                .status
                .current,
            2
        );
        assert_eq!(write_count(&controller), before);

        // Closing drops the highlight and the box; the button toggles it too.
        click(&mut session, "Close search");
        assert!(state.get().terminal.ui.terminal_search.is_none());
        assert!(controller.selected_text().is_none());
        click(&mut session, "Search terminal output");
        assert!(state.get().terminal.ui.terminal_search.is_some());
        click(&mut session, "Search terminal output");
        assert!(state.get().terminal.ui.terminal_search.is_none());

        // Escape inside the box closes it as well.
        click(&mut session, "Search terminal output");
        click(&mut session, "Find in terminal output");
        step(
            &mut session,
            InputEvent::Keyboard(key(LogicalKey::Named(NamedKey::Escape), Default::default())),
        );
        assert!(state.get().terminal.ui.terminal_search.is_none());
    }

    /// Renders the whole terminal for tab-strip tests and hands back the handles.
    fn strip_harness(
        width: f32,
        sessions: usize,
    ) -> (
        lgui::session::UiSession,
        lgui::application::AppView,
        AppContext,
        crate::app::terminal_binding::TerminalStore,
        UiRect,
    ) {
        use lgui::application::AppView;
        use lgui::core::UiScale;
        use lgui::session::UiSession;
        use std::sync::Mutex;
        let viewport = UiRect::new(0.0, 0.0, width, 200.0);
        let application = Arc::new(ApplicationHandle::new(|task| task(), || {}));
        let exposed = Arc::new(Mutex::new(None));
        let output = exposed.clone();
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(|| {
                let mut app = AppState::new();
                app.terminal.ui.terminal_focused = true;
                app
            }));
            let tabs = crate::app::terminal_binding::test_store(cx, Some(state.clone()), || {
                let mut tabs = TerminalTabs::new();
                for _ in 1..sessions {
                    tabs.add(ShellKind::Bash);
                }
                tabs
            });
            *output.lock().unwrap() = Some((state.clone(), tabs.clone()));
            let id = cx.use_stable_id();
            let focus = cx.focus_handle(id.clone());
            let editor_id = cx.use_stable_id();
            let editor_focus = cx.focus_handle(editor_id);
            let controller = tabs.get().active().unwrap().controller.clone();
            group(viewport)
                .child(render(
                    viewport,
                    state.clone(),
                    editor_focus.clone(),
                    focus.clone(),
                    id,
                    controller,
                    tabs.clone(),
                    application.clone(),
                    true,
                    viewport.height(),
                ))
                .child(crate::app::terminal_view::menu(
                    viewport,
                    state,
                    tabs,
                    application.clone(),
                    focus,
                    editor_focus,
                ))
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let (state, tabs) = exposed.lock().unwrap().clone().unwrap();
        (session, view, state, tabs, viewport)
    }

    fn tab_rects(session: &lgui::session::UiSession, ids: &[u64]) -> Vec<UiRect> {
        ids.iter()
            .map(|id| {
                session
                    .tree()
                    .node(&UiId::owned(format!("terminal-tab-select-{id}")))
                    .unwrap_or_else(|| panic!("no tab {id}"))
                    .hit_rect
            })
            .collect()
    }

    #[test]
    fn ordinary_and_split_tab_selection_stays_visible_across_themes() {
        use lgui::core::UiScale;
        struct RestoreTheme(theme::ThemeId);
        impl Drop for RestoreTheme {
            fn drop(&mut self) {
                theme::set(self.0);
            }
        }
        fn luminance(color: lgui::prelude::Color) -> f64 {
            let channel = |shift: u32| {
                let value = f64::from((color.0 >> shift) & 255_u32) / 255.0;
                if value <= 0.04045 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
        }
        let _restore = RestoreTheme(theme::current());
        let (mut session, view, state, tabs, viewport) = strip_harness(1400.0, 3);
        let mut sibling = None;
        tabs.update(|tabs| sibling = tabs.split(1));
        let sibling = sibling.unwrap();
        let contrast = |a: lgui::prelude::Color, b: lgui::prelude::Color| {
            let (a, b) = (luminance(a), luminance(b));
            (a.max(b) + 0.05) / (a.min(b) + 0.05)
        };
        for id in theme::ThemeId::all() {
            theme::set(id);
            let palette = theme::c();
            // The selected tab's block is the strong fill and the others' the faint
            // one, in every theme and whether or not the terminal has focus.
            for focused in [true, false] {
                state.update(|app| app.terminal.ui.terminal_focused = focused);
                for (selected, others) in [(3, vec![1, sibling, 2]), (sibling, vec![1, 3, 2])] {
                    tabs.update(|tabs| tabs.select(selected));
                    session.render_view(&view, viewport, UiScale::ONE);
                    let block = |tab: u64| {
                        session
                            .tree()
                            .node(&UiId::owned(format!("terminal-tab-background-{tab}")))
                            .map(|node| node.style.fill)
                    };
                    assert_eq!(
                        block(selected),
                        Some(Some(palette.active_tab)),
                        "{} selected tab {selected}, focused {focused}",
                        id.id()
                    );
                    for other in others {
                        assert_eq!(
                            block(other),
                            Some(Some(palette.active_line)),
                            "{} unselected tab {other} has the faint block",
                            id.id()
                        );
                    }
                    assert!(
                        session
                            .tree()
                            .node(&UiId::new("terminal-tab-group-1"))
                            .is_none(),
                        "no background joins a split group"
                    );
                }
            }
            assert!(
                contrast(palette.active_tab, palette.sidebar) >= 1.1,
                "{} the selected block is indistinguishable from the strip",
                id.id()
            );
            let link = session
                .tree()
                .node(&UiId::new("terminal-tab-link-1"))
                .expect("split members are joined by a line")
                .style
                .fill
                .unwrap();
            assert!(
                contrast(link, palette.sidebar) >= 1.5,
                "{} the split line blends into the strip",
                id.id()
            );
        }
    }

    #[test]
    fn tab_widths_follow_the_name_and_never_selection_hover_or_programs() {
        use lgui::core::{InputEvent, PointerData, UiScale};
        let (mut session, view, state, tabs, viewport) = strip_harness(1400.0, 4);
        let ids = [1_u64, 2, 3, 4];
        let baseline = tab_rects(&session, &ids);
        // Short names all get the minimum width, a gap apart, equal selecting areas.
        assert!(
            baseline
                .windows(2)
                .all(|w| (w[1].left - w[0].left - (100.0 + TAB_STRIP_GAP)).abs() < 0.01)
        );
        assert!(
            baseline
                .iter()
                .all(|r| (r.width() - baseline[0].width()).abs() < 0.01)
        );
        let render = |session: &mut lgui::session::UiSession| {
            session.render_view(&view, viewport, UiScale::ONE);
        };

        // Selecting another tab moves nothing.
        tabs.update(|tabs| tabs.select(3));
        render(&mut session);
        assert_eq!(tab_rects(&session, &ids), baseline);
        // Neither does a program starting or exiting in one of them.
        tabs.update(|tabs| tabs.update_programs(&[(2, Some("cargo watch -x run".into()))]));
        render(&mut session);
        assert_eq!(tab_rects(&session, &ids), baseline);
        // A longer name does widen its own tab, up to the maximum, and only that.
        let long = "a long session name that cannot fit in its tab";
        tabs.update(|tabs| tabs.set_title(3, Some(long)));
        render(&mut session);
        let named = tab_rects(&session, &ids);
        assert!(named[2].width() > baseline[2].width());
        assert_eq!(named[0], baseline[0]);
        assert_eq!(named[1], baseline[1]);

        // The long name is cut in the tab for the active and inactive alike, and is
        // shown in full only while the pointer rests on the tab.
        let full = tabs.get().tabs()[2].label();
        let nodes_with_full = |session: &lgui::session::UiSession| {
            session
                .tree()
                .nodes()
                .iter()
                .filter(|n| n.text.as_deref() == Some(full.as_str()))
                .count()
        };
        assert_eq!(nodes_with_full(&session), 0);
        let target = named[2];
        let over = Point::new(
            (target.left + target.right) / 2.0,
            (target.top + target.bottom) / 2.0,
        );
        session.handle_input(InputEvent::PointerMove(PointerData::mouse(over)));
        render(&mut session);
        assert_eq!(nodes_with_full(&session), 1);
        assert!(
            session
                .tree()
                .node(&UiId::new("terminal-tab-tip-3"))
                .is_some()
        );
        // Hovering moved nothing either.
        assert_eq!(tab_rects(&session, &ids), named);
        session.handle_input(InputEvent::PointerMove(PointerData::mouse(Point::new(
            600.0, 150.0,
        ))));
        render(&mut session);
        assert_eq!(nodes_with_full(&session), 0);
        let _ = state;
    }

    #[test]
    fn tabs_scroll_with_a_list_of_every_terminal_when_they_cannot_shrink_further() {
        use crate::ui::terminal::state::TerminalTabMenuKind;
        use lgui::application::ApplicationContext;
        use lgui::core::{InputEvent, PointerData, UiScale, dispatch_runtime_output};
        let context = ApplicationContext::empty(Default::default());
        // Four short names fit at the minimum width: no scrolling and no list.
        let (session, _, _, _, _) = strip_harness(1400.0, 4);
        let rects = tab_rects(&session, &[1, 2, 3, 4]);
        assert!(
            rects
                .windows(2)
                .all(|w| (w[1].left - w[0].left - (100.0 + TAB_STRIP_GAP)).abs() < 0.01)
        );
        assert!(
            named_try(&session, "All terminals").is_none(),
            "no list while everything fits"
        );

        // Twelve tabs cannot squeeze to the floor: the strip scrolls at 100px each,
        // a gap apart, and shows a list.
        let (mut session, view, state, tabs, viewport) = strip_harness(1200.0, 12);
        let rects = tab_rects(&session, &[1, 2, 3, 4, 5, 6]);
        assert!(
            rects
                .windows(2)
                .all(|w| (w[1].left - w[0].left - (100.0 + TAB_STRIP_GAP)).abs() < 0.01)
        );
        let list = named_point(&session, "All terminals");
        let click = |session: &mut lgui::session::UiSession, point: Point| {
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
                let events = session.handle_input(event);
                dispatch_runtime_output(
                    events,
                    &context,
                    &lgui::window::WindowId::new("terminal-list-test"),
                    |action| session.handle_default_action(action),
                    |_| {},
                );
                session.render_view(&view, viewport, UiScale::ONE);
            }
        };
        click(&mut session, list);
        let menu = state.get().terminal.ui.terminal_tab_context_menu.unwrap();
        assert_eq!(menu.kind, TerminalTabMenuKind::Sessions);
        // Picking a terminal from the list selects it and closes the list.
        let last = tabs.get().tabs().last().unwrap().id;
        let row = session
            .tree()
            .node(&UiId::owned(format!("terminal-session-row-{last}")))
            .expect("a row for every terminal")
            .hit_rect;
        click(
            &mut session,
            Point::new((row.left + row.right) / 2.0, (row.top + row.bottom) / 2.0),
        );
        assert_eq!(tabs.get().active_id(), Some(last));
        assert!(state.get().terminal.ui.terminal_tab_context_menu.is_none());
        // The reveal runs as an effect after that frame, so the next frame shows it.
        session.runtime().run_effects();
        session.render_view(&view, viewport, lgui::core::UiScale::ONE);
        // Selecting it scrolled the strip so the tab is in view.
        let strip_right = list.x - 4.0;
        let shown = tab_rects(&session, &[last])[0];
        assert!(
            shown.left >= 0.0 && shown.left < strip_right,
            "{shown:?} vs {strip_right}"
        );
    }

    /// The center of the control with this accessible name, if there is one.
    fn named_try(session: &lgui::session::UiSession, name: &str) -> Option<Point> {
        session
            .tree()
            .nodes()
            .iter()
            .find(|n| n.semantics.as_ref().and_then(|s| s.name.as_deref()) == Some(name))
            .map(|n| {
                Point::new(
                    (n.layout_rect.left + n.layout_rect.right) * 0.5,
                    (n.layout_rect.top + n.layout_rect.bottom) * 0.5,
                )
            })
    }

    #[test]
    fn focused_terminal_cursor_follows_blink_phase() {
        assert!(cursor_is_drawn(true, true, true));
        assert!(!cursor_is_drawn(true, true, false));
        assert!(cursor_is_drawn(true, false, false));
        assert!(!cursor_is_drawn(false, true, true));
    }

    fn key(key: LogicalKey, modifiers: lgui::core::KeyModifiers) -> KeyboardEvent {
        KeyboardEvent {
            state: KeyState::Down,
            key,
            modifiers,
            ..Default::default()
        }
    }

    #[test]
    fn modified_navigation_keys_use_xterm_sequences() {
        use lgui::core::KeyModifiers;
        let left = LogicalKey::Named(NamedKey::ArrowLeft);
        assert_eq!(
            key_bytes(&key(left.clone(), KeyModifiers::empty()), false).unwrap(),
            b"\x1b[D"
        );
        assert_eq!(
            key_bytes(&key(left.clone(), KeyModifiers::empty()), true).unwrap(),
            b"\x1bOD"
        );
        assert_eq!(
            key_bytes(&key(left, KeyModifiers::CONTROL), false).unwrap(),
            b"\x1b[1;5D"
        );
        assert_eq!(
            key_bytes(
                &key(LogicalKey::Named(NamedKey::Delete), KeyModifiers::CONTROL),
                false
            )
            .unwrap(),
            b"\x1b[3;5~"
        );
        assert_eq!(
            key_bytes(
                &key(
                    LogicalKey::Named(NamedKey::Backspace),
                    KeyModifiers::CONTROL
                ),
                false
            )
            .unwrap(),
            b"\x08"
        );
    }

    #[test]
    fn pointer_maps_to_the_nearest_cell_boundary() {
        let content = UiRect::new(10.0, 20.0, 400.0, 200.0);
        let point = |x, y| Point { x, y };
        assert_eq!(cell_at(content, point(10.0, 20.0), 5, 40), (0, 0));
        assert_eq!(
            cell_at(
                content,
                point(
                    10.0 + terminal_cell_width() * 2.6,
                    20.0 + terminal_line_h() * 1.5
                ),
                5,
                40
            ),
            (1, 3)
        );
        assert_eq!(cell_at(content, point(-50.0, 900.0), 5, 40), (4, 0));
        assert_eq!(cell_at(content, point(9_000.0, 0.0), 5, 40), (0, 40));
    }

    #[test]
    fn terminal_resize_height_is_clamped_to_its_allowed_range() {
        assert_eq!(resized_height(600.0, 450.0, 420.0), 150.0);
        assert_eq!(resized_height(600.0, 550.0, 420.0), theme::TERMINAL_MIN_H);
        assert_eq!(resized_height(600.0, 100.0, 420.0), 420.0);
    }

    #[test]
    fn pty_size_tracks_the_visible_terminal_body() {
        let small = pty_size(UiRect::new(0.0, 0.0, 400.0, 160.0));
        let large = pty_size(UiRect::new(0.0, 0.0, 800.0, 320.0));
        assert!(large.cols > small.cols);
        assert!(large.rows > small.rows);
        assert!(small.rows > 0 && small.cols > 1);
    }
}
