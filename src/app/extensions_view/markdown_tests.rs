//! Exercise package documents through the real shared parser, layout worker and reader.
use super::*;
use crate::app::{
    extensions::host::ExtensionHost, markdown_service::MarkdownService, state::AppState,
    test_support::SavingUi,
};
use crate::features::markdown::{model::NodeKind, reader::ReaderEvent};
use crate::ui::markdown::card::{Document, Frame};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Default)]
pub(super) struct Clipboard(Mutex<String>);
impl lgui::services::Clipboard for Clipboard {
    fn read_text(&self) -> Result<Option<String>, lgui::services::ClipboardError> {
        Ok(Some(self.0.lock().unwrap().clone()))
    }
    fn write_text(&self, text: &str) -> Result<(), lgui::services::ClipboardError> {
        *self.0.lock().unwrap() = text.to_owned();
        Ok(())
    }
}
struct Harness {
    ui: SavingUi,
    host: Arc<ExtensionHost>,
    directory: PathBuf,
    clipboard: Arc<Clipboard>,
}
impl Harness {
    fn new(name: &str, source: &str) -> Self {
        let directory =
            std::env::temp_dir().join(format!("loom-extension-docs-{name}-{}", std::process::id()));
        std::fs::create_dir_all(directory.join("docs")).unwrap();
        std::fs::write(directory.join("manifest.json"), r#"{"schema":1,"id":"test.docs","version":"1.0.0","readme":"docs/README.md","changelog":"docs/CHANGELOG.md"}"#).unwrap();
        std::fs::write(directory.join("docs/README.md"), source).unwrap();
        std::fs::write(directory.join("docs/CHANGELOG.md"), "# Changelog\n\n## 1.0.0\n\n**Fixed** ~~old~~ behavior.\n\n| Change | Status |\n| --- | --- |\n| Rendering | Done |\n").unwrap();
        image::RgbaImage::from_pixel(30, 10, image::Rgba([30, 210, 120, 255]))
            .save(directory.join("docs/asset.png"))
            .unwrap();
        let mut app = AppState::new();
        app.workspace.open_extensions();
        app.extensions_ui.show_installed("test.docs".to_owned());
        let ui = SavingUi::new(app);
        let clipboard = Arc::new(Clipboard::default());
        ui.runtime.install_markdown(MarkdownService::new(
            ui.state.clone(),
            ui.tasks.application(),
            clipboard.clone(),
        ));
        let host = ExtensionHost::start(
            ui.state.clone(),
            ui.tasks.application(),
            directory.join("cache"),
            ui.runtime.trust().clone(),
            None,
        )
        .unwrap();
        host.load_package(&directory).unwrap();
        ui.runtime.install_extensions(host);
        let host = ui.runtime.extensions().unwrap();
        Self {
            ui,
            host,
            directory,
            clipboard,
        }
    }
    fn document(&self, tab: DetailTab) -> Document {
        self.ui.state.update(|app| app.extensions_ui.tab = tab);
        let binding = binding(&self.ui.state);
        let frame = binding.frame();
        binding.documentation(&frame.installed[0], tab).unwrap()
    }
    fn ready(&self, document: &Document, predicate: impl Fn(&Frame) -> bool) -> Frame {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            self.ui.tasks.run_all();
            if let Some(frame) = (document.port.as_ref().unwrap().resolve)(560.0, 360.0).unwrap() {
                if predicate(&frame) {
                    return frame;
                }
            }
            assert!(
                Instant::now() < deadline,
                "extension Markdown did not become ready"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.ui.runtime.clear_markdown();
        self.ui.runtime.clear_extensions();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn extension_markdown_preserves_formatting_images_copy_and_anchor_navigation() {
    let source = format!(
        "# Extension guide\n\n**Bold** *italic* ~~deleted~~ and `inline`.\n\n[Jump](#destination) [Website](https://example.com)\n\n![Local asset](asset.png)\n\n> A quote.\n\n- [x] A task\n  - Nested item\n\n| Name | Value |\n| --- | --- |\n| Example | **Yes** |\n\n```rust\nlet answer = 42;\n```\n\n{}\n## Destination\n\nThe end.\n",
        "A paragraph that keeps all of its original Markdown source.\n\n".repeat(1300)
    );
    assert!(source.len() > 64 * 1024);
    let harness = Harness::new("rich", &source);
    let document = harness.document(DetailTab::Details);
    assert_eq!(document.content.text.as_str(), source);
    let frame = harness.ready(&document, |frame| {
        frame
            .reader
            .layout
            .images
            .iter()
            .any(|image| image.rect.bottom > image.rect.top)
    });
    let nodes = &frame.reader.document.nodes;
    for kind in [
        NodeKind::Strong,
        NodeKind::Emphasis,
        NodeKind::Strikethrough,
        NodeKind::Quote,
    ] {
        assert!(nodes.iter().any(|node| node.kind == kind));
    }
    assert!(
        nodes
            .iter()
            .any(|node| matches!(node.kind, NodeKind::Table { .. }))
    );
    assert!(nodes.iter().any(|node| matches!(
        node.kind,
        NodeKind::ListItem {
            task: Some(true),
            ..
        }
    )));
    let code = nodes.iter().position(|node| matches!(&node.kind, NodeKind::CodeBlock { language: Some(language), .. } if language == "rust")).unwrap();
    let port = document.port.as_ref().unwrap();
    let effect = (port.event)(ReaderEvent::CopyNode {
        key: frame.reader.key,
        node: crate::features::markdown::model::NodeId(code),
    });
    (port.copy)(effect.copy.unwrap());
    assert_eq!(*harness.clipboard.0.lock().unwrap(), "let answer = 42;\n");
    (port.event)(ReaderEvent::Focus(true));
    (port.event)(ReaderEvent::SelectAll {
        key: frame.reader.key,
    });
    let selected = (port.event)(ReaderEvent::Copy {
        key: frame.reader.key,
    })
    .copy
    .unwrap();
    assert!(selected.contains("Bold italic deleted"));
    assert!(!selected.contains("**Bold**"));
    assert!(
        selected.len() > 64 * 1024,
        "copy covers text outside the viewport"
    );

    // Native paragraph tiles and package-relative images enter the same UI surface.
    use lgui::{
        application::{AppView, ApplicationContext},
        core::UiScale,
        session::UiSession,
    };
    let context = ApplicationContext::empty(Default::default());
    let _fonts = lgui::backend::install_text_environment(
        &context,
        lgui::render_skia::skia_text_system_handle(),
    );
    let state = harness.ui.state.clone();
    let view: AppView = Arc::new(move |cx| {
        let search = cx.use_stable_id();
        let focus = cx.focus_handle(search.clone());
        crate::ui::extensions_page::render(
            UiRect::new(0.0, 0.0, 1280.0, 800.0),
            binding(&state),
            search,
            focus,
        )
    });
    let mut session = UiSession::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        harness.ui.tasks.run_all();
        // The application state is mounted in SavingUi's separate session.
        session.invalidate_all();
        session.render_view(&view, UiRect::new(0.0, 0.0, 1280.0, 800.0), UiScale::ONE);
        if session.tree().scene().image_requests().iter().any(|request| matches!(request.source(), lgui::core::UiImageSource::Bytes { key, .. } if key.starts_with("markdown-png-"))) { break; }
        assert!(
            Instant::now() < deadline,
            "the complete extension page did not render Markdown"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(session.tree().scene().image_requests().iter().any(|request| matches!(request.source(), lgui::core::UiImageSource::Bytes { key, .. } if key.starts_with("markdown-png-"))));
    assert!(session.tree().scene().image_requests().iter().any(|request| matches!(request.source(), lgui::core::UiImageSource::Bytes { key, .. } if key.starts_with("md-image-"))));

    let link = nodes.iter().position(|node| matches!(&node.kind, NodeKind::Link { destination, .. } if destination == "#destination")).unwrap();
    (port.event)(ReaderEvent::OpenLink {
        key: frame.reader.key,
        node: crate::features::markdown::model::NodeId(link),
    });
    harness.ready(&document, |frame| frame.reader.viewport.top > 360.0);

    let changelog = harness.document(DetailTab::Changelog);
    let changes = harness.ready(&changelog, |_| true);
    assert!(
        changes
            .reader
            .document
            .nodes
            .iter()
            .any(|node| matches!(node.kind, NodeKind::Table { .. }))
    );
    assert!(
        changes
            .reader
            .document
            .nodes
            .iter()
            .any(|node| node.kind == NodeKind::Strong)
    );
    assert!(
        (port.resolve)(560.0, 360.0).is_err(),
        "changing tabs releases the old reader"
    );
}

/// The readme of an extension reads like the Markdown preview: a page with
/// rules under the first two heading levels and boxed code, not the compact
/// layout of a hover card.
#[test]
fn a_readme_is_laid_out_as_a_page_like_the_markdown_preview() {
    let harness = Harness::new(
        "page",
        "# Title\n\nText.\n\n## Section\n\n```text\ncode\n```\n\n### Small\n",
    );
    let document = harness.document(DetailTab::Details);
    let frame = harness.ready(&document, |_| true);
    let layout = &frame.reader.layout;
    assert!(!layout.style.card, "not the compact card layout");
    assert_eq!(layout.style.padding, 12.0);
    let rules = layout
        .decorations
        .iter()
        .filter(|d| {
            matches!(
                d,
                crate::features::markdown::layout::Decoration::Rule { .. }
            )
        })
        .count();
    assert_eq!(
        rules, 2,
        "one under the h1 and one under the h2, none under the h3"
    );
    assert!(
        layout.decorations.iter().any(|d| matches!(
            d,
            crate::features::markdown::layout::Decoration::Code { .. }
        )),
        "code sits in a box"
    );
}

#[test]
fn reloading_documentation_replaces_the_reader_and_rejects_stale_events() {
    let harness = Harness::new("reload", "# Before\n\nOld documentation.");
    let old = harness.document(DetailTab::Details);
    let before = harness.ready(&old, |_| true);
    std::fs::write(
        harness.directory.join("docs/README.md"),
        "# After\n\n**New documentation.**",
    )
    .unwrap();
    harness.host.load_package(&harness.directory).unwrap();
    let updated = harness.document(DetailTab::Details);
    let after = harness.ready(&updated, |_| true);
    assert_ne!(old.content.identity, updated.content.identity);
    assert!(
        after
            .reader
            .document
            .display_text
            .contains("New documentation.")
    );
    let stale = old.port.as_ref().unwrap();
    assert!((stale.resolve)(560.0, 360.0).is_err());
    assert!(
        (stale.event)(ReaderEvent::SelectAll {
            key: before.reader.key
        })
        .selected
        .is_none()
    );
    harness
        .ui
        .state
        .update(|app| app.extensions_ui.selected = Some("other.extension".to_owned()));
    assert!((updated.port.as_ref().unwrap().resolve)(560.0, 360.0).is_err());
}
