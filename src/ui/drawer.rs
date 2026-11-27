//! Right-hand drawer chrome: the icon helper used across drawer views and the
//! pieces shared by its collapsible sections (a title bar that folds the
//! section and a rule above it that moves the split).

use std::sync::Arc;

use lgui::core::{
    CursorIcon, EventPolicy, IconStyle, PointerButton, SemanticRole, Semantics, UiElement, UiId,
    precompiled,
};
use lgui::prelude::{Element, UiRect, VisualStyle, panel, text};

use crate::ui::theme;

/// A collapsible section under the file tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawerSection {
    Search,
    Outline,
}

/// Height of a section's bar, which is all that shows while it is collapsed.
pub const SECTION_H: f32 = theme::TABS_H;

/// `id` must be unique in the frame: elements sharing one replace each
/// other, so repeated rows need ids such as `UiId::owned(format!(..))`.
pub fn drawer_icon(
    id: impl Into<UiId>,
    key: &'static str,
    rect: UiRect,
    color: lgui::prelude::Color,
) -> Element {
    precompiled(UiElement::icon(id.into(), rect, key).icon_style(IconStyle::new(color)))
}

/// The bar of a collapsible section: a chevron and title that fold it, and an
/// optional figure after the title. `reserved` is the width kept free on the
/// right for the section's own buttons, which the caller adds as children; they
/// sit beside the fold target, so pressing one never folds the section.
/// `key` names the section's elements, as in `"{key}-section"`.
pub fn section_bar(
    rect: UiRect,
    key: &'static str,
    title: &'static str,
    open: bool,
    figure: Option<String>,
    reserved: f32,
    toggle: Arc<dyn Fn() + Send + Sync>,
) -> Element {
    let target = UiRect::new(rect.left, rect.top, rect.right - reserved, rect.bottom);
    let title_left = rect.left + 25.0;
    let mut target_element = panel(target, VisualStyle::default())
        .key(format!("{key}-section"))
        .semantics({
            let mut semantics = Semantics::new(SemanticRole::Button).name(title);
            semantics.state.expanded = Some(open);
            semantics
        })
        .event_policy(EventPolicy::INTERACTIVE)
        .cursor(CursorIcon::Pointer)
        .on_click(move || toggle())
        .child(drawer_icon(
            UiId::owned(format!("{key}-section-chevron")),
            if open { "chevron-down" } else { "chevron-right" },
            theme::icon_rect(UiRect::new(
                rect.left + 9.0,
                rect.top + (rect.height() - 12.0) / 2.0,
                rect.left + 21.0,
                rect.top + (rect.height() + 12.0) / 2.0,
            )),
            theme::c().text_dim,
        ))
        .child(text(
            UiRect::new(
                title_left,
                rect.top,
                (target.right - 4.0).max(title_left),
                rect.bottom,
            ),
            title,
            theme::sans(theme::c().text_muted, 11.0),
        ));
    if let Some(figure) = figure {
        let title_width = crate::ui::components::text::SingleLineText::new(
            title,
            UiRect::new(0.0, 0.0, 1000.0, rect.height()),
            theme::sans(theme::c().text_muted, 11.0),
        )
        .width();
        let left = title_left + title_width + 6.0;
        if left + 8.0 < target.right {
            target_element = target_element.child(text(
                UiRect::new(left, rect.top, target.right - 4.0, rect.bottom),
                figure,
                theme::sans(theme::c().text_ghost, 10.0),
            ));
        }
    }
    panel(rect, VisualStyle::filled(theme::c().sidebar)).child(target_element)
}

/// What the divider above an open section reports while it is dragged.
#[derive(Clone)]
pub struct DividerEvents {
    pub start: Arc<dyn Fn() + Send + Sync>,
    /// The pointer's vertical position.
    pub drag: Arc<dyn Fn(f32) + Send + Sync>,
    pub end: Arc<dyn Fn() + Send + Sync>,
}

/// The rule along the top of `rect`. While the section is open it can be
/// dragged to move the split; a collapsed section only draws the rule.
pub fn section_divider(
    key: &'static str,
    rect: UiRect,
    active: bool,
    events: Option<DividerEvents>,
) -> Element {
    let line = UiRect::new(rect.left, rect.top, rect.right, rect.top + 1.0);
    let Some(events) = events else {
        return panel(line, VisualStyle::filled(theme::c().border));
    };
    let (start, drag, end) = (events.start, events.drag, events.end);
    crate::ui::components::resize_handle::render(
        format!("{key}-split-handle"),
        UiRect::new(rect.left, rect.top - 3.0, rect.right, rect.top + 4.0),
        line,
        CursorIcon::ResizeVertical,
        active,
    )
    .on_pointer_down_with_button(move |_cx, _p, button| {
        if button == PointerButton::Left {
            start();
        }
    })
    .on_pointer_drag(move |_cx, p| drag(p.point.y))
    .on_pointer_up(move |_cx, _p| end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lgui::application::ApplicationContext;
    use lgui::core::{
        InputEvent, Point, PointerButton, PointerData, UiScale, dispatch_runtime_output,
    };
    use lgui::session::UiSession;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn clicking_the_title_folds_but_clicking_a_button_beside_it_does_not() {
        let viewport = UiRect::new(0.0, 0.0, 300.0, 100.0);
        let folds = Arc::new(AtomicUsize::new(0));
        let counter = folds.clone();
        let view: lgui::application::AppView = Arc::new(move |_| {
            let counter = counter.clone();
            section_bar(
                UiRect::new(0.0, 0.0, 300.0, SECTION_H),
                "test",
                "Test",
                true,
                Some("12".to_owned()),
                40.0,
                Arc::new(move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                }),
            )
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let context = ApplicationContext::empty(Default::default());
        let mut click = |x: f32| {
            for input in [
                InputEvent::PointerDown {
                    pointer: PointerData::mouse(Point::new(x, 14.0)),
                    button: PointerButton::Left,
                },
                InputEvent::PointerUp {
                    pointer: PointerData::mouse(Point::new(x, 14.0)),
                    button: PointerButton::Left,
                },
            ] {
                dispatch_runtime_output(
                    session.handle_input(input),
                    &context,
                    &lgui::window::WindowId::new("drawer-test"),
                    |action| session.handle_default_action(action),
                    |_| {},
                );
            }
        };
        click(120.0);
        assert_eq!(folds.load(Ordering::SeqCst), 1);
        click(290.0);
        assert_eq!(folds.load(Ordering::SeqCst), 1, "the reserved strip is free");
    }
}
