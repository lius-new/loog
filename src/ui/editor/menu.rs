//! Context menu shares the same commands and clipboard path as keyboard editing.
use crate::ui::theme;
use lgui::core::UiFocusHandle;
use lgui::core::{CursorIcon, EventPolicy};
use lgui::prelude::{Element, TextAlign, UiRect, VisualStyle, panel, text};
use lgui::services::ServicesContextExt;
use loog_editor_core::commands::Command;
use std::sync::Arc;

pub(crate) struct Item {
    pub label: &'static str,
    pub command: Command,
    pub shortcut: String,
    pub enabled: bool,
}
pub(crate) enum Event {
    Close,
    Hover(usize),
    Execute(Command, lgui::services::ClipboardHandle),
}
pub(crate) type Commands = Arc<dyn Fn(Event) + Send + Sync>;

pub fn render(
    viewport: UiRect,
    pos: (f32, f32),
    items: Vec<Item>,
    hovered: Option<usize>,
    commands: Commands,
    focus: UiFocusHandle,
) -> Element {
    let width = 230.0;
    let height = items.len() as f32 * 28.0 + 8.0;
    let x = pos
        .0
        .clamp(viewport.left, (viewport.right - width).max(viewport.left));
    let y = pos
        .1
        .clamp(viewport.top, (viewport.bottom - height).max(viewport.top));
    let card = UiRect::new(x, y, x + width, y + height);
    let close = commands.clone();
    let close_focus = focus.clone();
    // Menu surfaces do not take focus away from the editor, so Escape still works.
    let policy = EventPolicy {
        focus: false,
        ..EventPolicy::INTERACTIVE
    };
    let overlay = panel(viewport, VisualStyle::default())
        .event_policy(policy)
        .on_pointer_down_with_button(move |cx, p, _| {
            if !card.contains(p.point) {
                close(Event::Close);
                close_focus.focus();
            }
            cx.stop_propagation();
        });
    let mut menu =
        theme::bordered(card, theme::c().surface, theme::c().border, 5.0, 1.0).event_policy(policy);
    for (
        i,
        Item {
            label,
            command,
            shortcut,
            enabled,
        },
    ) in items.into_iter().enumerate()
    {
        let r = UiRect::new(
            x + 4.0,
            y + 4.0 + i as f32 * 28.0,
            x + width - 4.0,
            y + 4.0 + (i + 1) as f32 * 28.0,
        );
        let color = if enabled {
            theme::c().text_soft
        } else {
            theme::c().text_dim
        };
        let st = commands.clone();
        let hover = commands.clone();
        let target = focus.clone();
        let row = panel(
            r,
            if enabled && hovered == Some(i) {
                VisualStyle::filled(theme::c().active_line)
            } else {
                VisualStyle::default()
            },
        )
        .event_policy(policy)
        .cursor(if enabled {
            CursorIcon::Pointer
        } else {
            CursorIcon::Default
        })
        .on_pointer_move(move |_, _| {
            hover(Event::Hover(i));
        })
        .on_click(move |cx: &mut lgui::core::UiEventContext| {
            if enabled {
                st(Event::Execute(command, cx.application().clipboard()));
                target.focus();
            }
            cx.stop_propagation();
        })
        .child(text(
            UiRect::new(r.left + 8.0, r.top, r.left + 100.0, r.bottom),
            label,
            theme::sans(color, theme::UI_SIZE),
        ))
        .child(text(
            UiRect::new(r.right - 110.0, r.top, r.right - 8.0, r.bottom),
            shortcut,
            {
                let mut style = theme::mono(color, theme::SMALL);
                style.align = TextAlign::Right;
                style
            },
        ));
        menu = menu.child(row);
    }
    overlay.child(menu)
}
