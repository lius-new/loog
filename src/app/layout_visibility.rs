//! Compose the saved spatial layout with content visibility and live sessions.
use crate::app::context::AppContext;
use crate::app::terminal_binding::TerminalStore;
use crate::domain::application_layout::Content;
use crate::ui::layout_visibility::{Binding, Command, Frame, Region};
use lgui::prelude::UiRect;

pub(crate) type FocusTargets = (
    lgui::core::UiFocusHandle,
    lgui::core::UiFocusHandle,
    TerminalStore,
);

pub(crate) fn open(state: &AppContext) {
    state.try_update(|app| {
        if app.layout.visibility_popup.is_some() {
            return false;
        }
        app.layout.visibility_popup = Some(app.terminal.ui.terminal_focused);
        true
    });
}

fn close(state: &AppContext, focus: Option<&FocusTargets>) {
    let mut restore = None;
    state.try_update(|app| {
        restore = app.layout.visibility_popup.take();
        restore.is_some()
    });
    let (Some(was_terminal), Some((editor, terminal, tabs))) = (restore, focus) else {
        return;
    };
    let app = state.get();
    let Some(layout) = &app.layout.application_layout else {
        return;
    };
    let flags = crate::app::terminal::visibility(&app);
    let terminal_visible = tabs
        .get()
        .active_id()
        .is_some_and(|id| layout.terminal_visible(id, flags));
    let editor_visible = layout.leaves().iter().any(|(id, content)| {
        matches!(content, Content::Editor) && layout.region_visible(*id, flags)
    });
    if terminal_visible && (was_terminal || !editor_visible) {
        terminal.focus();
    } else if editor_visible {
        editor.focus();
    }
}

pub(crate) fn toggle_region(state: &AppContext, id: u64) {
    state.try_update(|app| {
        let flags = crate::app::terminal::visibility(app);
        let Some(layout) = app.layout.application_layout.as_mut() else {
            return false;
        };
        let Some((_, content)) = layout
            .leaves()
            .into_iter()
            .find(|(region, _)| *region == id)
        else {
            return false;
        };
        let show = !layout.region_visible(id, flags);
        // Existing content commands continue to own their visibility flags.
        // Other terminal regions and the editor have independent layout masks.
        match content {
            Content::Git => app.source_control.visible = show,
            Content::Files => app.drawer.visible = show,
            Content::Terminal { .. } if layout.default_terminal_region() == Some(id) => {
                app.terminal_chrome.show_terminal = show;
            }
            _ => {
                layout.set_region_visible(id, show);
            }
        }
        true
    });
}

pub(crate) fn binding(
    state: &AppContext,
    tabs: &TerminalStore,
    source: UiRect,
    focus: FocusTargets,
) -> Binding {
    let reader = state.clone();
    let reader_tabs = tabs.clone();
    let writer = state.clone();
    Binding::new(
        move || {
            let app = reader.get();
            let mut frame = Frame {
                open: app.layout.visibility_popup.is_some(),
                source,
                regions: vec![],
            };
            let Some(layout) = &app.layout.application_layout else {
                return frame;
            };
            let flags = crate::app::terminal::visibility(&app);
            let model = reader_tabs.get();
            let model = model.model();
            frame.regions = layout
                .overview(source.into())
                .regions
                .into_iter()
                .map(|(id, content, rect)| {
                    let label = match content {
                        Content::Editor => "Editor".to_owned(),
                        Content::Git => "Changes".to_owned(),
                        Content::Files => match app.drawer.mode {
                            crate::ui::drawer::DrawerMode::Search => "Search",
                            crate::ui::drawer::DrawerMode::Outline => "Outline",
                            _ => "Files",
                        }
                        .to_owned(),
                        Content::Terminal { tabs, active } => active
                            .or_else(|| tabs.first().copied())
                            .and_then(|active| model.tabs().iter().find(|t| t.id() == active))
                            .map(|tab| {
                                crate::app::terminal_binding::display_title(
                                    tab.shell().short_label(),
                                    tab.title().as_deref(),
                                    tab.purpose().as_deref(),
                                    tab.running_program().as_deref(),
                                )
                            })
                            .unwrap_or_else(|| "Terminal".to_owned()),
                    };
                    Region {
                        id,
                        rect: rect.into(),
                        shown: layout.region_visible(id, flags),
                        label,
                    }
                })
                .collect();
            frame
        },
        move |command| match command {
            Command::Open => open(&writer),
            Command::Close => close(&writer, Some(&focus)),
            Command::Toggle(id) => toggle_region(&writer, id),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::terminal_runtime::TerminalDisplay;
    use crate::app::{
        state::AppState,
        terminal_binding::{TerminalTabs, test_store},
    };
    use crate::domain::application_layout::ApplicationLayout;
    use lgui::application::{AppView, ApplicationContext};
    use lgui::core::{
        InputEvent, KeyboardEvent, LogicalKey, NamedKey, Point, PointerButton, PointerData, UiId,
        UiScale, dispatch_runtime_output,
    };
    use lgui::prelude::group;
    use lgui::session::UiSession;
    use std::sync::{Arc, Mutex};

    fn point(session: &UiSession, name: &str) -> Point {
        let rect = session
            .tree()
            .nodes()
            .iter()
            .find(|node| node.semantics.as_ref().and_then(|s| s.name.as_deref()) == Some(name))
            .unwrap_or_else(|| panic!("Missing {name}"))
            .layout_rect;
        Point::new(
            (rect.left + rect.right) * 0.5,
            (rect.top + rect.bottom) * 0.5,
        )
    }

    #[test]
    fn hover_visibility_popup_keeps_the_bridge_and_body_open_and_closes_on_leave() {
        let viewport = UiRect::new(0.0, 0.0, 1200.0, 800.0);
        let bar = UiRect::new(0.0, 0.0, 1200.0, 30.0);
        let source = UiRect::new(0.0, 30.0, 1200.0, 776.0);
        let exposed = Arc::new(Mutex::new(None::<(AppContext, TerminalStore)>));
        let output = exposed.clone();
        let view: AppView = Arc::new(move |cx| {
            let tabs = test_store(cx, None, || {
                let mut tabs = TerminalTabs::new();
                tabs.split(1);
                tabs
            });
            let state = AppContext::detached(cx.state_with(|| {
                let mut state = AppState::new();
                state.source_control.visible = true;
                state.drawer.visible = true;
                state.terminal_chrome.show_terminal = true;
                let mut layout = ApplicationLayout::initial(source.into(), 220.0, 220.0, 240.0);
                layout.sync(&[(1, 1), (2, 1)], Some(1));
                assert!(layout.split_beside(2, 1));
                state.layout.application_layout = Some(layout);
                state
            }));
            *output.lock().unwrap() = Some((state.clone(), tabs.clone()));
            let focus = (
                cx.focus_handle(UiId::new("visibility-editor")),
                cx.focus_handle(UiId::new("visibility-terminal")),
                tabs.clone(),
            );
            let binding = binding(&state, &tabs, source, focus.clone());
            let anchor = crate::ui::titlebar::layout_toggle_rect(bar);
            let root = crate::ui::layout_visibility::attach(
                group(viewport),
                viewport,
                anchor,
                binding.clone(),
            );
            root.child(crate::ui::titlebar::render(
                bar,
                crate::app::chrome::title_binding(&state),
                group(UiRect::default()),
            ))
            .child(crate::ui::layout_visibility::render(
                viewport, anchor, binding,
            ))
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let (state, tabs) = exposed.lock().unwrap().clone().unwrap();
        let context = ApplicationContext::empty(Default::default());
        let send = |session: &mut UiSession, input| {
            dispatch_runtime_output(
                session.handle_input(input),
                &context,
                &lgui::window::WindowId::new("visibility-test"),
                |action| session.handle_default_action(action),
                |_| {},
            );
            session.render_view(&view, viewport, UiScale::ONE);
        };
        let click = |session: &mut UiSession, p| {
            send(session, InputEvent::PointerMove(PointerData::mouse(p)));
            send(
                session,
                InputEvent::PointerDown {
                    pointer: PointerData::mouse(p),
                    button: PointerButton::Left,
                },
            );
            send(
                session,
                InputEvent::PointerUp {
                    pointer: PointerData::mouse(p),
                    button: PointerButton::Left,
                },
            );
        };
        let icon = point(&session, "Layout view");
        send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(icon)),
        );
        assert!(state.get().layout.visibility_popup.is_some());
        // Clicking an already hovered icon must not immediately hide the popup.
        click(&mut session, icon);
        assert!(state.get().layout.visibility_popup.is_some());
        let popup = crate::ui::layout_visibility::popup_rect(
            viewport,
            crate::ui::titlebar::layout_toggle_rect(bar),
        );
        let gap = Point::new(icon.x, bar.bottom + 3.0);
        send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(gap)),
        );
        assert!(state.get().layout.visibility_popup.is_some());
        assert!(
            session
                .tree()
                .node(&UiId::new("layout-visibility-glyph-1"))
                .is_none()
        );
        let editor = point(&session, "Hide Editor");
        send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(editor)),
        );
        assert!(
            session
                .tree()
                .node(&UiId::new("layout-visibility-glyph-1"))
                .is_some()
        );
        click(&mut session, editor);
        assert!(state.get().layout.visibility_popup.is_some());
        assert!(
            !state
                .get()
                .layout
                .application_layout
                .as_ref()
                .unwrap()
                .region_visible(1, crate::app::terminal::visibility(&state.get()))
        );
        let restore = point(&session, "Show Editor");
        assert_eq!(editor, restore);
        send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(Point::new(
                popup.left + 1.0,
                popup.bottom - 1.0,
            ))),
        );
        assert!(
            session
                .tree()
                .node(&UiId::new("layout-visibility-glyph-1"))
                .is_some()
        );

        let before = tabs
            .get()
            .tabs()
            .into_iter()
            .map(|t| (t.id, t.controller))
            .collect::<Vec<_>>();
        for (_, controller) in &before {
            controller.begin_selection(0, 0);
            controller.extend_selection(0, 2);
        }
        let detached = state
            .get()
            .layout
            .application_layout
            .as_ref()
            .unwrap()
            .terminal_region(2)
            .unwrap();
        toggle_region(&state, detached);
        assert!(
            !state
                .get()
                .layout
                .application_layout
                .as_ref()
                .unwrap()
                .terminal_visible(2, crate::app::terminal::visibility(&state.get()))
        );
        assert_eq!(tabs.get().tabs().len(), before.len());
        assert!(
            tabs.get()
                .tabs()
                .iter()
                .all(|t| before.iter().any(|(id, c)| *id == t.id
                    && c.generation().is_some()
                    && c.generation() == t.controller.generation()
                    && !t.controller.snapshot().selection.is_empty()))
        );
        toggle_region(&state, detached);
        assert!(
            state
                .get()
                .layout
                .application_layout
                .as_ref()
                .unwrap()
                .terminal_visible(2, crate::app::terminal::visibility(&state.get()))
        );
        click(&mut session, restore);
        assert!(
            state
                .get()
                .layout
                .application_layout
                .as_ref()
                .unwrap()
                .region_visible(1, crate::app::terminal::visibility(&state.get()))
        );
        send(
            &mut session,
            InputEvent::Keyboard(KeyboardEvent {
                key: LogicalKey::Named(NamedKey::Escape),
                ..Default::default()
            }),
        );
        assert!(state.get().layout.visibility_popup.is_none());
        send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(icon)),
        );
        assert!(state.get().layout.visibility_popup.is_some());
        send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(Point::new(
                popup.left + 1.0,
                popup.top + 1.0,
            ))),
        );
        assert!(state.get().layout.visibility_popup.is_some());
        send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(Point::new(10.0, 790.0))),
        );
        assert!(state.get().layout.visibility_popup.is_none());
    }
}
