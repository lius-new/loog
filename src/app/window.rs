//! Window placement persistence and application close event coordination.
use crate::infrastructure::window_geometry as native;
use lgui::{WindowOptions, core::UiEventContext};
pub use native::{is_maximized, is_minimized, observe_viewport, remember_native_window};

pub fn scale_preference(percent: Option<u16>) -> lgui::platform::dpi::ScalePreference {
    use lgui::platform::dpi::ScalePreference;
    match crate::features::settings::scale::clamped(percent) {
        Some(percent) => ScalePreference::Multiplier(f32::from(percent) / 100.0),
        None => ScalePreference::Auto,
    }
}
#[derive(Clone, Copy, Debug)]
pub struct AppCloseRequested;

impl lgui::events::Event for AppCloseRequested {
    const NAME: &'static str = "loom.app_close_requested";
}
pub fn restore_options(options: WindowOptions) -> WindowOptions {
    native::restore_options(options, crate::app::workspace_session::load().window)
}
fn save_current() {
    let previous = crate::app::workspace_session::load().window;
    let geometry = native::capture(previous);
    let _ = crate::app::workspace_session::save_window_geometry(geometry);
}
pub fn handle_focus_change(focused: bool) {
    if focused {
        native::remember_native_window();
    } else {
        save_current();
    }
}
pub fn handle_close_requested(context: &mut UiEventContext) {
    save_current();
    context.emit(AppCloseRequested);
    context.request_frame();
}
