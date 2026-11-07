//! Signature overload selection through the real document bridge and layout tree.
use super::*;
use crate::features::keymap::{
    action::Action, dispatcher::dispatch_key, keymap_file::default_keymap, keystroke::Keystroke,
};
use serde_json::{Value, json};
use std::path::PathBuf;

struct Files(PathBuf);
impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Case {
    h: LanguageHarness,
    files: Files,
}
fn signatures() -> Value {
    json!({"signatures": [
        {"label":"fn f(a: i32, b: i32)", "documentation":"Integer overload", "parameters":[{"label":"a: i32"},{"label":"b: i32"}]},
        {"label":"fn f(😀: str, count: usize)", "documentation":{"kind":"markdown", "value":"Text overload"}, "activeParameter":0,
            "parameters":[{"label":[5,12], "documentation":"String input"},{"label":[14,26]}]}
    ], "activeSignature":0, "activeParameter":1})
}
impl Case {
    fn new(slow: bool) -> Self {
        let root = std::env::temp_dir().join(format!(
            "loog-signatures-{}-{}",
            std::process::id(),
            LANGUAGE_CASES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let response = root.join("response.json");
        let log = root.join("messages.jsonl");
        std::fs::write(&response, signatures().to_string()).unwrap();
        let mut h = LanguageHarness::new(
            "f(\n",
            &[
                "--features",
                "signature,completion",
                "--signature-file",
                response.to_str().unwrap(),
                "--log",
                log.to_str().unwrap(),
                "--slow-ms",
                if slow { "150" } else { "0" },
            ],
        );
        h.until_ready();
        h.set_cursor(2);
        h.act(Action::ShowSignatureHelp);
        h.until("two signatures", |h| position(h) == Some((1, 2)));
        Self {
            h,
            files: Files(root),
        }
    }
    fn requests(&self) -> Vec<Value> {
        std::fs::read_to_string(self.files.0.join("messages.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|value| value["method"] == "textDocument/signatureHelp")
            .collect()
    }
    fn replace(&self, value: Value) {
        std::fs::write(self.files.0.join("response.json"), value.to_string()).unwrap();
    }
    fn refresh(&mut self) {
        let pending = self.start_refresh();
        self.h.until("refreshed signature", |h| {
            h.state
                .get()
                .editor
                .language
                .signature
                .as_ref()
                .is_some_and(|s| s.revision > pending)
        });
        self.h.render();
    }
    fn start_refresh(&self) -> u64 {
        let mut pending = 0;
        self.h.state.update(|app| {
            crate::app::language_ui::signature::request(&self.h.state, app, None, true);
            pending = app.editor.language.signature.as_ref().unwrap().revision;
        });
        pending
    }
    fn wait_context(&mut self, kind: u64, character: Option<&str>) -> Value {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(request) = self.requests().last() {
                let context = &request["params"]["context"];
                if context["triggerKind"].as_u64() == Some(kind)
                    && context["triggerCharacter"].as_str() == character
                {
                    return context.clone();
                }
            }
            assert!(
                std::time::Instant::now() < until,
                "missing signature trigger {kind} {character:?}"
            );
            self.h.render();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
fn position(h: &LanguageHarness) -> Option<(usize, usize)> {
    let app = h.state.get();
    crate::app::language_ui::signature::view(app.editor.language.signature.as_ref()?)
        .and_then(|view| view.position)
}
fn key(h: &LanguageHarness, stroke: &str) -> Option<Action> {
    dispatch_key(
        &default_keymap(),
        Vec::new(),
        Keystroke::parse(stroke).unwrap(),
        &crate::app::keymap_context::context_stack(&h.state.get()),
    )
    .actions
    .first()
    .copied()
}

fn drawn_words(h: &LanguageHarness) -> String {
    h.session
        .tree()
        .nodes()
        .iter()
        .filter_map(|node| {
            node.text.as_deref().or_else(|| {
                node.semantics
                    .as_ref()
                    .and_then(|semantics| semantics.value.as_deref())
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn overload_keys_cycle_locally_and_keep_parameter_docs_caret_and_completion_navigation() {
    let mut c = Case::new(false);
    let before = c.requests().len();
    let text = c.h.document_text();
    let cursor = c.h.cursor();
    assert_eq!(key(&c.h, "alt-down"), Some(Action::NextSignature));
    assert_eq!(key(&c.h, "alt-up"), Some(Action::PreviousSignature));
    c.h.act(Action::PreviousSignature);
    assert_eq!(position(&c.h), Some((2, 2)));
    assert!(c.h.has_text("😀: str"));
    c.h.until("native signature documentation", |h| {
        drawn_words(h).contains("Text overload")
    });
    let drawn = drawn_words(&c.h);
    assert!(
        drawn.contains("String input") && drawn.contains("Text overload"),
        "{drawn}"
    );
    c.h.act(Action::NextSignature);
    assert_eq!(position(&c.h), Some((1, 2)));
    assert!(c.h.has_text("b: i32"));
    c.h.act(Action::NextSignature);
    assert_eq!(position(&c.h), Some((2, 2)));
    assert_eq!(
        c.requests().len(),
        before,
        "changing overloads does not issue requests"
    );
    assert_eq!(c.h.document_text(), text);
    assert_eq!(c.h.cursor(), cursor);
    c.h.act(Action::ShowCompletions);
    c.h.until("completion next to signature help", |h| {
        crate::app::language_ui::completion_visible(&h.state.get())
    });
    assert_eq!(key(&c.h, "up"), Some(Action::MenuSelectPrevious));
    assert_eq!(key(&c.h, "down"), Some(Action::MenuSelectNext));
    assert_eq!(key(&c.h, "alt-down"), Some(Action::NextSignature));
    c.h.type_text("a");
    c.h.state.update(|app| {
        app.editor.vim.options.enabled = true;
        crate::app::editor_vim::sync_view(app);
    });
    assert_eq!(key(&c.h, "alt-up"), Some(Action::PreviousSignature));
    c.h.state.update(crate::app::language_ui::signature::close);
    assert_ne!(key(&c.h, "alt-down"), Some(Action::NextSignature));
}

#[test]
fn overload_buttons_dispatch_without_moving_the_caret_or_stealing_focus() {
    use lgui::core::{InputEvent, Point, PointerButton, PointerData};
    let mut c = Case::new(false);
    let cursor = c.h.cursor();
    // Give the editor actual framework focus before clicking its floating card.
    let (x, y) = c.h.point_at(0, 1);
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
        c.h.send(input);
    }
    c.h.set_cursor(cursor);
    // The previous card remains visible while the new request is pending.
    // Wait for the response revision before using its button callbacks.
    c.refresh();
    assert_eq!(position(&c.h), Some((1, 2)));
    let focused = c.h.session.runtime().interaction_state().focused.clone();
    assert!(focused.is_some(), "the editor received framework focus");
    let before = c.requests().len();
    for (label, expected) in [("↓", (2, 2)), ("↑", (1, 2))] {
        let bounds =
            c.h.session
                .tree()
                .nodes()
                .iter()
                .find(|n| n.text.as_deref() == Some(label))
                .unwrap()
                .paint_bounds;
        let pointer = PointerData::mouse(Point::new(
            (bounds.left + bounds.right) / 2.0,
            (bounds.top + bounds.bottom) / 2.0,
        ));
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
            c.h.send(input);
        }
        assert_eq!(position(&c.h), Some(expected));
        assert_eq!(c.h.cursor(), cursor);
        assert!(c.h.state.get().editor.focused);
        assert_eq!(c.h.session.runtime().interaction_state().focused, focused);
    }
    assert_eq!(c.requests().len(), before);
    c.h.type_text("a");
    assert_eq!(
        c.h.document_text(),
        "f(a\n",
        "typing still reaches the editor"
    );
}

#[test]
fn retriggers_echo_the_selected_wire_signature_and_preserve_selection_across_reordering() {
    let mut c = Case::new(false);
    c.h.act(Action::NextSignature);
    c.refresh();
    assert_eq!(position(&c.h), Some((2, 2)));
    let manual = c.requests().pop().unwrap();
    assert_eq!(manual["params"]["context"]["triggerKind"], 1);
    assert_eq!(manual["params"]["context"]["isRetrigger"], true);
    let active = &manual["params"]["context"]["activeSignatureHelp"];
    assert_eq!(active["activeSignature"], 1);
    assert_eq!(
        active["signatures"][1]["parameters"][0]["label"],
        json!([5, 12]),
        "wire offsets stay UTF-16"
    );
    c.h.type_text(",");
    c.h.until("comma retrigger", |h| {
        h.has_text("2/2")
            && h.state
                .get()
                .editor
                .language
                .signature
                .as_ref()
                .unwrap()
                .revision
                > 0
    });
    let comma = c.wait_context(2, Some(","));
    assert_eq!(comma["activeSignatureHelp"]["activeSignature"], 1);
    assert_eq!(
        c.h.state
            .get()
            .editor
            .language
            .signature
            .as_ref()
            .unwrap()
            .opened_at,
        1,
        "comma keeps the call anchor"
    );
    let mut reordered = signatures();
    reordered["signatures"].as_array_mut().unwrap().reverse();
    c.replace(reordered);
    c.refresh();
    assert_eq!(position(&c.h), Some((1, 2)));
    c.h.until("native signature documentation", |h| {
        drawn_words(h).contains("Text overload")
    });
    assert!(drawn_words(&c.h).contains("Text overload"));
    let mut removed = signatures();
    removed["signatures"].as_array_mut().unwrap().truncate(1);
    c.replace(removed);
    c.refresh();
    assert_eq!(position(&c.h), None);
    c.h.until("native integer signature documentation", |h| {
        drawn_words(h).contains("Integer overload")
    });
    assert!(drawn_words(&c.h).contains("Integer overload"));
    assert_ne!(key(&c.h, "alt-down"), Some(Action::NextSignature));
    c.h.type_text(")");
    assert!(c.h.state.get().editor.language.signature.is_none());
}

#[test]
fn content_changes_keep_the_chosen_overload_and_a_nested_call_starts_fresh() {
    let mut c = Case::new(false);
    c.h.act(Action::NextSignature);
    c.h.type_text("a");
    let content = c.wait_context(3, None);
    assert_eq!(content["isRetrigger"], true);
    assert_eq!(content["activeSignatureHelp"]["activeSignature"], 1);
    c.h.type_text("(");
    let nested = c.wait_context(2, Some("("));
    assert_eq!(nested["isRetrigger"], false);
    assert!(nested.get("activeSignatureHelp").is_none());
    c.h.until("fresh nested help", |h| position(h) == Some((1, 2)));
    assert_eq!(
        c.h.state
            .get()
            .editor
            .language
            .signature
            .as_ref()
            .unwrap()
            .opened_at,
        3
    );
    c.h.type_text(")");
    assert!(c.h.state.get().editor.language.signature.is_none());
}

#[test]
fn an_in_flight_refresh_keeps_a_new_selection_and_old_card_events_are_ignored() {
    let mut c = Case::new(true);
    let old =
        c.h.state
            .get()
            .editor
            .language
            .signature
            .as_ref()
            .unwrap()
            .revision;
    let pending = c.start_refresh();
    c.h.state.update(|app| {
        assert!(!crate::app::language_ui::signature::step(
            app,
            true,
            Some(old)
        ))
    });
    c.h.act(Action::NextSignature);
    c.h.until("refresh after changing selection", |h| {
        h.state
            .get()
            .editor
            .language
            .signature
            .as_ref()
            .unwrap()
            .revision
            > pending
    });
    assert_eq!(position(&c.h), Some((2, 2)));
    c.h.act(Action::ShowSignatureHelp);
    c.h.state.update(crate::app::language_ui::signature::close);
    std::thread::sleep(std::time::Duration::from_millis(300));
    c.h.render();
    assert!(
        c.h.state.get().editor.language.signature.is_none(),
        "late response cannot reopen dismissed help"
    );
}

#[test]
fn a_posted_trigger_cannot_reopen_dismissed_help_or_move_to_another_document_or_language() {
    let mut c = Case::new(false);
    c.h.type_text(",");
    // Let the timer post its callback without pumping the application's queue.
    std::thread::sleep(std::time::Duration::from_millis(50));
    c.h.state.update(crate::app::language_ui::signature::close);
    let before = c.requests().len();
    c.h.render();
    assert!(c.h.state.get().editor.language.signature.is_none());
    assert_eq!(c.requests().len(), before);
    c.h.type_text("(");
    std::thread::sleep(std::time::Duration::from_millis(50));
    let other = c.files.0.join("other.rs");
    c.h.state.update(|app| {
        app.workspace.open_path(other, "other()".into());
    });
    c.h.render();
    assert!(c.h.state.get().editor.language.signature.is_none());
    let changed_language = Case::new(false);
    let old = changed_language
        .h
        .state
        .get()
        .editor
        .language
        .signature
        .as_ref()
        .unwrap()
        .revision;
    changed_language.h.state.update(|app| {
        let id = app.workspace.active_document().unwrap();
        app.workspace
            .documents
            .get_mut(&id)
            .unwrap()
            .meta
            .language_generation += 1;
        assert!(!crate::app::language_ui::signature::step(
            app,
            true,
            Some(old)
        ));
    });
    assert!(
        changed_language
            .h
            .state
            .get()
            .editor
            .language
            .signature
            .is_none()
    );
}

#[test]
fn signature_controls_stay_inside_narrow_cards_and_do_not_overlap_the_label() {
    let c = Case::new(false);
    for width in [128.0, 260.0, 800.0] {
        let area = UiRect::new(40.0, 20.0, 40.0 + width, 180.0);
        let (_, mut view) =
            crate::app::language_ui::signature_view(&c.h.state.get(), PaneId::new(1)).unwrap();
        view.label = view.label.repeat(30);
        view.docs = Some(
            crate::features::markdown::content::Markup::markdown(
                "Long documentation.\n\n".repeat(30),
            )
            .into(),
        );
        let anchor = UiRect::new(
            area.right - 10.0,
            area.top + 20.0,
            area.right,
            area.top + 38.0,
        );
        let (card, _) = crate::ui::editor::signature::render(
            &view,
            anchor,
            area,
            Arc::new(|_| {}),
            crate::ui::editor::docs::Actions::ignored(),
        );
        assert!(
            card.left >= area.left
                && card.right <= area.right
                && card.top >= area.top
                && card.bottom <= area.bottom
        );
        let ui: lgui::application::AppView = Arc::new(move |_| {
            crate::ui::editor::signature::render(
                &view,
                anchor,
                area,
                Arc::new(|_| {}),
                crate::ui::editor::docs::Actions::ignored(),
            )
            .1
        });
        let mut session = lgui::session::UiSession::new();
        session.render_view(&ui, area, lgui::core::UiScale::ONE);
        let nodes = session.tree().nodes();
        for label in ["↑", "↓", "1/2"] {
            let bounds = nodes
                .iter()
                .find(|n| n.text.as_deref() == Some(label))
                .unwrap()
                .paint_bounds;
            assert!(
                bounds.left >= card.left
                    && bounds.right <= card.right
                    && bounds.top >= card.top
                    && bounds.bottom <= card.bottom,
                "{width}: {bounds:?}"
            );
            let signature = nodes
                .iter()
                .find(|n| n.text.as_deref() == Some("b: i32"))
                .unwrap()
                .paint_bounds;
            assert!(
                bounds.bottom <= signature.top,
                "controls have a separate row"
            );
        }
    }
}
