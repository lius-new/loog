use super::*;

#[test]
fn opaque_windows_do_not_require_an_alpha_channel() {
    assert!(supports_window_transparency(false, 0, Some(false)));
}

#[test]
fn transparent_windows_require_an_alpha_channel() {
    assert!(!supports_window_transparency(true, 0, Some(true)));
}

#[test]
fn transparent_window_policy_matches_the_platform_compositor() {
    #[cfg(target_os = "windows")]
    {
        assert!(!request_gl_config_transparency(true));
        assert!(supports_window_transparency(true, 8, Some(false)));
    }
    #[cfg(not(target_os = "windows"))]
    {
        assert!(request_gl_config_transparency(true));
        assert!(!supports_window_transparency(true, 8, Some(false)));
    }
}
