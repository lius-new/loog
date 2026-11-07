//! Wrapped text input using the renderer's caret, selection and IME geometry.
use super::input::{AccessInputOptions, InputBinding, InputState};
use crate::ui::editor::keys::{self, Command};
use lgui::core::{
    CursorIcon, EventPolicy, SemanticRole, Semantics, TextStyle, TextWrap, UiElement,
    UiFocusHandle, UiId, WheelUnit, clip,
};
use lgui::prelude::{Element, UiRect, VisualStyle, group, panel, text};
use lgui::services::ServicesContextExt;
use lgui::text::{self as text_system, TextLayout, TextLayoutRequest, TextVerticalAlign};
use loog_editor_core::normal;

fn normalized(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\r', "\n")
}
fn byte_at(value: &str, index: usize) -> usize {
    value
        .char_indices()
        .nth(index)
        .map_or(value.len(), |(byte, _)| byte)
}
fn layout(value: &str, width: f32, style: TextStyle) -> TextLayout {
    text_system::layout(&TextLayoutRequest::for_style(
        value,
        UiRect::new(0.0, 0.0, width.max(1.0), 1_000_000.0),
        &style,
    ))
    .unwrap_or_default()
}
fn reveal(input: &mut InputState, width: f32, height: f32, style: TextStyle) {
    let measured = layout(input.text(), width, style);
    let index = input.text()[..input.selection.cursor].chars().count();
    if let Some(caret) = measured.caret_rect(index) {
        if caret.top < input.scroll_y {
            input.scroll_y = caret.top;
        }
        if caret.bottom > input.scroll_y + height {
            input.scroll_y = caret.bottom - height;
        }
    }
    input.scroll_y = input
        .scroll_y
        .clamp(0.0, (measured.height - height).max(0.0));
}

pub fn render<T: Clone + Send + 'static>(
    rect: UiRect,
    id: UiId,
    focus: UiFocusHandle,
    binding: InputBinding<T>,
    options: AccessInputOptions,
) -> Element {
    let inset = 8.0_f32
        .min(rect.width().max(0.0) / 2.0)
        .min(rect.height().max(0.0) / 2.0);
    let content = UiRect::new(
        rect.left + inset,
        rect.top + inset,
        (rect.right - inset).max(rect.left + inset),
        (rect.bottom - inset).max(rect.top + inset),
    );
    let mut style = options.style.text.wrap(TextWrap::Words);
    style.vertical_align = TextVerticalAlign::Top;
    let mut placeholder_style = options.style.placeholder.wrap(TextWrap::Words);
    placeholder_style.vertical_align = TextVerticalAlign::Top;
    let snapshot = binding.get();
    let cursor = snapshot.selection.cursor;
    let display = format!(
        "{}{}{}",
        &snapshot.text()[..cursor],
        snapshot.preedit,
        &snapshot.text()[cursor..]
    );
    let measured = layout(&display, content.width(), style);
    let original = layout(snapshot.text(), content.width(), style);
    let preedit_index = snapshot
        .preedit_cursor
        .as_ref()
        .map_or(snapshot.preedit.len(), |r| r.start)
        .min(snapshot.preedit.len());
    let preedit_index = snapshot
        .preedit
        .get(..preedit_index)
        .unwrap_or("")
        .chars()
        .count();
    let caret_index = snapshot.text()[..cursor].chars().count() + preedit_index;
    let caret =
        measured
            .caret_rect(caret_index)
            .unwrap_or(UiRect::new(0.0, 0.0, 1.0, style.height + 3.0));
    let max_scroll = (measured.height - content.height()).max(0.0);
    let scroll = snapshot.scroll_y.clamp(0.0, max_scroll);
    let caret_rect = UiRect::new(
        content.left + caret.left,
        content.top + caret.top - scroll,
        content.left + caret.left + 1.0,
        content.top + caret.bottom - scroll,
    );
    let mut semantics = Semantics::new(SemanticRole::TextInput)
        .name(options.label)
        .value(snapshot.text().to_owned());
    semantics.state.multiline = true;
    semantics.text.character_count = Some(snapshot.text().chars().count());
    let down = binding.clone();
    let drag = binding.clone();
    let up = binding.clone();
    let insert = binding.clone();
    let change = binding.clone();
    let key = binding.clone();
    let composition = binding.clone();
    let composition_end = binding.clone();
    let focused = binding.clone();
    let blur = binding.clone();
    let wheel = binding;
    let down_layout = original.clone();
    let drag_layout = original;
    let pointer_focus = focus;
    let root = Element::new(move |cx| {
        UiElement::panel(id, rect, VisualStyle::default())
            .ime_cursor_rect(caret_rect)
            .children(cx.children)
    })
    .semantics(semantics)
    .event_policy(EventPolicy::INTERACTIVE)
    .cursor(CursorIcon::Text)
    .on_pointer_down(move |cx, pointer| {
        pointer_focus.focus();
        let index = down_layout
            .hit_test(
                pointer.point.x - content.left,
                pointer.point.y - content.top + scroll,
            )
            .index;
        down.update(|input| {
            let byte = byte_at(input.text(), index);
            input.editor().select_to(byte, false);
            input.drag_anchor = Some(byte);
        });
        cx.stop_propagation();
    })
    .on_pointer_drag(move |cx, pointer| {
        let index = drag_layout
            .hit_test(
                pointer.point.x - content.left,
                pointer.point.y - content.top + scroll,
            )
            .index;
        drag.update(|input| {
            if let Some(anchor) = input.drag_anchor {
                let byte = byte_at(input.text(), index);
                input.editor().set_cursor(anchor);
                input.editor().select_to(byte, true);
                reveal(input, content.width(), content.height(), style);
            }
        });
        cx.stop_propagation();
    })
    .on_pointer_up(move |_, _| up.update(|input| input.drag_anchor = None))
    .on_input(move |cx, value| {
        let value = normalized(value);
        insert.update(|input| {
            normal::insert(&mut input.editor(), &value);
            input.preedit.clear();
            input.preedit_cursor = None;
            reveal(input, content.width(), content.height(), style);
        });
        cx.stop_propagation();
    })
    .on_change(move |_, value| {
        change.update(|input| {
            input.set_text(normalized(value.unwrap_or_default()));
            reveal(input, content.width(), content.height(), style);
        })
    })
    .on_key_down(move |cx, event| {
        if !key.get().preedit.is_empty() {
            return;
        }
        let Some(command) = keys::command_for(event, 3) else {
            return;
        };
        if command == Command::Escape {
            if let Some(cancel) = &options.on_cancel {
                cancel();
            } else {
                return;
            }
        }
        if matches!(command, Command::Indent(_)) {
            return;
        }
        let clipboard = cx.application().clipboard();
        key.update(|input| {
            match command {
                Command::Copy | Command::Cut => {
                    let mut editor = input.editor();
                    if let Some(value) = editor.selected_text() {
                        if clipboard.write_text(value).is_ok() && command == Command::Cut {
                            normal::backspace(&mut editor);
                        }
                    }
                    editor.break_undo_group();
                }
                Command::Paste => {
                    if let Ok(Some(value)) = clipboard.read_text() {
                        normal::insert(&mut input.editor(), &normalized(&value));
                        input.editor().break_undo_group();
                    }
                }
                _ => keys::apply(&mut input.editor(), command),
            }
            reveal(input, content.width(), content.height(), style);
        });
        cx.prevent_default();
        cx.stop_propagation();
    })
    .on_composition_update(move |_, value, cursor| {
        composition.update(|input| {
            input.preedit = value.to_owned();
            input.preedit_cursor = cursor.clone();
        })
    })
    .on_composition_end(move |_| {
        composition_end.update(|input| {
            input.preedit.clear();
            input.preedit_cursor = None;
        })
    })
    .on_focus(move |_| focused.update(|input| input.focused = true))
    .on_blur(move |_| {
        blur.update(|input| {
            input.focused = false;
            input.drag_anchor = None;
            input.preedit.clear();
            input.preedit_cursor = None;
            input.editor().break_undo_group();
        });
        if let Some(on_blur) = &options.on_blur {
            on_blur();
        }
    })
    .on_wheel(move |cx, delta| {
        let step = match delta.unit {
            WheelUnit::Lines => delta.y * 36.0,
            WheelUnit::Pixels => delta.y,
        };
        wheel.update(|input| input.scroll_y = (scroll - step).clamp(0.0, max_scroll));
        cx.stop_propagation();
    });
    let bounds = UiRect::new(
        content.left,
        content.top,
        content.right,
        content.top + measured.height.max(content.height()),
    );
    let mut layer = group(bounds);
    if let Some(selection) = snapshot.selection.range() {
        let range = snapshot.text()[..selection.start].chars().count()
            ..snapshot.text()[..selection.end].chars().count();
        for rect in measured.selection_rects(range) {
            layer = layer.child(panel(
                rect.translate(content.left, content.top),
                VisualStyle::filled(options.style.selection).alpha(70),
            ));
        }
    }
    layer = layer.child(if display.is_empty() {
        text(bounds, options.placeholder, placeholder_style)
    } else {
        text(bounds, display, style)
    });
    if !snapshot.preedit.is_empty() {
        let start = snapshot.text()[..cursor].chars().count();
        for rect in measured.selection_rects(start..start + snapshot.preedit.chars().count()) {
            layer = layer.child(panel(
                UiRect::new(
                    content.left + rect.left,
                    content.top + rect.bottom - 1.0,
                    content.left + rect.right,
                    content.top + rect.bottom,
                ),
                VisualStyle::filled(options.style.caret),
            ));
        }
    }
    if snapshot.focused {
        layer = layer.child(panel(
            UiRect::new(
                content.left + caret.left,
                content.top + caret.top,
                content.left + caret.left + 1.0,
                content.top + caret.bottom,
            ),
            VisualStyle::filled(options.style.caret),
        ));
    }
    root.child(clip(content, 0.0, -scroll).child(layer))
}
