use super::*;

pub(super) enum WinitSkiaRenderer {
    #[cfg(feature = "renderer-skia")]
    Software {
        surface: SoftSurface<OwnedDisplayHandle, Arc<Window>>,
        renderer: SkiaSoftwareSurface,
        fallback_reason: Option<&'static str>,
    },
    #[cfg(feature = "renderer-skia-gl")]
    OpenGl(super::winit_skia_gl::WinitOpenGlRenderer),
    #[cfg(all(
        feature = "renderer-skia-vulkan",
        any(target_os = "windows", target_os = "linux")
    ))]
    Vulkan(super::winit_skia_vulkan::WinitVulkanRenderer),
    #[cfg(all(feature = "renderer-skia-metal", target_os = "macos"))]
    Metal(super::winit_skia_metal::WinitMetalRenderer),
    Unavailable,
}

#[derive(Clone, Copy, Debug, Default)]
#[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
pub(super) struct WinitRendererCacheStats {
    pub budget_bytes: usize,
    pub resident_bytes: usize,
    pub entries: usize,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub text_resident_bytes: usize,
    pub text_entries: usize,
    pub text_hits: u64,
    pub text_misses: u64,
    pub text_evictions: u64,
    pub largest_entry_bytes: usize,
    pub largest_text_entry_bytes: usize,
}

#[cfg(feature = "renderer-skia")]
impl From<lgui_render_skia::SkiaCacheStats> for WinitRendererCacheStats {
    fn from(stats: lgui_render_skia::SkiaCacheStats) -> Self {
        Self {
            budget_bytes: stats.budget_bytes,
            resident_bytes: stats.resident_bytes,
            entries: stats.entries,
            hits: stats.hits,
            misses: stats.misses,
            evictions: stats.evictions,
            text_resident_bytes: stats.text_resident_bytes,
            text_entries: stats.text_entries,
            text_hits: stats.text_hits,
            text_misses: stats.text_misses,
            text_evictions: stats.text_evictions,
            largest_entry_bytes: stats.largest_entry_bytes,
            largest_text_entry_bytes: stats.largest_text_entry_bytes,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RendererRecoveryState {
    Healthy,
    Recovering { attempt: u8, gpu_failure: bool },
    Fallback { reason: &'static str, attempts: u8 },
    Failed { attempts: u8 },
}

impl RendererRecoveryState {
    pub(super) fn attempt(self) -> u8 {
        match self {
            Self::Healthy => 0,
            Self::Fallback { attempts, .. } => attempts,
            Self::Recovering { attempt, .. } => attempt,
            Self::Failed { attempts } => attempts,
        }
    }

    #[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Recovering { .. } => "recovering",
            Self::Fallback { .. } => "fallback",
            Self::Failed { .. } => "failed",
        }
    }
}

pub(super) struct WinitRenderError {
    pub(super) stage: lgui_render_api::RenderErrorStage,
    pub(super) operation: &'static str,
    pub(super) message: String,
}

#[derive(Clone, Copy, Debug, Default)]
#[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
pub(crate) struct WinitFrameTimings {
    pub acquire_ms: f32,
    pub draw_ms: f32,
    pub flush_ms: f32,
    pub present_ms: f32,
}

impl WinitRenderError {
    pub(super) fn new(
        stage: lgui_render_api::RenderErrorStage,
        operation: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            stage,
            operation,
            message: message.into(),
        }
    }
}

impl WinitSkiaRenderer {
    pub(super) fn name(&self) -> &'static str {
        match self {
            #[cfg(feature = "renderer-skia")]
            Self::Software { .. } => "skia-software",
            #[cfg(feature = "renderer-skia-gl")]
            Self::OpenGl(_) => "skia-opengl",
            #[cfg(all(
                feature = "renderer-skia-vulkan",
                any(target_os = "windows", target_os = "linux")
            ))]
            Self::Vulkan(_) => "skia-vulkan",
            #[cfg(all(feature = "renderer-skia-metal", target_os = "macos"))]
            Self::Metal(_) => "skia-metal",
            Self::Unavailable => "skia-unavailable",
        }
    }

    pub(super) fn fallback_reason(&self) -> Option<&'static str> {
        match self {
            #[cfg(feature = "renderer-skia")]
            Self::Software {
                fallback_reason, ..
            } => *fallback_reason,
            #[cfg(feature = "renderer-skia-gl")]
            Self::OpenGl(_) => None,
            #[cfg(all(
                feature = "renderer-skia-vulkan",
                any(target_os = "windows", target_os = "linux")
            ))]
            Self::Vulkan(_) => None,
            #[cfg(all(feature = "renderer-skia-metal", target_os = "macos"))]
            Self::Metal(_) => None,
            Self::Unavailable => None,
        }
    }

    pub(super) fn set_fallback_reason(&mut self, _reason: &'static str) {
        #[cfg(feature = "renderer-skia")]
        if let Self::Software {
            fallback_reason, ..
        } = self
        {
            *fallback_reason = Some(_reason);
        }
    }

    pub(super) fn is_gpu(&self) -> bool {
        match self {
            #[cfg(feature = "renderer-skia")]
            Self::Software { .. } => false,
            Self::Unavailable => false,
            #[cfg(feature = "renderer-skia-gl")]
            Self::OpenGl(_) => true,
            #[cfg(all(
                feature = "renderer-skia-vulkan",
                any(target_os = "windows", target_os = "linux")
            ))]
            Self::Vulkan(_) => true,
            #[cfg(all(feature = "renderer-skia-metal", target_os = "macos"))]
            Self::Metal(_) => true,
        }
    }

    pub(super) fn draw_and_present(
        &mut self,
        _scene: &lgui_core::core::Scene,
        _frame: &FrameInfo<'_>,
        _damage: &[PhysicalRect],
    ) -> Result<WinitFrameTimings, WinitRenderError> {
        match self {
            #[cfg(feature = "renderer-skia")]
            Self::Software {
                surface, renderer, ..
            } => {
                renderer.set_text_rendering(_frame.text_rendering());
                let direct = cfg!(target_os = "windows") && renderer.is_opaque();
                let draw_started = Instant::now();
                if !direct {
                    renderer.draw(_scene, _frame).map_err(|error| {
                        WinitRenderError::new(
                            lgui_render_api::RenderErrorStage::Draw,
                            "skia_software_draw",
                            error,
                        )
                    })?;
                }
                let mut draw_ms = draw_started.elapsed().as_secs_f32() * 1_000.0;
                let acquire_started = Instant::now();
                let (width, height) = (_frame.viewport().width(), _frame.viewport().height());
                surface
                    .resize(
                        NonZeroU32::new(width.max(1) as u32).unwrap(),
                        NonZeroU32::new(height.max(1) as u32).unwrap(),
                    )
                    .map_err(|error| {
                        WinitRenderError::new(
                            lgui_render_api::RenderErrorStage::Prepare,
                            "softbuffer_resize",
                            error.to_string(),
                        )
                    })?;
                let mut buffer = surface.buffer_mut().map_err(|error| {
                    WinitRenderError::new(
                        lgui_render_api::RenderErrorStage::Prepare,
                        "softbuffer_buffer",
                        error.to_string(),
                    )
                })?;
                let acquire_ms = acquire_started.elapsed().as_secs_f32() * 1_000.0;
                if direct {
                    // Windows' retained DIB uses little-endian BGRA bytes.
                    // A new buffer needs full coverage, even for partial damage.
                    let draw_started = Instant::now();
                    let frame = FrameInfo::new(
                        _frame.viewport(),
                        _frame.damage(),
                        _frame.scale(),
                        _frame.reason(),
                        _frame.is_full_redraw() || buffer.age() != 1,
                    )
                    .with_text_rendering(_frame.text_rendering());
                    renderer
                        .draw_into_bgra(_scene, &frame, bytemuck::cast_slice_mut(&mut buffer))
                        .map_err(|error| {
                            WinitRenderError::new(
                                lgui_render_api::RenderErrorStage::Draw,
                                "skia_software_draw",
                                error,
                            )
                        })?;
                    // Softbuffer requires a zero high byte; opaque Skia ignores it.
                    for pixel in buffer.iter_mut() {
                        *pixel &= 0x00ff_ffff;
                    }
                    draw_ms = draw_started.elapsed().as_secs_f32() * 1_000.0;
                } else {
                    // Transparent targets retain premultiplied alpha in their
                    // owned surface. Other platforms keep their existing path.
                    for (destination, source) in
                        buffer.iter_mut().zip(renderer.pixels().chunks_exact(4))
                    {
                        *destination = ((source[2] as u32) << 16)
                            | ((source[1] as u32) << 8)
                            | source[0] as u32;
                    }
                }
                let full_damage = [_frame.viewport()];
                let effective_damage = if renderer.painted_full_frame() {
                    &full_damage[..]
                } else {
                    _damage
                };
                let damage = effective_damage
                    .iter()
                    .filter_map(|rect| {
                        Some(softbuffer::Rect {
                            x: rect.left.max(0) as u32,
                            y: rect.top.max(0) as u32,
                            width: NonZeroU32::new(rect.width().max(0) as u32)?,
                            height: NonZeroU32::new(rect.height().max(0) as u32)?,
                        })
                    })
                    .collect::<Vec<_>>();
                let present_started = Instant::now();
                buffer.present_with_damage(&damage).map_err(|error| {
                    WinitRenderError::new(
                        lgui_render_api::RenderErrorStage::Present,
                        "softbuffer_present",
                        error.to_string(),
                    )
                })?;
                #[cfg(target_os = "windows")]
                super::winit_windows::flush_gdi();
                Ok(WinitFrameTimings {
                    acquire_ms,
                    draw_ms,
                    flush_ms: 0.0,
                    present_ms: present_started.elapsed().as_secs_f32() * 1_000.0,
                })
            }
            #[cfg(feature = "renderer-skia-gl")]
            Self::OpenGl(renderer) => renderer.draw_scene(_scene, _frame).map_err(|error| {
                WinitRenderError::new(
                    lgui_render_api::RenderErrorStage::Present,
                    "skia_opengl_frame",
                    error,
                )
            }),
            #[cfg(all(
                feature = "renderer-skia-vulkan",
                any(target_os = "windows", target_os = "linux")
            ))]
            Self::Vulkan(renderer) => renderer.draw_scene(_scene, _frame).map_err(|error| {
                WinitRenderError::new(
                    lgui_render_api::RenderErrorStage::Present,
                    "skia_vulkan_frame",
                    error,
                )
            }),
            #[cfg(all(feature = "renderer-skia-metal", target_os = "macos"))]
            Self::Metal(renderer) => renderer.draw_scene(_scene, _frame).map_err(|error| {
                WinitRenderError::new(
                    lgui_render_api::RenderErrorStage::Present,
                    "skia_metal_frame",
                    error,
                )
            }),
            Self::Unavailable => Err(WinitRenderError::new(
                lgui_render_api::RenderErrorStage::Create,
                "renderer_unavailable",
                "renderer recreation has not completed",
            )),
        }
    }

    #[cfg(feature = "diagnostics")]
    pub(super) fn device_info(&self) -> lgui_diagnostics::RendererDeviceInfo {
        match self {
            #[cfg(feature = "renderer-skia")]
            Self::Software { .. } => lgui_diagnostics::RendererDeviceInfo {
                api: "software".to_owned(),
                color_format: "BGRA8 premultiplied".to_owned(),
                present_mode: "softbuffer damage".to_owned(),
                ..lgui_diagnostics::RendererDeviceInfo::default()
            },
            #[cfg(feature = "renderer-skia-gl")]
            Self::OpenGl(renderer) => renderer.device_info(),
            #[cfg(all(
                feature = "renderer-skia-vulkan",
                any(target_os = "windows", target_os = "linux")
            ))]
            Self::Vulkan(renderer) => renderer.device_info(),
            #[cfg(all(feature = "renderer-skia-metal", target_os = "macos"))]
            Self::Metal(renderer) => renderer.device_info(),
            Self::Unavailable => lgui_diagnostics::RendererDeviceInfo::default(),
        }
    }

    pub(super) fn trim(&mut self, _pressure: MemoryPressure) {
        match self {
            #[cfg(feature = "renderer-skia")]
            Self::Software { renderer, .. } => renderer.trim(_pressure),
            #[cfg(feature = "renderer-skia-gl")]
            Self::OpenGl(renderer) => renderer.trim(_pressure),
            #[cfg(all(
                feature = "renderer-skia-vulkan",
                any(target_os = "windows", target_os = "linux")
            ))]
            Self::Vulkan(renderer) => renderer.trim(_pressure),
            #[cfg(all(feature = "renderer-skia-metal", target_os = "macos"))]
            Self::Metal(renderer) => renderer.trim(_pressure),
            Self::Unavailable => {}
        }
    }

    /// Connects the renderer's caches to the application's observer.
    pub(super) fn set_cache_hooks(&mut self, _hooks: lgui_core::memory::CacheHooks) {
        match self {
            #[cfg(feature = "renderer-skia")]
            Self::Software { renderer, .. } => renderer.set_cache_hooks(Some(_hooks)),
            #[cfg(feature = "renderer-skia-gl")]
            Self::OpenGl(renderer) => renderer.set_cache_hooks(Some(_hooks)),
            #[cfg(all(
                feature = "renderer-skia-vulkan",
                any(target_os = "windows", target_os = "linux")
            ))]
            Self::Vulkan(renderer) => renderer.set_cache_hooks(Some(_hooks)),
            #[cfg(all(feature = "renderer-skia-metal", target_os = "macos"))]
            Self::Metal(renderer) => renderer.set_cache_hooks(Some(_hooks)),
            Self::Unavailable => {}
        }
    }

    #[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
    pub(super) fn cache_stats(&self) -> WinitRendererCacheStats {
        match self {
            #[cfg(feature = "renderer-skia")]
            Self::Software { renderer, .. } => renderer.cache_stats().into(),
            #[cfg(feature = "renderer-skia-gl")]
            Self::OpenGl(renderer) => renderer.cache_stats().into(),
            #[cfg(all(
                feature = "renderer-skia-vulkan",
                any(target_os = "windows", target_os = "linux")
            ))]
            Self::Vulkan(renderer) => renderer.cache_stats().into(),
            #[cfg(all(feature = "renderer-skia-metal", target_os = "macos"))]
            Self::Metal(renderer) => renderer.cache_stats().into(),
            Self::Unavailable => WinitRendererCacheStats::default(),
        }
    }
}

pub(super) fn next_frame_deadline(
    current: Option<Instant>,
    now: Instant,
    interval_ms: Option<u64>,
) -> Option<Instant> {
    interval_ms.map(|milliseconds| {
        let requested = now + Duration::from_millis(milliseconds.max(1));
        current.map_or(requested, |deadline| deadline.min(requested))
    })
}

pub(super) fn renderer_recovery_state(renderer: &WinitSkiaRenderer) -> RendererRecoveryState {
    renderer
        .fallback_reason()
        .map_or(RendererRecoveryState::Healthy, |reason| {
            RendererRecoveryState::Fallback {
                reason,
                attempts: 0,
            }
        })
}

pub(super) fn recovery_preference(
    configured: GraphicsPreference,
    was_fallback: bool,
    gpu_failure: bool,
    attempt: u8,
) -> (GraphicsPreference, bool) {
    let use_software =
        configured == GraphicsPreference::Auto && (was_fallback || (gpu_failure && attempt >= 2));
    if use_software {
        (GraphicsPreference::Software, true)
    } else {
        (configured, false)
    }
}

pub(super) fn graphics_preference_supported(preference: GraphicsPreference) -> bool {
    match preference {
        GraphicsPreference::Auto | GraphicsPreference::Software => cfg!(feature = "renderer-skia"),
        GraphicsPreference::OpenGl => cfg!(feature = "renderer-skia-gl"),
        GraphicsPreference::Vulkan => cfg!(all(
            feature = "renderer-skia-vulkan",
            any(target_os = "windows", target_os = "linux")
        )),
        GraphicsPreference::Metal => {
            cfg!(all(feature = "renderer-skia-metal", target_os = "macos"))
        }
    }
}

#[cfg(test)]
pub(super) fn auto_driver_order() -> Vec<GraphicsPreference> {
    #[allow(unused_mut)]
    let mut drivers = Vec::with_capacity(3);
    #[cfg(all(feature = "renderer-skia-metal", target_os = "macos"))]
    drivers.push(GraphicsPreference::Metal);
    #[cfg(all(feature = "renderer-skia-vulkan", target_os = "linux"))]
    drivers.push(GraphicsPreference::Vulkan);
    #[cfg(feature = "renderer-skia-gl")]
    drivers.push(GraphicsPreference::OpenGl);
    #[cfg(feature = "renderer-skia")]
    drivers.push(GraphicsPreference::Software);
    drivers
}

#[cfg(feature = "renderer-skia")]
pub(super) fn create_renderer(
    preference: GraphicsPreference,
    soft_context: &SoftContext<OwnedDisplayHandle>,
    window: Arc<Window>,
    transparent: bool,
    cache_budget: usize,
) -> Result<WinitSkiaRenderer, WinitApplicationError> {
    #[allow(unused_mut)]
    let mut fallback_reason = None;
    #[cfg(not(any(
        feature = "renderer-skia-gl",
        feature = "renderer-skia-vulkan",
        feature = "renderer-skia-metal"
    )))]
    let _ = transparent;

    #[cfg(all(feature = "renderer-skia-metal", target_os = "macos"))]
    if matches!(
        preference,
        GraphicsPreference::Auto | GraphicsPreference::Metal
    ) {
        match super::winit_skia_metal::WinitMetalRenderer::new(
            Arc::clone(&window),
            cache_budget,
            transparent,
        ) {
            Ok(renderer) => return Ok(WinitSkiaRenderer::Metal(renderer)),
            Err(error) if preference == GraphicsPreference::Metal => {
                return Err(WinitApplicationError(error));
            }
            Err(error) => {
                eprintln!("lgui: Skia Metal initialization failed: {error}");
                fallback_reason = Some("metal-init-failed");
            }
        }
    }

    #[cfg(all(
        feature = "renderer-skia-vulkan",
        any(target_os = "windows", target_os = "linux")
    ))]
    if preference == GraphicsPreference::Vulkan
        || (preference == GraphicsPreference::Auto && cfg!(target_os = "linux"))
    {
        match super::winit_skia_vulkan::WinitVulkanRenderer::new(
            Arc::clone(&window),
            cache_budget,
            transparent,
        ) {
            Ok(renderer) => return Ok(WinitSkiaRenderer::Vulkan(renderer)),
            Err(error) if preference == GraphicsPreference::Vulkan => {
                return Err(WinitApplicationError(error));
            }
            Err(error) => {
                eprintln!("lgui: Skia Vulkan initialization failed: {error}");
                fallback_reason = Some("vulkan-init-failed");
            }
        }
    }

    #[cfg(feature = "renderer-skia-gl")]
    if matches!(
        preference,
        GraphicsPreference::Auto | GraphicsPreference::OpenGl
    ) {
        match super::winit_skia_gl::WinitOpenGlRenderer::new(
            Arc::clone(&window),
            cache_budget,
            transparent,
        ) {
            Ok(renderer) => return Ok(WinitSkiaRenderer::OpenGl(renderer)),
            Err(error) if preference == GraphicsPreference::OpenGl => {
                return Err(WinitApplicationError(error));
            }
            Err(error) => {
                eprintln!("lgui: Skia OpenGL initialization failed: {error}");
                fallback_reason = Some("opengl-init-failed");
            }
        }
    }

    if !matches!(
        preference,
        GraphicsPreference::Auto | GraphicsPreference::Software
    ) {
        return Err(WinitApplicationError(format!(
            "the {} Skia driver is not available on this target",
            preference.as_str()
        )));
    }
    let surface = SoftSurface::new(soft_context, window)
        .map_err(|error| WinitApplicationError(format!("create software surface: {error}")))?;
    Ok(WinitSkiaRenderer::Software {
        surface,
        renderer: if transparent {
            SkiaSoftwareSurface::new(cache_budget)
        } else {
            SkiaSoftwareSurface::new_opaque(cache_budget)
        },
        fallback_reason: fallback_reason.or_else(|| {
            (preference == GraphicsPreference::Auto).then_some("gpu-driver-unavailable")
        }),
    })
}

#[cfg(not(feature = "renderer-skia"))]
pub(super) fn create_renderer(
    _preference: GraphicsPreference,
    _soft_context: &SoftContext<OwnedDisplayHandle>,
    _window: Arc<Window>,
    _transparent: bool,
    _cache_budget: usize,
) -> Result<WinitSkiaRenderer, WinitApplicationError> {
    Err(WinitApplicationError(
        "the winit backend requires a renderer-skia feature".to_owned(),
    ))
}
