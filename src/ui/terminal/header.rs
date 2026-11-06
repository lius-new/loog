//! Right-hand tools of the terminal header, laid out right to left in three
//! groups: actions on the sessions, tools of the terminal itself, and actions
//! of the whole panel. A thin rule separates the groups.

use lgui::core::{CursorIcon, EventPolicy, SemanticRole, Semantics, UiElement, UiId, UiScale};
use lgui::prelude::{Color, Element, TextStyle, UiRect, VisualStyle, panel};
use lgui::text::{self, TextLayoutRequest};

use crate::ui::theme;

/// Distances of the right-hand tools. A wide header spreads them out; a narrow
/// one keeps them close so the session tabs still have room.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Metrics {
    /// Edge of a square tool button.
    button: f32,
    /// Space between neighbouring buttons.
    gap: f32,
    /// Space between the last button and the header's right edge.
    inset: f32,
    /// Space on each side of a group rule.
    rule_gap: f32,
}
const COMPACT: Metrics = Metrics {
    button: 22.0,
    gap: 4.0,
    inset: 8.0,
    rule_gap: 9.0,
};
const ROOMY: Metrics = Metrics {
    button: 24.0,
    gap: 8.0,
    inset: 10.0,
    rule_gap: 14.0,
};
/// Narrowest header that uses the roomy distances.
const ROOMY_MIN_W: f32 = 560.0;
/// Narrower headers name only the open panel; the other tabs are icons.
const ICON_ONLY_BELOW_W: f32 = 840.0;
/// Space before the category switch on the header's left.
const INSET: f32 = 8.0;
const RULE_H: f32 = 14.0;

thread_local! {
    static SCOPE: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Marks the buttons built while it lives as belonging to one terminal region,
/// so two regions on screen never share a node id.
pub(super) struct Scope;

impl Scope {
    pub(super) fn enter(region: Option<u64>) -> Self {
        SCOPE.with(|scope| scope.set(region));
        Self
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        SCOPE.with(|scope| scope.set(None));
    }
}

fn scoped(id: &str) -> String {
    match SCOPE.with(std::cell::Cell::get) {
        Some(region) => format!("{id}-region-{region}"),
        None => id.to_owned(),
    }
}

fn metrics(header: UiRect) -> Metrics {
    if header.width() >= ROOMY_MIN_W {
        ROOMY
    } else {
        COMPACT
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Spec {
    /// A foreground program is running, so it can be interrupted.
    pub running: bool,
    /// The default bottom panel owns maximize and shows the collapse icon; a
    /// terminal docked in another region has neither.
    pub panel: bool,
}

#[derive(Debug)]
pub(super) struct Layout {
    pub stop: Option<UiRect>,
    pub new: UiRect,
    pub shell: UiRect,
    pub split: UiRect,
    pub search: UiRect,
    pub more: UiRect,
    pub maximize: Option<UiRect>,
    pub collapse: Option<UiRect>,
    /// Rules between groups, as their horizontal centers.
    pub rules: Vec<f32>,
    /// Left edge of the tools; the session strip must end before it.
    pub left: f32,
}

/// Walks leftwards from the header's right edge.
struct Cursor {
    x: f32,
    top: f32,
    m: Metrics,
}

impl Cursor {
    fn button(&mut self) -> UiRect {
        let b = self.m.button;
        let rect = UiRect::new(self.x - b, self.top, self.x, self.top + b);
        self.x -= b + self.m.gap;
        rect
    }

    /// Drops the gap left after the previous button and returns the centre of
    /// a rule placed there.
    fn rule(&mut self) -> f32 {
        let center = self.x + self.m.gap - self.m.rule_gap;
        self.x = center - self.m.rule_gap;
        center
    }
}

pub(super) fn layout(header: UiRect, spec: Spec) -> Layout {
    let m = metrics(header);
    let mut at = Cursor {
        x: header.right - m.inset,
        top: header.top + (header.height() - m.button) / 2.0,
        m,
    };
    let mut rules = Vec::new();
    let collapse = spec.panel.then(|| at.button());
    let maximize = spec.panel.then(|| at.button());
    if collapse.is_some() {
        rules.push(at.rule());
    }
    let more = at.button();
    let search = at.button();
    let split = at.button();
    rules.push(at.rule());
    let shell = at.button();
    let new = at.button();
    let stop = spec.running.then(|| at.button());
    Layout {
        stop,
        new,
        shell,
        split,
        search,
        more,
        maximize,
        collapse,
        rules,
        left: at.x + m.gap,
    }
}

/// Right end of a dock header that has no terminal tools: the panel's own
/// buttons, a rule, then `count` tool slots, all on the terminal header's
/// spacing so every tab of the dock looks alike.
#[derive(Debug)]
pub(crate) struct DockTools {
    pub collapse: UiRect,
    pub maximize: UiRect,
    /// Centre of the rule between the tools and the panel's buttons.
    pub rule: f32,
    /// Tool buttons, rightmost first.
    pub slots: Vec<UiRect>,
    /// Left edge of the tools; whatever sits to their left must end before it.
    pub left: f32,
}

pub(crate) fn dock_tools(header: UiRect, count: usize) -> DockTools {
    let m = metrics(header);
    let mut at = Cursor {
        x: header.right - m.inset,
        top: header.top + (header.height() - m.button) / 2.0,
        m,
    };
    let collapse = at.button();
    let maximize = at.button();
    let rule = at.rule();
    let slots = (0..count).map(|_| at.button()).collect();
    DockTools {
        collapse,
        maximize,
        rule,
        slots,
        left: at.x + m.gap,
    }
}

/// A compact segmented switch, distinct from the full-height session tabs.
pub(super) fn category(
    header: UiRect,
    sessions: usize,
    selected: super::state::PanelTab,
    badges: [super::state::Badge; 2],
    switch: std::sync::Arc<dyn Fn(super::state::PanelTab) + Send + Sync>,
) -> (Element, f32) {
    use super::state::{Badge, PanelTab};
    use crate::ui::components::text::SingleLineText;
    use crate::ui::tabs::measure;
    let compact = header.width() < 640.0;
    let size = if compact {
        theme::SMALL
    } else {
        theme::UI_SIZE
    };
    let count = sessions.to_string();
    let count_w = if compact {
        0.0
    } else {
        measure(&count, theme::SMALL, 400)
    };
    let label_width = |label| {
        SingleLineText::new(
            label,
            UiRect::new(0.0, 0.0, 10_000.0, size),
            theme::sans_semibold(theme::c().text, size),
        )
        .width()
    };
    // The Output and Problems badges, in tab order.
    let badge_of = |tab: PanelTab| match tab {
        PanelTab::Output => badges[0],
        PanelTab::Problems => badges[1],
        _ => Badge::None,
    };
    let badge_text = |badge: Badge| match badge {
        Badge::Count(n) => Some(if n > 99 {
            "99+".to_owned()
        } else {
            n.to_string()
        }),
        _ => None,
    };
    let badge_w = |badge: Badge| match badge {
        Badge::None => 0.0,
        Badge::Dot => 6.0 + 6.0,
        Badge::Count(_) => 6.0 + measure(&badge_text(badge).unwrap_or_default(), theme::SMALL, 400),
    };
    // A narrow header keeps only the open tab's name; the others are icons, so
    // four tabs leave the session tabs their room.
    let labelled = |tab: PanelTab| header.width() >= ICON_ONLY_BELOW_W || tab == selected;
    let slot_w = |tab: PanelTab| {
        let name = if labelled(tab) {
            label_width(tab.label()) + 6.0
        } else {
            0.0
        };
        let count = if tab == PanelTab::Terminal && !compact {
            count_w + 6.0
        } else {
            0.0
        };
        name + count + 14.0 + 12.0 + badge_w(badge_of(tab))
    };
    let widths = PanelTab::ALL.map(slot_w);
    let track_h = 22.0;
    let top = header.top + (header.height() - track_h) / 2.0;
    let track = UiRect::new(
        header.left + INSET,
        top,
        header.left + INSET + 4.0 + widths.iter().sum::<f32>() + 2.0 * widths.len() as f32,
        top + track_h,
    );
    let mut root = Element::new(move |cx| {
        category_frame(
            cx.id,
            track,
            theme::c().active_line,
            4.0,
            cx.context.scale(),
        )
        .children(cx.children)
    })
    .key("bottom-panel-categories");
    let mut left = track.left + 3.0;
    for (tab, width) in PanelTab::ALL.into_iter().zip(widths) {
        let (label, glyph, id) = (tab.label(), tab.glyph(), tab.node_id());
        let badge = badge_of(tab);
        let slot = UiRect::new(left, track.top + 2.0, left + width, track.bottom - 2.0);
        left = slot.right + 2.0;
        let active = tab == selected;
        let color = if active {
            theme::c().accent
        } else {
            theme::c().text_muted
        };
        let mut semantics = Semantics::new(SemanticRole::Button).name(label);
        semantics.state.selected = active;
        let click = switch.clone();
        let node_id = UiId::new(id);
        let mut button = Element::new(move |cx| {
            let flags = cx.context.interaction_flags(&node_id);
            let scale = cx.context.scale();
            let background_id = UiId::owned(format!("{id}-background"));
            let button = UiElement::panel(node_id, slot, VisualStyle::default());
            if active {
                // The selected segment stands out by its fill alone, without an outline.
                let painted = scale.logical_rect(scale.physical_rect(slot));
                return button
                    .child(UiElement::panel(
                        background_id,
                        painted,
                        VisualStyle::filled(theme::c().surface).radius(3.0),
                    ))
                    .children(cx.children);
            }
            let style = if flags.hovered {
                VisualStyle::filled(theme::c().surface)
                    .alpha(140)
                    .radius(2.0)
            } else {
                VisualStyle::filled(theme::c().active_line).radius(2.0)
            };
            let painted = scale.logical_rect(scale.physical_rect(slot));
            button
                .child(UiElement::panel(background_id, painted, style))
                .children(cx.children)
        })
        .key(id)
        .event_policy(EventPolicy::INTERACTIVE)
        .cursor(CursorIcon::Pointer)
        .semantics(semantics)
        .on_click(move || click(tab));
        let shows_label = labelled(tab);
        let label_w = if shows_label {
            label_width(label)
        } else {
            -6.0
        };
        let content_w = 14.0
            + 6.0
            + label_w
            + if tab == PanelTab::Terminal && !compact {
                6.0 + count_w
            } else {
                0.0
            }
            + badge_w(badge);
        let icon_left = slot.left + (slot.width() - content_w) / 2.0;
        let label_rect = UiRect::new(
            icon_left + 20.0,
            slot.top,
            icon_left + 20.0 + label_w + 2.0,
            slot.bottom,
        );
        // Every tab uses the selected one's weight, so choosing a tab changes
        // its colour and fill but never the width of its name.
        let label_style = theme::sans_semibold(color, size);
        button = button.child(super::view::icon(
            id,
            glyph,
            UiRect::new(
                icon_left,
                slot.top + 2.0,
                icon_left + 14.0,
                slot.bottom - 2.0,
            ),
            color,
        ));
        if shows_label {
            button = button.child(lgui::prelude::text(label_rect, label, label_style));
        }
        if tab == PanelTab::Terminal && !compact {
            let count_rect = UiRect::new(
                icon_left + 26.0 + label_w,
                slot.top,
                slot.right - 4.0,
                slot.bottom,
            );
            let count_style = theme::mono(theme::c().text_faint, theme::SMALL);
            let baseline_offset = category_baseline(label, label_rect, label_style)
                .zip(category_baseline(&count, count_rect, count_style))
                .map_or(0.0, |(label, count)| label - count);
            button = button.child(lgui::prelude::text(
                count_rect.translate(0.0, baseline_offset),
                count.clone(),
                count_style,
            ));
        }
        let badge_left = icon_left + 20.0 + label_w + 6.0;
        match badge {
            Badge::None => {}
            Badge::Dot => {
                let size = 6.0;
                let y = slot.top + (slot.height() - size) / 2.0;
                button = button.child(
                    lgui::prelude::panel(
                        UiRect::new(badge_left, y, badge_left + size, y + size),
                        VisualStyle::filled(theme::c().error).radius(size / 2.0),
                    )
                    .key(format!("{id}-badge")),
                );
            }
            Badge::Count(_) => {
                let value = badge_text(badge).unwrap_or_default();
                let rect = UiRect::new(
                    badge_left,
                    slot.top,
                    badge_left + badge_w(badge) - 6.0 + 2.0,
                    slot.bottom,
                );
                button = button.child(lgui::prelude::text(
                    rect,
                    value,
                    theme::mono(theme::c().error_text, theme::SMALL),
                ));
            }
        }
        root = root.child(button);
    }
    (root, track.right)
}

/// Smaller monospace counts share the label's baseline, rather than centring
/// each font's capital-height box independently.
fn category_baseline(value: &str, rect: UiRect, style: TextStyle) -> Option<f32> {
    text::layout(&TextLayoutRequest::for_style(value, rect, &style))?
        .lines()
        .first()
        .map(|line| line.baseline)
}

/// Keep both edges of the border on physical pixels, including when a 22px
/// control is centred in the header's odd-height content area.
fn category_frame(id: UiId, rect: UiRect, fill: Color, radius: f32, scale: UiScale) -> UiElement {
    let painted = scale.logical_rect(scale.physical_rect(rect));
    let inset = scale.logical_value(scale.physical_length(1.0));
    let inner = UiRect::new(
        painted.left + inset,
        painted.top + inset,
        painted.right - inset,
        painted.bottom - inset,
    );
    let fill_id = UiId::owned(format!("{}-fill", id.as_str()));
    UiElement::panel(
        id,
        painted,
        VisualStyle::filled(theme::c().border).radius(radius),
    )
    .hit_rect(rect)
    .child(UiElement::panel(
        fill_id,
        inner,
        VisualStyle::filled(fill).radius((radius - inset).max(0.0)),
    ))
}

/// Transparent until hovered or pressed, filled while `active`.
pub(crate) fn button(
    id: &'static str,
    rect: UiRect,
    icon: &'static str,
    name: impl Into<String>,
    active: bool,
    color: Color,
    click: impl Fn() + Send + Sync + 'static,
) -> Element {
    button_in(id, rect, rect, icon, name, active, color, click)
}

/// Like [`button`], with the icon centred in `slot` instead of in `rect`, for a
/// button whose box is wider on one side than its glyph's visual centre.
#[allow(clippy::too_many_arguments)]
pub(crate) fn button_in(
    id: &'static str,
    rect: UiRect,
    slot: UiRect,
    icon: &'static str,
    name: impl Into<String>,
    active: bool,
    color: Color,
    click: impl Fn() + Send + Sync + 'static,
) -> Element {
    button_styled(
        id,
        rect,
        slot,
        icon,
        name,
        active,
        color,
        (theme::c().surface, 255),
        click,
    )
}

/// Like [`button_in`], with the fill drawn while hovered or active given as a
/// colour and opacity, so a button on a tinted row can use a matching tint.
#[allow(clippy::too_many_arguments)]
pub(super) fn button_styled(
    id: &'static str,
    rect: UiRect,
    slot: UiRect,
    icon: &'static str,
    name: impl Into<String>,
    active: bool,
    color: Color,
    fill: (Color, u8),
    click: impl Fn() + Send + Sync + 'static,
) -> Element {
    let mut semantics = Semantics::new(SemanticRole::Button).name(name.into());
    semantics.state.selected = active;
    // An explicit node id, so a neighbour such as a highlighted menu row can ask
    // whether this button is hovered.
    let scoped_id = scoped(id);
    let node_id = UiId::owned(scoped_id.clone());
    Element::new(move |cx| {
        let flags = cx.context.interaction_flags(&node_id);
        let style = if active || flags.hovered || flags.pressed {
            VisualStyle::filled(fill.0).alpha(fill.1).radius(3.0)
        } else {
            VisualStyle::default()
        };
        UiElement::panel(node_id.clone(), rect, style).children(cx.children)
    })
    .key(scoped_id)
    .event_policy(EventPolicy::INTERACTIVE)
    .cursor(CursorIcon::Pointer)
    .semantics(semantics)
    .on_click(click)
    .child(super::view::icon(
        id,
        icon,
        theme::icon_rect(slot),
        if active { theme::c().accent } else { color },
    ))
}

pub(crate) fn rule(header: UiRect, center: f32) -> Element {
    let top = header.top + (header.height() - RULE_H) / 2.0;
    panel(
        UiRect::new(center - 0.5, top, center + 0.5, top + RULE_H),
        VisualStyle::filled(theme::c().border),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> UiRect {
        UiRect::new(0.0, 0.0, 600.0, 28.0)
    }

    #[test]
    fn groups_run_right_to_left_without_overlap() {
        let layout = layout(
            header(),
            Spec {
                running: true,
                panel: true,
            },
        );
        let order = [
            layout.collapse.unwrap(),
            layout.maximize.unwrap(),
            layout.more,
            layout.search,
            layout.split,
            layout.shell,
            layout.new,
            layout.stop.unwrap(),
        ];
        for pair in order.windows(2) {
            assert!(pair[1].right <= pair[0].left, "{pair:?}");
        }
        assert_eq!(layout.rules.len(), 2);
        // Each rule sits strictly between two adjacent groups.
        assert!(layout.maximize.unwrap().left > layout.rules[0]);
        assert!(layout.rules[0] > layout.more.right);
        assert!(layout.split.left > layout.rules[1]);
        assert!(layout.rules[1] > layout.shell.right);
        assert!(layout.left <= layout.stop.unwrap().left);
    }

    #[test]
    fn a_docked_terminal_has_no_panel_group_and_stop_only_while_running() {
        let layout = layout(
            header(),
            Spec {
                running: false,
                panel: false,
            },
        );
        assert!(layout.collapse.is_none() && layout.maximize.is_none());
        assert!(layout.stop.is_none());
        assert_eq!(layout.rules.len(), 1);
        assert_eq!(layout.more.right, header().right - metrics(header()).inset);
        assert!(layout.left <= layout.new.left);
    }

    #[test]
    fn buttons_are_centred_vertically_in_the_header() {
        let layout = layout(
            header(),
            Spec {
                running: false,
                panel: true,
            },
        );
        for rect in [layout.more, layout.new, layout.collapse.unwrap()] {
            assert_eq!(rect.top - header().top, header().bottom - rect.bottom);
        }
    }

    #[test]
    fn a_wide_header_spaces_the_tools_out_and_a_narrow_one_keeps_them_close() {
        let spec = Spec {
            running: false,
            panel: true,
        };
        let roomy = layout(UiRect::new(0.0, 0.0, 900.0, 28.0), spec);
        let compact = layout(UiRect::new(0.0, 0.0, 400.0, 28.0), spec);
        let pitch = |l: &Layout| l.search.left - l.split.left;
        // Neighbouring tools sit further apart in the wide header...
        assert!(pitch(&roomy) > pitch(&compact));
        assert_eq!(roomy.split.width(), ROOMY.button);
        assert_eq!(compact.split.width(), COMPACT.button);
        // ...and so does the space around each group rule.
        let around = |l: &Layout| l.maximize.unwrap().left - l.rules[0];
        assert!(around(&roomy) > around(&compact));
        // The narrow tools still leave the strip room in the layout's 200px minimum.
        let tight = layout(
            UiRect::new(0.0, 0.0, 200.0, 28.0),
            Spec {
                running: false,
                panel: false,
            },
        );
        assert!(tight.left > 40.0, "left edge of the tools: {}", tight.left);
    }
}
