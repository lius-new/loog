//! Custom frameless title bar: the project menu and the active file's path on the left;
//! standing notes, the layout view and the native window controls on the right.
//!
//! The bar is marked as a native window drag region; interactive children
//! (the buttons) are excluded automatically by hit testing.

use lgui::core::{
    AnimProperty, AnimationBinding, Color, CursorIcon, EventPolicy, IconStyle, PhysicalRect,
    SemanticRole, Semantics, UiElement, UiEventContext, UiId, UiScale,
};
use lgui::prelude::{Element, UiRect, VisualStyle, panel, text};

use crate::features::update::{Indicator, Stage};
use crate::ui::git::badge::{BADGE_H, BADGE_ICON, BADGE_PAD, ICON_GAP, TEXT_MARGIN, measure};

use crate::ui::chrome::{PathSegment, Project, Restricted, TitleBinding};
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

/// The popover is aligned with the single layout control, which sits against the window
/// controls.
pub(crate) fn layout_toggle_rect(rect: UiRect) -> UiRect {
    UiRect::new(
        rect.right - 140.0,
        rect.top,
        rect.right - 108.0,
        rect.bottom,
    )
}

pub(crate) fn bottom_border(rect: UiRect) -> Element {
    crate::ui::components::divider::bottom(rect).inert(true)
}

const PROJECT_H: f32 = 24.0;
const TILE: f32 = 18.0;
/// Space left of the project button, and between it and the path.
const EDGE: f32 = 6.0;
const PATH_GAP: f32 = 8.0;
/// Padding inside one path segment.
const SEGMENT_PAD: f32 = 4.0;

fn project_label(project: &Project) -> &str {
    if project.name.is_empty() {
        "No Folder"
    } else {
        &project.name
    }
}

fn project_style() -> lgui::prelude::TextStyle {
    theme::mono_bold(mix(theme::c().accent, Color::WHITE, 0.55), theme::UI_SIZE)
}

/// The project menu's button: a tinted fill, the project's initial on a solid tile, its
/// name and a chevron. It is a control, so it is the one filled thing in the corner.
fn project_button(bar: UiRect, project: Project, state: TitleBinding) -> (Element, f32) {
    let label = project_label(&project).to_owned();
    let width =
        3.0 + TILE + 7.0 + measure(&label, &project_style()) + TEXT_MARGIN + 7.0 + 12.0 + 8.0;
    let top = bar.top + (bar.height() - PROJECT_H) / 2.0;
    let rect = UiRect::new(
        bar.left + EDGE,
        top,
        bar.left + EDGE + width,
        top + PROJECT_H,
    );
    let open = project.menu_open;
    let initial = project.initial.to_string();
    let accessible = format!("Project {label}");
    let element = Element::new(move |cx| {
        let c = theme::c();
        let hover = cx.animation_value(AnimProperty::Hover).clamp(0.0, 1.0);
        let strength = 0.16 + 0.10 * if open { 1.0 } else { hover };
        let fill = VisualStyle {
            fill: Some(c.accent),
            fill_alpha: (strength * 255.0_f32).round() as u8,
            ..VisualStyle::default()
        }
        .radius(6.0);
        let middle = (rect.top + rect.bottom) / 2.0;
        let tile = UiRect::new(
            rect.left + 3.0,
            middle - TILE / 2.0,
            rect.left + 3.0 + TILE,
            middle + TILE / 2.0,
        );
        let text_left = tile.right + 7.0;
        let chevron_left = rect.right - 8.0 - 12.0;
        UiElement::panel(cx.id, rect, fill)
            .child(
                UiElement::panel(
                    UiId::new("titlebar.project.tile"),
                    tile,
                    VisualStyle::filled(c.accent).radius(4.0),
                )
                .child(UiElement::text(
                    UiId::new("titlebar.project.initial"),
                    tile,
                    initial.clone(),
                    crate::ui::highlight_menu::centered(c.sidebar, 11.0, 700),
                )),
            )
            .child(UiElement::text(
                UiId::new("titlebar.project.name"),
                UiRect::new(text_left, rect.top, chevron_left, rect.bottom),
                label.clone(),
                project_style(),
            ))
            .child(
                UiElement::icon(
                    UiId::new("titlebar.project.chevron"),
                    UiRect::new(
                        chevron_left,
                        middle - 6.0,
                        chevron_left + 12.0,
                        middle + 6.0,
                    ),
                    "chevron-down",
                )
                .icon_style(IconStyle::new(project_style().color)),
            )
    })
    .key("titlebar.project.button")
    .animation(AnimationBinding::new(AnimProperty::Hover, 0.0, 1.0))
    .semantics(Semantics::new(SemanticRole::Button).name(accessible))
    .event_policy(EventPolicy::INTERACTIVE)
    .cursor(CursorIcon::Pointer)
    .on_click(move |_: &mut UiEventContext| state.open_project());
    (element, rect.right)
}

fn path_style() -> lgui::prelude::TextStyle {
    theme::mono(theme::c().text_dim, theme::UI_SIZE)
}

fn separator_width() -> f32 {
    measure("›", &theme::mono(theme::c().text_faint, theme::UI_SIZE)) + SEGMENT_PAD
}

fn segment_width(label: &str) -> f32 {
    measure(label, &path_style()) + TEXT_MARGIN + SEGMENT_PAD * 2.0
}

fn path_width(segments: &[PathSegment]) -> f32 {
    segments
        .iter()
        .map(|s| segment_width(&s.label))
        .sum::<f32>()
        + separator_width() * segments.len().saturating_sub(1) as f32
}

/// The path that fits in `budget`: the whole path, else its leading folders folded into
/// "…" until it fits, always keeping the last two parts.
fn fit_path(segments: Vec<PathSegment>, budget: f32) -> Vec<PathSegment> {
    let most = segments.len().saturating_sub(2);
    for hidden in 0..=most {
        let shown: Vec<PathSegment> = if hidden == 0 {
            segments.clone()
        } else {
            std::iter::once(PathSegment {
                label: "…".to_owned(),
                folder: segments[hidden - 1].folder.clone(),
            })
            .chain(segments[hidden..].iter().cloned())
            .collect()
        };
        if path_width(&shown) <= budget || hidden == most {
            return shown;
        }
    }
    segments
}

/// The active file's path as plain text. Folders show a fill on hover and show themselves in
/// the file tree when clicked; the file has no fill, and neither has any part at rest.
fn path_elements(
    bar: UiRect,
    left: f32,
    budget: f32,
    segments: Vec<PathSegment>,
    state: &TitleBinding,
) -> Vec<Element> {
    let segments = fit_path(segments, budget);
    let top = bar.top + (bar.height() - 20.0) / 2.0;
    let mut x = left;
    let mut elements = Vec::new();
    for (index, segment) in segments.into_iter().enumerate() {
        if index > 0 {
            let width = separator_width();
            elements.push(
                text(
                    UiRect::new(
                        x + SEGMENT_PAD / 2.0,
                        bar.top,
                        x + width + TEXT_MARGIN,
                        bar.bottom,
                    ),
                    "›".to_owned(),
                    theme::mono(theme::c().text_faint, theme::UI_SIZE),
                )
                .into(),
            );
            x += width;
        }
        let width = segment_width(&segment.label);
        let rect = UiRect::new(x, top, x + width, top + 20.0);
        x += width;
        let label = segment.label;
        let Some(folder) = segment.folder else {
            elements.push(
                text(
                    UiRect::new(rect.left + SEGMENT_PAD, bar.top, rect.right, bar.bottom),
                    label,
                    path_style(),
                )
                .into(),
            );
            continue;
        };
        let reveal = state.clone();
        let name = format!("Show {label} in the file tree");
        elements.push(
            Element::new(move |cx| {
                let c = theme::c();
                let hover = cx.animation_value(AnimProperty::Hover).clamp(0.0, 1.0);
                let fill = VisualStyle {
                    fill: Some(c.active_line),
                    fill_alpha: (hover * 255.0_f32).round() as u8,
                    ..VisualStyle::default()
                }
                .radius(3.0);
                let color = if hover > 0.5 {
                    c.text_muted
                } else {
                    c.text_dim
                };
                UiElement::panel(cx.id, rect, fill).child(UiElement::text(
                    UiId::owned(format!("titlebar.path.{index}")),
                    UiRect::new(rect.left + SEGMENT_PAD, rect.top, rect.right, rect.bottom),
                    label.clone(),
                    theme::mono(color, theme::UI_SIZE),
                ))
            })
            .key(format!("titlebar.path.{index}"))
            .animation(AnimationBinding::new(AnimProperty::Hover, 0.0, 1.0))
            .semantics(Semantics::new(SemanticRole::Button).name(name))
            .event_policy(EventPolicy::INTERACTIVE)
            .cursor(CursorIcon::Pointer)
            .on_click(move |_: &mut UiEventContext| reveal.reveal(folder.clone())),
        );
    }
    elements
}

pub fn render(rect: UiRect, state: TitleBinding) -> Element {
    let frame = state.frame();
    let maximized = frame.maximized;
    let mut bar = panel(rect, VisualStyle::filled(theme::c().sidebar)).window_drag_region();

    // ---- Right cluster (anchored to the window controls) --------------
    // Flush with the window edge, like native caption buttons, so the close
    // button's hover fill reaches the corner.
    let right = rect.right;

    // The layout control sits against the window controls; its popover opens on hover.
    let layout_rect = layout_toggle_rect(rect);
    let toggle_state = state.clone();
    bar = bar.child(
        caption_button(
            "titlebar.layout.button",
            layout_rect,
            CaptionIcon {
                id: "titlebar.layout.icon",
                key: "layout-view",
                color: if frame.layout_open {
                    theme::c().accent
                } else {
                    theme::c().text_muted
                },
                glyph: Glyph::Ui,
            },
            false,
            move |_: &mut UiEventContext| toggle_state.open_layout(),
        )
        .semantics(Semantics::new(SemanticRole::Button).name("Layout view"))
        .event_policy(EventPolicy {
            hover: true,
            press: true,
            focus: false,
        })
        .cursor(CursorIcon::Pointer),
    );

    // Standing notes sit left of the layout control, the newest need nearest the edge.
    let mut notes_right = layout_rect.left - 6.0;
    if let Some(indicator) = frame.update {
        let width = badge_width(&indicator.text);
        bar = bar.child(update_button(notes_right, rect, indicator, state.clone()));
        notes_right -= width + 6.0;
    }
    if let Some(restricted) = frame.restricted {
        let width = badge_width(&if restricted.count > 1 {
            format!("Restricted · {}", restricted.count)
        } else {
            "Restricted".to_owned()
        });
        bar = bar.child(restricted_button(
            notes_right,
            rect,
            restricted,
            state.clone(),
        ));
        notes_right -= width + 6.0;
    }

    // ---- Left: the project, then the active file's path ---------------
    let (project, project_right) = project_button(rect, frame.project, state.clone());
    bar = bar.child(project);
    let path_left = project_right + PATH_GAP;
    for element in path_elements(
        rect,
        path_left,
        (notes_right - 8.0 - path_left).max(0.0),
        frame.path,
        &state,
    ) {
        bar = bar.child(element);
    }

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

    bar
}
