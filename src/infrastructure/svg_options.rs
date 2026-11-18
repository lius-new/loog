//! Share system fonts across SVG consumers, discovering them only for text.
use resvg::usvg::{self, fontdb};
use std::sync::{Arc, OnceLock};

fn system_fonts() -> Arc<fontdb::Database> {
    static FONTS: OnceLock<Arc<fontdb::Database>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut fonts = fontdb::Database::new();
            fonts.load_system_fonts();
            Arc::new(fonts)
        })
        .clone()
}

pub(crate) fn new() -> usvg::Options<'static> {
    with_fonts(system_fonts)
}

fn with_fonts(
    load: impl Fn() -> Arc<fontdb::Database> + Send + Sync + 'static,
) -> usvg::Options<'static> {
    let mut options = usvg::Options::default();
    let select = usvg::FontResolver::default_font_selector();
    options.font_resolver.select_font = Box::new(move |font, database| {
        // The parser calls this for text, never for path-only artwork. Our base
        // database is empty: no font IDs exist before this initial population.
        // Afterwards retain it, including any fonts a parser adds dynamically.
        if database.is_empty() {
            *database = load();
        }
        select(font, database)
    });
    options
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn paths_skip_font_discovery_and_text_matches_eager_system_fonts() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let lazy = with_fonts(move || {
            observed.fetch_add(1, Ordering::Relaxed);
            system_fonts()
        });
        let path = r#"<svg xmlns="http://www.w3.org/2000/svg" width="160" height="40"><path d="M0 0H20V20H0Z" fill="red"/></svg>"#;
        let render = |source: &str, options: &usvg::Options| {
            let tree = usvg::Tree::from_str(source, options).unwrap();
            let mut pixels = resvg::tiny_skia::Pixmap::new(160, 40).unwrap();
            resvg::render(
                &tree,
                resvg::tiny_skia::Transform::identity(),
                &mut pixels.as_mut(),
            );
            pixels.data().to_vec()
        };
        let path_pixels = render(path, &lazy);
        assert!(path_pixels.iter().any(|byte| *byte != 0));
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        let eager = usvg::Options {
            fontdb: system_fonts(),
            ..Default::default()
        };
        // Installed fonts need not match the requested/default families (for example,
        // Linux without Arial or Times New Roman). Compare against eager rendering
        // even when both produce no glyphs.
        let has_fonts = !system_fonts().is_empty();
        for source in [
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="160" height="40"><text x="2" y="26" font-family="sans-serif" font-size="18">Hello 中文</text></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="160" height="40"><text x="2" y="26" font-family="missing-font" font-size="18">Fallback 0689</text></svg>"#,
        ] {
            let actual = render(source, &lazy);
            assert_eq!(actual, render(source, &eager));
        }
        // Without fonts the database stays empty, so discovery may repeat.
        let discoveries = calls.load(Ordering::Relaxed);
        assert!(if has_fonts {
            discoveries == 2
        } else {
            discoveries >= 2
        });
    }
}
