//! Choice snippets received from an LSP server, through the editor and window tree.
use super::*;
use crate::app::language_ui::snippet_session as snippets;
use crate::features::keymap::{
    action::Action, dispatcher::dispatch_key, keymap_file::default_keymap, keystroke::Keystroke,
};

struct SnippetFile(std::path::PathBuf);
impl Drop for SnippetFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
struct ChoiceHarness {
    inner: LanguageHarness,
    _file: SnippetFile,
}
impl std::ops::Deref for ChoiceHarness {
    type Target = LanguageHarness;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
impl std::ops::DerefMut for ChoiceHarness {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

fn received_snippet(source: &str, accept_on: &str) -> ChoiceHarness {
    let file = SnippetFile(std::env::temp_dir().join(format!(
        "loom-choice-{}-{}.txt",
        std::process::id(),
        LANGUAGE_CASES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )));
    std::fs::write(&file.0, source).unwrap();
    let mut h = LanguageHarness::configured(
        "\n",
        &[
            "--features",
            "completion",
            "--completion-snippet-file",
            file.0.to_str().unwrap(),
        ],
        |app| {
            app.preferences
                .values_mut()
                .language_features
                .completion
                .accept_on = accept_on.into();
            app.preferences
                .values_mut()
                .language_features
                .completion
                .words_fallback = false;
        },
    );
    h.until_ready();
    h.type_text("be");
    h.until("the snippet completion", |h| {
        h.completion_labels() == ["beta"]
    });
    h.act(if accept_on == "tab" {
        Action::MenuConfirmTab
    } else {
        Action::MenuConfirm
    });
    h.until("received snippet session", |h| {
        h.state.get().editor.language.snippet.is_some()
    });
    ChoiceHarness {
        inner: h,
        _file: file,
    }
}

fn choice_snippet(source: &str, accept_on: &str) -> ChoiceHarness {
    let mut h = received_snippet(source, accept_on);
    h.until("snippet choices", |h| {
        snippets::choice_visible(&h.state.get())
    });
    h
}

#[test]
fn server_snippet_transforms_commit_on_tab_and_undo_redo_as_one_edit() {
    let mut h = choice_snippet(
        "${1|red,blue|}:$1:${1/(.*)/${1:/upcase}/}:${2:tail}$0",
        "enter_and_tab",
    );
    h.act(Action::MenuSelectNext);
    h.act(Action::MenuConfirm);
    assert_eq!(h.document_text(), "blue:blue:blue:tail\n");
    h.act(Action::NextSnippetTabstop);
    assert_eq!(h.document_text(), "blue:blue:BLUE:tail\n");
    let range = h.selection().unwrap();
    assert_eq!(&h.document_text()[range], "tail");
    crate::app::editor::execute_command(
        &h.state,
        Command::Undo,
        h.rect,
        Arc::new(TestClipboard::default()),
    );
    h.render();
    assert_eq!(h.document_text(), "blue:blue:blue:tail\n");
    assert!(h.state.get().editor.language.snippet.is_none());
    crate::app::editor::execute_command(
        &h.state,
        Command::Redo,
        h.rect,
        Arc::new(TestClipboard::default()),
    );
    h.render();
    assert_eq!(h.document_text(), "blue:blue:BLUE:tail\n");
}

#[test]
fn server_unknown_variables_are_editable_mirrors_and_known_unset_variables_use_defaults() {
    let mut h = received_snippet(
        "${1:hello}:$MISSING/$MISSING:${CLIPBOARD:fallback}$0",
        "enter_and_tab",
    );
    h.type_text("hi");
    h.act(Action::NextSnippetTabstop);
    h.type_text("名字");
    assert_eq!(h.document_text(), "hi:名字/名字:fallback\n");
    h.act(Action::NextSnippetTabstop);
    assert!(h.state.get().editor.language.snippet.is_none());
    assert_eq!(h.cursor(), h.document_text().len() - 1);
}

#[test]
fn server_variable_transforms_use_the_real_completion_document_path() {
    let h = received_snippet(
        "${TM_FILENAME/(.*)\\.rs/${1:/upcase}/}: ${1:body}$0",
        "enter_and_tab",
    );
    let name = h
        .state
        .get()
        .workspace
        .active_path()
        .unwrap()
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .to_uppercase();
    assert_eq!(h.document_text(), format!("{name}: body\n"));
    assert_eq!(&h.document_text()[h.selection().unwrap()], "body");
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

fn choice_index(h: &LanguageHarness) -> usize {
    let app = h.state.get();
    snippets::choice_view(&app, app.workspace.active_pane())
        .unwrap()
        .1
        .selected
}

#[test]
fn snippet_choice_keys_accept_independently_of_completion_settings_and_tab_visits_the_rest() {
    for accept_on in ["enter", "tab"] {
        let mut h = choice_snippet("${1|red,蓝色,green|}:$1 $2$0", accept_on);
        assert!(h.has_text("Choose a snippet value"));
        assert_eq!(key(&h, "down"), Some(Action::MenuSelectNext));
        assert_eq!(key(&h, "tab"), Some(Action::MenuConfirmTab));
        h.act(Action::MenuSelectNext);
        h.act(if accept_on == "enter" {
            Action::MenuConfirmTab
        } else {
            Action::MenuConfirm
        });
        assert_eq!(h.document_text(), "蓝色:蓝色 \n");
        assert_eq!(h.selection(), Some(0..6));
        assert!(!snippets::choice_visible(&h.state.get()));
        assert_eq!(key(&h, "tab"), Some(Action::NextSnippetTabstop));
        h.act(Action::NextSnippetTabstop);
        assert_eq!(h.cursor(), h.document_text().len() - 1);
        h.act(Action::PreviousSnippetTabstop);
        assert!(snippets::choice_visible(&h.state.get()));
        assert_eq!(choice_index(&h), 1);
        h.act(Action::MenuCancel);
        assert!(!snippets::choice_visible(&h.state.get()));
        assert!(h.state.get().editor.language.snippet.is_some());
        assert_eq!(key(&h, "ctrl-space"), Some(Action::ShowSnippetChoices));
        h.act(Action::ShowSnippetChoices);
        assert_eq!(choice_index(&h), 1);
    }
}

#[test]
fn snippet_choice_mouse_selection_keeps_focus_and_old_menu_events_cannot_change_a_new_stop() {
    use lgui::core::{InputEvent, Point, PointerData};
    let mut h = choice_snippet("${1|red,blue,green|}:$1 ${2|x,y|}$0", "enter_and_tab");
    let (x, y) = h.point_at(0, 1);
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
    let focused = h.session.runtime().interaction_state().focused.clone();
    assert!(focused.is_some());
    h.act(Action::ShowSnippetChoices);
    let bounds = h
        .session
        .tree()
        .nodes()
        .iter()
        .find(|n| n.text.as_deref() == Some("green"))
        .unwrap()
        .paint_bounds;
    let pointer = PointerData::mouse(Point::new(
        (bounds.left + bounds.right) / 2.0,
        (bounds.top + bounds.bottom) / 2.0,
    ));
    let old = snippets::choice_view(&h.state.get(), PaneId::new(1))
        .unwrap()
        .0;
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
    assert_eq!(h.document_text(), "green:green x\n");
    assert_eq!(h.selection(), Some(0..5));
    assert_eq!(h.session.runtime().interaction_state().focused, focused);
    h.act(Action::NextSnippetTabstop);
    let before = h.document_text();
    h.state.update(|app| {
        assert!(!snippets::choice_event(
            app,
            old,
            crate::ui::editor::popup::Event::Pick(1)
        ))
    });
    assert_eq!(h.document_text(), before);
    assert_eq!(choice_index(&h), 0);
}

#[test]
fn snippet_choices_allow_literal_typing_and_explicit_completion_without_reopening_the_menu() {
    let mut h = choice_snippet("${1|beta,other|}:$1 $0", "enter_and_tab");
    h.type_text("custom");
    assert_eq!(h.document_text(), "custom:custom \n");
    assert!(!snippets::choice_visible(&h.state.get()));
    h.act(Action::ShowSnippetChoices);
    h.act(Action::MenuConfirm);
    assert_eq!(h.document_text(), "beta:beta \n");
    h.act(Action::ShowSnippetChoices);
    h.act(Action::ShowCompletions);
    h.until("explicit completion while filling a snippet", |h| {
        crate::app::language_ui::completion_visible(&h.state.get())
    });
    assert!(!snippets::choice_visible(&h.state.get()));
    h.act(Action::ShowSnippetChoices);
    assert!(snippets::choice_visible(&h.state.get()));
    assert!(!crate::app::language_ui::completion_visible(&h.state.get()));
    h.act(Action::MenuCancel);
    crate::app::editor::execute_command(
        &h.state,
        Command::Escape,
        h.rect,
        Arc::new(TestClipboard::default()),
    );
    assert!(h.state.get().editor.language.snippet.is_none());
    assert_eq!(key(&h, "ctrl-space"), Some(Action::ShowCompletions));
}

#[test]
fn snippet_choice_undo_redo_uses_one_step_for_all_mirrors_and_ends_the_session() {
    let mut h = choice_snippet("${1|one,two|}:$1 $0", "enter_and_tab");
    h.act(Action::MenuSelectNext);
    h.act(Action::MenuConfirm);
    assert_eq!(h.document_text(), "two:two \n");
    h.act(Action::ShowSnippetChoices);
    crate::app::editor::execute_command(
        &h.state,
        Command::Undo,
        h.rect,
        Arc::new(TestClipboard::default()),
    );
    h.render();
    assert_eq!(h.document_text(), "one:one \n");
    assert!(h.state.get().editor.language.snippet.is_none());
    crate::app::editor::execute_command(
        &h.state,
        Command::Redo,
        h.rect,
        Arc::new(TestClipboard::default()),
    );
    h.render();
    assert_eq!(h.document_text(), "two:two \n");
    assert!(!h.has_text("Choose a snippet value"));
}

#[test]
fn snippet_choice_wheel_pages_through_rows_and_empty_escaped_choices_keep_their_literal_values() {
    let options = (0..24)
        .map(|n| format!("v{n}"))
        .collect::<Vec<_>>()
        .join(",");
    let mut h = choice_snippet(&format!("${{1|{options}|}} $0"), "enter_and_tab");
    h.act(Action::MenuPageDown);
    assert_eq!(choice_index(&h), 10);
    let (revision, _) = snippets::choice_view(&h.state.get(), PaneId::new(1)).unwrap();
    h.state.update(|app| {
        snippets::choice_event(app, revision, crate::ui::editor::popup::Event::Scroll(10));
    });
    h.render();
    assert!(h.has_text("v20"));
    h.act(Action::MenuPageDown);
    h.act(Action::MenuConfirm);
    assert_eq!(h.document_text(), "v20 \n");
    let mut literals = choice_snippet(r"${1|,a\,b,c\|d|}=$1$0", "enter_and_tab");
    assert!(literals.has_text("(empty)"));
    literals.act(Action::MenuSelectNext);
    literals.act(Action::MenuConfirm);
    assert_eq!(literals.document_text(), "a,b=a,b\n");
    literals.act(Action::ShowSnippetChoices);
    literals.act(Action::MenuSelectNext);
    literals.act(Action::MenuConfirm);
    assert_eq!(literals.document_text(), "c|d=c|d\n");
}
