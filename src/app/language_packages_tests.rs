//! Application-level negative coverage: background document work is not a
//! request to parse a language. Run in a fresh process to measure cold resources.
use super::{
    runtime::AppRuntime,
    state::AppState,
    syntax::Highlighter,
    test_support::{UiTasks, live},
};
use crate::{
    domain::workspace_session::{EditorLayout, SavedPaneNode},
    features::search::{Request, SearchSpec},
    infrastructure::{explorer_filesystem, language_packs, search_filesystem},
};
use std::{
    fs,
    process::Command,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

#[test]
fn directory_search_and_hidden_session_restore_do_not_load_languages() {
    const CHILD: &str = "LOOG_LANGUAGE_ACTIVITY_TEST";
    if let Some(root) = std::env::var_os(CHILD) {
        let root = std::path::PathBuf::from(root);
        language_packs::initialize();
        let source = language_packs::resources().current();
        assert_eq!(source.activity(), Default::default());
        let md = root.join("readme.md");
        let mut app = AppState::new();
        super::workspace_commands::hydrate_editor_layout(
            &mut app,
            Some(EditorLayout {
                root: SavedPaneNode::Pane {
                    items: vec![
                        md.clone().into(),
                        root.join("hidden.rs").into(),
                        root.join("hidden.css").into(),
                    ],
                    active: Some(0),
                },
                active_pane: 0,
            }),
        );
        let queue = UiTasks::default();
        let runtime = Arc::new(AppRuntime::default());
        let state = live(app).with_runtime(&runtime);
        let highlighter = Highlighter::new(state.clone(), queue.application());
        runtime.install_highlighter(highlighter.clone());
        let restore =
            super::workspace_restore::SessionRestore::new(state.clone(), queue.application());
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            queue.run_all();
            if state.get().workspace.loading_files().is_empty() {
                break;
            }
            assert!(Instant::now() < deadline, "restore did not finish");
            std::thread::sleep(Duration::from_millis(2));
        }
        let model = state.get();
        assert_eq!(
            model
                .workspace
                .view_meta(model.workspace.active().unwrap())
                .unwrap()
                .local_path(),
            Some(md.as_path())
        );
        drop(model);
        assert_eq!(explorer_filesystem::read_directory(&root).len(), 3);
        let mut final_results = None;
        search_filesystem::run(
            Request {
                roots: vec![root],
                buffers: vec![],
                spec: SearchSpec {
                    query: "needle".into(),
                    include: String::new(),
                    exclude: String::new(),
                    case_sensitive: false,
                    whole_word: false,
                    regex: false,
                    include_hidden: false,
                    include_ignored: false,
                },
            },
            &AtomicBool::new(false),
            |results| final_results = Some(results),
        )
        .unwrap();
        let results = final_results.unwrap();
        assert!(results.complete);
        assert_eq!(results.matches, 2);
        assert_eq!(
            source.activity(),
            Default::default(),
            "session contents, directory entries and search results do not request syntax"
        );
        restore.shutdown();
        highlighter.shutdown();
        return;
    }
    let root = std::env::temp_dir().join(format!("loog-language-activity-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("readme.md"), "# Active core document").unwrap();
    fs::write(root.join("hidden.rs"), "fn needle() {}").unwrap();
    fs::write(root.join("hidden.css"), ".needle {}").unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "app::language_packages_tests::directory_search_and_hidden_session_restore_do_not_load_languages", "--nocapture"])
        .env(CHILD, &root).current_dir(&root).output().unwrap();
    assert_eq!(root.parent(), Some(std::env::temp_dir().as_path()));
    let _ = fs::remove_dir_all(&root);
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
