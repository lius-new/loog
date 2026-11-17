use super::*;
use lgui_core::text::TextRenderingOptions;

pub struct SkiaSoftwareSurface {
    pixels: Vec<u8>,
    size: (i32, i32),
    cache: SkiaCache,
    opaque: bool,
    text_rendering: TextRenderingOptions,
    last_text_props: Option<(skia_safe::PixelGeometry, u32, u32, TextRenderingOptions)>,
    painted_full_frame: bool,
}

impl SkiaSoftwareSurface {
    pub fn new(cache_budget: usize) -> Self {
        Self {
            pixels: Vec::new(),
            size: (0, 0),
            cache: SkiaCache::new(cache_budget),
            opaque: false,
            text_rendering: TextRenderingOptions::default(),
            last_text_props: None,
            painted_full_frame: false,
        }
    }

    /// Native presenters that discard alpha use an opaque target. Offscreen
    /// consumers can keep using `new` for premultiplied transparent pixels.
    pub fn new_opaque(cache_budget: usize) -> Self {
        Self {
            opaque: true,
            ..Self::new(cache_budget)
        }
    }

    pub fn is_opaque(&self) -> bool {
        self.opaque
    }

    pub fn set_text_rendering(&mut self, options: TextRenderingOptions) {
        self.text_rendering = options;
    }

    /// Presentation must include the full viewport when retained coverage changes.
    pub fn painted_full_frame(&self) -> bool {
        self.painted_full_frame
    }

    fn ensure_surface(&mut self, width: i32, height: i32) {
        let size = (width.max(1), height.max(1));
        if self.size != size {
            self.size = size;
            self.last_text_props = None;
            self.cache.trim(MemoryPressure::Critical);
        }
    }

    pub fn draw(&mut self, scene: &Scene, frame: &FrameInfo<'_>) -> Result<(), String> {
        let viewport = frame.viewport();
        self.ensure_surface(viewport.width(), viewport.height());
        let mut pixels = std::mem::take(&mut self.pixels);
        let required = self.size.0 as usize * self.size.1 as usize * 4;
        if pixels.len() != required {
            pixels.resize(required, 0);
            self.last_text_props = None;
        }
        let result = self.draw_pixels(scene, frame, &mut pixels);
        self.pixels = pixels;
        result
    }

    /// Paint directly into an opaque BGRA destination owned by the presenter.
    /// The caller must request a full redraw if this buffer does not contain
    /// the previous frame (for example, after allocation or buffer rotation).
    /// Offscreen callers retain their original owned pixels through `draw`.
    pub fn draw_into_bgra(
        &mut self,
        scene: &Scene,
        frame: &FrameInfo<'_>,
        pixels: &mut [u8],
    ) -> Result<(), String> {
        if !self.opaque {
            return Err("External presentation requires an opaque surface".into());
        }
        let viewport = frame.viewport();
        self.ensure_surface(viewport.width(), viewport.height());
        if pixels.len() != self.size.0 as usize * self.size.1 as usize * 4 {
            return Err("External BGRA buffer size does not match the viewport".into());
        }
        self.draw_pixels(scene, frame, pixels)
    }

    fn draw_pixels(
        &mut self,
        scene: &Scene,
        frame: &FrameInfo<'_>,
        pixels: &mut [u8],
    ) -> Result<(), String> {
        let viewport = frame.viewport();
        // softbuffer presents RGB and discards alpha. Describe that actual opaque
        // destination to Skia; transparent offscreen layers remain premultiplied.
        let alpha = if self.opaque {
            AlphaType::Opaque
        } else {
            AlphaType::Premul
        };
        let info = ImageInfo::new(self.size, ColorType::BGRA8888, alpha, None);
        let props = if self.opaque {
            TextRasterization::system().surface_props_with_options(self.text_rendering)
        } else {
            TextRasterization::Grayscale.surface_props_with_options(self.text_rendering)
        };
        let text_props = (
            props.pixel_geometry(),
            props.text_gamma().to_bits(),
            props.text_contrast().to_bits(),
            self.text_rendering,
        );
        // A gamma/pixel-order change affects every retained glyph, even when
        // the caller only invalidated a small part of the window.
        let refresh = FrameInfo::new(
            viewport,
            frame.damage(),
            frame.scale(),
            frame.reason(),
            frame.is_full_redraw() || self.last_text_props != Some(text_props),
        )
        .with_text_rendering(self.text_rendering);
        let mut surface =
            surfaces::wrap_pixels(&info, pixels, self.size.0 as usize * 4, Some(&props))
                .ok_or_else(|| "Skia could not wrap the retained software surface".to_owned())?;
        paint_scene_damage(surface.canvas(), &mut self.cache, scene, &refresh)?;
        self.last_text_props = Some(text_props);
        self.painted_full_frame = refresh.is_full_redraw();
        Ok(())
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub fn size(&self) -> (i32, i32) {
        self.size
    }

    pub fn trim(&mut self, pressure: MemoryPressure) {
        self.cache.trim(pressure);
        if pressure == MemoryPressure::Critical {
            self.pixels.clear();
            self.size = (0, 0);
            self.last_text_props = None;
        }
    }

    pub fn set_cache_budget(&mut self, budget_bytes: usize) {
        self.cache.set_budget(budget_bytes);
    }

    /// Connects the render cache to the application's observer.
    pub fn set_cache_hooks(&mut self, hooks: Option<lgui_core::memory::CacheHooks>) {
        self.cache.set_hooks(hooks);
    }

    pub fn cache_stats(&self) -> SkiaCacheStats {
        self.cache.stats()
    }
}

pub fn paint_scene_damage(
    canvas: &Canvas,
    cache: &mut SkiaCache,
    scene: &Scene,
    frame: &FrameInfo<'_>,
) -> Result<(), String> {
    let rendering_changed = cache.set_text_rendering(frame.text_rendering());
    cache.begin_frame();
    let clips: Vec<PhysicalRect> = if frame.is_full_redraw() || rendering_changed {
        vec![frame.viewport()]
    } else {
        frame.damage().to_vec()
    };
    let mut painter = SkiaPainter { cache };
    for clip in clips {
        canvas.save();
        canvas.clip_rect(physical_rect(clip), None, false);
        // Preserve the target's alpha contract. The native window is opaque;
        // transparent offscreen/test canvases still clear to transparent.
        canvas.clear(if canvas.image_info().alpha_type() == AlphaType::Opaque {
            SkColor::BLACK
        } else {
            SkColor::TRANSPARENT
        });
        painter.draw_commands(canvas, scene.commands(), Some(ui_rect_from_physical(clip)))?;
        canvas.restore();
    }
    painter.cache.end_frame();
    Ok(())
}

#[cfg(test)]
mod startup_tests {
    use super::*;
    use lgui_core::core::{RenderPhase, UiId, UiScale};
    use lgui_render_api::FrameReason;

    #[test]
    fn grayscale_profile_changes_refresh_retained_pixels_on_partial_frames() {
        let scale = UiScale::new(0.8);
        let bounds = UiRect::new(0.0, 0.0, 420.0, 60.0);
        let viewport = scale.physical_rect(bounds);
        let mut scene = Scene::new();
        scene.push(ScenePrimitive::Rect {
            id: UiId::new("bg"),
            rect: bounds,
            style: VisualStyle::filled(Color(0x14161b)),
            phase: RenderPhase::Content,
        });
        scene.push(ScenePrimitive::Text {
            id: UiId::new("text"),
            rect: UiRect::new(80.0, 0.0, 420.0, 60.0),
            text: "Fractional text 中文".into(),
            style: TextStyle::new(Color::WHITE, 16.0, 400),
            phase: RenderPhase::Content,
        });
        let physical = scene.project_to_physical(scale);
        let damage = [PhysicalRect::new(0, 0, 4, 4)];
        let partial = FrameInfo::new(viewport, &damage, scale, FrameReason::SceneChange, false);
        let full = FrameInfo::new(viewport, &[], scale, FrameReason::SceneChange, true);
        let custom = TextRenderingOptions::default()
            .with_grayscale_gamma(1.4)
            .unwrap()
            .with_grayscale_hinting(lgui_core::text::TextHinting::None);
        let mut retained = SkiaSoftwareSurface::new(4 * 1024 * 1024);
        for options in [
            TextRenderingOptions::default(),
            custom,
            TextRenderingOptions::default(),
        ] {
            retained.set_text_rendering(options);
            retained.draw(&physical, &partial).unwrap();
            assert!(retained.painted_full_frame());
            let mut fresh = SkiaSoftwareSurface::new(4 * 1024 * 1024);
            fresh.set_text_rendering(options);
            fresh.draw(&physical, &full).unwrap();
            assert_eq!(retained.pixels(), fresh.pixels());
        }
    }

    #[test]
    fn coverage_changes_refresh_cached_text_outside_partial_damage() {
        let scale = UiScale::new(1.0);
        let bounds = UiRect::new(0.0, 0.0, 420.0, 48.0);
        let viewport = scale.physical_rect(bounds);
        let mut scene = Scene::new();
        scene.push(ScenePrimitive::Rect {
            id: UiId::owned("coverage-background"),
            rect: bounds,
            style: VisualStyle::filled(Color::BLACK),
            phase: RenderPhase::Content,
        });
        scene.push(ScenePrimitive::Text {
            id: UiId::owned("coverage-cached-text"),
            rect: UiRect::new(100.0, 0.0, 420.0, 48.0),
            text: "English 中文 00 0689".into(),
            style: TextStyle::new(Color::WHITE, 13.0, 400),
            phase: RenderPhase::Content,
        });
        let damage = [scale.physical_rect(UiRect::new(0.0, 0.0, 8.0, 48.0))];
        let partial = FrameInfo::new(viewport, &damage, scale, FrameReason::SceneChange, false);
        let full = FrameInfo::new(viewport, &[], scale, FrameReason::SceneChange, true);
        let mut retained = SkiaSoftwareSurface::new_opaque(1024 * 1024);
        let mut independent = SkiaSoftwareSurface::new_opaque(1024 * 1024);
        independent.draw(&scene, &full).unwrap();
        let default_pixels = independent.pixels().to_vec();
        let custom = TextRenderingOptions::default()
            .with_lcd_gamma(1.25)
            .unwrap()
            .with_lcd_contrast(0.25)
            .unwrap();
        for options in [
            TextRenderingOptions::default(),
            custom,
            TextRenderingOptions::default(),
        ] {
            // Keep the scene fixed to isolate a coverage-profile change. All
            // text is outside the supplied damage; compare to a fresh full draw.
            retained.set_text_rendering(options);
            retained.draw(&scene, &partial).unwrap();
            let mut fresh = SkiaSoftwareSurface::new_opaque(1024 * 1024);
            fresh.set_text_rendering(options);
            fresh.draw(&scene, &full).unwrap();
            assert_eq!(retained.pixels(), fresh.pixels());
            independent.draw(&scene, &full).unwrap();
            assert_eq!(independent.pixels(), default_pixels);
        }
        assert_eq!(retained.cache_stats().text_entries, 1);
        assert_eq!(retained.cache_stats().text_hits, 2);
    }

    #[test]
    fn opaque_window_first_frame_and_cached_repaint_survive_at_fractional_dpi() {
        let mut renderer = SkiaSoftwareSurface::new_opaque(8 * 1024 * 1024);
        let mut logical = Scene::new();
        for (row, sample) in ["Native API started", "代码编辑器", "00 0689"]
            .iter()
            .enumerate()
        {
            logical.push(ScenePrimitive::Text {
                id: UiId::owned(format!("startup-text-{row}")),
                rect: UiRect::new(4.0, row as f32 * 28.0, 400.0, (row + 1) as f32 * 28.0),
                text: (*sample).to_owned().into(),
                style: TextStyle::new(Color::WHITE, 14.0, 400),
                phase: RenderPhase::Content,
            });
        }
        for factor in [0.8, 1.0, 1.25, 1.5] {
            let scale = UiScale::new(factor);
            let scene = logical.project_to_physical(scale);
            let viewport = scale.physical_rect(UiRect::new(0.0, 0.0, 420.0, 96.0));
            for _ in 0..2 {
                let frame = FrameInfo::new(viewport, &[], scale, FrameReason::SceneChange, true);
                renderer.draw(&scene, &frame).unwrap();
                assert!(renderer.pixels().chunks_exact(4).all(|p| p[3] == 255));
                assert!(
                    renderer
                        .pixels()
                        .chunks_exact(4)
                        .any(|p| p[..3].iter().any(|v| *v > 32))
                );
            }
            assert!(renderer.cache_stats().text_hits >= 3);
        }
    }

}
