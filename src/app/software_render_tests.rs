//! Pixel regression for the native presenter's caller-owned framebuffer.
use lgui::core::{PhysicalRect, RenderPhase, Scene, ScenePrimitive, UiId, UiScale};
use lgui::prelude::{Color, TextStyle, UiRect, VisualStyle};
use lgui::render_skia::SkiaSoftwareSurface;
use lgui::text::TextRenderingOptions;
use lgui::{FrameInfo, FrameReason};

#[test]
fn external_bgra_matches_owned_pixels_across_damage_resize_and_text_profiles() {
    let mut owned = SkiaSoftwareSurface::new_opaque(1024 * 1024);
    let mut direct = SkiaSoftwareSurface::new_opaque(1024 * 1024);
    for factor in [1.0, 1.25, 1.5, 0.8] {
        let scale = UiScale::new(factor);
        let bounds = UiRect::new(0.0, 0.0, 240.0, 48.0);
        let viewport = scale.physical_rect(bounds);
        let mut pixels = vec![0u8; viewport.width() as usize * viewport.height() as usize * 4];
        let custom = TextRenderingOptions::default()
            .with_lcd_gamma(1.25)
            .unwrap();
        for (index, options) in [
            TextRenderingOptions::default(),
            TextRenderingOptions::default(),
            custom,
            TextRenderingOptions::default(),
        ]
        .into_iter()
        .enumerate()
        {
            let mut logical = Scene::new();
            logical.push(ScenePrimitive::Rect {
                id: UiId::new("background"),
                rect: bounds,
                style: VisualStyle::filled(Color(0x14161b)),
                phase: RenderPhase::Content,
            });
            logical.push(ScenePrimitive::Rect {
                id: UiId::new("changed"),
                rect: UiRect::new(0.0, 0.0, 8.0, 8.0),
                style: VisualStyle::filled(if index % 2 == 0 {
                    Color::WHITE
                } else {
                    Color::BLACK
                }),
                phase: RenderPhase::Content,
            });
            logical.push(ScenePrimitive::Text {
                id: UiId::new("text"),
                rect: UiRect::new(12.0, 0.0, 240.0, 48.0),
                text: "English 中文 0689".into(),
                style: TextStyle::new(Color::WHITE, 14.0, 400),
                phase: RenderPhase::Content,
            });
            let scene = logical.project_to_physical(scale);
            let damage = [scale.physical_rect(UiRect::new(0.0, 0.0, 8.0, 8.0))];
            let frame = FrameInfo::new(
                viewport,
                &damage,
                scale,
                FrameReason::SceneChange,
                index == 0,
            );
            owned.set_text_rendering(options);
            direct.set_text_rendering(options);
            owned.draw(&scene, &frame).unwrap();
            direct.draw_into_bgra(&scene, &frame, &mut pixels).unwrap();
            assert!(
                direct.pixels().is_empty(),
                "Direct path must not allocate a second framebuffer"
            );
            for (expected, actual) in owned.pixels().chunks_exact(4).zip(pixels.chunks_exact(4)) {
                assert_eq!(&expected[..3], &actual[..3]);
            }
            // Match softbuffer's high-byte contract between partial paints.
            for pixel in pixels.chunks_exact_mut(4) {
                pixel[3] = 0;
            }
        }
    }
    assert!(
        direct
            .draw_into_bgra(
                &Scene::new(),
                &FrameInfo::new(
                    PhysicalRect::new(0, 0, 2, 2),
                    &[],
                    UiScale::new(1.0),
                    FrameReason::SceneChange,
                    true
                ),
                &mut [0; 4]
            )
            .is_err()
    );
}
