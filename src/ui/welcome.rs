//! Welcome canvas shown when no project or document is open.

use lgui::core::{
    CursorIcon, EventPolicy, SemanticRole, Semantics, UiElement, UiEventKind, UiFocusHandle, UiId,
    clip,
};
use lgui::prelude::{Element, UiRect, VisualStyle, group, panel, text};

use crate::ui::chrome::{HomeBinding, HomeCommand};
use crate::ui::tabs::{TEXT_MARGIN, ellipsize, measure};
use crate::ui::theme;

const MAX_PAGE_W: f32 = 760.0;
const PAGE_PAD_X: f32 = 32.0;
const WIDE_BREAKPOINT: f32 = 600.0;
const COLUMN_GAP: f32 = 48.0;
const SECTION_TITLE_H: f32 = 22.0;
const ACTION_H: f32 = 34.0;
const ACTION_GAP: f32 = 4.0;
const WORKSPACE_CARD_H: f32 = 50.0;
const WORKSPACE_CARD_GAP: f32 = 6.0;
const MAX_VISIBLE_RECENTS: usize = 5;
const FOOTER_H: f32 = 52.0;

#[derive(Clone, Copy)]
enum StartAction {
    OpenFile,
    OpenFolder,
}

const ACTIONS: [(StartAction, &str); 2] = [
    (StartAction::OpenFile, "Open File..."),
    (StartAction::OpenFolder, "Open Folder..."),
];

pub fn render(
    rect: UiRect,
    state: HomeBinding,
    surface_id: UiId,
    editor_focus: UiFocusHandle,
) -> Element {
    let snapshot = state.frame();
    let page_w = (rect.width() - PAGE_PAD_X * 2.0).clamp(0.0, MAX_PAGE_W);
    let page_left = rect.left + (rect.width() - page_w) / 2.0;
    let page_right = page_left + page_w;
    let page_top = rect.top + if rect.height() >= 420.0 { 42.0 } else { 24.0 };

    let clear_hover = state.clone();
    let mut welcome = Element::new(move |cx| {
        UiElement::panel(surface_id, rect, VisualStyle::filled(theme::c().bg))
            .semantics(Semantics::new(SemanticRole::Group).name("Welcome"))
            .children(cx.children)
    })
    .event_policy(EventPolicy::INTERACTIVE)
    .on_event_capture(UiEventKind::PointerMove, move |_cx, _payload| {
        clear_hover.run(HomeCommand::Hover(None));
    });
    welcome = crate::ui::track_main_surface_focus(welcome, state.clone());

    let title_w = measure("Loog", 18.0, 700);
    welcome = welcome.child(text(
        UiRect::new(
            page_left,
            page_top,
            page_left + title_w + TEXT_MARGIN,
            page_top + 26.0,
        ),
        "Loog",
        theme::mono_bold(theme::c().text_bright, 18.0),
    ));
    welcome = welcome.child(text(
        UiRect::new(
            page_left + title_w + 14.0,
            page_top + 2.0,
            page_right,
            page_top + 24.0,
        ),
        format!("v{}", env!("CARGO_PKG_VERSION")),
        theme::mono(theme::c().text_dim, theme::SMALL),
    ));
    welcome = welcome.child(text(
        UiRect::new(page_left, page_top + 28.0, page_right, page_top + 48.0),
        "Open a file or folder to start editing.",
        theme::sans(theme::c().text_dim, theme::UI_SIZE),
    ));

    let sections_top = page_top + 76.0;
    let wide = page_w >= WIDE_BREAKPOINT;
    let (actions_rect, workspace_rect) = if wide {
        let actions_w = (page_w - COLUMN_GAP) * 5.0 / 12.0;
        (
            UiRect::new(page_left, sections_top, page_left + actions_w, rect.bottom),
            UiRect::new(
                page_left + actions_w + COLUMN_GAP,
                sections_top,
                page_right,
                rect.bottom,
            ),
        )
    } else {
        let workspace_top =
            sections_top + SECTION_TITLE_H + ACTIONS.len() as f32 * (ACTION_H + ACTION_GAP) + 24.0;
        (
            UiRect::new(page_left, sections_top, page_right, workspace_top),
            UiRect::new(page_left, workspace_top, page_right, rect.bottom),
        )
    };

    welcome = welcome.child(section_title(actions_rect, "START"));
    let mut action_top = actions_rect.top + SECTION_TITLE_H;
    for (index, (action, label)) in ACTIONS.into_iter().enumerate() {
        let row = UiRect::new(
            actions_rect.left,
            action_top,
            actions_rect.right,
            action_top + ACTION_H,
        );
        welcome = welcome.child(action_row(
            row,
            index,
            action,
            label,
            snapshot.hover == Some(index),
            state.clone(),
            editor_focus.clone(),
        ));
        action_top += ACTION_H + ACTION_GAP;
    }

    let footer_min_height = if wide { 320.0 } else { 440.0 };
    welcome = welcome.child(section_title(workspace_rect, "RECENT"));
    let list_top = workspace_rect.top + SECTION_TITLE_H;
    let list_bottom = if rect.height() >= footer_min_height {
        rect.bottom - FOOTER_H - 12.0
    } else {
        rect.bottom
    };
    let available = (list_bottom - list_top).max(WORKSPACE_CARD_H);
    let visible_count = (((available + WORKSPACE_CARD_GAP)
        / (WORKSPACE_CARD_H + WORKSPACE_CARD_GAP))
        .floor() as usize)
        .clamp(1, MAX_VISIBLE_RECENTS);

    if snapshot.paths.is_empty() {
        welcome = welcome.child(empty_recent_card(UiRect::new(
            workspace_rect.left,
            list_top,
            workspace_rect.right,
            list_top + WORKSPACE_CARD_H,
        )));
    } else {
        let mut card_top = list_top;
        for (recent_index, path) in snapshot.paths.iter().take(visible_count).enumerate() {
            let hover_index = ACTIONS.len() + recent_index;
            let card = UiRect::new(
                workspace_rect.left,
                card_top,
                workspace_rect.right,
                card_top + WORKSPACE_CARD_H,
            );
            welcome = welcome.child(recent_workspace_card(
                card,
                path,
                hover_index,
                snapshot.hover == Some(hover_index),
                state.clone(),
            ));
            card_top += WORKSPACE_CARD_H + WORKSPACE_CARD_GAP;
        }
    }

    if rect.height() >= footer_min_height {
        welcome = welcome.child(footer(UiRect::new(
            page_left,
            rect.bottom - FOOTER_H,
            page_right,
            rect.bottom,
        )));
    }

    clip(rect, 0.0, 0.0).child(welcome)
}

fn section_title(rect: UiRect, label: &'static str) -> Element {
    text(
        UiRect::new(rect.left, rect.top, rect.right, rect.top + SECTION_TITLE_H),
        label,
        theme::mono(theme::c().text_dim, theme::SMALL).tracking(0.8),
    )
    .into()
}

fn action_row(
    rect: UiRect,
    index: usize,
    action: StartAction,
    label: &'static str,
    hovered: bool,
    state: HomeBinding,
    editor_focus: UiFocusHandle,
) -> Element {
    let surface = if hovered {
        theme::bordered(rect, theme::c().surface, theme::c().border, 4.0, 1.0)
    } else {
        panel(rect, VisualStyle::default().radius(4.0))
    };
    let hover_state = state.clone();
    let click_state = state.clone();
    surface
        .event_policy(EventPolicy::INTERACTIVE)
        .cursor(CursorIcon::Pointer)
        .on_pointer_move(move |_cx, _pointer| {
            hover_state.run(HomeCommand::Hover(Some(index)));
        })
        .on_click(move || match action {
            StartAction::OpenFile => {
                if click_state.run(HomeCommand::OpenFile) {
                    editor_focus.focus();
                }
            }
            StartAction::OpenFolder => {
                click_state.run(HomeCommand::OpenFolder);
            }
        })
        .child(text(
            UiRect::new(rect.left + 12.0, rect.top, rect.right - 12.0, rect.bottom),
            label,
            theme::sans(
                if hovered {
                    theme::c().text_bright
                } else {
                    theme::c().text_soft
                },
                theme::UI_SIZE,
            ),
        ))
}

fn empty_recent_card(rect: UiRect) -> Element {
    theme::bordered(rect, theme::c().bg, theme::c().border, 5.0, 1.0).child(text(
        UiRect::new(rect.left + 12.0, rect.top, rect.right - 12.0, rect.bottom),
        "No recent folders",
        theme::mono(theme::c().text_dim, theme::UI_SIZE),
    ))
}

fn recent_workspace_card(
    rect: UiRect,
    path: &std::path::Path,
    hover_index: usize,
    hovered: bool,
    state: HomeBinding,
) -> Element {
    let name = path
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    let path_label = super::display_path(path);
    let display_path = ellipsize(
        &path_label,
        (rect.width() - 24.0).max(0.0),
        theme::SMALL,
        400,
    );
    let surface = if hovered {
        theme::bordered(rect, theme::c().surface, theme::c().border, 5.0, 1.0)
    } else {
        theme::bordered(rect, theme::c().bg, theme::c().border, 5.0, 1.0)
    };
    let hover_state = state.clone();
    let click_state = state;
    let workspace_path = path.to_path_buf();
    surface
        .event_policy(EventPolicy::INTERACTIVE)
        .cursor(CursorIcon::Pointer)
        .on_pointer_move(move |_cx, _pointer| {
            hover_state.run(HomeCommand::Hover(Some(hover_index)));
        })
        .on_click(move || {
            click_state.run(HomeCommand::OpenRecent(workspace_path.clone()));
        })
        .child(text(
            UiRect::new(
                rect.left + 12.0,
                rect.top + 8.0,
                rect.right - 12.0,
                rect.top + 28.0,
            ),
            name,
            theme::sans_semibold(
                if hovered {
                    theme::c().text_bright
                } else {
                    theme::c().text
                },
                theme::UI_SIZE,
            ),
        ))
        .child(text(
            UiRect::new(
                rect.left + 12.0,
                rect.top + 29.0,
                rect.right - 12.0,
                rect.bottom - 6.0,
            ),
            display_path,
            theme::mono(theme::c().text_dim, theme::SMALL),
        ))
}

fn footer(rect: UiRect) -> Element {
    group(rect)
        .child(panel(
            UiRect::new(rect.left, rect.top, rect.right, rect.top + 1.0),
            VisualStyle::filled(theme::c().border),
        ))
        .child(text(
            UiRect::new(rect.left, rect.top + 10.0, rect.right, rect.top + 28.0),
            "Welcome to Loog",
            theme::sans_semibold(theme::c().text_soft, theme::UI_SIZE),
        ))
        .child(text(
            UiRect::new(rect.left, rect.top + 28.0, rect.right, rect.bottom),
            "A focused workspace for exploring projects, editing code, and running local shells.",
            theme::sans(theme::c().text_dim, theme::SMALL),
        ))
}
