//! Terminal choices and editable user profiles on the Settings page.

use crate::features::terminal::ShellKind;
use crate::features::terminal::profiles::format_arguments;
use crate::ui::components::input::{
    self, AccessInputOptions, InputBinding, InputState, InputStyle,
};
use crate::ui::tabs::{TEXT_MARGIN, ellipsize, measure};
use crate::ui::terminal::settings_binding::{Binding, Command, Frame};
use crate::ui::terminal::state::{ProfileEditor, TerminalUiState};
use crate::ui::theme;
use lgui::core::{CursorIcon, EventPolicy};
use lgui::prelude::{Element, TextAlign, UiRect, VisualStyle, component, group, panel, text};

/// Label and description, matching the other settings rows.
const HEADER: f32 = 58.0;
/// Space below the terminal list before the next setting.
const FOOTER: f32 = 20.0;
const ROW: f32 = 48.0;
const RADIO: f32 = 14.0;
const BUTTON_H: f32 = 28.0;
const BUTTON_PAD_X: f32 = 12.0;
const ADD_LABEL: &str = "Add terminal";
const LABEL_W: f32 = 96.0;
const FORM_TITLE_H: f32 = 44.0;
const FIELD_H: f32 = 30.0;
const FIELD_STRIDE: f32 = 40.0;
const FORM_H: f32 = FORM_TITLE_H + FIELD_STRIDE * 2.0 + FIELD_H + 14.0 + BUTTON_H + 16.0;

pub fn height(app: &Frame, ui: &TerminalUiState) -> f32 {
    HEADER + card_height(app, ui) + FOOTER
}

fn card_height(app: &Frame, ui: &TerminalUiState) -> f32 {
    let editing = ui.shell_editor.as_ref().filter(|e| e.open);
    let rows = app.shell_options.len().max(1) as f32 * ROW;
    let form = match editing {
        Some(editor) if editor.id.is_none() => FORM_H,
        // An edited row is replaced by its form.
        Some(_) => FORM_H - ROW,
        None => 0.0,
    };
    rows + form + 2.0
}

pub fn render(rect: UiRect, state: Binding) -> Element {
    let frame = state.frame();
    let app = &frame;
    let ui_snapshot = state.ui.get();
    let ui = &ui_snapshot;
    let add_w = measure(ADD_LABEL, theme::UI_SIZE, 400) + BUTTON_PAD_X * 2.0;
    let add_rect = UiRect::new(
        (rect.right - add_w).max(rect.left),
        rect.top + 14.0,
        rect.right,
        rect.top + 14.0 + BUTTON_H,
    );
    let text_right = (add_rect.left - 24.0).max(rect.left);
    let add = state.clone();
    let description = match app.project_shell.clone() {
        Some((folder, shell)) => ellipsize(
            &format!("This is your default. {folder} uses {shell} from its .loog/settings.json."),
            (text_right - rect.left).max(0.0),
            theme::SMALL + 1.0,
            400,
        ),
        None => "Used when a new terminal opens without picking a shell.".to_owned(),
    };
    let root = group(rect)
        .child(text(
            UiRect::new(rect.left, rect.top + 11.0, text_right, rect.top + 29.0),
            "Default shell",
            theme::sans(theme::c().text, theme::UI_SIZE),
        ))
        .child(text(
            UiRect::new(rect.left, rect.top + 30.0, text_right, rect.top + 48.0),
            description,
            theme::sans(theme::c().text_dim, theme::SMALL + 1.0),
        ))
        .child(button(add_rect, ADD_LABEL, "shell-add", false, move || {
            add.run(Command::Edit(None))
        }));

    let card = UiRect::new(
        rect.left,
        rect.top + HEADER,
        rect.right,
        rect.top + HEADER + card_height(app, ui),
    );
    let mut list = theme::bordered(card, theme::c().surface, theme::c().border, 6.0, 1.0);
    let inner_left = card.left + 1.0;
    let inner_right = card.right - 1.0;
    let editor = ui.shell_editor.as_ref().filter(|e| e.open);
    let mut top = card.top + 1.0;
    let mut first = true;
    let divider = |list: Element, top: f32, first: &mut bool| {
        let list = if *first {
            list
        } else {
            list.child(panel(
                UiRect::new(inner_left, top, inner_right, top + 1.0),
                VisualStyle::filled(theme::c().border),
            ))
        };
        *first = false;
        list
    };
    for option in &app.shell_options {
        list = divider(list, top, &mut first);
        let editing_this = matches!(
            (editor.and_then(|e| e.id), option.kind),
            (Some(id), ShellKind::Custom(kind)) if id == kind
        );
        if editing_this {
            let form = UiRect::new(inner_left, top, inner_right, top + FORM_H);
            list = list.child(editor_form(form, state.clone(), editor.unwrap()));
            top += FORM_H;
        } else {
            let row = UiRect::new(inner_left, top, inner_right, top + ROW);
            list = list.child(shell_row(row, option, app, state.clone()));
            top += ROW;
        }
    }
    if app.shell_options.is_empty() {
        list = list.child(text(
            UiRect::new(inner_left + 16.0, top, inner_right - 16.0, top + ROW),
            "No shell was found. Add a terminal to get started.",
            theme::sans(theme::c().text_dim, theme::UI_SIZE),
        ));
        first = false;
        top += ROW;
    }
    if let Some(editor) = editor.filter(|e| e.id.is_none()) {
        list = divider(list, top, &mut first);
        let form = UiRect::new(inner_left, top, inner_right, top + FORM_H);
        list = list.child(editor_form(form, state, editor));
    }
    root.child(list)
}

fn shell_row(
    row: UiRect,
    option: &crate::features::terminal::profiles::ShellOption,
    app: &Frame,
    state: Binding,
) -> Element {
    let available = option.available();
    let active = option.kind == app.default_shell;
    let custom = match option.kind {
        ShellKind::Custom(id) => Some(id),
        _ => None,
    };
    let actions_w = if custom.is_some() { 132.0 } else { 0.0 };
    let text_left = row.left + 44.0;
    let text_right = (row.right - actions_w - 16.0).max(text_left);

    let mut item = panel(
        row,
        if active {
            VisualStyle::filled(theme::c().active_line)
        } else {
            VisualStyle::default()
        },
    )
    .key(format!("settings-shell-{:?}", option.kind));
    if available {
        let select = state.clone();
        let kind = option.kind;
        item = item
            .event_policy(EventPolicy::INTERACTIVE)
            .cursor(CursorIcon::Pointer)
            .on_click(move || select.run(Command::Select(kind)));
    }
    item = item.child(radio(
        UiRect::new(
            row.left + 16.0,
            row.top + (ROW - RADIO) / 2.0,
            row.left + 16.0 + RADIO,
            row.top + (ROW + RADIO) / 2.0,
        ),
        active,
        available,
    ));

    // Name, then a status tag that only appears when it says something.
    let name_w = measure(&option.name, theme::UI_SIZE, 400) + TEXT_MARGIN;
    let tag = if !available && active && app.effective_shell != option.kind {
        Some((
            format!(
                "Unavailable · using {}",
                app.shell_name(app.effective_shell)
            ),
            theme::c().warning,
        ))
    } else if !available {
        Some(("Unavailable".to_owned(), theme::c().warning))
    } else if active {
        Some(("Default".to_owned(), theme::c().accent))
    } else {
        None
    };
    let name_right = (text_left + name_w).min(text_right);
    item = item.child(text(
        UiRect::new(text_left, row.top + 7.0, name_right, row.top + 25.0),
        ellipsize(
            &option.name,
            (name_right - text_left).max(0.0),
            theme::UI_SIZE,
            400,
        ),
        theme::sans(
            if available {
                theme::c().text_bright
            } else {
                theme::c().text_muted
            },
            theme::UI_SIZE,
        ),
    ));
    if let Some((tag, color)) = tag {
        let left = (name_right - TEXT_MARGIN + 4.0).max(text_left);
        item = item.child(text(
            UiRect::new(left, row.top + 8.0, text_right.max(left), row.top + 25.0),
            ellipsize(&tag, (text_right - left).max(0.0), theme::SMALL, 400),
            theme::sans(color, theme::SMALL),
        ));
    }

    let mut detail = option
        .program
        .as_ref()
        .map(|p| crate::ui::display_path(p))
        .unwrap_or_else(|| "Edit the executable path to use this terminal.".into());
    if !option.args.is_empty() {
        detail = format!("{detail}  {}", format_arguments(&option.args));
    }
    item = item.child(text(
        UiRect::new(text_left, row.top + 25.0, text_right, row.top + 42.0),
        ellipsize(
            &detail,
            (text_right - text_left).max(0.0),
            theme::SMALL,
            400,
        ),
        theme::mono(theme::c().text_dim, theme::SMALL),
    ));

    if let Some(id) = custom {
        let edit_state = state.clone();
        let top = row.top + (ROW - BUTTON_H) / 2.0;
        item = item
            .child(button(
                UiRect::new(row.right - 136.0, top, row.right - 80.0, top + BUTTON_H),
                "Edit",
                &format!("shell-edit-{id}"),
                false,
                move || edit_state.run(Command::Edit(Some(id))),
            ))
            .child(button(
                UiRect::new(row.right - 74.0, top, row.right - 12.0, top + BUTTON_H),
                "Remove",
                &format!("shell-delete-{id}"),
                false,
                move || state.run(Command::Remove(id)),
            ));
    }
    item
}

fn radio(rect: UiRect, on: bool, enabled: bool) -> Element {
    let ring = if on {
        theme::c().accent
    } else if enabled {
        theme::c().text_dim
    } else {
        theme::c().text_ghost
    };
    let mut radio = theme::bordered(rect, theme::c().surface, ring, RADIO / 2.0, 1.5);
    if on {
        let inset = (RADIO - 6.0) / 2.0;
        radio = radio.child(panel(
            UiRect::new(
                rect.left + inset,
                rect.top + inset,
                rect.right - inset,
                rect.bottom - inset,
            ),
            VisualStyle::filled(theme::c().accent).radius(3.0),
        ));
    }
    radio
}

fn button(
    rect: UiRect,
    label: &'static str,
    key: &str,
    primary: bool,
    click: impl Fn() + Send + Sync + 'static,
) -> Element {
    let mut label_style = theme::sans(
        if primary {
            theme::c().on_accent
        } else {
            theme::c().text
        },
        theme::UI_SIZE,
    );
    label_style.align = TextAlign::Center;
    let frame = if primary {
        panel(rect, VisualStyle::filled(theme::c().accent).radius(4.0))
    } else {
        theme::bordered(rect, theme::c().surface, theme::c().border, 4.0, 1.0)
    };
    frame
        .key(key)
        .event_policy(EventPolicy::INTERACTIVE)
        .cursor(CursorIcon::Pointer)
        .on_click(move |cx: &mut lgui::core::UiEventContext| {
            click();
            cx.stop_propagation();
        })
        .child(text(rect, label, label_style))
}

fn editor_form(rect: UiRect, state: Binding, editor: &ProfileEditor) -> Element {
    let mut root = panel(rect, VisualStyle::filled(theme::c().bg));
    root = root.child(text(
        UiRect::new(
            rect.left + 16.0,
            rect.top + 12.0,
            rect.right - 16.0,
            rect.top + 30.0,
        ),
        if editor.id.is_some() {
            "Edit terminal"
        } else {
            "New terminal"
        },
        theme::sans_semibold(theme::c().text_bright, theme::UI_SIZE),
    ));
    type Read = fn(&TerminalUiState) -> &InputState;
    type Write = fn(&mut TerminalUiState) -> &mut InputState;
    let fields: [(&str, &'static str, Read, Write); 3] = [
        (
            "Name",
            "My shell",
            |app| &app.shell_editor.as_ref().unwrap().name,
            |app| &mut app.shell_editor.as_mut().unwrap().name,
        ),
        (
            "Executable",
            "Path or command name, e.g. nu",
            |app| &app.shell_editor.as_ref().unwrap().program,
            |app| &mut app.shell_editor.as_mut().unwrap().program,
        ),
        (
            "Arguments",
            "Optional, e.g. --login \"-c some command\"",
            |app| &app.shell_editor.as_ref().unwrap().args,
            |app| &mut app.shell_editor.as_mut().unwrap().args,
        ),
    ];
    let label_w = (rect.width() * 0.25).clamp(0.0, LABEL_W);
    let input_left = rect.left + 16.0 + label_w;
    let input_right = (rect.right - 16.0).max(input_left);
    for (index, (label, placeholder, read, write)) in fields.into_iter().enumerate() {
        let top = rect.top + FORM_TITLE_H + index as f32 * FIELD_STRIDE;
        root = root.child(text(
            UiRect::new(rect.left + 16.0, top, input_left, top + FIELD_H),
            label,
            theme::sans(theme::c().text_muted, theme::UI_SIZE),
        ));
        let frame = UiRect::new(input_left, top, input_right, top + FIELD_H);
        let input_state = state.clone();
        root = root.child(
            theme::bordered(frame, theme::c().surface, theme::c().border, 4.0, 1.0).child(
                component(
                    UiRect::new(
                        frame.left + 4.0,
                        frame.top + 1.0,
                        (frame.right - 4.0).max(frame.left + 4.0),
                        frame.bottom - 1.0,
                    ),
                    move |cx, &rect| {
                        let id = cx.use_stable_id();
                        let focus = cx.focus_handle(id.clone());
                        let focus_state = input_state.clone();
                        if index == 0 {
                            let mounted = focus.clone();
                            cx.use_effect((), move || {
                                mounted.focus();
                            });
                        }
                        let submit = input_state.clone();
                        let cancel = input_state.clone();
                        input::render_access(
                            rect,
                            id,
                            focus,
                            InputBinding::new_access(input_state.ui.clone(), read, write),
                            AccessInputOptions {
                                label: placeholder,
                                placeholder,
                                style: InputStyle {
                                    text: theme::mono(theme::c().text, theme::UI_SIZE),
                                    placeholder: theme::mono(theme::c().text_faint, theme::UI_SIZE),
                                    caret: theme::c().text,
                                    selection: theme::c().selection,
                                },
                                on_submit: Some(std::sync::Arc::new(move || {
                                    submit.run(Command::Save)
                                })),
                                on_cancel: Some(std::sync::Arc::new(move || {
                                    cancel.run(Command::Close)
                                })),
                                on_blur: None,
                            },
                        )
                        .on_focus(move |_| focus_state.run(Command::Focus))
                    },
                )
                .key(format!("shell-profile-input-{index}")),
            ),
        );
    }

    let footer = rect.bottom - 16.0 - BUTTON_H;
    let save_rect = UiRect::new(
        (rect.right - 16.0 - 72.0).max(rect.left),
        footer,
        rect.right - 16.0,
        footer + BUTTON_H,
    );
    let cancel_rect = UiRect::new(
        (save_rect.left - 8.0 - 72.0).max(rect.left),
        footer,
        save_rect.left - 8.0,
        footer + BUTTON_H,
    );
    if let Some(error) = &editor.error {
        root = root.child(text(
            UiRect::new(
                input_left,
                footer,
                (cancel_rect.left - 12.0).max(input_left),
                footer + BUTTON_H,
            ),
            ellipsize(
                error,
                (cancel_rect.left - 12.0 - input_left).max(0.0),
                theme::SMALL + 1.0,
                400,
            ),
            theme::sans(theme::c().error_text, theme::SMALL + 1.0),
        ));
    }
    let save = state.clone();
    root.child(button(
        cancel_rect,
        "Cancel",
        "shell-cancel",
        false,
        move || state.run(Command::Close),
    ))
    .child(button(save_rect, "Save", "shell-save", true, move || {
        save.run(Command::Save)
    }))
}
