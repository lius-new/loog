use super::*;
use crate::{
    domain::{identity::FileId, language},
    features::{
        highlighting::HighlightId,
        syntax::{Source, Window, WindowTokens},
    },
};
use loom_editor_core::text::{EditKind, EditorMut, Selection, TextBuffer};
use serde_json::json;
use std::{
    process::Command,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

static RUN: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "loom-wasm-{}-{}",
            std::process::id(),
            RUN.fetch_add(1, Ordering::Relaxed)
        ));
        let input = PathBuf::from(
            std::env::var_os("LOOM_TEST_WASM_PACKAGES")
                .expect("supply the pinned upstream WASM fixture"),
        );
        fs::create_dir_all(path.join("rust")).unwrap();
        fs::create_dir_all(path.join("rust/queries")).unwrap();
        for file in [
            "tree-sitter.json",
            "tree-sitter-rust.wasm",
            "queries/highlights.scm",
            "queries/injections.scm",
        ] {
            fs::copy(input.join("rust").join(file), path.join("rust").join(file)).unwrap();
        }
        Self(path)
    }
    fn source(&self) -> Arc<Packages> {
        Arc::new(Packages::discover(&self.0, None))
    }
    fn module(&self) -> PathBuf {
        self.0.join("rust/tree-sitter-rust.wasm")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        assert_eq!(self.0.parent(), Some(std::env::temp_dir().as_path()));
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn paint(index: &mut TreeIndex, buffer: &TextBuffer) -> Vec<WindowTokens> {
    index
        .prepare(
            FileId::new(1),
            Source::new(buffer),
            language::builtin("rust"),
            &[Window {
                line: 0..buffer.len(),
                range: 0..buffer.len(),
            }],
            &AtomicBool::new(false),
        )
        .expect("resource failure is plain text, not cancellation")
}

#[test]
fn checked_in_json_and_markdown_are_complete_loadable_wasm_packages() {
    use crate::features::syntax::{Resources, validate_producer_grammar};
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/extensions");
    let source = Packages::discover(&root, None);
    assert!(
        source.activity().errors.is_empty(),
        "{:?}",
        source.activity()
    );
    source.registry().unwrap();
    for (name, text) in [
        ("json", "{\"value\": true}"),
        ("markdown", "# Heading\n\n**bold**\n"),
        ("markdown_inline", "**bold** `code`"),
    ] {
        let parser = source.parser(name).unwrap();
        assert!(validate_producer_grammar(&source, name, parser, text).unwrap() > 0);
    }
    assert_eq!(source.activity().wasm_loads, 3);
    assert!(source.activity().errors.is_empty());
}

#[test]
fn cold_discovery_leaves_wasm_engine_uninitialized_then_uses_upstream_bytes() {
    const CHILD: &str = "LOOM_WASM_COLD_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "infrastructure::language_packs::wasm_tests::cold_discovery_leaves_wasm_engine_uninitialized_then_uses_upstream_bytes", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    assert!(WASM_ENGINE.get().is_none());
    let fixture = Fixture::new();
    let source = fixture.source();
    assert_eq!(
        source
            .registry()
            .unwrap()
            .definition_of(source.registry().unwrap().resolve_path(Path::new("new.rs")))
            .name
            .as_ref(),
        "rust"
    );
    assert_eq!(source.activity(), Activity::default());
    assert!(WASM_ENGINE.get().is_none());
    let language = source.language("rust").unwrap();
    assert!(language.is_wasm());
    assert!(WASM_ENGINE.get().is_some());
    let loaded = source.activity();
    assert_eq!(loaded.wasm_loads, 1);
    assert_eq!(
        loaded.wasm_modules,
        BTreeSet::from(["tree-sitter-rust.wasm".to_owned()])
    );
    source.language("rust").unwrap();
    assert_eq!(source.activity(), loaded);
    assert!(!fixture.0.join("rust/package.json").exists());
    assert!(!fixture.0.join("rust/SOURCE.md").exists());
    // The loading store was dropped already. Each execution store and the
    // reference-counted language must remain usable after its adapter is gone.
    let mut first = source.parser("rust").unwrap();
    let mut second = source.parser("rust").unwrap();
    drop(source);
    let text = "fn hello() { let s = r#\"raw string\"#; /* outer /* nested */ */ }";
    let tree = first.parse(text, None).unwrap();
    assert!(!tree.root_node().has_error());
    drop(first);
    assert!(!second.parse(text, None).unwrap().root_node().has_error());
    assert!(
        tree_sitter::Query::new(&language, "(function_item name: (identifier) @function)").is_ok()
    );
}

#[test]
fn wasm_incremental_edits_and_multiple_indexes_match_fresh_parses() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let mut index = TreeIndex::new(source.clone());
    let mut buffer = TextBuffer::new("fn first() { let s = r#\"hello\"#; /* /* nested */ */ }\n");
    assert!(
        paint(&mut index, &buffer)[0]
            .spans
            .iter()
            .any(|s| s.class == Some(HighlightId::KEYWORD))
    );
    for text in ["// inserted\n", "fn added() {}\n", "/* unfinished\n"] {
        EditorMut::new(&mut buffer, &mut Selection::default(), Vec::new())
            .replace(0..0, text, EditKind::Other)
            .unwrap();
        let current = paint(&mut index, &buffer);
        let fresh = paint(&mut TreeIndex::new(source.clone()), &buffer);
        assert_eq!(current[0].spans, fresh[0].spans);
    }
    assert_eq!(index.stats.incremental_parses, 3);
    assert_eq!(source.activity().wasm_loads, 1);
    assert!(source.activity().errors.is_empty());
}

#[test]
fn visible_wasm_injections_use_separate_stores_for_host_and_layer() {
    let fixture = Fixture::new();
    fs::write(
        fixture.0.join("rust/queries/injections.scm"),
        r#"((string_literal (string_content) @injection.content) @_literal
          (#match? @_literal "^\"fn ")
          (#set! injection.language "rust"))"#,
    )
    .unwrap();
    let source = fixture.source();
    let mut index = TreeIndex::new(source.clone());
    let text = "fn host() { let snippet = \"fn nested() {}\"; }\n";
    let buffer = TextBuffer::new(text);
    let result = paint(&mut index, &buffer);
    let position = text.find("fn nested").unwrap();
    assert!(
        result[0]
            .spans
            .iter()
            .any(|s| s.range.contains(&position) && s.class == Some(HighlightId::KEYWORD))
    );
    assert!(index.layer_summary(FileId::new(1)).contains(&("rust", 1)));
    assert_eq!(source.activity().wasm_loads, 1);
    assert!(source.activity().errors.is_empty());
}

#[test]
fn corrupt_wasm_and_wrong_exports_fail_once_without_poisoning_other_packages() {
    let fixture = Fixture::new();
    let manifest_path = fixture.0.join("rust/tree-sitter.json");
    let original = fs::read_to_string(&manifest_path).unwrap();
    let mut manifest: Value = serde_json::from_str(&original).unwrap();
    manifest["grammars"][0]["name"] = json!("missing");
    fs::copy(
        fixture.module(),
        fixture.0.join("rust/tree-sitter-missing.wasm"),
    )
    .unwrap();
    fs::write(&manifest_path, manifest.to_string()).unwrap();
    let source = fixture.source();
    assert!(source.language("missing").is_err());
    let failed = source.activity();
    assert_eq!(failed.errors.len(), 1);
    assert!(source.language("missing").is_err());
    assert_eq!(source.activity(), failed);
    fs::write(manifest_path, original).unwrap();
    // A valid core header with no Tree-sitter dylink data exercises the official
    // WASM parser's error path, not just the loader's magic-byte check.
    fs::write(fixture.module(), b"\0asm\x01\0\0\0").unwrap();
    let broken = fixture.source();
    assert!(broken.language("rust").is_err());
    let failed = broken.activity();
    assert!(broken.language("rust").is_err());
    assert_eq!(broken.activity(), failed);
    let valid = Fixture::new();
    assert!(valid.source().language("rust").unwrap().is_wasm());
}

#[test]
fn parser_store_setup_failure_is_plain_text_instead_of_panicking_or_retrying() {
    struct WithoutExecutionStore(Arc<Packages>);
    impl Resources for WithoutExecutionStore {
        fn language(&self, grammar: &str) -> Result<Language, String> {
            self.0.language(grammar)
        }
        fn text(&self, grammar: &str, file: &str) -> TextResult {
            self.0.text(grammar, file)
        }
        fn report(&self, error: &str) {
            self.0.report(error);
        }
    }
    let fixture = Fixture::new();
    let source = fixture.source();
    let mut index = TreeIndex::new(Arc::new(WithoutExecutionStore(source.clone())));
    let result = paint(&mut index, &TextBuffer::new("fn example() {}"));
    assert!(result[0].spans.iter().all(|s| s.class.is_none()));
    assert_eq!(index.stats.plain_fallbacks, 1);
    assert_eq!(source.activity().errors.len(), 1);
}

#[test]
fn wasm_replacement_and_removal_are_applied_only_after_rediscovery() {
    let fixture = Fixture::new();
    let source = fixture.source();
    assert!(source.language("rust").unwrap().is_wasm());
    let user = fixture.0.join("user/tree-sitter-rust");
    fs::create_dir_all(&user).unwrap();
    let mut replacement: Value =
        serde_json::from_str(&fs::read_to_string(fixture.0.join("rust/tree-sitter.json")).unwrap())
            .unwrap();
    replacement["grammars"][0]["file-types"] = json!(["downloaded"]);
    fs::write(user.join("tree-sitter.json"), replacement.to_string()).unwrap();
    let restarted = Packages::discover(&fixture.0, Some(&fixture.0.join("user")));
    assert_eq!(restarted.activity(), Activity::default());
    assert!(restarted.language("rust").is_err());
    assert!(source.language("rust").unwrap().is_wasm());
    fs::remove_file(user.join("tree-sitter.json")).unwrap();
    let restored = Packages::discover(&fixture.0, Some(&fixture.0.join("user")));
    assert!(restored.language("rust").unwrap().is_wasm());
}

#[test]
fn wasm_worker_loads_and_publishes_from_the_background_thread() {
    const CHILD: &str = "LOOM_WASM_WORKER_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "infrastructure::language_packs::wasm_tests::wasm_worker_loads_and_publishes_from_the_background_thread", "--nocapture"])
            .env(CHILD, "1")
            .env("LOOM_TEST_LANGUAGE_PACKS", std::env::var_os("LOOM_TEST_WASM_PACKAGES").unwrap())
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    use crate::{
        domain::{document::DocumentStamp, identity::PaneId},
        infrastructure::syntax_worker::Worker,
    };
    use std::{sync::mpsc, time::Duration};
    let source = resources();
    assert_eq!(source.activity(), Activity::default());
    assert!(WASM_ENGINE.get().is_none());
    let (send, receive) = mpsc::channel();
    let worker = Worker::new(move |stamp| {
        let _ = send.send(stamp);
    });
    let buffer = TextBuffer::new("fn background() { let raw = r#\"value\"#; }\n");
    let stamp = DocumentStamp::new(FileId::new(1), 1, 0, &buffer);
    let query = || {
        worker.query(
            PaneId::new(1),
            stamp,
            Source::new(&buffer),
            language::builtin("rust"),
            vec![Window {
                line: 0..buffer.len(),
                range: 0..buffer.len(),
            }],
        )
    };
    assert!(query().is_empty());
    assert_eq!(
        receive.recv_timeout(Duration::from_secs(10)).unwrap(),
        stamp
    );
    assert!(
        query()[0]
            .spans
            .iter()
            .any(|s| s.class == Some(HighlightId::KEYWORD))
    );
    assert_eq!(source.activity().wasm_loads, 1);
    assert!(source.activity().errors.is_empty());
    worker.shutdown();
}

#[test]
fn wasm_tree_folding_is_structural_cached_and_rejects_a_new_text_snapshot() {
    let fixture=Fixture::new();
    let mut trees=crate::features::syntax::TreeIndex::new(fixture.source());
    let id=FileId::new(42001);
    let mut buffer=TextBuffer::new("impl A {\n fn run(&self) {\n  if true {\n   work();\n  }\n }\n}\n");
    let source=Source::new(&buffer);let cancel=AtomicBool::new(false);
    let window=Window{line:0..buffer.len(),range:0..buffer.len()};
    trees.prepare(id,source.clone(),language::builtin("rust"),&[window],&cancel).unwrap();
    let ranges=trees.folding(id,&source,&cancel).expect("current Rust syntax tree");
    assert!(ranges.len()>=3,"{ranges:?}");
    assert!(ranges.iter().all(|r|r.start<r.end && r.end<=buffer.len()));
    assert!(Arc::ptr_eq(&ranges,&trees.folding(id,&source,&cancel).unwrap()));
    let mut selection=Selection::default();EditorMut::new(&mut buffer,&mut selection,Vec::new()).replace(0..0,"// new\n",EditKind::Other).unwrap();
    let newer=Source::new(&buffer);assert!(trees.folding(id,&newer,&cancel).is_none());
    let window=Window{line:0..buffer.len(),range:0..buffer.len()};
    trees.prepare(id,newer.clone(),language::builtin("rust"),&[window],&cancel).unwrap();
    let new_ranges=trees.folding(id,&newer,&cancel).unwrap();
    assert_eq!(new_ranges.len(),ranges.len());assert_eq!(new_ranges[0].start,ranges[0].start+7);
    assert!(trees.folding(id,&newer,&AtomicBool::new(true)).is_none());
}
