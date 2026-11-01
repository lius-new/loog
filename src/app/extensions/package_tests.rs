//! The sample extensions in `tests/extensions/packages`, loaded the way the
//! application loads them: a theme extension and a language extension with a
//! real WebAssembly grammar. They prove that themes, syntax highlighting and
//! loading work for an extension that was written from scratch.
use super::*;
use crate::features::highlighting::HighlightId;
use crate::features::settings::theme_id::{REGISTRY_TURN, ThemeId};
use crate::infrastructure::language_packs::{
    Packages, check_extension_pack, extension_packs, packs_in,
};
use std::path::Path;
use std::sync::Arc;

fn samples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/extensions/packages")
}

fn service(name: &str) -> ExtensionService {
    ExtensionService::new(
        std::env::temp_dir().join(format!("loom-ext-cache-{name}-{}", std::process::id())),
    )
}

#[test]
fn the_sample_theme_extension_loads_and_both_themes_draw_with_their_own_colors() {
    let _turn = REGISTRY_TURN.lock().unwrap_or_else(|e| e.into_inner());
    let mut service = service("midnight");
    let id = service
        .load_package(&samples().join("loom-midnight"))
        .unwrap();
    let package = service.package(&id).unwrap();
    assert!(!package.manifest.has_behaviour(), "data only: no component");
    assert_eq!(
        package.resources.themes,
        ["loom.midnight.dark", "loom.midnight.light"]
    );
    assert!(
        package.resources.rejected.is_empty(),
        "{:?}",
        package.resources.rejected
    );

    let themes = service.themes();
    let names: Vec<(&str, &str)> = themes
        .iter()
        .map(|t| (t.0.as_str(), t.1.as_str()))
        .collect();
    assert_eq!(
        names,
        [
            ("loom.midnight.dark", "Loom Midnight"),
            ("loom.midnight.light", "Loom Daybreak")
        ]
    );

    crate::ui::theme::set_package_themes(themes);
    let dark = ThemeId::parse("loom.midnight.dark").expect("offered by id");
    let light = ThemeId::parse("loom.midnight.light").expect("offered by id");
    let (dark_palette, light_palette) = (&dark.theme().palette, &light.theme().palette);
    assert_eq!(dark_palette.bg.0, 0x0e1320);
    assert_eq!(light_palette.bg.0, 0xfdfbf7);
    // What the file does not name is the base theme's, so nothing is left undefined.
    let aura_dark = ThemeId::default().theme();
    assert_eq!(dark_palette.ansi, aura_dark.palette.ansi);
    assert_eq!(dark_palette.syntax_color(HighlightId::KEYWORD).0, 0xc4b5fd);
    assert_eq!(light_palette.syntax_color(HighlightId::KEYWORD).0, 0x7c3aed);
    let comment = dark_palette
        .syntax
        .resolve(HighlightId::COMMENT, Default::default());
    assert_eq!((comment.fg, comment.italic), (Some(0x64748b), Some(true)));
    assert_ne!(
        dark_palette.syntax_color(HighlightId::STRING).0,
        aura_dark.palette.syntax_color(HighlightId::STRING).0
    );

    // Stopping the extension takes its themes away; a selection draws as the default.
    service.stop(&id);
    crate::ui::theme::set_package_themes(service.themes());
    assert!(ThemeId::parse("loom.midnight.dark").is_none());
    assert_eq!(dark.theme().id, ThemeId::default().theme().id);
    crate::ui::theme::set_package_themes(Vec::new());
}

#[test]
fn the_sample_language_extension_is_found_at_startup_and_highlights_with_its_module() {
    let root = samples();
    let package = root.join("loom-properties");

    // As a package it is accepted whole: the grammar pack and the snippets.
    let mut service = service("properties");
    let id = service.load_package(&package).unwrap();
    let loaded = service.package(&id).unwrap();
    assert_eq!(loaded.resources.languages, ["loom.properties.grammar"]);
    assert_eq!(loaded.resources.snippets, ["loom.properties.snippets"]);
    assert!(
        loaded.resources.rejected.is_empty(),
        "{:?}",
        loaded.resources.rejected
    );
    assert!(check_extension_pack(&package, "grammar/tree-sitter.json").is_ok());

    // Startup discovery finds the pack inside it (and nothing in a data-only package).
    let found = packs_in(vec![package.clone()], &Default::default());
    assert_eq!(found, [package.join("grammar").canonicalize().unwrap()]);
    let nowhere = std::env::temp_dir().join(format!("loom-no-packs-{}", std::process::id()));
    let packages = Arc::new(Packages::discover_with(&nowhere, &found, None));
    let registry = packages.registry().unwrap();
    let resolved = registry.resolve_path(Path::new("server.properties"));
    assert_eq!(registry.definition_of(resolved).name.as_ref(), "properties");
    let grammar = registry.grammar_name(resolved.grammar.expect("a grammar"));
    assert_eq!(grammar, "properties");
    assert_eq!(
        packages.activity().wasm_loads,
        0,
        "nothing loads until needed"
    );

    // The WebAssembly grammar and its queries highlight real text.
    let text = "# server\nserver.port=8080\npath=C:\\\\tmp\nhome=${HOME}\n";
    let spans = crate::features::syntax::highlight_with(&*packages, grammar, text).unwrap();
    assert_eq!(packages.activity().wasm_loads, 1);
    assert_eq!(
        spans.iter().map(|s| s.range.len()).sum::<usize>(),
        text.len()
    );
    let class_of = |needle: &str| {
        let start = text.find(needle).unwrap();
        spans
            .iter()
            .find(|s| s.range.start <= start && start < s.range.end)
            .and_then(|s| s.class)
    };
    assert_eq!(class_of("# server"), Some(HighlightId::COMMENT));
    assert_eq!(class_of("server.port"), Some(HighlightId::PROPERTY));
    assert_eq!(class_of("8080"), Some(HighlightId::STRING));

    // The theme extension colors those classes; the language knows nothing about colors.
    let midnight = crate::ui::theme_package::parse(
        &std::fs::read_to_string(root.join("loom-midnight/themes/midnight.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        midnight.palette.syntax_color(HighlightId::PROPERTY).0,
        0xa5b4fc
    );
    assert_eq!(
        midnight.palette.syntax_color(HighlightId::STRING).0,
        0x86efac
    );
}

/// What a person sees of an extension before and after installing it: all of it
/// comes from the manifest and the files it names.
#[test]
fn the_samples_are_packed_with_the_script_and_import_with_everything_to_show() {
    let _turn = REGISTRY_TURN.lock().unwrap_or_else(|e| e.into_inner());
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let work = std::env::temp_dir().join(format!("loom-pack-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    let installed = work.join("extensions");
    let mut packed = Vec::new();
    for folder in ["loom-midnight", "loom-properties"] {
        let output = std::process::Command::new("node")
            .arg(root.join("scripts/extensions/package/pack.mjs"))
            .arg(samples().join(folder))
            .arg("--out")
            .arg(work.join("out"))
            .output()
            .expect("node is available");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        packed.push(
            std::fs::read_dir(work.join("out"))
                .unwrap()
                .flatten()
                .map(|e| e.path())
                .find(|p| {
                    p.extension().is_some_and(|x| x == "loomext")
                        && p.file_name().is_some_and(|n| {
                            n.to_string_lossy()
                                .contains(&folder.replace("loom-", "loom."))
                        })
                })
                .expect("the package was written"),
        );
    }

    let mut service = service("packed");
    let ui = crate::app::test_support::SavingUi::new(AppState::new());
    let host = host::ExtensionHost::start(
        ui.state.clone(),
        ui.tasks.application(),
        work.join("ui-cache"),
        ui.runtime.trust().clone(),
        None,
    )
    .unwrap();
    ui.runtime.install_extensions(host);
    let host = ui.runtime.extensions().unwrap();
    for archive in &packed {
        let imported = crate::infrastructure::extensions::package::import(
            archive,
            &installed,
            env!("CARGO_PKG_VERSION"),
        )
        .unwrap();
        // The sidecar the script wrote is the digest the importer computed.
        let sidecar = std::fs::read_to_string(format!("{}.sha256", archive.display())).unwrap();
        assert!(sidecar.starts_with(&imported.sha256), "{sidecar}");
        let id = service.load_package(&imported.directory).unwrap();
        let package = service.package(&id).unwrap();
        assert!(
            package.resources.rejected.is_empty(),
            "{:?}",
            package.resources.rejected
        );
        let m = &package.manifest;
        // Everything the extensions page shows is there, and the files it points at too.
        for field in [
            &m.name,
            &m.publisher,
            &m.description,
            &m.license,
            &m.repository,
        ] {
            assert!(field.as_deref().is_some_and(|text| !text.is_empty()));
        }
        assert!(!m.categories.is_empty());
        for path in [&m.icon, &m.readme, &m.changelog] {
            let file = imported.directory.join(path.as_ref().unwrap());
            assert!(std::fs::metadata(file).unwrap().len() > 0);
        }
        assert!(m.runs_on(env!("CARGO_PKG_VERSION")));

        // Importing from any list clears filters and opens the actual package's detail.
        ui.state.update(|app| {
            use crate::ui::extensions_binding::{DetailTab, StateFilter};
            app.extensions_ui.search = crate::ui::components::input::InputState::new("unrelated");
            app.extensions_ui.filter = StateFilter::Disabled;
            app.extensions_ui.tab = DetailTab::Changelog;
        });
        host.import_into(&ui.state, archive, &work.join("ui-extensions"));
        let app = ui.state.get();
        use crate::ui::extensions_binding::{DetailTab, StateFilter};
        assert_eq!(app.extensions_ui.selected.as_deref(), Some(id.as_str()));
        assert!(app.extensions_ui.search.text().is_empty());
        assert_eq!(app.extensions_ui.filter, StateFilter::All);
        assert_eq!(app.extensions_ui.tab, DetailTab::Details);
        assert_eq!(
            app.workspace.active_page(),
            Some(crate::app::workspace::AppPage::Extensions)
        );
        assert_eq!(
            app.chrome.toast.as_ref().unwrap().kind,
            crate::domain::notification::ToastKind::Success
        );
        let shown = crate::app::extensions_view::binding(&ui.state).frame();
        let detail = shown.installed.iter().find(|entry| entry.id == id).unwrap();
        assert_eq!(Some(detail.name.as_str()), m.name.as_deref());
        assert!(detail.icon_image.is_some(), "packed artwork reaches the UI");
        assert!(!detail.readme_document.as_ref().unwrap().content.is_empty());
        assert!(
            !detail
                .changelog_document
                .as_ref()
                .unwrap()
                .content
                .is_empty()
        );
        assert!(detail.contribution_count() > 0);
    }
    // Re-importing replaces one entry and selects it rather than adding a duplicate.
    host.import_into(&ui.state, &packed[0], &work.join("ui-extensions"));
    assert_eq!(host.frame().installed.len(), 2);
    let selected = ui.state.get().extensions_ui.selected.clone();
    let invalid = work.join("broken.loomext");
    std::fs::write(&invalid, b"not an extension package").unwrap();
    host.import_into(&ui.state, &invalid, &work.join("ui-extensions"));
    let app = ui.state.get();
    assert_eq!(app.extensions_ui.selected, selected);
    assert_eq!(
        app.chrome.toast.as_ref().unwrap().kind,
        crate::domain::notification::ToastKind::Error
    );
    assert!(
        app.chrome
            .toast
            .as_ref()
            .unwrap()
            .message
            .contains("Could not import")
    );
    assert_eq!(host.frame().installed.len(), 2);
    // A valid archive with invalid theme data loads the package but reports the failure.
    let broken_theme = work.join("broken-theme.loomext");
    let encoder = flate2::write::GzEncoder::new(
        std::fs::File::create(&broken_theme).unwrap(),
        flate2::Compression::default(),
    );
    let mut tar = tar::Builder::new(encoder);
    for file in [
        "manifest.json",
        "icon.svg",
        "README.md",
        "CHANGELOG.md",
        "themes/midnight.json",
        "themes/daybreak.json",
    ] {
        let bytes = if file.starts_with("themes/") {
            b"invalid theme".to_vec()
        } else {
            std::fs::read(samples().join("loom-midnight").join(file)).unwrap()
        };
        let mut header = tar::Header::new_ustar();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, format!("loom.midnight/{file}"), bytes.as_slice())
            .unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap();
    host.import_into(&ui.state, &broken_theme, &work.join("ui-extensions"));
    let app = ui.state.get();
    assert_eq!(app.extensions_ui.selected.as_deref(), Some("loom.midnight"));
    assert_eq!(
        app.chrome.toast.as_ref().unwrap().kind,
        crate::domain::notification::ToastKind::Error
    );
    assert!(
        app.chrome
            .toast
            .as_ref()
            .unwrap()
            .message
            .contains("could not load")
    );
    assert!(
        host.frame()
            .installed
            .iter()
            .find(|entry| entry.id == "loom.midnight")
            .unwrap()
            .installed
            .as_ref()
            .unwrap()
            .warning
            .is_some()
    );
    ui.runtime.clear_extensions();
    crate::ui::theme::set_package_themes(Vec::new());
    // The language pack inside the installed extension is found at startup.
    assert_eq!(extension_packs(&installed).len(), 1);
    let _ = std::fs::remove_dir_all(&work);
}
