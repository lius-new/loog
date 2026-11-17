# lgui-render-skia

Skia rendering backend for [LGUI](https://github.com/lius-new/lgui).

This crate owns scene painting, retained renderer caches, software surfaces,
text layout, and the shared support used by GPU surfaces.

Most applications should depend on the `lgui` facade crate.

## Software text rendering

The Winit presenter selects an opaque software target for nontransparent windows.
Standalone offscreen consumers retain transparent pixels with
`SkiaSoftwareSurface::new`; opaque presenters can use `new_opaque`.

On Windows, opaque unscaled targets follow the system's font-smoothing and RGB/BGR
pixel-order settings. Unknown pixel geometry, transparent text/layers and
transformed targets use grayscale antialiasing. Font hinting remains `Slight`,
and subpixel positioning remains enabled; enabling LCD does not change metrics.
Standalone `SkiaSoftwareSurface` instances keep transparent behavior by default.

Applications can supply explicit LCD coverage parameters for each window:

```rust,ignore
let options = lgui::text::TextRenderingOptions::default()
    .with_lcd_gamma(gamma)?
    .with_lcd_contrast(contrast)?;
let window = lgui::WindowOptions::new("editor").text_rendering(options);
// Update one existing window and request a full repaint:
application.windows().set_text_rendering("editor", options);
```

Defaults preserve the backend's coverage curve. Applications choose overrides;
the renderer has no theme names, background-brightness heuristic or theme-specific
gamma. Values are validated before reaching Skia. Gamma changes only glyph-edge
blending, preserving font weight, size, foreground colors and advances. LCD-only
overrides leave grayscale and transparent fallback targets unchanged. Currently
these overrides apply to software LCD rendering; GPU drivers retain their defaults.

Configuration belongs to each window/renderer instance. Changing one window does
not alter other windows or applications. Standalone surfaces support
`set_text_rendering(options)`. A coverage or pixel-order change refreshes all
retained software pixels and presents the full viewport; shaped paragraphs remain
reusable. Renderer recovery reapplies the window's parameters on its next frame.

Canvas properties use the output-parameter `Canvas::props()` binding. The
Skia 0.99 by-value `Canvas::top_props()` binding faults on Windows/MSVC; the
opaque first-frame regression test covers the native software drawing path.

Run the headless renderer regressions with:

```text
cargo test -p lgui-render-skia --lib
```
