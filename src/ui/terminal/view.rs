//! Bottom terminal panel backed by a real platform PTY.

use super::keys;

use super::bindings as commands;
use super::bindings::{
    Binding, Command, FrameRequest, SessionFrame as TerminalTab, Sessions, View as TerminalTabs,
};
use super::find;
use super::header;
use super::layout_binding::{MapBinding, MapCommand};
use super::state::TerminalTabMenuKind;
use super::strip;

use lgui::core::{
    CursorIcon, EventPolicy, IconStyle, PhysicalRect, Point, PointerButton, SemanticRole,
    Semantics, UiElement, UiEventKind, UiEventPayload, UiFocusHandle, UiId, WheelUnit, clip,
};
use lgui::prelude::{Color, Element, UiRect, VisualStyle, component, group, panel, text};
use lgui::services::ServicesContextExt;

use crate::features::terminal::SessionId;
use crate::features::terminal::{TerminalRun, TerminalSize};
use crate::ui::components::scrollbar;
use crate::ui::editor::view::{scroll_from_thumb, thumb_geometry};
use crate::ui::fonts;
use crate::ui::tabs::{GAP as TAB_CONTENT_GAP, PAD as TAB_PAD, TEXT_MARGIN, ellipsize, measure};
use crate::ui::terminal::layout_binding::StripDrag;
use crate::ui::terminal::state::SelectionDrag;
use crate::ui::theme;

const HEADER_H: f32 = theme::TABS_H;
/// How much wider the search button's box is on its right than the other tools'.
const SEARCH_BOX_EXTRA: f32 = 2.0;
/// Narrowest header that still shows the maximize and collapse group.
const PANEL_TOOLS_MIN_W: f32 = 640.0;
/// Status glyph at the start of a session tab, and its gap to the name.
const GLYPH: f32 = 14.0;
const GLYPH_GAP: f32 = 6.0;
/// Room before the first session tab and between tabs. Each tab is a rounded
/// block, so neighbours need space between them; a split group's members are
/// joined by a line across that space.
pub(crate) const TAB_STRIP_PAD: f32 = 6.0;
pub(crate) const TAB_STRIP_GAP: f32 = 6.0;
/// A tab's rounded background is shorter than the strip, which stays the hit
/// area of the whole tab.
pub(crate) const TAB_BLOCK_H: f32 = 20.0;
pub(crate) const TAB_BLOCK_RADIUS: f32 = 5.0;
/// Opacity of an unselected, unhovered tab's block.
pub(crate) const TAB_BLOCK_REST_ALPHA: u8 = 110;
const BODY_PAD_X: f32 = 12.0;
/// The right edge is nearly flush: the overlay scrollbar draws over the text
/// instead of reserving columns, as in the editor.
const BODY_PAD_RIGHT: f32 = 0.0;
const BODY_PAD_Y: f32 = 6.0;
const RESIZE_HIT_H: f32 = 8.0;
const SHELL_MENU_W: f32 = 172.0;
const SHELL_MENU_ROW_H: f32 = 28.0;

/// Observe the whole window so leaving a session also hides its scrollbar.
pub(crate) fn track_hover(
    root: Element,
    state: Binding,
    sessions: Vec<(SessionId, UiRect)>,
) -> Element {
    root.on_event_capture(UiEventKind::PointerMove, move |_, payload| {
        if let UiEventPayload::PointerMove { pointer } = payload {
            let hovered = sessions
                .iter()
                .find(|(_, rect)| rect.contains(pointer.point))
                .map(|(id, _)| *id);
            state.ui.try_update(|ui| {
                let changed = ui.hovered_terminal != hovered;
                ui.hovered_terminal = hovered;
                changed
            });
        }
    })
}

pub(crate) fn terminal_cell_width() -> f32 {
    fonts::terminal().char_w
}

pub(crate) fn terminal_line_h() -> f32 {
    fonts::terminal().line_h
}

pub(crate) fn icon(id: &'static str, key: &'static str, rect: UiRect, color: Color) -> Element {
    Element::new(move |cx| UiElement::icon(cx.id, rect, key).icon_style(IconStyle::new(color)))
        .key(id)
}

pub(crate) fn pty_size(rect: UiRect) -> TerminalSize {
    pty_size_for_body(UiRect::new(
        rect.left,
        rect.top + HEADER_H,
        rect.right,
        rect.bottom,
    ))
}

#[cfg(test)]
pub(crate) fn pane_rects(rect: UiRect, count: usize) -> Vec<UiRect> {
    weighted_pane_rects(rect, &vec![1.0; count])
}

pub(crate) fn session_rects(rect: UiRect, sessions: &[TerminalTab]) -> Vec<UiRect> {
    weighted_pane_rects(
        rect,
        &sessions
            .iter()
            .map(|session| session.width_weight)
            .collect::<Vec<_>>(),
    )
}

pub(crate) fn weighted_pane_rects(rect: UiRect, weights: &[f32]) -> Vec<UiRect> {
    let count = weights.len();
    if count == 0 {
        return Vec::new();
    }
    let minimum = crate::ui::pane_sash::PANE_MIN
        .width
        .min(rect.width().max(0.0) / count as f32);
    let remaining = (rect.width() - minimum * count as f32).max(0.0);
    let total: f32 = weights.iter().sum();
    let mut left = rect.left;
    (0..count)
        .map(|index| {
            let right = if index + 1 == count {
                rect.right
            } else {
                left + minimum + remaining * weights[index] / total
            };
            let pane = UiRect::new(
                left + if index > 0 { 1.0 } else { 0.0 },
                rect.top + HEADER_H,
                right,
                rect.bottom,
            );
            left = right;
            pane
        })
        .collect()
}

pub(crate) fn pty_size_for_body(rect: UiRect) -> TerminalSize {
    let cell_width = terminal_cell_width();
    let body_w = (rect.width() - BODY_PAD_X - BODY_PAD_RIGHT).max(cell_width);
    let body_h = (rect.height() - BODY_PAD_Y * 2.0).max(terminal_line_h());
    TerminalSize {
        rows: (body_h / terminal_line_h())
            .floor()
            .clamp(1.0, u16::MAX as f32) as u16,
        cols: (body_w / cell_width).floor().clamp(2.0, u16::MAX as f32) as u16,
        pixel_width: cell_width.round().max(1.0) as u16,
        pixel_height: terminal_line_h().round() as u16,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_view(
    rect: UiRect,
    state: Binding,
    map: MapBinding,
    editor_focus: UiFocusHandle,
    terminal_focus: UiFocusHandle,
    terminal_id: UiId,
    controller: SessionId,
    terminal_tabs: Sessions,
    application: FrameRequest,
    cursor_blink_visible: bool,
    max_height: f32,
    region_view: Option<(u64, TerminalTabs, bool)>,
) -> Element {
    let _scope = header::Scope::enter(region_view.as_ref().map(|view| view.0));
    let app_state = state.get();
    let primary = region_view.as_ref().is_none_or(|v| v.2);
    let terminal_focused = app_state.ui.terminal_focused && primary;
    let shell_menu_open = app_state.ui.terminal_shell_menu && primary;
    let snapshot = terminal_tabs.screen(controller);
    let terminal_tab_snapshot = region_view
        .as_ref()
        .map(|v| v.1.clone())
        .unwrap_or_else(|| terminal_tabs.get());
    let active_tab_id = terminal_tab_snapshot
        .active_id()
        .expect("a visible terminal panel must have an active tab");
    let tab_items = terminal_tab_snapshot.tabs().to_vec();
    let header = UiRect::new(rect.left, rect.top + 1.0, rect.right, rect.top + HEADER_H);
    let body = UiRect::new(rect.left, header.bottom, rect.right, rect.bottom);
    let sessions = terminal_tab_snapshot.active_group();
    let body_rects = session_rects(rect, &sessions);
    let active_index = sessions
        .iter()
        .position(|session| session.id == active_tab_id)
        .unwrap_or(0);
    let active_body = body_rects.get(active_index).copied().unwrap_or(body);
    let active_content = UiRect::new(
        active_body.left + BODY_PAD_X,
        active_body.top + BODY_PAD_Y,
        active_body.right - BODY_PAD_RIGHT,
        active_body.bottom - BODY_PAD_Y,
    );
    let cursor_rect = terminal_cursor_rect(
        active_content,
        snapshot.cursor,
        snapshot.rows,
        snapshot.cols,
    );
    let mut terminal = Element::new(move |cx| {
        UiElement::panel(terminal_id, rect, VisualStyle::filled(theme::c().bg))
            .ime_cursor_rect(cursor_rect)
            .semantics(Semantics::new(SemanticRole::TextInput).name("Terminal"))
            .children(cx.children)
    })
    .event_policy(EventPolicy::INTERACTIVE);
    let input_tabs = terminal_tabs.clone();
    let input_application = application.clone();
    terminal = terminal.on_input(move |cx, input| {
        if !input.is_empty() {
            commands::write(&input_tabs, active_tab_id, input.as_bytes());
            input_application.request_frame();
            cx.stop_propagation();
        }
    });

    let key_tabs = terminal_tabs.clone();
    let key_application = application.clone();
    let application_cursor = snapshot.application_cursor;
    let win32_input = snapshot.win32_input_mode;
    let key_state = state.clone();
    terminal = terminal.on_key_down(move |cx, event| {
        if let Some(bytes) = keys::key_bytes_in_mode(event, application_cursor, win32_input) {
            if win32_input {
                key_state
                    .ui
                    .update(|ui| ui.pressed_keys.press(active_tab_id, event));
            }
            if keys::is_modifier(event) {
                commands::protocol_input(&key_tabs, active_tab_id, &bytes);
            } else {
                commands::write(&key_tabs, active_tab_id, &bytes);
            }
            key_application.request_frame();
            cx.prevent_default();
            cx.stop_propagation();
        }
    });

    let release_state = state.clone();
    let release_tabs = terminal_tabs.clone();
    terminal = terminal.on_key_up(move |cx, event| {
        let mut releases = Vec::new();
        release_state.ui.try_update(|ui| {
            releases = ui.pressed_keys.release(event);
            !releases.is_empty()
        });
        for (target, event) in releases {
            if release_tabs.screen(target).win32_input_mode
                && let Some(bytes) = keys::key_bytes_in_mode(&event, application_cursor, true)
            {
                commands::protocol_input(&release_tabs, target, &bytes);
                cx.prevent_default();
                cx.stop_propagation();
            }
        }
    });

    let focus_state = state.clone();
    terminal = terminal.on_focus(move |_cx| {
        focus_state.run(Command::Focus);
    });
    let blur_state = state.clone();
    let blur_tabs = terminal_tabs.clone();
    terminal = terminal.on_blur(move |_cx| {
        let mut releases = Vec::new();
        blur_state.ui.update(|ui| {
            ui.terminal_focused = false;
            releases = ui.pressed_keys.release_all();
        });
        for (target, event) in releases {
            if blur_tabs.screen(target).win32_input_mode
                && let Some(bytes) = keys::key_bytes_in_mode(&event, application_cursor, true)
            {
                commands::protocol_input(&blur_tabs, target, &bytes);
            }
        }
    });

    terminal = terminal.child(panel(header, VisualStyle::filled(theme::c().sidebar)));

    let selected_shell = app_state.ui.terminal_shell_selection.and_then(|kind| {
        app_state
            .shell_options
            .iter()
            .find(|option| option.kind == kind && option.available())
    });
    let new_shell = selected_shell.map(|option| option.kind);
    let running = tab_items
        .iter()
        .find(|tab| tab.id == active_tab_id)
        .is_some_and(|tab| {
            tab.activity == crate::features::terminal::activity::CommandActivity::Running
        });
    // Only the default bottom region has maximize and collapse. The app always
    // renders regions, so compare with the default one; a plain render (no
    // region) is the default panel too.
    let owns_panel_tools = region_view
        .as_ref()
        .is_none_or(|(region, _, _)| app_state.default_region == Some(*region));
    let tools = header::layout(
        header,
        header::Spec {
            running,
            // A narrow header gives the room to the tabs instead of the panel group.
            panel: owns_panel_tools && header.width() >= PANEL_TOOLS_MIN_W,
        },
    );
    let tool_color = theme::c().text_soft;
    let menu_target = app_state
        .ui
        .terminal_tab_context_menu
        .as_ref()
        .filter(|menu| menu.target == active_tab_id);
    let more_open = menu_target.is_some_and(|menu| menu.kind == TerminalTabMenuKind::More);

    if let Some(stop) = tools.stop {
        let stop_tabs = terminal_tabs.clone();
        let stop_focus = terminal_focus.clone();
        let stop_application = application.clone();
        terminal = terminal.child(header::button(
            "terminal-stop",
            stop,
            "terminal-stop",
            "Stop running program",
            false,
            theme::c().error,
            move || {
                // The same byte a keyboard Ctrl+C sends to the foreground program.
                commands::write(&stop_tabs, active_tab_id, &[0x03]);
                stop_application.request_frame();
                stop_focus.focus();
            },
        ));
    }
    let add_tabs = terminal_tabs.clone();
    let add_state = state.clone();
    let add_focus = terminal_focus.clone();
    // The chosen shell has no visible label any more, so name it for assistive
    // technology and tests.
    let new_name = selected_shell.map_or_else(
        || "New terminal".to_owned(),
        |option| format!("New terminal: {}", option.name),
    );
    terminal = terminal.child(header::button(
        "terminal-new-tab",
        tools.new,
        "plus",
        new_name,
        false,
        tool_color,
        move || {
            if commands::new_from_target(&add_state, &add_tabs, active_tab_id, new_shell).is_none()
            {
                return;
            }
            add_focus.focus();
        },
    ));
    let menu_tabs = terminal_tabs.clone();
    let menu_state = state.clone();
    let menu_focus = terminal_focus.clone();
    terminal = terminal.child(header::button(
        "terminal-shell-menu",
        tools.shell,
        "chevron-down",
        "Choose shell",
        shell_menu_open,
        tool_color,
        move || {
            if !commands::shell_menu(&menu_state, &menu_tabs, active_tab_id) {
                return;
            }
            menu_focus.focus();
        },
    ));
    let split_tabs = terminal_tabs.clone();
    let split_focus = terminal_focus.clone();
    terminal = terminal.child(header::button(
        "terminal-split",
        tools.split,
        "terminal-split",
        "Split terminal",
        sessions.len() > 1,
        tool_color,
        move || {
            if commands::split(&split_tabs, active_tab_id).is_some() {
                split_focus.focus();
            }
        },
    ));
    let find_open = app_state
        .ui
        .terminal_search
        .as_ref()
        .is_some_and(|bar| bar.target == active_tab_id);
    let find_state = state.clone();
    // The magnifier's mass sits toward its handle, leaving more empty space on
    // the left of the box; the box grows on the right while the glyph stays put.
    let search_box = UiRect::new(
        tools.search.left,
        tools.search.top,
        tools.search.right + SEARCH_BOX_EXTRA,
        tools.search.bottom,
    );
    terminal = terminal.child(header::button_in(
        "terminal-search",
        search_box,
        tools.search,
        "search",
        "Search terminal output",
        find_open,
        tool_color,
        move || {
            find_state.run(Command::ToggleSearch(active_tab_id));
        },
    ));
    let more_state = state.clone();
    let more_anchor = tools.more;
    terminal = terminal.child(header::button(
        "terminal-more",
        tools.more,
        "terminal-more",
        "More terminal actions",
        more_open,
        tool_color,
        move || {
            more_state.run(Command::OpenMenu {
                target: active_tab_id,
                // The menu aligns its right edge with the button.
                position: (more_anchor.right, more_anchor.bottom + 2.0),
                kind: TerminalTabMenuKind::More,
            });
        },
    ));
    if let Some(maximize) = tools.maximize {
        let maximized = app_state.maximized;
        let maximize_state = state.clone();
        let maximize_focus = terminal_focus.clone();
        terminal = terminal.child(header::button(
            "terminal-maximize",
            maximize,
            if maximized {
                "terminal-restore"
            } else {
                "terminal-maximize"
            },
            if maximized {
                "Restore terminal size"
            } else {
                "Maximize terminal"
            },
            maximized,
            tool_color,
            move || {
                maximize_state.run(Command::ToggleMaximize);
                maximize_focus.focus();
            },
        ));
    }
    if let Some(collapse) = tools.collapse {
        let collapse_state = state.clone();
        let collapse_editor = editor_focus.clone();
        terminal = terminal.child(header::button(
            "terminal-collapse",
            collapse,
            "terminal-minimize",
            "Hide terminal panel",
            false,
            tool_color,
            move || {
                if collapse_state.run(Command::CollapsePanel).applied {
                    collapse_editor.focus();
                }
            },
        ));
    }
    for center in &tools.rules {
        terminal = terminal.child(header::rule(header, *center));
    }

    // Categories belong to the shared default dock, rather than detached sessions.
    let mut tab_strip_left = rect.left;
    if owns_panel_tools && header.width() >= 360.0 {
        let gap = if header.width() < PANEL_TOOLS_MIN_W {
            8.0
        } else {
            12.0
        };
        let (category, right) = categories(
            header,
            state.clone(),
            terminal_focus.clone(),
            editor_focus.clone(),
        );
        terminal = terminal
            .child(category)
            .child(header::rule(header, right + gap));
        tab_strip_left = right + gap * 2.0;
    }
    let tab_strip = UiRect::new(
        tab_strip_left,
        header.top,
        (tools.left - 8.0).max(tab_strip_left),
        header.bottom,
    );
    let tab_strip_element = render_terminal_tabs(
        tab_strip,
        &tab_items,
        active_tab_id,
        terminal_tabs.clone(),
        state.clone(),
        map.clone(),
        terminal_focus.clone(),
        editor_focus.clone(),
        application.clone(),
    );

    for (session, session_rect) in sessions.iter().zip(body_rects.iter().copied()) {
        let active = session.id == active_tab_id;
        let id = UiId::owned(format!("terminal-session-{}", session.id));
        terminal = terminal.child(render_session(
            session_rect,
            state.clone(),
            terminal_tabs.clone(),
            terminal_focus.clone(),
            id,
            session.id,
            session.id,
            terminal_focused && active,
            application.clone(),
            cursor_blink_visible,
        ));
    }

    if let Some(bar) = app_state
        .ui
        .terminal_search
        .as_ref()
        .filter(|bar| bar.target == active_tab_id)
    {
        terminal = terminal.child(find::render(
            active_body,
            state.clone(),
            bar,
            terminal_focus.clone(),
        ));
    }

    // Paint one full-width rule after the clipped tab contents and session surfaces.
    terminal = terminal
        .child(tab_strip_element.contents)
        .child(tab_header_rule(header))
        // The list button and tooltips sit outside the strip's clip.
        .child(tab_strip_element.extras);

    // Add handles after all session surfaces so they win hit testing.
    for index in 0..sessions.len().saturating_sub(1) {
        let left_id = sessions[index].id;
        let right_id = sessions[index + 1].id;
        let pair_left = body_rects[index].left - if index > 0 { 1.0 } else { 0.0 };
        let pair_right = body_rects[index + 1].right;
        let minimum = crate::ui::pane_sash::PANE_MIN
            .width
            .min(rect.width() / sessions.len() as f32);
        let remaining = (rect.width() - minimum * sessions.len() as f32).max(0.0);
        let down = state.clone();
        let moving = state.clone();
        let up = state.clone();
        let tabs = terminal_tabs.clone();
        terminal = terminal.child(crate::ui::pane_sash::horizontal(
            format!("terminal-split-{left_id}-{right_id}"),
            body_rects[index].right + 0.5,
            (body.top, body.bottom),
            app_state.ui.terminal_split_drag == Some(left_id),
            move || {
                down.ui
                    .update(|app| app.terminal_split_drag = Some(left_id))
            },
            move |x| {
                if moving.get().ui.terminal_split_drag != Some(left_id) || remaining <= 0.0 {
                    return;
                }
                let position = crate::ui::pane_sash::split_position(pair_left, pair_right, x);
                let pair_extra = (pair_right - pair_left - minimum * 2.0).max(0.0);
                if pair_extra > 0.0 {
                    let fraction =
                        ((position - pair_left - minimum) / pair_extra).clamp(0.001, 0.999);
                    commands::resize_pair(&tabs, left_id, right_id, fraction);
                }
            },
            move || up.ui.update(|app| app.terminal_split_drag = None),
        ));
    }

    if shell_menu_open {
        terminal = terminal.child(shell_menu(
            tools.shell,
            rect,
            state.clone(),
            terminal_focus.clone(),
        ));
    }

    if region_view.is_some() {
        return terminal;
    }
    resize_panel(terminal, rect, state, max_height)
}

pub(crate) fn resize_panel(
    mut terminal: Element,
    rect: UiRect,
    state: Binding,
    max_height: f32,
) -> Element {
    let resizing = state.get().chrome.resizing_terminal;
    let handle = UiRect::new(
        rect.left,
        rect.top - RESIZE_HIT_H / 2.0,
        rect.right,
        rect.top + RESIZE_HIT_H / 2.0,
    );
    let panel_bottom = rect.bottom;
    let resize_down = state.clone();
    let resize_drag = state.clone();
    let resize_up = state;
    terminal = terminal.child(
        crate::ui::components::resize_handle::render(
            "terminal-resize-handle",
            handle,
            UiRect::new(rect.left, rect.top, rect.right, rect.top + 1.0),
            CursorIcon::ResizeVertical,
            resizing,
        )
        .on_pointer_down_with_button(move |_cx, _pointer, button| {
            if button == PointerButton::Left {
                resize_down.run(Command::ResizeStart);
            }
        })
        .on_pointer_drag(move |_cx, pointer| {
            resize_drag.run(Command::Resize(resized_height(
                panel_bottom,
                pointer.point.y,
                max_height,
            )));
        })
        .on_pointer_up(move |_cx, _pointer| {
            resize_up.run(Command::ResizeEnd);
        }),
    );

    terminal
}

pub(crate) fn categories(
    header: UiRect,
    state: Binding,
    terminal_focus: UiFocusHandle,
    editor_focus: UiFocusHandle,
) -> (Element, f32) {
    let selected = state.get().ui.panel_tab;
    header::category(
        header,
        selected,
        std::sync::Arc::new(move |panel| {
            if state.get().ui.panel_tab != panel {
                state.run(Command::SelectPanel(panel));
            }
            if panel == super::state::PanelTab::Terminal {
                terminal_focus.focus();
            } else {
                editor_focus.focus();
            }
        }),
    )
}

/// Common dock header while its content is the existing commit history.
pub(crate) fn commits_header(
    rect: UiRect,
    state: Binding,
    terminal_focus: UiFocusHandle,
    editor_focus: UiFocusHandle,
    can_maximize: bool,
    toolbar: impl FnOnce(UiRect) -> Element,
) -> Element {
    let header = UiRect::new(rect.left, rect.top + 1.0, rect.right, rect.top + HEADER_H);
    let (category, right) = categories(header, state.clone(), terminal_focus, editor_focus.clone());
    let tools = header::layout(
        header,
        header::Spec {
            running: false,
            panel: true,
        },
    );
    let mut root = panel(header, VisualStyle::filled(theme::c().sidebar))
        .key("commits-panel-header")
        .child(crate::ui::components::divider::top(rect))
        .child(category)
        .child(header::rule(header, right + 12.0))
        .child(toolbar(UiRect::new(
            right + 24.0,
            header.top,
            (tools.left - 8.0).max(right + 24.0),
            header.bottom,
        )));
    if can_maximize {
        let maximized = state.get().maximized;
        let maximize = state.clone();
        root = root.child(header::button(
            "commits-maximize",
            tools.maximize.unwrap(),
            if maximized {
                "terminal-restore"
            } else {
                "terminal-maximize"
            },
            if maximized {
                "Restore panel size"
            } else {
                "Maximize panel"
            },
            false,
            theme::c().text_soft,
            move || {
                maximize.run(Command::ToggleMaximize);
            },
        ));
    }
    let collapse = state;
    root.child(header::button(
        "commits-collapse",
        tools.collapse.unwrap(),
        "terminal-minimize",
        "Hide bottom panel",
        false,
        theme::c().text_soft,
        move || {
            if collapse.run(Command::CollapsePanel).applied {
                editor_focus.focus();
            }
        },
    ))
    .child(tab_header_rule(header))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_session(
    body: UiRect,
    state: Binding,
    terminal_tabs: Sessions,
    terminal_focus: UiFocusHandle,
    terminal_id: UiId,
    controller: SessionId,
    session_id: u64,
    focused: bool,
    application: FrameRequest,
    cursor_blink_visible: bool,
) -> Element {
    let rect = body;
    let snapshot = terminal_tabs.screen(controller);
    let content = UiRect::new(
        body.left + BODY_PAD_X,
        body.top + BODY_PAD_Y,
        body.right - BODY_PAD_RIGHT,
        body.bottom - BODY_PAD_Y,
    );
    let cursor_rect = terminal_cursor_rect(content, snapshot.cursor, snapshot.rows, snapshot.cols);
    let mut semantics = Semantics::new(SemanticRole::TextInput)
        .name(format!("{} terminal", snapshot.shell_name))
        .description("Interactive integrated terminal");
    semantics.state.multiline = true;

    let mut terminal = Element::new(move |cx| {
        UiElement::panel(terminal_id, rect, VisualStyle::filled(theme::c().bg))
            .ime_cursor_rect(cursor_rect)
            .children(cx.children)
    })
    .event_policy(EventPolicy {
        hover: true,
        press: true,
        focus: false,
    })
    .semantics(semantics);

    let focus_on_click = terminal_focus.clone();
    let click_tabs = terminal_tabs.clone();
    let click_state = state.clone();
    terminal = terminal.on_click(move || {
        commands::select(&click_state, &click_tabs, session_id);
        focus_on_click.focus();
    });

    let (screen_rows, screen_cols) = (snapshot.rows, snapshot.cols);
    // lgui wheel payloads contain the delta only. Retain the latest pointer
    // cell in the session controller across redraws for application reports.
    let mouse_tabs = terminal_tabs.clone();
    let mouse_state = state.clone();
    let mouse_application = application.clone();
    terminal = terminal.on_pointer_move(move |_cx, pointer| {
        let (row, col) = mouse_cell_at(content, pointer.point, screen_rows, screen_cols);
        commands::mouse_position(&mouse_tabs, session_id, row, col);
        // Underline the link under the pointer while Ctrl (Cmd) is held.
        let modifiers = mouse_state.get().modifiers;
        let hover = (modifiers.ctrl() || modifiers.meta()).then_some((row, col));
        if commands::link_hover(&mouse_tabs, session_id, hover) {
            mouse_application.request_frame();
        }
    });
    let select_input_tabs = terminal_tabs.clone();
    let select_tabs = terminal_tabs.clone();
    let select_focus = terminal_focus.clone();
    let select_state = state.clone();
    let select_application = application.clone();
    terminal = terminal.on_pointer_down_with_button(move |cx, pointer, button| {
        if !content.contains(pointer.point) {
            return;
        }
        if !commands::select(&select_state, &select_tabs, session_id) {
            return;
        }
        select_focus.focus();
        let (row, col) = cell_at(content, pointer.point, screen_rows, screen_cols);
        match button {
            PointerButton::Left => {
                let (cell_row, cell) =
                    mouse_cell_at(content, pointer.point, screen_rows, screen_cols);
                let modifiers = select_state.get().modifiers;
                // Ctrl-click (Cmd on macOS) opens a link; elsewhere it selects.
                if (modifiers.ctrl() || modifiers.meta())
                    && commands::open_link(&select_input_tabs, session_id, cell_row, cell)
                {
                    cx.stop_propagation();
                    return;
                }
                let extend = select_state.get().modifiers.shift();
                commands::press_selection(&select_input_tabs, session_id, row, col, cell, extend);
                select_state.ui.update(|app| {
                    app.selecting_terminal = true;
                    app.selection_drag = Some(SelectionDrag {
                        session: session_id,
                        content,
                        point: pointer.point,
                        rows: screen_rows,
                        cols: screen_cols,
                    });
                });
            }
            // Right click copies a selection, or pastes when there is none.
            PointerButton::Right => {
                let clipboard = cx.application().clipboard();
                let result = commands::clipboard(
                    &select_input_tabs,
                    session_id,
                    commands::ClipboardShortcut::CopyOrPaste,
                    clipboard.as_ref(),
                );
                commands::report_clipboard_error(&select_state, result);
            }
            _ => return,
        }
        select_application.request_frame();
    });

    let drag_tabs = terminal_tabs.clone();
    let drag_state = state.clone();
    let drag_application = application.clone();
    terminal = terminal.on_pointer_drag(move |cx, pointer| {
        if drag_state.get().ui.selecting_terminal {
            let (row, col) = cell_at(content, pointer.point, screen_rows, screen_cols);
            commands::extend_selection(&drag_tabs, session_id, row, col);
            let point = pointer.point;
            drag_state
                .ui
                .try_update(|app| match app.selection_drag.as_mut() {
                    Some(drag) if drag.point != point => {
                        drag.point = point;
                        true
                    }
                    _ => false,
                });
            drag_application.request_frame();
            cx.stop_propagation();
        }
    });
    let select_up_state = state.clone();
    terminal = terminal.on_pointer_up(move |_cx, _pointer| {
        select_up_state.ui.try_update(|app| {
            app.selection_drag = None;
            std::mem::take(&mut app.selecting_terminal)
        });
    });

    let wheel_tabs = terminal_tabs.clone();
    let wheel_application = application.clone();
    let wheel_state = state.clone();
    terminal = terminal.on_wheel(move |cx, delta| {
        let lines = match delta.unit {
            WheelUnit::Lines => delta.y,
            WheelUnit::Pixels => delta.y / (terminal_line_h() * 3.0),
        };
        if lines.is_finite() && lines != 0.0 {
            commands::wheel(
                &wheel_tabs,
                session_id,
                lines,
                keys::modifiers(wheel_state.get().modifiers),
            );
            wheel_application.request_frame();
            cx.stop_propagation();
        }
    });

    // Default cursors inherit in lgui, so keep the text cursor off the
    // scrollbar's ancestors and apply it only to the screen content.
    // A hand over a Ctrl-hovered link shows that a click opens it.
    let screen_cursor = if snapshot.link.is_some() {
        CursorIcon::Pointer
    } else {
        CursorIcon::Text
    };
    terminal = terminal.child(
        render_screen(content, &snapshot, focused, cursor_blink_visible).cursor(screen_cursor),
    );
    let ui = state.get().ui;
    let emphasized = ui.hovered_terminal == Some(session_id)
        || ui
            .terminal_scrollbar_drag
            .is_some_and(|(id, _)| id == session_id);
    if snapshot.scrollback_rows > 0 || emphasized {
        terminal = terminal.child(vertical_scrollbar(
            body,
            &snapshot,
            state,
            terminal_tabs,
            terminal_focus,
            session_id,
            application,
            emphasized,
        ));
    }
    terminal
}

fn vertical_scrollbar(
    body: UiRect,
    snapshot: &crate::features::terminal::TerminalSnapshot,
    state: Binding,
    tabs: Sessions,
    focus: UiFocusHandle,
    session: SessionId,
    application: FrameRequest,
    emphasized: bool,
) -> Element {
    let track = UiRect::new(
        body.right - scrollbar::SIZE,
        body.top,
        body.right,
        body.bottom,
    );
    if track.height() <= 0.0 {
        return group(body);
    }
    let history = snapshot.scrollback_rows;
    let rows = usize::from(snapshot.rows);
    // Scroll offsets count rows from the oldest line, as in the editor.
    let scroll = (history - snapshot.scrollback.min(history)) as f32;
    let geometry = thumb_geometry(
        track.top,
        track.height(),
        rows as f32,
        (history + rows) as f32,
        scroll,
    );
    let travel = track.height() - geometry.length;
    let alpha = if emphasized { 0xb8 } else { 0x68 };
    // Keep the pressed node alive until release if a program switches screens
    // or clears history during the drag. An empty rect paints no scrollbar.
    let thumb_rect = if history == 0 {
        UiRect::new(track.right, track.top, track.right, track.top)
    } else {
        UiRect::new(
            track.left + scrollbar::INSET,
            geometry.start,
            track.right - scrollbar::INSET,
            geometry.start + geometry.length,
        )
    };
    let scroll_to = {
        let tabs = tabs.clone();
        let application = application.clone();
        move |pointer_y: f32, offset: f32| {
            let from_top =
                scroll_from_thumb(pointer_y - track.top - offset, travel, history as f32);
            tabs.run(Command::Scrollback {
                target: session,
                rows: history.saturating_sub(from_top.round() as usize),
            });
            application.request_frame();
        }
    };
    let drag_scroll_to = scroll_to.clone();
    let press = {
        let state = state.clone();
        let tabs = tabs.clone();
        move |cx: &mut lgui::core::UiEventContext, pointer_y: f32, offset: f32, jump: bool| {
            commands::select(&state, &tabs, session);
            focus.focus();
            state.ui.update(|ui| {
                ui.selecting_terminal = false;
                ui.selection_drag = None;
                ui.terminal_scrollbar_drag = Some((session, offset));
            });
            if jump {
                scroll_to(pointer_y, offset);
            }
            cx.stop_propagation();
        }
    };
    let drag = {
        let state = state.clone();
        let scroll_to = drag_scroll_to;
        move |cx: &mut lgui::core::UiEventContext, pointer_y: f32| {
            if let Some((id, offset)) = state.get().ui.terminal_scrollbar_drag
                && id == session
                && travel > 0.0
            {
                scroll_to(pointer_y, offset);
                cx.stop_propagation();
            }
        }
    };
    let release = move |cx: &mut lgui::core::UiEventContext| {
        state.ui.try_update(|ui| {
            if ui
                .terminal_scrollbar_drag
                .is_some_and(|(id, _)| id == session)
            {
                ui.terminal_scrollbar_drag = None;
                true
            } else {
                false
            }
        });
        cx.stop_propagation();
    };
    let policy = EventPolicy {
        hover: true,
        press: true,
        focus: false,
    };

    let (track_press, track_drag, track_release) = (press.clone(), drag.clone(), release.clone());
    let track_hit = scrollbar::track(track)
        .key(format!("terminal-scrollbar-track-{session}"))
        .event_policy(policy)
        .on_pointer_down_with_button(move |cx, pointer, button| {
            if button == PointerButton::Left {
                // Jump with the thumb centred on the pointer, then keep dragging.
                track_press(cx, pointer.point.y, geometry.length / 2.0, true);
            }
        })
        .on_pointer_drag(move |cx, pointer| track_drag(cx, pointer.point.y))
        .on_pointer_up(move |cx, _| track_release(cx));
    let thumb = scrollbar::thumb(thumb_rect, alpha)
        .key(format!("terminal-scrollbar-thumb-{session}"))
        .event_policy(policy)
        .on_pointer_down_with_button(move |cx, pointer, button| {
            if button == PointerButton::Left {
                press(cx, pointer.point.y, pointer.point.y - geometry.start, false);
            }
        })
        .on_pointer_drag(move |cx, pointer| drag(cx, pointer.point.y))
        .on_pointer_up(move |cx, _| release(cx));
    group(track)
        .key(format!("terminal-scrollbar-{session}"))
        .child(track_hit)
        .child(thumb)
}

pub(crate) struct TabStrip {
    pub(crate) contents: Element,
    /// What belongs outside the strip's clip: the full-list button and the
    /// hover tooltips.
    pub(crate) extras: Element,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_terminal_tabs(
    rect: UiRect,
    tabs: &[TerminalTab],
    active_id: u64,
    terminal_tabs: Sessions,
    state: Binding,
    map: MapBinding,
    terminal_focus: UiFocusHandle,
    editor_focus: UiFocusHandle,
    application: FrameRequest,
) -> TabStrip {
    let mut strip = panel(rect, VisualStyle::filled(theme::c().sidebar));
    if tabs.is_empty() || rect.width() <= 0.0 {
        return TabStrip {
            contents: strip,
            extras: group(UiRect::new(rect.left, rect.top, rect.left, rect.top)),
        };
    }

    let labels = tabs.iter().map(TerminalTab::label).collect::<Vec<_>>();
    let close_width = measure("✕", theme::SMALL, 400);
    // A tab is as wide as its name asks, between the minimum and the maximum
    // width. The name leaves out the running program, so selecting, hovering or
    // a program starting or exiting never moves a tab or its close button.
    let fixed_width =
        TAB_PAD + GLYPH + GLYPH_GAP + TAB_CONTENT_GAP + close_width + TAB_PAD + TEXT_MARGIN;
    let naturals = tabs
        .iter()
        .map(|tab| fixed_width + measure(&stable_name(tab), theme::UI_SIZE, 400))
        .collect::<Vec<_>>();
    let metrics = strip::layout(rect.width(), &naturals, TAB_STRIP_PAD, TAB_STRIP_GAP);
    let strip_key = tabs[0].id;
    let scroll = state
        .get()
        .ui
        .tab_scroll(strip_key)
        .clamp(0.0, metrics.max_scroll);
    // The tab being renamed holds a text field instead of its name.
    let renaming = state
        .get()
        .ui
        .terminal_rename
        .as_ref()
        .map(|editor| editor.target);
    let widths = metrics.widths.clone();
    let viewport = UiRect::new(
        rect.left,
        rect.top,
        rect.left + metrics.viewport_w,
        rect.bottom,
    );
    let mut tips: Vec<(UiId, UiId, UiId, Element)> = Vec::new();
    let strip_drag = {
        let mut left = rect.left + TAB_STRIP_PAD - scroll;
        let slots = tabs
            .iter()
            .zip(&widths)
            .map(|(tab, width)| {
                let slot = (tab.id, left, left + width);
                left += width + TAB_STRIP_GAP;
                slot
            })
            .collect();
        StripDrag { rect, tabs: slots }
    };
    // Lines that join the members of a split group across the gap between their
    // rounded blocks; added after the tabs.
    let mut links: Vec<Element> = Vec::new();
    let mut left = rect.left + TAB_STRIP_PAD - scroll;
    for (index, tab) in tabs.iter().enumerate() {
        let name = &labels[index];
        let active = tab.id == active_id;
        let tab_width = widths[index];
        let tab_rect = UiRect::new(left, rect.top, left + tab_width, rect.bottom);
        let joins_next = tabs
            .get(index + 1)
            .is_some_and(|next| next.group_id == tab.group_id);
        // The rounded block, centred in the strip and shorter than it.
        let block_inset = ((tab_rect.height() - TAB_BLOCK_H) / 2.0).max(0.0);
        let block_rect = UiRect::new(
            tab_rect.left,
            tab_rect.top + block_inset,
            tab_rect.right,
            tab_rect.bottom - block_inset,
        );
        if joins_next {
            let link_id = tab.id;
            links.push(
                Element::new(move |cx| {
                    let scale = cx.context.scale();
                    let middle = scale.physical_value((block_rect.top + block_rect.bottom) / 2.0);
                    let line = scale.logical_rect(PhysicalRect::new(
                        scale.physical_value(block_rect.right),
                        middle,
                        scale.physical_value(block_rect.right + TAB_STRIP_GAP),
                        middle + scale.physical_length(1.0),
                    ));
                    UiElement::panel(
                        UiId::owned(format!("terminal-tab-link-{link_id}")),
                        line,
                        VisualStyle::filled(theme::c().text_faint),
                    )
                    .hit_rect(UiRect::new(line.left, line.top, line.left, line.top))
                })
                .key(format!("terminal-tab-link-{link_id}")),
            );
        }
        let name_left = tab_rect.left + TAB_PAD + GLYPH + GLYPH_GAP;
        let close_left = tab_rect.right - TAB_PAD - close_width;
        let close_rect = UiRect::new(
            close_left,
            tab_rect.top,
            tab_rect.right - TAB_PAD,
            tab_rect.bottom,
        );
        let name_slot_width = (close_left - TAB_CONTENT_GAP - name_left).max(0.0);
        // A long name is cut to the same slot in every tab, active or not; the
        // tooltip shows it in full.
        let display_name = ellipsize(
            name,
            (name_slot_width - TEXT_MARGIN).max(0.0),
            theme::UI_SIZE,
            400,
        );
        if tab_rect.right > viewport.left && tab_rect.left < viewport.right {
            let tip_id = UiId::owned(format!("terminal-tab-tip-{}", tab.id));
            tips.push((
                UiId::owned(format!("terminal-tab-select-{}", tab.id)),
                UiId::owned(format!("terminal-tab-close-{}", tab.id)),
                tip_id.clone(),
                tab_tooltip(tip_id, rect, tab_rect, tab),
            ));
        }
        let label_rect = UiRect::new(
            name_left,
            tab_rect.top,
            name_left + name_slot_width,
            tab_rect.bottom,
        );
        let mut visual = panel(tab_rect, VisualStyle::default());
        let editing = renaming == Some(tab.id);
        if editing {
            visual = visual.child(theme::bordered(
                block_rect,
                theme::c().active_tab,
                theme::c().accent,
                TAB_BLOCK_RADIUS,
                1.0,
            ));
        }
        // Editing a name leaves only the field inside a focus border.
        if !editing {
            let glyph_top = tab_rect.top + (tab_rect.height() - GLYPH) / 2.0;
            let glyph_left = tab_rect.left + TAB_PAD;
            visual = visual.child(session_glyph(
                tab.id,
                UiRect::new(glyph_left, glyph_top, glyph_left + GLYPH, glyph_top + GLYPH),
                tab.activity == crate::features::terminal::activity::CommandActivity::Running,
            ));
        }
        if !editing {
            visual = visual.child(text(
                label_rect,
                display_name,
                theme::mono(
                    if active {
                        theme::c().text_bright
                    } else {
                        theme::c().text_muted
                    },
                    theme::UI_SIZE,
                ),
            ));
        }
        let select_tabs = terminal_tabs.clone();
        let select_focus = terminal_focus.clone();
        let press_state = state.clone();
        let select_map = map.clone();
        let select_strip = strip_drag.clone();
        let menu_state = state.clone();
        let tab_id = tab.id;
        let close_id = UiId::owned(format!("terminal-tab-close-{tab_id}"));
        let close_node_id = close_id.clone();
        let select_node_id = UiId::owned(format!("terminal-tab-select-{tab_id}"));
        let mut tab_element = Element::new(move |cx| {
            let id = UiId::owned(format!("terminal-tab-{tab_id}"));
            let hovered = cx.context.interaction_flags(&id).hovered
                || cx.context.interaction_flags(&select_node_id).hovered
                || cx.context.interaction_flags(&close_node_id).hovered;
            // Every tab is a rounded block: the selected one in the strong fill, a
            // hovered one in the hover fill and the rest faintly, so a split
            // group's line always joins two visible blocks.
            let (fill, alpha) = if active || editing {
                (theme::c().active_tab, 255)
            } else if hovered {
                (theme::c().active_line, 255)
            } else {
                (theme::c().active_line, TAB_BLOCK_REST_ALPHA)
            };
            let scale = cx.context.scale();
            UiElement::panel(id, tab_rect, VisualStyle::default())
                .child(
                    UiElement::panel(
                        UiId::owned(format!("terminal-tab-background-{tab_id}")),
                        scale.logical_rect(scale.physical_rect(block_rect)),
                        VisualStyle::filled(fill)
                            .alpha(alpha)
                            .radius(TAB_BLOCK_RADIUS),
                    )
                    .hit_rect(UiRect::new(
                        block_rect.left,
                        block_rect.top,
                        block_rect.left,
                        block_rect.top,
                    )),
                )
                .children(cx.children)
        })
        .key(format!("terminal-tab-{tab_id}"))
        // Padding and the gap before the close button belong to the tab, so
        // they share its hand instead of showing an arrow.
        .cursor(CursorIcon::Pointer)
        .event_policy(EventPolicy {
            hover: true,
            press: false,
            focus: false,
        })
        .on_event_capture(lgui::core::UiEventKind::PointerDown, move |cx, payload| {
            if let lgui::core::UiEventPayload::PointerDown {
                pointer,
                button: PointerButton::Right,
            } = payload
            {
                menu_state.run(Command::OpenMenu {
                    target: tab_id,
                    position: (pointer.point.x, pointer.point.y),
                    kind: super::state::TerminalTabMenuKind::Actions,
                });
                cx.stop_propagation();
            }
        })
        .child(visual);
        if renaming == Some(tab_id) {
            // Renaming replaces selecting and closing: the field fills the tab.
            tab_element = tab_element.child(super::rename::input(
                UiRect::new(
                    tab_rect.left + 6.0,
                    tab_rect.top + 4.0,
                    tab_rect.right - 6.0,
                    tab_rect.bottom - 4.0,
                ),
                state.clone(),
                terminal_focus.clone(),
            ));
            strip = strip.child(tab_element);
            left += tab_width + TAB_STRIP_GAP;
            continue;
        }
        let select_rect = UiRect::new(
            tab_rect.left,
            tab_rect.top,
            (close_left - TAB_CONTENT_GAP).max(tab_rect.left),
            tab_rect.bottom,
        );
        // The pressed handle owns the cursor while dragging. As its own
        // component it follows the map's cursor without re-rendering the strip.
        tab_element = tab_element.child(
            component(select_rect, move |cx, &select_rect| {
                let cursor = cx.use_observable(
                    select_map.observable(),
                    |s: &super::layout_binding::MapSignal| s.cursor,
                );
                let capture_map = select_map.clone();
                Element::new(move |cx| {
                    let id = UiId::owned(format!("terminal-tab-select-{tab_id}"));
                    if capture_map
                        .with(|d| d.is_some_and(|d| d.session == tab_id && d.release.is_none()))
                        && !cx.context.interaction_flags(&id).pressed
                    {
                        capture_map.cancel();
                    }
                    UiElement::panel(id, select_rect, VisualStyle::default()).children(cx.children)
                })
                .key(format!("terminal-tab-select-{tab_id}"))
                .event_policy(EventPolicy {
                    hover: true,
                    press: true,
                    focus: false,
                })
                .cursor(cursor)
                .on_pointer_down_with_button(move |cx, pointer, button| {
                    if button == PointerButton::Left {
                        if !commands::select(&press_state, &select_tabs, tab_id) {
                            return;
                        }
                        select_focus.focus();
                        select_map.run(MapCommand::Begin {
                            session: tab_id,
                            point: pointer.point,
                            strip: Some(select_strip.clone()),
                        });
                        cx.stop_propagation();
                    }
                })
            })
            .key(format!("terminal-tab-select-component-{tab_id}")),
        );

        let close_tabs = terminal_tabs.clone();
        let close_state = state.clone();
        let close_terminal_focus = terminal_focus.clone();
        let close_editor_focus = editor_focus.clone();
        let close_application = application.clone();
        tab_element = tab_element.child(
            Element::new(move |cx| {
                UiElement::panel(close_id, close_rect, VisualStyle::default()).children(cx.children)
            })
            .key(format!("terminal-tab-close-{tab_id}"))
            .event_policy(EventPolicy::INTERACTIVE)
            .cursor(CursorIcon::Pointer)
            .on_click(move || {
                if !commands::close(
                    &close_state,
                    &close_tabs,
                    tab_id,
                    crate::features::terminal::actions::MenuAction::Close,
                    &close_application,
                ) {
                    return;
                }
                let closing_last_tab = close_tabs.get().is_empty();
                if closing_last_tab {
                    close_editor_focus.focus();
                } else {
                    close_terminal_focus.focus();
                }
            })
            .child(text(
                UiRect::new(
                    close_left,
                    tab_rect.top,
                    close_left + close_width + TEXT_MARGIN,
                    tab_rect.bottom,
                ),
                "✕",
                theme::mono(theme::c().text_dim, theme::SMALL),
            )),
        );

        strip = strip.child(tab_element);
        left += tab_width + TAB_STRIP_GAP;
    }
    for link in links {
        strip = strip.child(link);
    }

    if metrics.scrolls() {
        let wheel_state = state.clone();
        let max = metrics.max_scroll;
        strip = strip.on_wheel(move |cx, delta| {
            let primary = if delta.x.abs() > delta.y.abs() {
                delta.x
            } else {
                delta.y
            };
            let step = match delta.unit {
                WheelUnit::Lines => primary * 48.0,
                WheelUnit::Pixels => primary,
            };
            // Wheel down or right moves toward the later tabs.
            wheel_state
                .ui
                .update(|ui| ui.scroll_tabs(strip_key, -step, max));
            cx.stop_propagation();
        });
    }
    // Selecting a tab that is out of view, or a changed room or count, brings the
    // active tab into view once; scrolling by hand afterwards is left alone.
    let active_index = tabs.iter().position(|t| t.id == active_id).unwrap_or(0);
    let reveal_state = state.clone();
    let reveal_layout = metrics.clone();
    strip = strip.child(
        component(rect, move |cx, _| {
            let state = reveal_state.clone();
            let layout = reveal_layout.clone();
            cx.use_effect(
                (
                    strip_key,
                    active_id,
                    active_index,
                    (layout.widths.iter().sum::<f32>() * 10.0) as i32,
                    (layout.viewport_w * 10.0) as i32,
                ),
                move || {
                    state.ui.update(|ui| {
                        let current = ui.tab_scroll(strip_key);
                        let next = strip::reveal(
                            current,
                            &layout,
                            active_index,
                            TAB_STRIP_PAD,
                            TAB_STRIP_GAP,
                        );
                        ui.set_tab_scroll(strip_key, next);
                    });
                },
            );
            group(UiRect::new(rect.left, rect.top, rect.left, rect.top))
        })
        .key(format!("terminal-tab-reveal-{strip_key}")),
    );

    // Outside the clip: the full-list button and the hover tooltips.
    let mut extras = group(UiRect::new(rect.left, rect.top, rect.left, rect.top));
    if metrics.scrolls() {
        let button = UiRect::new(
            viewport.right + 2.0,
            rect.top + 3.0,
            rect.right - 2.0,
            rect.bottom - 3.0,
        );
        let list_state = state.clone();
        let list_open = state
            .get()
            .ui
            .terminal_tab_context_menu
            .as_ref()
            .is_some_and(|menu| menu.kind == TerminalTabMenuKind::Sessions);
        extras = extras.child(header::button(
            "terminal-sessions",
            button,
            "terminal-list",
            "All terminals",
            list_open,
            theme::c().text_soft,
            move || {
                list_state.run(Command::OpenMenu {
                    target: active_id,
                    // The list aligns its right edge with the button.
                    position: (button.right, button.bottom + 2.0),
                    kind: TerminalTabMenuKind::Sessions,
                });
            },
        ));
    }
    if renaming.is_none() {
        // Every tooltip is a child, but only the one of the hovered tab is kept.
        let hover_ids = tips
            .iter()
            .map(|(select, close, tip, _)| (select.clone(), close.clone(), tip.clone()))
            .collect::<Vec<_>>();
        let mut tooltips = Element::new(move |cx| {
            let hovered = hover_ids
                .iter()
                .find(|(select, close, _)| {
                    cx.context.interaction_flags(select).hovered
                        || cx.context.interaction_flags(close).hovered
                })
                .map(|(_, _, tip)| tip.clone());
            UiElement::group(cx.id, rect).children(
                cx.children
                    .into_iter()
                    .filter(|child| Some(&child.node().id) == hovered.as_ref()),
            )
        })
        .key("terminal-tab-tips");
        for (_, _, _, tip) in tips {
            tooltips = tooltips.child(tip);
        }
        extras = extras.child(tooltips);
    }

    TabStrip {
        contents: clip(viewport, 0.0, 0.0).child(strip),
        extras,
    }
}

/// The name that decides a tab's width: the user's name or the shell's label,
/// and the purpose. The running program is left out because it comes and goes.
fn stable_name(tab: &TerminalTab) -> String {
    let name = tab.title_override.as_deref().unwrap_or(&tab.short_label);
    match tab.purpose.as_deref() {
        Some(purpose) => format!("{name} · {purpose}"),
        None => name.to_owned(),
    }
}

/// The card shown under a hovered tab with its full name and where it runs.
fn tab_tooltip(id: UiId, strip: UiRect, tab_rect: UiRect, tab: &TerminalTab) -> Element {
    let title = tab.label();
    let directory = tab.resource_directory.clone().or_else(|| {
        tab.working_directory()
            .map(|path| path.display().to_string())
    });
    let detail = match directory {
        Some(dir) => format!("{} · {}", tab.short_label, dir),
        None => tab.short_label.clone(),
    };
    let width = (measure(&title, theme::SMALL, 400).max(measure(&detail, theme::SMALL, 400))
        + 20.0)
        .clamp(120.0, 520.0);
    let left = tab_rect.left.max(strip.left);
    let card = UiRect::new(
        left,
        strip.bottom + 4.0,
        left + width,
        strip.bottom + 4.0 + 44.0,
    );
    let line = |top: f32, content: String, color| {
        text(
            UiRect::new(card.left + 9.0, top, card.right - 9.0, top + 18.0),
            ellipsize(&content, width - 18.0, theme::SMALL, 400),
            theme::mono(color, theme::SMALL),
        )
    };
    let body = theme::bordered(card, theme::c().surface, theme::c().border, 4.0, 1.0)
        .child(line(card.top + 5.0, title, theme::c().text))
        .child(line(card.top + 23.0, detail, theme::c().text_muted));
    Element::new(move |cx| UiElement::group(id.clone(), card).children(cx.children))
        .key("terminal-tab-tip")
        .child(body)
}

/// A spinner while a program runs in the session, a quiet dot otherwise.
fn session_glyph(id: u64, rect: UiRect, running: bool) -> Element {
    if running {
        return crate::ui::components::loading_indicator::render(
            rect,
            theme::c().diff.add_mark,
            format!("terminal-spin-{id}"),
        );
    }
    let (x, y) = (
        (rect.left + rect.right) / 2.0,
        (rect.top + rect.bottom) / 2.0,
    );
    panel(
        UiRect::new(x - 3.0, y - 3.0, x + 3.0, y + 3.0),
        VisualStyle::filled(theme::c().text_dim).radius(3.0),
    )
}

// Paint the header's bottom edge outside the fractional tab clip without
// intercepting selecting or dragging a tab.
pub(crate) fn tab_header_rule(header: UiRect) -> Element {
    Element::new(move |cx| {
        let no_hit = UiRect::new(header.left, header.bottom, header.left, header.bottom);
        cx.compile(crate::ui::components::divider::bottom(header))
            .hit_rect(no_hit)
    })
}
fn cell_fill(rect: UiRect, color: Color) -> Element {
    Element::new(move |cx| {
        // Shared cell edges must land on the same physical pixel. Otherwise
        // adjacent antialiased fills blend with the canvas and leave row seams.
        let scale = cx.context.scale();
        let snap = |value| scale.physical_value(value) as f32 / scale.factor();
        UiElement::panel(
            cx.id,
            UiRect::new(
                snap(rect.left),
                snap(rect.top),
                snap(rect.right),
                snap(rect.bottom),
            ),
            VisualStyle::filled(color),
        )
    })
}

pub(crate) fn render_screen(
    content: UiRect,
    snapshot: &crate::features::terminal::TerminalSnapshot,
    focused: bool,
    cursor_blink_visible: bool,
) -> Element {
    let mut screen = group(content);
    for (row, runs) in snapshot.lines.iter().enumerate() {
        let top = content.top + row as f32 * terminal_line_h();
        if top >= content.bottom {
            break;
        }
        for run in runs {
            if let Some(background) = run.style.background {
                let (left, right) = run_span(content, run);
                screen = screen.child(cell_fill(
                    UiRect::new(left, top, right, top + terminal_line_h()),
                    Color(background),
                ));
            }
        }
        // Selection sits above cell backgrounds and below the glyphs.
        for &(_, start_col, end_col) in snapshot
            .selection
            .iter()
            .filter(|(selected_row, ..)| usize::from(*selected_row) == row)
        {
            let left = content.left + f32::from(start_col) * terminal_cell_width();
            let right =
                (content.left + f32::from(end_col) * terminal_cell_width()).min(content.right);
            screen = screen.child(cell_fill(
                UiRect::new(left, top, right, top + terminal_line_h()),
                theme::c().selection,
            ));
        }
        for run in runs {
            let (left, right) = run_span(content, run);
            let run_rect = UiRect::new(left, top, right, top + terminal_line_h());
            if !run.text.is_empty() {
                let foreground = run.style.foreground.map(Color).unwrap_or(theme::c().text);
                let mut style = if run.style.bold {
                    fonts::terminal().bold(foreground)
                } else {
                    fonts::terminal().style(foreground)
                };
                if run.style.dim {
                    style = style.alpha(0x90);
                }
                // Keep a terminal run on one row even when shaping/rounding
                // makes its natural width slightly exceed the allocated cells.
                let measured = crate::ui::components::text::SingleLineText::new(
                    run.text.clone(),
                    UiRect::new(left, top, left + 100_000.0, top + terminal_line_h()),
                    style,
                );
                let text_rect = UiRect::new(
                    left,
                    top,
                    left + measured.width().max(right - left) + 2.0,
                    top + terminal_line_h(),
                );
                screen = screen.child(clip(run_rect, 0.0, 0.0).child(text(
                    text_rect,
                    run.text.clone(),
                    style,
                )));
                if run.style.underline {
                    screen = screen.child(panel(
                        UiRect::new(
                            left,
                            top + terminal_line_h() - 3.0,
                            right,
                            top + terminal_line_h() - 2.0,
                        ),
                        VisualStyle::filled(foreground),
                    ));
                }
            }
        }
    }

    if let Some((row, start, end)) = snapshot.link {
        let top = content.top + f32::from(row) * terminal_line_h();
        let left = content.left + f32::from(start) * terminal_cell_width();
        let right = (content.left + f32::from(end) * terminal_cell_width()).min(content.right);
        screen = screen.child(panel(
            UiRect::new(
                left,
                top + terminal_line_h() - 3.0,
                right,
                top + terminal_line_h() - 2.0,
            ),
            VisualStyle::filled(theme::c().text),
        ));
    }

    if cursor_is_drawn(snapshot.cursor_visible, focused, cursor_blink_visible) {
        let cursor = terminal_cursor_rect(content, snapshot.cursor, snapshot.rows, snapshot.cols);
        let cursor_style = if focused {
            VisualStyle::filled(theme::c().text_soft).alpha(0xc8)
        } else {
            VisualStyle::default().stroked(theme::hairline(theme::c().text_dim))
        };
        screen = screen.child(panel(cursor, cursor_style));
    }

    if let Some(message) = snapshot
        .status
        .message()
        .or_else(|| snapshot.notice.clone())
    {
        let top = (content.bottom - terminal_line_h()).max(content.top);
        screen = screen.child(text(
            UiRect::new(content.left, top, content.right, top + terminal_line_h()),
            message,
            theme::mono(theme::c().text_dim, theme::SMALL),
        ));
    }

    clip(content, 0.0, 0.0).child(screen)
}

pub(crate) fn run_span(content: UiRect, run: &TerminalRun) -> (f32, f32) {
    let cell_width = terminal_cell_width();
    let left = content.left + f32::from(run.start_col) * cell_width;
    let right = (left + f32::from(run.columns) * cell_width + 2.0).min(content.right + 2.0);
    (left, right)
}

/// Maps a pointer to the nearest cell boundary, clamped to the screen.
pub(crate) fn cell_at(content: UiRect, point: Point, rows: u16, cols: u16) -> (u16, u16) {
    let row = ((point.y - content.top) / terminal_line_h())
        .floor()
        .clamp(0.0, f32::from(rows.saturating_sub(1)));
    let col = ((point.x - content.left) / terminal_cell_width())
        .round()
        .clamp(0.0, f32::from(cols));
    (row as u16, col as u16)
}

/// History rows to scroll per tick while a selection drag rests outside the
/// screen: positive reveals older output, negative newer. Zero inside.
pub(crate) fn selection_scroll_rows(content: UiRect, y: f32) -> i32 {
    let line = terminal_line_h();
    let past = if y < content.top {
        content.top - y
    } else if y > content.bottom {
        -(y - content.bottom)
    } else {
        return 0;
    };
    let rows = ((past.abs() / line).ceil() as i32).clamp(1, 8);
    if past < 0.0 { -rows } else { rows }
}

pub(crate) fn mouse_cell_at(content: UiRect, point: Point, rows: u16, cols: u16) -> (u16, u16) {
    let row = ((point.y - content.top) / terminal_line_h())
        .floor()
        .clamp(0.0, f32::from(rows.saturating_sub(1)));
    let col = ((point.x - content.left) / terminal_cell_width())
        .floor()
        .clamp(0.0, f32::from(cols.saturating_sub(1)));
    (row as u16, col as u16)
}

pub(crate) fn cursor_is_drawn(pty_visible: bool, focused: bool, blink_visible: bool) -> bool {
    pty_visible && (!focused || blink_visible)
}

pub(crate) fn shell_menu(
    instance_rect: UiRect,
    bounds: UiRect,
    state: Binding,
    focus: UiFocusHandle,
) -> Element {
    let frame = state.get();
    let options = frame.shell_options;
    let space = (bounds.bottom - instance_rect.bottom).max(instance_rect.top - bounds.top);
    let visible = (((space - 12.0) / SHELL_MENU_ROW_H).floor() as usize)
        .clamp(1, 7)
        .min(options.len().max(1));
    let offset = state
        .get()
        .ui
        .terminal_shell_scroll
        .min(options.len().saturating_sub(visible));
    let height = 8.0 + SHELL_MENU_ROW_H * options.len().min(visible).max(1) as f32;
    let top = if instance_rect.bottom + 4.0 + height <= bounds.bottom {
        instance_rect.bottom + 4.0
    } else {
        (instance_rect.top - 4.0 - height).max(bounds.top)
    };
    let width = SHELL_MENU_W.min(bounds.width().max(0.0));
    let left =
        (instance_rect.right - width).clamp(bounds.left, (bounds.right - width).max(bounds.left));
    let menu_rect = UiRect::new(left, top, left + width, top + height);
    let mut menu = theme::bordered(menu_rect, theme::c().surface, theme::c().border, 4.0, 1.0);
    let scroll_state = state.clone();
    let max_offset = options.len().saturating_sub(visible);
    menu = menu
        .event_policy(EventPolicy::INTERACTIVE)
        .on_wheel(move |cx, delta| {
            let step = delta.y.abs().ceil().max(1.0) as usize;
            scroll_state.ui.update(|app| {
                app.terminal_shell_scroll = if delta.y > 0.0 {
                    offset.saturating_sub(step)
                } else {
                    offset.saturating_add(step).min(max_offset)
                }
            });
            cx.stop_propagation();
        });
    for (index, option) in options.iter().skip(offset).take(visible).enumerate() {
        let shell = option.kind;
        let top = menu_rect.top + 4.0 + index as f32 * SHELL_MENU_ROW_H;
        let row = UiRect::new(
            menu_rect.left + 4.0,
            top,
            menu_rect.right - 4.0,
            top + SHELL_MENU_ROW_H,
        );
        let row_state = state.clone();
        let row_focus = focus.clone();
        let style = if frame.ui.terminal_shell_selection == Some(shell) {
            VisualStyle::filled(theme::c().active_line)
        } else {
            VisualStyle::default()
        };
        let mut item = panel(row, style.radius(3.0)).key(format!("terminal-shell-{shell:?}"));
        if option.available() {
            item = item
                .event_policy(EventPolicy::INTERACTIVE)
                .cursor(CursorIcon::Pointer)
                .on_click(move || {
                    row_state.ui.update(|app| {
                        app.terminal_shell_selection = Some(shell);
                        app.terminal_shell_menu = false;
                    });
                    row_focus.focus();
                });
        }
        let item = item.child(text(
            UiRect::new(row.left + 8.0, row.top, row.right - 8.0, row.bottom),
            ellipsize(
                &format!(
                    "{}{}",
                    option.name,
                    if option.available() {
                        ""
                    } else {
                        " (unavailable)"
                    }
                ),
                (row.width() - 16.0).max(0.0),
                theme::UI_SIZE,
                400,
            ),
            theme::mono(
                if option.available() {
                    theme::c().text
                } else {
                    theme::c().text_dim
                },
                theme::UI_SIZE,
            ),
        ));
        menu = menu.child(item);
    }
    menu
}

pub(crate) fn terminal_cursor_rect(
    content: UiRect,
    cursor: (u16, u16),
    rows: u16,
    cols: u16,
) -> UiRect {
    let cell_width = terminal_cell_width();
    // vt100 keeps col == cols while a full line waits for the next character
    // to trigger wrapping. The visible cursor stays on the last cell meanwhile.
    let left = content.left + f32::from(cursor.1.min(cols.saturating_sub(1))) * cell_width;
    let top = content.top + f32::from(cursor.0.min(rows.saturating_sub(1))) * terminal_line_h();
    UiRect::new(
        left,
        top + 2.0,
        left + cell_width,
        top + terminal_line_h() - 2.0,
    )
}

pub(crate) fn resized_height(panel_bottom: f32, pointer_y: f32, max_height: f32) -> f32 {
    (panel_bottom - pointer_y).clamp(theme::TERMINAL_MIN_H, max_height)
}
