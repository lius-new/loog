//! Confirmation for irreversible file operations, outside the tree layout.
use super::commands::{Command, Commands, EditInput};
use super::state::ExplorerPanelState;
use crate::ui::components::input::{self, AccessInputOptions, InputBinding, InputStyle};
use crate::ui::components::state_access::StateAccess;
use crate::ui::theme;
use lgui::core::{EventPolicy, SemanticRole, Semantics};
use lgui::prelude::{Element, UiRect, VisualStyle, panel, text};
use std::sync::Arc;

pub fn visible(panel: &ExplorerPanelState) -> bool {
    super::source::deletion_name(panel).is_some()
}

pub fn render(
    viewport: UiRect,
    state: StateAccess<ExplorerPanelState>,
    commands: Commands,
    input: EditInput,
) -> Element {
    let snapshot = state.get();
    let Some(name) = super::source::deletion_name(&snapshot) else {
        return panel(viewport, VisualStyle::default());
    };
    let width = 520.0_f32.min(viewport.width());
    let left = viewport.left + (viewport.width() - width) / 2.0;
    let top = viewport.top + (viewport.height() - 230.0) * 0.36;
    let card = UiRect::new(left, top, left + width, top + 230.0);
    let field = UiRect::new(left + 22.0, top + 107.0, card.right - 22.0, top + 137.0);
    let submit = commands.clone();
    let cancel = commands.clone();
    let mut overlay = panel(viewport, VisualStyle::filled(theme::c().bg).alpha(214))
        .event_policy(EventPolicy::INTERACTIVE)
        .semantics(Semantics::new(SemanticRole::Dialog).name("Delete permanently?"))
        .on_key_down(|cx, _| {
            cx.prevent_default();
            cx.stop_propagation();
        })
        .child(theme::bordered(
            card,
            theme::c().sidebar,
            theme::c().border,
            8.0,
            1.0,
        ))
        .child(text(
            UiRect::new(left + 22.0, top + 20.0, card.right - 22.0, top + 44.0),
            "Delete permanently?",
            theme::sans_semibold(theme::c().text_bright, 15.0),
        ))
        .child(text(
            UiRect::new(left + 22.0, top + 52.0, card.right - 22.0, top + 76.0),
            format!("Type {name} to confirm. This cannot be undone."),
            theme::sans(theme::c().text_muted, theme::UI_SIZE),
        ))
        .child(theme::bordered(
            field,
            theme::c().bg,
            theme::c().border,
            3.0,
            1.0,
        ))
        .child(input::render_access(
            field,
            input.id,
            input.focus,
            InputBinding::new_access(
                state,
                |panel| &panel.ui.create_input,
                |panel| &mut panel.ui.create_input,
            ),
            AccessInputOptions {
                label: "Confirm file name",
                placeholder: "",
                style: InputStyle {
                    text: theme::mono(theme::c().text, theme::UI_SIZE),
                    placeholder: theme::mono(theme::c().text_faint, theme::UI_SIZE),
                    caret: theme::c().text,
                    selection: theme::c().accent,
                },
                on_submit: Some(Arc::new(move || submit.dispatch(Command::SubmitEdit))),
                on_cancel: Some(Arc::new(move || cancel.dispatch(Command::CancelEdit))),
                on_blur: None,
            },
        ));
    if let Some(error) = snapshot.ui.create_error {
        overlay = overlay.child(text(
            UiRect::new(left + 22.0, top + 141.0, card.right - 22.0, top + 163.0),
            error,
            theme::sans(theme::c().error_text, theme::SMALL),
        ));
    }
    for (label, offset, action, destructive) in [
        ("Cancel", 198.0, Command::CancelEdit, false),
        ("Delete", 104.0, Command::SubmitEdit, true),
    ] {
        let button = UiRect::new(
            card.right - offset,
            card.bottom - 46.0,
            card.right - offset + 82.0,
            card.bottom - 16.0,
        );
        let commands = commands.clone();
        let color = if destructive {
            theme::c().error_text
        } else {
            theme::c().text
        };
        overlay = overlay.child(
            theme::bordered(button, theme::c().sidebar, color, 4.0, 1.0)
                .event_policy(EventPolicy::INTERACTIVE)
                .on_click(move |cx: &mut lgui::core::UiEventContext| {
                    commands.dispatch(action.clone());
                    cx.stop_propagation();
                })
                .child(text(
                    UiRect::new(
                        button.left + 16.0,
                        button.top + 6.0,
                        button.right,
                        button.bottom,
                    ),
                    label,
                    theme::sans(color, theme::UI_SIZE),
                )),
        );
    }
    overlay
}
