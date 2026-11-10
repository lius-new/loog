//! Loading indicators whose rotation is composited independently of the application view.
use lgui::core::{
    CompositingLayerAnimation, CompositingLayerSpec, IconStyle, LayerTransform, Stroke, UiElement,
    UiId, animated_compositing_layer, precompiled,
};
use lgui::prelude::{Color, Element, UiRect, VisualStyle, group};

pub const ICON_KEY: &str = "tab-loading";
pub const SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5"><path d="M8 2.5a5.5 5.5 0 1 1-5.5 5.5" stroke-linecap="round"/></svg>"#;

pub fn render(rect: UiRect, color: Color, key: String) -> Element {
    let track_id = UiId::owned(format!("{key}-track"));
    let group_key = key.clone();
    Element::new(move |cx| {
        let scale = cx.context.scale();
        // The icon rounds its raster size to the nearest pixel, while the
        // layer rounds up. Stay just below a whole pixel size so they agree
        // even after converting back through logical coordinates.
        let pixels = scale.physical_length(rect.width()) as f32;
        let side = (pixels - 1.0 / 64.0) / scale.factor();
        let icon_rect = rect.inflate((side - rect.width()) / 2.0, (side - rect.height()) / 2.0);

        // Keep the faint track still so resampling the arc cannot move it.
        let unit = side / 16.0;
        let track = UiElement::ellipse(
            track_id,
            icon_rect.inflate(-2.5 * unit, -2.5 * unit),
            VisualStyle::default().stroked(Stroke::new(color, 1.5 * unit, 41)),
        );
        // Compile the wrapper and its children together so the animation
        // gets a child scope of its own during incremental layout.
        cx.compile(
            group(rect)
                .child(precompiled(track))
                .child(render_icon(icon_rect, color, key, ICON_KEY)),
        )
    })
    .key(group_key)
}

/// The same turn, of another icon: a registered name or an `svg-path:` key.
pub fn render_icon(rect: UiRect, color: Color, key: String, icon: &'static str) -> Element {
    let id = UiId::owned(format!("{key}-svg"));
    animated_compositing_layer(rect, |_motion: &mut Spin| {})
        .key(key)
        .child(Element::new(move |_| {
            UiElement::icon(id, rect, icon).icon_style(IconStyle::new(color))
        }))
}

#[derive(Clone, Default)]
struct Spin {
    angle: f32,
}
impl CompositingLayerAnimation for Spin {
    fn compositing_layer_spec(&self) -> CompositingLayerSpec {
        CompositingLayerSpec::new()
            .transform(LayerTransform::identity().rotation_degrees(self.angle))
    }
    fn wants_frame(&self) -> bool {
        true
    }
    fn frame_interval_ms(&self) -> u64 {
        16
    }
    fn advance(&mut self, elapsed_ms: f32) -> bool {
        self.angle = (self.angle + elapsed_ms.max(0.0) * 360.0 / 900.0).rem_euclid(360.0);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rotation_uses_retained_frames_and_stops_when_the_spinner_is_removed() {
        use lgui::{
            application::AppView,
            core::UiScale,
            prelude::{State, component},
            session::UiSession,
        };
        use std::sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        };
        let exposed = Arc::new(Mutex::new(None::<State<bool>>));
        let output = exposed.clone();
        let renders = Arc::new(AtomicUsize::new(0));
        let count = renders.clone();
        let viewport = UiRect::new(0.0, 0.0, 160.0, 32.0);
        let indicator = UiRect::new(12.25, 8.25, 26.25, 22.25);
        let scale = UiScale::new(1.1);
        let view: AppView = Arc::new(move |_| {
            let output = output.clone();
            let count = count.clone();
            component(viewport, move |cx, _| {
                count.fetch_add(1, Ordering::Relaxed);
                let shown = cx.state(false);
                *output.lock().unwrap() = Some(shown.clone());
                let mut root = group(viewport);
                if shown.get() {
                    root = root.child(Element::new(move |cx| {
                        let spinner =
                            cx.compile(render(indicator, Color::WHITE, "spinner-test".into()));
                        assert_eq!(spinner.node().id, cx.id);
                        spinner
                    }));
                }
                root
            })
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, scale);
        let shown = exposed.lock().unwrap().as_ref().unwrap().clone();
        shown.update(|shown| *shown = true);
        session.render_view(&view, viewport, scale);
        let initial = renders.load(Ordering::Relaxed);
        assert_eq!(session.runtime().frame_interval_ms(), Some(16));
        session.advance(16.0);
        session.advance(225.0);
        session.render_view(&view, viewport, scale);
        assert_eq!(
            renders.load(Ordering::Relaxed),
            initial,
            "rotation must not rebuild the application"
        );
        let node = session.tree().node(&UiId::new("spinner-test-svg")).unwrap();
        let layer = session.tree().node(node.parent.as_ref().unwrap()).unwrap();
        let track = session
            .tree()
            .node(&UiId::new("spinner-test-track"))
            .unwrap();
        assert_eq!(
            track.parent, layer.parent,
            "the track must stay outside the rotation"
        );
        let raster = scale.physical_ui_rect(layer.layout_rect);
        let icon = scale.physical_ui_rect(node.layout_rect);
        assert_eq!(raster.width().ceil(), icon.width().round());
        assert_eq!(raster.height().ceil(), icon.height().round());
        let center = |rect: UiRect| {
            (
                (rect.left + rect.right) / 2.0,
                (rect.top + rect.bottom) / 2.0,
            )
        };
        assert_eq!(center(track.layout_rect), center(indicator));
        assert_eq!(center(layer.layout_rect), center(indicator));
        assert!(
            layer
                .compositing_layer
                .unwrap()
                .transform
                .rotation_degrees_f32()
                > 0.0
        );
        shown.update(|shown| *shown = false);
        session.render_view(&view, viewport, scale);
        assert!(
            session
                .tree()
                .node(&UiId::new("spinner-test-svg"))
                .is_none()
        );
        // Consume the frame already scheduled before removal.
        session.advance(16.0);
        session.render_view(&view, viewport, scale);
        assert!(session.runtime().frame_interval_ms().is_none());
    }
}
