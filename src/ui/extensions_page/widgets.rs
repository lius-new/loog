//! Drawing helpers shared by the extensions page: tiles, buttons, chips and
//! text that wraps.
use lgui::core::{
    Color, CursorIcon, EventPolicy, ImageFit, PhysicalRect, SemanticRole, Semantics, TextStyle,
    UiElement, UiEventContext, UiId, UiImageSource,
};
use lgui::prelude::{Element, TextAlign, UiRect, VisualStyle, panel, text};

use crate::ui::extensions_binding::{Binding, Entry};
use crate::ui::tabs::{icon, measure_sans};
use crate::ui::theme;

/// Hands out element ids that are unique within one render of the page.
pub(super) struct Ids {
    prefix: String,
    next: usize,
}

impl Ids {
    pub(super) fn new(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
            next: 0,
        }
    }

    pub(super) fn next(&mut self) -> UiId {
        self.next += 1;
        UiId::owned(format!("{}.{}", self.prefix, self.next))
    }
}

/// `over` laid on `base` at `amount` (0 to 1).
pub(super) fn blend(base: Color, over: Color, amount: f32) -> Color {
    let channel = |shift: u32| {
        let a = ((base.0 >> shift) & 0xFF) as f32;
        let b = ((over.0 >> shift) & 0xFF) as f32;
        ((a + (b - a) * amount).round() as u32) << shift
    };
    Color(channel(16) | channel(8) | channel(0))
}

/// The colour of a state: enabled, failed, or quiet.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Tone {
    Ok,
    Bad,
    Quiet,
}

pub(super) fn tone(tone: Tone) -> Color {
    let c = theme::c();
    match tone {
        Tone::Ok => c.diff.add_mark,
        Tone::Bad => c.error,
        Tone::Quiet => c.text_dim,
    }
}

pub(super) fn line(rect: UiRect, value: impl Into<String>, style: TextStyle) -> Element {
    text(rect, value.into(), style).into()
}

pub(super) fn aligned(style: TextStyle, align: TextAlign) -> TextStyle {
    let mut style = style;
    style.align = align;
    style
}

/// Lines of `value` wrapped to `width`.
pub(super) fn wrap(value: &str, width: f32, style: TextStyle) -> Vec<String> {
    crate::ui::file_error::wrapped_lines(value, width.max(40.0), style)
}

/// A paragraph drawn line by line from `(x, y)`; returns the elements and the
/// height they use.
pub(super) fn paragraph(
    x: f32,
    y: f32,
    width: f32,
    value: &str,
    style: TextStyle,
    line_h: f32,
) -> (Vec<Element>, f32) {
    let lines = wrap(value, width, style);
    let height = lines.len() as f32 * line_h;
    let elements = lines
        .into_iter()
        .enumerate()
        .map(|(index, text)| {
            let top = y + index as f32 * line_h;
            // Wide enough that physical font rounding never wraps a drawn line again.
            line(
                UiRect::new(x, top, x + width + 32.0, top + line_h),
                text,
                style,
            )
        })
        .collect();
    (elements, height)
}

/// Package artwork retains its colors and fills the tile in the list and detail.
pub(super) fn extension_tile(
    ids: &mut Ids,
    rect: UiRect,
    entry: &Entry,
    quiet: bool,
    state: &Binding,
) -> Element {
    let Some(image) = entry.icon_image.clone() else {
        return tile(ids, rect, entry.icon, entry.tint, quiet);
    };
    let state = state.clone();
    let id = ids.next();
    Element::new(move |cx| {
        let scale = cx.context.scale();
        let edge = scale.physical_length(rect.width().min(rect.height()));
        let (bytes, version, rect) = match state.icon_thumbnail(&image, edge as u32) {
            Some(thumbnail) => {
                // Place the prefiltered bitmap at its native device size.
                let (width, height) = (thumbnail.width as i32, thumbnail.height as i32);
                let left = scale.physical_value(rect.left) + (edge - width) / 2;
                let top = scale.physical_value(rect.top) + (edge - height) / 2;
                let bounds = PhysicalRect::new(left, top, left + width, top + height);
                (thumbnail.bytes, edge as u64, scale.logical_rect(bounds))
            }
            None => (image.bytes, 0, rect),
        };
        let source = UiImageSource::bytes(
            format!("extension-icon-{:016x}", image.identity),
            version,
            bytes,
        );
        let request = crate::ui::image_rendering::image(source)
            .cache_policy(lgui::memory::ImageCachePolicy::WhileVisible)
            .namespace("extension-icons");
        UiElement::requested_image(id, rect, request, ImageFit::Contain)
    })
}

/// A rounded fallback tile with `icon_key` in `tint`.
pub(super) fn tile(
    ids: &mut Ids,
    rect: UiRect,
    icon_key: &'static str,
    tint: u32,
    quiet: bool,
) -> Element {
    let c = theme::c();
    let tint = if quiet {
        blend(c.bg, Color(tint), 0.45)
    } else {
        Color(tint)
    };
    let inset = rect.width() * 0.27;
    theme::bordered(
        rect,
        blend(c.bg, tint, 0.14),
        blend(c.bg, tint, 0.38),
        rect.width() * 0.22,
        1.0,
    )
    .child(icon(
        ids.next(),
        icon_key,
        UiRect::new(
            rect.left + inset,
            rect.top + inset,
            rect.right - inset,
            rect.bottom - inset,
        ),
        tint,
    ))
}

/// How a pad looks at rest and under the pointer.
#[derive(Clone, Copy)]
pub(super) struct Look {
    pub fill: Option<Color>,
    pub hover: Color,
    pub border: Option<Color>,
    pub radius: f32,
}

/// A clickable region whose fill follows the pointer. Its content goes in as
/// children.
pub(super) fn pad(key: String, rect: UiRect, look: Look) -> Element {
    Element::new(move |cx| {
        let hovered = cx.context.interaction_flags(&cx.id).hovered;
        let fill = if hovered { Some(look.hover) } else { look.fill };
        let style = |color: Option<Color>, radius: f32| match color {
            Some(color) => VisualStyle::filled(color).radius(radius),
            None => VisualStyle::default().radius(radius),
        };
        match look.border {
            Some(border) => {
                let inner = UiRect::new(
                    rect.left + 1.0,
                    rect.top + 1.0,
                    rect.right - 1.0,
                    rect.bottom - 1.0,
                );
                UiElement::panel(cx.id.clone(), rect, style(Some(border), look.radius)).child(
                    UiElement::panel(
                        UiId::owned(format!("{}.fill", cx.id.as_str())),
                        inner,
                        style(fill, (look.radius - 1.0).max(0.0)),
                    )
                    .children(cx.children),
                )
            }
            None => UiElement::panel(cx.id, rect, style(fill, look.radius)).children(cx.children),
        }
    })
    .key(key)
    .event_policy(EventPolicy::INTERACTIVE)
    .cursor(CursorIcon::Pointer)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Primary,
    Plain,
}

/// Width of a button with `label` and an optional icon.
pub(super) fn button_width(label: &str, with_icon: bool, size: f32) -> f32 {
    measure_sans(label, size, 500) + 24.0 + if with_icon { 20.0 } else { 0.0 }
}

/// A button. `on_click` runs on its own; the click goes no further.
pub(super) fn button(
    ids: &mut Ids,
    key: String,
    rect: UiRect,
    label: &str,
    icon_key: Option<&'static str>,
    kind: Kind,
    on_click: impl Fn() + Send + Sync + 'static,
) -> Element {
    let c = theme::c();
    let (fill, hover, border, ink) = match kind {
        Kind::Primary => (c.accent, c.accent_hover, c.accent, c.on_accent),
        Kind::Plain => (c.surface, c.active_line, c.border, c.text),
    };
    let size = if rect.height() < 26.0 {
        theme::SMALL + 1.0
    } else {
        theme::UI_SIZE
    };
    let style = if kind == Kind::Primary {
        theme::sans_semibold(ink, size)
    } else {
        theme::sans(ink, size)
    };
    let label_w = measure_sans(label, size, 500);
    let icon_w = if icon_key.is_some() { 20.0 } else { 0.0 };
    let start = rect.left + (rect.width() - label_w - icon_w) / 2.0;
    let mut button = pad(
        key,
        rect,
        Look {
            fill: Some(fill),
            hover,
            border: Some(border),
            radius: 5.0,
        },
    )
    .semantics(Semantics::new(SemanticRole::Button).name(label.to_owned()));
    if let Some(icon_key) = icon_key {
        let middle = (rect.top + rect.bottom) / 2.0;
        button = button.child(icon(
            ids.next(),
            icon_key,
            UiRect::new(start, middle - 7.0, start + 14.0, middle + 7.0),
            ink,
        ));
    }
    button = button.child(line(
        UiRect::new(
            start + icon_w,
            rect.top,
            start + icon_w + label_w + 24.0,
            rect.bottom,
        ),
        label.to_owned(),
        style,
    ));
    button.on_click(move |cx: &mut UiEventContext| {
        on_click();
        cx.stop_propagation();
    })
}

/// Small text that acts on click.
pub(super) fn link(
    key: String,
    rect: UiRect,
    label: &str,
    align: TextAlign,
    on_click: impl Fn() + Send + Sync + 'static,
) -> Element {
    pad(
        key,
        rect,
        Look {
            fill: None,
            hover: theme::c().active_line,
            border: None,
            radius: 4.0,
        },
    )
    .semantics(Semantics::new(SemanticRole::Button).name(label.to_owned()))
    .child(line(
        UiRect::new(rect.left + 4.0, rect.top, rect.right - 4.0, rect.bottom),
        label.to_owned(),
        aligned(theme::sans(theme::c().accent, theme::SMALL + 1.0), align),
    ))
    .on_click(move |cx: &mut UiEventContext| {
        on_click();
        cx.stop_propagation();
    })
}

/// A rounded outline holding a short label.
pub(super) fn chip(rect: UiRect, label: &str, style: TextStyle) -> Element {
    lgui::badge(rect, label, Some(style.color))
}

pub(super) fn rule(rect: UiRect) -> Element {
    panel(rect, VisualStyle::filled(theme::c().border))
}
