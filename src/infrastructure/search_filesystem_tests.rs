use super::*;
use crate::features::search::*;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
static NEXT: AtomicU64 = AtomicU64::new(0);
fn spec(query: &str) -> SearchSpec {
    SearchSpec {
        query: query.into(),
        include: String::new(),
        exclude: String::new(),
        case_sensitive: false,
        whole_word: false,
        regex: false,
        include_hidden: false,
        include_ignored: false,
    }
}
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "loog-search-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn scan(root: &Path, spec: SearchSpec, buffers: Vec<BufferSnapshot>) -> Results {
    let mut result = Results::default();
    run(
        Request {
            spec,
            roots: vec![root.into(), root.join("nested")],
            buffers,
        },
        &AtomicBool::new(false),
        |next| result = next,
    )
    .unwrap();
    result
}
#[test]
fn removed_paths_keep_their_normalized_parent_identity() {
    let temp = Temp::new();
    std::fs::create_dir(temp.0.join("nested")).unwrap();
    let root = temp.0.join("nested/..");
    let file = root.join("a.rs");
    std::fs::write(&file, "needle").unwrap();
    let normalized = normalized_path(&file);
    std::fs::remove_file(&file).unwrap();
    assert_eq!(normalized_path(&file), normalized);
    assert!(paths_affect_search(&spec("needle"), &[root], &[file]));
}

#[test]
fn watcher_scope_uses_search_filters_and_ignore_rules() {
    let temp = Temp::new();
    let root = &temp.0;
    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::create_dir_all(root.join("nested")).unwrap();
    std::fs::write(root.join(".gitignore"), "target/\nignored.rs\n").unwrap();
    std::fs::write(root.join("nested/.rgignore"), "skip.rs\n").unwrap();
    for name in [
        "a.rs",
        "ignored.rs",
        ".hidden.rs",
        "target/generated.rs",
        "nested/skip.rs",
        "nested/b.rs",
        "b.txt",
    ] {
        std::fs::write(root.join(name), "needle").unwrap();
    }
    let mut query = spec("needle");
    query.include = "*.rs".into();
    let affects = |query: &SearchSpec, name: &str| {
        paths_affect_search(query, std::slice::from_ref(root), &[root.join(name)])
    };
    assert!(affects(&query, "a.rs"));
    assert!(affects(&query, "nested/b.rs"));
    for name in [
        "ignored.rs",
        ".hidden.rs",
        "target/generated.rs",
        "nested/skip.rs",
        "b.txt",
        ".git/index",
    ] {
        assert!(
            !affects(&query, name),
            "{name} should not invalidate search"
        );
    }
    assert!(affects(&query, ".gitignore"));
    query.include_hidden = true;
    assert!(affects(&query, ".hidden.rs"));
    query.include_ignored = true;
    assert!(affects(&query, "target/generated.rs"));
    assert!(affects(&query, "nested/skip.rs"));
    query.exclude = "target/**, nested/**".into();
    assert!(!affects(&query, "target/generated.rs"));
    assert!(!affects(&query, "nested/b.rs"));
    query.include_ignored = false;
    std::fs::remove_file(root.join("target/generated.rs")).unwrap();
    assert!(!affects(&query, "target/generated.rs"));
    query.exclude.clear();
    assert!(!affects(&query, "target/generated.rs"));
    std::fs::remove_file(root.join("a.rs")).unwrap();
    assert!(affects(&query, "a.rs"));
    std::fs::remove_dir_all(root.join("nested")).unwrap();
    assert!(affects(&query, "nested"));
}
#[test]
fn unopened_nested_files_ignore_filters_and_buffer_override() {
    let temp = Temp::new();
    let root = &temp.0;
    std::fs::create_dir_all(root.join("nested")).unwrap();
    std::fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
    for (path, text) in [
        ("nested/a.rs", "disk"),
        ("ignored.txt", "needle"),
        (".hidden", "needle"),
        ("other.txt", "needle"),
        ("binary", "needle\0"),
    ] {
        std::fs::write(root.join(path), text).unwrap();
    }
    let buffer = BufferSnapshot {
        path: root.join("nested/a.rs"),
        text: Arc::new("needle 中文".into()),
        version: 1,
    };
    let mut query = spec("needle");
    query.include = "*.rs".into();
    let result = scan(root, query, vec![buffer.clone()]);
    assert_eq!(result.matches, 1);
    assert_eq!(result.files.len(), 1);
    let result = scan(root, spec("needle"), vec![buffer]);
    assert_eq!(result.matches, 2);
    assert_eq!(result.skipped, 1);
    let mut query = spec("needle");
    query.include_hidden = true;
    query.include_ignored = true;
    assert_eq!(scan(root, query, vec![]).matches, 3);
}
#[test]
fn chinese_crlf_literal_regex_and_relocation() {
    let mut query = spec("中文");
    query.case_sensitive = true;
    let matcher = Matcher::new(&query).unwrap();
    let text = "abc\r\n中文😀中文\r\n";
    let matches = matcher.matches(text, &AtomicBool::new(false), 20);
    assert_eq!(matches[0].range, 5..11);
    assert_eq!(matches[0].line, 1);
    let file = FileMatches {
        path: PathBuf::new(),
        root: PathBuf::new(),
        matches: matches.clone(),
        content_hash: hash(text),
    };
    assert_eq!(
        matcher.locate(&format!("new\n{text}"), &file, &matches[1]),
        Some(19..25)
    );
    assert!(
        matcher
            .locate("different 中文", &file, &matches[0])
            .is_none()
    );
    query.query = "[".into();
    query.regex = true;
    assert!(Matcher::new(&query).is_err());
    query.regex = false;
    assert!(Matcher::new(&query).is_ok());
}
#[test]
fn filters_whole_word_zero_width_and_result_limit() {
    let mut query = spec("one");
    query.whole_word = true;
    query.include = "src/**/*.{rs,ts}, *.txt".into();
    query.exclude = "vendor/**".into();
    let matcher = Matcher::new(&query).unwrap();
    assert!(matcher.includes(Path::new("src/lib/main.rs")));
    assert!(matcher.includes(Path::new("nested/a.txt")));
    assert!(!matcher.includes(Path::new("vendor/a.txt")));
    assert_eq!(
        matcher
            .matches("ONE someone one", &AtomicBool::new(false), 1)
            .len(),
        1
    );
    query.query = "^".into();
    query.regex = true;
    query.whole_word = false;
    assert_eq!(
        Matcher::new(&query)
            .unwrap()
            .matches("中文\n", &AtomicBool::new(false), 10)
            .len(),
        2
    );
    let temp = Temp::new();
    std::fs::write(temp.0.join("many"), "a ".repeat(MAX_MATCHES + 2)).unwrap();
    let result = scan(&temp.0, spec("a"), vec![]);
    assert_eq!(result.matches, MAX_MATCHES);
    assert!(result.limit_hit);
}
#[test]
fn cancellation_is_observed_between_files() {
    let temp = Temp::new();
    std::fs::write(temp.0.join("a"), "needle").unwrap();
    let mut result = Results::default();
    run(
        Request {
            spec: spec("needle"),
            roots: vec![temp.0.clone()],
            buffers: vec![],
        },
        &AtomicBool::new(true),
        |next| result = next,
    )
    .unwrap();
    assert!(result.cancelled);
    assert_eq!(result.searched, 0);
}
