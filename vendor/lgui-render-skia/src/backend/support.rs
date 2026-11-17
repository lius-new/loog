use super::*;

#[cfg(any(
    feature = "renderer-skia-gl",
    feature = "renderer-skia-vulkan",
    feature = "renderer-skia-metal"
))]
pub const fn gpu_cache_budget(total: usize) -> usize {
    total.saturating_mul(2) / 3
}

#[cfg(any(
    feature = "renderer-skia-gl",
    feature = "renderer-skia-vulkan",
    feature = "renderer-skia-metal"
))]
pub const fn cpu_cache_budget(total: usize) -> usize {
    total.saturating_sub(gpu_cache_budget(total))
}

#[cfg(any(
    feature = "renderer-skia-gl",
    feature = "renderer-skia-vulkan",
    feature = "renderer-skia-metal"
))]
pub fn with_gpu_cache_usage(
    mut stats: SkiaCacheStats,
    context: &skia_safe::gpu::DirectContext,
) -> SkiaCacheStats {
    let usage = context.resource_cache_usage();
    stats.budget_bytes = stats
        .budget_bytes
        .saturating_add(context.resource_cache_limit());
    stats.resident_bytes = stats.resident_bytes.saturating_add(usage.resource_bytes);
    stats.entries = stats.entries.saturating_add(usage.resource_count);
    stats
}

pub fn probe_skia_support(preference: GraphicsPreference) -> Result<(), String> {
    match preference {
        GraphicsPreference::Auto | GraphicsPreference::Software => {
            surfaces::raster_n32_premul((1, 1))
                .map(|_| ())
                .ok_or_else(|| "Skia could not create a raster surface".to_owned())
        }
        #[cfg(feature = "renderer-skia-gl")]
        GraphicsPreference::OpenGl => surfaces::raster_n32_premul((1, 1))
            .map(|_| ())
            .ok_or_else(|| "Skia could not create a raster surface".to_owned()),
        #[cfg(not(feature = "renderer-skia-gl"))]
        GraphicsPreference::OpenGl => Err("OpenGL is not enabled on this target".to_owned()),
        #[cfg(all(
            feature = "renderer-skia-vulkan",
            any(target_os = "windows", target_os = "linux")
        ))]
        GraphicsPreference::Vulkan => unsafe { ash::Entry::load() }
            .map(|_| ())
            .map_err(|error| format!("load Vulkan runtime: {error}")),
        #[cfg(not(all(
            feature = "renderer-skia-vulkan",
            any(target_os = "windows", target_os = "linux")
        )))]
        GraphicsPreference::Vulkan => Err("Vulkan is not enabled on this build".to_owned()),
        #[cfg(all(feature = "renderer-skia-metal", target_os = "macos"))]
        GraphicsPreference::Metal => surfaces::raster_n32_premul((1, 1))
            .map(|_| ())
            .ok_or_else(|| "Skia could not initialize Metal support".to_owned()),
        #[cfg(not(all(feature = "renderer-skia-metal", target_os = "macos")))]
        GraphicsPreference::Metal => Err("Metal is not enabled on this target".to_owned()),
    }
}
