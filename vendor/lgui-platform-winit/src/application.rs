use super::*;

#[cfg(feature = "store")]
use lgui_store::StoreApplicationExt as _;

#[derive(Debug)]
pub struct WinitApplicationError(pub(super) String);

impl fmt::Display for WinitApplicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for WinitApplicationError {}

impl From<winit::error::EventLoopError> for WinitApplicationError {
    fn from(error: winit::error::EventLoopError) -> Self {
        Self(error.to_string())
    }
}

#[derive(Clone, Copy, Debug)]
pub struct WinitApplication {
    pub(super) preference: GraphicsPreference,
}

impl WinitApplication {
    pub fn new(preference: GraphicsPreference) -> Self {
        Self { preference }
    }
}

impl ApplicationBackend for WinitApplication {
    type Error = WinitApplicationError;

    fn run(
        self,
        options: WindowOptions,
        view: AppView,
        context: ApplicationContext,
    ) -> Result<(), Self::Error> {
        if !graphics_preference_supported(self.preference) {
            return Err(WinitApplicationError(format!(
                "the winit backend does not support the explicit {} driver on this target",
                self.preference.as_str()
            )));
        }

        let event_loop = EventLoop::<WinitUserEvent>::with_user_event()
            .build()
            .map_err(|error| WinitApplicationError(error.to_string()))?;
        let proxy = event_loop.create_proxy();
        lgui_core::backend::install_window_commands(&context.windows(), {
            let proxy = proxy.clone();
            move |command| {
                let _ = proxy.send_event(WinitUserEvent::Window(command));
            }
        });
        let application_handle = ApplicationHandle::new(
            {
                let proxy = proxy.clone();
                move |task| {
                    let _ = proxy.send_event(WinitUserEvent::Task(task));
                }
            },
            {
                let proxy = proxy.clone();
                move || {
                    let _ = proxy.send_event(WinitUserEvent::RequestAllFrames);
                }
            },
        );
        context.resources().provide(application_handle.clone());
        #[cfg(feature = "images")]
        if context
            .try_resource::<lgui_assets::ImageCacheHandle>()
            .is_none()
        {
            let loader = context
                .try_resource::<lgui_assets::RemoteImageLoaderHandle>()
                .map(|loader| (*loader).clone())
                .unwrap_or_else(lgui_assets::http_image_loader);
            let image_wake = application_handle.clone();
            let budget = context.memory().options().budget.cache_bytes;
            let cache = lgui_assets::backend::async_image_cache(
                loader,
                move || image_wake.request_frame(),
                budget,
                context.memory().clone(),
            );
            context.resources().provide(cache);
        }
        #[cfg(feature = "store")]
        context.stores().set_wake({
            let proxy = proxy.clone();
            Arc::new(move || {
                let _ = proxy.send_event(WinitUserEvent::RequestAllFrames);
            })
        });
        #[cfg(all(feature = "notifications-win32", target_os = "windows"))]
        lgui_platform_win32::install_notification_service(&context)
            .map_err(|error| WinitApplicationError(error.to_string()))?;
        #[cfg(feature = "renderer-skia")]
        let _text_environment = lgui_core::backend::install_text_environment(
            &context,
            lgui_render_skia::skia_text_system_handle(),
        );

        let display = event_loop.owned_display_handle();
        let soft_context =
            Arc::new(SoftContext::new(display).map_err(|error| {
                WinitApplicationError(format!("create software display: {error}"))
            })?);
        let main_id = options.id.clone();
        let mut handler = WinitHost {
            initial: Some((options, view)),
            main_id,
            context,
            proxy,
            soft_context,
            windows: HashMap::new(),
            ids: HashMap::new(),
            scale_preference: None,
            preference: self.preference,
            #[cfg(all(feature = "tray-win32", target_os = "windows"))]
            tray_host: None,
            exit_requested: false,
            backgrounded: false,
        };
        event_loop.run_app(&mut handler)?;
        Ok(())
    }
}
