//! The left end of the status bar: the manage button, the repository's branch, and its
//! remote actions. The branch opens the branch picker; pull, push and publish show only when
//! there is something to move. Also the measurements the title bar's notes share.

use std::sync::Arc;

use lgui::core::{
    AnimProperty, AnimationBinding, CursorIcon, EventPolicy, IconStyle, SemanticRole, Semantics,
    UiElement, UiId, group,
};
use lgui::prelude::{Color, Element, TextStyle, UiRect, VisualStyle};

use crate::features::git::actions::SyncPlan;
use crate::ui::git::commands::{Binding, Command, Snapshot};
use crate::ui::theme;

pub(crate) const BADGE_H: f32 = 22.0;
pub(crate) const BADGE_PAD: f32 = 8.0;
pub(crate) const BADGE_ICON: f32 = 14.0;
/// Gap between an icon and the text it labels.
pub(crate) const ICON_GAP: f32 = 5.0;
/// Extra right margin inside each text rect so the last glyph is not clipped.
pub(crate) const TEXT_MARGIN: f32 = 6.0;

/// The status bar's left padding; menus anchored to its controls start here.
pub const LEFT: f32 = 12.0;
/// The manage button's width.
pub const MANAGE_W: f32 = 24.0;
const CONTROL_H: f32 = 20.0;
const ICON: f32 = 12.0;
const GAP: f32 = 2.0;

/// A sync control: shown, id, icon, count, tooltip name and the command it runs.
type SyncAction = (
    bool,
    &'static str,
    &'static str,
    u32,
    &'static str,
    fn() -> Command,
);

pub(crate) fn measure(s: &str, style: &TextStyle) -> f32 {
    crate::ui::tabs::measure(s, style.height, style.weight)
}

/// The manage button's state, and what pressing it does.
#[derive(Clone)]
pub struct Manage {
    pub open: bool,
    /// An extension failed: the button carries a dot.
    pub attention: bool,
    pub press: Arc<dyn Fn() + Send + Sync>,
}

#[derive(Clone)]
enum Part {
    Icon {
        id: &'static str,
        key: &'static str,
        color: Color,
    },
    Text {
        id: &'static str,
        label: String,
        style: TextStyle,
    },
    Dot {
        color: Color,
    },
}

/// A flat control that fills on hover and while `open`; one that cannot be pressed shows no
/// fill. `parts` are laid out from `rect.left`, each with the gap before it.
#[allow(clippy::too_many_arguments)]
fn control(
    key: &'static str,
    rect: UiRect,
    name: &str,
    open: bool,
    enabled: bool,
    parts: Vec<(f32, Part)>,
    on_click: impl Fn() + Send + Sync + 'static,
) -> Element {
    let mut semantics = Semantics::new(SemanticRole::Button).name(name);
    semantics.state.disabled = !enabled;
    let element = Element::new(move |cx| {
        // The hover fades in with the element's own animation, as the title bar buttons do.
        let hover = if enabled {
            cx.animation_value(AnimProperty::Hover).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let fill = VisualStyle {
            fill: Some(theme::c().active_line),
            fill_alpha: ((if open { 1.0 } else { hover }) * 255.0_f32).round() as u8,
            ..VisualStyle::default()
        }
        .radius(4.0);
        let middle = (rect.top + rect.bottom) / 2.0;
        let mut node = UiElement::panel(cx.id, rect, fill);
        let mut x = rect.left;
        for (gap, part) in &parts {
            x += gap;
            match part {
                Part::Icon { id, key, color } => {
                    node = node.child(
                        UiElement::icon(
                            UiId::new(id),
                            UiRect::new(x, middle - ICON / 2.0, x + ICON, middle + ICON / 2.0),
                            *key,
                        )
                        .icon_style(IconStyle::new(*color)),
                    );
                    x += ICON;
                }
                Part::Text { id, label, style } => {
                    let width = measure(label, style);
                    node = node.child(UiElement::text(
                        UiId::new(id),
                        UiRect::new(x, rect.top, x + width + TEXT_MARGIN, rect.bottom),
                        label.clone(),
                        *style,
                    ));
                    x += width;
                }
                Part::Dot { color } => {
                    node = node.child(UiElement::panel(
                        UiId::new("statusbar.manage.dot"),
                        UiRect::new(
                            rect.right - 8.0,
                            rect.top + 2.0,
                            rect.right - 2.0,
                            rect.top + 8.0,
                        ),
                        VisualStyle::filled(*color).radius(3.0),
                    ));
                }
            }
        }
        node
    })
    .key(key)
    .animation(AnimationBinding::new(AnimProperty::Hover, 0.0, 1.0))
    .semantics(semantics)
    .event_policy(if enabled {
        EventPolicy::INTERACTIVE
    } else {
        EventPolicy::NONE
    })
    .cursor(if enabled {
        CursorIcon::Pointer
    } else {
        CursorIcon::NotAllowed
    });
    if enabled {
        element.on_click(on_click)
    } else {
        element
    }
}

fn parts_width(parts: &[(f32, Part)]) -> f32 {
    parts
        .iter()
        .map(|(gap, part)| {
            gap + match part {
                Part::Icon { .. } => ICON,
                Part::Text { label, style, .. } => measure(label, style),
                Part::Dot { .. } => 0.0,
            }
        })
        .sum()
}

/// The controls of the status bar's left end, laid out from `origin` (the top-left of the
/// group in its parent's coordinates, `height` tall). `left` is where the group's left edge
/// sits in the window, for the branch picker's anchor.
pub fn status_leading(
    origin: (f32, f32),
    height: f32,
    left: f32,
    manage: Manage,
    state: Binding,
    store: Snapshot,
    diff_loading: bool,
) -> Element {
    let git = store.get();
    let palette = theme::c();
    let top = origin.1 + (height - CONTROL_H) / 2.0;
    let mut x = origin.0;
    let mut children: Vec<Element> = Vec::new();

    // Manage: Settings, Extensions, Keyboard Shortcuts, Command Palette.
    let manage_rect = UiRect::new(x, top, x + MANAGE_W, top + CONTROL_H);
    let mut manage_parts = vec![(
        (MANAGE_W - ICON) / 2.0,
        Part::Icon {
            id: "statusbar.manage.icon",
            key: "settings",
            color: palette.text_muted,
        },
    )];
    if manage.attention {
        manage_parts.push((
            0.0,
            Part::Dot {
                color: palette.warning,
            },
        ));
    }
    let press = manage.press.clone();
    children.push(control(
        "statusbar.manage.button",
        manage_rect,
        "Manage",
        manage.open,
        true,
        manage_parts,
        move || press(),
    ));
    x = manage_rect.right + 4.0;

    // The branch, dirty marked with `*`, and an operation in progress.
    let picker_open = state.get().ui.branch_picker.is_some();
    let mut branch_parts: Vec<(f32, Part)> = Vec::new();
    match git.active() {
        None => {
            branch_parts.push((
                6.0,
                Part::Icon {
                    id: "statusbar.git.icon",
                    key: "git-branch",
                    color: palette.text_ghost,
                },
            ));
            branch_parts.push((
                ICON_GAP,
                Part::Text {
                    id: "statusbar.git.label",
                    label: "No Git".into(),
                    style: theme::mono(palette.text_ghost, theme::SMALL),
                },
            ));
        }
        Some(repository) => {
            let dirty = git
                .presentation
                .repositories
                .get(&repository.id)
                .is_some_and(|prepared| prepared.dirty_count > 0);
            let color = if dirty {
                palette.warning
            } else {
                palette.text_muted
            };
            branch_parts.push((
                6.0,
                Part::Icon {
                    id: "statusbar.git.icon",
                    key: "git-branch",
                    color,
                },
            ));
            branch_parts.push((
                ICON_GAP,
                Part::Text {
                    id: "statusbar.git.label",
                    label: format!(
                        "{}{}",
                        repository.branch_label(),
                        if dirty { "*" } else { "" }
                    ),
                    style: theme::mono(color, theme::SMALL),
                },
            ));
            if let Some(label) = crate::ui::git::panel::operation_label(repository) {
                branch_parts.push((
                    6.0,
                    Part::Text {
                        id: "statusbar.git.operation",
                        label,
                        style: theme::mono_bold(palette.warning, theme::SMALL),
                    },
                ));
            }
        }
    }
    let branch_width = parts_width(&branch_parts) + TEXT_MARGIN + 6.0;
    let branch_rect = UiRect::new(x, top, x + branch_width, top + CONTROL_H);
    let anchor_x = left + (branch_rect.left - origin.0);
    let (picker_state, picker_store) = (state.clone(), store.clone());
    children.push(control(
        "statusbar.git.button",
        branch_rect,
        "Switch branch",
        picker_open,
        true,
        branch_parts,
        move || super::branch_picker::open(&picker_state, &picker_store, anchor_x),
    ));
    x = branch_rect.right;

    // Remote actions, only when there is something to move: pull and push carry their
    // counts, publish stands in for both while the branch has no upstream. Fetch stays in
    // Source Control.
    if let Some(repository) = git.active()
        && let Some(plan) = crate::features::git::actions::sync_plan(repository)
    {
        let enabled = git.operation.is_none();
        let color = if enabled {
            palette.accent
        } else {
            palette.text_ghost
        };
        let publish = plan == SyncPlan::Publish;
        let actions: [SyncAction; 3] = [
            (
                publish,
                "statusbar.publish",
                "cloud-upload",
                0,
                "Publish branch",
                || Command::Push,
            ),
            (
                !publish && repository.behind > 0,
                "statusbar.pull",
                "arrow-down",
                repository.behind,
                "Pull changes",
                || Command::Pull,
            ),
            (
                !publish && repository.ahead > 0,
                "statusbar.push",
                "arrow-up",
                repository.ahead,
                "Push changes",
                || Command::Push,
            ),
        ];
        for (visible, id, icon, count, name, command) in actions {
            if !visible {
                continue;
            }
            let mut parts = vec![(
                4.0,
                Part::Icon {
                    id,
                    key: icon,
                    color,
                },
            )];
            if count > 0 {
                parts.push((
                    2.0,
                    Part::Text {
                        id: match id {
                            "statusbar.pull" => "statusbar.pull.count",
                            _ => "statusbar.push.count",
                        },
                        label: count.to_string(),
                        style: theme::mono(color, theme::SMALL),
                    },
                ));
            }
            let width = parts_width(&parts) + if count > 0 { TEXT_MARGIN } else { 0.0 } + 4.0;
            let rect = UiRect::new(x + GAP, top, x + GAP + width, top + CONTROL_H);
            x = rect.right;
            let action_state = state.clone();
            children.push(control(id, rect, name, false, enabled, parts, move || {
                action_state.dispatch(command());
            }));
        }
    }

    // A Git operation in progress, else a diff being read. Reading blocks nothing, so it is
    // shown without disabling the controls above.
    let busy = match &git.operation {
        Some(operation) => Some(operation.message.as_str()),
        None => diff_loading.then_some("Loading diff…"),
    };
    if let Some(message) = busy {
        let style = theme::mono(palette.text_dim, theme::SMALL);
        let middle = origin.1 + height / 2.0;
        let spinner = UiRect::new(
            x + 8.0,
            middle - ICON / 2.0,
            x + 8.0 + ICON,
            middle + ICON / 2.0,
        );
        children.push(crate::ui::components::loading_indicator::render(
            spinner,
            palette.accent,
            "statusbar-git-busy".into(),
        ));
        let text_left = spinner.right + ICON_GAP;
        let width = measure(message, &style) + TEXT_MARGIN;
        children.push(
            lgui::prelude::text(
                UiRect::new(text_left, origin.1, text_left + width, origin.1 + height),
                message.to_owned(),
                style,
            )
            .into(),
        );
        x = text_left + width;
    }

    let mut leading = group(UiRect::new(origin.0, origin.1, x, origin.1 + height));
    for child in children {
        leading = leading.child(child);
    }
    leading
}
