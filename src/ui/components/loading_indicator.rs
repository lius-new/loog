//! SVG spinner composited independently of the document and application view.
use lgui::core::{
    CompositingLayerAnimation, CompositingLayerSpec, IconStyle, LayerTransform, UiElement, UiId,
    animated_compositing_layer,
};
use lgui::prelude::{Color, Element, UiRect};

pub const ICON_KEY: &str = "tab-loading";
pub const SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5"><circle cx="8" cy="8" r="5.5" opacity="0.16"/><path d="M8 2.5a5.5 5.5 0 1 1-5.5 5.5" stroke-linecap="round"/></svg>"#;

pub fn render(rect: UiRect, color: Color, key: String) -> Element {
    render_icon(rect, color, key, ICON_KEY)
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
            prelude::{State, component, group},
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
        let view: AppView = Arc::new(move |_| {
            let output = output.clone();
            let count = count.clone();
            component(viewport, move |cx, _| {
                count.fetch_add(1, Ordering::Relaxed);
                let shown = cx.state(true);
                *output.lock().unwrap() = Some(shown.clone());
                let mut root = group(viewport);
                if shown.get() {
                    root = root.child(render(
                        UiRect::new(12.0, 8.0, 28.0, 24.0),
                        Color::WHITE,
                        "spinner-test".into(),
                    ));
                }
                root
            })
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let initial = renders.load(Ordering::Relaxed);
        assert_eq!(session.runtime().frame_interval_ms(), Some(16));
        session.advance(16.0);
        session.advance(225.0);
        session.render_view(&view, viewport, UiScale::ONE);
        assert_eq!(
            renders.load(Ordering::Relaxed),
            initial,
            "rotation must not rebuild the application"
        );
        let node = session.tree().node(&UiId::new("spinner-test-svg")).unwrap();
        let layer = session.tree().node(node.parent.as_ref().unwrap()).unwrap();
        assert!(
            layer
                .compositing_layer
                .unwrap()
                .transform
                .rotation_degrees_f32()
                > 0.0
        );
        exposed
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .update(|shown| *shown = false);
        session.render_view(&view, viewport, UiScale::ONE);
        assert!(
            session
                .tree()
                .node(&UiId::new("spinner-test-svg"))
                .is_none()
        );
        // Consume the frame already scheduled before removal.
        session.advance(16.0);
        session.render_view(&view, viewport, UiScale::ONE);
        assert!(session.runtime().frame_interval_ms().is_none());
    }
}
