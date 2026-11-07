//! Availability, stale events and deliberate action execution through the editor.
use super::*;
use crate::app::language_ui::lightbulb;
use crate::features::keymap::action::Action;
use serde_json::{Value, json};

struct Files(std::path::PathBuf);
impl Drop for Files {
    fn drop(&mut self) {
        if std::thread::panicking()
            && let Ok(log) = std::fs::read_to_string(self.0.join("log.jsonl"))
        {
            eprintln!("Fake LSP traffic for failed UI fixture:\n{log}");
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Case {
    h: LanguageHarness,
    files: Files,
}
impl Case {
    fn new(extra: &[&str]) -> Self {
        let root = std::env::temp_dir().join(format!(
            "loog-bulb-{}-{}",
            std::process::id(),
            LANGUAGE_CASES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let log = root.join("log.jsonl");
        let mut args = vec![
            "--features",
            "codeAction,execute",
            "--log",
            log.to_str().unwrap(),
        ];
        args.extend_from_slice(extra);
        let mut h = LanguageHarness::new("fn alpha() {}\n", &args);
        h.until_ready();
        if extra.contains(&"--diagnostics") && !extra.contains(&"--diag-defer-until-save") {
            // Pointer hover only starts a diagnostic card for known diagnostics.
            // Ready can be published before the initial findings arrive.
            h.until("initial diagnostics", |h| {
                let app = h.state.get();
                let id = app.workspace.active_document().unwrap();
                let doc = &app.workspace.documents[&id];
                h.runtime
                    .language_services()
                    .unwrap()
                    .results(&doc.meta.path, &doc.stamp(id))
                    .diagnostics
                    .is_some_and(|found| !found.is_empty())
            });
            // Findings may have arrived after the last render in until().
            h.render();
        }
        Self {
            h,
            files: Files(root),
        }
    }
    fn wait_diagnostics(&mut self, range: std::ops::Range<usize>) {
        self.h.until("diagnostics for the current text", |h| {
            let app = h.state.get();
            let id = app.workspace.active_document().unwrap();
            let doc = &app.workspace.documents[&id];
            h.runtime
                .language_services()
                .unwrap()
                .results(&doc.meta.path, &doc.stamp(id))
                .diagnostics
                .is_some_and(|found| found.iter().any(|d| d.range == range))
        });
        // until() can observe a publication that arrived after its last render.
        self.h.render();
    }
    fn requests(&self) -> Vec<Value> {
        std::fs::read_to_string(self.files.0.join("log.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|v| v["method"] == "textDocument/codeAction")
            .collect()
    }
    fn wait_bulb(&mut self) -> u64 {
        self.h.until("rendered code action bulb", |h| {
            revision(h).is_some()
                && h.session.tree().nodes().iter().any(|node| {
                    node.semantics.as_ref().and_then(|s| s.name.as_deref())
                        == Some("Code actions (Ctrl+.)")
                })
        });
        revision(&self.h).unwrap()
    }
}
fn revision(h: &LanguageHarness) -> Option<u64> {
    let app = h.state.get();
    lightbulb::for_view(&h.state, &app, app.workspace.active_pane())
}
fn click(h: &mut LanguageHarness, x: f32, y: f32) {
    use lgui::core::{InputEvent, Point, PointerButton, PointerData};
    let pointer = PointerData::mouse(Point::new(x, y));
    for input in [
        InputEvent::PointerDown {
            pointer,
            button: PointerButton::Left,
        },
        InputEvent::PointerUp {
            pointer,
            button: PointerButton::Left,
        },
    ] {
        h.send(input);
    }
}
fn open(h: &LanguageHarness, old: u64) -> bool {
    let mut opened = false;
    h.state.update(|app| {
        opened = lightbulb::open(&h.state, app, app.workspace.active_pane(), old);
    });
    opened
}

fn saved(h: &LanguageHarness) {
    let app = h.state.get();
    let path = app.workspace.active_path().unwrap().to_path_buf();
    let id = app.workspace.active_document().unwrap();
    let text = app.workspace.documents[&id].buffer.text_snapshot();
    drop(app);
    crate::app::test_support::language::saved(&h.state, path, text);
}

#[test]
fn code_action_lightbulb_refreshes_on_diagnostics_without_a_caret_or_text_change() {
    let mut c = Case::new(&["--diagnostics", "--diag-defer-until-save"]);
    c.h.set_cursor(1);
    let old = c.wait_bulb();
    assert!(
        c.requests().last().unwrap()["params"]["context"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    saved(&c.h);
    c.h.until("diagnostic publication", |h| {
        let app = h.state.get();
        let id = app.workspace.active_document().unwrap();
        let doc = &app.workspace.documents[&id];
        h.runtime
            .language_services()
            .unwrap()
            .results(&doc.meta.path, &doc.stamp(id))
            .diagnostics
            .is_some_and(|found| !found.is_empty())
    });
    assert!(!open(&c.h, old));
    assert_ne!(c.wait_bulb(), old);
    assert_eq!(
        c.requests().last().unwrap()["params"]["context"]["diagnostics"][0]["message"],
        "fake error"
    );
    assert_eq!(c.h.cursor(), 1);
    assert_eq!(c.h.document_text(), "fn alpha() {}\n");
}

#[test]
fn code_action_lightbulb_invalidates_same_id_registration_replacements_and_server_restarts() {
    let files = Files(std::env::temp_dir().join(format!(
        "loog-bulb-reg-{}-{}",
        std::process::id(),
        LANGUAGE_CASES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )));
    std::fs::create_dir_all(&files.0).unwrap();
    let script = files.0.join("script.json");
    std::fs::write(&script, json!([
        {"step":1,"id":8201,"method":"client/registerCapability","params":{"registrations":[{"id":"actions","method":"textDocument/codeAction","registerOptions":{"resolveProvider":true}}]}},
        {"step":2,"id":8202,"method":"client/registerCapability","params":{"registrations":[{"id":"actions","method":"textDocument/codeAction","registerOptions":{"resolveProvider":true,"documentSelector":[{"language":"plaintext"}]}}]}}
    ]).to_string()).unwrap();
    let mut c = Case::new(&["--registration-script", script.to_str().unwrap()]);
    for _ in [1, 2] {
        let old = c.wait_bulb();
        let revision =
            c.h.runtime
                .language_services()
                .unwrap()
                .capability_sources(None)
                .into_iter()
                .map(|i| i.revision)
                .max()
                .unwrap();
        saved(&c.h);
        c.h.until("committed dynamic registration", |h| {
            h.runtime
                .language_services()
                .unwrap()
                .capability_sources(None)
                .iter()
                .any(|i| i.revision > revision)
        });
        assert!(!open(&c.h, old));
    }
    let old = c.wait_bulb();
    let run =
        c.h.runtime
            .language_services()
            .unwrap()
            .capability_sources(None)[0]
            .run;
    c.h.runtime.extensions().unwrap().reload_all(&c.h.state);
    c.h.until("restarted server", |h| {
        h.runtime
            .language_services()
            .unwrap()
            .capability_sources(None)
            .iter()
            .any(|i| i.run > run && i.providers.code_action.is_some())
    });
    assert!(!open(&c.h, old));
    assert_ne!(c.wait_bulb(), old);
}

#[test]
fn code_action_lightbulb_click_reuses_sorted_actions_preserves_focus_and_undo() {
    let mut c = Case::new(&["--diagnostics"]);
    let (x, y) = c.h.point_at(0, 4);
    click(&mut c.h, x, y);
    c.h.set_cursor(1);
    c.wait_bulb();
    let requests = c.requests();
    assert_eq!(
        requests.last().unwrap()["params"]["context"]["triggerKind"],
        2
    );
    assert_eq!(
        requests.last().unwrap()["params"]["context"]["diagnostics"][0]["message"],
        "fake error"
    );
    assert!(c.h.state.get().editor.language.popup.is_none());
    assert_eq!(
        c.h.document_text(),
        "fn alpha() {}\n",
        "discovery never applies an edit"
    );
    let focused = c.h.session.runtime().interaction_state().focused.clone();
    assert!(focused.is_some());
    let rect = c
        .h
        .session
        .tree()
        .nodes()
        .iter()
        .find(|n| {
            n.semantics.as_ref().and_then(|s| s.name.as_deref()) == Some("Code actions (Ctrl+.)")
        })
        .unwrap()
        .paint_bounds;
    assert!(
        rect.right <= fonts::editor().gutter_w() - 10.0,
        "dedicated lane leaves line numbers and Git marks alone"
    );
    click(
        &mut c.h,
        (rect.left + rect.right) / 2.0,
        (rect.top + rect.bottom) / 2.0,
    );
    assert_eq!(c.h.cursor(), 1);
    assert_eq!(c.h.session.runtime().interaction_state().focused, focused);
    assert!(c.h.popup_labels()[0].contains("Fix it"));
    assert_eq!(
        c.requests().len(),
        requests.len(),
        "click uses the valid cached list"
    );
    c.h.act(Action::MenuConfirm);
    c.h.until("selected fix", |h| {
        h.document_text().starts_with("// fixed\n")
    });
    c.h.state.update(|app| {
        app.workspace.active_editor_mut().unwrap().undo();
    });
    assert_eq!(c.h.document_text(), "fn alpha() {}\n");
}

#[test]
fn diagnostic_card_quick_fix_uses_the_hovered_range_and_keeps_focus_and_cursor() {
    let mut c = Case::new(&["--diagnostics"]);
    let (x, y) = c.h.point_at(0, 9);
    click(&mut c.h, x, y);
    c.h.set_cursor(10);
    let (x, y) = c.h.point_at(0, 1);
    c.h.move_to(x, y);
    c.h.until("diagnostic quick fix button", |h| {
        h.session.tree().nodes().iter().any(|n| {
            n.semantics.as_ref().and_then(|s| s.name.as_deref()) == Some("Quick fix: Fix it")
        })
    });
    let request = c
        .requests()
        .into_iter()
        .find(|v| v["params"]["context"]["only"] == json!(["quickfix"]))
        .unwrap();
    assert_eq!(request["params"]["range"]["start"]["character"], 0);
    assert_eq!(request["params"]["range"]["end"]["character"], 2);
    assert_eq!(
        request["params"]["context"]["diagnostics"][0]["message"],
        "fake error"
    );
    let focus = c.h.session.runtime().interaction_state().focused.clone();
    let bounds = c
        .h
        .session
        .tree()
        .nodes()
        .iter()
        .find(|n| n.semantics.as_ref().and_then(|s| s.name.as_deref()) == Some("Quick fix: Fix it"))
        .unwrap()
        .paint_bounds;
    click(
        &mut c.h,
        (bounds.left + bounds.right) / 2.0,
        (bounds.top + bounds.bottom) / 2.0,
    );
    c.h.until("direct diagnostic fix", |h| {
        h.document_text().starts_with("// fixed\n")
    });
    assert_eq!(
        c.h.cursor(),
        19,
        "the edit maps the existing caret, rather than moving it to the diagnostic"
    );
    assert_eq!(c.h.session.runtime().interaction_state().focused, focus);
    c.h.state.update(|app| {
        app.workspace.active_editor_mut().unwrap().undo();
    });
    assert_eq!(c.h.document_text(), "fn alpha() {}\n");
}

#[test]
fn diagnostic_card_quick_fix_rejects_old_callbacks_and_expires_on_scroll_or_typing() {
    let mut c = Case::new(&["--diagnostics", "--diag-defer-until-save"]);
    saved(&c.h);
    c.wait_diagnostics(0..2);
    let (x, y) = c.h.point_at(0, 1);
    c.h.move_to(x, y);
    c.h.until("diagnostic actions", |h| {
        crate::app::language_ui::quickfix::view(&h.state, &h.state.get(), PaneId::new(1)).is_some()
    });
    let revision =
        crate::app::language_ui::quickfix::view(&c.h.state, &c.h.state.get(), PaneId::new(1))
            .unwrap()
            .revision;
    c.h.type_text("x");
    crate::app::language_ui::quickfix::choose(&c.h.state, PaneId::new(1), revision, 0, c.h.rect);
    assert!(!c.h.document_text().contains("fixed"));
    assert!(c.h.state.get().editor.editor_diagnostic_hover.is_none());
    // The old range follows the insertion until the server publishes again.
    c.wait_diagnostics(1..3);
    saved(&c.h);
    c.wait_diagnostics(0..2);
    c.h.move_to(400.0, 300.0);
    c.h.move_to(x, y);
    c.h.until("new diagnostic actions", |h| {
        crate::app::language_ui::quickfix::view(&h.state, &h.state.get(), PaneId::new(1)).is_some()
    });
    c.h.state
        .update(|app| app.workspace.set_scroll(PaneId::new(1), 0.0, 20.0));
    assert!(c.h.state.get().editor.editor_diagnostic_hover.is_none());
}

#[test]
fn diagnostic_card_documentation_links_are_rendered_without_launching_a_browser() {
    let mut c = Case::new(&[
        "--diagnostics",
        "--diag-code-url",
        "https://example.com/error/7",
    ]);
    let (x, y) = c.h.point_at(0, 1);
    c.h.move_to(x, y);
    c.h.until("diagnostic code link", |h| {
        h.session.tree().nodes().iter().any(|n| {
            n.semantics.as_ref().and_then(|s| s.name.as_deref())
                == Some("Open diagnostic documentation")
        })
    });
}

#[test]
fn save_resolves_lazy_source_actions_runs_their_commands_and_waits_for_edits_before_formatting() {
    let _budget =
        crate::app::language_ui::save::test_step_budget(std::time::Duration::from_secs(5));
    let files = Files(std::env::temp_dir().join(format!(
        "loog-save-lazy-{}-{}",
        std::process::id(),
        LANGUAGE_CASES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )));
    std::fs::create_dir_all(&files.0).unwrap();
    let actions = files.0.join("actions.json");
    let log = files.0.join("log.jsonl");
    std::fs::write(
        &actions,
        json!([{"title":"Lazy organize imports","kind":"source.organizeImports","data":7}])
            .to_string(),
    )
    .unwrap();
    let mut h = LanguageHarness::configured(
        "fn alpha() {}\n",
        &[
            "--features",
            "formatting,codeAction,execute",
            "--code-actions-file",
            actions.to_str().unwrap(),
            "--exec-edit",
            "--log",
            log.to_str().unwrap(),
        ],
        on_save(&["source.organizeImports"], true),
    );
    h.until_ready();
    h.act(Action::Save);
    let path = h.state.get().workspace.active_path().unwrap().to_path_buf();
    h.until("resolved and commanded save", |_| {
        std::fs::read_to_string(&path)
            .is_ok_and(|t| t == "// formatted\n// by server\nfn alpha() {}\n")
    });
    assert_eq!(
        h.document_text(),
        "// formatted\n// by server\nfn alpha() {}\n"
    );
    let methods = server_log(&h, &log);
    let resolve = methods
        .iter()
        .position(|s| s == "codeAction/resolve")
        .unwrap();
    let execute = methods
        .iter()
        .position(|s| s == "workspace/executeCommand")
        .unwrap();
    let format = methods
        .iter()
        .position(|s| s == "textDocument/formatting")
        .unwrap();
    assert!(resolve < execute && execute < format);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn save_finishes_the_requested_document_after_the_user_switches_tabs() {
    let mut h = LanguageHarness::configured(
        "fn alpha() {}\n",
        &["--features", "formatting", "--slow-ms", "200"],
        on_save(&[], true),
    );
    h.until_ready();
    let original = h.state.get().workspace.active_path().unwrap().to_path_buf();
    let next = original.with_extension("other.rs");
    h.act(Action::Save);
    h.state.update(|app| {
        app.workspace
            .open_path(next.clone(), "other unsaved text".into());
    });
    h.until("the originally requested file", |_| {
        std::fs::read_to_string(&original).is_ok_and(|t| t == "fn alpha() {}\n")
    });
    assert!(!next.exists());
    assert_eq!(h.state.get().workspace.active_path(), Some(next.as_path()));
    assert_eq!(h.document_text(), "other unsaved text");
    std::fs::remove_file(original).unwrap();
}

#[test]
fn save_actions_load_unopened_documents_before_formatting_and_leave_other_files_dirty() {
    let files = Files(std::env::temp_dir().join(format!(
        "loog-save-cross-file-{}-{}",
        std::process::id(),
        LANGUAGE_CASES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )));
    std::fs::create_dir_all(&files.0).unwrap();
    let actions = files.0.join("actions.json");
    let other = files.0.join("other.rs");
    std::fs::write(&other, "original\n").unwrap();
    std::fs::write(&actions, "[]").unwrap();
    let mut h = LanguageHarness::configured(
        "fn alpha() {}\n",
        &[
            "--features",
            "formatting,codeAction",
            "--code-actions-file",
            actions.to_str().unwrap(),
        ],
        on_save(&["source.organizeImports"], true),
    );
    h.until_ready();
    let original = h.state.get().workspace.active_path().unwrap().to_path_buf();
    let insert = |path: &std::path::Path, text: &str| json!({"textDocument":{"uri":crate::features::language_service::uri::file_uri(path).unwrap(),"version":null},"edits":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}},"newText":text}]});
    std::fs::write(&actions, json!([{"title":"Cross file imports","kind":"source.organizeImports","edit":{"documentChanges":[insert(&original,"// action\n"),insert(&other,"// imported\n")]}}]).to_string()).unwrap();
    h.act(Action::Save);
    h.until("cross file action before format and save", |_| {
        std::fs::read_to_string(&original)
            .is_ok_and(|t| t == "// formatted\n// action\nfn alpha() {}\n")
    });
    let app = h.state.get();
    let id = app.workspace.file_id_for_path(&other).unwrap();
    assert_eq!(
        app.workspace.documents[&id].buffer.text(),
        "// imported\noriginal\n"
    );
    assert_eq!(std::fs::read_to_string(&other).unwrap(), "original\n");
    assert_eq!(app.workspace.active_path(), Some(original.as_path()));
    drop(app);
    std::fs::remove_file(original).unwrap();
}

#[test]
fn code_action_lightbulb_rejects_old_clicks_after_cursor_selection_language_and_document_changes() {
    let mut c = Case::new(&[]);
    let old = c.wait_bulb();
    assert!(open(&c.h, old));
    c.h.set_cursor(2);
    assert!(c.h.state.get().editor.language.popup.is_none());
    assert!(!open(&c.h, old));
    assert!(revision(&c.h).is_none());
    let old = c.wait_bulb();
    c.h.state.update(|app| {
        app.workspace
            .active_editor_mut()
            .unwrap()
            .set_selection(loog_editor_core::text::Selection::spanning(0, 2))
    });
    assert!(!open(&c.h, old));
    let old = c.wait_bulb();
    c.h.state.update(|app| {
        let id = app.workspace.active_document().unwrap();
        app.workspace
            .documents
            .get_mut(&id)
            .unwrap()
            .meta
            .language_generation += 1;
    });
    assert!(!open(&c.h, old));
    let old = c.wait_bulb();
    c.h.type_text("x");
    assert!(!open(&c.h, old));
    let old = c.wait_bulb();
    c.h.state.update(|app| {
        app.workspace
            .open_path(c.files.0.join("other.rs"), "fn other() {}".into());
    });
    assert!(!open(&c.h, old));
}

#[test]
fn code_action_lightbulb_late_response_and_queued_timer_cannot_override_manual_requests() {
    let mut c = Case::new(&["--slow-ms", "200"]);
    let log = c.files.0.join("log.jsonl");
    c.h.until("background request", |_| {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .contains("textDocument/codeAction")
    });
    c.h.set_cursor(3);
    std::thread::sleep(std::time::Duration::from_millis(260));
    c.h.render();
    assert!(
        revision(&c.h).is_none(),
        "old cursor response cannot show a bulb"
    );
    // Queue the latest timer without executing it, then request explicitly.
    std::thread::sleep(std::time::Duration::from_millis(150));
    c.h.act(Action::ToggleCodeActions);
    c.h.until_popup("explicit action request");
    assert_eq!(
        c.requests().last().unwrap()["params"]["context"]["triggerKind"],
        1
    );
    assert_eq!(c.h.popup_labels().len(), 2);
}

#[test]
fn code_action_lightbulb_empty_disabled_and_failed_queries_stay_quiet() {
    let mut c = Case::new(&[]);
    let response = c.files.0.join("actions.json");
    for value in [
        json!([]),
        json!([{"title":"Unavailable fix", "disabled":{"reason":"Needs a selection"}}]),
    ] {
        std::fs::write(&response, value.to_string()).unwrap();
        let mut h = LanguageHarness::new(
            "fn alpha() {}\n",
            &[
                "--features",
                "codeAction",
                "--code-actions-file",
                response.to_str().unwrap(),
            ],
        );
        h.until_ready();
        // Allow debounce and the process response to complete.
        for _ in 0..25 {
            std::thread::sleep(std::time::Duration::from_millis(25));
            h.render();
        }
        assert!(revision(&h).is_none());
        assert!(h.state.get().editor.language.popup.is_none());
        if !value.as_array().unwrap().is_empty() {
            h.act(Action::ToggleCodeActions);
            h.until_popup("manual disabled action");
            h.until("disabled reason rendered", |h| {
                h.has_text("Needs a selection")
            });
            h.act(Action::MenuConfirm);
            assert_eq!(h.document_text(), "fn alpha() {}\n");
        }
    }
    c.wait_bulb();
    let mut failed = Case::new(&["--feature-error", "--do-not-retrigger"]);
    let log = failed.files.0.join("log.jsonl");
    failed.h.until("failed discovery request", |_| {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .contains("textDocument/codeAction")
    });
    for _ in 0..10 {
        std::thread::sleep(std::time::Duration::from_millis(25));
        failed.h.render();
    }
    assert!(revision(&failed.h).is_none());
    assert!(failed.h.state.get().editor.language.popup.is_none());
}

#[test]
fn dragging_across_a_diagnostic_message_selects_it_and_leaving_drops_the_selection() {
    use lgui::core::{InputEvent, Point, PointerButton, PointerData};
    let mut c = Case::new(&["--diagnostics", "--diag-message", "first line\nsecond line"]);
    let (x, y) = c.h.point_at(0, 1);
    c.h.move_to(x, y);
    c.h.until("the diagnostic card", |h| h.has_text("second line"));
    let rect_of = |c: &Case, wanted: &str| {
        c.h.session
            .tree()
            .nodes()
            .iter()
            .find(|node| node.text.as_deref() == Some(wanted))
            .map(|node| node.layout_rect)
            .expect("the message line is drawn")
    };
    let (first, last) = (rect_of(&c, "first line"), rect_of(&c, "second line"));
    let middle = |r: lgui::prelude::UiRect| (r.top + r.bottom) / 2.0;
    let at = |x, y| PointerData::mouse(Point::new(x, y));
    c.h.move_to(first.left + 1.0, middle(first));
    c.h.send(InputEvent::PointerDown {
        pointer: at(first.left + 1.0, middle(first)),
        button: PointerButton::Left,
    });
    c.h.send(InputEvent::PointerMove(at(last.right - 1.0, middle(last))));
    c.h.send(InputEvent::PointerUp {
        pointer: at(last.right - 1.0, middle(last)),
        button: PointerButton::Left,
    });
    assert_eq!(
        c.h.state.get().editor.language.hover.selection.as_deref(),
        Some("first line\nsecond line")
    );
    assert!(c.h.has_text("second line"), "selecting keeps the card");
    // Moving off the card ends it, and the selection with it.
    c.h.move_to(790.0, 390.0);
    assert!(c.h.state.get().editor.language.hover.selection.is_none());
}
