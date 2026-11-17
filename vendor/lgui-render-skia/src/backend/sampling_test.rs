use super::*;
use lgui_core::core::ImageSampling;

fn sample(image: &Image, width: i32, sampling: ImageSampling) -> Vec<u8> {
    let mut surface = surfaces::raster_n32_premul((width, 1)).unwrap();
    surface.canvas().clear(SkColor::WHITE);
    draw_image(
        surface.canvas(),
        image,
        UiRect::new(0.0, 0.0, width as f32, 1.0),
        ImageFit::Fill,
        sampling,
        None,
    );
    let info = ImageInfo::new((width, 1), ColorType::RGBA8888, AlphaType::Premul, None);
    let mut pixels = vec![0; width as usize * 4];
    assert!(surface.read_pixels(&info, &mut pixels, width as usize * 4, (0, 0)));
    pixels
}

#[test]
fn filtering_preserves_native_pixels_and_integrates_downscaled_detail() {
    let mut source = surfaces::raster_n32_premul((4, 1)).unwrap();
    source.canvas().clear(SkColor::WHITE);
    let mut black = Paint::default();
    black.set_color(SkColor::BLACK);
    for x in [0, 2] {
        source
            .canvas()
            .draw_rect(Rect::from_xywh(x as f32, 0.0, 1.0, 1.0), &black);
    }
    let image = source.image_snapshot();
    let exact = sample(&image, 4, ImageSampling::Nearest);
    assert_eq!(exact, sample(&image, 4, ImageSampling::Linear));
    let nearest = sample(&image, 3, ImageSampling::Nearest);
    let filtered = sample(&image, 3, ImageSampling::Linear);
    assert_ne!(nearest, filtered);
    assert!(
        nearest
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| p[0] == 0 || p[0] == 255)
    );
    assert!(
        filtered
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[0] > 0 && p[0] < 255)
    );
    assert_ne!(nearest, sample(&image, 3, ImageSampling::Mitchell));
}

#[test]
fn sampling_changes_request_identity_without_duplicating_decoded_source() {
    use lgui_core::core::ImageRequest;
    use std::hash::{DefaultHasher, Hash, Hasher};
    let request = ImageRequest::new(UiImageSource::bytes("shared", 1, Arc::new(vec![1, 2, 3])));
    let filtered = request.clone().sampling(ImageSampling::Linear);
    assert_eq!(request.cache_key(), filtered.cache_key());
    // Requests are hashed into scene signatures: changing sampling must cause
    // retained rendering to invalidate even if source and bounds are unchanged.
    let hash = |r: &ImageRequest| {
        let mut h = DefaultHasher::new();
        r.hash(&mut h);
        h.finish()
    };
    assert_ne!(hash(&request), hash(&filtered));
}
