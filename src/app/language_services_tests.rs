use super::*;
use crate::features::language_service::diagnostics::{Diagnostic, Severity};

fn closed(line: u32, severity: Severity) -> crate::features::problems::Finding {
    use crate::features::language_service::reply::Span;
    use crate::features::providers::PositionEncoding;
    use crate::features::providers::position::Position;
    crate::features::problems::Finding {
        severity,
        message: "m".into(),
        source: None,
        code: None,
        span: Span {
            start: Position { line, character: 0 },
            end: Position { line, character: 1 },
            encoding: PositionEncoding::Utf16,
        },
    }
}

#[test]
fn files_that_are_not_open_keep_their_findings_per_source_and_yield_to_open_ones() {
    let services = LanguageServices::new();
    let a = PathBuf::from("/w/a.rs");
    let b = PathBuf::from("/w/b.rs");
    services.publish_findings(1, a.clone(), vec![closed(1, Severity::Error)]);
    services.publish_findings(2, a.clone(), vec![closed(5, Severity::Warning)]);
    services.publish_findings(1, b.clone(), vec![closed(0, Severity::Error)]);
    let none = std::collections::BTreeSet::new();
    let shown = services.closed_problems(&none);
    // Two sources about one file are one file with both findings.
    assert_eq!(shown.len(), 2);
    assert_eq!(shown.iter().find(|f| f.path == a).unwrap().items.len(), 2);
    let all = services.closed_counts(&none);
    assert_eq!((all.errors, all.warnings, all.infos), (2, 1, 0));

    // A file that is open shows its document's findings instead.
    let open = std::collections::BTreeSet::from([crate::features::problems::file_key(&a)]);
    assert_eq!(services.closed_problems(&open).len(), 1);
    assert_eq!(services.closed_counts(&open).errors, 1);

    // Publishing none removes the file; clearing a source removes all of its files.
    services.publish_findings(2, a.clone(), vec![]);
    services.clear_findings(1);
    assert!(services.closed_problems(&none).is_empty());
}

#[test]
fn clearing_an_external_source_takes_its_closed_findings_too() {
    let services = LanguageServices::new();
    services.publish_findings(
        7,
        PathBuf::from("/w/a.rs"),
        vec![closed(1, Severity::Error)],
    );
    services.clear_external(7);
    assert!(services.closed_problems(&Default::default()).is_empty());
}

fn finding(range: std::ops::Range<usize>) -> Diagnostic {
    Diagnostic {
        range,
        severity: Severity::Error,
        message: "finding".into(),
        source: None,
        code: None,
        documentation_url: None,
    }
}

#[test]
fn provider_diagnostics_follow_edit_metadata_and_results_expire_with_language_or_document_lifetimes()
 {
    let services = LanguageServices::new();
    let mut app = AppState::new();
    let path = std::env::temp_dir().join("loog-neutral-results.txt");
    let file = app.workspace.open_path(path.clone(), "a😀b".into());
    let stamp = app.workspace.documents[&file].stamp(file);
    services.publish_external(
        1,
        &path,
        stamp,
        app.workspace.documents[&file].buffer.text_snapshot(),
        vec![finding(1..5)].into(),
    );
    services.publish_highlights(
        2,
        path.clone(),
        stamp,
        6,
        vec![crate::features::syntax::TokenSpan {
            range: 1..5,
            class: Some(crate::features::highlighting::HighlightId::FUNCTION),
        }],
    );
    app.workspace.active_editor_mut().unwrap().set_cursor(0);
    app.workspace
        .active_editor_mut()
        .unwrap()
        .replace_selection("x", crate::model::text::EditKind::Typing)
        .unwrap();
    services.sync(&app);
    let current = app.workspace.documents[&file].stamp(file);
    let results = services.results(&path, &current);
    assert_eq!(results.diagnostics.unwrap()[0].range, 2..6);
    assert!(results.semantic.is_none());
    app.workspace
        .documents
        .get_mut(&file)
        .unwrap()
        .meta
        .language_generation += 1;
    services.sync(&app);
    assert!(
        services
            .results(&path, &app.workspace.documents[&file].stamp(file))
            .diagnostics
            .is_none()
    );
    let document = &app.workspace.documents[&file];
    services.publish_external(
        1,
        &path,
        document.stamp(file),
        document.buffer.text_snapshot(),
        vec![finding(0..1)].into(),
    );
    app.workspace.close(file);
    let replacement = app.workspace.open_path(path.clone(), "xa😀b".into());
    services.sync(&app);
    assert!(
        services
            .results(
                &path,
                &app.workspace.documents[&replacement].stamp(replacement)
            )
            .diagnostics
            .is_none()
    );
}

#[test]
fn opening_editing_saving_and_closing_reach_the_extension_without_host_servers() {
    use crate::app::test_support::{SavingUi, language};
    use std::time::{Duration, Instant};
    let directory =
        std::env::temp_dir().join(format!("loog-provider-bridge-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let log = directory.join("messages.jsonl");
    let path = directory.join("a.rs");
    let _ = std::fs::remove_file(&log);
    let ui = SavingUi::new(AppState::new());
    ui.runtime
        .install_language_services(LanguageServices::new());
    language::install(
        &ui.state,
        &ui.runtime,
        &[
            "--sync".into(),
            "object".into(),
            "--diagnostics".into(),
            "--semantic".into(),
            "full".into(),
            "--log".into(),
            log.to_string_lossy().into_owned(),
        ],
    );
    let messages = || {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .collect::<Vec<_>>()
    };
    let until = |condition: &dyn Fn() -> bool| {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            ui.tasks.run_all();
            if condition() {
                break;
            }
            assert!(Instant::now() < deadline, "messages: {:?}", messages());
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    ui.state.update(|app| {
        app.workspace.open_path(path.clone(), "fn a() {}".into());
    });
    until(&|| {
        messages()
            .iter()
            .any(|message| message["method"] == "textDocument/didOpen")
    });
    until(&|| {
        let app = ui.state.get();
        let file = app.workspace.active_document().unwrap();
        let found = ui
            .runtime
            .language_services()
            .unwrap()
            .results(&path, &app.workspace.documents[&file].stamp(file));
        found.diagnostics.is_some_and(|items| !items.is_empty())
            && found.semantic.is_some_and(|items| !items.is_empty())
    });
    ui.state.update(|app| {
        app.workspace
            .active_editor_mut()
            .unwrap()
            .replace_selection("x", crate::model::text::EditKind::Typing)
            .unwrap();
    });
    until(&|| {
        messages()
            .iter()
            .any(|message| message["method"] == "textDocument/didChange")
    });
    let document = {
        let app = ui.state.get();
        app.workspace.active_document().unwrap()
    };
    let text = ui.state.get().workspace.documents[&document]
        .buffer
        .text_snapshot();
    language::saved(&ui.state, path.clone(), text.clone());
    until(&|| {
        messages().iter().any(|message| {
            message["method"] == "textDocument/didSave"
                && message["params"]["text"] == text.as_str()
        })
    });
    ui.state.update(|app| {
        app.workspace.close(document);
    });
    until(&|| {
        messages()
            .iter()
            .any(|message| message["method"] == "textDocument/didClose")
    });
    ui.runtime.clear_extensions();
    let _ = std::fs::remove_file(&log);
}
