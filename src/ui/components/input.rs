//! Reusable single-line text input with renderer-backed measurement.

use std::ops::Range;

use lgui::core::{
    Color, CursorIcon, EventPolicy, SemanticRole, Semantics, TextStyle, UiElement, UiFocusHandle,
    UiId, clip,
};
use lgui::prelude::{Element, State, UiRect, VisualStyle, group, panel};
use lgui::services::ServicesContextExt;

use crate::ui::editor::keys::{self as commands, Command};
use loog_editor_core::normal::{self, Movement};
use loog_editor_core::text::{EditorMut, Selection, TextBuffer};

use super::state_access::StateAccess;
use super::text::SingleLineText;
use std::sync::Arc;

const TEXT_INSET: f32 = 6.0;
const CURSOR_MARGIN: f32 = 8.0;

#[derive(Clone)]
pub struct InputState {
    portable: lgui::TextInputModel,
    buffer: TextBuffer,
    pub(super) selection: Selection,
    pub focused: bool,
    pub scroll_x: f32,
    pub scroll_y: f32,
    pub preedit: String,
    pub preedit_cursor: Option<Range<usize>>,
    pub(super) drag_anchor: Option<usize>,
    /// Draw a bullet for every character instead of the text, and keep the text
    /// out of the accessibility value. Only [`render_access`] honours it.
    pub masked: bool,
}

/// What a masked field draws for each character.
const MASK: char = '•';

/// The byte offset `offset` of `text` once every character is a [`MASK`].
fn masked_offset(text: &str, offset: usize) -> usize {
    text.get(..offset.min(text.len()))
        .map_or(0, |before| before.chars().count())
        * MASK.len_utf8()
}

/// The byte offset of `text` that a masked field's byte offset `offset` stands for.
fn unmasked_offset(text: &str, offset: usize) -> usize {
    text.char_indices()
        .nth(offset / MASK.len_utf8())
        .map_or(text.len(), |(index, _)| index)
}

impl Default for InputState {
    fn default() -> Self {
        Self::new("")
    }
}

impl InputState {
    pub fn new(value: impl Into<String>) -> Self {
        let buffer = TextBuffer::new(value);
        let selection = Selection::caret(buffer.text().len());
        Self {
            portable: lgui::TextInputModel::new(buffer.text()),
            buffer,
            selection,
            focused: false,
            scroll_x: 0.0,
            scroll_y: 0.0,
            preedit: String::new(),
            preedit_cursor: None,
            drag_anchor: None,
            masked: false,
        }
    }

    /// The text, all of it selected, so typing replaces it.
    pub fn new_selected(value: impl Into<String>) -> Self {
        let mut input = Self::new(value);
        input.selection.anchor = Some(0);
        input
    }

    pub fn text(&self) -> &str {
        self.buffer.text()
    }

    pub(super) fn editor(&mut self) -> EditorMut<'_> {
        EditorMut::new(&mut self.buffer, &mut self.selection, Vec::new())
    }

    pub fn clear(&mut self) {
        self.buffer = TextBuffer::new("");
        self.selection = Selection::default();
        self.scroll_x = 0.0;
        self.scroll_y = 0.0;
        self.preedit.clear();
        self.preedit_cursor = None;
        self.drag_anchor = None;
    }

    pub fn set_text(&mut self, value: impl Into<String>) {
        let value = value.into();
        self.selection = Selection::caret(value.len());
        self.buffer = TextBuffer::new(value);
        self.preedit.clear();
        self.preedit_cursor = None;
    }
}

#[derive(Clone)]
pub struct InputBinding<T> {
    state: StateAccess<T>,
    legacy_state: Option<State<T>>,
    read: fn(&T) -> &InputState,
    write: fn(&mut T) -> &mut InputState,
}

impl<T> InputBinding<T>
where
    T: Clone + Send + 'static,
{
    pub fn new(
        state: State<T>,
        read: fn(&T) -> &InputState,
        write: fn(&mut T) -> &mut InputState,
    ) -> Self {
        Self {
            state: state.clone().into(),
            legacy_state: Some(state),
            read,
            write,
        }
    }

    pub fn new_access(
        state: StateAccess<T>,
        read: fn(&T) -> &InputState,
        write: fn(&mut T) -> &mut InputState,
    ) -> Self {
        Self {
            state,
            legacy_state: None,
            read,
            write,
        }
    }

    pub(super) fn get(&self) -> InputState {
        let state = self.state.get();
        (self.read)(&state).clone()
    }

    pub(super) fn update(&self, update: impl FnOnce(&mut InputState)) {
        let write = self.write;
        self.state.update(move |state| update(write(state)));
    }
}

#[derive(Clone, Copy)]
pub struct InputStyle {
    pub text: TextStyle,
    pub placeholder: TextStyle,
    pub caret: Color,
    pub selection: Color,
}

pub struct InputOptions<T> {
    pub label: &'static str,
    pub placeholder: &'static str,
    pub style: InputStyle,
    pub on_submit: Option<fn(&State<T>)>,
    pub on_cancel: Option<fn(&State<T>)>,
    pub on_blur: Option<fn(&State<T>)>,
}

pub struct AccessInputOptions {
    pub label: &'static str,
    pub placeholder: &'static str,
    pub style: InputStyle,
    pub on_submit: Option<Arc<dyn Fn() + Send + Sync>>,
    pub on_cancel: Option<Arc<dyn Fn() + Send + Sync>>,
    pub on_blur: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// Keep Loog's projected state and actions while LGUI owns text editing/rendering.
pub fn render_portable<T: Clone + Send + 'static>(
    rect: UiRect,
    id: UiId,
    _focus: UiFocusHandle,
    binding: InputBinding<T>,
    options: AccessInputOptions,
    multiline: bool,
) -> Element {
    let reader = binding.clone();
    let writer = binding;
    let value = lgui::WidgetValue::new(
        move || {
            let input = reader.get();
            let mut model = input.portable.clone();
            if model.text != input.text() {
                model = lgui::TextInputModel::new(input.text());
            }
            model.select(
                input.selection.anchor.unwrap_or(input.selection.cursor),
                input.selection.cursor,
            );
            model.focused = input.focused;
            model.scroll = lgui::core::Point::new(input.scroll_x, input.scroll_y);
            model.preedit = input.preedit;
            model.preedit_cursor = input.preedit_cursor;
            model
        },
        move |model| {
            writer.update(|input| {
                if input.text() != model.text {
                    input.buffer = TextBuffer::new(model.text.clone());
                }
                input.selection = Selection::caret(model.cursor);
                if model.anchor != model.cursor {
                    input.selection.anchor = Some(model.anchor);
                }
                input.focused = model.focused;
                input.scroll_x = model.scroll.x;
                input.scroll_y = model.scroll.y;
                input.preedit = model.preedit.clone();
                input.preedit_cursor = model.preedit_cursor.clone();
                input.portable = model;
            })
        },
    );
    let label = options.label;
    let placeholder = options.placeholder;
    let style = options.style;
    let submit = options.on_submit;
    let cancel = options.on_cancel;
    let blur = options.on_blur;
    Element::new(move |cx| {
        let clipboard = cx
            .try_use_context::<lgui::application::ApplicationContext>()
            .map(|app| app.clipboard());
        let read = clipboard.clone();
        let write = clipboard;
        let clipboard = lgui::TextClipboard {
            read: read.map(|port| {
                Arc::new(move || port.read_text().ok().flatten().unwrap_or_default())
                    as Arc<dyn Fn() -> String + Send + Sync>
            }),
            write: write.map(|port| {
                Arc::new(move |text: String| {
                    let _ = port.write_text(&text);
                }) as Arc<dyn Fn(String) + Send + Sync>
            }),
        };
        let input = lgui::text_input(rect, value)
            .id(id)
            .label(label)
            .multiline(multiline)
            .placeholder(placeholder)
            .clipboard(clipboard)
            .style(VisualStyle::default(), style.text);
        cx.compile(
            Element::from(input)
                .on_event_capture(lgui::core::UiEventKind::KeyDown, move |cx, payload| {
                    let lgui::core::UiEventPayload::Keyboard { event } = payload else {
                        return;
                    };
                    let action = match event.key {
                        lgui::core::LogicalKey::Named(lgui::core::NamedKey::Enter)
                            if !multiline =>
                        {
                            submit.as_ref()
                        }
                        lgui::core::LogicalKey::Named(lgui::core::NamedKey::Escape) => {
                            cancel.as_ref()
                        }
                        _ => None,
                    };
                    if let Some(action) = action {
                        action();
                        cx.prevent_default();
                        cx.stop_propagation();
                    }
                })
                .on_blur(move |_| {
                    if let Some(action) = &blur {
                        action();
                    }
                }),
        )
    })
}

pub fn render<T>(
    rect: UiRect,
    id: UiId,
    focus: UiFocusHandle,
    binding: InputBinding<T>,
    options: InputOptions<T>,
) -> Element
where
    T: Clone + Send + 'static,
{
    let state = binding
        .legacy_state
        .clone()
        .expect("use render_access for projected input bindings");
    let callback = |action: Option<fn(&State<T>)>| -> Option<Arc<dyn Fn() + Send + Sync>> {
        action.map(|action| {
            let state = state.clone();
            Arc::new(move || action(&state)) as Arc<dyn Fn() + Send + Sync>
        })
    };
    let options = AccessInputOptions {
        label: options.label,
        placeholder: options.placeholder,
        style: options.style,
        on_submit: callback(options.on_submit),
        on_cancel: callback(options.on_cancel),
        on_blur: callback(options.on_blur),
    };
    render_access(rect, id, focus, binding, options)
}

pub fn render_access<T>(
    rect: UiRect,
    id: UiId,
    focus: UiFocusHandle,
    binding: InputBinding<T>,
    options: AccessInputOptions,
) -> Element
where
    T: Clone + Send + 'static,
{
    // Minimize transitions can leave less room than the parent's fixed padding.
    // Keep the input and its inset content ordered even when that room is zero.
    let rect = UiRect::new(
        rect.left,
        rect.top,
        rect.right.max(rect.left),
        rect.bottom.max(rect.top),
    );
    let text_inset = TEXT_INSET.min(rect.width() / 2.0);
    let snapshot = binding.get();
    let style = options.style;
    let label = options.label;
    let placeholder = options.placeholder;
    let on_submit = options.on_submit;
    let on_cancel = options.on_cancel;
    let on_blur = options.on_blur;
    let content = UiRect::new(
        rect.left + text_inset,
        rect.top,
        (rect.right - text_inset).max(rect.left + text_inset),
        rect.bottom,
    );
    let text_bounds = UiRect::new(
        content.left,
        content.top,
        content.left + 100_000.0,
        content.bottom,
    );
    let measure_bounds = UiRect::new(0.0, 0.0, 100_000.0, rect.height());
    let masked = snapshot.masked;
    // What is drawn: the text, or a bullet per character for a secret. Offsets into
    // the real text are translated at each place that meets the drawn text.
    let shown = if masked {
        MASK.to_string().repeat(snapshot.text().chars().count())
    } else {
        snapshot.text().to_owned()
    };
    let shown_offset = |offset: usize| {
        if masked {
            masked_offset(snapshot.text(), offset)
        } else {
            offset
        }
    };
    let real_text = if masked {
        snapshot.text().to_owned()
    } else {
        String::new()
    };
    let measured = SingleLineText::new(shown.as_str(), measure_bounds, style.text);
    let viewport_w = content.width().max(1.0);
    let base_caret = measured.caret_offset(shown_offset(snapshot.selection.cursor));
    let preedit_cursor = snapshot
        .preedit_cursor
        .as_ref()
        .map_or(snapshot.preedit.len(), |range| range.start)
        .min(snapshot.preedit.len());
    let preedit_prefix = snapshot.preedit.get(..preedit_cursor).unwrap_or("");
    let preedit_bounds = measure_bounds;
    let preedit_caret = SingleLineText::new(preedit_prefix, preedit_bounds, style.text).width();
    let preedit_width = SingleLineText::new(&snapshot.preedit, preedit_bounds, style.text).width();
    let content_width = measured.width().max(base_caret + preedit_width);
    let scroll = scroll_for_position(
        snapshot.scroll_x,
        content_width,
        base_caret + preedit_caret,
        viewport_w,
    );
    let caret_x =
        (content.left + base_caret + preedit_caret - scroll).clamp(content.left, content.right);
    let caret_height = (style.text.height + 4.0)
        .min((rect.height() - 4.0).max(1.0))
        .max(1.0);
    let caret_top = rect.top + (rect.height() - caret_height) / 2.0;
    let caret_rect = UiRect::new(caret_x, caret_top, caret_x + 1.0, caret_top + caret_height);
    // A secret never reaches the accessibility tree.
    let mut semantics = Semantics::new(SemanticRole::TextInput)
        .name(label)
        .value(if masked {
            String::new()
        } else {
            snapshot.text().to_owned()
        });
    semantics.text.character_count = Some(snapshot.text().chars().count());

    let pointer_layout = measured.clone();
    let drag_text = real_text.clone();
    let pointer_text = real_text;
    let pointer_binding = binding.clone();
    let pointer_focus = focus.clone();
    let drag_layout = measured.clone();
    let drag_binding = binding.clone();
    let up_binding = binding.clone();
    let input_binding = binding.clone();
    let change_binding = binding.clone();
    let key_binding = binding.clone();
    let composition_binding = binding.clone();
    let composition_end_binding = binding.clone();
    let focus_binding = binding.clone();
    let blur_binding = binding.clone();

    let mut root = Element::new(move |cx| {
        UiElement::panel(id, rect, VisualStyle::default())
            .ime_cursor_rect(caret_rect)
            .children(cx.children)
    })
    .event_policy(EventPolicy::INTERACTIVE)
    .semantics(semantics)
    .cursor(CursorIcon::Text)
    .on_pointer_down(move |cx, pointer| {
        pointer_focus.focus();
        let offset = pointer_layout.hit_byte_offset(
            pointer.point.x - content.left + scroll,
            pointer.point.y - rect.top,
        );
        let offset = if masked {
            unmasked_offset(&pointer_text, offset)
        } else {
            offset
        };
        pointer_binding.update(move |input| {
            input.editor().select_to(offset, false);
            input.drag_anchor = Some(offset);
            reveal_cursor(input, viewport_w, style.text);
        });
        cx.stop_propagation();
    })
    .on_pointer_drag(move |cx, pointer| {
        let offset = drag_layout.hit_byte_offset(
            pointer.point.x - content.left + scroll,
            pointer.point.y - rect.top,
        );
        let offset = if masked {
            unmasked_offset(&drag_text, offset)
        } else {
            offset
        };
        drag_binding.update(move |input| {
            if let Some(anchor) = input.drag_anchor {
                input.editor().set_cursor(anchor);
                input.editor().select_to(offset, true);
                reveal_cursor(input, viewport_w, style.text);
            }
        });
        cx.stop_propagation();
    })
    .on_pointer_up(move |_cx, _pointer| {
        up_binding.update(|input| input.drag_anchor = None);
    })
    .on_input(move |cx, value| {
        let value = single_line(value);
        if !value.is_empty() {
            input_binding.update(move |input| {
                normal::insert(&mut input.editor(), &value);
                input.preedit.clear();
                input.preedit_cursor = None;
                reveal_cursor(input, viewport_w, style.text);
            });
            cx.stop_propagation();
        }
    })
    .on_change(move |_cx, value| {
        let value = single_line(value.unwrap_or_default());
        change_binding.update(move |input| {
            input.set_text(value);
            reveal_cursor(input, viewport_w, style.text);
        });
    })
    .on_composition_update(move |_cx, value, cursor| {
        let value = value.to_owned();
        let cursor = cursor.clone();
        composition_binding.update(move |input| {
            input.preedit = value;
            input.preedit_cursor = cursor;
        });
    })
    .on_composition_end(move |_cx| {
        composition_end_binding.update(|input| {
            input.preedit.clear();
            input.preedit_cursor = None;
        });
    })
    .on_key_down(move |cx, event| {
        if !key_binding.get().preedit.is_empty() {
            return;
        }
        let Some(command) = commands::command_for(event, 1) else {
            if (event.modifiers.ctrl() || event.modifiers.meta()) && !event.modifiers.alt() {
                cx.prevent_default();
            }
            return;
        };
        if command == Command::Enter {
            if let Some(on_submit) = &on_submit {
                on_submit();
            }
            cx.prevent_default();
            cx.stop_propagation();
            return;
        }
        if command == Command::Escape
            && let Some(on_cancel) = &on_cancel
        {
            on_cancel();
            cx.prevent_default();
            cx.stop_propagation();
            return;
        }
        if matches!(command, Command::Indent(_)) {
            return;
        }
        let clipboard = cx.application().clipboard();
        key_binding.update(move |input| {
            apply_command(input, command, clipboard.as_ref());
            reveal_cursor(input, viewport_w, style.text);
        });
        cx.prevent_default();
        cx.stop_propagation();
    })
    .on_focus(move |_cx| focus_binding.update(|input| input.focused = true))
    .on_blur(move |_cx| {
        blur_binding.update(|input| {
            input.focused = false;
            input.drag_anchor = None;
            input.preedit.clear();
            input.preedit_cursor = None;
            input.editor().break_undo_group();
        });
        if let Some(on_blur) = &on_blur {
            on_blur();
        }
    });

    let mut content_group = group(text_bounds);
    if let Some(selection) = snapshot.selection.range() {
        let selection = shown_offset(selection.start)..shown_offset(selection.end);
        for selection in measured.selection_rects(selection) {
            content_group = content_group.child(panel(
                UiRect::new(
                    content.left + selection.left,
                    rect.top + selection.top,
                    content.left + selection.right,
                    rect.top + selection.bottom,
                ),
                VisualStyle::filled(style.selection).alpha(70),
            ));
        }
    }
    content_group = content_group.child(
        if snapshot.text().is_empty() && snapshot.preedit.is_empty() {
            SingleLineText::new(placeholder, text_bounds, style.placeholder).element()
        } else {
            SingleLineText::new(shown.as_str(), text_bounds, style.text).element()
        },
    );
    // A secret typed through an input method is not shown while it is composed.
    if !snapshot.preedit.is_empty() && !masked {
        let preedit_left = content.left + measured.caret_offset(snapshot.selection.cursor);
        let preedit_bounds = UiRect::new(
            preedit_left,
            text_bounds.top,
            text_bounds.right,
            text_bounds.bottom,
        );
        let preedit = SingleLineText::new(snapshot.preedit.clone(), preedit_bounds, style.text);
        let underline_right = preedit_left + preedit.width().max(2.0);
        content_group = content_group.child(preedit.element()).child(panel(
            UiRect::new(
                preedit_left,
                rect.bottom - 8.0,
                underline_right,
                rect.bottom - 7.0,
            ),
            VisualStyle::filled(style.caret),
        ));
    }
    root = root.child(clip(content, -scroll, 0.0).child(content_group));
    if snapshot.focused {
        root = root.child(panel(caret_rect, VisualStyle::filled(style.caret)));
    }
    root
}

fn apply_command(
    input: &mut InputState,
    command: Command,
    clipboard: &dyn lgui::services::Clipboard,
) {
    match command {
        Command::Copy | Command::Cut => {
            let mut editor = input.editor();
            if let Some(value) = editor.selected_text() {
                if clipboard.write_text(value).is_ok() && command == Command::Cut {
                    normal::backspace(&mut editor);
                }
                editor.break_undo_group();
            }
        }
        Command::Paste => {
            if let Ok(Some(value)) = clipboard.read_text() {
                normal::insert(&mut input.editor(), &single_line(&value));
                input.editor().break_undo_group();
            }
        }
        Command::Move(Movement::Up | Movement::PageUp(_), extend) => {
            normal::navigate(&mut input.editor(), Movement::Home, extend);
        }
        Command::Move(Movement::Down | Movement::PageDown(_), extend) => {
            normal::navigate(&mut input.editor(), Movement::End, extend);
        }
        _ => commands::apply(&mut input.editor(), command),
    }
}

fn visible_scroll(input: &InputState, measured: &SingleLineText, viewport_w: f32) -> f32 {
    scroll_for_position(
        input.scroll_x,
        measured.width(),
        measured.caret_offset(input.selection.cursor),
        viewport_w,
    )
}

fn scroll_for_position(stored: f32, content_width: f32, caret: f32, viewport_w: f32) -> f32 {
    let max_scroll = (content_width - viewport_w).max(0.0);
    let mut scroll = stored.clamp(0.0, max_scroll);
    if caret < scroll + CURSOR_MARGIN {
        scroll = (caret - CURSOR_MARGIN).max(0.0);
    } else if caret > scroll + viewport_w - CURSOR_MARGIN {
        scroll = caret - viewport_w + CURSOR_MARGIN;
    }
    scroll.clamp(0.0, max_scroll)
}

fn reveal_cursor(input: &mut InputState, viewport_w: f32, style: TextStyle) {
    let bounds = UiRect::new(0.0, 0.0, 100_000.0, style.height + 12.0);
    let measured = SingleLineText::new(input.text(), bounds, style);
    input.scroll_x = visible_scroll(input, &measured, viewport_w);
}

fn single_line(value: &str) -> String {
    value.replace(['\r', '\n'], "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use lgui::application::{AppView, ApplicationContext};
    use lgui::core::{
        ImeEvent, InputEvent, Point, PointerButton, PointerData, UiScale, dispatch_runtime_output,
    };
    use lgui::session::UiSession;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Harness {
        input: InputState,
    }

    fn read(state: &Harness) -> &InputState {
        &state.input
    }

    fn write(state: &mut Harness) -> &mut InputState {
        &mut state.input
    }

    #[test]
    fn portable_control_maps_native_unicode_hits_and_preedit_to_byte_state() {
        let exposed = Arc::new(Mutex::new(None::<State<Harness>>));
        let observed = exposed.clone();
        let view: AppView = Arc::new(move |cx| {
            let state = cx.state_with(|| Harness {
                input: InputState::new("a👩‍💻中"),
            });
            *observed.lock().unwrap() = Some(state.clone());
            let id = cx.use_stable_id();
            let focus = cx.focus_handle(id.clone());
            render_portable(
                cx.viewport(),
                id,
                focus,
                InputBinding::new(state, read, write),
                AccessInputOptions {
                    label: "Portable text",
                    placeholder: "",
                    style: InputStyle {
                        text: TextStyle::new(Color::WHITE, 14.0, 400),
                        placeholder: TextStyle::new(Color::WHITE, 14.0, 400),
                        caret: Color::WHITE,
                        selection: Color::WHITE,
                    },
                    on_submit: None,
                    on_cancel: None,
                    on_blur: None,
                },
                false,
            )
        });
        let application = ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &application,
            lgui::render_skia::skia_text_system_handle(),
        );
        let viewport = UiRect::new(0.0, 0.0, 360.0, 36.0);
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let text = session
            .tree()
            .nodes()
            .iter()
            .find(|n| n.text.as_deref() == Some("a👩‍💻中"))
            .unwrap();
        let layout = lgui::text::layout(&lgui::text::TextLayoutRequest::for_style(
            "a👩‍💻中",
            text.layout_rect,
            &text.text_style.unwrap(),
        ))
        .unwrap();
        let caret = layout.caret_rect(4).unwrap();
        send(
            &mut session,
            &application,
            &view,
            viewport,
            InputEvent::PointerDown {
                pointer: PointerData::mouse(Point::new(
                    caret.left + 0.1,
                    (caret.top + caret.bottom) / 2.0,
                )),
                button: PointerButton::Left,
            },
        );
        let state = exposed.lock().unwrap().clone().unwrap();
        assert_eq!(state.get().input.selection.cursor, "a👩‍💻".len());
        send(
            &mut session,
            &application,
            &view,
            viewport,
            InputEvent::Ime(ImeEvent::Preedit {
                text: "文".into(),
                cursor: Some(0..3),
            }),
        );
        assert_eq!(state.get().input.text(), "a👩‍💻中");
        assert!(
            session
                .tree()
                .nodes()
                .iter()
                .any(|n| n.text.as_deref() == Some("a👩‍💻文中"))
        );
        send(
            &mut session,
            &application,
            &view,
            viewport,
            InputEvent::TextInput("文".into()),
        );
        assert_eq!(state.get().input.text(), "a👩‍💻文中");
        assert_eq!(state.get().input.selection.cursor, "a👩‍💻文".len());
        assert!(state.get().input.preedit_cursor.is_none());
    }

    fn send(
        session: &mut UiSession,
        application: &ApplicationContext,
        view: &AppView,
        viewport: UiRect,
        input: InputEvent,
    ) {
        let events = session.handle_input(input);
        dispatch_runtime_output(
            events,
            application,
            &lgui::window::WindowId::new("single-line-input-test"),
            |action| session.handle_default_action(action),
            |_| {},
        );
        session.render_view(view, viewport, UiScale::ONE);
    }

    #[test]
    fn commit_input_survives_minimized_width_and_restores_its_text_and_caret() {
        let exposed = Arc::new(Mutex::new(None::<State<Harness>>));
        let output = exposed.clone();
        let view: AppView = Arc::new(move |cx| {
            let state = cx.state_with(|| {
                let mut input = InputState::new("commit message");
                input.focused = true;
                input.selection.anchor = Some(0);
                input.preedit = "你好".to_owned();
                input.preedit_cursor = Some("你好".len().."你好".len());
                Harness { input }
            });
            *output.lock().unwrap() = Some(state.clone());
            let id = cx.use_stable_id();
            let focus = cx.focus_handle(id.clone());
            // Match the Git panel's commit input, including its fixed margins.
            let viewport = cx.viewport();
            render(
                UiRect::new(6.0, 0.0, viewport.right - 38.0, 38.0),
                id,
                focus,
                InputBinding::new(state, read, write),
                InputOptions {
                    label: "Commit message",
                    placeholder: "Commit message",
                    style: InputStyle {
                        text: TextStyle::new(Color::WHITE, 10.0, 400),
                        placeholder: TextStyle::new(Color::WHITE, 10.0, 400),
                        caret: Color::WHITE,
                        selection: Color::WHITE,
                    },
                    on_submit: None,
                    on_cancel: None,
                    on_blur: None,
                },
            )
        });
        let mut session = UiSession::new();
        let application = ApplicationContext::empty(Default::default());
        let _text = lgui::backend::install_text_environment(
            &application,
            lgui::render_skia::skia_text_system_handle(),
        );
        for width in [240.0, 55.0, 44.0, 36.0, 0.0, 240.0] {
            session.invalidate_all();
            session.render_view(&view, UiRect::new(0.0, 0.0, width, 38.0), UiScale::ONE);
            let caret = session
                .tree()
                .nodes()
                .iter()
                .find_map(|node| node.ime_cursor_rect)
                .unwrap();
            assert!(caret.left.is_finite(), "width={width}, caret={caret:?}");
            assert!(caret.left >= 6.0 && caret.left <= (width - 38.0).max(6.0));
            let state = exposed.lock().unwrap().clone().unwrap().get();
            assert_eq!(state.input.text(), "commit message");
            assert_eq!(state.input.selection.anchor, Some(0));
            assert_eq!(state.input.selection.cursor, "commit message".len());
            assert_eq!(state.input.preedit, "你好");
        }
    }

    #[test]
    fn committed_ascii_and_ime_text_leave_the_caret_at_the_end() {
        let exposed = Arc::new(Mutex::new(None::<State<Harness>>));
        let output = exposed.clone();
        let viewport = UiRect::new(0.0, 0.0, 240.0, 38.0);
        let view: AppView = Arc::new(move |cx| {
            let state = cx.state(Harness::default());
            *output.lock().unwrap() = Some(state.clone());
            let id = cx.use_stable_id();
            let focus = cx.focus_handle(id.clone());
            render(
                viewport,
                id,
                focus,
                InputBinding::new(state, read, write),
                InputOptions {
                    label: "Test input",
                    placeholder: "Test input",
                    style: InputStyle {
                        text: TextStyle::new(Color::WHITE, 10.0, 400),
                        placeholder: TextStyle::new(Color::WHITE, 10.0, 400),
                        caret: Color::WHITE,
                        selection: Color::WHITE,
                    },
                    on_submit: None,
                    on_cancel: None,
                    on_blur: None,
                },
            )
        });
        let mut session = UiSession::new();
        let application = ApplicationContext::empty(Default::default());
        let _text = lgui::backend::install_text_environment(
            &application,
            lgui::render_skia::skia_text_system_handle(),
        );
        session.render_view(&view, viewport, UiScale::ONE);
        let state = exposed.lock().unwrap().clone().unwrap();

        send(
            &mut session,
            &application,
            &view,
            viewport,
            InputEvent::PointerDown {
                pointer: PointerData::mouse(Point::new(12.0, 19.0)),
                button: PointerButton::Left,
            },
        );
        send(
            &mut session,
            &application,
            &view,
            viewport,
            InputEvent::TextInput("abc".to_owned()),
        );
        assert_eq!(state.get().input.text(), "abc");
        assert_eq!(state.get().input.selection.cursor, 3);
        let rendered_text = session
            .tree()
            .nodes()
            .iter()
            .filter_map(|node| node.text.as_deref())
            .collect::<Vec<_>>();
        assert!(
            rendered_text.contains(&"abc"),
            "rendered text: {rendered_text:?}"
        );
        let ascii_caret = session
            .tree()
            .nodes()
            .iter()
            .find_map(|node| node.ime_cursor_rect)
            .unwrap();
        assert!(
            ascii_caret.left > TEXT_INSET,
            "ASCII caret did not advance: {ascii_caret:?}"
        );
        assert_eq!(ascii_caret.height(), 14.0);

        send(
            &mut session,
            &application,
            &view,
            viewport,
            InputEvent::Ime(ImeEvent::Preedit {
                text: "nihao".to_owned(),
                cursor: Some(5..5),
            }),
        );
        send(
            &mut session,
            &application,
            &view,
            viewport,
            InputEvent::Ime(ImeEvent::Commit("你好".to_owned())),
        );
        assert_eq!(state.get().input.text(), "abc你好");
        assert_eq!(state.get().input.selection.cursor, "abc你好".len());
        assert!(state.get().input.preedit.is_empty());
        let ime_caret = session
            .tree()
            .nodes()
            .iter()
            .find_map(|node| node.ime_cursor_rect)
            .unwrap();
        assert!(ime_caret.left > ascii_caret.left);
    }

    #[test]
    fn masked_offsets_count_characters_not_bytes() {
        let text = "aé漢b";
        for (index, _) in text.char_indices() {
            assert_eq!(unmasked_offset(text, masked_offset(text, index)), index);
        }
        assert_eq!(masked_offset(text, text.len()), 4 * MASK.len_utf8());
        assert_eq!(unmasked_offset(text, 99), text.len());
    }
}
