use super::*;

#[cfg(test)]
pub(super) fn layer_surface(width: f32, height: f32) -> Result<Surface, String> {
    layer_surface_with_options(width, height, Default::default())
}

pub(super) fn layer_surface_with_options(
    width: f32,
    height: f32,
    options: lgui_core::text::TextRenderingOptions,
) -> Result<Surface, String> {
    let info = ImageInfo::new_n32_premul(
        (width.ceil().max(1.0) as i32, height.ceil().max(1.0) as i32),
        None,
    );
    let props = TextRasterization::Grayscale.surface_props_with_options(options);
    surfaces::raster(&info, None, Some(&props))
        .ok_or_else(|| "Skia could not create an offscreen layer".to_owned())
}

pub(super) fn draw_rect(canvas: &Canvas, rect: UiRect, style: VisualStyle) {
    let area = sk_rect(rect);
    if let Some(fill) = style.fill {
        let paint = color_paint(fill, style.fill_alpha);
        if style.radius > 0.0 {
            canvas.draw_rrect(RRect::new_rect_xy(area, style.radius, style.radius), &paint);
        } else {
            canvas.draw_rect(area, &paint);
        }
    }
    if let Some(stroke) = style.stroke {
        let paint = stroke_paint(stroke);
        if style.radius > 0.0 {
            canvas.draw_rrect(RRect::new_rect_xy(area, style.radius, style.radius), &paint);
        } else {
            canvas.draw_rect(area, &paint);
        }
    }
}

pub(super) fn draw_ellipse(canvas: &Canvas, rect: UiRect, style: VisualStyle) {
    if let Some(fill) = style.fill {
        canvas.draw_oval(sk_rect(rect), &color_paint(fill, style.fill_alpha));
    }
    if let Some(stroke) = style.stroke {
        canvas.draw_oval(sk_rect(rect), &stroke_paint(stroke));
    }
}

pub(super) fn draw_path(canvas: &Canvas, path: &UiPath, style: PathStyle) {
    let path = sk_path(path);
    if let Some(fill) = style.fill {
        canvas.draw_path(&path, &color_paint(fill, style.fill_alpha));
    }
    if let Some(stroke) = style.stroke {
        canvas.draw_path(&path, &stroke_paint(stroke));
    }
}

pub(super) fn draw_image(
    canvas: &Canvas,
    image: &Image,
    rect: UiRect,
    fit: ImageFit,
    sampling: lgui_core::core::ImageSampling,
    paint: Option<&Paint>,
) {
    let image_size = (image.width() as f32, image.height() as f32);
    let destination = fitted_rect(rect, image_size, fit);
    let default_paint = Paint::default();
    if fit == ImageFit::Cover {
        canvas.save();
        canvas.clip_rect(sk_rect(rect), None, true);
    }
    canvas.draw_image_rect_with_sampling_options(
        image,
        None,
        sk_rect(destination),
        match sampling {
            lgui_core::core::ImageSampling::Nearest => SamplingOptions::default(),
            lgui_core::core::ImageSampling::Linear => skia_safe::FilterMode::Linear.into(),
            lgui_core::core::ImageSampling::Mitchell => {
                skia_safe::CubicResampler::mitchell().into()
            }
        },
        paint.unwrap_or(&default_paint),
    );
    if fit == ImageFit::Cover {
        canvas.restore();
    }
}

pub(super) fn draw_glow(canvas: &Canvas, rect: UiRect, color: Color, alpha: u8) {
    let center = (
        rect.left + rect.width() / 2.0,
        rect.top + rect.height() / 2.0,
    );
    let colors = [
        sk_color_f(color, alpha as f32 / 255.0),
        sk_color_f(color, 0.0),
    ];
    let positions = [0.0, 1.0];
    let gradient = skia_safe::gradient::Gradient::new(
        skia_safe::gradient::Colors::new(
            colors.as_slice(),
            Some(positions.as_slice()),
            TileMode::Clamp,
            None,
        ),
        skia_safe::gradient::Interpolation::default(),
    );
    if let Some(shader) = skia_safe::gradient::shaders::radial_gradient(
        (center, rect.width().max(rect.height()) / 2.0),
        &gradient,
        None,
    ) {
        let mut paint = Paint::default();
        paint.set_shader(shader);
        canvas.draw_rect(sk_rect(rect), &paint);
    }
}

pub(super) fn draw_overlay(canvas: &Canvas, rect: UiRect, style: &lgui_core::core::OverlayStyle) {
    for layer in &style.vertical_layers {
        let colors = [
            sk_color_f(layer.color, layer.alpha_top),
            sk_color_f(layer.color, layer.alpha_bottom),
        ];
        let gradient = skia_safe::gradient::Gradient::new(
            skia_safe::gradient::Colors::new_evenly_spaced(
                colors.as_slice(),
                TileMode::Clamp,
                None,
            ),
            skia_safe::gradient::Interpolation::default(),
        );
        if let Some(shader) = skia_safe::gradient::shaders::linear_gradient(
            ((rect.left, rect.top), (rect.left, rect.bottom)),
            &gradient,
            None,
        ) {
            let mut paint = Paint::default();
            paint.set_shader(shader);
            canvas.draw_rect(sk_rect(rect), &paint);
        }
    }
    for layer in &style.radial_layers {
        let center = (
            rect.left + rect.width() * layer.center_x,
            rect.top + rect.height() * layer.center_y,
        );
        let radius = rect.width().min(rect.height()) * layer.radius;
        let colors = [
            sk_color_f(layer.color, layer.alpha),
            sk_color_f(layer.color, 0.0),
        ];
        let gradient = skia_safe::gradient::Gradient::new(
            skia_safe::gradient::Colors::new_evenly_spaced(
                colors.as_slice(),
                TileMode::Clamp,
                None,
            ),
            skia_safe::gradient::Interpolation::default(),
        );
        if let Some(shader) =
            skia_safe::gradient::shaders::radial_gradient((center, radius), &gradient, None)
        {
            let mut paint = Paint::default();
            paint.set_shader(shader);
            canvas.draw_rect(sk_rect(rect), &paint);
        }
    }
}

pub(super) fn draw_composited(
    canvas: &Canvas,
    image: &Image,
    rect: UiRect,
    opacity: u8,
    transform: LayerTransform,
) {
    canvas.save();
    let origin = (
        rect.left + rect.width() * transform.origin_x(),
        rect.top + rect.height() * transform.origin_y(),
    );
    canvas.translate((
        origin.0 + transform.translation_x(),
        origin.1 + transform.translation_y(),
    ));
    canvas.rotate(transform.rotation_degrees_f32(), None);
    canvas.scale((transform.scale_x(), transform.scale_y()));
    canvas.translate((-origin.0, -origin.1));
    let mut paint = Paint::default();
    paint.set_alpha(opacity);
    let destination = sk_rect(rect);
    canvas.draw_image_rect(image, None, &destination, &paint);
    canvas.restore();
}

pub(super) fn custom_scene(
    key: &str,
    rect: UiRect,
    style: lgui_core::core::CustomPaintStyle,
) -> Result<Option<lgui_assets::SceneFragment>, String> {
    let Some(provider) = render_resources().custom_paint().cloned() else {
        return Ok(None);
    };
    provider
        .record(key, rect, style)
        .map_err(|error| error.to_string())
}

pub(super) fn fitted_rect(bounds: UiRect, image: (f32, f32), fit: ImageFit) -> UiRect {
    if fit == ImageFit::Fill || image.0 <= 0.0 || image.1 <= 0.0 {
        return bounds;
    }
    let scale = match fit {
        ImageFit::Contain => (bounds.width() / image.0).min(bounds.height() / image.1),
        ImageFit::Cover => (bounds.width() / image.0).max(bounds.height() / image.1),
        ImageFit::Fill => 1.0,
    };
    let width = image.0 * scale;
    let height = image.1 * scale;
    UiRect::new(
        bounds.left + (bounds.width() - width) / 2.0,
        bounds.top + (bounds.height() - height) / 2.0,
        bounds.left + (bounds.width() + width) / 2.0,
        bounds.top + (bounds.height() + height) / 2.0,
    )
}

pub(super) fn sk_path(path: &UiPath) -> Path {
    let mut result = PathBuilder::new();
    for command in path.commands() {
        match command {
            UiPathCommand::MoveTo(point) => {
                result.move_to((point.x, point.y));
            }
            UiPathCommand::LineTo(point) => {
                result.line_to((point.x, point.y));
            }
            UiPathCommand::QuadraticTo { control, to } => {
                result.quad_to((control.x, control.y), (to.x, to.y));
            }
            UiPathCommand::CubicTo {
                control1,
                control2,
                to,
            } => {
                result.cubic_to(
                    (control1.x, control1.y),
                    (control2.x, control2.y),
                    (to.x, to.y),
                );
            }
            UiPathCommand::Close => {
                result.close();
            }
        }
    }
    result.detach()
}

pub(super) fn stroke_paint(stroke: Stroke) -> Paint {
    let mut paint = color_paint(stroke.color, stroke.alpha);
    paint.set_style(PaintStyle::Stroke);
    paint.set_stroke_width(stroke.width.max(0.0));
    paint
}

pub(super) fn color_paint(color: Color, alpha: u8) -> Paint {
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_color(sk_color(color, alpha));
    paint
}

pub(super) fn sk_color(color: Color, alpha: u8) -> SkColor {
    SkColor::from_argb(
        alpha,
        ((color.0 >> 16) & 0xFF) as u8,
        ((color.0 >> 8) & 0xFF) as u8,
        (color.0 & 0xFF) as u8,
    )
}

pub(super) fn sk_color_f(color: Color, alpha: f32) -> Color4f {
    Color4f::new(
        ((color.0 >> 16) & 0xFF) as f32 / 255.0,
        ((color.0 >> 8) & 0xFF) as f32 / 255.0,
        (color.0 & 0xFF) as f32 / 255.0,
        alpha.clamp(0.0, 1.0),
    )
}

pub(super) fn sk_rect(rect: UiRect) -> Rect {
    Rect::new(rect.left, rect.top, rect.right, rect.bottom)
}

pub(super) fn physical_rect(rect: PhysicalRect) -> Rect {
    Rect::new(
        rect.left as f32,
        rect.top as f32,
        rect.right as f32,
        rect.bottom as f32,
    )
}

pub(super) fn ui_rect_from_physical(rect: PhysicalRect) -> UiRect {
    UiRect::new(
        rect.left as f32,
        rect.top as f32,
        rect.right as f32,
        rect.bottom as f32,
    )
}

pub(super) fn image_key(source: &UiImageSource) -> String {
    match source {
        UiImageSource::Static(key) => format!("asset:{key}"),
        UiImageSource::File(path) => format!("file:{}", path.display()),
        UiImageSource::Url(url) => format!("url:{url}"),
        UiImageSource::Bytes { key, version, .. } => format!("bytes:{key}:{version}"),
    }
}

pub(super) fn tint_svg(svg: &str, color: Color, alpha: u8) -> String {
    let hex = format!("#{:06X}", color.0 & 0xFFFFFF);
    svg.replace("currentColor", &hex)
        .replace("currentOpacity", &format!("{:.6}", alpha as f32 / 255.0))
}
