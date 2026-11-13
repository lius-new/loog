//! Native Source Control for repositories whose IO is provided by an environment.
use super::commands::{Binding, Command};
use crate::{
    features::git::{ChangeKind, DiffTarget, HeadState, resources::Intent as ResourceCommand},
    ui::{
        components::input::{self, AccessInputOptions, InputBinding, InputStyle},
        theme,
    },
};
use lgui::{
    core::{
        CursorIcon, EventPolicy, SemanticRole, Semantics, UiFocusHandle, UiId, WheelUnit, clip,
    },
    prelude::{Element, UiRect, VisualStyle, panel, text},
};

pub(super) const SELECTOR_H: f32 = 28.0;
pub(super) fn selector(rect: UiRect, state: Binding) -> Element {
    let app = state.get();
    let selected = app.resources.model.selected;
    let label = selected
        .and_then(|scope| app.resources.model.entries.get(&scope))
        .map(|entry| entry.root.name.as_str())
        .or_else(|| {
            app.resources
                .model
                .offline
                .as_ref()
                .map(|(_, name)| name.as_str())
        })
        .unwrap_or("Local repositories");
    let header = UiRect::new(
        rect.left,
        rect.top,
        rect.right,
        (rect.top + SELECTOR_H).min(rect.bottom),
    );
    let toggle = state.clone();
    let mut root = panel(header, VisualStyle::filled(theme::c().sidebar))
        .key("git-environment-selector")
        .event_policy(EventPolicy::INTERACTIVE)
        .semantics(
            Semantics::new(SemanticRole::Button).name(format!("Repository environment: {label}")),
        )
        .cursor(CursorIcon::Pointer)
        .on_click(move || toggle.update(|app| app.resources.chooser = !app.resources.chooser))
        .child(text(
            UiRect::new(
                header.left + 12.0,
                header.top,
                header.right - 10.0,
                header.bottom,
            ),
            format!("{label} ▾"),
            theme::sans(theme::c().text, theme::SMALL),
        ));
    if app.resources.chooser {
        let options = std::iter::once((None, "Local repositories".to_owned())).chain(
            app.resources
                .model
                .entries
                .iter()
                .map(|(scope, entry)| (Some(*scope), entry.root.name.clone())),
        );
        let mut top = header.bottom;
        for (scope, label) in options {
            let row = UiRect::new(rect.left, top, rect.right, (top + 26.0).min(rect.bottom));
            if row.height() <= 0.0 {
                break;
            }
            top += 26.0;
            let choose = state.clone();
            root = root.child(
                panel(
                    row,
                    VisualStyle::filled(
                        if scope == selected && app.resources.model.offline.is_none() {
                            theme::c().surface
                        } else {
                            theme::c().sidebar
                        },
                    ),
                )
                .key(format!("git-environment-{scope:?}"))
                .event_policy(EventPolicy::INTERACTIVE)
                .semantics(Semantics::new(SemanticRole::Button).name(label.clone()))
                .cursor(CursorIcon::Pointer)
                .on_click(move || {
                    choose.update(|app| app.resources.chooser = false);
                    choose.dispatch(Command::Resource(ResourceCommand::Select(scope)));
                })
                .child(text(
                    UiRect::new(row.left + 12.0, row.top, row.right - 8.0, row.bottom),
                    label,
                    theme::sans(theme::c().text, theme::SMALL),
                )),
            );
        }
    }
    root
}

fn action(
    rect: UiRect,
    key: String,
    label: &'static str,
    enabled: bool,
    state: &Binding,
    command: ResourceCommand,
) -> Element {
    if !enabled {
        return panel(rect, VisualStyle::default()).child(text(
            rect,
            label,
            theme::sans(theme::c().text_ghost, theme::SMALL),
        ));
    }
    let state = state.clone();
    super::action_button::render(rect, key, label, label.into(), move || {
        state.dispatch(Command::Resource(command.clone()));
    })
}

pub(super) fn render(rect: UiRect, state: Binding, focus: UiFocusHandle, id: UiId) -> Element {
    let app = state.get();
    if let Some((_, name)) = &app.resources.model.offline {
        return panel(rect, VisualStyle::filled(theme::c().sidebar))
            .key("resource-git-offline")
            .child(text(
                UiRect::new(
                    rect.left + 12.0,
                    rect.top + 8.0,
                    rect.right - 12.0,
                    (rect.top + 80.0).min(rect.bottom),
                ),
                format!("{name} is disconnected. Reconnect its environment to use Source Control."),
                theme::sans(theme::c().text_dim, theme::SMALL),
            ));
    }
    let Some(scope) = app.resources.model.selected else {
        return panel(rect, VisualStyle::default());
    };
    let Some(entry) = app.resources.model.entries.get(&scope) else {
        return panel(rect, VisualStyle::default());
    };
    let mut root = panel(rect, VisualStyle::filled(theme::c().sidebar)).key("resource-git-changes");
    let branch =
        entry.repository.as_ref().map_or(
            "Source Control".to_owned(),
            |repository| match &repository.head {
                HeadState::Branch(name) | HeadState::Unborn(name) => name.clone(),
                HeadState::Detached(oid) => oid.chars().take(8).collect(),
            },
        );
    root = root
        .child(text(
            UiRect::new(
                rect.left + 12.0,
                rect.top,
                rect.right - 72.0,
                rect.top + 30.0,
            ),
            branch,
            theme::sans(theme::c().text, theme::UI_SIZE),
        ))
        .child(action(
            UiRect::new(
                rect.right - 70.0,
                rect.top + 4.0,
                rect.right - 8.0,
                rect.top + 26.0,
            ),
            "resource-git-refresh".into(),
            "Refresh",
            !entry.busy,
            &state,
            ResourceCommand::Refresh(scope),
        ));
    let status = entry.error.clone().unwrap_or_else(|| {
        if entry.busy {
            "Reading repository…".into()
        } else if entry.repository.is_none() {
            "Open a file in a Git repository to see its changes.".into()
        } else {
            String::new()
        }
    });
    let status_h = if status.is_empty() { 0.0 } else { 50.0 };
    root = root.child(text(
        UiRect::new(
            rect.left + 12.0,
            rect.top + 30.0,
            rect.right - 12.0,
            rect.top + 30.0 + status_h,
        ),
        status,
        theme::sans(theme::c().text_dim, theme::SMALL),
    ));
    let Some(repository) = &entry.repository else {
        return root;
    };
    let list = UiRect::new(
        rect.left,
        (rect.top + 32.0 + status_h).min(rect.bottom),
        rect.right,
        (rect.bottom - 140.0).max((rect.top + 32.0 + status_h).min(rect.bottom)),
    );
    let groups = [
        ("CONFLICTS", 0),
        ("STAGED", 1),
        ("UNSTAGED", 2),
        ("UNTRACKED", 3),
    ]
    .map(|(label, group)| (label, group, entry.groups[group].clone()));
    let height: f32 = groups
        .iter()
        .filter(|(_, _, paths)| !paths.is_empty())
        .map(|(_, _, paths)| 28.0 + paths.len() as f32 * 26.0)
        .sum();
    let max_scroll = (height - list.height()).max(0.0);
    let scroll = app.resources.scroll.clamp(0.0, max_scroll);
    let wheel = state.clone();
    let mut layer = clip(list, 0.0, -scroll);
    let mut top = list.top;
    for (label, group, paths) in groups {
        if paths.is_empty() {
            continue;
        }
        let header = UiRect::new(list.left + 12.0, top, list.right - 10.0, top + 28.0);
        top += 28.0;
        layer = layer.child(text(
            header,
            format!("{label}  {}", paths.len()),
            theme::sans(theme::c().text_dim, theme::SMALL),
        ));
        let start = (((list.top + scroll - top) / 26.0).floor().max(0.0) as usize).min(paths.len());
        let end = (((list.bottom + scroll - top) / 26.0).ceil().max(0.0) as usize).min(paths.len());
        let base = top;
        top += paths.len() as f32 * 26.0;
        for (index, path) in paths.iter().enumerate().take(end).skip(start) {
            let y = base + index as f32 * 26.0;
            let row = UiRect::new(list.left, y, list.right, y + 26.0);
            let compare = state.clone();
            let selected = path.clone();
            let target = if group == 1 {
                DiffTarget::HeadToIndex
            } else {
                DiffTarget::IndexToWorktree
            };
            layer = layer
                .child(
                    panel(
                        UiRect::new(row.left, row.top, row.right - 70.0, row.bottom),
                        VisualStyle::default(),
                    )
                    .key(format!("resource-git-file-{group}-{}", path.display()))
                    .event_policy(EventPolicy::INTERACTIVE)
                    .semantics(
                        Semantics::new(SemanticRole::Button)
                            .name(format!("Open diff {}", path.display())),
                    )
                    .cursor(CursorIcon::Pointer)
                    .on_click(move || {
                        compare.dispatch(Command::Resource(ResourceCommand::Diff(
                            scope,
                            selected.clone(),
                            target,
                        )));
                    })
                    .child(text(
                        UiRect::new(row.left + 12.0, row.top, row.right - 74.0, row.bottom),
                        path.display().to_string(),
                        theme::sans(theme::c().text, theme::SMALL),
                    )),
                )
                .child(action(
                    UiRect::new(
                        row.right - 68.0,
                        row.top + 2.0,
                        row.right - 8.0,
                        row.bottom - 2.0,
                    ),
                    format!("resource-git-stage-{group}-{}", path.display()),
                    if group == 1 { "Unstage" } else { "Stage" },
                    !entry.busy,
                    &state,
                    ResourceCommand::Stage(scope, vec![path.clone()], group == 1),
                ));
        }
    }
    if height == 0.0 {
        layer = layer.child(text(
            list,
            "No changes",
            theme::sans(theme::c().text_dim, theme::SMALL),
        ));
    }
    root = root.child(
        panel(list, VisualStyle::default())
            .key("resource-git-files")
            .on_wheel(move |cx, delta| {
                let step = match delta.unit {
                    WheelUnit::Lines => delta.y * 26.0,
                    WheelUnit::Pixels => delta.y,
                };
                wheel.update(|app| app.resources.scroll = (scroll - step).clamp(0.0, max_scroll));
                cx.stop_propagation();
            })
            .child(layer),
    );
    let input_rect = UiRect::new(
        rect.left + 12.0,
        (rect.bottom - 132.0).max(list.bottom),
        rect.right - 12.0,
        (rect.bottom - 42.0).max(list.bottom),
    );
    root = root.child(input::render_portable(
        input_rect,
        id,
        focus,
        InputBinding::new_access(
            state.panel.clone(),
            |app| {
                app.resources
                    .model
                    .selected
                    .and_then(|scope| app.resources.model.entries.get(&scope))
                    .and_then(|entry| app.resources.messages.get(&entry.root.resource))
                    .unwrap_or(&app.ui.commit_input)
            },
            |app| match app
                .resources
                .model
                .selected
                .and_then(|scope| app.resources.model.entries.get(&scope))
            {
                Some(entry) => app
                    .resources
                    .messages
                    .entry(entry.root.resource.clone())
                    .or_default(),
                None => &mut app.ui.commit_input,
            },
        ),
        AccessInputOptions {
            label: "Commit message",
            placeholder: "Commit staged changes",
            style: InputStyle {
                text: theme::sans(theme::c().text, 12.0),
                placeholder: theme::sans(theme::c().text_ghost, 12.0),
                caret: theme::c().text_faint,
                selection: theme::c().accent,
            },
            on_submit: None,
            on_cancel: None,
            on_blur: None,
        },
        true,
    ));
    let has_message = app
        .resources
        .messages
        .get(&entry.root.resource)
        .is_some_and(|input| !input.text().trim().is_empty());
    let staged = repository
        .files
        .values()
        .any(|file| file.index != ChangeKind::Unmodified);
    root.child(action(
        UiRect::new(
            rect.left + 12.0,
            (rect.bottom - 32.0).max(list.bottom),
            rect.right - 12.0,
            (rect.bottom - 8.0).max(list.bottom),
        ),
        "resource-git-commit".into(),
        "Commit",
        !entry.busy && staged && has_message,
        &state,
        ResourceCommand::Commit(scope),
    ))
}
