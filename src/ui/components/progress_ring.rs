//! Loom's percentage convention adapted to the portable LGUI progress control.
use lgui::core::UiId;
use lgui::prelude::{Color, Element, UiRect};
pub fn render(id: UiId, rect: UiRect, percent: u8, color: Color) -> Element {
    lgui::progress_ring(rect, Some(f32::from(percent.min(100)) / 100.0), color).key(id.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_ring_keeps_percentage_semantics_without_svg_registrations() {
        use lgui::{
            application::AppView,
            core::{SemanticRole, UiScale},
            session::UiSession,
        };
        let view: AppView = std::sync::Arc::new(|_| {
            render(
                UiId::new("progress"),
                UiRect::new(0.0, 0.0, 16.0, 16.0),
                50,
                Color::WHITE,
            )
        });
        let mut session = UiSession::new();
        session.render_view(&view, UiRect::new(0.0, 0.0, 100.0, 100.0), UiScale::ONE);
        let sem = session
            .tree()
            .nodes()
            .iter()
            .filter_map(|n| n.semantics.as_ref())
            .find(|s| s.role == SemanticRole::ProgressIndicator)
            .unwrap();
        assert_eq!(sem.numeric_value, Some(0.5));
    }
}
