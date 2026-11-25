//! Scrollable editor tabs with language badges and close buttons.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Instant;

use lgui::core::{
    Color, CompositingLayerAnimation, CompositingLayerSpec, CursorIcon, EventPolicy, IconStyle,
    LayerTransform, PhysicalRect, Point, PointerButton, Signal, UiElement, UiFocusHandle, UiId,
    WheelUnit, animated_compositing_layer, clip, precompiled,
};
use lgui::prelude::{Element, TextStyle, UiRect, VisualStyle, panel, text};
use lgui::text::{self, TextLayoutRequest};

use crate::domain::document::FileMeta;
use crate::domain::identity::ViewId;
use crate::domain::pane_layout::{Direction, PaneId};
use crate::domain::workspace::AppPage;
use crate::ui::chrome::tab_state::TabDrop;
use crate::ui::chrome::tabs_binding::{Binding, Command, Frame};
use crate::ui::components::tab_tooltip;
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
/// Extra distance beyond each viewport edge whose tabs are still built, so a
/// wheel step never reveals an empty gap before the next frame.
const RENDER_OVERSCAN: f32 = 240.0;

/// Upper bound for cached text widths; ellipsis probing adds short-lived
/// candidates, so the cache is reset rather than allowed to grow unbounded.
const MEASURE_CACHE_LIMIT: usize = 1_024;

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

/// `measure` without remembering the answer: for text measured once, such as every name in a
/// large tree, where remembering thousands of strings would cost more than it saves.
pub(crate) fn measure_once(s: &str, size: f32, weight: i32) -> f32 {
    let style = (size.to_bits(), weight, true);
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
    request.font_families = theme::MONO_FAMILIES;
    match text::layout(&request) {
        Some(layout) => layout.width,
        None => crate::ui::components::text::fallback_width(s, size),
    }
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
    allowed: bool,
) -> Element {
    let candidate =
        if allowed && app.editor.tabs.tab_drag.is_none() && app.editor.tabs.file_drag.is_none() {
            app.editor.tabs.tab_hovered.and_then(|(pane, view)| {
                tooltip(viewport, strips, app, pane, view).map(|tooltip| (pane, view, tooltip))
            })
        } else {
            None
        };
    let target = candidate
        .as_ref()
        .map(|&(pane, view, _)| (tab_id(pane, view), tab_close_id(pane, view)));
    let mut layer = Element::new(move |mut cx| {
        let hovered = target
            .filter(|(tab, close)| tab_tooltip::is_hovered(&cx, tab, close))
            .map(|(tab, _)| tab);
        let children = if tab_tooltip::ready(&cx, hovered).is_some() {
            cx.compile_deferred_children()
        } else {
            Vec::new()
        };
        UiElement::group(cx.id, UiRect::default()).children(children)
    })
    .defer_children_compile()
    .key("editor-tab-tooltip");
    if let Some((_, _, tooltip)) = candidate {
        layer = layer.child(tooltip);
    }
    layer
}

fn tooltip(
    viewport: UiRect,
    strips: &[(PaneId, UiRect)],
    app: &Frame<'_>,
    pane: PaneId,
    hovered: ViewId,
) -> Option<Element> {
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

fn active_tab_style() -> VisualStyle {
    VisualStyle::filled(theme::c().active_tab).alpha(51)
}

fn tab_id(pane: PaneId, view: ViewId) -> UiId {
    UiId::owned(format!("editor-tab-{}-{view:?}", pane.get()))
}

fn tab_close_id(pane: PaneId, view: ViewId) -> UiId {
    UiId::owned(format!("editor-tab-close-{}-{view:?}", pane.get()))
}

fn tab_panel(id: UiId, bounds: UiRect, style: VisualStyle) -> Element {
    let key = id.clone();
    Element::new(move |cx| UiElement::panel(id, bounds, style).children(cx.children))
        .key(key.as_str())
}

struct TabFace<'a> {
    label: &'a str,
    badge: TabBadge,
    mark: Option<(&'static str, Color)>,
    loading: bool,
    active: bool,
    preview: bool,
}

impl<'a> TabFace<'a> {
    fn new(
        app: &Frame<'_>,
        pane: PaneId,
        id: ViewId,
        meta: &'a FileMeta,
        label: Option<&'a String>,
        active: bool,
    ) -> Self {
        Self {
            label: label.map_or(meta.name.as_str(), String::as_str),
            badge: tab_badge(app, id, meta),
            mark: tab_state_mark(app, id),
            loading: app
                .workspace
                .view_document(id)
                .is_some_and(|id| app.workspace.loading_indicator(id)),
            active,
            preview: app.workspace.pane(pane).is_some_and(|p| p.is_preview(id)),
        }
    }

    fn render(self, tab: Element, pill: UiRect, key: &str) -> Element {
        let badge_left = pill.left + PAD;
        let name_left = badge_left + self.badge.width() + GAP;
        let close_left = tab_close_bounds(pill).left;
        let mark_width = self
            .mark
            .map_or(0.0, |(mark, _)| measure(mark, theme::SMALL, 400));
        let mark_left = close_left - GAP - mark_width;
        let name_right = if self.mark.is_some() {
            mark_left - GAP
        } else {
            close_left - GAP
        };
        let name_width = (name_right - name_left).max(0.0);
        let name = if self.active && measure_sans(self.label, theme::UI_SIZE, 400) <= name_width {
            self.label.to_owned()
        } else {
            ellipsize_sans(
                self.label,
                (name_width - TEXT_MARGIN).max(0.0),
                theme::UI_SIZE,
                400,
            )
        };
        let badge = if self.loading {
            let top = pill.top + (pill.height() - BADGE_ICON) / 2.0;
            crate::ui::components::loading_indicator::render(
                UiRect::new(badge_left, top, badge_left + BADGE_ICON, top + BADGE_ICON),
                theme::c().text_muted,
                format!("tabs.restore.{key}"),
            )
        } else {
            self.badge.render(badge_left, pill, key)
        };
        let mut tab = tab.child(badge).child(text(
            UiRect::new(name_left, pill.top, name_left + name_width, pill.bottom),
            name,
            tab_name_style(self.active, self.preview),
        ));
        if let Some((mark, color)) = self.mark {
            tab = tab.child(text(
                UiRect::new(
                    mark_left,
                    pill.top,
                    mark_left + mark_width + TEXT_MARGIN,
                    pill.bottom,
                ),
                mark,
                theme::mono_bold(color, theme::SMALL),
            ));
        }
        tab
    }
}

fn tab_close_bounds(pill: UiRect) -> UiRect {
    UiRect::new(
        pill.right - PAD - CLOSE_ICON,
        pill.top,
        pill.right - PAD,
        pill.bottom,
    )
}

fn tab_close_icon(pill: UiRect, key: &str) -> Element {
    close_glyph(
        UiId::owned(format!("tabs.close.{key}")),
        tab_close_bounds(pill),
        theme::c().text_dim,
    )
}

fn tab_right_edge(pill: UiRect) -> Element {
    Element::new(move |cx| {
        let scale = cx.context.scale();
        let edge = scale.physical_value(pill.right) - scale.physical_length(1.0);
        let line = scale.logical_rect(PhysicalRect::new(
            edge,
            scale.physical_value(pill.top),
            edge + scale.physical_length(1.0),
            scale.physical_value(pill.bottom),
        ));
        UiElement::panel(cx.id, line, VisualStyle::filled(theme::c().border))
            .hit_rect(UiRect::default())
    })
}

fn selected_tab_edge(pill: UiRect) -> Element {
    Element::new(move |cx| {
        let scale = cx.context.scale();
        let top = scale.physical_value(pill.top);
        let edge = scale.logical_rect(PhysicalRect::new(
            scale.physical_value(pill.left),
            top,
            scale.physical_value(pill.right),
            top + scale.physical_length(2.0),
        ));
        UiElement::panel(cx.id, edge, VisualStyle::filled(theme::c().accent))
            .hit_rect(UiRect::default())
    })
}

pub(crate) fn render_dragged_tab(app: &Frame<'_>, pointer: Signal<Point>) -> Option<Element> {
    let drag = app
        .editor
        .tabs
        .tab_drag
        .as_ref()
        .filter(|drag| drag.floating)?;
    let meta = app.workspace.view_meta(drag.source)?;
    let labels = app.workspace.tab_labels(drag.pane);
    let face = TabFace::new(
        app,
        drag.pane,
        drag.source,
        meta,
        labels.get(&drag.source),
        true,
    );
    let width = drag.preview_width;
    let grab = drag.grab;
    let rect = UiRect::new(0.0, 0.0, width, theme::TABS_H);
    let transform = move |point: Point| {
        CompositingLayerSpec::new().transform(
            LayerTransform::identity().translation(point.x - grab.x * width, point.y - grab.y),
        )
    };
    let initial = transform(pointer.get());
    let face = face
        .render(panel(rect, active_tab_style()), rect, "editor-dragged-tab")
        .child(tab_close_icon(rect, "editor-dragged-tab"));
    Some(
        Element::new(move |cx| {
            UiElement::compositing_layer(UiId::new("editor-dragged-tab"), rect, initial)
                .hit_rect(UiRect::default())
                .children(cx.children)
        })
        .key("editor-dragged-tab")
        .event_policy(EventPolicy::NONE)
        .bind(&pointer, move |point, node| {
            node.compositing_layer = Some(transform(*point));
        })
        // Include the strip color under the translucent selected-tab fill.
        .child(
            panel(rect, VisualStyle::filled(theme::c().sidebar))
                .child(face)
                .child(tab_right_edge(rect))
                .child(selected_tab_edge(rect)),
        ),
    )
}

/// The layer only moves horizontally. Its tab contents and layout stay retained
/// while animation frames repaint the previous and current positions.
#[derive(Clone, Default)]
struct TabSlide {
    offset: f32,
    from: f32,
    target: f32,
    started: Option<Instant>,
}

impl TabSlide {
    fn retarget(&mut self, target: f32) {
        if self.target != target {
            self.from = self.offset;
            self.target = target;
            self.started = (self.offset != target).then(Instant::now);
        }
    }
}

impl CompositingLayerAnimation for TabSlide {
    fn advance(&mut self, _elapsed_ms: f32) -> bool {
        let Some(started) = self.started else {
            return false;
        };
        let t = (started.elapsed().as_secs_f32() / 0.12).min(1.0);
        self.offset = self.target + (self.from - self.target) * (1.0 - t).powi(3);
        if t >= 1.0 {
            self.started = None;
        }
        true
    }

    fn compositing_layer_spec(&self) -> CompositingLayerSpec {
        CompositingLayerSpec::new()
            .transform(LayerTransform::identity().translation(self.offset, 0.0))
    }

    fn wants_frame(&self) -> bool {
        self.started.is_some()
    }
}

fn tab_motion(pill: UiRect, scroll_x: f32, target: Option<f32>) -> Element {
    Element::new(move |cx| {
        if let Some(target) = target {
            let scale = cx.context.scale();
            // Snap the layer's screen-space origin and size to device pixels.
            // Otherwise offscreen text shaping and texture sampling can shift
            // the glyphs when the layer is first mounted.
            let screen = pill.translate(-scroll_x, 0.0);
            let bounds = scale
                .logical_rect(scale.physical_rect_outward(screen))
                .translate(scroll_x, 0.0);
            let layer = cx.compile(animated_compositing_layer(
                bounds,
                move |motion: &mut TabSlide| motion.retarget(target),
            ));
            let offset = layer
                .node()
                .compositing_layer
                .map_or(0.0, |spec| spec.transform.translation_x());
            if target != 0.0 || offset != 0.0 {
                return layer.children(cx.children);
            }
        }
        // A stationary tab keeps the normal direct text paint path.
        UiElement::group(cx.id, pill)
            .hit_rect(UiRect::default())
            .children(cx.children)
    })
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
        tab_layer = tab_layer.child(panel(pill, active_tab_style()).child(text(
            UiRect::new(pill.left + PAD, pill.top, pill.right - PAD, pill.bottom),
            label,
            theme::mono(theme::c().text_bright, theme::UI_SIZE),
        )));
    }
    // Only tabs near the viewport become elements. The drag source is kept
    // even when edge auto-scroll moves it away so its keyed element survives
    // until the gesture ends.
    let open_files = pane_items(&s, pane);
    let drag = s.editor.tabs.tab_drag.as_ref();
    let active_drag = drag.filter(|drag| drag.active);
    let floating_source = active_drag.filter(|drag| drag.floating && drag.pane == pane);
    let tab_cursor = if active_drag.is_some_and(|drag| !drag.floating) {
        CursorIcon::Move
    } else {
        CursorIcon::Pointer
    };
    let source_index = drag
        .filter(|drag| drag.pane == pane)
        .and_then(|drag| open_files.iter().position(|id| *id == drag.source));
    let shift = floating_source.zip(source_index).and_then(|(drag, index)| {
        let width = layout.items.get(index)?.width + TAB_GAP;
        let target = drag.target_index.min(open_files.len().saturating_sub(1));
        if target < index {
            Some((target..index, width))
        } else {
            Some((index + 1..target + 1, -width))
        }
    });
    let visible = tab_layout::visible_range(
        &layout.items,
        layout.scroll_x,
        layout.viewport_width,
        RENDER_OVERSCAN,
    );
    let shifted_into_view = shift.iter().flat_map(|(indices, offset)| {
        let visible = visible.clone();
        tab_layout::visible_range(
            &layout.items,
            layout.scroll_x - offset,
            layout.viewport_width,
            RENDER_OVERSCAN,
        )
        .filter(move |index| indices.contains(index) && !visible.contains(index))
    });
    let pressed_outside_view = source_index.filter(|index| !visible.contains(index));
    let scroll_x = layout.scroll_x;
    for index in visible
        .clone()
        .chain(shifted_into_view)
        .chain(pressed_outside_view)
    {
        let Some(&id) = open_files.get(index) else {
            continue;
        };
        let Some(m) = s.workspace.view_meta(id) else {
            continue;
        };
        let Some(layout_item) = layout.items.get(index) else {
            continue;
        };
        let tab_w = layout_item.width;
        let x = rect.left + layout_item.left;

        let pill = UiRect::new(
            x,
            rect.top + PILL_INSET,
            x + tab_w,
            rect.bottom - PILL_INSET,
        );
        let st = state.clone();
        let focus = editor_focus.clone();
        // The active tab uses a neutral background and an accent bottom edge.
        let lifted = source_index == Some(index) && floating_source.is_some();
        let style = if lifted {
            VisualStyle::default()
        } else if Some(id) == active {
            active_rect = Some(pill);
            active_tab_style()
        } else {
            VisualStyle::default().radius(2.0)
        };
        let mut tab_el = tab_panel(tab_id(pane, id), pill, style)
            .event_policy(EventPolicy::INTERACTIVE)
            .cursor(tab_cursor)
            .on_pointer_down_with_button(move |cx, pointer, button| {
                match button {
                    PointerButton::Left => {
                        let point = pointer.point;
                        let grab = Point::new(
                            ((point.x - pill.left + scroll_x) / pill.width()).clamp(0.0, 1.0),
                            point.y - pill.top,
                        );
                        st.run(Command::Press(pane, id, index, point, grab));
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
        let motion_key = format!("editor-tab-slide-{}-{id:?}", pane.get());
        if lifted {
            // The original hit node still owns the gesture, even when its
            // visuals have moved to the window overlay or it scrolls out.
            // Pointer drag events invalidate its paint bounds, so keep them empty.
            let no_paint = UiRect::new(pill.left, pill.top, pill.left, pill.top);
            tab_layer = tab_layer.child(
                tab_motion(pill, scroll_x, None)
                    .key(motion_key)
                    .child(tab_el.paint_bounds(no_paint)),
            );
            continue;
        }

        let visual_key = format!("{}-{id:?}", pane.get());
        let face = TabFace::new(&s, pane, id, m, labels.get(&id), Some(id) == active);
        tab_el = face.render(tab_el, pill, &visual_key);

        // The close slot is always reserved by the layout, but the control is
        // only painted and hit-testable for the active or hovered tab.
        if Some(id) == active || s.editor.tabs.tab_hovered == Some((pane, id)) {
            let st = state.clone();
            let focus = editor_focus.clone();
            tab_el = tab_el.child(
                tab_panel(
                    tab_close_id(pane, id),
                    tab_close_bounds(pill),
                    VisualStyle::default(),
                )
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
                .child(tab_close_icon(pill, &visual_key)),
            );
        }

        // Tabs are divided by a 1px edge on their right side, snapped to
        // physical pixels so fractional tab widths never blur it.
        let separator = tab_right_edge(pill);
        let offset = floating_source.map(|_| {
            shift
                .as_ref()
                .filter(|(indices, _)| indices.contains(&index))
                .map_or(0.0, |(_, offset)| *offset)
        });
        tab_layer = tab_layer.child(
            tab_motion(pill, scroll_x, offset)
                .key(motion_key)
                .child(tab_el)
                .child(separator),
        );
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

    // Tab drags outside their source strip and files from the drawer use the
    // insertion marker when the drop target is a tab strip.
    let file_drop = s
        .editor
        .tabs
        .file_drag
        .as_ref()
        .filter(|drag| drag.active)
        .and_then(|drag| drag.drop);
    let marker = active_drag
        .filter(|drag| !drag.floating)
        .map(|drag| (Some(drag), drag.drop))
        .or(file_drop.map(|drop| (None, Some(drop))))
        .and_then(|(drag, drop)| match drop {
            None => {
                let (drag, index) = drag.zip(source_index)?;
                let target = layout.items.get(drag.target_index)?;
                (drag.target_index != index).then_some(if drag.target_index < index {
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

    // Paint the separator after the backgrounds and fades to keep the edge crisp.
    bar = bar.child(crate::ui::components::divider::bottom(rect));
    if let Some(active) = active_rect {
        // Decoration only: a hit-testable clip over the whole viewport would
        // sit in front of the tabs and hide their cursor.
        let scroll_x = layout.scroll_x;
        bar = bar.child(
            Element::new(move |cx| {
                UiElement::clip(cx.id, tab_viewport, -scroll_x, 0.0)
                    .hit_rect(UiRect::default())
                    .children(cx.children)
            })
            .child(selected_tab_edge(active)),
        );
    }

    bar
}
