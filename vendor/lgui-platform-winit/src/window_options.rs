use winit::window::Icon;

pub use winit::window::{BadIcon as WinitIconError, Icon as WinitWindowIcon};

/// Winit-specific options applied when a native window is created.
///
/// Store this value in [`lgui_core::WindowOptions`] with
/// [`lgui_core::WindowOptions::with_platform_options`]. Keeping these options
/// in the Winit backend avoids leaking native window policy into `lgui-core`.
#[derive(Clone, Debug, Default)]
pub struct WinitWindowOptions {
    pub(crate) window_icon: Option<Icon>,
    #[cfg(target_os = "windows")]
    pub(crate) taskbar_icon: Option<Icon>,
    #[cfg(target_os = "windows")]
    pub(crate) drag_and_drop: Option<bool>,
}

impl WinitWindowOptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Enables Winit's OLE file-drop target on Windows. The default is unchanged.
    /// Disable this when the application installs its own native drop handler.
    #[cfg(target_os = "windows")]
    pub fn drag_and_drop(mut self, enabled: bool) -> Self {
        self.drag_and_drop = Some(enabled);
        self
    }

    /// Sets the regular window icon (`ICON_SMALL` on Windows).
    pub fn window_icon(mut self, icon: WinitWindowIcon) -> Self {
        self.window_icon = Some(icon);
        self
    }

    /// Sets the Windows taskbar icon (`ICON_BIG`).
    #[cfg(target_os = "windows")]
    pub fn taskbar_icon(mut self, icon: WinitWindowIcon) -> Self {
        self.taskbar_icon = Some(icon);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn icon(size: u32) -> WinitWindowIcon {
        WinitWindowIcon::from_rgba(vec![0; (size * size * 4) as usize], size, size)
            .expect("valid test icon")
    }

    #[test]
    fn stores_window_icon() {
        let options = WinitWindowOptions::new().window_icon(icon(16));
        assert!(options.window_icon.is_some());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn stores_taskbar_icon() {
        let options = WinitWindowOptions::new().taskbar_icon(icon(32));
        assert!(options.taskbar_icon.is_some());
    }
}
