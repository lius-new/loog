//! Search field shared by workspace search and the extensions page.

use std::sync::Arc;

use lgui::core::{UiFocusHandle, UiId};
use lgui::prelude::{Element, UiRect};

use super::input::{self, AccessInputOptions, InputBinding, InputStyle};
use crate::ui::theme;

pub const HEIGHT: f32 = 24.0;

pub struct Options {
    pub label: &'static str,
    /// Space reserved for controls at the right of the field.
    pub trailing_width: f32,
    pub invalid: bool,
    pub on_submit: Option<Arc<dyn Fn() + Send + Sync>>,
    pub on_cancel: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl Options {
    pub fn new(label: &'static str) -> Self {
        Self {
            label,
            trailing_width: 4.0,
            invalid: false,
            on_submit: None,
            on_cancel: None,
        }
    }
}

pub fn render<T: Clone + Send + 'static>(
    rect: UiRect,
    id: UiId,
    focus: UiFocusHandle,
    binding: InputBinding<T>,
    options: Options,
) -> Element {
    let c = theme::c();
    let focused = binding.get().focused;
    let left = rect.left + 2.0;
    let inner = UiRect::new(
        left,
        rect.top + 1.0,
        (rect.right - options.trailing_width).max(left),
        rect.bottom - 1.0,
    );
    theme::bordered(
        rect,
        c.surface,
        if options.invalid {
            c.error
        } else if focused {
            c.accent
        } else {
            c.border
        },
        4.0,
        1.0,
    )
    .child(input::render_access(
        inner,
        id,
        focus,
        binding,
        AccessInputOptions {
            label: options.label,
            placeholder: "Search",
            style: InputStyle {
                text: theme::mono(c.text, theme::SMALL),
                placeholder: theme::mono(c.text_muted, theme::SMALL),
                caret: c.accent,
                selection: c.accent,
            },
            on_submit: options.on_submit,
            on_cancel: options.on_cancel,
            on_blur: None,
        },
    ))
}
