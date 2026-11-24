//! Animate only the painted caret. Editing and IME use the logical position.
use lgui::core::{
    CompositingLayerAnimation, CompositingLayerSpec, LayerTransform, UiElement, UiId, UiScale,
    animated_compositing_layer,
};
use lgui::prelude::{Element, UiRect, VisualStyle};
use loog_editor_core::{text::Selection, vim::CursorShape};
use std::time::Instant;

use crate::domain::identity::PaneId;
use crate::domain::identity::ViewId;
use crate::ui::fonts;

const DURATION_MS: f32 = 80.0;
const FRAME_INTERVAL_MS: u64 = 8;
const BLINK_INTERVAL_MS: f32 = 500.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct CaretContext {
    pub pane: PaneId,
    pub document: ViewId,
    pub viewport: UiRect,
    pub scroll: (f32, f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct BlinkActivity {
    pub document_version: u64,
    pub selection: Selection,
    pub shape: CursorShape,
}

/// Paints `shape` and animates its character cell. Motion follows `cell`, the
/// top-left of the character cell, so switching between bar, block and
/// underline shapes changes the painted rect without a vertical slide.
pub(super) fn render(
    shape: UiRect,
    cell: [f32; 2],
    style: VisualStyle,
    context: CaretContext,
    animate: bool,
    blink: Option<BlinkActivity>,
) -> Element {
    Element::new(move |cx| {
        let scale = cx.context.scale();
        let target = viewport_rect(shape, context.scroll, scale);
        let blink = blink.filter(|_| target.intersect(context.viewport).is_some());
        // Snap the cell like the painted rect, or the first frame jumps a subpixel.
        let snap_scrolled = |value: f32, scroll: f32| snap(value - scroll, scale);
        let cell = [
            snap_scrolled(cell[0], context.scroll.0),
            snap_scrolled(cell[1], context.scroll.1),
        ];
        let id = UiId::owned(format!("editor-caret-{}", context.pane.get()));
        let fill_id = UiId::owned(format!("{id}-fill"));
        // The physical layer size can round slightly above an integer and
        // allocate one extra texel. Fill it so resampling the layer cannot
        // replace a caret pixel with transparency. The layer bounds clip it.
        let texel = scale.logical_value(1);
        let fill = UiRect::new(
            target.left,
            target.top,
            target.right + texel,
            target.bottom + texel,
        );
        // The framework applies layer transforms and opacity directly on
        // animation frames, without executing the app view.
        cx.compile(
            animated_compositing_layer(target, move |motion: &mut CaretMotion| {
                let scale_changed = motion.scale != Some(scale);
                let changed =
                    scale_changed || motion.context != Some(context) || motion.target != cell;
                motion.blink.configure(blink, changed, Instant::now());
                motion.scale = Some(scale);
                motion.retarget(cell, context, animate && !scale_changed);
            })
            .child(Element::new(move |_| {
                UiElement::group(id, target).child(UiElement::panel(fill_id, fill, style))
            })),
        )
    })
    .key("editor-caret")
}

/// The thin insertion bar used by the default editor and Vim insert mode.
pub(super) fn bar(cell: [f32; 2]) -> UiRect {
    UiRect::new(
        cell[0],
        cell[1],
        cell[0] + 2.0,
        cell[1] + fonts::editor().line_h,
    )
}

pub(super) fn secondary_bar(cell: [f32; 2], scroll: (f32, f32), style: VisualStyle) -> Element {
    let shape = bar(cell);
    Element::new(move |cx| {
        let target = viewport_rect(shape, scroll, cx.context.scale());
        UiElement::panel(cx.id, target, style)
    })
}

fn viewport_rect(shape: UiRect, (x, y): (f32, f32), scale: UiScale) -> UiRect {
    pixel_aligned(shape.translate(-x, -y), scale)
}

#[derive(Clone, Default)]
struct CaretBlink {
    activity: Option<BlinkActivity>,
    phase_ms: f32,
    started_at: Option<Instant>,
}

impl CaretBlink {
    fn configure(&mut self, activity: Option<BlinkActivity>, moved: bool, now: Instant) {
        if self.activity != activity || moved {
            self.activity = activity;
            self.phase_ms = 0.0;
            self.started_at = activity.map(|_| now);
        }
    }

    fn visible(&self) -> bool {
        self.phase_ms < BLINK_INTERVAL_MS
    }

    fn advance(&mut self, elapsed_ms: f32, now: Instant) -> bool {
        if self.activity.is_none() {
            return false;
        }
        let visible = self.visible();
        // A pending window tick can predate the activity that restarted blinking.
        let elapsed_ms = self.started_at.take().map_or(elapsed_ms, |started| {
            elapsed_ms.min(now.saturating_duration_since(started).as_secs_f32() * 1000.0)
        });
        self.phase_ms = (self.phase_ms + elapsed_ms.max(0.0)) % (2.0 * BLINK_INTERVAL_MS);
        visible != self.visible()
    }

    fn next_toggle_ms(&self) -> u64 {
        (BLINK_INTERVAL_MS - self.phase_ms % BLINK_INTERVAL_MS).ceil() as u64
    }
}

#[derive(Clone, Default)]
struct CaretMotion {
    context: Option<CaretContext>,
    scale: Option<UiScale>,
    position: [f32; 2],
    from: [f32; 2],
    target: [f32; 2],
    elapsed_ms: f32,
    first_frame_at: Option<Instant>,
    moving: bool,
    blink: CaretBlink,
}

impl CaretMotion {
    fn retarget(&mut self, target: [f32; 2], context: CaretContext, animate: bool) -> [f32; 2] {
        if !animate || self.context != Some(context) {
            self.context = Some(context);
            self.position = target;
            self.from = target;
            self.target = target;
            self.moving = false;
            self.first_frame_at = None;
        } else if self.target != target {
            // Retarget from the displayed position; repeated input never queues animations.
            self.from = self.position;
            self.target = target;
            self.elapsed_ms = 0.0;
            self.first_frame_at = Some(Instant::now());
            self.moving = self.from != self.target;
        }
        self.position
    }
}

impl CaretMotion {
    fn advance_at(&mut self, elapsed_ms: f32, now: Instant) -> bool {
        let blink_changed = self.blink.advance(elapsed_ms, now);
        if !self.moving {
            return blink_changed;
        }
        // lgui's first tick includes time spent idle before this animation began.
        // Count only time since the latest target changed, then use frame deltas.
        let elapsed_ms = self.first_frame_at.take().map_or(elapsed_ms, |started| {
            elapsed_ms.min(now.saturating_duration_since(started).as_secs_f32() * 1000.0)
        });
        self.elapsed_ms = (self.elapsed_ms + elapsed_ms.max(0.0)).min(DURATION_MS);
        let eased = css_ease(self.elapsed_ms / DURATION_MS);
        self.position = std::array::from_fn(|axis| {
            self.from[axis] + (self.target[axis] - self.from[axis]) * eased
        });
        if self.elapsed_ms >= DURATION_MS {
            self.position = self.target;
            self.moving = false;
        }
        true
    }
}

impl CompositingLayerAnimation for CaretMotion {
    fn compositing_layer_spec(&self) -> CompositingLayerSpec {
        // Keep continuous coordinates in flight; the resting target is pixel aligned.
        CompositingLayerSpec::new()
            .opacity(if self.blink.visible() { 1.0 } else { 0.0 })
            .transform(LayerTransform::identity().translation(
                self.position[0] - self.target[0],
                self.position[1] - self.target[1],
            ))
    }
    fn wants_frame(&self) -> bool {
        self.moving || self.blink.activity.is_some()
    }
    fn frame_interval_ms(&self) -> u64 {
        if self.moving || self.blink.activity.is_none() {
            FRAME_INTERVAL_MS
        } else {
            self.blink.next_toggle_ms()
        }
    }
    fn advance(&mut self, elapsed_ms: f32) -> bool {
        self.advance_at(elapsed_ms, Instant::now())
    }
}

// VS Code uses an 80 ms CSS transition with the default ease curve:
// cubic-bezier(0.25, 0.1, 0.25, 1). Solve x(t) before evaluating y(t).
fn css_ease(progress: f32) -> f32 {
    if progress <= 0.0 {
        return 0.0;
    }
    if progress >= 1.0 {
        return 1.0;
    }
    let bezier = |t: f32, a, b| {
        let remaining = 1.0 - t;
        3.0 * remaining * remaining * t * a + 3.0 * remaining * t * t * b + t * t * t
    };
    let (mut low, mut high) = (0.0, 1.0);
    for _ in 0..16 {
        let t = (low + high) / 2.0;
        if bezier(t, 0.25, 0.25) < progress {
            low = t;
        } else {
            high = t;
        }
    }
    bezier((low + high) / 2.0, 0.1, 1.0)
}

fn snap(value: f32, scale: UiScale) -> f32 {
    scale.physical_value(value) as f32 / scale.factor()
}

fn pixel_aligned(rect: UiRect, scale: UiScale) -> UiRect {
    let left = snap(rect.left, scale);
    // Snap the width rather than the right edge so a bar keeps a constant width.
    // Drop float error first: `x + 2.0 - x` may be 1.9999, which rounds down at 125%.
    let width = (rect.width() * 64.0).round() / 64.0;
    UiRect::new(
        left,
        snap(rect.top, scale),
        left + scale.physical_length(width) as f32 / scale.factor(),
        snap(rect.bottom, scale),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caret_at(rect: UiRect, context: CaretContext) -> Element {
        let style = VisualStyle::filled(crate::ui::theme::c().accent);
        render(rect, [rect.left, rect.top], style, context, true, None)
    }

    fn context() -> CaretContext {
        CaretContext {
            pane: PaneId::new(1),
            document: ViewId::new(1),
            viewport: UiRect::new(0.0, 0.0, 500.0, 300.0),
            scroll: (0.0, 0.0),
        }
    }

    #[test]
    fn retained_animation_updates_only_caret_and_stops_requesting_frames() {
        use lgui::application::AppView;
        use lgui::prelude::{State, component, group};
        use lgui::session::UiSession;
        use std::sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        };

        let exposed = Arc::new(Mutex::new(None::<State<UiRect>>));
        let output = exposed.clone();
        let renders = Arc::new(AtomicUsize::new(0));
        let render_count = renders.clone();
        let viewport = context().viewport;
        let view: AppView = Arc::new(move |_| {
            let output = output.clone();
            let render_count = render_count.clone();
            component(viewport, move |cx, _| {
                render_count.fetch_add(1, Ordering::Relaxed);
                let target = cx.state(UiRect::new(10.0, 3.0, 12.0, 23.0));
                *output.lock().unwrap() = Some(target.clone());
                group(viewport).child(caret_at(target.get(), context()))
            })
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let target = exposed.lock().unwrap().clone().unwrap();
        let caret_id = UiId::new("editor-caret-1");
        let left = |session: &UiSession| {
            let node = session.tree().node(&caret_id).unwrap();
            let layer = session.tree().node(node.parent.as_ref().unwrap()).unwrap();
            node.layout_rect.left + layer.compositing_layer.unwrap().transform.translation_x()
        };
        assert_eq!(left(&session), 10.0);
        target.update(|target| *target = UiRect::new(110.0, 43.0, 112.0, 63.0));
        session.render_view(&view, viewport, UiScale::ONE);
        assert_eq!(left(&session), 10.0);
        assert_eq!(session.runtime().frame_interval_ms(), Some(8));
        let root_renders = renders.load(Ordering::Relaxed);
        session.advance(0.0);
        session.advance(20.0);
        session.render_view(&view, viewport, UiScale::ONE);
        assert!(left(&session) > 10.0 && left(&session) < 110.0);
        session.advance(60.0);
        session.render_view(&view, viewport, UiScale::ONE);
        assert_eq!(left(&session), 110.0);
        session.advance(16.0);
        session.render_view(&view, viewport, UiScale::ONE);
        assert!(session.runtime().frame_interval_ms().is_none());
        assert_eq!(
            renders.load(Ordering::Relaxed),
            root_renders,
            "animation must not rebuild the application"
        );
    }

    #[test]
    fn animated_caret_paints_crisp_pixels_and_clears_old_positions_with_partial_damage() {
        use lgui::application::AppView;
        use lgui::core::{PhysicalRect, clip};
        use lgui::prelude::{State, component, group};
        use lgui::render_skia::SkiaSoftwareSurface;
        use lgui::renderer::{FrameInfo, FrameReason};
        use lgui::session::UiSession;
        use std::sync::{Arc, Mutex};

        let viewport = UiRect::new(0.0, 0.0, 128.0, 64.0);
        for factor in [1.0_f32, 1.25, 1.5, 2.0] {
            let scale = UiScale::new(factor);
            let exposed = Arc::new(Mutex::new(None::<State<UiRect>>));
            let output = exposed.clone();
            let mut context = context();
            context.viewport = viewport;
            context.scroll = (0.37, 0.63);
            let view: AppView = Arc::new(move |_| {
                let output = output.clone();
                component(viewport, move |cx, _| {
                    let target = cx.state(UiRect::new(30.37, 8.63, 32.37, 32.63));
                    *output.lock().unwrap() = Some(target.clone());
                    group(viewport)
                        .child(clip(viewport, 0.0, 0.0).child(caret_at(target.get(), context)))
                })
            });
            let mut session = UiSession::new();
            let mut surface = SkiaSoftwareSurface::new(1024 * 1024);
            let mut commit = session.render_view(&view, viewport, scale);
            let target = exposed.lock().unwrap().clone().unwrap();
            let size = (
                (128.0 * factor).round() as usize,
                (64.0 * factor).round() as usize,
            );
            for frame_index in 0..4 {
                if frame_index == 1 {
                    target.update(|target| *target = UiRect::new(90.37, 8.63, 92.37, 32.63));
                    commit = session.render_view(&view, viewport, scale);
                    session.advance(0.0);
                } else if frame_index > 1 {
                    session.advance(40.0);
                    commit = session.render_view(&view, viewport, scale);
                }
                let dirty = commit
                    .damage
                    .dirty
                    .effective_rects()
                    .into_iter()
                    .map(|rect| scale.physical_rect(rect))
                    .collect::<Vec<_>>();
                let frame = FrameInfo::new(
                    PhysicalRect::new(0, 0, size.0 as i32, size.1 as i32),
                    &dirty,
                    scale,
                    FrameReason::SceneChange,
                    frame_index == 0,
                );
                surface
                    .draw(&session.tree().scene().project_to_physical(scale), &frame)
                    .unwrap();
                let y = (16.0 * factor).round() as usize;
                let painted = (0..size.0)
                    .filter_map(|x| {
                        let alpha = surface.pixels()[(y * size.0 + x) * 4 + 3];
                        (alpha > 0).then_some((x, alpha))
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    painted.len(),
                    scale.physical_length(2.0) as usize,
                    "scale={factor}, frame={frame_index}, painted={painted:?}"
                );
                assert!(
                    painted.iter().all(|(_, alpha)| *alpha == 255),
                    "caret edges must remain sharp: {painted:?}"
                );
                let node = session.tree().node(&UiId::new("editor-caret-1")).unwrap();
                let layer = session.tree().node(node.parent.as_ref().unwrap()).unwrap();
                let expected = scale.physical_value(
                    node.layout_rect.left
                        + layer.compositing_layer.unwrap().transform.translation_x(),
                ) as usize;
                assert_eq!(
                    painted[0].0, expected,
                    "no caret may remain at an old position"
                );
            }
        }
    }

    #[test]
    fn insertion_bar_fills_its_pixel_width_and_the_entire_row() {
        use lgui::application::AppView;
        use lgui::core::clip;
        use lgui::prelude::group;
        use lgui::render_skia::SkiaSoftwareSurface;
        use lgui::renderer::{FrameInfo, FrameReason};
        use lgui::session::UiSession;
        use std::sync::Arc;

        let viewport = UiRect::new(0.0, 0.0, 80.0, 64.0);
        let line_h = fonts::editor().line_h;
        let mut context = context();
        context.viewport = viewport;
        for (factor, (sx, sy)) in [
            (0.75, (0.0, 0.0)),
            (0.8, (0.37, 0.63)),
            (1.25, (96.83, 811.27)),
        ] {
            let scale = UiScale::new(factor);
            context.scroll = (sx, sy);
            let physical = scale.physical_rect(viewport);
            let mut session = UiSession::new();
            let mut surface = SkiaSoftwareSurface::new(1024 * 1024);
            for x in [24.37, 34.27, 43.75] {
                let cell = [x + sx, 16.91 + sy];
                let view: AppView = Arc::new(move |_| {
                    let accent = VisualStyle::filled(crate::ui::theme::c().accent);
                    group(viewport).child(clip(viewport, 0.0, 0.0).child(render(
                        bar(cell),
                        cell,
                        accent,
                        context,
                        false,
                        None,
                    )))
                });
                let scene = session
                    .render_view(&view, viewport, scale)
                    .scene
                    .project_to_physical(scale);
                let frame = FrameInfo::new(physical, &[], scale, FrameReason::SceneChange, true);
                surface.draw(&scene, &frame).unwrap();

                let top = scale.physical_value(cell[1] - sy);
                let bottom = scale.physical_value(cell[1] + line_h - sy);
                let left = scale.physical_value(cell[0] - sx);
                let right = left + scale.physical_length(2.0);
                for y in top - 1..=bottom {
                    for x in left - 1..=right {
                        let offset = (y * physical.width() + x) as usize * 4;
                        let inside = (left..right).contains(&x) && (top..bottom).contains(&y);
                        assert_eq!(
                            surface.pixels()[offset + 3],
                            if inside { 255 } else { 0 },
                            "scale={factor}, cell={cell:?}, pixel=({x}, {y})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn blink_restarts_visible_after_activity_and_can_be_paused() {
        use std::time::Duration;

        let start = Instant::now();
        let mut blink = CaretBlink::default();
        let mut activity = BlinkActivity {
            document_version: 1,
            selection: Selection::caret(7),
            shape: CursorShape::Bar,
        };
        blink.configure(Some(activity), false, start);
        assert!(!blink.advance(1000.0, start + Duration::from_millis(100)));
        assert_eq!(blink.next_toggle_ms(), 400);
        assert!(blink.advance(400.0, start + Duration::from_millis(500)));
        assert!(!blink.visible());
        blink.configure(Some(activity), false, start + Duration::from_millis(600));
        assert!(!blink.visible(), "unchanged renders do not reset blinking");
        activity.document_version += 1;
        blink.configure(Some(activity), false, start + Duration::from_millis(600));
        assert!(blink.visible());
        assert_eq!(blink.next_toggle_ms(), 500);
        blink.configure(None, false, start + Duration::from_millis(650));
        assert!(!blink.advance(600.0, start + Duration::from_millis(1250)));
        assert!(blink.visible());
    }

    #[test]
    fn blinking_each_cursor_shape_only_repaints_the_scrolled_caret() {
        use lgui::application::AppView;
        use lgui::core::clip;
        use lgui::prelude::{Color, component, panel};
        use lgui::render_skia::SkiaSoftwareSurface;
        use lgui::renderer::{FrameInfo, FrameReason};
        use lgui::session::UiSession;
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        let viewport = UiRect::new(0.0, 0.0, 192.0, 96.0);
        let scale = UiScale::new(0.9);
        let physical = scale.physical_rect(viewport);
        let mut context = context();
        context.viewport = viewport;
        context.scroll = (96.83, 811.27);
        let cell = [context.scroll.0 + 27.48, context.scroll.1 + 20.82];
        let [x, y] = cell;
        let line_h = fonts::editor().line_h;
        for (shape, bounds) in [
            (CursorShape::Bar, bar(cell)),
            (
                CursorShape::Block,
                UiRect::new(x, y + 1.0, x + 8.0, y + line_h - 1.0),
            ),
            (
                CursorShape::Underline,
                UiRect::new(x, y + line_h - 3.0, x + 8.0, y + line_h - 1.0),
            ),
            (
                CursorShape::HalfBlock,
                UiRect::new(x, y + line_h / 2.0, x + 8.0, y + line_h - 1.0),
            ),
        ] {
            let activity = BlinkActivity {
                document_version: 1,
                selection: Selection::caret(5),
                shape,
            };
            let renders = Arc::new(AtomicUsize::new(0));
            let counter = renders.clone();
            let view: AppView = Arc::new(move |_| {
                let counter = counter.clone();
                component(viewport, move |_, _| {
                    counter.fetch_add(1, Ordering::Relaxed);
                    let style = VisualStyle::filled(Color(0x818cf8));
                    let caret = render(bounds, cell, style, context, false, Some(activity));
                    panel(viewport, VisualStyle::filled(Color(0x14161b)))
                        .child(clip(viewport, 0.0, 0.0).child(caret))
                })
            });
            let mut session = UiSession::new();
            let mut surface = SkiaSoftwareSurface::new_opaque(1024 * 1024);
            let first = session.render_view(&view, viewport, scale);
            let frame = FrameInfo::new(physical, &[], scale, FrameReason::SceneChange, true);
            surface
                .draw(&first.scene.project_to_physical(scale), &frame)
                .unwrap();
            let expected = scale.physical_rect(viewport_rect(bounds, context.scroll, scale));
            let pixel = (expected.top * physical.width() + expected.left) as usize * 4;
            let original = surface.pixels().to_vec();
            assert_eq!(&original[pixel..pixel + 3], &[0xf8, 0x8c, 0x81]);
            assert_eq!(session.runtime().frame_interval_ms(), Some(500));
            session.advance(0.0);
            assert!(!session.advance(499.0).animation_changed);
            assert_eq!(session.runtime().frame_interval_ms(), Some(1));
            for (elapsed, color) in [(1.0, [0x1b, 0x16, 0x14]), (500.0, [0xf8, 0x8c, 0x81])] {
                let tick = session.advance(elapsed);
                let changed = tick.dirty_bounds.expect("blink must damage the caret");
                assert_eq!(scale.physical_rect(changed), expected, "{shape:?}");
                session.invalidations_mut().invalidate_rect(changed);
                let commit = session.render_view(&view, viewport, scale);
                assert!(!commit.damage.dirty.is_full());
                let damage = commit
                    .damage
                    .dirty
                    .effective_rects()
                    .into_iter()
                    .map(|rect| scale.physical_rect_outward(rect))
                    .collect::<Vec<_>>();
                let local = scale.physical_rect_outward(changed.inflate(2.0, 2.0));
                assert_eq!(damage, [local], "{shape:?}");
                let frame =
                    FrameInfo::new(physical, &damage, scale, FrameReason::SceneChange, false);
                surface
                    .draw(&commit.scene.project_to_physical(scale), &frame)
                    .unwrap();
                assert!(!surface.painted_full_frame());
                assert_eq!(&surface.pixels()[pixel..pixel + 3], &color);
            }
            assert_eq!(original, surface.pixels());
            assert_eq!(renders.load(Ordering::Relaxed), 1);
        }
    }

    fn step(motion: &mut CaretMotion, elapsed_ms: f32) -> bool {
        let now = motion.first_frame_at.unwrap_or_else(Instant::now)
            + std::time::Duration::from_secs_f32(elapsed_ms / 1000.0);
        motion.advance_at(elapsed_ms, now)
    }

    #[test]
    fn first_frame_excludes_idle_time_and_real_stalls_still_complete() {
        let mut motion = CaretMotion::default();
        motion.retarget([0.0, 0.0], context(), true);
        motion.retarget([10.0, 0.0], context(), true);
        let started = motion.first_frame_at.unwrap();
        motion.advance_at(5000.0, started + std::time::Duration::from_millis(8));
        assert_eq!(motion.elapsed_ms, 8.0);
        assert!(motion.position[0] > 0.0 && motion.position[0] < 2.0);
        assert!(motion.wants_frame());
        motion.retarget([20.0, 0.0], context(), true);
        let started = motion.first_frame_at.unwrap();
        motion.advance_at(5000.0, started + std::time::Duration::from_millis(8));
        assert_eq!(
            motion.elapsed_ms, 8.0,
            "each retarget must reset the animation clock"
        );
        motion.advance_at(200.0, started + std::time::Duration::from_millis(208));
        assert_eq!(motion.position, [20.0, 0.0]);
        assert!(!motion.wants_frame());
    }

    #[test]
    fn short_moves_keep_subpixel_positions_and_use_css_ease_at_eight_ms_intervals() {
        let mut motion = CaretMotion::default();
        motion.retarget([0.0, 0.0], context(), true);
        motion.retarget([8.0, 0.0], context(), true);
        step(&mut motion, 8.0);
        let displayed = 8.0 + motion.compositing_layer_spec().transform.translation_x();
        assert!(
            displayed > 0.0 && displayed < 1.0,
            "first frame must not jump half a character: {displayed}"
        );
        assert!((displayed - displayed.round()).abs() > 0.01);
        assert_eq!(motion.frame_interval_ms(), 8);
        assert!((css_ease(0.25) - 0.40851).abs() < 0.0001);
        assert!((css_ease(0.5) - 0.80240).abs() < 0.0001);
        assert_eq!(css_ease(0.0), 0.0);
        assert_eq!(css_ease(1.0), 1.0);
    }

    #[test]
    fn motion_eases_to_target_and_stops_in_eighty_ms() {
        let mut motion = CaretMotion::default();
        assert_eq!(motion.retarget([10.0, 3.0], context(), true), [10.0, 3.0]);
        assert!(!motion.wants_frame());
        assert_eq!(motion.retarget([110.0, 43.0], context(), true), [10.0, 3.0]);
        let mut previous = motion.position;
        for _ in 0..4 {
            assert!(step(&mut motion, 20.0));
            for (axis, &previous_position) in previous.iter().enumerate() {
                assert!(
                    motion.position[axis] > previous_position
                        && motion.position[axis] <= motion.target[axis]
                );
            }
            previous = motion.position;
        }
        assert_eq!(motion.position, [110.0, 43.0]);
        assert!(!motion.wants_frame());
        assert!(!step(&mut motion, 16.0));
    }

    #[test]
    fn repeated_input_retargets_from_current_position_without_jumping() {
        let mut motion = CaretMotion::default();
        motion.retarget([0.0, 0.0], context(), true);
        motion.retarget([100.0, 50.0], context(), true);
        step(&mut motion, 20.0);
        let displayed = motion.position;
        assert_eq!(motion.retarget([200.0, 0.0], context(), true), displayed);
        assert_eq!(motion.from, displayed);
        step(&mut motion, 80.0);
        assert_eq!(motion.position, [200.0, 0.0]);
        assert!(!motion.wants_frame());
    }

    #[test]
    fn scroll_document_pane_layout_and_disabled_motion_snap_immediately() {
        let original = context();
        let mut scrolled = original;
        scrolled.scroll = (4.0, 40.0);
        let mut document = original;
        document.document = ViewId::new(2);
        let mut pane = original;
        pane.pane = PaneId::new(2);
        let mut resized = original;
        resized.viewport.right += 50.0;
        for (next, enabled) in [
            (scrolled, true),
            (document, true),
            (pane, true),
            (resized, true),
            (original, false),
        ] {
            let mut motion = CaretMotion::default();
            motion.retarget([0.0, 0.0], original, true);
            motion.retarget([100.0, 0.0], original, true);
            step(&mut motion, 20.0);
            assert_eq!(motion.retarget([200.0, 40.0], next, enabled), [200.0, 40.0]);
            assert!(!motion.wants_frame());
        }
    }

    #[test]
    fn painting_aligns_both_edges_at_fractional_display_scales() {
        for factor in [1.0, 1.25, 1.5, 2.0] {
            let scale = UiScale::new(factor);
            let rect = pixel_aligned(UiRect::new(23.37, 9.63, 25.37, 31.63), scale);
            for value in [rect.left, rect.right, rect.top, rect.bottom] {
                assert!((value * factor - (value * factor).round()).abs() < 0.0001);
            }
        }
    }
}
