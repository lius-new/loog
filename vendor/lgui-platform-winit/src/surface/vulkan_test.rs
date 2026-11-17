use super::*;

#[test]
fn surface_format_prefers_unorm_srgb_color_space() {
    let formats = [
        vk::SurfaceFormatKHR {
            format: vk::Format::B8G8R8A8_SRGB,
            color_space: vk::ColorSpaceKHR::SRGB_NONLINEAR,
        },
        vk::SurfaceFormatKHR {
            format: vk::Format::B8G8R8A8_UNORM,
            color_space: vk::ColorSpaceKHR::SRGB_NONLINEAR,
        },
    ];
    assert!(choose_surface_format(&formats).unwrap().format == vk::Format::B8G8R8A8_UNORM);
}

#[test]
fn transparent_surface_rejects_opaque_only_composition() {
    assert!(choose_composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE, true).is_err());
}
