//! Stage 0 compatibility fixtures and opt-in measurements of existing paths.
//! No application behaviour or architecture is changed by this module.

use std::hint::black_box;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::app::workspace_session::{SavedPaneNode, WorkspaceSession};
use crate::features::keymap::action::Action;
use crate::features::keymap::bindings::BindingSource;
use crate::features::terminal::ShellKind;
use crate::infrastructure::settings_filesystem::{self as settings_persistence, Scope};

#[test]
fn phase0_legacy_session_defaults_fixture() {
    let session: WorkspaceSession =
        serde_json::from_str(include_str!("../../tests/fixtures/session-minimal.json")).unwrap();
    assert_eq!(session.open_folders, [PathBuf::from("workspace")]);
    assert!(session.editor_layout.is_none());
    assert!(session.terminal_groups.is_empty());
    assert!(session.terminal_directories.is_empty());
    assert_eq!(
        serde_json::from_str::<WorkspaceSession>("{}").unwrap(),
        WorkspaceSession::default()
    );
}

#[test]
fn phase0_split_session_fixture_preserves_order_and_terminal_metadata() {
    let session: WorkspaceSession =
        serde_json::from_str(include_str!("../../tests/fixtures/session-split.json")).unwrap();
    let layout = session.editor_layout.as_ref().unwrap();
    assert_eq!(layout.active_pane, 1);
    let SavedPaneNode::Split { children, .. } = &layout.root else {
        panic!("fixture must contain two panes");
    };
    assert_eq!(children.len(), 2);
    let SavedPaneNode::Pane { items, active } = &children[0].node else {
        panic!("first child must be a pane");
    };
    assert_eq!(
        items
            .iter()
            .map(|v| v.path().to_path_buf())
            .collect::<Vec<_>>(),
        &[
            PathBuf::from("workspace/main.rs"),
            PathBuf::from("workspace/中文.txt")
        ]
    );
    assert_eq!(*active, Some(1));
    assert_eq!(
        session.terminal_tabs,
        [ShellKind::PowerShell, ShellKind::Cmd, ShellKind::Bash]
    );
    assert_eq!(session.terminal_groups, [0, 0, 1]);
    assert_eq!(session.terminal_widths, [1.5, 0.5, 1.0]);
    assert_eq!(session.terminal_directories[1], None);
    assert_eq!(session.terminal_purposes[2].as_deref(), Some("Development"));
    let encoded = serde_json::to_string(&session).unwrap();
    assert_eq!(
        serde_json::from_str::<WorkspaceSession>(&encoded).unwrap(),
        session
    );
}

#[test]
fn phase0_settings_jsonc_fixture_preserves_valid_values_and_scope() {
    let source = include_str!("../../tests/fixtures/settings.jsonc");
    let loaded = settings_persistence::parse(source).unwrap();
    assert_eq!(loaded.settings.editor_font.size, 16);
    assert_eq!(loaded.settings.editor_font.family, "Fixture Mono");
    assert!(loaded.settings.vim.enabled);
    assert_eq!(loaded.settings.default_shell, ShellKind::Bash);
    assert_eq!(
        loaded.errors.len(),
        1,
        "only the invalid boolean is rejected"
    );
    assert!(loaded.settings.smooth_caret);
    let (project, errors) = settings_persistence::parse_overrides(source, Scope::Project).unwrap();
    assert_eq!(
        project.len(),
        1,
        "only the existing project whitelist applies"
    );
    assert_eq!(project["default_shell"], "bash");
    assert_eq!(errors.len(), 3);
}

#[test]
fn phase0_keymap_jsonc_fixture_preserves_actions_and_partial_errors() {
    let loaded = crate::features::keymap::keymap_file::parse(
        include_str!("../../tests/fixtures/keymap.jsonc"),
        BindingSource::User,
    )
    .unwrap();
    assert_eq!(loaded.errors.len(), 1);
    assert_eq!(loaded.bindings.len(), 4);
    assert!(
        loaded
            .bindings
            .iter()
            .any(|binding| binding.action == Action::Save)
    );
    assert!(
        loaded
            .bindings
            .iter()
            .any(|binding| binding.action == Action::NoAction)
    );
    assert!(
        loaded
            .bindings
            .iter()
            .any(|binding| binding.action == Action::ActivateItem(0))
    );
    assert!(
        loaded
            .bindings
            .iter()
            .any(|binding| matches!(binding.action, Action::VimKeys(_)))
    );
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "loog-phase0-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Delete only the uniquely named direct child we created in the temp dir.
        if self.0.parent() == Some(std::env::temp_dir().as_path())
            && self
                .0
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("loog-phase0-")
        {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

fn emit(scenario: &str, metric: &str, elapsed: Duration, bytes: usize, operations: usize) {
    println!(
        "LOOG_BASELINE {}",
        serde_json::json!({
            "schema": 1,
            "scenario": scenario,
            "metric": metric,
            "value": elapsed.as_secs_f64() * 1000.0,
            "unit": "ms",
            "bytes": bytes,
            "operations": operations
        })
    );
}

fn ascii_text(bytes: usize) -> String {
    let line = "let value = compute(alpha, beta, gamma); // baseline text\n";
    let mut text = line.repeat(bytes / line.len());
    text.push_str(&line[..bytes % line.len()]);
    text
}

#[test]
#[ignore = "opt-in stage 0 cached file read and index preparation baseline"]
fn phase0_perf_file_preparation() {
    use crate::app::workspace::PreparedTextFile;
    let scratch = Scratch::new("files");
    for mib in [1, 10, 64] {
        let bytes = mib * 1024 * 1024;
        let path = scratch.0.join(format!("{mib}m.txt"));
        std::fs::write(&path, ascii_text(bytes)).unwrap();
        // Warm the OS file cache; creation and warmup are outside the measurement.
        let warm = crate::app::workspace_commands::read_text_file(&path).unwrap();
        assert_eq!(warm.len(), bytes);
        drop(warm);
        let started = Instant::now();
        let text = crate::app::workspace_commands::read_text_file(&path).unwrap();
        let read = started.elapsed();
        assert_eq!(text.len(), bytes);
        emit(&format!("ascii-{mib}m"), "file_read_cached", read, bytes, 1);
        let started = Instant::now();
        let prepared = PreparedTextFile::new(path, text);
        let prepare = started.elapsed();
        black_box(&prepared);
        emit(
            &format!("ascii-{mib}m"),
            "document_prepare",
            prepare,
            bytes,
            1,
        );
    }
}

fn search_spec() -> crate::features::search::SearchSpec {
    crate::features::search::SearchSpec {
        query: "needle".into(),
        include: "*.txt".into(),
        exclude: String::new(),
        case_sensitive: true,
        whole_word: false,
        regex: false,
        include_hidden: false,
        include_ignored: false,
    }
}

#[test]
#[ignore = "opt-in stage 0 search scan and active cancellation baseline"]
fn phase0_perf_search_scan_and_cancel() {
    use crate::features::search::{Request, Results};
    let scratch = Scratch::new("search");
    let bytes_per_file = 64 * 1024;
    let text = "needle\n".to_owned() + &ascii_text(bytes_per_file - "needle\n".len());
    for index in 0..100 {
        std::fs::write(scratch.0.join(format!("file-{index}.txt")), &text).unwrap();
    }
    let request = || Request {
        spec: search_spec(),
        roots: vec![scratch.0.clone()],
        buffers: Vec::new(),
    };
    let mut results = Results::default();
    // Warm regex, traversal and cached reads before the timed scan.
    crate::infrastructure::search_filesystem::run(request(), &AtomicBool::new(false), |_| {})
        .unwrap();
    let started = Instant::now();
    crate::infrastructure::search_filesystem::run(request(), &AtomicBool::new(false), |next| {
        results = next
    })
    .unwrap();
    let scan = started.elapsed();
    assert_eq!(results.matches, 100);
    assert_eq!(results.searched, 100);
    assert_eq!(results.errors, 0);
    emit(
        "100-files-6.25m",
        "search_scan_cached",
        scan,
        bytes_per_file * 100,
        100,
    );

    let cancel = AtomicBool::new(false);
    let requested = std::sync::Mutex::new(None::<Instant>);
    let mut cancelled = Results::default();
    let ended = std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(2));
            *requested.lock().unwrap() = Some(Instant::now());
            cancel.store(true, Ordering::Release);
        });
        crate::infrastructure::search_filesystem::run(request(), &cancel, |next| cancelled = next)
            .unwrap();
        Instant::now()
    });
    assert!(cancelled.complete);
    let requested = requested.lock().unwrap().unwrap();
    if cancelled.cancelled {
        emit(
            "100-files-6.25m",
            "search_cancel_latency",
            ended.saturating_duration_since(requested),
            bytes_per_file * 100,
            1,
        );
    } else {
        // A completed scan is not evidence of observed active cancellation.
        println!("LOOG_BASELINE_STATUS cancellation_not_observed scan_completed_before_request");
    }
}

#[test]
#[ignore = "opt-in stage 0 ANSI and Unicode terminal parser throughput baseline"]
fn phase0_perf_terminal_parser() {
    let chunk = "\u{1b}[31m中文 abc\u{1b}[0m\r\n";
    let text = chunk.repeat(8 * 1024 * 1024 / chunk.len());
    let mut parser = vt100::Parser::new(24, 80, 1000);
    parser.process(chunk.as_bytes());
    let started = Instant::now();
    parser.process(text.as_bytes());
    let elapsed = started.elapsed();
    assert!(parser.screen().contents().contains("abc"));
    emit("ansi-unicode-8m", "terminal_parse", elapsed, text.len(), 1);
    let started = Instant::now();
    for _ in 0..100 {
        black_box(parser.screen().contents());
    }
    emit(
        "24x80-screen",
        "terminal_screen_snapshot_100",
        started.elapsed(),
        text.len(),
        100,
    );
}
