use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use super::*;

#[test]
fn software_backend_rejects_an_explicit_unavailable_driver() {
    let backend = WinitApplication::new(GraphicsPreference::Vulkan);
    assert_eq!(backend.preference, GraphicsPreference::Vulkan);
}

#[test]
fn repeated_frame_requests_preserve_the_earliest_deadline() {
    let now = Instant::now();
    let original = now + Duration::from_millis(16);

    assert_eq!(
        next_frame_deadline(Some(original), now + Duration::from_millis(4), Some(16)),
        Some(original)
    );
    assert_eq!(
        next_frame_deadline(Some(original), now + Duration::from_millis(4), Some(4)),
        Some(now + Duration::from_millis(8))
    );
}

#[test]
fn mouse_buttons_keep_extended_button_identity() {
    assert_eq!(pointer_button(MouseButton::Back), PointerButton::Back);
    assert_eq!(
        pointer_button(MouseButton::Other(9)),
        PointerButton::Other(9)
    );
}

#[test]
fn auto_recovery_is_bounded_before_software_fallback() {
    assert_eq!(
        recovery_preference(GraphicsPreference::Auto, false, true, 1),
        (GraphicsPreference::Auto, false)
    );
    assert_eq!(
        recovery_preference(GraphicsPreference::Auto, false, true, 2),
        (GraphicsPreference::Software, true)
    );
}

#[test]
fn explicit_gpu_recovery_never_silently_falls_back() {
    assert_eq!(
        recovery_preference(GraphicsPreference::OpenGl, false, true, 3),
        (GraphicsPreference::OpenGl, false)
    );
}

#[test]
fn driver_support_matches_compiled_winit_backends() {
    assert_eq!(
        graphics_preference_supported(GraphicsPreference::Auto),
        cfg!(feature = "renderer-skia")
    );
    assert_eq!(
        graphics_preference_supported(GraphicsPreference::Software),
        cfg!(feature = "renderer-skia")
    );
    assert_eq!(
        graphics_preference_supported(GraphicsPreference::OpenGl),
        cfg!(feature = "renderer-skia-gl")
    );
    assert_eq!(
        graphics_preference_supported(GraphicsPreference::Vulkan),
        cfg!(all(
            feature = "renderer-skia-vulkan",
            any(target_os = "windows", target_os = "linux")
        ))
    );
    assert_eq!(
        graphics_preference_supported(GraphicsPreference::Metal),
        cfg!(all(feature = "renderer-skia-metal", target_os = "macos"))
    );
}

#[test]
fn auto_driver_order_matches_each_platform_policy() {
    let order = auto_driver_order();
    assert_eq!(
        order.last(),
        cfg!(feature = "renderer-skia").then_some(&GraphicsPreference::Software)
    );
    #[cfg(target_os = "windows")]
    assert_eq!(
        order.first(),
        if cfg!(feature = "renderer-skia-gl") {
            Some(&GraphicsPreference::OpenGl)
        } else if cfg!(feature = "renderer-skia") {
            Some(&GraphicsPreference::Software)
        } else {
            None
        }
    );
    #[cfg(target_os = "linux")]
    assert_eq!(
        order.first(),
        if cfg!(feature = "renderer-skia-vulkan") {
            Some(&GraphicsPreference::Vulkan)
        } else if cfg!(feature = "renderer-skia-gl") {
            Some(&GraphicsPreference::OpenGl)
        } else if cfg!(feature = "renderer-skia") {
            Some(&GraphicsPreference::Software)
        } else {
            None
        }
    );
    #[cfg(target_os = "macos")]
    assert_eq!(
        order.first(),
        if cfg!(feature = "renderer-skia-metal") {
            Some(&GraphicsPreference::Metal)
        } else if cfg!(feature = "renderer-skia-gl") {
            Some(&GraphicsPreference::OpenGl)
        } else if cfg!(feature = "renderer-skia") {
            Some(&GraphicsPreference::Software)
        } else {
            None
        }
    );
}

#[test]
fn auxiliary_windows_inherit_the_main_owner_and_application_context() {
    let context = ApplicationContext::empty(lgui_core::memory::test_memory_options());
    let expected = context.clone();
    let rendered = Arc::new(AtomicBool::new(false));
    let rendered_view = Arc::clone(&rendered);
    let view: AppView = Arc::new(move |cx| {
        assert!(cx.application() == expected);
        rendered_view.store(true, Ordering::Relaxed);
        lgui_core::core::content_text("auxiliary")
    });
    let (options, view) = prepare_auxiliary_window(
        &context,
        &WindowId::new("main"),
        WindowOptions::new("chat"),
        view,
    );

    assert_eq!(options.owner, Some(WindowId::new("main")));
    let mut session = UiSession::new();
    let _ = session.render_view(&view, UiRect::new(0.0, 0.0, 320.0, 240.0), UiScale::ONE);
    assert!(rendered.load(Ordering::Relaxed));
}

#[test]
fn auxiliary_windows_preserve_an_explicit_owner() {
    let context = ApplicationContext::empty(lgui_core::memory::test_memory_options());
    let view: AppView = Arc::new(|_| lgui_core::core::content_text("auxiliary"));
    let (options, _) = prepare_auxiliary_window(
        &context,
        &WindowId::new("main"),
        WindowOptions::new("chat").owner("workspace"),
        view,
    );

    assert_eq!(options.owner, Some(WindowId::new("workspace")));
}
