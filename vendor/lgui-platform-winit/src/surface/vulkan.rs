use std::{
    ffi::{CStr, CString},
    mem::ManuallyDrop,
    os::raw::c_void,
    ptr,
    sync::Arc,
    time::Instant,
};

use ash::{vk, vk::Handle};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use skia_safe::{
    Color as SkColor, ColorType, ImageInfo, Paint, Rect, Surface,
    gpu::{self, SurfaceOrigin, backend_render_targets, direct_contexts, surfaces},
    surfaces as raster_surfaces,
};
use winit::window::Window;

use crate::WinitFrameTimings;
use lgui_core::core::Scene;
use lgui_render_api::{FrameInfo, MemoryPressure};
use lgui_render_skia::{
    SkiaCache, SkiaCacheStats, cpu_cache_budget, gpu_cache_budget, paint_scene_damage,
    with_gpu_cache_usage,
};

const VULKAN_API_VERSION: u32 = vk::make_api_version(0, 1, 1, 0);

pub(crate) struct WinitVulkanRenderer {
    scene_surface: ManuallyDrop<Surface>,
    skia_context: ManuallyDrop<gpu::DirectContext>,
    cache: SkiaCache,
    scene_size: (i32, i32),
    _entry: ash::Entry,
    instance: ash::Instance,
    surface_loader: ash::khr::surface::Instance,
    surface: vk::SurfaceKHR,
    physical_device: vk::PhysicalDevice,
    device: ash::Device,
    queue: vk::Queue,
    queue_family: u32,
    swapchain_loader: ash::khr::swapchain::Device,
    swapchain: vk::SwapchainKHR,
    swapchain_images: Vec<vk::Image>,
    swapchain_format: vk::Format,
    swapchain_extent: vk::Extent2D,
    acquire_fence: vk::Fence,
    transparent: bool,
    adapter_name: String,
    _window: Arc<Window>,
}

impl WinitVulkanRenderer {
    pub(crate) fn new(
        window: Arc<Window>,
        cache_budget: usize,
        transparent: bool,
    ) -> Result<Self, String> {
        let temporary_surface = raster_surfaces::raster_n32_premul((1, 1))
            .ok_or_else(|| "Skia could not create a temporary Vulkan surface".to_owned())?;
        let display_handle = window
            .display_handle()
            .map_err(|error| format!("read Vulkan display handle: {error}"))?;
        let window_handle = window
            .window_handle()
            .map_err(|error| format!("read Vulkan window handle: {error}"))?;
        let entry = unsafe { ash::Entry::load() }
            .map_err(|error| format!("load Vulkan runtime: {error}"))?;
        let required_extensions =
            ash_window::enumerate_required_extensions(display_handle.as_raw())
                .map_err(|error| format!("enumerate Vulkan surface extensions: {error:?}"))?;
        let application_name = CString::new("lgui").unwrap();
        let application_info = vk::ApplicationInfo::default()
            .application_name(&application_name)
            .application_version(1)
            .engine_name(&application_name)
            .engine_version(1)
            .api_version(VULKAN_API_VERSION);
        let instance_info = vk::InstanceCreateInfo::default()
            .application_info(&application_info)
            .enabled_extension_names(required_extensions);
        let instance = unsafe { entry.create_instance(&instance_info, None) }
            .map_err(|error| format!("create Vulkan instance: {error:?}"))?;
        let surface_loader = ash::khr::surface::Instance::new(&entry, &instance);
        let surface = match unsafe {
            ash_window::create_surface(
                &entry,
                &instance,
                display_handle.as_raw(),
                window_handle.as_raw(),
                None,
            )
        } {
            Ok(surface) => surface,
            Err(error) => {
                unsafe { instance.destroy_instance(None) };
                return Err(format!("create Vulkan window surface: {error:?}"));
            }
        };

        let selected = select_physical_device(&instance, &surface_loader, surface);
        let (physical_device, queue_family, adapter_name) = match selected {
            Ok(selected) => selected,
            Err(error) => {
                unsafe {
                    surface_loader.destroy_surface(surface, None);
                    instance.destroy_instance(None);
                }
                return Err(error);
            }
        };
        let queue_priority = [1.0_f32];
        let queue_info = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family)
            .queue_priorities(&queue_priority)];
        let device_extensions = [ash::khr::swapchain::NAME.as_ptr()];
        let device_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queue_info)
            .enabled_extension_names(&device_extensions);
        let device = match unsafe { instance.create_device(physical_device, &device_info, None) } {
            Ok(device) => device,
            Err(error) => {
                unsafe {
                    surface_loader.destroy_surface(surface, None);
                    instance.destroy_instance(None);
                }
                return Err(format!("create Vulkan device: {error:?}"));
            }
        };
        let queue = unsafe { device.get_device_queue(queue_family, 0) };
        let swapchain_loader = ash::khr::swapchain::Device::new(&instance, &device);
        let acquire_fence =
            match unsafe { device.create_fence(&vk::FenceCreateInfo::default(), None) } {
                Ok(fence) => fence,
                Err(error) => {
                    unsafe {
                        device.destroy_device(None);
                        surface_loader.destroy_surface(surface, None);
                        instance.destroy_instance(None);
                    }
                    return Err(format!("create Vulkan acquire fence: {error:?}"));
                }
            };

        let get_proc = |request: gpu::vk::GetProcOf| -> *const c_void {
            let function = unsafe {
                match request {
                    gpu::vk::GetProcOf::Instance(handle, name) => {
                        entry.get_instance_proc_addr(vk::Instance::from_raw(handle as _), name)
                    }
                    gpu::vk::GetProcOf::Device(handle, name) => {
                        instance.get_device_proc_addr(vk::Device::from_raw(handle as _), name)
                    }
                }
            };
            function.map_or(ptr::null(), |function| {
                function as *const () as *const c_void
            })
        };
        let instance_extension_names = required_extensions
            .iter()
            .map(|name| {
                unsafe { CStr::from_ptr(*name) }
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        let instance_extension_refs = instance_extension_names
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let backend = unsafe {
            gpu::vk::BackendContext::new_builder(
                instance.handle().as_raw() as _,
                physical_device.as_raw() as _,
                device.handle().as_raw() as _,
                (queue.as_raw() as _, queue_family as usize),
                &get_proc,
                Some(gpu::vk::Version::new(1, 1, 0)),
            )
            .with_extensions(&instance_extension_refs, &["VK_KHR_swapchain"])
            .build()
        };
        let mut skia_context = match direct_contexts::make_vulkan(&backend, None) {
            Some(context) => context,
            None => {
                unsafe {
                    device.destroy_fence(acquire_fence, None);
                    device.destroy_device(None);
                    surface_loader.destroy_surface(surface, None);
                    instance.destroy_instance(None);
                }
                return Err("Skia could not create a Vulkan DirectContext".to_owned());
            }
        };
        drop(backend);
        skia_context.set_resource_cache_limit(gpu_cache_budget(cache_budget));

        let mut renderer = Self {
            scene_surface: ManuallyDrop::new(temporary_surface),
            skia_context: ManuallyDrop::new(skia_context),
            cache: SkiaCache::new(cpu_cache_budget(cache_budget)),
            scene_size: (1, 1),
            _entry: entry,
            instance,
            surface_loader,
            surface,
            physical_device,
            device,
            queue,
            queue_family,
            swapchain_loader,
            swapchain: vk::SwapchainKHR::null(),
            swapchain_images: Vec::new(),
            swapchain_format: vk::Format::UNDEFINED,
            swapchain_extent: vk::Extent2D {
                width: 0,
                height: 0,
            },
            acquire_fence,
            transparent,
            adapter_name,
            _window: window,
        };
        renderer.recreate_swapchain()?;
        renderer.recreate_scene_surface()?;
        Ok(renderer)
    }

    pub(crate) fn draw_scene(
        &mut self,
        scene: &Scene,
        frame: &FrameInfo<'_>,
    ) -> Result<WinitFrameTimings, String> {
        let requested = (
            frame.viewport().width().max(1),
            frame.viewport().height().max(1),
        );
        if requested != self.scene_size
            || self.swapchain_extent.width != requested.0 as u32
            || self.swapchain_extent.height != requested.1 as u32
        {
            self.recreate_swapchain()?;
            self.recreate_scene_surface()?;
        }
        let paint_started = Instant::now();
        paint_scene_damage(self.scene_surface.canvas(), &mut self.cache, scene, frame)?;
        let paint_ms = paint_started.elapsed().as_secs_f32() * 1_000.0;

        let acquire_started = Instant::now();
        let (image_index, suboptimal) = self.acquire_next_image()?;
        let acquire_ms = acquire_started.elapsed().as_secs_f32() * 1_000.0;
        if suboptimal {
            self.recreate_swapchain()?;
            return Err("Vulkan swapchain became suboptimal during acquire".to_owned());
        }
        let image = self.swapchain_images[image_index as usize];
        let copy_started = Instant::now();
        let mut window_surface = self.surface_for_image(image)?;
        let snapshot = self.scene_surface.image_snapshot();
        let destination = Rect::from_wh(
            self.swapchain_extent.width as f32,
            self.swapchain_extent.height as f32,
        );
        window_surface.canvas().clear(SkColor::TRANSPARENT);
        window_surface
            .canvas()
            .draw_image_rect(snapshot, None, &destination, &Paint::default());
        let draw_ms = paint_ms + copy_started.elapsed().as_secs_f32() * 1_000.0;

        let flush_started = Instant::now();
        let present_state = gpu::vk::mutable_texture_states::new_vulkan(
            gpu::vk::ImageLayout::PRESENT_SRC_KHR,
            self.queue_family,
        );
        self.skia_context.flush_surface_with_texture_state(
            &mut window_surface,
            &gpu::FlushInfo::default(),
            Some(&present_state),
        );
        if !self.skia_context.submit(gpu::SubmitInfo {
            sync: gpu::SyncCpu::Yes,
            ..gpu::SubmitInfo::default()
        }) {
            return Err("submit Vulkan Skia work".to_owned());
        }
        let flush_ms = flush_started.elapsed().as_secs_f32() * 1_000.0;
        drop(window_surface);

        let present_started = Instant::now();
        let swapchains = [self.swapchain];
        let indices = [image_index];
        let present_info = vk::PresentInfoKHR::default()
            .swapchains(&swapchains)
            .image_indices(&indices);
        match unsafe {
            self.swapchain_loader
                .queue_present(self.queue, &present_info)
        } {
            Ok(suboptimal) if suboptimal => {
                self.recreate_swapchain()?;
            }
            Ok(_) => {}
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR | vk::Result::SUBOPTIMAL_KHR) => {
                self.recreate_swapchain()?;
            }
            Err(error) => return Err(format!("present Vulkan swapchain: {error:?}")),
        };
        Ok(WinitFrameTimings {
            acquire_ms,
            draw_ms,
            flush_ms,
            present_ms: present_started.elapsed().as_secs_f32() * 1_000.0,
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
            api: "Vulkan".to_owned(),
            api_version: Some("1.1".to_owned()),
            color_format: vulkan_format_name(self.swapchain_format).to_owned(),
            present_mode: "FIFO (vsync)".to_owned(),
        }
    }

    fn acquire_next_image(&mut self) -> Result<(u32, bool), String> {
        unsafe {
            self.device
                .reset_fences(&[self.acquire_fence])
                .map_err(|error| format!("reset Vulkan acquire fence: {error:?}"))?;
        }
        let acquired = unsafe {
            self.swapchain_loader.acquire_next_image(
                self.swapchain,
                u64::MAX,
                vk::Semaphore::null(),
                self.acquire_fence,
            )
        };
        let acquired = match acquired {
            Ok(acquired) => acquired,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                self.recreate_swapchain()?;
                unsafe {
                    self.device
                        .reset_fences(&[self.acquire_fence])
                        .map_err(|error| format!("reset Vulkan acquire fence: {error:?}"))?;
                }
                unsafe {
                    self.swapchain_loader.acquire_next_image(
                        self.swapchain,
                        u64::MAX,
                        vk::Semaphore::null(),
                        self.acquire_fence,
                    )
                }
                .map_err(|error| format!("acquire recreated Vulkan swapchain image: {error:?}"))?
            }
            Err(error) => return Err(format!("acquire Vulkan swapchain image: {error:?}")),
        };
        unsafe {
            self.device
                .wait_for_fences(&[self.acquire_fence], true, u64::MAX)
                .map_err(|error| format!("wait for Vulkan swapchain image: {error:?}"))?;
        }
        Ok(acquired)
    }

    fn recreate_swapchain(&mut self) -> Result<(), String> {
        unsafe {
            self.device
                .device_wait_idle()
                .map_err(|error| format!("wait for Vulkan device before resize: {error:?}"))?;
        }
        let capabilities = unsafe {
            self.surface_loader
                .get_physical_device_surface_capabilities(self.physical_device, self.surface)
        }
        .map_err(|error| format!("query Vulkan surface capabilities: {error:?}"))?;
        let formats = unsafe {
            self.surface_loader
                .get_physical_device_surface_formats(self.physical_device, self.surface)
        }
        .map_err(|error| format!("query Vulkan surface formats: {error:?}"))?;
        let surface_format = choose_surface_format(&formats)?;
        let requested = self._window.inner_size();
        let extent = if capabilities.current_extent.width != u32::MAX {
            capabilities.current_extent
        } else {
            vk::Extent2D {
                width: requested.width.max(1).clamp(
                    capabilities.min_image_extent.width,
                    capabilities.max_image_extent.width,
                ),
                height: requested.height.max(1).clamp(
                    capabilities.min_image_extent.height,
                    capabilities.max_image_extent.height,
                ),
            }
        };
        let mut image_count = capabilities.min_image_count.saturating_add(1).max(2);
        if capabilities.max_image_count > 0 {
            image_count = image_count.min(capabilities.max_image_count);
        }
        let composite_alpha =
            choose_composite_alpha(capabilities.supported_composite_alpha, self.transparent)?;
        let create_info = vk::SwapchainCreateInfoKHR::default()
            .surface(self.surface)
            .min_image_count(image_count)
            .image_format(surface_format.format)
            .image_color_space(surface_format.color_space)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(capabilities.current_transform)
            .composite_alpha(composite_alpha)
            .present_mode(vk::PresentModeKHR::FIFO)
            .clipped(true)
            .old_swapchain(self.swapchain);
        let new_swapchain = unsafe { self.swapchain_loader.create_swapchain(&create_info, None) }
            .map_err(|error| format!("create Vulkan swapchain: {error:?}"))?;
        let images = match unsafe { self.swapchain_loader.get_swapchain_images(new_swapchain) } {
            Ok(images) => images,
            Err(error) => {
                unsafe { self.swapchain_loader.destroy_swapchain(new_swapchain, None) };
                return Err(format!("enumerate Vulkan swapchain images: {error:?}"));
            }
        };
        if self.swapchain != vk::SwapchainKHR::null() {
            unsafe {
                self.swapchain_loader
                    .destroy_swapchain(self.swapchain, None)
            };
        }
        self.swapchain = new_swapchain;
        self.swapchain_images = images;
        self.swapchain_format = surface_format.format;
        self.swapchain_extent = extent;
        Ok(())
    }

    fn recreate_scene_surface(&mut self) -> Result<(), String> {
        let size = (
            self.swapchain_extent.width.max(1) as i32,
            self.swapchain_extent.height.max(1) as i32,
        );
        let info = ImageInfo::new_n32_premul(size, None);
        let surface = surfaces::render_target(
            &mut self.skia_context,
            gpu::Budgeted::Yes,
            &info,
            None,
            SurfaceOrigin::TopLeft,
            None,
            false,
            false,
        )
        .ok_or_else(|| "Skia could not create the retained Vulkan scene surface".to_owned())?;
        unsafe { ManuallyDrop::drop(&mut self.scene_surface) };
        self.scene_surface = ManuallyDrop::new(surface);
        self.scene_size = size;
        self.cache.trim(MemoryPressure::Critical);
        Ok(())
    }

    fn surface_for_image(&mut self, image: vk::Image) -> Result<Surface, String> {
        let (skia_format, color_type) = skia_surface_format(self.swapchain_format)?;
        let info = unsafe {
            gpu::vk::ImageInfo::new(
                image.as_raw() as _,
                gpu::vk::Alloc::default(),
                gpu::vk::ImageTiling::OPTIMAL,
                gpu::vk::ImageLayout::PRESENT_SRC_KHR,
                skia_format,
                1,
                self.queue_family,
                None,
                None,
                Some(gpu::vk::SharingMode::EXCLUSIVE),
            )
        };
        let target = backend_render_targets::make_vk(
            (
                self.swapchain_extent.width as i32,
                self.swapchain_extent.height as i32,
            ),
            &info,
        );
        surfaces::wrap_backend_render_target(
            &mut self.skia_context,
            &target,
            SurfaceOrigin::TopLeft,
            color_type,
            None,
            None,
        )
        .ok_or_else(|| "Skia could not wrap the Vulkan swapchain image".to_owned())
    }
}

impl Drop for WinitVulkanRenderer {
    fn drop(&mut self) {
        let device_idle = unsafe { self.device.device_wait_idle() }.is_ok();
        if !device_idle {
            self.skia_context.abandon();
        }
        unsafe {
            ManuallyDrop::drop(&mut self.scene_surface);
            if device_idle {
                self.skia_context.release_resources_and_abandon();
            }
            ManuallyDrop::drop(&mut self.skia_context);
            self.device.destroy_fence(self.acquire_fence, None);
            if self.swapchain != vk::SwapchainKHR::null() {
                self.swapchain_loader
                    .destroy_swapchain(self.swapchain, None);
            }
            self.device.destroy_device(None);
            self.surface_loader.destroy_surface(self.surface, None);
            self.instance.destroy_instance(None);
        }
    }
}

fn select_physical_device(
    instance: &ash::Instance,
    surface_loader: &ash::khr::surface::Instance,
    surface: vk::SurfaceKHR,
) -> Result<(vk::PhysicalDevice, u32, String), String> {
    let devices = unsafe { instance.enumerate_physical_devices() }
        .map_err(|error| format!("enumerate Vulkan physical devices: {error:?}"))?;
    let mut selected = None;
    for physical_device in devices {
        let extensions = unsafe { instance.enumerate_device_extension_properties(physical_device) }
            .map_err(|error| format!("enumerate Vulkan device extensions: {error:?}"))?;
        let supports_swapchain = extensions.iter().any(|extension| unsafe {
            CStr::from_ptr(extension.extension_name.as_ptr()) == ash::khr::swapchain::NAME
        });
        if !supports_swapchain {
            continue;
        }
        let properties = unsafe { instance.get_physical_device_properties(physical_device) };
        let queue_families =
            unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
        let queue_family = queue_families
            .iter()
            .enumerate()
            .find_map(|(index, family)| {
                if !family.queue_flags.contains(vk::QueueFlags::GRAPHICS) {
                    return None;
                }
                unsafe {
                    surface_loader.get_physical_device_surface_support(
                        physical_device,
                        index as u32,
                        surface,
                    )
                }
                .ok()
                .filter(|supported| *supported)
                .map(|_| index as u32)
            });
        let Some(queue_family) = queue_family else {
            continue;
        };
        let name = unsafe { CStr::from_ptr(properties.device_name.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        let score = match properties.device_type {
            vk::PhysicalDeviceType::DISCRETE_GPU => 3,
            vk::PhysicalDeviceType::INTEGRATED_GPU => 2,
            vk::PhysicalDeviceType::VIRTUAL_GPU => 1,
            _ => 0,
        };
        if selected
            .as_ref()
            .is_none_or(|(current, _, _, _)| score > *current)
        {
            selected = Some((score, physical_device, queue_family, name));
        }
    }
    selected
        .map(|(_, device, queue, name)| (device, queue, name))
        .ok_or_else(|| "no Vulkan device supports graphics and presentation".to_owned())
}

fn choose_surface_format(formats: &[vk::SurfaceFormatKHR]) -> Result<vk::SurfaceFormatKHR, String> {
    formats
        .iter()
        .copied()
        .find(|format| {
            matches!(
                format.format,
                vk::Format::B8G8R8A8_UNORM | vk::Format::R8G8B8A8_UNORM
            ) && format.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR
        })
        .or_else(|| {
            formats.iter().copied().find(|format| {
                matches!(
                    format.format,
                    vk::Format::B8G8R8A8_SRGB
                        | vk::Format::R8G8B8A8_SRGB
                        | vk::Format::B8G8R8A8_UNORM
                        | vk::Format::R8G8B8A8_UNORM
                )
            })
        })
        .ok_or_else(|| "the Vulkan surface has no Skia-compatible RGBA format".to_owned())
}

fn vulkan_format_name(format: vk::Format) -> &'static str {
    match format {
        vk::Format::B8G8R8A8_UNORM => "BGRA8 UNORM",
        vk::Format::B8G8R8A8_SRGB => "BGRA8 SRGB",
        vk::Format::R8G8B8A8_UNORM => "RGBA8 UNORM",
        vk::Format::R8G8B8A8_SRGB => "RGBA8 SRGB",
        _ => "unknown",
    }
}

fn choose_composite_alpha(
    supported: vk::CompositeAlphaFlagsKHR,
    transparent: bool,
) -> Result<vk::CompositeAlphaFlagsKHR, String> {
    let candidates = if transparent {
        [
            vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED,
            vk::CompositeAlphaFlagsKHR::POST_MULTIPLIED,
            vk::CompositeAlphaFlagsKHR::INHERIT,
            vk::CompositeAlphaFlagsKHR::OPAQUE,
        ]
    } else {
        [
            vk::CompositeAlphaFlagsKHR::OPAQUE,
            vk::CompositeAlphaFlagsKHR::INHERIT,
            vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED,
            vk::CompositeAlphaFlagsKHR::POST_MULTIPLIED,
        ]
    };
    let selected = candidates
        .into_iter()
        .find(|candidate| supported.contains(*candidate))
        .ok_or_else(|| "the Vulkan surface exposes no composite-alpha mode".to_owned())?;
    if transparent && selected == vk::CompositeAlphaFlagsKHR::OPAQUE {
        return Err("the Vulkan surface does not support transparent composition".to_owned());
    }
    Ok(selected)
}

fn skia_surface_format(format: vk::Format) -> Result<(gpu::vk::Format, ColorType), String> {
    match format {
        vk::Format::B8G8R8A8_UNORM => Ok((gpu::vk::Format::B8G8R8A8_UNORM, ColorType::BGRA8888)),
        vk::Format::B8G8R8A8_SRGB => Ok((gpu::vk::Format::B8G8R8A8_SRGB, ColorType::BGRA8888)),
        vk::Format::R8G8B8A8_UNORM => Ok((gpu::vk::Format::R8G8B8A8_UNORM, ColorType::RGBA8888)),
        vk::Format::R8G8B8A8_SRGB => Ok((gpu::vk::Format::R8G8B8A8_SRGB, ColorType::RGBA8888)),
        _ => Err(format!(
            "unsupported Vulkan surface format: {}",
            format.as_raw()
        )),
    }
}

#[cfg(test)]
#[path = "vulkan_test.rs"]
mod tests;
