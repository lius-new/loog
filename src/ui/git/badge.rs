//! Repository controls in the title bar: the repository and branch badge,
//! which opens the branch picker, then the sync button and the message of a
//! Git operation in progress.

use lgui::core::{
    CursorIcon, EventPolicy, IconStyle, SemanticRole, Semantics, UiElement, UiId, precompiled,
};
use lgui::prelude::{Color, Element, TextStyle, UiRect, VisualStyle, panel, text};

use crate::features::git::actions::SyncPlan;
use crate::features::git::{GitStoreSnapshot, RepositorySnapshot};
use crate::ui::git::commands::{Binding, Command, Snapshot};
use crate::ui::theme;

pub(crate) const BADGE_H: f32 = 22.0;
pub(crate) const BADGE_PAD: f32 = 8.0;
pub(crate) const BADGE_ICON: f32 = 14.0;
/// Gap between an icon and the text it labels.
pub(crate) const ICON_GAP: f32 = 5.0;
/// Gap between the badge's groups: repository, branch, operation.
pub(crate) const GROUP_GAP: f32 = 8.0;
/// Gap between the badge, the sync button and the operation message.
pub(crate) const ITEM_GAP: f32 = 2.0;
/// Extra right margin inside each text rect so the last glyph is not clipped.
pub(crate) const TEXT_MARGIN: f32 = 6.0;

pub(crate) fn measure(s: &str, style: &TextStyle) -> f32 {
    crate::ui::tabs::measure(s, style.height, style.weight)
}

pub(crate) enum BadgePiece {
    Icon {
        id: &'static str,
        key: &'static str,
        color: Color,
    },
    Text {
        label: String,
        style: TextStyle,
    },
}

impl BadgePiece {
    pub(crate) fn width(&self) -> f32 {
        match self {
            Self::Icon { .. } => BADGE_ICON,
            Self::Text { label, style } => measure(label, style),
        }
    }
}

/// Add the repository controls to `bar`, starting at `left` within the
/// title bar `rect`.
pub fn attach(
    mut bar: Element,
    rect: UiRect,
    left: f32,
    state: Binding,
    store: Snapshot,
    diff_loading: bool,
) -> Element {
    let git = store.get();
    let picker_open = state.get().ui.branch_picker.is_some();

    // Repository and branch: opens the branch picker below the title bar.
    let (badge, mut right) = badge_element(rect, left, badge_pieces(&git), picker_open);
    let badge_state = state.clone();
    let badge_store = store.clone();
    bar = bar.child(
        badge
            .event_policy(EventPolicy::INTERACTIVE)
            .semantics(Semantics::new(SemanticRole::Button).name("Switch branch"))
            .cursor(CursorIcon::Pointer)
            .on_click(move || {
                super::branch_picker::open(&badge_state, &badge_store, left);
            }),
    );

    // Sync: publish, pull, push or fetch, whichever is due.
    if let Some(repository) = git.active()
        && let Some(plan) = crate::features::git::actions::sync_plan(repository)
    {
        let enabled = git.operation.is_none();
        let color = if enabled {
            theme::c().text_muted
        } else {
            theme::c().text_ghost
        };
        let (sync, sync_right) = badge_element(
            rect,
            right + ITEM_GAP,
            sync_pieces(repository, plan, color),
            false,
        );
        right = sync_right;
        bar = bar.child(
            sync.event_policy(EventPolicy::INTERACTIVE)
                .semantics(Semantics::new(SemanticRole::Button).name(sync_name(plan)))
                .cursor(if enabled {
                    CursorIcon::Pointer
                } else {
                    CursorIcon::NotAllowed
                })
                .on_click(move || {
                    if enabled {
                        state.dispatch(Command::Sync);
                    }
                }),
        );
    }

    // A Git operation in progress, else a diff being read. Reading blocks
    // nothing, so it is shown here without disabling the controls above.
    let busy = match &git.operation {
        Some(operation) => Some(operation.message.as_str()),
        None => diff_loading.then_some("Loading diff…"),
    };
    if let Some(message) = busy {
        let style = theme::mono(theme::c().text_dim, theme::SMALL);
        let spinner = UiRect::new(
            right + GROUP_GAP,
            rect.top + (rect.height() - BADGE_ICON) / 2.0,
            right + GROUP_GAP + BADGE_ICON,
            rect.top + (rect.height() + BADGE_ICON) / 2.0,
        );
        bar = bar.child(crate::ui::components::loading_indicator::render(
            spinner,
            theme::c().accent,
            "titlebar-git-busy".into(),
        ));
        let left = spinner.right + ICON_GAP;
        let width = measure(message, &style) + TEXT_MARGIN;
        bar = bar.child(text(
            UiRect::new(left, rect.top, left + width, rect.bottom),
            message.to_owned(),
            style,
        ));
    }
    bar
}

/// The badge's contents, each with the gap before it: the repository, the
/// branch (`*` when dirty), then an operation in progress.
pub(crate) fn badge_pieces(git: &GitStoreSnapshot) -> Vec<(f32, BadgePiece)> {
    let Some(repository) = git.active() else {
        let color = theme::c().text_ghost;
        return vec![
            (
                0.0,
                BadgePiece::Icon {
                    id: "titlebar.git",
                    key: "git-branch",
                    color,
                },
            ),
            (
                ICON_GAP,
                BadgePiece::Text {
                    label: "No Git".into(),
                    style: theme::mono(color, theme::UI_SIZE),
                },
            ),
        ];
    };
    let dirty = git
        .presentation
        .repositories
        .get(&repository.id)
        .is_some_and(|prepared| prepared.dirty_count > 0);
    let color = if dirty {
        theme::c().warning
    } else {
        theme::c().text_muted
    };
    let name = repository.worktree_root.file_name().map_or_else(
        || crate::ui::display_path(&repository.worktree_root),
        |name| name.to_string_lossy().into_owned(),
    );
    let mut pieces = vec![
        (
            0.0,
            BadgePiece::Text {
                label: name,
                style: theme::mono_bold(theme::c().text_soft, theme::UI_SIZE),
            },
        ),
        (
            ICON_GAP,
            BadgePiece::Text {
                label: "/".into(),
                style: theme::mono(theme::c().text_faint, theme::UI_SIZE),
            },
        ),
        (
            ICON_GAP,
            BadgePiece::Icon {
                id: "titlebar.git",
                key: "git-branch",
                color,
            },
        ),
        (
            ICON_GAP,
            BadgePiece::Text {
                label: format!(
                    "{}{}",
                    repository.branch_label(),
                    if dirty { "*" } else { "" }
                ),
                style: theme::mono(color, theme::UI_SIZE),
            },
        ),
    ];
    if let Some(label) = crate::ui::git::panel::operation_label(repository) {
        pieces.push((
            GROUP_GAP,
            BadgePiece::Text {
                label,
                style: theme::mono_bold(theme::c().warning, theme::SMALL),
            },
        ));
    }
    pieces
}

/// The sync button's contents: a publish icon for a branch with no
/// upstream, else the sync icon with the commits behind and ahead.
pub(crate) fn sync_pieces(
    repository: &RepositorySnapshot,
    plan: SyncPlan,
    color: Color,
) -> Vec<(f32, BadgePiece)> {
    if plan == SyncPlan::Publish {
        return vec![(
            0.0,
            BadgePiece::Icon {
                id: "titlebar.publish",
                key: "cloud-upload",
                color,
            },
        )];
    }
    let mut pieces = vec![(
        0.0,
        BadgePiece::Icon {
            id: "titlebar.sync",
            key: "sync",
            color,
        },
    )];
    for (count, id, key) in [
        (repository.behind, "titlebar.behind", "arrow-down"),
        (repository.ahead, "titlebar.ahead", "arrow-up"),
    ] {
        if count > 0 {
            pieces.push((ICON_GAP, BadgePiece::Icon { id, key, color }));
            pieces.push((
                1.0,
                BadgePiece::Text {
                    label: count.to_string(),
                    style: theme::mono(color, theme::SMALL),
                },
            ));
        }
    }
    pieces
}

/// What the sync button says it will do, for assistive technology.
pub(crate) fn sync_name(plan: SyncPlan) -> &'static str {
    match plan {
        SyncPlan::Publish => "Publish branch",
        SyncPlan::Fetch => "Fetch from remote",
        SyncPlan::Pull { then_push: true } => "Pull and push changes",
        SyncPlan::Pull { then_push: false } => "Pull changes",
        SyncPlan::Push => "Push changes",
    }
}

/// A badge laid out from `pieces`, its left edge at `left`, centered in the
/// bar `rect`. Returns it without a click handler, and its right edge.
pub(crate) fn badge_element(
    rect: UiRect,
    left: f32,
    pieces: Vec<(f32, BadgePiece)>,
    highlighted: bool,
) -> (Element, f32) {
    let width = BADGE_PAD * 2.0
        + pieces
            .iter()
            .map(|(gap, piece)| gap + piece.width())
            .sum::<f32>()
        + TEXT_MARGIN;
    let top = rect.top + (rect.height() - BADGE_H) / 2.0;
    let visual = UiRect::new(left, top, left + width, top + BADGE_H);
    let mut badge = panel(
        visual,
        if highlighted {
            VisualStyle::filled(theme::c().active_line).radius(4.0)
        } else {
            VisualStyle::default().radius(4.0)
        },
    );
    let mut x = visual.left + BADGE_PAD;
    for (gap, piece) in pieces {
        x += gap;
        let width = piece.width();
        badge = match piece {
            BadgePiece::Icon { id, key, color } => badge.child(precompiled(
                UiElement::icon(
                    UiId::new(id),
                    UiRect::new(
                        x,
                        top + (BADGE_H - BADGE_ICON) / 2.0,
                        x + BADGE_ICON,
                        top + (BADGE_H + BADGE_ICON) / 2.0,
                    ),
                    key,
                )
                .icon_style(IconStyle::new(color)),
            )),
            BadgePiece::Text { label, style } => badge.child(text(
                UiRect::new(x, visual.top, x + width + TEXT_MARGIN, visual.bottom),
                label,
                style,
            )),
        };
        x += width;
    }
    (badge, visual.right)
}
