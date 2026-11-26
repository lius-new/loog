//! Retained two-dimensional motion for the layout miniature.
use lgui::core::{
    CompositingLayerAnimation, CompositingLayerSpec, EventPolicy, LayerTransform, Point, Signal,
    UiElement, UiId, animated_compositing_layer,
};
use lgui::prelude::{Element, UiRect};
use std::time::Instant;

pub(super) fn slide(id: u64, target: UiRect, child: Element) -> Element {
    animated_compositing_layer(target, move |motion: &mut Slide| motion.retarget(target))
        .key(format!("layout-region-motion-{id}"))
        .event_policy(EventPolicy::NONE)
        .child(child)
}

pub(super) fn floating(
    bounds: UiRect,
    area: UiRect,
    pressed: Point,
    pointer: Signal<Point>,
) -> Element {
    let transform = move |point: Point| {
        let left = (bounds.left + point.x - pressed.x)
            .clamp(area.left, (area.right - bounds.width()).max(area.left));
        let top = (bounds.top + point.y - pressed.y)
            .clamp(area.top, (area.bottom - bounds.height()).max(area.top));
        CompositingLayerSpec::new()
            .transform(LayerTransform::identity().translation(left - bounds.left, top - bounds.top))
    };
    let initial = transform(pointer.get());
    Element::new(move |cx| {
        UiElement::compositing_layer(UiId::new("layout-region-floating"), bounds, initial)
            .hit_rect(UiRect::default())
            .children(cx.children)
    })
    .key("layout-region-floating")
    .event_policy(EventPolicy::NONE)
    .bind(&pointer, move |point, node| {
        node.compositing_layer = Some(transform(*point))
    })
}

const DURATION_MS: f32 = 140.0;

#[derive(Clone, Default)]
struct Slide {
    current: Option<UiRect>,
    from: UiRect,
    target: UiRect,
    started: Option<Instant>,
}

impl Slide {
    fn retarget(&mut self, target: UiRect) {
        let Some(current) = self.current else {
            self.current = Some(target);
            self.target = target;
            self.started = None;
            return;
        };
        if self.target != target {
            self.from = current;
            self.target = target;
            self.started = Some(Instant::now());
        }
    }

    fn advance_at(&mut self, now: Instant) -> bool {
        let Some(started) = self.started else {
            return false;
        };
        let t =
            (now.saturating_duration_since(started).as_secs_f32() * 1000.0 / DURATION_MS).min(1.0);
        if t >= 1.0 {
            self.current = Some(self.target);
            self.started = None;
            return true;
        }
        let eased = 1.0 - (1.0 - t).powi(3);
        let lerp = |from: f32, to: f32| from + (to - from) * eased;
        self.current = Some(UiRect::new(
            lerp(self.from.left, self.target.left),
            lerp(self.from.top, self.target.top),
            lerp(self.from.right, self.target.right),
            lerp(self.from.bottom, self.target.bottom),
        ));
        true
    }
}

impl CompositingLayerAnimation for Slide {
    fn advance(&mut self, _elapsed_ms: f32) -> bool {
        // The native clock may include a long idle interval before this gesture.
        self.advance_at(Instant::now())
    }

    fn compositing_layer_spec(&self) -> CompositingLayerSpec {
        let current = self.current.unwrap_or(self.target);
        CompositingLayerSpec::new().transform(
            LayerTransform::identity()
                .origin(0.0, 0.0)
                .scale_xy(
                    current.width() / self.target.width().max(1.0),
                    current.height() / self.target.height().max(1.0),
                )
                .translation(
                    current.left - self.target.left,
                    current.top - self.target.top,
                ),
        )
    }

    fn wants_frame(&self) -> bool {
        self.started.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swaps_animate_both_axes_and_sizes_and_retarget_without_jumping() {
        let a = UiRect::new(0.0, 0.0, 80.0, 150.0);
        let b = UiRect::new(100.0, 160.0, 380.0, 200.0);
        let mut slide = Slide::default();
        slide.retarget(a);
        assert!(!slide.wants_frame());
        slide.retarget(b);
        assert_eq!(slide.current, Some(a));
        assert!(slide.wants_frame());
        let started = slide.started.unwrap();
        assert!(slide.advance(10_000.0));
        assert!(
            slide.wants_frame(),
            "an idle native clock must not finish a new transition"
        );
        assert!(slide.advance_at(started + std::time::Duration::from_millis(40)));
        let mid = slide.current.unwrap();
        assert!(mid.left > a.left && mid.left < b.left);
        assert!(mid.top > a.top && mid.top < b.top);
        assert!(mid.width() > a.width() && mid.width() < b.width());
        slide.retarget(a);
        assert_eq!(slide.current, Some(mid));
        assert!(slide.advance_at(slide.started.unwrap() + std::time::Duration::from_millis(140)));
        assert_eq!(slide.current, Some(a));
        assert!(!slide.wants_frame());
        assert!(!slide.advance(16.0));
    }
}
