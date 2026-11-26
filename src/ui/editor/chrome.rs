//! What every card over the code shares: its surface and the bar of key hints
//! and buttons under it. The completion list and the hover card both build on
//! these, so a change to the look is made once.
use crate::ui::theme;
use lgui::core::{CursorIcon, EventPolicy, SemanticRole, Semantics, UiEventContext};
use lgui::prelude::{
    Element, TextAlign, UiRect, VisualStyle, panel, text,
};

/// Corner radius of a card over the code.
pub(crate) const RADIUS: f32 = 8.0;
/// Height of the bar under a card, with its rule.
pub(crate) const FOOTER_H: f32 = 28.0;

/// Width of `label` in the small proportional face the bar uses.
pub(crate) fn measure(label: &str) -> f32 {
    crate::ui::tabs::measure_sans(label, theme::SMALL, 400)
}

/// The border and fill of a card.
pub(crate) fn surface(card: UiRect) -> Element {
    theme::bordered(card, theme::c().surface, theme::c().border, RADIUS, 1.0)
}

/// The bar under a card: a rule, then items from the left and an optional note
/// on the right. Nothing is filled, so the card's rounded corners stay intact.
pub(crate) struct Footer {
    bar: UiRect,
    x: f32,
    items: Vec<Element>,
}

impl Footer {
    pub fn new(card: UiRect) -> Self {
        let bar = UiRect::new(
            card.left + 1.0,
            card.bottom - 1.0 - FOOTER_H,
            card.right - 1.0,
            card.bottom - 1.0,
        );
        let rule = panel(
            UiRect::new(bar.left, bar.top, bar.right, bar.top + 1.0),
            VisualStyle::filled(theme::c().border),
        );
        Self {
            bar,
            x: bar.left + 10.0,
            items: vec![rule],
        }
    }

    fn row(&self, h: f32) -> (f32, f32) {
        (self.bar.top + 1.0 + (FOOTER_H - 1.0 - h) / 2.0, h)
    }

    fn push(&mut self, item: Element) {
        self.items.push(item);
    }

    /// A key shown as a cap, such as `Tab`.
    pub fn cap(mut self, label: &str) -> Self {
        let (top, h) = self.row(16.0);
        let r = UiRect::new(
            self.x,
            top,
            self.x + (measure(label) + 10.0).max(16.0),
            top + h,
        );
        let mut style = theme::sans(theme::c().text_muted, theme::SMALL);
        style.align = TextAlign::Center;
        self.x = r.right + 5.0;
        self.push(
            theme::bordered(r, theme::c().bg, theme::c().border, 4.0, 1.0).child(text(
                r,
                label.to_owned(),
                style,
            )),
        );
        self
    }

    /// A key shown as an icon on a cap, such as an arrow.
    pub fn icon_cap(mut self, name: &'static str) -> Self {
        let (top, h) = self.row(16.0);
        let r = UiRect::new(self.x, top, self.x + 18.0, top + h);
        let glyph = UiRect::new(r.left + 4.0, r.top + 3.0, r.right - 4.0, r.bottom - 3.0);
        self.x = r.right + 5.0;
        self.push(
            theme::bordered(r, theme::c().bg, theme::c().border, 4.0, 1.0).child(
                crate::ui::tabs::icon(
                    lgui::core::UiId::owned(format!("card-footer.key.{name}")),
                    name,
                    glyph,
                    theme::c().text_muted,
                ),
            ),
        );
        self
    }

    /// What the keys before it do.
    pub fn word(mut self, label: &str) -> Self {
        let r = UiRect::new(
            self.x,
            self.bar.top + 1.0,
            self.x + measure(label) + 2.0,
            self.bar.bottom,
        );
        self.x += measure(label) + 14.0;
        self.push(text(
            r,
            label.to_owned(),
            theme::sans(theme::c().text_faint, theme::SMALL),
        ).into());
        self
    }

    /// A button: its label, then the key that does the same, if any.
    pub fn button(
        mut self,
        label: &str,
        key: Option<&str>,
        on_click: impl Fn(&mut UiEventContext) + Send + Sync + 'static,
    ) -> Self {
        let key_w = key.map_or(0.0, |key| (measure(key) + 10.0).max(16.0) + 5.0);
        let w = measure(label) + key_w + 16.0;
        let r = UiRect::new(
            self.x,
            self.bar.top + 3.0,
            self.x + w,
            self.bar.bottom - 3.0,
        );
        self.x = r.right + 4.0;
        let mut button = panel(r, VisualStyle::default().radius(5.0))
            .event_policy(EventPolicy {
                focus: false,
                ..EventPolicy::INTERACTIVE
            })
            .cursor(CursorIcon::Pointer)
            .semantics(Semantics::new(SemanticRole::Button).name(label.to_owned()))
            .on_pointer_down_with_button(|cx, _, _| cx.stop_propagation())
            .on_click(move |cx: &mut UiEventContext| {
                cx.stop_propagation();
                on_click(cx);
            })
            .child(text(
                UiRect::new(r.left + 8.0, r.top, r.right, r.bottom),
                label.to_owned(),
                theme::sans(theme::c().text_muted, theme::SMALL),
            ));
        if let Some(key) = key {
            let h = 16.0;
            let top = r.top + (r.height() - h) / 2.0;
            let left = r.left + 8.0 + measure(label) + 6.0;
            let cap = UiRect::new(left, top, r.right - 6.0, top + h);
            let mut style = theme::sans(theme::c().text_faint, theme::SMALL);
            style.align = TextAlign::Center;
            button = button.child(
                theme::bordered(cap, theme::c().bg, theme::c().border, 4.0, 1.0).child(text(
                    cap,
                    key.to_owned(),
                    style,
                )),
            );
        }
        self.push(button);
        self
    }

    /// The width the items so far need, for sizing the card.
    pub fn used(&self) -> f32 {
        self.x - self.bar.left
    }

    /// A note at the right edge, such as a count or the servers' names.
    pub fn note(mut self, label: &str) -> Self {
        let mut style = theme::sans(theme::c().text_faint, theme::SMALL);
        style.align = TextAlign::Right;
        self.push(text(
            UiRect::new(
                self.x,
                self.bar.top + 1.0,
                self.bar.right - 10.0,
                self.bar.bottom,
            ),
            label.to_owned(),
            style,
        ).into());
        self
    }

    /// Puts the bar on `card`.
    pub fn attach(self, mut card: Element) -> Element {
        for item in self.items {
            card = card.child(item);
        }
        card
    }
}
