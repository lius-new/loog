//! Host-only loader checks use synthetic metadata and never require a grammar.
use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};

static RUN: AtomicUsize = AtomicUsize::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "loom-host-packs-{}-{}",
            std::process::id(),
            RUN.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn write(&self, id: &str, value: &Value) {
        let dir = self.0.join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("package.json"), value.to_string()).unwrap();
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        assert_eq!(self.0.parent(), Some(std::env::temp_dir().as_path()));
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn manifest() -> Value {
    json!({"schema":1,"id":"sample","languages":[{
        "name":"sample","display_name":"Sample","badge":"S",
        "variants":[{"id":"sample","grammar":"sample"}],
        "files":[{"extensions":["sample"],"variant":"sample"}]
    }],"grammars":{"sample":{"module":"missing_module","symbol":"tree_sitter_sample"}}})
}

#[test]
fn standard_entry_points_are_inferred_and_explicit_aliases_are_preserved() {
    let default: Module = serde_json::from_value(json!({"module":"python.dll"})).unwrap();
    assert_eq!(default.entry_point("python"), "tree_sitter_python");
    let alias: Module =
        serde_json::from_value(json!({"module":"python.dll","symbol":"tree_sitter_python"}))
            .unwrap();
    assert_eq!(alias.entry_point("my_python"), "tree_sitter_python");
    let temp = Temp::new();
    let mut value = manifest();
    value["grammars"]["sample"] = json!({"module":"sample.dll"});
    temp.write("sample", &value);
    let source = Packages::discover(&temp.0, None);
    assert!(
        source
            .registry()
            .unwrap()
            .language_by_name("sample")
            .is_some()
    );
    assert_eq!(source.activity(), Activity::default());
}

#[test]
fn standard_native_filenames_are_used_without_renaming_or_path_escape() {
    for (platform, extension) in [("windows", "dll"), ("linux", "so"), ("macos", "dylib")] {
        assert_eq!(
            module_file("python", platform).unwrap(),
            format!("python.{extension}")
        );
        for file in [
            "python.dll",
            "python.so",
            "python.dylib",
            "libtree-sitter-python.so",
            "tree-sitter-python.wasm",
        ] {
            assert_eq!(module_file(file, platform).unwrap(), file);
        }
    }
    for invalid in [
        "../python.dll",
        "a/python.so",
        "a\\python.dll",
        "C:python.dll",
        "python..so",
        "python.so.exe",
        "python.json",
        "../python.wasm",
        "python.wasm.dll",
        "",
    ] {
        assert!(module_file(invalid, "windows").is_err(), "{invalid}");
    }
}

#[test]
fn wasm_downloads_are_preferred_for_stems_and_explicit_filenames_select_one_format() {
    let temp = Temp::new();
    let mut value = manifest();
    value["grammars"]["sample"] = json!({"module":"sample"});
    temp.write("sample", &value);
    let modules = temp.0.join("sample/modules");
    fs::create_dir_all(&modules).unwrap();
    for file in module_candidates("sample", std::env::consts::OS) {
        fs::write(modules.join(file), "unloaded payload").unwrap();
    }
    let source = Packages::discover(&temp.0, None);
    let (package, _) = source.grammar("sample").unwrap();
    assert_eq!(
        Packages::module_path(package, "sample"),
        modules.join("tree-sitter-sample.wasm")
    );
    assert_eq!(
        Packages::module_path(package, "sample.wasm"),
        modules.join("sample.wasm")
    );
    assert_eq!(
        Packages::module_path(package, "sample.dll"),
        modules.join("sample.dll")
    );
    assert_eq!(source.activity(), Activity::default());
}

#[test]
fn invalid_and_oversized_wasm_are_isolated_and_failures_are_cached() {
    let temp = Temp::new();
    let mut value = manifest();
    value["grammars"]["sample"] = json!({"module":"tree-sitter-sample.wasm"});
    temp.write("sample", &value);
    let modules = temp.0.join("sample/modules");
    fs::create_dir_all(&modules).unwrap();
    let path = modules.join("tree-sitter-sample.wasm");
    fs::write(&path, "not WASM").unwrap();
    let source = Packages::discover(&temp.0, None);
    assert_eq!(source.activity(), Activity::default());
    assert!(
        source
            .language("sample")
            .unwrap_err()
            .contains("WebAssembly module header")
    );
    let failed = source.activity();
    assert_eq!(failed.wasm_loads, 1);
    assert!(source.language("sample").is_err());
    assert_eq!(source.activity(), failed);
    fs::File::create(path)
        .unwrap()
        .set_len(MAX_WASM_BYTES + 1)
        .unwrap();
    let source = Packages::discover(&temp.0, None);
    assert!(source.language("sample").unwrap_err().contains("exceeds"));
    let alias: Module =
        serde_json::from_value(json!({"module":"sample.wasm","symbol":"tree_sitter_rust"}))
            .unwrap();
    assert_eq!(alias.wasm_name("custom").unwrap(), "rust");
    let invalid: Module =
        serde_json::from_value(json!({"module":"sample.wasm","symbol":"other"})).unwrap();
    assert!(invalid.wasm_name("sample").is_err());
}

#[test]
fn no_packages_provide_plain_text_and_no_resource_activity() {
    let temp = Temp::new();
    let source = Packages::discover(&temp.0.join("missing"), None);
    let registry = source.registry().unwrap();
    assert_eq!(registry.languages().len(), 1);
    for file in ["a.md", "a.json", "a.rs"] {
        assert_eq!(
            &*registry
                .definition_of(registry.resolve_path(Path::new(file)))
                .name,
            "plaintext"
        );
    }
    assert_eq!(source.activity(), Activity::default());
}

#[test]
fn metadata_recognition_is_lazy_and_missing_payload_failure_is_cached() {
    let temp = Temp::new();
    temp.write("sample", &manifest());
    let source = Packages::discover(&temp.0, None);
    let registry = source.registry().unwrap();
    assert_eq!(
        &*registry
            .definition_of(registry.resolve_path(Path::new("a.sample")))
            .name,
        "sample"
    );
    assert_eq!(source.activity(), Activity::default());
    assert!(source.language("sample").is_err());
    let failed = source.activity();
    assert_eq!(failed.language_loads, 1);
    assert!(source.language("sample").is_err());
    assert_eq!(source.activity(), failed);
    assert!(source.text("sample", "highlights.scm").is_err());
    let failed = source.activity();
    assert!(source.text("sample", "highlights.scm").is_err());
    assert_eq!(source.activity(), failed);
}

#[test]
fn overrides_disable_and_restart_are_metadata_only() {
    let app = Temp::new();
    let user = Temp::new();
    app.write("sample", &manifest());
    let first = Packages::discover(&app.0, Some(&user.0));
    let mut replacement = manifest();
    replacement["enabled"] = json!(false);
    user.write("sample", &replacement);
    assert!(
        first
            .registry()
            .unwrap()
            .language_by_name("sample")
            .is_some()
    );
    let next = Packages::discover(&app.0, Some(&user.0));
    assert!(
        next.registry()
            .unwrap()
            .language_by_name("sample")
            .is_none()
    );
    assert_eq!(next.activity(), Activity::default());
    replacement["schema"] = json!(99);
    user.write("sample", &replacement);
    let invalid = Packages::discover(&app.0, Some(&user.0));
    assert!(
        invalid
            .registry()
            .unwrap()
            .language_by_name("sample")
            .is_some()
    );
    assert_eq!(invalid.activity().errors.len(), 1);
}

#[test]
fn invalid_packages_do_not_corrupt_valid_metadata() {
    let temp = Temp::new();
    temp.write("sample", &manifest());
    for (id, value) in [
        (
            "path",
            json!({"schema":1,"id":"path","languages":[],"grammars":{"../escape":{"module":"x","symbol":"x"}}}),
        ),
        (
            "field",
            json!({"schema":1,"id":"field","languages":[],"grammars":{},"unknown":true}),
        ),
        (
            "zconflict",
            json!({"schema":1,"id":"zconflict","languages":[],"grammars":{"sample":{"module":"x","symbol":"x"}}}),
        ),
    ] {
        temp.write(id, &value);
    }
    // A conflicting incoming package is rejected atomically.
    let source = Packages::discover(&temp.0, None);
    assert!(
        source
            .registry()
            .unwrap()
            .language_by_name("sample")
            .is_some()
    );
    assert_eq!(source.activity().errors.len(), 3);
    assert_eq!(source.activity().language_loads, 0);
}

#[test]
fn platform_paths_are_absolute_and_portable_directories_are_not_cargo_profiles() {
    let home = std::env::temp_dir().join("loom-data");
    assert_eq!(
        user_directory_from("windows", |_| Some(home.clone().into_os_string())),
        Some(home.join("Loom/language-packs"))
    );
    assert_eq!(
        user_directory_from("windows", |_| Some("relative".into())),
        None
    );
    assert_eq!(
        locations(Path::new("/portable/deps/Loom.exe")),
        PathBuf::from("/portable/deps/language-packs")
    );
}

fn standard_manifest() -> Value {
    json!({"grammars":[{"name":"sample","scope":"source.sample","path":".","file-types":["sample","special.test"],"highlights":["queries/base.scm","queries/highlights.scm"]}],"metadata":{"version":"1.2.3"},"bindings":{"rust":true},"future-upstream-field":{}})
}
fn write_standard(temp: &Temp, directory: &str, value: &Value) -> PathBuf {
    let root = temp.0.join(directory);
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("tree-sitter.json"), value.to_string()).unwrap();
    root
}

#[test]
fn upstream_manifest_alone_registers_suffixes_without_loading_any_payload() {
    let temp = Temp::new();
    write_standard(&temp, "tree-sitter-sample-main", &standard_manifest());
    let source = Packages::discover(&temp.0, None);
    let registry = source.registry().unwrap();
    for path in ["hello.sample", "my.special.test", "special.test"] {
        assert_eq!(
            registry
                .definition_of(registry.resolve_path(Path::new(path)))
                .name
                .as_ref(),
            "sample"
        );
    }
    assert_eq!(
        registry
            .definition_of(registry.resolve_path(Path::new("hello.test")))
            .name
            .as_ref(),
        "plaintext"
    );
    assert_eq!(source.activity(), Activity::default());
}

/// An extension package folder with a manifest that lists `resources` as language resources.
fn extension_package(temp: &Temp, name: &str, resources: &[&str]) -> PathBuf {
    let package = temp.0.join("extensions").join(name);
    fs::create_dir_all(&package).unwrap();
    let list: Vec<Value> = resources
        .iter()
        .enumerate()
        .map(|(n, path)| json!({ "id": format!("acme.{name}.lang{n}"), "path": path }))
        .collect();
    fs::write(
        package.join("manifest.json"),
        json!({ "schema": 1, "id": format!("acme.{name}"), "version": "1.0.0",
                "contributes": { "languages": list } })
        .to_string(),
    )
    .unwrap();
    package
}

fn write_pack(package: &Path, directory: &str, grammar: &str, suffix: &str) {
    let root = package.join(directory);
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("tree-sitter.json"),
        json!({"grammars":[{"name":grammar,"file-types":[suffix]}]}).to_string(),
    )
    .unwrap();
}

#[test]
fn a_language_pack_inside_an_extension_package_is_found_at_startup() {
    let temp = Temp::new();
    let package = extension_package(&temp, "weave", &["grammars/weave/tree-sitter.json"]);
    write_pack(&package, "grammars/weave", "weave", "wv");
    // A package whose resource is not a pack manifest, or leaves the package, contributes nothing.
    extension_package(
        &temp,
        "other",
        &["notes/readme.json", "../outside/tree-sitter.json"],
    );
    fs::create_dir_all(temp.0.join("extensions/other/notes")).unwrap();
    fs::write(temp.0.join("extensions/other/notes/readme.json"), "{}").unwrap();
    write_pack(&temp.0, "outside", "outside", "ou");

    let found = extension_packs(&temp.0.join("extensions"));
    assert_eq!(
        found,
        [package.join("grammars/weave").canonicalize().unwrap()]
    );

    let bundled = temp.0.join("bundled");
    let source = Packages::discover_with(&bundled, &found, None);
    let registry = source.registry().unwrap();
    for (file, expected) in [("a.wv", "weave"), ("a.ou", "plaintext")] {
        assert_eq!(
            registry
                .definition_of(registry.resolve_path(Path::new(file)))
                .name
                .as_ref(),
            expected,
            "{file}"
        );
    }
    assert!(check_extension_pack(&package, "grammars/weave/tree-sitter.json").is_ok());
    assert!(check_extension_pack(&package, "grammars/weave/readme.json").is_err());
}

#[test]
fn an_extension_pack_cannot_replace_a_bundled_collection_but_a_user_copy_replaces_both() {
    let temp = Temp::new();
    let bundled = temp.0.join("bundled");
    write_pack(&bundled, "tree-sitter-weave", "weave", "wv");
    let package = extension_package(&temp, "weave", &["g/tree-sitter.json"]);
    write_pack(&package, "g", "weave", "weave2");
    let found = extension_packs(&temp.0.join("extensions"));

    let source = Packages::discover_with(&bundled, &found, None);
    let registry = source.registry().unwrap();
    let name = |file: &str| {
        registry
            .definition_of(registry.resolve_path(Path::new(file)))
            .name
            .to_string()
    };
    assert_eq!(name("a.wv"), "weave", "the bundled collection stays");
    assert_eq!(name("a.weave2"), "plaintext", "the duplicate was refused");
    assert!(
        source
            .activity()
            .errors
            .iter()
            .any(|e| e.contains("duplicate")),
        "{:?}",
        source.activity().errors
    );

    let user = temp.0.join("user");
    write_pack(&user, "mine", "weave", "mine");
    let registry = Packages::discover_with(&bundled, &found, Some(&user))
        .registry()
        .unwrap();
    assert_eq!(
        registry
            .definition_of(registry.resolve_path(Path::new("a.mine")))
            .name
            .as_ref(),
        "weave"
    );
}

#[test]
fn standard_suffixes_include_dotfiles_and_longest_suffix_wins() {
    let temp = Temp::new();
    write_standard(
        &temp,
        "standard",
        &json!({"grammars":[
        {"name":"generic","path":null,"file-types":["test"]},
            {"name":"specific","file-types":["special.test",".sample"]}
        ]}),
    );
    let registry = Packages::discover(&temp.0, None).registry().unwrap();
    for (file, expected) in [
        ("file.test", "generic"),
        ("file.special.test", "specific"),
        (".sample", "specific"),
        ("protest", "plaintext"),
        ("file.not-special.test", "generic"),
        ("file.sample", "plaintext"),
    ] {
        assert_eq!(
            registry
                .definition_of(registry.resolve_path(Path::new(file)))
                .name
                .as_ref(),
            expected
        );
    }
}

#[test]
fn standard_required_query_errors_are_cached_and_bad_user_manifests_preserve_bundled_metadata() {
    let app = Temp::new();
    let user = Temp::new();
    write_standard(&app, "upstream", &standard_manifest());
    write_standard(
        &user,
        "user-copy",
        &json!({"grammars":[{"name":"sample","highlights":"../outside"}]}),
    );
    let source = Packages::discover(&app.0, Some(&user.0));
    assert_eq!(source.grammar_names(), ["sample"]);
    assert_eq!(source.activity().errors.len(), 1);
    assert!(source.text("sample", "highlights.scm").is_err());
    let failed = source.activity();
    assert!(source.text("sample", "highlights.scm").is_err());
    assert_eq!(source.activity(), failed);
}

#[test]
fn standard_queries_preserve_declared_order_and_do_not_require_source_metadata() {
    let temp = Temp::new();
    let root = write_standard(&temp, "sample", &standard_manifest());
    fs::create_dir_all(root.join("queries")).unwrap();
    fs::write(root.join("queries/base.scm"), "; first").unwrap();
    fs::write(root.join("queries/highlights.scm"), "; second").unwrap();
    let source = Packages::discover(&temp.0, None);
    assert_eq!(source.activity(), Activity::default());
    assert_eq!(
        source
            .text("sample", "highlights.scm")
            .unwrap()
            .unwrap()
            .as_ref(),
        "; first\n; second\n"
    );
    assert!(source.text("sample", "injections.scm").unwrap().is_none());
    assert_eq!(source.activity().language_loads, 0);
    assert!(!root.join("package.json").exists());
    assert!(!root.join("SOURCE.md").exists());
    let loaded = source.activity();
    source.text("sample", "highlights.scm").unwrap();
    assert_eq!(source.activity(), loaded);
}

#[test]
fn standard_multi_grammar_paths_are_root_relative_and_modules_can_be_in_subdirectories() {
    let temp = Temp::new();
    let value = json!({"grammars":[
        {"name":"first","path":"grammar/first","file-types":["first"],"highlights":"shared/base.scm"},
        {"name":"second","path":"grammar/second","file-types":null,"highlights":["shared/base.scm"]}
    ]});
    let root = write_standard(&temp, "tree-sitter-multi", &value);
    fs::create_dir_all(root.join("grammar/second")).unwrap();
    fs::create_dir_all(root.join("shared")).unwrap();
    fs::write(root.join("shared/base.scm"), "; shared").unwrap();
    fs::write(
        root.join("grammar/second/tree-sitter-second.wasm"),
        "unopened",
    )
    .unwrap();
    let source = Packages::discover(&temp.0, None);
    assert_eq!(source.grammar_names(), ["first", "second"]);
    let (package, module) = source.grammar("second").unwrap();
    assert_eq!(
        Packages::module_path(package, &module.module),
        root.join("grammar/second/tree-sitter-second.wasm")
    );
    assert_eq!(
        source
            .text("second", "highlights.scm")
            .unwrap()
            .unwrap()
            .as_ref(),
        "; shared\n"
    );
    assert_eq!(source.activity().language_loads, 0);
}

#[test]
fn standard_user_override_is_independent_of_directory_name_and_never_borrows_queries() {
    let app = Temp::new();
    let user = Temp::new();
    let root = write_standard(&app, "tree-sitter-sample", &standard_manifest());
    fs::create_dir_all(root.join("queries")).unwrap();
    fs::write(root.join("queries/base.scm"), "; app").unwrap();
    fs::write(root.join("queries/highlights.scm"), "; app").unwrap();
    let first = Packages::discover(&app.0, Some(&user.0));
    write_standard(&user, "downloaded-version", &standard_manifest());
    let next = Packages::discover(&app.0, Some(&user.0));
    assert_eq!(next.activity(), Activity::default());
    assert!(next.text("sample", "highlights.scm").is_err());
    assert!(first.text("sample", "highlights.scm").is_ok());
}

#[test]
fn invalid_standard_paths_duplicates_and_conflicts_leave_valid_packages_available() {
    let temp = Temp::new();
    write_standard(&temp, "a-valid", &standard_manifest());
    for (directory, field, value) in [
        ("bad-query", "highlights", json!("../outside.scm")),
        ("bad-path", "path", json!("C:/outside")),
        ("backslash", "highlights", json!("queries\\outside.scm")),
    ] {
        let mut document = standard_manifest();
        document["grammars"][0][field] = value;
        write_standard(&temp, directory, &document);
    }
    let mut duplicate = standard_manifest();
    let repeated = duplicate["grammars"][0].clone();
    duplicate["grammars"].as_array_mut().unwrap().push(repeated);
    write_standard(&temp, "duplicate", &duplicate);
    let mut conflict = standard_manifest();
    conflict["grammars"][0]["name"] = json!("other");
    write_standard(&temp, "z-conflict", &conflict);
    let source = Packages::discover(&temp.0, None);
    assert_eq!(source.grammar_names(), ["sample"]);
    assert_eq!(source.activity().errors.len(), 5);
    assert_eq!(source.activity().query_reads, 0);
}

#[test]
fn standard_manifest_is_authoritative_and_malformed_input_does_not_fall_back_to_legacy() {
    let temp = Temp::new();
    temp.write("sample", &manifest());
    let root = write_standard(&temp, "sample", &json!({"grammars":[]}));
    let source = Packages::discover(&temp.0, None);
    assert!(source.grammar_names().is_empty());
    assert_eq!(source.activity().errors.len(), 1);
    fs::write(
        root.join("tree-sitter.json"),
        vec![b' '; MAX_MANIFEST as usize + 1],
    )
    .unwrap();
    assert!(
        Packages::discover(&temp.0, None)
            .activity()
            .errors
            .iter()
            .any(|e| e.contains("exceeds"))
    );
}

#[test]
fn standard_rust_keeps_application_editing_defaults_without_external_editor_metadata() {
    let temp = Temp::new();
    write_standard(
        &temp,
        "upstream-rust",
        &json!({"grammars":[{"name":"rust","file-types":["rs"]}]}),
    );
    let registry = Packages::discover(&temp.0, None).registry().unwrap();
    let rust = registry.resolve_path(Path::new("new.rs"));
    assert_eq!(
        registry.definition_of(rust).comments.line.as_deref(),
        Some("//")
    );
}
