use std::time::Instant;

use lgui::core::{
    CompositingLayerAnimation, CompositingLayerSpec, EventPolicy, LayerTransform, Signal,
    UiElement, UiId, animated_compositing_layer,
};
use lgui::prelude::{Element, UiRect};

/// Move the retained layer horizontally at the supplied bounds' fixed height.
pub(crate) fn floating(
    key: &'static str,
    bounds: UiRect,
    grab_x: f32,
    pointer_x: Signal<f32>,
) -> Element {
    let transform = move |x: f32| {
        CompositingLayerSpec::new()
            .transform(LayerTransform::identity().translation(x - grab_x, 0.0))
    };
    let initial = transform(pointer_x.get());
    Element::new(move |cx| {
        UiElement::compositing_layer(UiId::new(key), bounds, initial)
            .hit_rect(UiRect::default())
            .children(cx.children)
    })
    .key(key)
    .event_policy(EventPolicy::NONE)
    .bind(&pointer_x, move |x, node| {
        node.compositing_layer = Some(transform(*x));
    })
}

/// Stationary tabs keep the direct text paint path. Only displaced tabs need a layer.
pub(crate) fn slide(bounds: UiRect, scroll_x: f32, target: Option<f32>) -> Element {
    Element::new(move |cx| {
        if let Some(target) = target {
            let scale = cx.context.scale();
            // A fractional raster origin makes the text jump when a layer is mounted.
            let screen = bounds.translate(-scroll_x, 0.0);
            let aligned = scale
                .logical_rect(scale.physical_rect_outward(screen))
                .translate(scroll_x, 0.0);
            let layer = cx.compile(animated_compositing_layer(
                aligned,
                move |motion: &mut Slide| motion.retarget(target),
            ));
            let offset = layer
                .node()
                .compositing_layer
                .map_or(0.0, |spec| spec.transform.translation_x());
            if target != 0.0 || offset != 0.0 {
                return layer.children(cx.children);
            }
        }
        UiElement::group(cx.id, bounds)
            .hit_rect(UiRect::default())
            .children(cx.children)
    })
}

/// Final-slot hysteresis keeps live reorder steady at a neighbor's midpoint.
pub(crate) fn reorder_slot(
    current: usize,
    center_x: f32,
    slot_at: impl Fn(f32) -> Option<usize>,
) -> Option<usize> {
    const HYSTERESIS: f32 = 4.0;
    slot_at(center_x - HYSTERESIS)
        .filter(|index| *index > current)
        .or_else(|| slot_at(center_x + HYSTERESIS).filter(|index| *index < current))
}

#[derive(Clone, Default)]
struct Slide {
    offset: f32,
    from: f32,
    target: f32,
    started: Option<Instant>,
}

impl Slide {
    fn retarget(&mut self, target: f32) {
        if self.target != target {
            self.from = self.offset;
            self.target = target;
            self.started = (self.offset != target).then(Instant::now);
        }
    }
}

impl CompositingLayerAnimation for Slide {
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
