use super::*;
use crate::app::{git_tasks::Task, state::AppState};
use crate::features::git::{ChangeDiff, DiffContent, diff::parse_unified};
use lgui::application::{AppView, ApplicationContext};
use lgui::core::UiScale;
use lgui::prelude::{UiRect, VisualStyle, panel};
use lgui::session::UiSession;
use std::{
    collections::VecDeque,
    sync::Mutex,
    time::{Duration, Instant},
};

fn load() -> Load {
    Load {
        request: DiffRequest {
            repository_root: "repo".into(),
            path: PathBuf::new(),
            target: DiffTarget::Changes,
        },
        revision: None,
        sequence: 10,
    }
}
fn request(path: &str) -> DiffRequest {
    DiffRequest {
        repository_root: "repo".into(),
        path: path.into(),
        target: DiffTarget::IndexToWorktree,
    }
}
fn entries() -> Vec<ChangeEntry> {
    ["a.txt", "b.txt", "c.txt"]
        .map(|path| ChangeEntry {
            request: request(path),
            group: ChangeGroup::Unstaged,
        })
        .into()
}
fn content(path: &str, extra: usize) -> DiffContent {
    let mut patch = format!(
        "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -1,3 +1,{} @@\n head\n-old\n+selected new code\n",
        3 + extra
    );
    for _ in 0..extra {
        patch.push_str("+extra\n");
    }
    patch.push_str(" tail\n");
    DiffContent::Unified(parse_unified(&patch).unwrap())
}
fn file(path: &str) -> Arc<DiffDocument> {
    Arc::new(DiffDocument::from_content(request(path), content(path, 0)))
}
fn full() -> Arc<DiffDocument> {
    Arc::new(DiffDocument::from_content(
        load().request,
        DiffContent::Changes(
            entries()
                .into_iter()
                .map(|entry| ChangeDiff {
                    path: entry.request.path.clone(),
                    target: entry.request.target,
                    group: entry.group,
                    content: Ok(content(entry.request.path.to_str().unwrap(), 20)),
                })
                .collect(),
        ),
    ))
}

#[test]
fn progressive_diff_completion_preserves_scroll_collapses_and_focus() {
    let mut app = AppState::new();
    let pane = app.workspace.active_pane();
    let document = preview(&load(), Some(entries()), file("b.txt"), false);
    let id = app.workspace.open_prepared_diff(document);
    app.workspace.toggle_diff_section(pane, request("c.txt"));
    app.workspace.focus_diff_section(&request("b.txt"), true);
    let (_, y) = app.workspace.scroll(pane);
    app.workspace.set_scroll(pane, 7.0, y + 5.0);
    let other = app
        .workspace
        .test_open_view("other.txt".into(), "keep".into());
    let retired = app.workspace.update_loading_diff(full(), 10, true);
    assert_eq!(retired.len(), 1);
    assert_eq!(
        app.workspace.active(),
        Some(other),
        "background completion must not take focus"
    );
    app.workspace.activate(pane, id);
    assert_eq!(
        app.workspace.collapsed_diffs(pane),
        [request("c.txt")].into_iter().collect()
    );
    assert_eq!(app.workspace.active_diff_focus(), Some(&request("b.txt")));
    let doc = app.workspace.active_diff().unwrap();
    assert!(doc.loading.is_none());
    let layout = doc.changes_layout(true, &app.workspace.collapsed_diffs(pane));
    let b = &layout.sections[1];
    assert_eq!(
        app.workspace.scroll(pane),
        (7.0, b.top as f32 * crate::ui::diff_editor::row_h() + 5.0)
    );
    assert!(doc.sections.as_ref().unwrap().iter().all(|s| !s.loading));
}

#[test]
fn progressive_diff_completion_rejects_closed_and_superseded_tabs() {
    for closed in [true, false] {
        let mut app = AppState::new();
        let pane = app.workspace.active_pane();
        let id = app.workspace.open_prepared_diff(preview(
            &load(),
            Some(entries()),
            file("a.txt"),
            false,
        ));
        if closed {
            app.workspace.close_item(pane, id);
        } else {
            let mut newer = load();
            newer.sequence = 11;
            app.workspace.open_prepared_diff(preview(
                &newer,
                Some(entries()),
                file("b.txt"),
                false,
            ));
        }
        assert!(
            app.workspace
                .update_loading_diff(full(), 10, true)
                .is_empty()
        );
        if closed {
            assert!(
                app.workspace
                    .diff_id_for(
                        std::path::Path::new("repo"),
                        &PathBuf::new(),
                        DiffTarget::Changes
                    )
                    .is_none()
            );
        } else {
            assert_eq!(app.workspace.active_diff().unwrap().loading, Some(11));
        }
    }
}

struct Harness {
    state: AppContext,
    tasks: Arc<Mutex<VecDeque<Task>>>,
    session: UiSession,
    view: AppView,
    viewport: UiRect,
    _runtime: Arc<crate::app::runtime::AppRuntime>,
}
impl Harness {
    fn new(
        root: PathBuf,
        commit: Option<(String, Vec<(crate::features::git::ChangeKind, PathBuf)>)>,
    ) -> Self {
        let tasks = Arc::new(Mutex::new(VecDeque::<Task>::new()));
        let queue = tasks.clone();
        let application =
            lgui::ApplicationHandle::new(move |task| queue.lock().unwrap().push_back(task), || {});
        let runtime = Arc::new(crate::app::runtime::AppRuntime::default());
        runtime.set_application(application);
        let worker_runtime = runtime.clone();
        let exposed = Arc::new(Mutex::new(None));
        let output = exposed.clone();
        let viewport = UiRect::new(0.0, 0.0, 1200.0, 800.0);
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(|| {
                let mut app = AppState::new();
                app.explorer.model.restore(vec![root.clone()], Vec::new());
                app.git.ui.split_diff = true;
                app.workspace
                    .test_open_view("keep.txt".into(), "keep".into());
                if let Some((oid, files)) = &commit {
                    app.git.model.toggle_commit(oid.clone());
                    app.git.model.set_commit_files(oid, Ok(files.clone()));
                }
                app
            }))
            .with_runtime(&worker_runtime);
            *output.lock().unwrap() = Some(state.clone());
            let store = cx.state_with(GitStoreSnapshot::default);
            let id = cx.use_stable_id();
            if state.get().workspace.active_diff().is_none() {
                return panel(viewport, VisualStyle::default());
            }
            let pane = state.get().workspace.active_pane();
            crate::app::diff_view::render(
                pane,
                viewport,
                state.clone(),
                crate::app::git_view::binding(&state, &store),
                id.clone(),
                cx.focus_handle(id),
                crate::app::editor_view::binding(state, store, pane, viewport),
            )
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let state = exposed.lock().unwrap().clone().unwrap();
        Self {
            state,
            tasks,
            session,
            view,
            viewport,
            _runtime: runtime,
        }
    }
    fn render(&mut self) {
        self.session
            .render_view(&self.view, self.viewport, UiScale::ONE);
    }
    fn wait(&mut self, predicate: impl Fn(&AppState) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !predicate(&self.state.get()) {
            let task = self.tasks.lock().unwrap().pop_front();
            if let Some(task) = task {
                task();
                self.render();
            } else {
                thread::sleep(Duration::from_millis(1));
            }
            assert!(Instant::now() < deadline, "Git completion did not arrive");
        }
    }
}

#[test]
fn progressive_diff_shows_selected_code_and_retains_headers_and_controls() {
    let context = ApplicationContext::empty(Default::default());
    let _text = lgui::backend::install_text_environment(
        &context,
        lgui::render_skia::skia_text_system_handle(),
    );
    for split in [false, true] {
        let mut harness = Harness::new("repo".into(), None);
        harness.state.update(|app| {
            app.git.ui.split_diff = split;
            app.workspace.open_prepared_diff(preview(
                &load(),
                Some(entries()),
                file("a.txt"),
                false,
            ));
        });
        harness.render();
        let texts = harness
            .session
            .tree()
            .nodes()
            .iter()
            .filter_map(|node| node.text.as_deref())
            .collect::<Vec<_>>();
        for text in [
            "a.txt",
            "b.txt",
            "c.txt",
            "selected new code",
            "Loading changes…",
            "Inline",
            "Split",
        ] {
            assert!(texts.contains(&text), "missing {text}, split={split}");
        }
        for path in ["a.txt", "b.txt", "c.txt"] {
            let expected = format!("Collapse {path}");
            assert_eq!(
                harness
                    .session
                    .tree()
                    .nodes()
                    .iter()
                    .filter(
                        |node| node.semantics.as_ref().and_then(|s| s.name.as_deref())
                            == Some(expected.as_str())
                    )
                    .count(),
                1
            );
        }
    }
}

#[test]
fn progressive_diff_actual_open_publishes_selected_file_before_completion() {
    let (root, backend) = crate::infrastructure::git::backend::tests::required_repository();
    // Discovery canonicalizes roots; temp paths may contain aliases or short names.
    let root = std::fs::canonicalize(root).expect("canonicalize Git fixture path");
    for path in ["a.txt", "b.txt"] {
        std::fs::write(root.join(path), "before\n").unwrap();
    }
    backend
        .stage(&root, &["a.txt".into(), "b.txt".into()])
        .unwrap();
    backend.commit(&root, "base", false, false).unwrap();
    for path in ["a.txt", "b.txt"] {
        std::fs::write(root.join(path), "after\n").unwrap();
    }
    let service = crate::infrastructure::git::service().unwrap();
    let snapshot = service.refresh(std::slice::from_ref(&root), None).unwrap();
    let root = snapshot
        .repository_for_path(&root)
        .expect("canonicalized Git fixture must be in the snapshot")
        .worktree_root
        .clone();
    let context = ApplicationContext::empty(Default::default());
    let _text = lgui::backend::install_text_environment(
        &context,
        lgui::render_skia::skia_text_system_handle(),
    );
    let mut harness = Harness::new(root.clone(), None);
    let selected = DiffRequest {
        repository_root: root,
        path: "a.txt".into(),
        target: DiffTarget::IndexToWorktree,
    };
    open_changes(&harness.state, selected.clone(), false, snapshot);
    harness.wait(|app| app.workspace.active_diff().is_some());
    let app = harness.state.get();
    let doc = app.workspace.active_diff().unwrap();
    assert!(doc.loading.is_some());
    let sections = doc.sections.as_ref().unwrap();
    assert_eq!(sections.len(), 2);
    assert!(!sections[0].loading && sections[1].loading);
    assert_eq!(sections[0].document.rows[1].text, "after");
    harness.wait(|app| {
        app.workspace
            .active_diff()
            .is_some_and(|doc| doc.loading.is_none())
    });
    assert_eq!(
        harness
            .state
            .get()
            .workspace
            .active_diff()
            .unwrap()
            .changed_file_count(),
        2
    );
    assert_eq!(
        harness.state.get().workspace.active_diff_focus(),
        Some(&selected)
    );
}

#[test]
#[ignore = "manual first-visible benchmark: requires the desktop identical-patch fixture"]
fn progressive_diff_first_visible_benchmark() {
    let root = std::fs::canonicalize(std::env::var_os("LOOM_DIFF_BENCH_ROOT").unwrap()).unwrap();
    let oid = std::env::var("LOOM_DIFF_BENCH_COMMIT").unwrap();
    let service = crate::infrastructure::git::service().unwrap();
    let snapshot = service.refresh(std::slice::from_ref(&root), None).unwrap();
    let files = service.backend().commit_files(&root, &oid).unwrap();
    let context = ApplicationContext::empty(Default::default());
    let _text = lgui::backend::install_text_environment(
        &context,
        lgui::render_skia::skia_text_system_handle(),
    );
    let mut samples = Vec::new();
    for round in 0..6 {
        for commit in if round % 2 == 0 {
            [true, false]
        } else {
            [false, true]
        } {
            let mut harness =
                Harness::new(root.clone(), commit.then(|| (oid.clone(), files.clone())));
            let start = Instant::now();
            if commit {
                open_commit_file(
                    &harness.state,
                    root.clone(),
                    oid.clone(),
                    "data/group-00/file-0000.txt".into(),
                );
            } else {
                open_changes(
                    &harness.state,
                    DiffRequest {
                        repository_root: root.clone(),
                        path: "data/group-00/file-0000.txt".into(),
                        target: DiffTarget::IndexToWorktree,
                    },
                    false,
                    snapshot.clone(),
                );
            }
            harness.wait(|app| app.workspace.active_diff().is_some());
            let visible_ms = start.elapsed().as_secs_f64() * 1000.0;
            assert!(harness.session.tree().nodes().iter().any(|node| {
                node.text
                    .as_deref()
                    .is_some_and(|s| s.starts_with("record_0000_0000 state=updated_"))
            }));
            let doc = harness.state.get().workspace.active_diff().unwrap().clone();
            assert_eq!(doc.changed_file_count(), 1000);
            assert_eq!(
                doc.sections
                    .as_ref()
                    .unwrap()
                    .iter()
                    .filter(|s| !s.loading)
                    .count(),
                1
            );
            harness.wait(|app| {
                app.workspace
                    .active_diff()
                    .is_some_and(|doc| doc.loading.is_none())
            });
            let complete_ms = start.elapsed().as_secs_f64() * 1000.0;
            let app = harness.state.get();
            let doc = app.workspace.active_diff().unwrap();
            assert_eq!((doc.additions(), doc.deletions()), (300000, 300000));
            let sample = serde_json::json!({"round":round,"route":if commit {"commit"}else{"sidebar"},"visible_ms":visible_ms,"complete_ms":complete_ms});
            println!("DIFF_FIRST_VISIBLE {sample}");
            samples.push(sample);
        }
    }
    std::fs::write(std::env::var_os("LOOM_DIFF_VISIBLE_OUTPUT").unwrap(),serde_json::to_vec_pretty(&serde_json::json!({"samples":samples,"files":1000,"additions":300000,"deletions":300000,"profile":if cfg!(debug_assertions){"debug"}else{"release"},"note":"Actual open dispatch through UI completion queue to native first frame containing selected code. Repository/commit file listings are already available, as in the visible sidebar/history. Excludes GPU presentation and OS mouse dispatch."})).unwrap()).unwrap();
}

#[test]
fn progressive_diff_unreadable_selected_file_does_not_block_other_files() {
    let (root, backend) = crate::infrastructure::git::backend::tests::required_repository();
    let root = std::fs::canonicalize(root).expect("canonicalize Git fixture path");
    std::fs::write(root.join("b.txt"), "before\n").unwrap();
    backend.stage(&root, &["b.txt".into()]).unwrap();
    backend.commit(&root, "base", false, false).unwrap();
    std::fs::write(root.join("b.txt"), "after\n").unwrap();
    std::fs::write(root.join("a.txt"), "untracked\n").unwrap();
    let service = crate::infrastructure::git::service().unwrap();
    let snapshot = service.refresh(std::slice::from_ref(&root), None).unwrap();
    let root = snapshot
        .repository_for_path(&root)
        .expect("canonicalized Git fixture must be in the snapshot")
        .worktree_root
        .clone();
    std::fs::remove_file(root.join("a.txt")).unwrap();
    let context = ApplicationContext::empty(Default::default());
    let _text = lgui::backend::install_text_environment(
        &context,
        lgui::render_skia::skia_text_system_handle(),
    );
    let mut harness = Harness::new(root.clone(), None);
    let selected = DiffRequest {
        repository_root: root,
        path: "a.txt".into(),
        target: DiffTarget::IndexToWorktree,
    };
    open_changes(&harness.state, selected.clone(), true, snapshot);
    harness.wait(|app| app.workspace.active_diff().is_some());
    let app = harness.state.get();
    let sections = app
        .workspace
        .active_diff()
        .unwrap()
        .sections
        .as_ref()
        .unwrap();
    let failed = sections.iter().find(|s| s.request() == selected).unwrap();
    assert!(failed.error.is_some() && !failed.loading);
    assert_eq!(failed.group, ChangeGroup::Untracked);
    harness.wait(|app| {
        app.workspace
            .active_diff()
            .is_some_and(|doc| doc.loading.is_none())
    });
    let app = harness.state.get();
    assert_eq!(app.workspace.active_diff().unwrap().changed_file_count(), 2);
    assert!(
        app.workspace
            .active_diff()
            .unwrap()
            .sections
            .as_ref()
            .unwrap()
            .iter()
            .any(|s| s.document.path == std::path::Path::new("b.txt")
                && s.error.is_none()
                && !s.document.rows.is_empty())
    );
}
