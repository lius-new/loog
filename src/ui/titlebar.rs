//! Custom frameless title bar: repository controls on the left; the update
//! button, panel toggles, Settings and the native window controls on the right.
//!
//! The bar is marked as a native window drag region; interactive children
//! (the buttons) are excluded automatically by hit testing.

use lgui::core::{
    AnimProperty, AnimationBinding, Color, CursorIcon, EventPolicy, IconStyle, PhysicalRect,
    SemanticRole, Semantics, UiElement, UiEventContext, UiId, UiScale,
};
use lgui::prelude::{Element, UiRect, VisualStyle, panel};

use crate::features::update::{Indicator, Stage};
use crate::ui::git::badge::{BADGE_H, BADGE_ICON, BADGE_PAD, ICON_GAP, TEXT_MARGIN, measure};

use crate::ui::chrome::{PanelToggle, Restricted, TitleBinding};
use crate::ui::theme;

/// Windows 11 caption close-button red, the same in light and dark themes.
const CLOSE_HOVER: Color = Color(0xC42B1C);

#[derive(Clone, Copy)]
enum Glyph {
    /// Pixel-snapped 12px window-control silhouette.
    Window,
    /// Regular UI icon at `theme::ICON_SIZE`.
    Ui,
}

#[derive(Clone, Copy)]
struct CaptionIcon {
    id: &'static str,
    key: &'static str,
    color: Color,
    glyph: Glyph,
}

/// A flat title bar button that fades in a hover background and dims while
/// pressed. `close` uses the red close-button treatment.
fn caption_button(
    key: &'static str,
    rect: UiRect,
    icon: CaptionIcon,
    close: bool,
    on_click: impl Fn(&mut UiEventContext) + Send + Sync + 'static,
) -> Element {
    Element::new(move |cx| {
        let hover = cx.animation_value(AnimProperty::Hover).clamp(0.0, 1.0);
        let pressed = cx.animation_value(AnimProperty::Pressed).clamp(0.0, 1.0);
        let c = theme::c();
        let (fill, fill_alpha, icon_color) = if close {
            (
                CLOSE_HOVER,
                hover * (1.0 - 0.1 * pressed),
                mix(icon.color, Color::WHITE, hover),
            )
        } else {
            (c.active_line, hover * (1.0 - 0.4 * pressed), icon.color)
        };
        let icon_rect = match icon.glyph {
            Glyph::Window => window_icon_rect(rect, cx.context.scale()),
            Glyph::Ui => theme::icon_rect(rect),
        };
        let style = VisualStyle {
            fill: Some(fill),
            fill_alpha: (fill_alpha * 255.0).round() as u8,
            ..VisualStyle::default()
        };
        let icon_alpha = (255.0 * (1.0 - 0.3 * pressed)).round() as u8;
        UiElement::panel(cx.id, rect, style).child(
            UiElement::icon(UiId::new(icon.id), icon_rect, icon.key)
                .icon_style(IconStyle::new(icon_color).alpha(icon_alpha)),
        )
    })
    .key(key)
    .event_policy(EventPolicy::INTERACTIVE)
    .animation(AnimationBinding::new(AnimProperty::Hover, 0.0, 1.0))
    .animation(AnimationBinding::new(AnimProperty::Pressed, 0.0, 1.0))
    .on_click(on_click)
}

fn mix(from: Color, to: Color, t: f32) -> Color {
    let channel = |shift: u32| {
        let a = ((from.0 >> shift) & 0xFF) as f32;
        let b = ((to.0 >> shift) & 0xFF) as f32;
        ((a + (b - a) * t).round() as u32) << shift
    };
    Color(channel(16) | channel(8) | channel(0))
}

/// Keep the SVG bitmap and its destination on the same physical pixel grid.
/// Auto scaling can shrink the UI below 100%; caption glyphs still need at least
/// their native 12px canvas, or a 1px edge becomes a faint fractional pixel.
fn window_icon_rect(button: UiRect, scale: UiScale) -> UiRect {
    let size = scale.physical_length(12.0).max(12);
    let left = scale.physical_value((button.left + button.right) / 2.0) - size / 2;
    let top = scale.physical_value((button.top + button.bottom) / 2.0) - size / 2;
    scale.logical_rect(PhysicalRect::new(left, top, left + size, top + size))
}

/// The update control left of Settings, styled like the repository badge:
/// an icon and short text in the accent color, a hover fill when it can be
/// clicked, and a thin progress line while the installer downloads.
fn update_button(right: f32, bar: UiRect, indicator: Indicator, state: TitleBinding) -> Element {
    let style = |color| theme::mono(color, theme::SMALL);
    let text_w = measure(&indicator.text, &style(theme::c().accent));
    let width = BADGE_PAD * 2.0 + BADGE_ICON + ICON_GAP + text_w + TEXT_MARGIN;
    let top = bar.top + (bar.height() - BADGE_H) / 2.0;
    let rect = UiRect::new(right - width, top, right, top + BADGE_H);
    let stage = indicator.stage;
    let actionable = !matches!(stage, Stage::Downloading(_));
    let label = indicator.text;
    let element = Element::new(move |cx| {
        let c = theme::c();
        let hovered = actionable && cx.context.interaction_flags(&cx.id).hovered;
        let (icon_key, icon_color, text_color) = match stage {
            Stage::Available => ("update-available", c.accent, c.accent),
            Stage::Downloading(_) => ("update-available", c.text_muted, c.text_dim),
            Stage::Ready => ("update-restart", c.accent, c.accent),
        };
        let fill = if hovered {
            VisualStyle::filled(c.active_line).radius(4.0)
        } else {
            VisualStyle::default().radius(4.0)
        };
        let icon_left = rect.left + BADGE_PAD;
        let icon_top = rect.top + (BADGE_H - BADGE_ICON) / 2.0;
        let text_left = icon_left + BADGE_ICON + ICON_GAP;
        let mut badge = UiElement::panel(cx.id, rect, fill)
            .child(
                UiElement::icon(
                    UiId::new("titlebar.update.icon"),
                    UiRect::new(
                        icon_left,
                        icon_top,
                        icon_left + BADGE_ICON,
                        icon_top + BADGE_ICON,
                    ),
                    icon_key,
                )
                .icon_style(IconStyle::new(icon_color)),
            )
            .child(UiElement::text(
                UiId::new("titlebar.update.label"),
                UiRect::new(text_left, rect.top, rect.right - BADGE_PAD, rect.bottom),
                label.clone(),
                style(text_color),
            ));
        if let Stage::Downloading(percent) = stage {
            let track = UiRect::new(
                rect.left + BADGE_PAD,
                rect.bottom - 3.0,
                rect.right - BADGE_PAD,
                rect.bottom - 1.0,
            );
            let done = track.left + track.width() * f32::from(percent) / 100.0;
            badge = badge
                .child(UiElement::panel(
                    UiId::new("titlebar.update.track"),
                    track,
                    VisualStyle::filled(c.border).radius(1.0),
                ))
                .child(UiElement::panel(
                    UiId::new("titlebar.update.progress"),
                    UiRect::new(track.left, track.top, done, track.bottom),
                    VisualStyle::filled(c.accent).radius(1.0),
                ));
        }
        badge
    })
    .key("titlebar.update.button")
    .semantics(Semantics::new(SemanticRole::Button).name(indicator.label));
    if !actionable {
        return element;
    }
    element
        .event_policy(EventPolicy::INTERACTIVE)
        .cursor(CursorIcon::Pointer)
        .on_click(move |ctx: &mut UiEventContext| {
            if state.activate_update() {
                let _ = ctx.window().request_close();
            }
        })
}

/// The width of a badge-style title bar control with `text` beside its icon.
fn badge_width(text: &str) -> f32 {
    let style = theme::mono(theme::c().accent, theme::SMALL);
    BADGE_PAD * 2.0 + BADGE_ICON + ICON_GAP + measure(text, &style) + TEXT_MARGIN
}

/// The standing note that open folders were declined, styled like the update
/// control but in the warning color. Clicking it asks about the folder again.
fn restricted_button(
    right: f32,
    bar: UiRect,
    restricted: Restricted,
    state: TitleBinding,
) -> Element {
    let text = if restricted.count > 1 {
        format!("Restricted · {}", restricted.count)
    } else {
        "Restricted".to_owned()
    };
    let width = badge_width(&text);
    let top = bar.top + (bar.height() - BADGE_H) / 2.0;
    let rect = UiRect::new(right - width, top, right, top + BADGE_H);
    let label = text.clone();
    Element::new(move |cx| {
        let c = theme::c();
        let hovered = cx.context.interaction_flags(&cx.id).hovered;
        let fill = if hovered {
            VisualStyle::filled(c.active_line).radius(4.0)
        } else {
            VisualStyle::default().radius(4.0)
        };
        let icon_left = rect.left + BADGE_PAD;
        let icon_top = rect.top + (BADGE_H - BADGE_ICON) / 2.0;
        let text_left = icon_left + BADGE_ICON + ICON_GAP;
        UiElement::panel(cx.id, rect, fill)
            .child(
                UiElement::icon(
                    UiId::new("titlebar.restricted.icon"),
                    UiRect::new(
                        icon_left,
                        icon_top,
                        icon_left + BADGE_ICON,
                        icon_top + BADGE_ICON,
                    ),
                    "workspace-untrusted",
                )
                .icon_style(IconStyle::new(c.warning)),
            )
            .child(UiElement::text(
                UiId::new("titlebar.restricted.label"),
                UiRect::new(text_left, rect.top, rect.right - BADGE_PAD, rect.bottom),
                label.clone(),
                theme::mono(c.warning, theme::SMALL),
            ))
    })
    .key("titlebar.restricted.button")
    .semantics(Semantics::new(SemanticRole::Button).name(restricted.label))
    .event_policy(EventPolicy::INTERACTIVE)
    .cursor(CursorIcon::Pointer)
    .on_click(move |_: &mut UiEventContext| state.review_trust())
}

/// Width of each panel toggle button.
const TOGGLE_W: f32 = 32.0;

/// Stable key and icon id of a panel toggle.
fn key_for(panel: PanelToggle) -> &'static str {
    match panel {
        PanelToggle::Drawer => "titlebar.toggle.drawer",
        PanelToggle::SourceControl => "titlebar.toggle.source-control",
        PanelToggle::Terminal => "titlebar.toggle.terminal",
    }
}

pub fn render(rect: UiRect, state: TitleBinding, git_controls: Element) -> Element {
    let frame = state.frame();
    let maximized = frame.maximized;
    let mut bar = panel(rect, VisualStyle::filled(theme::c().sidebar)).window_drag_region();

    // ---- Left: repository, branch and sync ----------------------------
    bar = bar.child(git_controls);

    // ---- Right cluster (anchored to the window controls) --------------
    // Flush with the window edge, like native caption buttons, so the close
    // button's hover fill reaches the corner.
    let right = rect.right;

    // Settings, just left of the window controls.
    let settings_r = UiRect::new(right - 148.0, rect.top, right - 116.0, rect.bottom);
    let settings_open = frame.settings_open;

    // Panel toggles, left of Settings: Source Control (left dock), Explorer (right dock), Terminal.
    let toggles = [
        (
            PanelToggle::SourceControl,
            "panel-left",
            "Toggle Source Control",
            frame.source_control_open,
        ),
        (
            PanelToggle::Drawer,
            "panel-right",
            "Toggle Explorer",
            frame.drawer_open,
        ),
        (
            PanelToggle::Terminal,
            "panel-bottom",
            "Toggle Terminal",
            frame.terminal_open,
        ),
    ];
    let extensions_r = UiRect::new(
        settings_r.left - TOGGLE_W,
        rect.top,
        settings_r.left,
        rect.bottom,
    );
    let toggles_left = extensions_r.left - TOGGLE_W * toggles.len() as f32;
    for (index, (panel, key, name, open)) in toggles.into_iter().enumerate() {
        let left = toggles_left + TOGGLE_W * index as f32;
        let toggle_state = state.clone();
        bar = bar.child(
            caption_button(
                key_for(panel),
                UiRect::new(left, rect.top, left + TOGGLE_W, rect.bottom),
                CaptionIcon {
                    id: key_for(panel),
                    key,
                    color: if open {
                        theme::c().accent
                    } else {
                        theme::c().text_muted
                    },
                    glyph: Glyph::Ui,
                },
                false,
                move |_: &mut UiEventContext| toggle_state.toggle(panel),
            )
            .semantics(Semantics::new(SemanticRole::Button).name(name))
            .cursor(CursorIcon::Pointer),
        );
    }

    // Standing notes sit left of the toggles, the newest need nearest the edge.
    let mut notes_right = toggles_left - 6.0;
    if let Some(indicator) = frame.update {
        let width = badge_width(&indicator.text);
        bar = bar.child(update_button(notes_right, rect, indicator, state.clone()));
        notes_right -= width + 6.0;
    }
    if let Some(restricted) = frame.restricted {
        bar = bar.child(restricted_button(
            notes_right,
            rect,
            restricted,
            state.clone(),
        ));
    }
    let extensions_state = state.clone();
    let extensions = caption_button(
        "titlebar.extensions.button",
        extensions_r,
        CaptionIcon {
            id: "titlebar.extensions",
            key: "extensions",
            color: if frame.extensions_open {
                theme::c().accent
            } else {
                theme::c().text_muted
            },
            glyph: Glyph::Ui,
        },
        false,
        move |_: &mut UiEventContext| extensions_state.open_extensions(),
    )
    .semantics(Semantics::new(SemanticRole::Button).name("Extensions"))
    .cursor(CursorIcon::Pointer);
    bar = bar.child(extensions);
    if frame.extensions_attention {
        // A failed extension: a dot at the icon's top-right corner.
        let (cx, cy) = (
            (extensions_r.left + extensions_r.right) / 2.0 + 6.0,
            (extensions_r.top + extensions_r.bottom) / 2.0 - 6.0,
        );
        bar = bar.child(
            panel(
                UiRect::new(cx - 3.0, cy - 3.0, cx + 3.0, cy + 3.0),
                VisualStyle::filled(theme::c().warning).radius(3.0),
            )
            .event_policy(EventPolicy::NONE),
        );
    }
    bar = bar.child(
        caption_button(
            "titlebar.settings.button",
            settings_r,
            CaptionIcon {
                id: "titlebar.settings",
                key: "settings",
                color: if settings_open {
                    theme::c().accent
                } else {
                    theme::c().text_muted
                },
                glyph: Glyph::Ui,
            },
            false,
            move |_: &mut UiEventContext| state.open_settings(),
        )
        .cursor(CursorIcon::Pointer),
    );

    // ---- Window controls (far right) ----------------------------------
    let min_r = UiRect::new(right - 108.0, rect.top, right - 72.0, rect.bottom);
    let max_r = UiRect::new(right - 72.0, rect.top, right - 36.0, rect.bottom);
    let close_r = UiRect::new(right - 36.0, rect.top, right, rect.bottom);
    let window_icon = |id, key| CaptionIcon {
        id,
        key,
        color: theme::c().text_soft,
        glyph: Glyph::Window,
    };

    bar = bar.child(caption_button(
        "titlebar.minimize.button",
        min_r,
        window_icon("titlebar.minimize", "window-minimize"),
        false,
        |ctx: &mut UiEventContext| {
            let _ = ctx.window().minimize();
        },
    ));

    // Maximize / restore. On Windows 11 the OS owns this button so hovering it
    // shows Snap Layouts; lgui replays its hover, press and click here.
    let max_key = if maximized {
        "window-restore"
    } else {
        "window-maximize"
    };
    bar = bar.child(
        caption_button(
            "titlebar.maximize.button",
            max_r,
            window_icon("titlebar.maximize", max_key),
            false,
            |ctx: &mut UiEventContext| {
                let _ = ctx.window().toggle_maximize();
            },
        )
        .window_maximize_button(),
    );

    bar = bar.child(caption_button(
        "titlebar.close.button",
        close_r,
        window_icon("titlebar.close", "window-close"),
        true,
        |ctx: &mut UiEventContext| {
            let _ = ctx.window().request_close();
        },
    ));

    // Hairline bottom border
    bar = bar.child(crate::ui::components::divider::bottom(rect));

    bar
}
