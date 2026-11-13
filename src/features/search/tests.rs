use super::model::{SearchKey, SearchModel};
use super::ports::{SearchSourcePort, SourceEntry};
use super::*;
use crate::domain::document_snapshot::DocumentSnapshot;
use loog_workspace_api::ResourceRef;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

pub(crate) struct MemorySource {
    pub(crate) files: HashMap<PathBuf, Result<String, String>>,
    reads: Mutex<Vec<PathBuf>>,
}
impl Default for MemorySource {
    fn default() -> Self {
        Self {
            files: HashMap::from([(PathBuf::from("project/note.txt"), Ok("needle".into()))]),
            reads: Mutex::default(),
        }
    }
}
impl SearchSourcePort for MemorySource {
    fn normalize(&self, path: &Path) -> PathBuf {
        path.into()
    }
    fn is_directory(&self, path: &Path) -> bool {
        path == Path::new("project")
    }
    fn read_text(&self, path: &Path) -> Result<String, String> {
        self.reads.lock().unwrap().push(path.into());
        self.files
            .get(path)
            .cloned()
            .unwrap_or_else(|| Err("unavailable".into()))
    }
    fn walk(
        &self,
        _: &Path,
        _: &SearchSpec,
        _: Arc<Matcher>,
    ) -> Box<dyn Iterator<Item = Result<SourceEntry, String>> + '_> {
        let mut paths = self.files.keys().cloned().collect::<Vec<_>>();
        paths.sort();
        Box::new(paths.into_iter().map(|path| {
            Ok(SourceEntry {
                path,
                is_file: true,
            })
        }))
    }
}
pub(crate) fn spec(value: &str) -> SearchSpec {
    SearchSpec {
        query: value.into(),
        include: String::new(),
        exclude: String::new(),
        case_sensitive: false,
        whole_word: false,
        regex: false,
        include_hidden: false,
        include_ignored: false,
    }
}
pub(crate) fn key(value: &str) -> SearchKey {
    SearchKey {
        scopes: Vec::new(),
        spec: spec(value),
        roots: vec![ResourceRef::local("project")],
        versions: Vec::new(),
        composing: false,
        refresh: 0,
    }
}
pub(crate) fn request() -> Request {
    Request {
        spec: spec("needle"),
        roots: vec!["project".into()],
        buffers: Vec::new(),
    }
}
pub(crate) fn result(value: &str) -> Results {
    let matches =
        Matcher::new(&spec("needle"))
            .unwrap()
            .matches(value, &AtomicBool::new(false), 10);
    Results {
        matches: matches.len(),
        files: vec![Arc::new(FileMatches {
            path: ResourceRef::local("project/note.txt"),
            root: ResourceRef::local("project"),
            provider: None,
            content_hash: hash(value),
            matches,
        })],
        complete: true,
        ..Results::default()
    }
}

#[test]
fn injected_source_prefers_dirty_snapshots_and_deduplicates_overlapping_roots() {
    let source = MemorySource::default();
    let mut request = request();
    request.roots.push("project".into());
    request.buffers.push(DocumentSnapshot {
        path: "project/note.txt".into(),
        text: Arc::new("dirty needle 中文".into()),
        version: 3,
    });
    let mut found = Results::default();
    service::run(&source, request, &AtomicBool::new(false), |next| {
        found = next
    })
    .unwrap();
    assert_eq!(found.matches, 1);
    assert_eq!(found.searched, 1);
    assert_eq!(found.files[0].content_hash, hash("dirty needle 中文"));
    assert!(source.reads.lock().unwrap().is_empty());
}

#[test]
fn injected_source_preserves_skip_categories_and_read_errors() {
    let mut source = MemorySource::default();
    source.files.extend([
        ("project/binary".into(), Err("Binary file".into())),
        (
            "project/encoding".into(),
            Err("Unsupported text encoding".into()),
        ),
        (
            "project/large".into(),
            Err("File is larger than 4 MiB".into()),
        ),
        ("project/error".into(), Err("read denied".into())),
    ]);
    let mut found = Results::default();
    service::run(&source, request(), &AtomicBool::new(false), |next| {
        found = next
    })
    .unwrap();
    assert_eq!(
        (
            found.skipped_binary,
            found.skipped_encoding,
            found.skipped_large,
            found.errors
        ),
        (1, 1, 1, 1)
    );
    assert_eq!(found.matches, 1);
    assert_eq!(found.first_error.as_deref(), Some("read denied"));
}

#[test]
fn cancelled_request_publishes_a_final_snapshot_without_reading_disk() {
    let source = MemorySource::default();
    let mut found = None;
    service::run(&source, request(), &AtomicBool::new(true), |next| {
        found = Some(next)
    })
    .unwrap();
    let found = found.unwrap();
    assert!(found.complete && found.cancelled);
    assert!(found.files.is_empty());
    assert!(source.reads.lock().unwrap().is_empty());
}

#[test]
fn late_success_and_error_from_an_old_generation_cannot_replace_current_results() {
    let mut model = SearchModel::default();
    let old = key("old");
    let latest = key("needle");
    let a = model.begin(old.clone());
    let b = model.begin(latest.clone());
    assert!(
        model
            .apply(b.generation, &latest, &latest, false, Ok(result("needle")))
            .changed
    );
    let current = model.status().results.clone();
    for update in [Ok(result("old needle")), Err("old failure".into())] {
        assert!(
            !model
                .apply(a.generation, &old, &latest, false, update)
                .changed
        );
    }
    assert!(Arc::ptr_eq(&model.status().results, &current));
    assert!(model.status().error.is_none());
}

#[test]
fn changing_roots_or_dirty_versions_rejects_in_flight_results() {
    for change_roots in [true, false] {
        let mut model = SearchModel::default();
        let expected = key("needle");
        let start = model.begin(expected.clone());
        let mut current = expected.clone();
        if change_roots {
            current.roots = vec![ResourceRef::local("other")];
        } else {
            current
                .versions
                .push((ResourceRef::local("project/note.txt"), 2));
        }
        assert!(
            !model
                .apply(
                    start.generation,
                    &expected,
                    &current,
                    false,
                    Ok(result("needle"))
                )
                .changed
        );
    }
}

#[test]
fn retained_results_survive_empty_partial_or_cancelled_updates_but_not_empty_completion() {
    let mut model = SearchModel::default();
    let key = key("needle");
    let first = model.begin(key.clone());
    model.apply(first.generation, &key, &key, false, Ok(result("needle")));
    let current = model.status().results.clone();
    let refresh = model.begin(key.clone());
    assert!(refresh.retained);
    assert!(
        !model
            .apply(
                refresh.generation,
                &key,
                &key,
                false,
                Ok(Results::default())
            )
            .replaced
    );
    assert!(Arc::ptr_eq(&current, &model.status().results));
    let cancelled = Results {
        complete: true,
        cancelled: true,
        ..Results::default()
    };
    assert!(
        !model
            .apply(refresh.generation, &key, &key, true, Ok(cancelled))
            .replaced
    );
    assert!(Arc::ptr_eq(&current, &model.status().results));
    assert!(
        model
            .apply(
                refresh.generation,
                &key,
                &key,
                false,
                Ok(Results {
                    complete: true,
                    ..Results::default()
                })
            )
            .replaced
    );
    assert!(model.status().results.files.is_empty());
}

#[test]
fn ime_drafts_do_not_start_search_and_clear_invalidates_all_pending_results() {
    let mut model = SearchModel::default();
    let mut composing = key("needle");
    composing.composing = true;
    assert!(!model.begin(composing).running);
    let key = key("needle");
    let start = model.begin(key.clone());
    model.clear();
    assert!(
        !model
            .apply(start.generation, &key, &key, false, Ok(result("needle")))
            .changed
    );
}

#[test]
fn navigation_intent_is_scoped_to_current_results_and_uses_unsaved_text() {
    let source = MemorySource::default();
    let mut model = SearchModel::default();
    let key = key("needle");
    let begin = model.begin(key.clone());
    model.apply(
        begin.generation,
        &key,
        &key,
        false,
        Ok(result("old needle")),
    );
    let file = model.status().results.files[0].clone();
    let intent = model
        .navigation(&key, file.clone(), 0, false, false)
        .unwrap();
    assert!(!intent.permanent && !intent.focus_editor);
    assert!(
        model
            .navigation(&key, Arc::new((*file).clone()), 0, true, true)
            .is_none()
    );
    let buffer = DocumentSnapshot {
        path: file.path.local_path().unwrap().to_path_buf(),
        text: Arc::new("prefix\nold needle".into()),
        version: 4,
    };
    let (text, range) = navigation::prepare(&source, &intent, Some(&buffer)).unwrap();
    assert_eq!(&text[range], "needle");
    assert!(source.reads.lock().unwrap().is_empty());
    let permanent = model.navigation(&key, file, 0, true, true).unwrap();
    assert!(permanent.permanent && permanent.focus_editor);
}

#[test]
fn stale_line_navigation_requests_refresh_instead_of_selecting_unrelated_text() {
    let source = MemorySource::default();
    let mut model = SearchModel::default();
    let key = key("needle");
    let begin = model.begin(key.clone());
    model.apply(
        begin.generation,
        &key,
        &key,
        false,
        Ok(result("old needle")),
    );
    let intent = model
        .navigation(
            &key,
            model.status().results.files[0].clone(),
            0,
            false,
            false,
        )
        .unwrap();
    let buffer = DocumentSnapshot {
        path: intent.file.path.local_path().unwrap().to_path_buf(),
        text: Arc::new("unrelated needle".into()),
        version: 4,
    };
    assert!(
        matches!(navigation::prepare(&source, &intent, Some(&buffer)),
        Err(navigation::OpenError::Changed(message)) if message.contains("File changed"))
    );
}
