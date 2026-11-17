use std::{mem::ManuallyDrop, sync::Arc, time::Instant};

use objc2::{
    rc::{Retained, autoreleasepool},
    runtime::ProtocolObject,
};
use objc2_app_kit::NSView;
use objc2_core_foundation::CGSize;
use objc2_metal::{
    MTLCommandBuffer, MTLCommandQueue, MTLCreateSystemDefaultDevice, MTLDevice, MTLDrawable,
    MTLPixelFormat,
};
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use skia_safe::{
    Color as SkColor, ColorType, ImageInfo, Paint, Rect, Surface,
    gpu::{self, SurfaceOrigin, backend_render_targets, direct_contexts, mtl, surfaces},
};
use winit::window::Window;

use crate::WinitFrameTimings;
use lgui_core::core::{PhysicalRect, Scene};
use lgui_render_api::{FrameInfo, MemoryPressure};
use lgui_render_skia::{
    SkiaCache, SkiaCacheStats, cpu_cache_budget, gpu_cache_budget, paint_scene_damage,
    with_gpu_cache_usage,
};

pub(crate) struct WinitMetalRenderer {
    scene_surface: ManuallyDrop<Surface>,
    skia_context: ManuallyDrop<gpu::DirectContext>,
    cache: SkiaCache,
    scene_size: (i32, i32),
    metal_layer: Retained<CAMetalLayer>,
    command_queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    adapter_name: String,
    _window: Arc<Window>,
}

impl WinitMetalRenderer {
    pub(crate) fn new(
        window: Arc<Window>,
        cache_budget: usize,
        transparent: bool,
    ) -> Result<Self, String> {
        autoreleasepool(|_| {
            let device = MTLCreateSystemDefaultDevice()
                .ok_or_else(|| "macOS did not provide a default Metal device".to_owned())?;
            let adapter_name = device.name().to_string();
            let metal_layer = CAMetalLayer::new();
            metal_layer.setDevice(Some(&device));
            metal_layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
            metal_layer.setPresentsWithTransaction(false);
            metal_layer.setFramebufferOnly(false);
            metal_layer.setOpaque(!transparent);
            let size = window.inner_size();
            metal_layer.setDrawableSize(CGSize::new(
                size.width.max(1) as f64,
                size.height.max(1) as f64,
            ));

            let handle = window
                .window_handle()
                .map_err(|error| format!("read Metal window handle: {error}"))?
                .as_raw();
            let RawWindowHandle::AppKit(handle) = handle else {
                return Err("winit did not expose an AppKit window handle".to_owned());
            };
            let view = unsafe { (handle.ns_view.as_ptr() as *mut NSView).as_ref() }
                .ok_or_else(|| "winit returned a null NSView".to_owned())?;
            view.setWantsLayer(true);
            view.setLayer(Some(&metal_layer.clone().into_super()));

            let command_queue = device
                .newCommandQueue()
                .ok_or_else(|| "create Metal command queue".to_owned())?;
            let backend = unsafe {
                mtl::BackendContext::new(
                    Retained::as_ptr(&device) as mtl::Handle,
                    Retained::as_ptr(&command_queue) as mtl::Handle,
                )
            };
            let mut skia_context = direct_contexts::make_metal(&backend, None)
                .ok_or_else(|| "Skia could not create a Metal DirectContext".to_owned())?;
            skia_context.set_resource_cache_limit(gpu_cache_budget(cache_budget));
            let scene_size = (size.width.max(1) as i32, size.height.max(1) as i32);
            let scene_surface = create_scene_surface(&mut skia_context, scene_size)?;

            Ok(Self {
                scene_surface: ManuallyDrop::new(scene_surface),
                skia_context: ManuallyDrop::new(skia_context),
                cache: SkiaCache::new(cpu_cache_budget(cache_budget)),
                scene_size,
                metal_layer,
                command_queue,
                adapter_name,
                _window: window,
            })
        })
    }

    pub(crate) fn draw_scene(
        &mut self,
        scene: &Scene,
        frame: &FrameInfo<'_>,
    ) -> Result<WinitFrameTimings, String> {
        autoreleasepool(|_| {
            let acquire_started = Instant::now();
            self.ensure_sized(frame.viewport())?;
            let drawable = self
                .metal_layer
                .nextDrawable()
                .ok_or_else(|| "Metal layer did not provide a drawable".to_owned())?;
            let acquire_ms = acquire_started.elapsed().as_secs_f32() * 1_000.0;
            let draw_started = Instant::now();
            paint_scene_damage(self.scene_surface.canvas(), &mut self.cache, scene, frame)?;
            let texture_info = unsafe {
                mtl::TextureInfo::new(Retained::as_ptr(&drawable.texture()) as mtl::Handle)
            };
            let target = backend_render_targets::make_mtl(self.scene_size, &texture_info);
            let mut window_surface = surfaces::wrap_backend_render_target(
                &mut self.skia_context,
                &target,
                SurfaceOrigin::TopLeft,
                ColorType::BGRA8888,
                None,
                None,
            )
            .ok_or_else(|| "Skia could not wrap the Metal drawable".to_owned())?;
            let snapshot = self.scene_surface.image_snapshot();
            let destination = Rect::from_wh(self.scene_size.0 as f32, self.scene_size.1 as f32);
            window_surface.canvas().clear(SkColor::TRANSPARENT);
            window_surface.canvas().draw_image_rect(
                snapshot,
                None,
                &destination,
                &Paint::default(),
            );
            let draw_ms = draw_started.elapsed().as_secs_f32() * 1_000.0;
            let flush_started = Instant::now();
            self.skia_context.flush_and_submit();
            let flush_ms = flush_started.elapsed().as_secs_f32() * 1_000.0;
            drop(window_surface);

            let present_started = Instant::now();
            let command_buffer = self
                .command_queue
                .commandBuffer()
                .ok_or_else(|| "create Metal presentation command buffer".to_owned())?;
            let drawable: Retained<ProtocolObject<dyn MTLDrawable>> = (&drawable).into();
            command_buffer.presentDrawable(&drawable);
            command_buffer.commit();
            Ok(WinitFrameTimings {
                acquire_ms,
                draw_ms,
                flush_ms,
                present_ms: present_started.elapsed().as_secs_f32() * 1_000.0,
            })
        })
    }

    pub(crate) fn trim(&mut self, pressure: MemoryPressure) {
        self.cache.trim(pressure);
        if pressure == MemoryPressure::Critical {
            self.skia_context.free_gpu_resources();
        }
    }

    pub(crate) fn set_cache_hooks(&mut self, hooks: Option<lgui_core::memory::CacheHooks>) {
        self.cache.set_hooks(hooks);
    }

    #[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
    pub(crate) fn cache_stats(&self) -> SkiaCacheStats {
        with_gpu_cache_usage(self.cache.stats(), &self.skia_context)
    }

    #[cfg(feature = "diagnostics")]
    pub(crate) fn device_info(&self) -> lgui_diagnostics::RendererDeviceInfo {
        lgui_diagnostics::RendererDeviceInfo {
            adapter_name: Some(self.adapter_name.clone()),
            api: "Metal".to_owned(),
            api_version: None,
            color_format: "BGRA8 UNORM".to_owned(),
            present_mode: "CAMetalLayer display sync".to_owned(),
        }
    }

    fn ensure_sized(&mut self, viewport: PhysicalRect) -> Result<(), String> {
        let size = (viewport.width().max(1), viewport.height().max(1));
        if size == self.scene_size {
            return Ok(());
        }
        self.skia_context.flush_and_submit();
        self.metal_layer
            .setDrawableSize(CGSize::new(size.0 as f64, size.1 as f64));
        let scene_surface = create_scene_surface(&mut self.skia_context, size)?;
        unsafe { ManuallyDrop::drop(&mut self.scene_surface) };
        self.scene_surface = ManuallyDrop::new(scene_surface);
        self.scene_size = size;
        self.cache.trim(MemoryPressure::Critical);
        Ok(())
    }
}

impl Drop for WinitMetalRenderer {
    fn drop(&mut self) {
        unsafe {
            ManuallyDrop::drop(&mut self.scene_surface);
            self.skia_context.release_resources_and_abandon();
            ManuallyDrop::drop(&mut self.skia_context);
        }
    }
}

fn create_scene_surface(
    context: &mut gpu::DirectContext,
    size: (i32, i32),
) -> Result<Surface, String> {
    surfaces::render_target(
        context,
        gpu::Budgeted::Yes,
        &ImageInfo::new_n32_premul(size, None),
        None,
        SurfaceOrigin::TopLeft,
        None,
        false,
        false,
    )
    .ok_or_else(|| "Skia could not create the retained Metal scene surface".to_owned())
}
