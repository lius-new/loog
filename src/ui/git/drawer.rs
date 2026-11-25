//! Controls shared by the Git drawer's filter, file rows and commit composer.
use crate::features::git::GitStoreSnapshot;
use crate::features::git::commit_selection::CheckState;
use crate::features::git::operation::{operation_text, primary_reason, title};
use crate::ui::git::{
    commands::{Binding, Snapshot},
    panel as git_panel,
};
use crate::ui::theme;
use lgui::core::{
    CursorIcon, EventPolicy, Point, PointerButton, SemanticRole, Semantics, UiFocusHandle, UiId,
    UiPath, UiPathCommand, clip, clip_path,
};
use lgui::prelude::{Element, UiRect, VisualStyle, panel, text};

const COMMIT_IDLE_FIELD_H: f32 = 36.0;
const COMMIT_FIELD_H: f32 = 54.0;
const COMMIT_BUTTON_H: f32 = 24.0;
const COMMIT_MAIN_W: f32 = 72.0;
const COMMIT_OPTIONS_W: f32 = 24.0;

pub fn checkbox(
    rect: UiRect,
    key: String,
    label: String,
    checked: bool,
    enabled: bool,
    action: impl Fn() + Send + Sync + 'static,
) -> Element {
    let state = if checked {
        CheckState::Checked
    } else {
        CheckState::Unchecked
    };
    check_box(rect, key, label, state, enabled, action)
}

/// A checkbox that can also show that only part of what it stands for is chosen.
pub fn check_box(
    rect: UiRect,
    key: String,
    label: String,
    state: CheckState,
    enabled: bool,
    action: impl Fn() + Send + Sync + 'static,
) -> Element {
    check_box_sized(rect, key, label, state, enabled, 12.0, action)
}

/// [`check_box`] with a box of `size`, for a row of controls of one size.
pub fn check_box_sized(
    rect: UiRect,
    key: String,
    label: String,
    state: CheckState,
    enabled: bool,
    size: f32,
    action: impl Fn() + Send + Sync + 'static,
) -> Element {
    let checked = state != CheckState::Unchecked;
    let size = size.min(rect.height().max(0.0)).min(rect.width().max(0.0));
    let top = rect.top + (rect.height() - size) / 2.0;
    let square = UiRect::new(
        rect.left + (rect.width() - size) / 2.0,
        top,
        rect.left + (rect.width() + size) / 2.0,
        top + size,
    );
    let mut semantics = Semantics::new(SemanticRole::CheckBox).name(label);
    semantics.state.checked = Some(state == CheckState::Checked);
    semantics.state.disabled = !enabled;
    let mut root = panel(rect, VisualStyle::default())
        .key(key.clone())
        .event_policy(EventPolicy::INTERACTIVE)
        .semantics(semantics)
        .cursor(if enabled {
            CursorIcon::Pointer
        } else {
            CursorIcon::NotAllowed
        })
        .on_pointer_down_with_button(|cx, _, button| {
            if button == PointerButton::Left {
                cx.stop_propagation();
            }
        })
        .on_click(move |cx: &mut lgui::core::UiEventContext| {
            if enabled {
                action();
            }
            cx.stop_propagation();
        })
        .child(theme::bordered(
            square,
            if checked {
                theme::c().accent
            } else {
                theme::c().bg
            },
            if checked {
                theme::c().accent
            } else {
                theme::c().text_ghost
            },
            2.0,
            1.0,
        ));
    if checked {
        root = root.child(crate::ui::drawer::drawer_icon(
            UiId::owned(format!("{key}-check")),
            if state == CheckState::Partial {
                "minus"
            } else {
                "check"
            },
            square,
            theme::c().on_accent,
        ));
    }
    root
}

pub fn commit_reason(
    app: &super::state::GitPanelState,
    git: &GitStoreSnapshot,
) -> Option<&'static str> {
    if git.operation.is_some() {
        return Some("Git operation in progress");
    }
    let Some(repository) = git.active() else {
        return Some("No repository");
    };
    let sections = git
        .presentation
        .repositories
        .get(&repository.id)
        .map(|p| &p.sections);
    if sections.is_some_and(|s| !s[git_panel::CONFLICTS as usize].is_empty())
        || repository
            .files
            .values()
            .any(|file| file.conflict.is_some())
    {
        return Some("Resolve conflicts before committing");
    }
    // A commit takes what is ticked in the Changes list.
    let files = git_panel::changed_files(git);
    let targets = app.ui.commit_selection.targets(
        files
            .iter()
            .map(|(path, untracked)| (path.as_path(), *untracked)),
    );
    if targets.is_empty() && !app.ui.commit_amend {
        return Some("Select the changes to commit");
    }
    if app.ui.commit_input.text().trim().is_empty() && !app.ui.commit_amend {
        return Some("Enter a commit message");
    }
    None
}

pub fn composer(
    rect: UiRect,
    state: Binding,
    store: Snapshot,
    focus: UiFocusHandle,
    id: UiId,
) -> Element {
    let app = state.get();
    let git = store.get();
    // While Git is mid-operation the commit message is not used, so this area
    // reports the operation and its button finishes it.
    let operation = git.active().and_then(|repository| {
        operation_text(repository.repository_state).map(|wording| (repository, wording))
    });
    if let Some((repository, wording)) = operation {
        return operation_composer(rect, state, store, repository, wording);
    }
    // Quiet until the message is wanted: one line while idle, and the message
    // box with its commit button once it has focus or content.
    let t = git_panel::composer_progress(&app.ui);
    // Ease the height and the message box between idle and open.
    let e = t * t * (3.0 - 2.0 * t);
    let c = theme::c();
    let root = panel(rect, VisualStyle::filled(c.sidebar));
    let inner = UiRect::new(
        rect.left + 10.0,
        rect.top + 8.0,
        (rect.right - 10.0).max(rect.left + 10.0),
        (rect.bottom - 8.0).max(rect.top + 8.0),
    );
    let field_box = theme::bordered(inner, c.bg, c.border, 6.0, 1.0);
    // The input pads its own text, so its rect spans the whole box.
    let field = UiRect::new(
        inner.left + 1.0,
        inner.top,
        (inner.right - 1.0 - 27.0 * (1.0 - e)).max(inner.left + 1.0),
        (inner.top + COMMIT_IDLE_FIELD_H + (COMMIT_FIELD_H - COMMIT_IDLE_FIELD_H) * e)
            .min(inner.bottom),
    );
    let mut field_box = field_box.child(git_panel::commit_input(field, state.clone(), focus, id));
    if e < 0.3 {
        field_box = field_box.child(crate::ui::drawer::drawer_icon(
            "git-commit-enter",
            "enter",
            UiRect::new(
                inner.right - 23.0,
                inner.top + 11.0,
                inner.right - 10.0,
                inner.top + 24.0,
            ),
            c.text_ghost,
        ));
    }
    // The button waits until the box has nearly finished opening.
    if e >= 0.8 {
        let button_bottom = inner.bottom - 6.0;
        let button_top = button_bottom - COMMIT_BUTTON_H;
        let options = UiRect::new(
            (inner.right - 6.0 - COMMIT_OPTIONS_W).max(inner.left),
            button_top,
            (inner.right - 6.0).max(inner.left),
            button_bottom,
        );
        let main = UiRect::new(
            (options.left - COMMIT_MAIN_W).max(inner.left),
            button_top,
            options.left,
            button_bottom,
        );
        let enabled = commit_reason(&app, &git).is_none();
        let group = UiRect::new(main.left, button_top, options.right, button_bottom);
        field_box = field_box.child(
            clip_path(group, rounded_rect(group, 4.0))
                .child(git_panel::commit_button(main, state.clone(), store))
                .child(git_panel::commit_options_button(options, state, enabled))
                .child(panel(
                    UiRect::new(options.left, button_top, options.left + 0.5, button_bottom),
                    if enabled {
                        VisualStyle::filled(c.on_accent).alpha(70)
                    } else {
                        VisualStyle::filled(c.border)
                    },
                )),
        );
    }
    clip(rect, 0.0, 0.0).child(root.child(field_box))
}

/// The composer while Git waits on an operation: its title and the button that
/// continues or aborts it. The message box is gone.
fn operation_composer(
    rect: UiRect,
    state: Binding,
    store: Snapshot,
    repository: &crate::features::git::RepositorySnapshot,
    wording: crate::features::git::operation::OperationText,
) -> Element {
    let git = store.get();
    let conflicted = repository
        .files
        .values()
        .filter(|file| file.conflict.is_some())
        .count();
    let root = panel(rect, VisualStyle::filled(theme::c().sidebar))
        .child(crate::ui::components::divider::top(rect));
    let inner = UiRect::new(
        rect.left + 12.0,
        rect.top + 10.0,
        (rect.right - 12.0).max(rect.left + 12.0),
        rect.bottom - 10.0,
    );
    let label = UiRect::new(inner.left, inner.top, inner.right, inner.top + 22.0);
    let root = root.child(text(
        UiRect::new(
            label.left,
            label.top,
            (label.right - 82.0).max(label.left),
            label.bottom,
        ),
        title(wording, repository.operation_step),
        theme::sans_semibold(theme::c().warning, 11.0),
    ));
    let button_bottom = inner.bottom;
    let button_top = (button_bottom - 32.0).max(label.bottom);
    let options = UiRect::new(
        (inner.right - 28.0).max(inner.left),
        button_top,
        inner.right,
        button_bottom,
    );
    let primary_reason = primary_reason(wording, conflicted, git.operation.is_some());
    let enabled = primary_reason.is_none();
    let group = UiRect::new(inner.left, button_top, inner.right, button_bottom);
    clip(rect, 0.0, 0.0).child(
        root.child(
            clip_path(group, rounded_rect(group, 4.0))
                .child(git_panel::operation_button(
                    UiRect::new(inner.left, button_top, options.left, button_bottom),
                    state.clone(),
                    wording.continue_label.unwrap_or(wording.abort_label),
                    wording.continue_label.is_some(),
                    primary_reason,
                ))
                .child(git_panel::commit_options_button(options, state, enabled))
                .child(panel(
                    UiRect::new(options.left, button_top, options.left + 0.5, button_bottom),
                    if enabled {
                        VisualStyle::filled(theme::c().on_accent).alpha(70)
                    } else {
                        VisualStyle::filled(theme::c().border)
                    },
                )),
        ),
    )
}

/// A rounded rectangle path, for clipping children to its outline.
fn rounded_rect(rect: UiRect, radius: f32) -> UiPath {
    let r = radius.min(rect.width() / 2.0).min(rect.height() / 2.0);
    let corner = |control: (f32, f32), to: (f32, f32)| UiPathCommand::QuadraticTo {
        control: Point::new(control.0, control.1),
        to: Point::new(to.0, to.1),
    };
    UiPath::new([
        UiPathCommand::MoveTo(Point::new(rect.left + r, rect.top)),
        UiPathCommand::LineTo(Point::new(rect.right - r, rect.top)),
        corner((rect.right, rect.top), (rect.right, rect.top + r)),
        UiPathCommand::LineTo(Point::new(rect.right, rect.bottom - r)),
        corner((rect.right, rect.bottom), (rect.right - r, rect.bottom)),
        UiPathCommand::LineTo(Point::new(rect.left + r, rect.bottom)),
        corner((rect.left, rect.bottom), (rect.left, rect.bottom - r)),
        UiPathCommand::LineTo(Point::new(rect.left, rect.top + r)),
        corner((rect.left, rect.top), (rect.left + r, rect.top)),
        UiPathCommand::Close,
    ])
}
