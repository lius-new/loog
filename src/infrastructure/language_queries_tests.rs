//! Stacked queries: extensions and the user add to or replace a grammar's own,
//! a bad layer is left out, and a change is announced to the syntax worker.
use super::*;
use std::sync::atomic::AtomicUsize;

static RUN: AtomicUsize = AtomicUsize::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "loom-queries-{}-{}",
            std::process::id(),
            RUN.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn write(&self, relative: &str, text: &str) -> PathBuf {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The JSON extension that ships with Loom: the smallest real grammar there is.
/// The standalone host is tested with every language extension removed.
fn packages() -> Option<Packages> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/extensions");
    root.join("loom.lang.json")
        .is_dir()
        .then(|| Packages::discover(&root, None))
}

fn highlights(packages: &Packages) -> String {
    packages
        .text("json", "highlights.scm")
        .unwrap()
        .unwrap()
        .to_string()
}

#[test]
fn extending_replacing_and_rejected_layers_apply_in_order() {
    let Some(packages) = packages() else {
        return;
    };
    let base = highlights(&packages);
    assert!(!base.contains("@keyword"));
    let temp = Temp::new();

    // An extending file in the user's folder is appended.
    temp.write("json/highlights.scm", "; extends\n(number) @keyword\n");
    packages.set_query_sources(Some(temp.0.clone()), Vec::new(), false);
    let extended = highlights(&packages);
    assert!(extended.starts_with(base.trim_end()) && extended.contains("(number) @keyword"));
    assert!(packages.query_problems().is_empty());

    // A layer that does not compile is left out; the language keeps its colours.
    temp.write(
        "json/highlights.scm",
        "; extends\n(no_such_node) @keyword\n",
    );
    packages.set_query_sources(Some(temp.0.clone()), Vec::new(), true);
    assert_eq!(highlights(&packages), base);
    let problems = packages.query_problems();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].starts_with("json/highlights.scm:"),
        "{problems:?}"
    );

    // One without `; extends` replaces the grammar's own query.
    temp.write("json/highlights.scm", "(number) @number\n");
    packages.set_query_sources(Some(temp.0.clone()), Vec::new(), true);
    assert_eq!(highlights(&packages), "(number) @number\n");
    assert!(packages.query_problems().is_empty());
}

#[test]
fn an_extension_layer_sits_below_the_users_and_names_its_extension() {
    let Some(packages) = packages() else {
        return;
    };
    let temp = Temp::new();
    let path = temp.write("ext/queries/json.scm", "; extends\n(null) @keyword\n");
    temp.write("user/json/highlights.scm", "; extends\n(true) @keyword\n");
    let layer = ExtensionQuery {
        extension: "acme.json".into(),
        grammar: "json".into(),
        file: "highlights.scm".into(),
        path,
    };
    packages.set_query_sources(Some(temp.0.join("user")), vec![layer], false);
    let text = highlights(&packages);
    let (ext, user) = (text.find("(null) @keyword"), text.find("(true) @keyword"));
    assert!(ext.is_some() && user.is_some() && ext < user, "{text}");
}

#[test]
fn inspecting_a_position_names_the_winning_capture_and_where_it_was_written() {
    use crate::features::syntax::inspect::{describe, inspect};
    let Some(packages) = packages() else {
        return;
    };
    let temp = Temp::new();
    let file = temp.write("json/highlights.scm", "; extends\n\n(number) @keyword\n");
    packages.set_query_sources(Some(temp.0.clone()), Vec::new(), false);
    let text = r#"{"a": 12}"#;
    let inspection = inspect(&packages, "json", text, text.find("12").unwrap()).unwrap();
    assert_eq!(inspection.nodes[0], "number");
    let names: Vec<_> = inspection.hits.iter().map(|h| h.capture.as_str()).collect();
    assert_eq!(names, ["number", "keyword"], "{inspection:?}");
    let winner = inspection.hits.iter().find(|hit| hit.wins).unwrap();
    assert_eq!(winner.capture, "keyword", "the later pattern wins a tie");
    let (source, line) = winner.origin.clone().unwrap();
    assert_eq!((Path::new(&source), line), (file.as_path(), 3));
    let base = inspection.hits.iter().find(|hit| !hit.wins).unwrap();
    assert!(base.origin.clone().unwrap().0.ends_with("highlights.scm"));

    let report = describe(&inspection, |_| "#abcdef".into());
    assert!(report.contains("> @keyword -> keyword #abcdef"), "{report}");
    assert!(report.contains("  @number -> number"), "{report}");

    // A place nothing colours says so.
    let blank = inspect(&packages, "json", text, 0).unwrap();
    assert!(blank.hits.is_empty());
    assert!(describe(&blank, |_| String::new()).contains("No query colours this"));
    assert!(inspect(&packages, "json", text, 999).is_err());
}

#[test]
fn a_language_without_a_grammar_is_a_description_with_highlight_rules() {
    let temp = Temp::new();
    temp.write(
        "sqlish/language.json",
        r#"{ "name": "sqlish", "display_name": "SQL-ish", "badge": "SQL",
             "comments": { "line": "--" },
             "highlight": { "keywords": ["select", "from"], "strings": ["'"] },
             "variants": [{ "id": "sqlish" }],
             "files": [{ "extensions": ["sqlish"], "variant": "sqlish" }] }"#,
    );
    let registry = Packages::discover(&temp.0, None).registry().unwrap();
    let language = registry.language_by_name("sqlish").unwrap();
    let definition = registry.definition(language);
    assert_eq!(
        definition.highlight.keywords.as_deref(),
        Some(&["select".to_owned(), "from".to_owned()][..])
    );
    assert!(registry.default_resolved(language).grammar.is_none());
    assert!(registry.resolve_path(Path::new("q.sqlish")).language == language);
}

#[test]
fn changing_the_sources_advances_the_generation_and_calls_listeners() {
    let Some(packages) = packages() else {
        return;
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    packages.subscribe(Arc::new(move || {
        seen.fetch_add(1, Ordering::SeqCst);
    }));
    let before = packages.generation();
    packages.set_query_sources(None, Vec::new(), false);
    assert_eq!(
        (packages.generation(), calls.load(Ordering::SeqCst)),
        (before, 0)
    );

    let temp = Temp::new();
    packages.set_query_sources(Some(temp.0.clone()), Vec::new(), false);
    assert_eq!(packages.generation(), before + 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Editing a file under the same names is announced only when forced.
    packages.set_query_sources(Some(temp.0.clone()), Vec::new(), true);
    assert_eq!(packages.generation(), before + 2);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
