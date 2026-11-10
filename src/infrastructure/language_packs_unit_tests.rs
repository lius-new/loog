//! Host-only loader checks use synthetic metadata and never require a grammar.
use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};

static RUN: AtomicUsize = AtomicUsize::new(0);

#[test]
fn language_identities_work_without_syntax_and_keep_independent_parser_bindings() {
    let temp = Temp::new();
    let identity = temp.0.join("identity.json");
    fs::write(&identity, json!({"name":"sample","display_name":"Sample","badge":"S","variants":[{"id":"sample"}],"files":[{"extensions":["identity"],"variant":"sample"}]}).to_string()).unwrap();
    let mut service_only = Packages::empty();
    service_only.add_identity_files(vec![identity.clone()]);
    let language = service_only
        .registry()
        .unwrap()
        .resolve_path(Path::new("a.identity"));
    assert!(language.grammar().is_none());
    assert_eq!(language.name(), "sample");
    assert!(service_only.grammar_names().is_empty());

    write_sample(&temp);
    let mut with_syntax = Packages::discover(&temp.0, None);
    with_syntax.add_identity_files(vec![identity]);
    let registry = with_syntax.registry().unwrap();
    for path in ["a.sample", "a.identity"] {
        let language = registry.resolve_path(Path::new(path));
        assert_eq!(language.definition().name.as_ref(), "sample");
        assert!(language.grammar().is_some());
    }
    assert!(
        with_syntax.activity().language_loads == 0,
        "metadata discovery does not load a parser"
    );
}

#[test]
fn identity_metadata_rejects_parser_bindings_and_preserves_a_previous_valid_identity() {
    let temp = Temp::new();
    let identity = temp.0.join("identity.json");
    let value = json!({"name":"sample","display_name":"Sample","badge":"S","variants":[{"id":"sample"}],"files":[{"extensions":["sample"],"variant":"sample"}]});
    fs::write(&identity, value.to_string()).unwrap();
    let mut source = Packages::empty();
    source.add_identity_files(vec![identity.clone()]);
    let mut invalid = value;
    invalid["variants"][0]["grammar"] = json!("uninstalled");
    fs::write(&identity, invalid.to_string()).unwrap();
    source.add_identity_files(vec![identity]);
    assert_eq!(
        source
            .registry()
            .unwrap()
            .resolve_path(Path::new("a.sample"))
            .name(),
        "sample"
    );
    assert!(!source.activity().errors.is_empty());
}
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "loog-host-packs-{}-{}",
            std::process::id(),
            RUN.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        assert_eq!(self.0.parent(), Some(std::env::temp_dir().as_path()));
        let _ = fs::remove_dir_all(&self.0);
    }
}
/// A pack with one grammar, `sample`, for `.sample` files; no module or query is written.
fn write_sample(temp: &Temp) -> PathBuf {
    write_standard(
        temp,
        "sample",
        &json!({"grammars":[{"name":"sample","file-types":["sample"]}]}),
    )
}

#[test]
fn the_wasm_module_is_looked_for_beside_the_pack_by_either_standard_name() {
    let temp = Temp::new();
    let root = write_sample(&temp);
    let source = Packages::discover(&temp.0, None);
    let (package, module) = source.grammar("sample").unwrap();
    assert_eq!(module.wasm_name("sample").unwrap(), "sample");
    assert!(module.wasm_name("Sample").is_err());
    assert_eq!(
        Packages::module_path(package, "sample"),
        root.join("tree-sitter-sample.wasm")
    );
    fs::write(root.join("sample.wasm"), "unloaded payload").unwrap();
    assert_eq!(
        Packages::module_path(package, "sample"),
        root.join("sample.wasm")
    );
    assert_eq!(source.activity(), Activity::default());
}

#[test]
fn invalid_and_oversized_wasm_are_isolated_and_failures_are_cached() {
    let temp = Temp::new();
    let root = write_sample(&temp);
    let path = root.join("tree-sitter-sample.wasm");
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
}

#[test]
fn no_packages_provide_plain_text_and_no_resource_activity() {
    let temp = Temp::new();
    let source = Packages::discover(&temp.0.join("missing"), None);
    let registry = source.registry().unwrap();
    assert_eq!(registry.languages().len(), 1);
    for file in ["a.md", "a.json", "a.rs"] {
        assert_eq!(
            &*registry.resolve_path(Path::new(file)).definition().name,
            "plaintext"
        );
    }
    assert_eq!(source.activity(), Activity::default());
}

#[test]
fn metadata_recognition_is_lazy_and_missing_payload_failure_is_cached() {
    let temp = Temp::new();
    write_sample(&temp);
    let source = Packages::discover(&temp.0, None);
    let registry = source.registry().unwrap();
    assert_eq!(
        &*registry
            .resolve_path(Path::new("a.sample"))
            .definition()
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
fn a_language_json_beside_a_grammar_replaces_the_description_made_for_it() {
    let temp = Temp::new();
    let root = write_sample(&temp);
    let before = Packages::discover(&temp.0, None).registry().unwrap();
    let made = before.definition(before.language_by_name("sample").unwrap());
    assert_eq!(made.display_name.as_ref(), "sample");
    fs::write(
        root.join("language.json"),
        json!({"name":"sample","display_name":"Sample","badge":"SMP",
            "comments":{"line":"#"},
            "variants":[{"id":"sample","grammar":"sample"}],
            "files":[{"extensions":["sample"],"variant":"sample"}]})
        .to_string(),
    )
    .unwrap();
    let source = Packages::discover(&temp.0, None);
    assert!(
        source.activity().errors.is_empty(),
        "{:?}",
        source.activity().errors
    );
    let registry = source.registry().unwrap();
    let described = registry.definition(registry.language_by_name("sample").unwrap());
    assert_eq!(described.display_name.as_ref(), "Sample");
    assert_eq!(described.comments.line.as_deref(), Some("#"));
    assert_eq!(source.grammar_names(), ["sample"]);
    assert_eq!(source.activity(), Activity::default());
}

#[test]
fn invalid_packages_do_not_corrupt_valid_metadata() {
    let temp = Temp::new();
    write_sample(&temp);
    for (directory, description) in [
        ("noname", json!({"display_name":"No name"})),
        (
            "badname",
            json!({"name":"Bad Name","display_name":"x","badge":"X","variants":[{"id":"x"}]}),
        ),
        (
            "field",
            json!({"name":"field","display_name":"F","badge":"F","variants":[{"id":"field"}],"unknown":true}),
        ),
        // Its id would be `sample`, which the grammar pack above already has.
        (
            "zconflict",
            json!({"name":"sample","display_name":"S","badge":"S","variants":[{"id":"sample"}]}),
        ),
    ] {
        let folder = temp.0.join(directory);
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("language.json"), description.to_string()).unwrap();
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
    assert_eq!(source.activity().errors.len(), 4);
    assert_eq!(source.activity().language_loads, 0);
}

#[test]
fn platform_paths_are_absolute_and_portable_directories_are_not_cargo_profiles() {
    let home = std::env::temp_dir().join("loog-data");
    assert_eq!(
        user_resource_directory_from(
            "windows",
            |key| (key != "LOOG_EXTENSIONS_DIR").then(|| home.clone().into_os_string()),
            "extensions"
        ),
        Some(home.join("Loog/extensions"))
    );
    let staged = home.join("staged");
    let get = |key: &str| (key == "LOOG_EXTENSIONS_DIR").then(|| staged.clone().into_os_string());
    assert_eq!(
        user_resource_directory_from("windows", get, "extensions"),
        Some(staged.clone())
    );
    assert_eq!(
        user_resource_directory_from("windows", get, "queries"),
        None
    );
    assert_eq!(
        user_resource_directory_from("windows", |_| Some("relative".into()), "extensions"),
        None
    );
    assert_eq!(
        bundled_directory(Path::new("/portable/deps/Loog.exe"), "extensions"),
        PathBuf::from("/portable/deps/extensions")
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
                .resolve_path(Path::new(path))
                .definition()
                .name
                .as_ref(),
            "sample"
        );
    }
    assert_eq!(
        registry
            .resolve_path(Path::new("hello.test"))
            .definition()
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
                "contributes": { "highlight": list } })
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

    let extensions = temp.0.join("extensions");
    let found = packs_in(
        ["other", "weave"]
            .map(|name| extensions.join(name))
            .to_vec(),
        &Default::default(),
    );
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
                .resolve_path(Path::new(file))
                .definition()
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
fn an_extension_can_contribute_a_language_that_is_only_a_description() {
    let temp = Temp::new();
    let package = extension_package(&temp, "notes", &["lang/language.json"]);
    fs::create_dir_all(package.join("lang")).unwrap();
    fs::write(
        package.join("lang/language.json"),
        json!({"name":"notes","display_name":"Notes","badge":"NT",
            "highlight":{"keywords":["todo"]},
            "variants":[{"id":"notes"}],
            "files":[{"extensions":["notes"],"variant":"notes"}]})
        .to_string(),
    )
    .unwrap();
    assert!(check_extension_pack(&package, "lang/language.json").is_ok());
    assert!(check_extension_pack(&package, "lang/package.json").is_err());
    let found = packs_in(vec![package.clone()], &Default::default());
    assert_eq!(found, [package.join("lang").canonicalize().unwrap()]);
    let registry = Packages::from_directories(found).registry().unwrap();
    let resolved = registry.resolve_path(Path::new("a.notes"));
    assert_eq!(resolved.definition().name.as_ref(), "notes");
    assert!(resolved.grammar().is_none());
}

#[test]
fn an_extension_pack_cannot_replace_a_bundled_collection_but_a_user_copy_replaces_both() {
    let temp = Temp::new();
    let bundled = temp.0.join("bundled");
    write_pack(&bundled, "tree-sitter-weave", "weave", "wv");
    let package = extension_package(&temp, "weave", &["g/tree-sitter.json"]);
    write_pack(&package, "g", "weave", "weave2");
    let found = packs_in(vec![package.clone()], &Default::default());

    let source = Packages::discover_with(&bundled, &found, None);
    let registry = source.registry().unwrap();
    let name = |file: &str| {
        registry
            .resolve_path(Path::new(file))
            .definition()
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
            .resolve_path(Path::new("a.mine"))
            .definition()
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
                .resolve_path(Path::new(file))
                .definition()
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
fn standard_manifest_is_authoritative_and_a_package_json_is_not_read() {
    let temp = Temp::new();
    let root = write_standard(&temp, "sample", &json!({"grammars":[]}));
    // The old pack format is not a fallback.
    fs::write(
        root.join("package.json"),
        json!({"schema":1,"id":"sample","languages":[],"grammars":{}}).to_string(),
    )
    .unwrap();
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
    assert_eq!(rust.definition().comments.line.as_deref(), Some("//"));
}
