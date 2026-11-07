//! Project-scoped home surface shown when a workspace is open without an
//! active document. It stays out of the tab model while providing a stable
//! place for project-level actions and future summaries.

use std::path::{Path, PathBuf};

use lgui::core::{
    CursorIcon, EventPolicy, SemanticRole, Semantics, UiElement, UiEventKind, UiId, clip,
};
use lgui::prelude::{Element, UiRect, VisualStyle, panel, text};

use crate::ui::chrome::{HomeBinding, HomeCommand};
use crate::ui::tabs::{TEXT_MARGIN, ellipsize, measure};
use crate::ui::theme;

const MAX_PAGE_W: f32 = 620.0;
const PAGE_PAD_X: f32 = 32.0;
const MIN_PAGE_TOP: f32 = 24.0;
const CONTENT_H: f32 = 252.0;
const ACTIONS_TOP: f32 = 116.0;
const SECTION_TITLE_H: f32 = 24.0;
const ACTION_H: f32 = 36.0;
const ACTION_GAP: f32 = 5.0;

#[derive(Clone, Copy)]
enum WorkspaceAction {
    OpenFile,
    ToggleTerminal,
    OpenSourceControl,
}

impl WorkspaceAction {
    fn label(self) -> &'static str {
        match self {
            Self::OpenFile => "Open File...",
            Self::ToggleTerminal => "Toggle Terminal",
            Self::OpenSourceControl => "Source Control",
        }
    }

    fn shortcut(self) -> &'static str {
        match self {
            Self::OpenFile => "",
            Self::ToggleTerminal => "Ctrl+`",
            Self::OpenSourceControl => "Ctrl+Shift+G",
        }
    }
}

const ACTIONS: [WorkspaceAction; 3] = [
    WorkspaceAction::OpenFile,
    WorkspaceAction::ToggleTerminal,
    WorkspaceAction::OpenSourceControl,
];

type WorkspaceHomeActions = HomeBinding;

pub fn render(rect: UiRect, state: HomeBinding, surface_id: UiId) -> Element {
    let snapshot = state.frame();
    let actions = state.clone();
    let (title, detail) = workspace_identity(&snapshot.paths);
    let page_w = (rect.width() - PAGE_PAD_X * 2.0).clamp(0.0, MAX_PAGE_W);
    let page_left = rect.left + (rect.width() - page_w) / 2.0;
    let page_right = page_left + page_w;
    let page_top = rect.top + ((rect.height() - CONTENT_H) / 2.0).max(MIN_PAGE_TOP);

    let clear_hover = state.clone();
    let mut home = Element::new(move |cx| {
        UiElement::panel(surface_id, rect, VisualStyle::filled(theme::c().bg))
            .semantics(Semantics::new(SemanticRole::Group).name("Workspace Home"))
            .children(cx.children)
    })
    .event_policy(EventPolicy::INTERACTIVE)
    .on_event_capture(UiEventKind::PointerMove, move |_cx, _payload| {
        clear_hover.run(HomeCommand::Hover(None));
    });
    home = crate::ui::track_main_surface_focus(home, state.clone());

    home = home.child(text(
        UiRect::new(page_left, page_top, page_right, page_top + 18.0),
        "WORKSPACE",
        theme::mono(theme::c().text_dim, theme::SMALL).tracking(0.8),
    ));

    let title = ellipsize(&title, page_w, 20.0, 700);
    home = home.child(text(
        UiRect::new(page_left, page_top + 23.0, page_right, page_top + 51.0),
        title,
        theme::sans_semibold(theme::c().text_bright, 20.0),
    ));

    let detail = ellipsize(&detail, page_w, theme::UI_SIZE, 400);
    home = home.child(text(
        UiRect::new(page_left, page_top + 55.0, page_right, page_top + 77.0),
        detail,
        theme::mono(theme::c().text_dim, theme::UI_SIZE),
    ));

    let divider_top = page_top + 91.0;
    home = home.child(panel(
        UiRect::new(page_left, divider_top, page_right, divider_top + 1.0),
        VisualStyle::filled(theme::c().border),
    ));

    let section_top = page_top + ACTIONS_TOP;
    home = home.child(text(
        UiRect::new(
            page_left,
            section_top,
            page_right,
            section_top + SECTION_TITLE_H,
        ),
        "QUICK ACTIONS",
        theme::mono(theme::c().text_dim, theme::SMALL).tracking(0.8),
    ));

    let mut action_top = section_top + SECTION_TITLE_H;
    for (index, action) in ACTIONS.into_iter().enumerate() {
        let action_rect = UiRect::new(page_left, action_top, page_right, action_top + ACTION_H);
        home = home.child(action_row(
            action_rect,
            index,
            action,
            snapshot.hover == Some(index),
            actions.clone(),
        ));
        action_top += ACTION_H + ACTION_GAP;
    }

    clip(rect, 0.0, 0.0).child(home)
}

fn action_row(
    rect: UiRect,
    index: usize,
    action: WorkspaceAction,
    hovered: bool,
    actions: WorkspaceHomeActions,
) -> Element {
    let surface = panel(
        rect,
        if hovered {
            VisualStyle::filled(theme::c().active_line).radius(4.0)
        } else {
            VisualStyle::default().radius(4.0)
        },
    );
    let hover_state = actions.clone();
    let shortcut = action.shortcut();
    let shortcut_w = if shortcut.is_empty() {
        0.0
    } else {
        measure(shortcut, theme::SMALL, 400) + TEXT_MARGIN
    };

    surface
        .key(format!("workspace-home-action-{index}"))
        .event_policy(EventPolicy::INTERACTIVE)
        .semantics(Semantics::new(SemanticRole::Button).name(action.label()))
        .cursor(CursorIcon::Pointer)
        .on_pointer_move(move |_cx, _pointer| {
            hover_state.run(HomeCommand::Hover(Some(index)));
        })
        .on_click(move || {
            actions.run(match action {
                WorkspaceAction::OpenFile => HomeCommand::OpenFile,
                WorkspaceAction::ToggleTerminal => HomeCommand::ToggleTerminal,
                WorkspaceAction::OpenSourceControl => HomeCommand::OpenSourceControl,
            });
        })
        .child(text(
            UiRect::new(
                rect.left + 12.0,
                rect.top,
                (rect.right - shortcut_w - 20.0).max(rect.left + 12.0),
                rect.bottom,
            ),
            action.label(),
            theme::sans(
                if hovered {
                    theme::c().text_bright
                } else {
                    theme::c().text_soft
                },
                theme::UI_SIZE,
            ),
        ))
        .child(text(
            UiRect::new(
                (rect.right - shortcut_w - 12.0).max(rect.left + 12.0),
                rect.top,
                rect.right - 12.0,
                rect.bottom,
            ),
            shortcut,
            theme::mono_right(theme::c().text_dim, theme::SMALL),
        ))
}

fn workspace_identity(folders: &[PathBuf]) -> (String, String) {
    match folders {
        [] => ("Workspace".to_owned(), String::new()),
        [folder] => (folder_name(folder), super::display_path(folder)),
        folders => {
            let names = folders
                .iter()
                .map(|folder| folder_name(folder))
                .collect::<Vec<_>>()
                .join("  ·  ");
            (format!("{} folders", folders.len()), names)
        }
    }
}

fn folder_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| super::display_path(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_workspace_uses_folder_name_and_full_path() {
        let path = PathBuf::from("projects/loog");
        let (title, detail) = workspace_identity(&[path]);

        assert_eq!(title, "loog");
        assert!(detail.ends_with("projects/loog") || detail.ends_with(r"projects\loog"));
    }

    #[test]
    fn multi_root_workspace_summarizes_every_root() {
        let folders = vec![PathBuf::from("projects/app"), PathBuf::from("projects/api")];
        let (title, detail) = workspace_identity(&folders);

        assert_eq!(title, "2 folders");
        assert!(detail.contains("app"));
        assert!(detail.contains("api"));
    }
}
