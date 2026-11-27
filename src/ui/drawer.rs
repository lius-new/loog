//! Right-hand drawer chrome shared by Explorer and Search: the page switcher
//! header and the icon helper used across drawer views.

use std::sync::Arc;

use lgui::core::{
    CursorIcon, EventPolicy, IconStyle, SemanticRole, Semantics, UiElement, UiId, precompiled,
};
use lgui::prelude::{Element, UiRect, VisualStyle, panel};

use crate::ui::theme;

/// Page shown in the right-hand drawer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DrawerMode {
    #[default]
    Files,
    Search,
}

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

/// Header icon button: filled while selected, otherwise transparent until
/// hovered or pressed.
fn hover_button(rect: UiRect, key: impl Into<String>, selected: bool) -> Element {
    Element::new(move |cx| {
        let flags = cx.context.interaction_flags(&cx.id);
        let style = if selected || flags.hovered || flags.pressed {
            VisualStyle::filled(theme::c().surface).radius(3.0)
        } else {
            VisualStyle::default()
        };
        UiElement::panel(cx.id, rect, style).children(cx.children)
    })
    .key(key.into())
}

/// Header with the Explorer/Search page buttons.
pub fn header(
    rect: UiRect,
    mode: DrawerMode,
    select: Arc<dyn Fn(DrawerMode) + Send + Sync>,
) -> Element {
    let mut header = panel(rect, VisualStyle::filled(theme::c().sidebar));
    for (index, label, icon, target) in [
        (0, "Explorer", "explorer", DrawerMode::Files),
        (1, "Search", "search", DrawerMode::Search),
    ] {
        let button_width = ((rect.width() - 44.0).max(0.0) / 2.0).min(28.0);
        let left = rect.left + 8.0 + index as f32 * (button_width + 4.0);
        let hit = UiRect::new(
            left,
            rect.top + 2.0,
            left + button_width,
            (rect.bottom - 2.0).max(rect.top + 2.0),
        );
        let active = mode == target;
        let select = select.clone();
        let button = hover_button(hit, format!("drawer-mode-{index}"), active)
            .event_policy(EventPolicy {
                hover: true,
                press: true,
                focus: false,
            })
            .semantics(Semantics::new(SemanticRole::Button).name(label))
            .cursor(CursorIcon::Pointer)
            .on_click(move || select(target))
            .child(drawer_icon(
                UiId::owned(format!("drawer-mode-icon-{index}")),
                icon,
                theme::icon_rect(hit),
                if active {
                    theme::c().accent
                } else {
                    theme::c().text_dim
                },
            ));
        header = header.child(button);
    }

    header.child(crate::ui::components::divider::bottom(rect))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lgui::application::ApplicationContext;
    use lgui::core::{InputEvent, Point, PointerData, UiScale, dispatch_runtime_output};
    use lgui::session::UiSession;

    #[test]
    fn header_buttons_fill_when_selected_or_hovered() {
        let viewport = UiRect::new(0.0, 0.0, 400.0, 200.0);
        let context = ApplicationContext::empty(Default::default());
        for mode in [DrawerMode::Files, DrawerMode::Search] {
            let view: lgui::application::AppView = Arc::new(move |_| {
                header(
                    UiRect::new(0.0, 0.0, 300.0, theme::TABS_H),
                    mode,
                    Arc::new(|_| {}),
                )
            });
            let mut session = UiSession::new();
            session.render_view(&view, viewport, UiScale::ONE);
            for (name, selected) in [
                ("Explorer", mode == DrawerMode::Files),
                ("Search", mode == DrawerMode::Search),
            ] {
                let find = |session: &UiSession| {
                    session
                        .tree()
                        .nodes()
                        .iter()
                        .find(|node| {
                            node.semantics.as_ref().and_then(|s| s.name.as_deref()) == Some(name)
                        })
                        .cloned()
                        .unwrap()
                };
                // Repeat to catch hover that only works the first few times.
                for _ in 0..3 {
                    for (point, hovered) in [
                        {
                            let hit = find(&session).hit_rect;
                            (
                                Point::new(
                                    (hit.left + hit.right) / 2.0,
                                    (hit.top + hit.bottom) / 2.0,
                                ),
                                true,
                            )
                        },
                        (Point::new(350.0, 150.0), false),
                    ] {
                        dispatch_runtime_output(
                            session
                                .handle_input(InputEvent::PointerMove(PointerData::mouse(point))),
                            &context,
                            &lgui::window::WindowId::new("drawer-test"),
                            |action| session.handle_default_action(action),
                            |_| {},
                        );
                        session.render_view(&view, viewport, UiScale::ONE);
                        assert_eq!(
                            find(&session).style.fill.is_some(),
                            hovered || selected,
                            "{name} in {mode:?}"
                        );
                    }
                }
            }
        }
    }
}
