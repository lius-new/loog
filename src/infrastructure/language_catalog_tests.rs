use super::*;
use crate::features::syntax::Resources;
use std::sync::atomic::AtomicUsize;

fn bundled() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    if env!("CARGO_PKG_NAME") == "loom-language-runtime-tests" {
        root.join("../../assets/extensions")
    } else {
        root.join("assets/extensions")
    }
}
fn archive() -> Option<Vec<u8>> {
    let root = bundled();
    if !root.is_dir() {
        return None;
    } // The standalone host also tests with all language resources removed.
    let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::default(),
    ));
    // The bundled JSON extension, repacked as the registry's `json` archive.
    for file in [
        "tree-sitter.json",
        "tree-sitter-json.wasm",
        "queries/highlights.scm",
        "LICENSE",
    ] {
        let path = format!("json/{file}");
        let bytes = fs::read(root.join("loom.lang.json").join(file)).unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive
            .append_data(&mut header, path, bytes.as_slice())
            .unwrap();
    }
    Some(archive.into_inner().unwrap().finish().unwrap())
}
fn catalog_file(root: &Path, bytes: &[u8]) -> PathBuf {
    let file = root.join("registry.toml");
    fs::write(
        &file,
        format!(
            r#"schema = 1
[distribution]
repository = "lius-new/loom"
[packages.json]
name = "JSON"
asset = "json.tar.gz"
sha256 = "{}"
size = {}
release = "language-packs-test"
[packages.json.grammars.json]
file-types = ["json", "jsonc", "geojson", "webmanifest"]
"#,
            sha(bytes),
            bytes.len()
        ),
    )
    .unwrap();
    file
}
fn source(root: &Path, bytes: &[u8], download: Arc<Download>) -> Arc<Packages> {
    let file = catalog_file(root, bytes);
    let catalog = Catalog::read(&file).unwrap();
    let descriptions = catalog.descriptions().unwrap();
    Arc::new(Packages {
        packages: BTreeMap::new(),
        loaded: Mutex::default(),
        catalog: Some(Manager::with_download(
            catalog,
            descriptions,
            Some(root.join("cache")),
            Some(root.join("extensions")),
            download,
        )),
        layering: Default::default(),
    })
}
fn wait(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(
            Instant::now() < deadline,
            "language installation did not finish"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn ready(source: &Packages) {
    wait(|| {
        for info in source.download_infos() {
            assert!(info.error.is_none(), "{}: {:?}", info.name, info.error);
        }
        source.generation() > 0
    });
}

#[test]
fn catalog_registers_missing_languages_without_loading_or_downloading() {
    let root = Temporary::new(&std::env::temp_dir()).unwrap();
    let Some(bytes) = archive() else {
        return;
    };
    let source = source(
        &root.0,
        &bytes,
        Arc::new(|_, _| panic!("startup must not download")),
    );
    let registry = source.registry().unwrap();
    assert_eq!(
        registry
            .definition(registry.resolve_path(Path::new("file.json")).language)
            .name
            .as_ref(),
        "json"
    );
    assert_eq!(source.activity(), Activity::default());
    assert!(source.download_infos().is_empty());
    assert!(!root.0.join("cache").exists());
}

#[test]
fn publication_catalog_is_valid_and_respects_installed_resources() {
    let root = Temporary::new(&std::env::temp_dir()).unwrap();
    let file = bundled().join("registry.toml");
    if !file.exists() {
        return;
    }
    let catalog = Catalog::read(&file).unwrap();
    assert_eq!(catalog.packages.len(), 50);
    let bundled = file.parent().unwrap();
    let mut source = Packages::discover(bundled, None);
    source
        .enable_catalog(
            &file,
            Some(root.0.join("cache")),
            Some(root.0.join("extensions")),
        )
        .unwrap();
    let registry = source.registry().unwrap();
    for path in [
        "main.rs",
        "test.py",
        "component.tsx",
        "CMakeLists.txt",
        "data.json",
    ] {
        assert_ne!(
            registry
                .definition(registry.resolve_path(Path::new(path)).language)
                .name
                .as_ref(),
            "plaintext",
            "{path}"
        );
    }
    assert!(
        !source
            .catalog
            .as_ref()
            .unwrap()
            .descriptions
            .contains_key("json")
    );
    assert!(
        !source
            .catalog
            .as_ref()
            .unwrap()
            .descriptions
            .contains_key("markdown")
    );
    assert_eq!(source.activity(), Activity::default());
}

#[test]
fn a_downloaded_language_becomes_an_extension_with_a_manifest_and_is_found_after_restart() {
    let root = Temporary::new(&std::env::temp_dir()).unwrap();
    let Some(bytes) = archive() else {
        return;
    };
    let download = bytes.clone();
    let source = source(&root.0, &bytes, Arc::new(move |_, _| Ok(download.clone())));
    assert!(source.language("json").is_err(), "it is downloading");
    ready(&source);
    assert!(source.language("json").is_ok());

    // Version 0.0.0 is what a release without a date gets.
    let folder = root.0.join("extensions/loom.lang.json/0.0.0");
    let recorded = &crate::infrastructure::extensions::state::read(&root.0.join("extensions"))
        .extensions["loom.lang.json"];
    assert!(matches!(
        recorded.source,
        crate::infrastructure::extensions::state::Source::Registry { .. }
    ));
    let manifest = crate::features::extensions::manifest::Manifest::parse(
        &fs::read_to_string(folder.join("manifest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest.id, "loom.lang.json");
    assert_eq!(manifest.contributes.languages[0].path, "tree-sitter.json");
    assert_eq!(manifest.version, "0.0.0", "a test release has no date");
    // The next start finds it as any other extension's language pack.
    assert_eq!(
        extension_packs(&root.0.join("extensions")),
        [folder.canonicalize().unwrap()]
    );
}

#[test]
fn a_registry_install_never_replaces_an_extension_the_user_installed() {
    let root = Temporary::new(&std::env::temp_dir()).unwrap();
    let Some(bytes) = archive() else {
        return;
    };
    let mine = root.0.join("extensions/loom.lang.json");
    fs::create_dir_all(&mine).unwrap();
    fs::write(mine.join("note.txt"), "mine").unwrap();
    let download = bytes.clone();
    let source = source(&root.0, &bytes, Arc::new(move |_, _| Ok(download.clone())));
    assert!(source.language("json").is_err());
    wait(|| source.download_infos().iter().any(|i| i.error.is_some()));
    let error = source.download_infos()[0].error.clone().unwrap();
    assert!(error.contains("already installed"), "{error}");
    assert_eq!(fs::read_to_string(mine.join("note.txt")).unwrap(), "mine");
    assert!(!mine.join("manifest.json").exists());
}

#[test]
fn demand_is_deduplicated_and_hot_activation_recovers_cached_plain_text() {
    use crate::{
        domain::{identity::FileId, language::Registry},
        features::syntax::{Source, TreeIndex, Window},
    };
    use loom_editor_core::text::TextBuffer;
    let root = Temporary::new(&std::env::temp_dir()).unwrap();
    let Some(bytes) = archive() else {
        return;
    };
    let count = Arc::new(AtomicUsize::new(0));
    let requests = count.clone();
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let download_release = release.clone();
    let fixture_bytes = bytes.clone();
    let source = source(
        &root.0,
        &fixture_bytes,
        Arc::new(move |url, size| {
            assert!(url.ends_with("/language-packs-test/json.tar.gz"));
            assert_eq!(size, bytes.len() as u64);
            requests.fetch_add(1, Ordering::SeqCst);
            let (lock, wake) = &*download_release;
            let mut ready = lock.lock().unwrap();
            while !*ready {
                ready = wake.wait(ready).unwrap();
            }
            Ok(bytes.clone())
        }),
    );
    let registry = Registry::builtin();
    let lang = registry.resolve_path(Path::new("test.json"));
    assert!(lang.grammar.is_some());
    let buffer = TextBuffer::new("{\"hello\": true}");
    let text = Source::new(&buffer);
    let window = Window {
        line: 0..text.text.len(),
        range: 0..text.text.len(),
    };
    let mut trees = TreeIndex::new(source.clone());
    let render = |trees: &mut TreeIndex| {
        trees
            .prepare(
                FileId::new(1),
                text.clone(),
                lang,
                std::slice::from_ref(&window),
                &std::sync::atomic::AtomicBool::new(false),
            )
            .unwrap()[0]
            .spans
            .iter()
            .filter(|span| span.class.is_some())
            .count()
    };
    assert_eq!(render(&mut trees), 0);
    for _ in 0..10 {
        assert!(source.language("json").is_err());
    }
    wait(|| count.load(Ordering::SeqCst) == 1);
    *release.0.lock().unwrap() = true;
    release.1.notify_all();
    ready(&source);
    assert!(render(&mut trees) > 0);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert!(
        source
            .download_infos()
            .iter()
            .all(|info| info.status == "ready")
    );
}

#[test]
fn invalid_download_is_not_activated_and_manual_retry_succeeds() {
    let root = Temporary::new(&std::env::temp_dir()).unwrap();
    let Some(bytes) = archive() else {
        return;
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let requests = calls.clone();
    let fixture_bytes = bytes.clone();
    let source = source(
        &root.0,
        &fixture_bytes,
        Arc::new(move |_, _| {
            if requests.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(vec![0])
            } else {
                Ok(bytes.clone())
            }
        }),
    );
    assert!(source.language("json").is_err());
    wait(|| source.download_infos().iter().any(|i| i.error.is_some()));
    assert_eq!(source.generation(), 0);
    for _ in 0..10 {
        assert!(source.language("json").is_err());
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    source.retry_downloads();
    ready(&source);
    assert!(source.language("json").is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn cached_pinned_archives_allow_installation_without_network_on_restart() {
    let root = Temporary::new(&std::env::temp_dir()).unwrap();
    let Some(bytes) = archive() else {
        return;
    };
    let returned = bytes.clone();
    let first = source(&root.0, &bytes, Arc::new(move |_, _| Ok(returned.clone())));
    assert!(first.language("json").is_err());
    ready(&first);
    let second = source(
        &root.0,
        &bytes,
        Arc::new(|_, _| panic!("verified cache must work offline")),
    );
    assert!(second.language("json").is_err());
    ready(&second);
    assert!(second.language("json").is_ok());
    // Damaged extracted files are repaired from the pinned archive, still offline.
    let installed = second.catalog.as_ref().unwrap().request("json").unwrap();
    fs::write(
        installed.root.join("queries/highlights.scm"),
        "corrupt cache",
    )
    .unwrap();
    let third = source(
        &root.0,
        &bytes,
        Arc::new(|_, _| panic!("cache repair must work offline")),
    );
    assert!(third.language("json").is_err());
    ready(&third);
    assert!(third.language("json").is_ok());
}

#[test]
fn injected_grammar_is_installed_only_when_its_region_is_visible() {
    use crate::{
        domain::{identity::FileId, language::Registry},
        features::syntax::{Source, TreeIndex, Window},
    };
    use loom_editor_core::text::TextBuffer;
    fn copy(source: &Path, destination: &Path) {
        fs::create_dir_all(destination).unwrap();
        for entry in fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let output = destination.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy(&entry.path(), &output);
            } else {
                fs::copy(entry.path(), output).unwrap();
            }
        }
    }
    let root = Temporary::new(&std::env::temp_dir()).unwrap();
    let Some(bytes) = archive() else {
        return;
    };
    let fixture_bytes = bytes.clone();
    copy(
        &bundled().join("loom.lang.markdown"),
        &root.0.join("application/markdown"),
    );
    let mut resources = Packages::discover(&root.0.join("application"), None);
    let file = catalog_file(&root.0, &bytes);
    let catalog = Catalog::read(&file).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let requests = calls.clone();
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let gate = release.clone();
    resources.catalog = Some(Manager::with_download(
        catalog.clone(),
        catalog.descriptions().unwrap(),
        Some(root.0.join("cache")),
        Some(root.0.join("extensions")),
        Arc::new(move |_, _| {
            requests.fetch_add(1, Ordering::SeqCst);
            let mut done = gate.0.lock().unwrap();
            while !*done {
                done = gate.1.wait(done).unwrap();
            }
            Ok(fixture_bytes.clone())
        }),
    ));
    let resources = Arc::new(resources);
    let buffer = TextBuffer::new("# Title\n\n```json\n{\"answer\": 42}\n```\n");
    let lang = Registry::builtin().resolve_path(Path::new("file.md"));
    let mut trees = TreeIndex::new(resources.clone());
    let render = |trees: &mut TreeIndex, range: std::ops::Range<usize>| {
        trees
            .prepare(
                FileId::new(1),
                Source::new(&buffer),
                lang,
                &[Window {
                    line: range.clone(),
                    range,
                }],
                &std::sync::atomic::AtomicBool::new(false),
            )
            .unwrap()
    };
    render(&mut trees, 0..7);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    render(&mut trees, 0..buffer.len());
    wait(|| calls.load(Ordering::SeqCst) == 1);
    assert!(
        !resources
            .activity()
            .wasm_modules
            .contains("tree-sitter-json.wasm")
    );
    *release.0.lock().unwrap() = true;
    release.1.notify_all();
    ready(&resources);
    let frame = render(&mut trees, 0..buffer.len());
    assert!(
        resources
            .activity()
            .wasm_modules
            .contains("tree-sitter-json.wasm")
    );
    let number = buffer.text_snapshot().find("42").unwrap();
    assert!(
        frame[0]
            .spans
            .iter()
            .any(|s| s.range.contains(&number) && s.class.is_some())
    );
}

#[test]
fn a_matching_archive_digest_does_not_bypass_module_and_query_validation() {
    let Some(bytes) = archive() else {
        return;
    };
    for native in [false, true] {
        let root = Temporary::new(&std::env::temp_dir()).unwrap();
        let mut changed = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::default(),
        ));
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes.as_slice()));
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let mut path = entry.path().unwrap().to_string_lossy().into_owned();
            let mut payload = Vec::new();
            entry.read_to_end(&mut payload).unwrap();
            if native && path.ends_with(".wasm") {
                let ext = if cfg!(windows) {
                    "dll"
                } else if cfg!(target_os = "macos") {
                    "dylib"
                } else {
                    "so"
                };
                path = format!("json/json.{ext}");
            } else if !native && path.ends_with("highlights.scm") {
                payload = b"(nonexistent_node) @keyword".to_vec();
            }
            let mut header = tar::Header::new_gnu();
            header.set_size(payload.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            changed
                .append_data(&mut header, path, payload.as_slice())
                .unwrap();
        }
        let bytes = changed.into_inner().unwrap().finish().unwrap();
        let input = bytes.clone();
        let resources = source(&root.0, &input, Arc::new(move |_, _| Ok(bytes.clone())));
        assert!(resources.language("json").is_err());
        wait(|| resources.download_infos().iter().any(|i| i.error.is_some()));
        assert_eq!(resources.generation(), 0);
        // An archive that ships a native library instead of the WebAssembly
        // grammar is refused: nothing was installed as an extension.
        assert!(
            !root.0.join("extensions").exists()
                || fs::read_dir(root.0.join("extensions"))
                    .unwrap()
                    .next()
                    .is_none()
        );
        assert!(resources.activity().wasm_modules.is_empty());
    }
}

#[test]
fn archives_reject_links_and_unexpected_roots() {
    let root = Temporary::new(&std::env::temp_dir()).unwrap();
    if let Some(bytes) = archive() {
        assert!(unpack(&bytes, &root.0, "rust").is_err());
    }
    let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::default(),
    ));
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_link_name("/outside").unwrap();
    header.set_size(0);
    header.set_mode(0o777);
    header.set_cksum();
    archive
        .append_data(&mut header, "json/link", std::io::empty())
        .unwrap();
    assert!(
        unpack(
            &archive.into_inner().unwrap().finish().unwrap(),
            &root.0,
            "json"
        )
        .is_err()
    );
}

#[test]
fn installation_wakes_the_worker_and_repaints_an_unchanged_document() {
    use crate::{
        domain::{
            document::DocumentStamp,
            identity::{FileId, PaneId},
            language::Registry,
        },
        features::syntax::{Source, Window},
        infrastructure::syntax_worker::Worker,
    };
    use loom_editor_core::text::TextBuffer;
    let root = Temporary::new(&std::env::temp_dir()).unwrap();
    let Some(bytes) = archive() else {
        return;
    };
    let fixture_bytes = bytes.clone();
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let gate = release.clone();
    let source = source(
        &root.0,
        &fixture_bytes,
        Arc::new(move |_, _| {
            let mut done = gate.0.lock().unwrap();
            while !*done {
                done = gate.1.wait(done).unwrap();
            }
            Ok(bytes.clone())
        }),
    );
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = Worker::with_resources(source.clone(), move |_| {
        let _ = tx.send(());
    });
    let buffer = TextBuffer::new("{\"hello\": true}");
    let stamp = DocumentStamp::new(FileId::new(1), 1, 0, &buffer);
    let lang = Registry::builtin().resolve_path(Path::new("file.json"));
    let windows = vec![Window {
        line: 0..buffer.len(),
        range: 0..buffer.len(),
    }];
    worker.query(
        PaneId::new(1),
        stamp,
        Source::new(&buffer),
        lang,
        windows.clone(),
    );
    rx.recv_timeout(Duration::from_secs(20)).unwrap();
    assert_eq!(worker.stats().tree.queries_compiled, 1);
    *release.0.lock().unwrap() = true;
    release.1.notify_all();
    ready(&source);
    // No document edit or new query is needed to schedule the completed resource.
    wait(|| worker.stats().tree.queries_compiled >= 2);
    let frame = worker.query(PaneId::new(1), stamp, Source::new(&buffer), lang, windows);
    assert!(
        frame
            .iter()
            .flat_map(|w| w.spans.iter())
            .any(|s| s.class.is_some())
    );
    worker.shutdown();
}

#[test]
fn only_two_downloads_run_and_other_packages_queue_without_duplicate_requests() {
    let root = Temporary::new(&std::env::temp_dir()).unwrap();
    let catalog = Catalog::read(&catalog_file(&root.0, b"unused fixture")).unwrap();
    let base = catalog.packages["json"].clone();
    let mut catalog = Catalog {
        packages: BTreeMap::new(),
        ..catalog
    };
    for id in ["one", "two", "three"] {
        catalog.packages.insert(
            id.to_owned(),
            Package {
                name: id.to_owned(),
                asset: format!("{id}.tar.gz"),
                grammars: BTreeMap::from([(
                    id.to_owned(),
                    CatalogGrammar {
                        file_types: vec![id.to_owned()],
                    },
                )]),
                ..base.clone()
            },
        );
    }
    let active = Arc::new(AtomicUsize::new(0));
    let running = active.clone();
    let peak = Arc::new(AtomicUsize::new(0));
    let maximum = peak.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let requests = calls.clone();
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let gate = release.clone();
    let descriptions = catalog.descriptions().unwrap();
    let manager = Manager::with_download(
        catalog,
        descriptions,
        Some(root.0.join("cache")),
        Some(root.0.join("extensions")),
        Arc::new(move |_, _| {
            requests.fetch_add(1, Ordering::SeqCst);
            maximum.fetch_max(running.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
            let mut done = gate.0.lock().unwrap();
            while !*done {
                done = gate.1.wait(done).unwrap();
            }
            running.fetch_sub(1, Ordering::SeqCst);
            Err("offline".into())
        }),
    );
    for id in ["one", "two", "three", "one", "two", "three"] {
        assert!(manager.request(id).is_err());
    }
    wait(|| active.load(Ordering::SeqCst) == 2);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(manager.infos().iter().any(|i| i.status == "queued"));
    *release.0.lock().unwrap() = true;
    release.1.notify_all();
    wait(|| manager.infos().iter().all(|i| i.error.is_some()));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(peak.load(Ordering::SeqCst), 2);
}

#[test]
fn catalog_rejects_download_path_injection_and_ambiguous_associations() {
    let root = Temporary::new(&std::env::temp_dir()).unwrap();
    let file = catalog_file(&root.0, b"fixture");
    let valid = fs::read_to_string(&file).unwrap();
    for text in [
        valid.replace("json.tar.gz", "../json.tar.gz"),
        valid.replace("language-packs-test", "language-packs-test/../other"),
        valid.replace("lius-new/loom", "../loom"),
        valid.replace("\"jsonc\"", "\"JSON\""),
    ] {
        fs::write(&file, text).unwrap();
        assert!(Catalog::read(&file).is_err());
    }
}
