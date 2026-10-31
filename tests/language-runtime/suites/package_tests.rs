use super::*;
use crate::{
    domain::{identity::FileId, language},
    features::{
        highlighting::HighlightId,
        syntax::{Source, Window, WindowTokens},
    },
};
use loom_editor_core::text::TextBuffer;
use serde_json::json;
use std::{
    ops::Range,
    process::Command,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

fn bundled() -> PathBuf {
    std::env::var_os("LOOM_TEST_LANGUAGE_PACKS")
        .expect("prebuilt language artifacts required")
        .into()
}
fn disk() -> Packages {
    Packages::discover(&bundled(), None)
}
fn legacy() -> PathBuf {
    std::env::var_os("LOOM_TEST_LEGACY_PACKAGES")
        .map(PathBuf::from)
        .unwrap_or_else(bundled)
}
fn paint(
    index: &mut TreeIndex,
    buffer: &TextBuffer,
    lang: &str,
    range: Range<usize>,
) -> Vec<WindowTokens> {
    index
        .prepare(
            FileId::new(1),
            Source::new(buffer),
            language::builtin(lang),
            &[Window {
                line: range.clone(),
                range,
            }],
            &AtomicBool::new(false),
        )
        .unwrap()
}

static RUN: AtomicUsize = AtomicUsize::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "loom-pack-test-{}-{}",
            std::process::id(),
            RUN.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn package(&self, id: &str) {
        let src = legacy().join(id);
        let dst = self.0.join(id);
        fs::create_dir_all(&dst).unwrap();
        fs::copy(src.join("package.json"), dst.join("package.json")).unwrap();
        let manifest: Manifest =
            serde_json::from_str(&fs::read_to_string(dst.join("package.json")).unwrap()).unwrap();
        for grammar in manifest.grammars.keys() {
            let target = dst.join("grammars").join(grammar);
            fs::create_dir_all(&target).unwrap();
            for file in ["highlights.scm", "SOURCE.md", "injections.scm"] {
                let from = src.join("grammars").join(grammar).join(file);
                if from.is_file() {
                    fs::copy(from, target.join(file)).unwrap();
                }
            }
        }
    }
    fn manifest(&self, id: &str) -> Value {
        serde_json::from_str(&fs::read_to_string(self.0.join(id).join("package.json")).unwrap())
            .unwrap()
    }
    fn write(&self, id: &str, value: &Value) {
        let dir = self.0.join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("package.json"),
            serde_json::to_vec_pretty(value).unwrap(),
        )
        .unwrap();
    }
    fn query(&self, id: &str, grammar: &str, query: &str) {
        let dir = self.0.join(id).join("grammars").join(grammar);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("highlights.scm"), query).unwrap();
    }
    fn native(&self, id: &str, grammar: &str) {
        let source = Packages::discover(&legacy(), None);
        let (package, _) = source.grammar(grammar).unwrap();
        let to = self.0.join(id).join("modules");
        fs::create_dir_all(&to).unwrap();
        for module in package.manifest.grammars.values() {
            let from = Packages::module_path(package, &module.module);
            fs::copy(&from, to.join(from.file_name().unwrap())).unwrap();
        }
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        assert!(self.0.starts_with(std::env::temp_dir()));
        assert_ne!(self.0, std::env::temp_dir());
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn discovery_and_file_recognition_load_no_payload_including_markdown_and_json() {
    let source = disk();
    assert!(
        source.activity().errors.is_empty(),
        "{:?}",
        source.activity()
    );
    let registry = source.registry().unwrap();
    for (file, name) in [("a.rs", "rust"), ("a.md", "markdown"), ("a.json", "json")] {
        let resolved = registry.resolve_path(Path::new(file));
        assert_eq!(&*registry.definition_of(resolved).name, name);
    }
    assert_eq!(source.activity(), Activity::default());
}

#[test]
fn unmodified_standard_module_loads_with_default_symbol_and_arbitrary_filename() {
    let temp = Temp::new();
    temp.package("json");
    let source = disk();
    let (package, module) = source.grammar("json").unwrap();
    let native = Packages::module_path(package, &module.module);
    let filename = format!(
        "third-party-json.{}",
        native.extension().unwrap().to_str().unwrap()
    );
    let modules = temp.0.join("json/modules");
    fs::create_dir_all(&modules).unwrap();
    fs::copy(native, modules.join(&filename)).unwrap();
    let mut manifest = temp.manifest("json");
    manifest["grammars"]["json"] = json!({"module":filename});
    temp.write("json", &manifest);
    let installed = Arc::new(Packages::discover(&temp.0, None));
    assert_eq!(installed.activity(), Activity::default());
    let mut index = TreeIndex::new(installed.clone());
    let text = TextBuffer::new("{\"key\": true}");
    assert!(
        paint(&mut index, &text, "json", 0..text.len())[0]
            .spans
            .iter()
            .any(|s| s.class.is_some())
    );
    assert!(installed.activity().errors.is_empty());
    // This is the standard upstream C module, without a Loom export wrapper.
    let library = unsafe { Library::new(modules.join(filename)) }.unwrap();
    assert!(
        unsafe { library.get::<unsafe extern "C" fn() -> *const ()>(b"tree_sitter_json") }.is_ok()
    );
    assert!(
        unsafe { library.get::<unsafe extern "C" fn() -> *const ()>(b"loom_tree_sitter_json") }
            .is_err()
    );
}

#[test]
fn markdown_json_and_visible_injections_are_all_demand_loaded_and_cached() {
    let source = Arc::new(disk());
    let mut index = TreeIndex::new(source.clone());
    let json = TextBuffer::new("{\"hello\": true}");
    assert!(
        paint(&mut index, &json, "json", 0..json.len())[0]
            .spans
            .iter()
            .any(|s| s.class.is_some())
    );
    assert_eq!(source.activity().modules, BTreeSet::from(["json".into()]));
    let md = TextBuffer::new("# Heading\n\n~~~rust\nfn later() {}\n~~~\n\n~~~css\nbody {}\n~~~\n");
    paint(&mut index, &md, "markdown", 0..10);
    assert_eq!(
        source.activity().modules,
        BTreeSet::from(["json".into(), "markdown".into(), "markdown_inline".into()])
    );
    assert!(
        source
            .activity()
            .queries
            .iter()
            .all(|(g, _)| ["json", "markdown", "markdown_inline"].contains(&g.as_str()))
    );
    let start = md.text().find("fn later").unwrap();
    let range = start..start + "fn later() {}".len();
    assert!(
        paint(&mut index, &md, "markdown", range.clone())[0]
            .spans
            .iter()
            .any(|s| s.class == Some(HighlightId::KEYWORD))
    );
    let loaded = source.activity();
    assert_eq!(
        loaded.modules,
        BTreeSet::from([
            "json".into(),
            "markdown".into(),
            "markdown_inline".into(),
            "rust".into()
        ])
    );
    assert!(!loaded.queries.iter().any(|(g, _)| g == "css"));
    assert!(loaded.errors.is_empty());
    paint(&mut index, &md, "markdown", range);
    assert_eq!(source.activity(), loaded);
}

#[test]
fn no_installed_packages_means_plain_text_without_payload_requests() {
    let temp = Temp::new();
    let source = Arc::new(Packages::discover(&temp.0, None));
    let registry = source.registry().unwrap();
    for file in ["a.md", "a.json", "a.rs"] {
        let resolved = registry.resolve_path(Path::new(file));
        assert_eq!(&*registry.definition_of(resolved).name, "plaintext");
        let text = TextBuffer::new("any text");
        TreeIndex::new(source.clone())
            .prepare(
                FileId::new(1),
                Source::new(&text),
                resolved,
                &[Window {
                    line: 0..text.len(),
                    range: 0..text.len(),
                }],
                &AtomicBool::new(false),
            )
            .unwrap();
    }
    assert_eq!(source.activity(), Activity::default());
}

#[test]
fn missing_markdown_json_modules_fail_once_and_other_languages_work() {
    let temp = Temp::new();
    for id in ["markdown", "json", "rust"] {
        temp.package(id);
    }
    let source = Arc::new(Packages::discover(&temp.0, None));
    for grammar in ["markdown", "json"] {
        assert!(source.language(grammar).is_err());
    }
    let failed = source.activity();
    for grammar in ["markdown", "json"] {
        assert!(source.language(grammar).is_err());
    }
    assert_eq!(source.activity(), failed);
    assert_eq!(failed.language_loads, 2);
    // Resource failures are isolated from a different package.
    let working = disk();
    assert!(working.language("rust").is_ok());
    let mut index = TreeIndex::new(source.clone());
    for (lang, text) in [("markdown", "# Heading"), ("json", "{\"a\":true}")] {
        let text = TextBuffer::new(text);
        assert!(
            paint(&mut index, &text, lang, 0..text.len())[0]
                .spans
                .iter()
                .all(|s| s.class.is_none())
        );
    }
}

#[test]
fn bad_exports_queries_and_missing_queries_are_isolated() {
    let temp = Temp::new();
    for id in ["rust", "json"] {
        temp.package(id);
    }
    temp.native("rust", "rust");
    temp.native("json", "json");
    let mut manifest = temp.manifest("rust");
    manifest["grammars"]["rust"]["symbol"] = json!("missing_entry");
    temp.write("rust", &manifest);
    let source = Packages::discover(&temp.0, None);
    assert!(
        source
            .language("rust")
            .unwrap_err()
            .contains("missing_entry")
    );
    assert!(source.language("json").is_ok());
    manifest["grammars"]["rust"]["symbol"] = json!("tree_sitter_rust");
    temp.write("rust", &manifest);
    temp.query("rust", "rust", "(this_node_does_not_exist) @keyword");
    let source = Arc::new(Packages::discover(&temp.0, None));
    let mut index = TreeIndex::new(source.clone());
    let buffer = TextBuffer::new("fn a() {}");
    assert!(
        paint(&mut index, &buffer, "rust", 0..buffer.len())[0]
            .spans
            .iter()
            .all(|s| s.class.is_none())
    );
    assert_eq!(source.activity().errors.len(), 1);
    fs::remove_file(temp.0.join("json/grammars/json/highlights.scm")).unwrap();
    assert!(source.text("json", "highlights.scm").is_err());
    assert!(disk().language("markdown").is_ok());
}

#[test]
fn malformed_packages_are_skipped_without_disabling_valid_packages() {
    let temp = Temp::new();
    temp.package("json");
    temp.native("json", "json");
    for (id, value) in [
        (
            "schema",
            json!({"schema":2,"id":"schema","languages":[],"grammars":{}}),
        ),
        (
            "badpath",
            json!({"schema":1,"id":"badpath","languages":[],"grammars":{"../rust":{"module":"x","symbol":"x"}}}),
        ),
        (
            "unknown",
            json!({"schema":1,"id":"unknown","languages":[],"grammars":{},"unexpected":true}),
        ),
        (
            "mismatch",
            json!({"schema":1,"id":"different","languages":[],"grammars":{}}),
        ),
        (
            "reference",
            json!({"schema":1,"id":"reference","languages":[{"name":"reference","display_name":"Ref","badge":"R","variants":[{"id":"reference","grammar":"missing"}]}],"grammars":{}}),
        ),
    ] {
        temp.write(id, &value);
    }
    let source = Packages::discover(&temp.0, None);
    assert_eq!(source.activity().errors.len(), 5);
    assert_eq!(source.activity().language_loads, 0);
    assert_eq!(source.activity().query_reads, 0);
    assert_eq!(source.grammar_names(), ["json"]);
    assert!(source.language("json").is_ok());
}

#[test]
fn user_packages_override_same_id_atomically_and_disabled_packages_are_not_loaded() {
    let app = Temp::new();
    app.package("json");
    app.native("json", "json");
    let user = Temp::new();
    user.package("json");
    let mut manifest = user.manifest("json");
    manifest["languages"][0]["display_name"] = json!("User JSON");
    user.write("json", &manifest);
    let source = Packages::discover(&app.0, Some(&user.0));
    let registry = source.registry().unwrap();
    let id = registry.language_by_name("json").unwrap();
    assert_eq!(&*registry.definition(id).display_name, "User JSON");
    // User packages must never fall back to a bundled/development DLL.
    assert!(source.language("json").is_err());
    manifest["enabled"] = json!(false);
    user.write("json", &manifest);
    let disabled = Packages::discover(&app.0, Some(&user.0));
    assert!(
        disabled
            .registry()
            .unwrap()
            .language_by_name("json")
            .is_none()
    );
    assert_eq!(disabled.activity(), Activity::default());
    manifest["schema"] = json!(99);
    user.write("json", &manifest);
    let fallback = Packages::discover(&app.0, Some(&user.0));
    assert!(
        fallback
            .registry()
            .unwrap()
            .language_by_name("json")
            .is_some()
    );
    assert!(fallback.language("json").is_ok());
}

#[test]
fn cross_package_conflicts_are_rejected_without_corrupting_the_registry() {
    let app = Temp::new();
    app.package("json");
    app.native("json", "json");
    let user = Temp::new();
    let mut manifest = app.manifest("json");
    manifest["id"] = json!("duplicate");
    user.write("duplicate", &manifest);
    let source = Packages::discover(&app.0, Some(&user.0));
    assert_eq!(source.activity().errors.len(), 1);
    assert_eq!(source.grammar_names(), ["json"]);
    assert!(source.language("json").is_ok());
}

#[test]
fn newly_installed_packages_require_a_new_discovery_and_loaded_metadata_is_stable() {
    let temp = Temp::new();
    let first = Packages::discover(&temp.0, None);
    temp.package("json");
    assert!(first.registry().unwrap().language_by_name("json").is_none());
    let second = Packages::discover(&temp.0, None);
    assert!(
        second
            .registry()
            .unwrap()
            .language_by_name("json")
            .is_some()
    );
    let mut manifest = temp.manifest("json");
    manifest["enabled"] = json!(false);
    temp.write("json", &manifest);
    assert!(
        second
            .registry()
            .unwrap()
            .language_by_name("json")
            .is_some()
    );
    assert!(
        Packages::discover(&temp.0, None)
            .registry()
            .unwrap()
            .language_by_name("json")
            .is_none()
    );
    assert_eq!(second.activity(), Activity::default());
}

#[cfg(windows)]
fn assert_native_modules(expected: &[&str]) {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(name: *const u16) -> *mut std::ffi::c_void;
    }
    for module in [
        "rust",
        "javascript",
        "typescript",
        "tsx",
        "c_sharp",
        "css",
        "html",
        "json",
        "markdown",
        "markdown_inline",
    ] {
        let wide: Vec<u16> = format!("{module}.dll")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        // SAFETY: terminated UTF-16 name; querying never loads a library.
        assert_eq!(
            !unsafe { GetModuleHandleW(wide.as_ptr()) }.is_null(),
            expected.contains(&module),
            "{module}"
        );
    }
}
#[cfg(not(windows))]
fn assert_native_modules(_expected: &[&str]) {}

/// Real independent packages in a fresh process: no global index edits, no Cargo
/// fallback, no source working directory and no startup grammar imports.
#[test]
fn downloaded_user_package_is_discovered_and_parsed_without_recompiling() {
    const CHILD: &str = "LOOM_USER_PACK_TEST";
    if let Some(root) = std::env::var_os(CHILD) {
        let root = PathBuf::from(root);
        let source = Arc::new(Packages::discover(
            &root.join("bundled"),
            user_directory().as_deref(),
        ));
        assert_eq!(source.activity(), Activity::default());
        assert_native_modules(&[]);
        Registry::install(source.registry().unwrap()).unwrap();
        let resolved = language::from_path(Path::new("new.fixture"));
        assert_eq!(resolved.name(), "fixturelang");
        assert_eq!(source.activity(), Activity::default());
        let mut index = TreeIndex::new(source.clone());
        let md = TextBuffer::new("# Heading\n\n*emphasis*\n");
        assert!(
            paint(&mut index, &md, "markdown", 0..md.len())[0]
                .spans
                .iter()
                .any(|s| s.class.is_some())
        );
        assert_native_modules(&["markdown", "markdown_inline"]);
        let json = TextBuffer::new("{\"a\":true}");
        assert!(
            paint(&mut index, &json, "json", 0..json.len())[0]
                .spans
                .iter()
                .any(|s| s.class.is_some())
        );
        assert_native_modules(&["markdown", "markdown_inline", "json"]);
        let text = TextBuffer::new("fn hello() {}");
        let result = index
            .prepare(
                FileId::new(2),
                Source::new(&text),
                resolved,
                &[Window {
                    line: 0..text.len(),
                    range: 0..text.len(),
                }],
                &AtomicBool::new(false),
            )
            .unwrap();
        assert!(
            result[0]
                .spans
                .iter()
                .any(|s| s.class == Some(HighlightId::KEYWORD))
        );
        assert_native_modules(&["markdown", "markdown_inline", "json", "rust"]);
        assert_eq!(source.activity().modules.len(), 4);
        return;
    }
    let temp = Temp::new();
    let app = Temp(temp.0.join("bundled"));
    let user_base = temp.0.join("user-data");
    let user = Temp(
        user_directory_from(std::env::consts::OS, |_| {
            Some(user_base.clone().into_os_string())
        })
        .unwrap(),
    );
    for id in ["markdown", "json"] {
        app.package(id);
        app.native(id, id);
    }
    let fixture = json!({"schema":1,"id":"fixture","languages":[{
        "name":"fixturelang","display_name":"Fixture","badge":"FIX",
        "variants":[{"id":"fixturelang","grammar":"fixturegrammar"}],
        "files":[{"extensions":["fixture"],"variant":"fixturelang"}]
    }],"grammars":{"fixturegrammar":{"module":"rust","symbol":"tree_sitter_rust"}}});
    user.write("fixture", &fixture);
    user.query("fixture", "fixturegrammar", "(\"fn\") @keyword");
    user.native("fixture", "rust");
    let output=Command::new(std::env::current_exe().unwrap())
        .args(["--exact","infrastructure::language_packs::tests::downloaded_user_package_is_discovered_and_parsed_without_recompiling","--nocapture"])
        .env(CHILD,&temp.0)
        .env("APPDATA", &user_base).env("LOCALAPPDATA", temp.0.join("unused-local")).env("HOME", &user_base).env("XDG_DATA_HOME", &user_base)
        .current_dir(&temp.0).output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn platform_user_data_locations_are_absolute_and_do_not_follow_the_working_directory() {
    let home = std::env::temp_dir().join("loom-home");
    let local = std::env::temp_dir().join("loom-local");
    let roaming = std::env::temp_dir().join("loom-roaming");
    let xdg = std::env::temp_dir().join("loom-data");
    let get = |key: &str| match key {
        "HOME" => Some(home.clone().into_os_string()),
        "LOCALAPPDATA" => Some(local.clone().into_os_string()),
        "APPDATA" => Some(roaming.clone().into_os_string()),
        "XDG_DATA_HOME" => Some(xdg.clone().into_os_string()),
        _ => None,
    };
    assert_eq!(
        user_directory_from("windows", get),
        Some(roaming.join("Loom/language-packs"))
    );
    assert_eq!(
        user_directory_from("macos", get),
        Some(home.join("Library/Application Support/Loom/language-packs"))
    );
    assert_eq!(
        user_directory_from("linux", get),
        Some(xdg.join("loom/language-packs"))
    );
    assert_eq!(
        user_directory_from("linux", |key| if key == "XDG_DATA_HOME" {
            Some("relative".into())
        } else {
            get(key)
        }),
        Some(home.join(".local/share/loom/language-packs"))
    );
    assert_eq!(
        user_directory_from("windows", |_| Some("relative".into())),
        None
    );
    assert_eq!(user_directory_from("macos", |_| None), None);
}

#[test]
fn independently_named_packages_cannot_claim_an_existing_file_rule() {
    let app = Temp::new();
    app.package("json");
    app.native("json", "json");
    let user = Temp::new();
    user.write(
        "collision",
        &json!({"schema":1,"id":"collision","languages":[{
        "name":"collision","display_name":"Collision","badge":"C",
        "variants":[{"id":"collision"}],"files":[{"extensions":["json"],"variant":"collision"}]
    }],"grammars":{}}),
    );
    let source = Packages::discover(&app.0, Some(&user.0));
    assert_eq!(source.activity().errors.len(), 1);
    assert!(
        source
            .registry()
            .unwrap()
            .language_by_name("collision")
            .is_none()
    );
    assert!(
        source
            .registry()
            .unwrap()
            .language_by_name("json")
            .is_some()
    );
}

#[test]
fn bundled_packages_are_self_contained_and_enabled() {
    let source = disk();
    assert_eq!(source.packages.len(), 8);
    for package in source.packages.values() {
        assert!(package.manifest.enabled);
        if package.standard.is_none() {
            package.manifest.validate(&package.root).unwrap();
        }
    }
}

#[test]
fn discovery_enforces_manifest_budget_and_ignores_unfinished_downloads() {
    let temp = Temp::new();
    temp.package("json");
    fs::create_dir_all(temp.0.join("unfinished")).unwrap();
    fs::create_dir_all(temp.0.join("oversize")).unwrap();
    fs::write(
        temp.0.join("oversize/package.json"),
        vec![b' '; MAX_MANIFEST as usize + 1],
    )
    .unwrap();
    let source = Packages::discover(&temp.0, None);
    assert_eq!(source.activity().errors.len(), 1);
    assert!(
        source
            .activity()
            .errors
            .iter()
            .any(|e| e.contains("exceeds"))
    );
    assert_eq!(source.activity().query_reads, 0);
    assert_eq!(source.activity().language_loads, 0);
    assert_eq!(source.grammar_names(), ["json"]);
}

#[test]
fn standard_templates_highlight_every_bundled_language_without_legacy_files() {
    fn copy_resources(source: &Path, target: &Path) {
        fs::create_dir_all(target).unwrap();
        for entry in fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                copy_resources(&path, &target.join(entry.file_name()));
            } else if path.file_name().unwrap() == "tree-sitter.json"
                || path.extension().is_some_and(|e| e == "scm")
            {
                fs::copy(path, target.join(entry.file_name())).unwrap();
            }
        }
    }
    let temp = Temp::new();
    let native = disk();
    for id in ["json", "markdown"] {
        let target = temp.0.join(format!("tree-sitter-{id}"));
        copy_resources(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../assets/extensions")
                .join(format!("loom.lang.{id}")),
            &target,
        );
        let document: Value =
            serde_json::from_str(&fs::read_to_string(target.join("tree-sitter.json")).unwrap())
                .unwrap();
        for grammar in document["grammars"].as_array().unwrap() {
            let name = grammar["name"].as_str().unwrap();
            let (package, module) = native.grammar(name).unwrap();
            let from = Packages::module_path(package, &module.module);
            fs::copy(
                &from,
                target.join(format!(
                    "{name}.{}",
                    from.extension().unwrap().to_str().unwrap()
                )),
            )
            .unwrap();
        }
        assert!(!target.join("package.json").exists());
        assert!(!target.join("SOURCE.md").exists());
    }
    let source = Arc::new(Packages::discover(&temp.0, None));
    assert_eq!(source.activity(), Activity::default());
    let registry = source.registry().unwrap();
    assert_eq!(registry.languages().len(), 3);
    let mut index = TreeIndex::new(source.clone());
    for (file, lang, text) in [
        ("a.json", "json", "{\"a\":true}"),
        ("a.md", "markdown", "# Title\n\n*emphasis*\n"),
    ] {
        let resolved = registry.resolve_path(Path::new(file));
        assert_eq!(registry.definition_of(resolved).name.as_ref(), lang);
        let buffer = TextBuffer::new(text);
        let frame = index
            .prepare(
                FileId::new(1),
                Source::new(&buffer),
                resolved,
                &[Window {
                    line: 0..buffer.len(),
                    range: 0..buffer.len(),
                }],
                &AtomicBool::new(false),
            )
            .unwrap();
        assert!(frame[0].spans.iter().any(|s| s.class.is_some()), "{file}");
    }
    assert!(
        source.activity().errors.is_empty(),
        "{:?}",
        source.activity()
    );
}
