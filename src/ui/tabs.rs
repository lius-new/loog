//! Editor tab strip: buffer tabs with language badges, a close button, and
//! the scrollbar.

use std::cell::RefCell;
use std::collections::HashMap;

use lgui::core::{
    Color, CursorIcon, EventPolicy, IconStyle, PointerButton, UiElement, UiFocusHandle, UiId,
    WheelUnit, clip, precompiled,
};
use lgui::prelude::{Element, TextStyle, UiRect, VisualStyle, panel, text};
use lgui::text::{self, TextLayoutRequest};

use crate::domain::document::FileMeta;
use crate::domain::identity::ViewId;
use crate::domain::pane_layout::{Direction, PaneId};
use crate::domain::workspace::AppPage;
use crate::ui::chrome::tab_state::TabDrop;
use crate::ui::chrome::tabs_binding::{Binding, Command, Frame};
use crate::ui::tab_layout::{self, TabLayoutInput, TabLayoutResult};
use crate::ui::theme;

/// Horizontal padding inside each tab pill (px-3 in the mockup).
pub(super) const PAD: f32 = 14.0;
/// Gap between badge, name and close icon (gap-2 in the mockup).
pub(super) const GAP: f32 = 8.0;
/// Adjacent tabs meet without a gap.
pub(super) const TAB_GAP: f32 = 0.0;
/// Extra right margin inside text rects so the last glyph is not clipped.
pub(super) const TEXT_MARGIN: f32 = 6.0;
/// The tab cluster has no leading or trailing outer padding.
pub(super) const TABS_PAD: f32 = 0.0;
/// Tabs fill the strip vertically without an outer margin.
pub(super) const PILL_INSET: f32 = 0.0;
/// Upper bound for inactive tabs. The active tab uses its full natural width.
pub(super) const MAX_TAB_W: f32 = 220.0;
/// Keeps the badge, an ellipsis and the close button usable when tabs shrink.
pub(super) const MIN_TAB_W: f32 = 96.0;
const EDITOR_MIN_TAB_W: f32 = 120.0;
const DIRTY_MARK: &str = "●";
const DISK_STATE_MARK: &str = "!";
const FADE_W: f32 = 16.0;
pub(crate) const DRAG_THRESHOLD: f32 = 5.0;
pub(crate) const DRAG_EDGE_W: f32 = 28.0;
/// Vertical drift allowed around the dragged tab's own strip before the drag
/// stops reordering and targets a pane drop instead.
pub(crate) const DRAG_REORDER_SLACK: f32 = 16.0;
/// Insertion marker inset from the strip's top and bottom edges.
pub(crate) const REORDER_MARKER_INSET: f32 = 4.0;
pub(crate) const DRAG_SCROLL_STEP: f32 = 18.0;
const TAB_SCROLLBAR_H: f32 = 4.0;
const TAB_SCROLLBAR_HIT_H: f32 = 8.0;
const TAB_SCROLLBAR_MIN_THUMB_W: f32 = 28.0;
/// Extra distance beyond each viewport edge whose tabs are still built, so a
/// wheel step never reveals an empty gap before the next frame.
const RENDER_OVERSCAN: f32 = 240.0;

/// Upper bound for cached text widths; ellipsis probing adds short-lived
/// candidates, so the cache is reset rather than allowed to grow unbounded.
const MEASURE_CACHE_LIMIT: usize = 8_192;

thread_local! {
    /// Shaped text widths. The tab strip lays out every
    /// open tab on each pointer move, so repeated shaping would dominate with
    /// many tabs. Font families are fixed and widths are in logical pixels, so
    /// the key changes whenever a name, size or weight does.
    static MEASURE_CACHE: RefCell<MeasureCache> = RefCell::new(MeasureCache::default());
}

/// Text widths grouped by `(size bits, weight, monospace)` so hits can look up by `&str`
/// without allocating.
#[derive(Default)]
struct MeasureCache {
    widths: HashMap<(u32, i32, bool), HashMap<String, f32>>,
    len: usize,
}

/// Natural text width measured with the renderer's text system, falling back
/// to a per-character estimate when no text system is installed.
pub(crate) fn measure(s: &str, size: f32, weight: i32) -> f32 {
    measure_in(s, size, weight, true)
}

/// The width of `s` in the interface's proportional font, which is what
/// `theme::sans` draws; `measure` is for the monospace face.
pub(crate) fn measure_sans(s: &str, size: f32, weight: i32) -> f32 {
    measure_in(s, size, weight, false)
}

fn measure_in(s: &str, size: f32, weight: i32, mono: bool) -> f32 {
    let style = (size.to_bits(), weight, mono);
    let cached = MEASURE_CACHE.with(|cache| {
        cache
            .borrow()
            .widths
            .get(&style)
            .and_then(|widths| widths.get(s))
            .copied()
    });
    if let Some(width) = cached {
        return width;
    }

    let bounds = UiRect::new(0.0, 0.0, 10_000.0, size);
    let mut request = TextLayoutRequest::single_line(s, bounds, size, weight);
    if mono {
        request.font_families = theme::MONO_FAMILIES;
    }
    match text::layout(&request) {
        Some(layout) => {
            // Only shaped widths are cached: the estimate below is a stand-in
            // until a text system is installed and must not outlive it.
            MEASURE_CACHE.with(|cache| {
                let mut cache = cache.borrow_mut();
                if cache.len >= MEASURE_CACHE_LIMIT {
                    *cache = MeasureCache::default();
                }
                cache.len += 1;
                cache
                    .widths
                    .entry(style)
                    .or_default()
                    .insert(s.to_owned(), layout.width);
            });
            layout.width
        }
        None => crate::ui::components::text::fallback_width(s, size),
    }
}

pub(crate) fn ellipsize(value: &str, max_width: f32, size: f32, weight: i32) -> String {
    ellipsize_with(value, max_width, |s| measure(s, size, weight))
}

/// `ellipsize` for text drawn in the interface's proportional font.
pub(crate) fn ellipsize_sans(value: &str, max_width: f32, size: f32, weight: i32) -> String {
    ellipsize_with(value, max_width, |s| measure_sans(s, size, weight))
}

fn ellipsize_with(value: &str, max_width: f32, measure: impl Fn(&str) -> f32) -> String {
    if measure(value) <= max_width {
        return value.to_owned();
    }

    const ELLIPSIS: &str = "…";
    if max_width <= measure(ELLIPSIS) {
        return ELLIPSIS.to_owned();
    }

    let suffix = value
        .rfind('.')
        .filter(|index| *index > 0)
        .map(|index| &value[index..])
        .filter(|suffix| measure(suffix) + measure(ELLIPSIS) < max_width)
        .unwrap_or("");
    let stem = &value[..value.len() - suffix.len()];
    let chars: Vec<char> = stem.chars().collect();
    let mut low = 0;
    let mut high = chars.len();
    while low < high {
        let middle = (low + high + 1) / 2;
        let candidate = chars[..middle]
            .iter()
            .copied()
            .chain(ELLIPSIS.chars())
            .chain(suffix.chars())
            .collect::<String>();
        if measure(&candidate) <= max_width {
            low = middle;
        } else {
            high = middle - 1;
        }
    }

    chars[..low]
        .iter()
        .copied()
        .chain(ELLIPSIS.chars())
        .chain(suffix.chars())
        .collect()
}

/// A color-tinted, resolution-independent SVG icon (rasterized at physical pixels).
pub(crate) fn icon(
    id: UiId,
    key: impl Into<std::sync::Arc<str>>,
    rect: UiRect,
    color: Color,
) -> Element {
    precompiled(UiElement::icon(id, rect, key).icon_style(IconStyle::new(color)))
}

/// The vector "close" icon, centred in `bounds`.
pub(crate) fn close_glyph(id: UiId, bounds: UiRect, color: Color) -> Element {
    let (cx, cy) = (
        (bounds.left + bounds.right) / 2.0,
        (bounds.top + bounds.bottom) / 2.0,
    );
    let half = CLOSE_ICON / 2.0;
    icon(
        id,
        "close",
        UiRect::new(cx - half, cy - half, cx + half, cy + half),
        color,
    )
}

/// File and loading icons share one 16 px slot on the tab's baseline.
const BADGE_ICON: f32 = 16.0;
const CLOSE_ICON: f32 = 12.0;

/// Leading tab marker: a language/diff label, or an SVG icon.
#[derive(Clone, Copy)]
enum TabBadge {
    Text(&'static str, Color),
    Icon(&'static str, Color),
}

impl TabBadge {
    fn width(self) -> f32 {
        match self {
            Self::Text(label, _) => measure(label, theme::SMALL, 700),
            Self::Icon(..) => BADGE_ICON,
        }
    }

    /// `key` names the tab, so icon element ids stay unique per tab.
    fn render(self, left: f32, pill: UiRect, key: &str) -> Element {
        match self {
            Self::Text(label, color) => text(
                UiRect::new(
                    left,
                    pill.top,
                    left + self.width() + TEXT_MARGIN,
                    pill.bottom,
                ),
                label,
                theme::mono_bold(color, theme::SMALL),
            )
            .into(),
            Self::Icon(icon_key, color) => {
                let top = pill.top + (pill.height() - BADGE_ICON) / 2.0;
                icon(
                    UiId::owned(format!("tabs.badge.{key}")),
                    icon_key,
                    UiRect::new(left, top, left + BADGE_ICON, top + BADGE_ICON),
                    color,
                )
            }
        }
    }
}

fn tab_badge(app: &Frame<'_>, id: ViewId, meta: &FileMeta) -> TabBadge {
    if let Some(page) = app.workspace.page(id) {
        let icon = match page {
            AppPage::Settings => "settings",
            AppPage::Keymap => "command",
            AppPage::Extensions => "extensions",
        };
        TabBadge::Icon(icon, theme::c().text_muted)
    } else if let Some(diff) = app.workspace.diff_of(id) {
        // A commit's all-files tab, or a file or repository changes diff.
        let icon = if diff.revision.is_some() && diff.sections.is_some() {
            "git-commit"
        } else {
            "diff"
        };
        TabBadge::Icon(icon, theme::c().badge.diff)
    } else {
        TabBadge::Icon(
            crate::ui::file_icons::icon_for_file(&meta.name),
            theme::c().text_muted,
        )
    }
}

pub(crate) fn tab_name_style(active: bool, preview: bool) -> TextStyle {
    let style = if active {
        theme::sans(theme::c().text_bright, theme::UI_SIZE)
    } else {
        theme::sans(theme::c().text_muted, theme::UI_SIZE)
    };
    if preview { style.italic() } else { style }
}

pub(crate) fn tab_state_mark(app: &Frame<'_>, id: ViewId) -> Option<(&'static str, Color)> {
    if app
        .workspace
        .view_document(id)
        .is_some_and(|id| app.workspace.is_missing_on_disk(id))
    {
        Some((DISK_STATE_MARK, theme::c().error))
    } else if app
        .workspace
        .view_document(id)
        .is_some_and(|id| app.workspace.has_disk_conflict(id))
    {
        Some((DISK_STATE_MARK, theme::c().warning))
    } else if app.workspace.is_view_dirty(id) {
        Some((DIRTY_MARK, theme::c().accent))
    } else {
        None
    }
}

pub(crate) fn pane_items<'a>(app: &Frame<'a>, pane: PaneId) -> &'a [ViewId] {
    app.workspace.pane(pane).map_or(&[], |pane| pane.items())
}

pub(crate) fn pane_active(app: &Frame<'_>, pane: PaneId) -> Option<ViewId> {
    app.workspace.pane(pane).and_then(|pane| pane.active())
}

pub(crate) fn natural_tab_widths(app: &Frame<'_>, pane: PaneId) -> Vec<f32> {
    let labels = app.workspace.tab_labels(pane);
    let mut widths: Vec<f32> = pane_items(app, pane)
        .iter()
        .filter_map(|id| {
            let meta = app.workspace.view_meta(*id)?;
            let badge = tab_badge(app, *id, meta);
            let state_slot = if let Some((mark, _)) = tab_state_mark(app, *id) {
                measure(mark, theme::SMALL, 400) + GAP
            } else {
                0.0
            };
            let fixed_width = PAD + badge.width() + GAP + GAP + state_slot + CLOSE_ICON + PAD;
            let label = labels.get(id).map(String::as_str).unwrap_or(&meta.name);
            Some(fixed_width + measure_sans(label, theme::UI_SIZE, 400) + TEXT_MARGIN)
        })
        .collect();
    if let Some(pending) = app.editor.pending_file.as_ref().filter(|p| p.pane == pane) {
        let meta = FileMeta::from_path(pending.path.clone());
        widths.push(
            (PAD * 2.0
                + BADGE_ICON
                + GAP * 2.0
                + CLOSE_ICON
                + measure_sans(&meta.name, theme::UI_SIZE, 400)
                + TEXT_MARGIN)
                .clamp(EDITOR_MIN_TAB_W, MAX_TAB_W),
        );
    }
    widths
}

pub(crate) fn tab_layout(
    rect: UiRect,
    app: &Frame<'_>,
    pane: PaneId,
    reveal_active: bool,
) -> TabLayoutResult {
    let widths = natural_tab_widths(app, pane);
    let active_index = pane_active(app, pane)
        .and_then(|active| pane_items(app, pane).iter().position(|id| *id == active));
    tab_layout::calculate(TabLayoutInput {
        natural_widths: &widths,
        viewport_width: rect.width().max(0.0),
        scroll_x: tab_scroll_x(app, pane),
        active_index,
        reveal_active,
        min_tab_width: EDITOR_MIN_TAB_W,
        max_tab_width: MAX_TAB_W,
        gap: TAB_GAP,
        padding: TABS_PAD,
    })
}

pub fn tab_scroll_x(app: &Frame<'_>, pane: PaneId) -> f32 {
    app.editor
        .tabs
        .tab_scroll
        .get(&pane)
        .copied()
        .unwrap_or(0.0)
}

pub fn active_visible_scroll(rect: UiRect, app: &Frame<'_>, pane: PaneId) -> f32 {
    tab_layout(rect, app, pane, true).scroll_x
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TabScrollbarGeometry {
    pub(crate) track: UiRect,
    pub(crate) thumb_left: f32,
    pub(crate) thumb_width: f32,
    pub(crate) travel: f32,
    pub(crate) max_scroll_x: f32,
}

pub(crate) fn scrollbar_geometry(
    rect: UiRect,
    layout: &TabLayoutResult,
) -> Option<TabScrollbarGeometry> {
    if layout.max_scroll_x <= 0.0 || layout.content_width <= 0.0 {
        return None;
    }
    let track = UiRect::new(
        rect.left + TABS_PAD,
        rect.bottom - TAB_SCROLLBAR_HIT_H,
        rect.left + layout.viewport_width - TABS_PAD,
        rect.bottom,
    );
    if track.width() <= 0.0 {
        return None;
    }
    let thumb_width = (track.width() * layout.viewport_width / layout.content_width)
        .max(TAB_SCROLLBAR_MIN_THUMB_W)
        .min(track.width());
    let travel = (track.width() - thumb_width).max(0.0);
    let thumb_left = if layout.max_scroll_x <= 0.0 {
        track.left
    } else {
        track.left + travel * (layout.scroll_x / layout.max_scroll_x)
    };
    Some(TabScrollbarGeometry {
        track,
        thumb_left,
        thumb_width,
        travel,
        max_scroll_x: layout.max_scroll_x,
    })
}

pub(crate) fn scroll_from_pointer(
    pointer_x: f32,
    offset: f32,
    geometry: TabScrollbarGeometry,
) -> f32 {
    if geometry.travel <= 0.0 {
        return 0.0;
    }
    let thumb_left = (pointer_x - geometry.track.left - offset).clamp(0.0, geometry.travel);
    thumb_left / geometry.travel * geometry.max_scroll_x
}

pub(crate) fn point_inside(rect: UiRect, x: f32, y: f32) -> bool {
    x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom
}

/// A pane's tab strip and the surface below it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaneRegion {
    pub pane: PaneId,
    pub strip: UiRect,
    pub body: UiRect,
}

/// Which side of a pane surface a dropped tab splits, or `None` to merge.
/// Mirrors VSCode: the centre merges; near an edge, the outer thirds of the
/// width split left/right and the middle third splits up/down.
pub fn drop_zone(body: UiRect, x: f32, y: f32) -> Option<Direction> {
    const EDGE: f32 = 0.2;
    let edge_x = body.width() * EDGE;
    let edge_y = body.height() * EDGE;
    let central = x > body.left + edge_x
        && x < body.right - edge_x
        && y > body.top + edge_y
        && y < body.bottom - edge_y;
    if central {
        return None;
    }
    let third = body.width() / 3.0;
    Some(if x < body.left + third {
        Direction::Left
    } else if x > body.right - third {
        Direction::Right
    } else if y < body.top + body.height() / 2.0 {
        Direction::Up
    } else {
        Direction::Down
    })
}

/// The rectangle a drop would fill, for the drag overlay.
pub fn drop_preview(body: UiRect, split: Option<Direction>) -> UiRect {
    let mid_x = body.left + body.width() / 2.0;
    let mid_y = body.top + body.height() / 2.0;
    match split {
        None => body,
        Some(Direction::Left) => UiRect::new(body.left, body.top, mid_x, body.bottom),
        Some(Direction::Right) => UiRect::new(mid_x, body.top, body.right, body.bottom),
        Some(Direction::Up) => UiRect::new(body.left, body.top, body.right, mid_y),
        Some(Direction::Down) => UiRect::new(body.left, mid_y, body.right, body.bottom),
    }
}

/// Insertion index for a tab dropped at `content_x` into a foreign strip.
pub(crate) fn insertion_index(layout: &TabLayoutResult, content_x: f32) -> usize {
    layout
        .items
        .iter()
        .find(|item| content_x < (item.left + item.right) / 2.0)
        .map_or(layout.items.len(), |item| item.index)
}

/// Where a file dragged from the Explorer would open: a tab strip inserts it,
/// a pane surface opens it there or splits that pane near an edge.
pub fn file_drop_target(
    app: &Frame<'_>,
    regions: &[PaneRegion],
    pointer_x: f32,
    pointer_y: f32,
) -> Option<TabDrop> {
    if let Some(region) = regions
        .iter()
        .find(|region| point_inside(region.strip, pointer_x, pointer_y))
    {
        let layout = tab_layout(region.strip, app, region.pane, false);
        let content_x = pointer_x - region.strip.left + layout.scroll_x;
        return Some(TabDrop::Insert {
            pane: region.pane,
            index: insertion_index(&layout, content_x),
        });
    }
    regions
        .iter()
        .find(|region| point_inside(region.body, pointer_x, pointer_y))
        .map(|region| TabDrop::Pane {
            pane: region.pane,
            split: drop_zone(region.body, pointer_x, pointer_y),
        })
}

pub fn render_tooltip(
    viewport: UiRect,
    strips: &[(PaneId, UiRect)],
    app: &Frame<'_>,
) -> Option<Element> {
    let (pane, hovered) = app.editor.tabs.tab_hovered?;
    let rect = strips.iter().find(|(id, _)| *id == pane)?.1;
    let meta = app.workspace.view_meta(hovered)?;
    let path = meta
        .local_path()
        .map(super::display_path)
        .unwrap_or_else(|| meta.resource.display().to_string());
    let index = pane_items(app, pane).iter().position(|id| *id == hovered)?;
    let layout = tab_layout(rect, app, pane, false);
    let item = layout.items.get(index)?;
    let natural_width = measure(&path, theme::SMALL, 400) + 20.0;
    let width = natural_width.min((viewport.width() - 16.0).max(80.0));
    let item_left = rect.left + item.left - layout.scroll_x;
    let left = item_left.clamp(viewport.left + 8.0, viewport.right - width - 8.0);
    let tooltip_rect = UiRect::new(left, rect.bottom + 4.0, left + width, rect.bottom + 28.0);
    Some(
        theme::bordered(
            tooltip_rect,
            theme::c().surface,
            theme::c().border,
            4.0,
            1.0,
        )
        .child(text(
            UiRect::new(
                tooltip_rect.left + 9.0,
                tooltip_rect.top + 4.0,
                tooltip_rect.right - 9.0,
                tooltip_rect.bottom - 3.0,
            ),
            path,
            theme::mono(theme::c().text_soft, theme::SMALL),
        )),
    )
}

fn active_tab_background(rect: UiRect) -> Element {
    panel(rect, VisualStyle::filled(theme::c().active_tab).alpha(51))
}

pub fn render(pane: PaneId, rect: UiRect, state: Binding, editor_focus: UiFocusHandle) -> Element {
    let snapshot = state.snapshot();
    let s = snapshot.frame();
    let active = pane_active(&s, pane);
    let labels = s.workspace.tab_labels(pane);

    let mut bar = panel(rect, VisualStyle::filled(theme::c().sidebar))
        .key(format!("tab-strip-{}", pane.get()));
    let layout = tab_layout(rect, &s, pane, false);
    let tab_viewport = UiRect::new(
        rect.left,
        rect.top,
        rect.left + layout.viewport_width,
        rect.bottom,
    );
    let mut tab_layer = clip(tab_viewport, -layout.scroll_x, 0.0);

    if layout.max_scroll_x > 0.0 {
        let wheel_state = state.clone();
        let max_scroll_x = layout.max_scroll_x;
        bar = bar.on_wheel(move |cx, delta| {
            let primary_delta = if delta.x.abs() > delta.y.abs() {
                delta.x
            } else {
                delta.y
            };
            let step = match delta.unit {
                WheelUnit::Lines => primary_delta * 48.0,
                WheelUnit::Pixels => primary_delta,
            };
            wheel_state.run(Command::Wheel(pane, step, max_scroll_x));
            cx.stop_propagation();
        });
    }

    let mut active_rect = None;
    // Tabs fill the strip vertically and meet without gaps or outer padding.
    if s.welcome_panes.contains(&pane)
        && !s
            .editor
            .pending_file
            .as_ref()
            .is_some_and(|p| p.pane == pane)
    {
        let label = "welcome";
        let label_w = measure(label, theme::UI_SIZE, 400);
        let tab_w = (PAD * 2.0 + label_w + TEXT_MARGIN).clamp(MIN_TAB_W, MAX_TAB_W);
        let pill = UiRect::new(
            rect.left + TABS_PAD,
            rect.top + PILL_INSET,
            rect.left + TABS_PAD + tab_w,
            rect.bottom - PILL_INSET,
        );
        active_rect = Some(pill);
        tab_layer = tab_layer.child(active_tab_background(pill).child(text(
            UiRect::new(pill.left + PAD, pill.top, pill.right - PAD, pill.bottom),
            label,
            theme::mono(theme::c().text_bright, theme::UI_SIZE),
        )));
    }
    // Only tabs near the viewport become elements. The drag source is kept
    // even when edge auto-scroll moves it away so its keyed element survives
    // until the gesture ends.
    let open_files = pane_items(&s, pane);
    // Same as the terminal tabs: a pointer while idle, a move cursor for the
    // whole drag.
    let tab_cursor = if s
        .editor
        .tabs
        .tab_drag
        .as_ref()
        .is_some_and(|drag| drag.active)
    {
        CursorIcon::Move
    } else {
        CursorIcon::Pointer
    };
    let visible = tab_layout::visible_range(
        &layout.items,
        layout.scroll_x,
        layout.viewport_width,
        RENDER_OVERSCAN,
    );
    let drag_source_index = s
        .editor
        .tabs
        .tab_drag
        .as_ref()
        .and_then(|drag| open_files.iter().position(|id| *id == drag.source))
        .filter(|index| !visible.contains(index));
    for index in visible.chain(drag_source_index) {
        let Some(&id) = open_files.get(index) else {
            continue;
        };
        let Some(m) = s.workspace.view_meta(id) else {
            continue;
        };
        let Some(layout_item) = layout.items.get(index) else {
            continue;
        };
        let badge = tab_badge(&s, id, m);
        let badge_w = badge.width();
        let state_mark = tab_state_mark(&s, id);
        let state_mark_w = state_mark.map_or(0.0, |(mark, _)| measure(mark, theme::SMALL, 400));
        let close_w = CLOSE_ICON;
        let tab_w = layout_item.width;
        let x = rect.left + layout_item.left;

        let pill = UiRect::new(
            x,
            rect.top + PILL_INSET,
            x + tab_w,
            rect.bottom - PILL_INSET,
        );
        let name_style = tab_name_style(
            Some(id) == active,
            s.workspace.pane(pane).is_some_and(|p| p.is_preview(id)),
        );

        let badge_left = pill.left + PAD;
        let name_left = badge_left + badge_w + GAP;
        let close_left = pill.right - PAD - close_w;
        let state_mark_left = close_left - GAP - state_mark_w;
        let name_right = if state_mark.is_some() {
            state_mark_left - GAP
        } else {
            close_left - GAP
        };
        let name_slot_w = (name_right - name_left).max(0.0);
        let label = labels.get(&id).cloned().unwrap_or_else(|| m.name.clone());
        let display_name = if Some(id) == active {
            label
        } else {
            ellipsize_sans(
                &label,
                (name_slot_w - TEXT_MARGIN).max(0.0),
                theme::UI_SIZE,
                400,
            )
        };

        let st = state.clone();
        let focus = editor_focus.clone();
        // The active tab uses a neutral background and an accent bottom edge.
        let mut tab_el = if Some(id) == active {
            active_rect = Some(pill);
            active_tab_background(pill)
        } else {
            panel(pill, VisualStyle::default().radius(2.0))
        }
        .key(format!("editor-tab-{}-{id:?}", pane.get()))
        .event_policy(EventPolicy::INTERACTIVE)
        .cursor(tab_cursor)
        .on_pointer_down_with_button(move |cx, pointer, button| {
            match button {
                PointerButton::Left => {
                    let pointer_x = pointer.point.x;
                    let pointer_y = pointer.point.y;
                    st.run(Command::Press(pane, id, index, pointer_x, pointer_y));
                    focus.focus();
                }
                PointerButton::Middle => {
                    st.run(Command::Close(pane, id));
                    focus.focus();
                }
                PointerButton::Right => {
                    let position = (pointer.point.x, pointer.point.y);
                    st.run(Command::Menu(pane, id, position));
                }
                _ => return,
            }
            cx.stop_propagation();
        });

        tab_el = tab_el.child(
            if s.workspace
                .view_document(id)
                .is_some_and(|id| s.workspace.loading_indicator(id))
            {
                let top = pill.top + (pill.height() - BADGE_ICON) / 2.0;
                crate::ui::components::loading_indicator::render(
                    UiRect::new(badge_left, top, badge_left + BADGE_ICON, top + BADGE_ICON),
                    theme::c().text_muted,
                    format!("tabs.restore.{}-{id:?}", pane.get()),
                )
            } else {
                badge.render(badge_left, pill, &format!("{}-{id:?}", pane.get()))
            },
        );
        tab_el = tab_el.child(text(
            UiRect::new(name_left, pill.top, name_left + name_slot_w, pill.bottom),
            display_name,
            name_style,
        ));
        if let Some((mark, color)) = state_mark {
            tab_el = tab_el.child(text(
                UiRect::new(
                    state_mark_left,
                    pill.top,
                    state_mark_left + state_mark_w + TEXT_MARGIN,
                    pill.bottom,
                ),
                mark,
                theme::mono_bold(color, theme::SMALL),
            ));
        }

        // The close slot is always reserved by the layout, but the control is
        // only painted and hit-testable for the active or hovered tab.
        if Some(id) == active || s.editor.tabs.tab_hovered == Some((pane, id)) {
            let st = state.clone();
            let focus = editor_focus.clone();
            tab_el = tab_el.child(
                panel(
                    UiRect::new(close_left, pill.top, pill.right - PAD, pill.bottom),
                    VisualStyle::default(),
                )
                .key(format!("editor-tab-close-{}-{id:?}", pane.get()))
                .event_policy(EventPolicy::INTERACTIVE)
                .on_pointer_down_with_button(move |cx, _pointer, button| {
                    if button == PointerButton::Left {
                        cx.stop_propagation();
                    }
                })
                .on_click(move || {
                    st.run(Command::Close(pane, id));
                    focus.focus();
                })
                .child(icon(
                    UiId::owned(format!("tabs.close.{}-{id:?}", pane.get())),
                    "close",
                    UiRect::new(
                        close_left,
                        pill.top + (pill.height() - CLOSE_ICON) / 2.0,
                        close_left + CLOSE_ICON,
                        pill.top + (pill.height() + CLOSE_ICON) / 2.0,
                    ),
                    theme::c().text_dim,
                )),
            );
        }

        tab_layer = tab_layer.child(tab_el);
        // Tabs are divided by a 1px edge on their right side, snapped to
        // physical pixels so fractional tab widths never blur it.
        let (right, top, bottom) = (pill.right, pill.top, pill.bottom);
        tab_layer = tab_layer.child(Element::new(move |cx| {
            let scale = cx.context.scale();
            let edge = scale.physical_value(right) - scale.physical_length(1.0);
            let line = scale.logical_rect(lgui::core::PhysicalRect::new(
                edge,
                scale.physical_value(top),
                edge + scale.physical_length(1.0),
                scale.physical_value(bottom),
            ));
            UiElement::panel(cx.id, line, VisualStyle::filled(theme::c().border))
                .hit_rect(UiRect::new(0.0, 0.0, 0.0, 0.0))
        }));
    }

    // The target tab reserves its icon slot immediately; only that icon changes
    // after 300 ms. The old document remains the active central surface.
    if let Some(pending) = s.editor.pending_file.as_ref().filter(|p| p.pane == pane)
        && let Some(item) = layout.items.get(open_files.len())
    {
        let pill = UiRect::new(
            rect.left + item.left,
            rect.top + PILL_INSET,
            rect.left + item.right,
            rect.bottom - PILL_INSET,
        );
        let meta = FileMeta::from_path(pending.path.clone());
        let badge_left = pill.left + PAD;
        let badge_top = pill.top + (pill.height() - BADGE_ICON) / 2.0;
        let badge_rect = UiRect::new(
            badge_left,
            badge_top,
            badge_left + BADGE_ICON,
            badge_top + BADGE_ICON,
        );
        let badge = if pending.indicator {
            crate::ui::components::loading_indicator::render(
                badge_rect,
                theme::c().text_muted,
                format!("tabs.loading.{}-{}", pane.get(), pending.generation),
            )
        } else {
            icon(
                UiId::owned(format!("tabs.pending.{}", pane.get())),
                crate::ui::file_icons::icon_for_file(&meta.name),
                badge_rect,
                theme::c().text_muted,
            )
        };
        let name = ellipsize_sans(
            &meta.name,
            (pill.width() - PAD * 2.0 - BADGE_ICON - GAP * 2.0 - CLOSE_ICON).max(0.0),
            theme::UI_SIZE,
            400,
        );
        let cancel_state = state.clone();
        let generation = pending.generation;
        tab_layer = tab_layer.child(
            panel(pill, VisualStyle::default().radius(2.0))
                .key(format!("loading-tab-{}", pane.get()))
                .child(badge)
                .child(text(
                    UiRect::new(
                        pill.left + PAD + BADGE_ICON + GAP,
                        pill.top,
                        pill.right - PAD - CLOSE_ICON - GAP,
                        pill.bottom,
                    ),
                    name,
                    theme::sans(theme::c().text_muted, theme::UI_SIZE),
                ))
                .child(
                    panel(
                        UiRect::new(
                            pill.right - PAD - CLOSE_ICON,
                            pill.top,
                            pill.right - PAD,
                            pill.bottom,
                        ),
                        VisualStyle::default(),
                    )
                    .event_policy(EventPolicy::INTERACTIVE)
                    .on_click(move || {
                        cancel_state.run(Command::CancelPending(generation));
                    })
                    .child(icon(
                        UiId::owned(format!("tabs.pending.close.{}", pane.get())),
                        "close",
                        UiRect::new(
                            pill.right - PAD - CLOSE_ICON,
                            pill.top + (pill.height() - CLOSE_ICON) / 2.0,
                            pill.right - PAD,
                            pill.top + (pill.height() + CLOSE_ICON) / 2.0,
                        ),
                        theme::c().text_dim,
                    )),
                ),
        );
    }
    bar = bar.child(tab_layer);

    // Insertion marker: a reorder within this strip, or a tab dragged in
    // from another pane.
    let file_drop = s
        .editor
        .tabs
        .file_drag
        .as_ref()
        .filter(|drag| drag.active)
        .and_then(|drag| drag.drop);
    let marker = s
        .editor
        .tabs
        .tab_drag
        .as_ref()
        .filter(|drag| drag.active)
        .map(|drag| (Some(drag), drag.drop))
        .or(file_drop.map(|drop| (None, Some(drop))))
        .and_then(|(drag, drop)| match drop {
            None if drag.is_some_and(|drag| drag.pane == pane) => {
                let drag = drag?;
                let source_index = open_files.iter().position(|id| *id == drag.source)?;
                let target = layout.items.get(drag.target_index)?;
                (drag.target_index != source_index).then_some(if drag.target_index < source_index {
                    target.left
                } else {
                    target.right
                })
            }
            Some(TabDrop::Insert {
                pane: target,
                index,
            }) if target == pane => Some(layout.items.get(index).map_or_else(
                || layout.items.last().map_or(TABS_PAD, |item| item.right),
                |item| item.left,
            )),
            _ => None,
        });
    if let Some(content_x) = marker {
        let screen_x = rect.left + content_x - layout.scroll_x;
        if screen_x >= tab_viewport.left && screen_x <= tab_viewport.right {
            bar = bar.child(panel(
                UiRect::new(
                    screen_x - 1.0,
                    rect.top + REORDER_MARKER_INSET,
                    screen_x + 1.0,
                    rect.bottom - REORDER_MARKER_INSET,
                ),
                VisualStyle::filled(theme::c().accent).radius(1.0),
            ));
        }
    }

    const FADE_ALPHA: [u8; 4] = [210, 150, 90, 35];
    if layout.scroll_x > 0.0 {
        let strip_w = FADE_W / FADE_ALPHA.len() as f32;
        for (index, alpha) in FADE_ALPHA.into_iter().enumerate() {
            let left = tab_viewport.left + index as f32 * strip_w;
            bar = bar.child(panel(
                UiRect::new(left, rect.top, left + strip_w, rect.bottom),
                VisualStyle::filled(theme::c().sidebar).alpha(alpha),
            ));
        }
    }
    if layout.scroll_x < layout.max_scroll_x {
        let strip_w = FADE_W / FADE_ALPHA.len() as f32;
        for (index, alpha) in FADE_ALPHA.into_iter().enumerate() {
            let right = tab_viewport.right - index as f32 * strip_w;
            bar = bar.child(panel(
                UiRect::new(right - strip_w, rect.top, right, rect.bottom),
                VisualStyle::filled(theme::c().sidebar).alpha(alpha),
            ));
        }
    }

    if (s.editor.tabs.tab_strip_hovered == Some(pane)
        || s.editor.tabs.tab_scrollbar_dragging == Some(pane))
        && let Some(geometry) = scrollbar_geometry(rect, &layout)
    {
        let visual_track = UiRect::new(
            geometry.track.left,
            geometry.track.bottom - TAB_SCROLLBAR_H - 1.0,
            geometry.track.right,
            geometry.track.bottom - 1.0,
        );
        let thumb_rect = UiRect::new(
            geometry.thumb_left,
            visual_track.top,
            geometry.thumb_left + geometry.thumb_width,
            visual_track.bottom,
        );
        let track_state = state.clone();
        let thumb_state = state.clone();
        let scrollbar = panel(geometry.track, VisualStyle::default())
            .key(format!("tabs-horizontal-scrollbar-{}", pane.get()))
            .event_policy(EventPolicy::INTERACTIVE)
            .on_pointer_down_with_button(move |cx, pointer, button| {
                if button != PointerButton::Left {
                    return;
                }
                let offset = geometry.thumb_width / 2.0;
                let next = scroll_from_pointer(pointer.point.x, offset, geometry);
                track_state.run(Command::Track(pane, next, offset));
                cx.stop_propagation();
            })
            .child(panel(
                visual_track,
                VisualStyle::filled(theme::c().scrollbar).radius(TAB_SCROLLBAR_H / 2.0),
            ))
            .child(
                panel(
                    thumb_rect,
                    VisualStyle::filled(theme::c().text_dim).radius(TAB_SCROLLBAR_H / 2.0),
                )
                .key(format!("tabs-horizontal-scrollbar-thumb-{}", pane.get()))
                .event_policy(EventPolicy::INTERACTIVE)
                .on_pointer_down_with_button(move |cx, pointer, button| {
                    if button != PointerButton::Left {
                        return;
                    }
                    let offset = pointer.point.x - geometry.thumb_left;
                    thumb_state.run(Command::Thumb(pane, offset));
                    cx.stop_propagation();
                }),
            );
        bar = bar.child(scrollbar);
    }

    // Paint the 1px separator after all backgrounds,
    // fades and scrollbar layers so they cannot tint or cover the edge.
    bar = bar.child(crate::ui::components::divider::bottom(rect));
    if let Some(active) = active_rect {
        // Decoration only: a hit-testable clip over the whole viewport would
        // sit in front of the tabs and hide their cursor.
        let scroll_x = layout.scroll_x;
        bar = bar.child(Element::new(move |cx| {
            let no_hit = UiRect::new(0.0, 0.0, 0.0, 0.0);
            let scale = cx.context.scale();
            let top = scale.physical_value(active.top);
            let underline = scale.logical_rect(lgui::core::PhysicalRect::new(
                scale.physical_value(active.left),
                top,
                scale.physical_value(active.right),
                top + scale.physical_length(2.0),
            ));
            let underline_id = cx.auto_id();
            UiElement::clip(cx.id, tab_viewport, -scroll_x, 0.0)
                .hit_rect(no_hit)
                .children(vec![
                    UiElement::panel(
                        underline_id,
                        underline,
                        VisualStyle::filled(theme::c().accent),
                    )
                    .hit_rect(no_hit),
                ])
        }));
    }

    bar
}
