//! Production-path integration checks, using real encoded files and repositories.
use super::{
    state::AppState,
    workspace::{OpenMode, ReconcileResult, Workspace},
};
use crate::features::encoding::{self, Charset, TextFormat};
use crate::infrastructure::{document_filesystem as files, search_filesystem};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "loog-encoding-tests-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn gbk() -> TextFormat {
    TextFormat::new(Charset::Gbk, false)
}
fn fixture() -> &'static str {
    "这是用于验证编码兼容性的中文文件，包含常用字符。\r\n第二行可以搜索中文内容。\r\n"
}
fn open(workspace: &mut Workspace, path: &std::path::Path) -> crate::domain::identity::FileId {
    workspace.open_prepared_text(super::encoding::prepare(path).unwrap(), OpenMode::Permanent)
}
#[test]
fn encoding_open_switch_undo_and_save_use_the_displayed_text() {
    let temp = Temp::new();
    let path = temp.0.join("中文.txt");
    let original = encoding::encode(fixture(), gbk()).unwrap();
    std::fs::write(&path, &original).unwrap();
    let mut w = Workspace::new();
    let id = open(&mut w, &path);
    assert_eq!(w.documents[&id].buffer.text(), fixture());
    assert_eq!(w.documents[&id].format(), gbk());
    assert!(!w.is_dirty(id));
    assert!(w.set_encoding(id, TextFormat::default()).unwrap());
    let displayed = encoding::decode_explicit(&original, TextFormat::default(), 10000)
        .unwrap()
        .text;
    assert_ne!(displayed, fixture());
    assert_eq!(w.documents[&id].buffer.text(), displayed);
    assert!(w.is_dirty(id));
    assert_eq!(std::fs::read(&path).unwrap(), original);
    w.document_editor_mut(id).unwrap().undo();
    assert_eq!(w.documents[&id].format(), gbk());
    assert_eq!(w.documents[&id].buffer.text(), fixture());
    assert!(!w.is_dirty(id));
    w.document_editor_mut(id).unwrap().redo();
    assert_eq!(w.documents[&id].format(), TextFormat::default());
    assert!(w.is_dirty(id));
    files::write_encoded(
        &path,
        w.documents[&id].buffer.text(),
        w.documents[&id].format(),
    )
    .unwrap();
    w.mark_saved(id);
    assert!(!w.is_dirty(id));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), displayed);
    let changed = format!("{}外部修改\r\n", fixture());
    files::write_encoded(&path, &changed, TextFormat::default()).unwrap();
    assert!(matches!(
        w.reconcile_document(id),
        ReconcileResult::Reloaded(_)
    ));
    assert_eq!(w.documents[&id].buffer.text(), changed);
    assert_eq!(w.documents[&id].format(), TextFormat::default());
    let bom = TextFormat::new(Charset::Utf16Le, true);
    files::write_encoded(&path, &changed, bom).unwrap();
    assert!(matches!(
        w.reconcile_document(id),
        ReconcileResult::Reloaded(_)
    ));
    assert_eq!(w.documents[&id].format(), TextFormat::default());
    assert_ne!(w.documents[&id].buffer.text(), changed);
    assert!(!w.is_dirty(id));
    assert!(matches!(
        w.reconcile_document(id),
        ReconcileResult::Unchanged(_)
    ));
}
#[test]
fn encoding_save_failure_preserves_original_and_ascii_selected_format_survives_save() {
    let temp = Temp::new();
    let path = temp.0.join("ascii.txt");
    std::fs::write(&path, "hello").unwrap();
    let mut w = Workspace::new();
    let id = open(&mut w, &path);
    w.set_encoding(id, gbk()).unwrap();
    files::write_encoded(&path, "hello", gbk()).unwrap();
    w.mark_saved(id);
    assert_eq!(w.documents[&id].format(), gbk());
    assert!(!w.is_dirty(id));
    assert!(files::write_encoded(&path, "hello 😀", gbk()).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
    assert_eq!(std::fs::read_dir(&temp.0).unwrap().count(), 1);
}
#[test]
fn encoding_metadata_only_changes_protect_dirty_documents_from_disk_updates() {
    let temp = Temp::new();
    let path = temp.0.join("file.txt");
    files::write_encoded(&path, "hello", gbk()).unwrap();
    let mut w = Workspace::new();
    let id = open(&mut w, &path);
    let selected = TextFormat::new(Charset::Utf8, true);
    w.set_encoding(id, selected).unwrap();
    files::write_encoded(&path, fixture(), TextFormat::new(Charset::Utf16Be, true)).unwrap();
    assert!(matches!(
        w.reconcile_document(id),
        ReconcileResult::Conflict(_)
    ));
    assert_eq!(w.documents[&id].format(), selected);
    assert!(w.is_dirty(id));
    w.accept_disk_version(id).unwrap();
    assert_eq!(w.documents[&id].format(), TextFormat::default());
    assert!(!w.is_dirty(id));
    files::write_encoded(&path, fixture(), gbk()).unwrap();
    let prepared = w.prepare_disk_updates(std::slice::from_ref(&path), false);
    w.set_encoding(id, gbk()).unwrap();
    w.apply_disk_updates(prepared);
    assert_eq!(w.documents[&id].format(), gbk());
    assert!(w.is_dirty(id));
}
#[test]
fn encoding_search_and_diff_open_return_the_same_unicode_offsets() {
    let temp = Temp::new();
    for (name, format) in [
        ("gbk.txt", gbk()),
        ("utf16.txt", TextFormat::new(Charset::Utf16Le, true)),
        ("utf8.txt", TextFormat::new(Charset::Utf8, true)),
    ] {
        let path = temp.0.join(name);
        files::write_encoded(&path, fixture(), format).unwrap();
        let text = search_filesystem::read_text(&path).unwrap();
        assert_eq!(text, fixture());
        assert_eq!(files::read_diff_file(&path).unwrap(), text);
        let mut w = Workspace::new();
        let id = open(&mut w, &path);
        let offset = text.find("搜索").unwrap();
        assert_eq!(&w.documents[&id].buffer.text()[offset..offset + 6], "搜索");
    }
}
#[test]
fn encoding_session_restores_unsaved_text_format_and_read_override() {
    let temp = Temp::new();
    let path = temp.0.join("session.txt");
    files::write_encoded(&path, fixture(), gbk()).unwrap();
    let mut app = AppState::new();
    let id = open(&mut app.workspace, &path);
    app.workspace
        .documents
        .get_mut(&id)
        .unwrap()
        .meta
        .read_override = Some(gbk());
    app.workspace
        .set_encoding(id, TextFormat::new(Charset::Utf8, true))
        .unwrap();
    {
        let mut editor = app.workspace.document_editor_mut(id).unwrap();
        loog_editor_core::normal::insert(&mut editor, "未保存的文字");
    }
    let expected = app.workspace.documents[&id].buffer.text().to_owned();
    let layout = super::workspace_session::saved_editor_layout(&app.workspace).unwrap();
    let serialized = serde_json::to_string(&layout).unwrap();
    let layout = serde_json::from_str(&serialized).unwrap();
    let mut restored = AppState::new();
    super::workspace_session::hydrate_editor_layout_with(
        &mut restored,
        Some(layout),
        files::read_text_file,
    );
    let id = restored.workspace.active_document().unwrap();
    let doc = &restored.workspace.documents[&id];
    assert_eq!(doc.buffer.text(), expected);
    assert_eq!(doc.format(), TextFormat::new(Charset::Utf8, true));
    assert_eq!(
        doc.meta.read_override,
        Some(TextFormat::new(Charset::Utf8, true))
    );
    assert!(doc.is_dirty());
}
#[test]
fn encoding_project_settings_apply_to_reads_and_defaults() {
    let temp = Temp::new();
    std::fs::create_dir(temp.0.join(".loog")).unwrap();
    std::fs::write(
        temp.0.join(".loog/settings.json"),
        r#"{"encoding":{"auto_detect":false,"new_file":{"charset":"gbk"}}}"#,
    )
    .unwrap();
    let path = temp.0.join("a.txt");
    files::write_encoded(&path, fixture(), gbk()).unwrap();
    let options = files::options(&path);
    assert!(!options.auto_detect);
    assert_eq!(options.new_file, gbk());
    assert_ne!(files::read_text_file(&path).unwrap(), fixture());
    assert_eq!(
        files::read_encoded(&path, Some(gbk()), 10000)
            .unwrap()
            .0
            .text,
        fixture()
    );
}
#[test]
#[ignore = "Set LOOG_ENCODING_FIXTURE to check a private local file without logging its contents"]
fn encoding_private_fixture_opens_and_converts_without_modifying_original() {
    let path = PathBuf::from(std::env::var_os("LOOG_ENCODING_FIXTURE").expect("fixture path"));
    let original = std::fs::read(&path).unwrap();
    let (decoded, _) = files::read_encoded(&path, None, files::MAX_EDITABLE_FILE_BYTES).unwrap();
    let temp = Temp::new();
    let copy = temp.0.join("copy.txt");
    files::write_encoded(&copy, &decoded.text, decoded.format).unwrap();
    assert_eq!(std::fs::read(&copy).unwrap(), original);
    files::write_encoded(&copy, &decoded.text, TextFormat::default()).unwrap();
    assert_eq!(files::read_text_file(&copy).unwrap(), decoded.text);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    eprintln!(
        "Fixture verified: {} bytes, {}",
        original.len(),
        decoded.format.label()
    );
}

#[test]
fn encoding_status_picker_marks_dirty_and_ctrl_s_flow_writes_selected_bytes() {
    let temp = Temp::new();
    let path = temp.0.join("picker.txt");
    files::write_encoded(&path, fixture(), gbk()).unwrap();
    let mut app = AppState::new();
    let id = open(&mut app.workspace, &path);
    let saving = super::test_support::SavingUi::new(app);
    let state = saving.state.clone();
    let binding = super::chrome::status_binding(&state);
    assert_eq!(binding.frame().file.unwrap().encoding, "GBK");
    binding.run(crate::ui::chrome::StatusCommand::Encoding);
    assert!(state.get().chrome.encoding.is_some());
    super::encoding::select(&state, TextFormat::new(Charset::Utf8, true));
    assert!(state.get().workspace.is_dirty(id));
    assert!(state.get().chrome.encoding.is_none());
    assert!(
        !std::fs::read(&path)
            .unwrap()
            .starts_with(&[0xef, 0xbb, 0xbf])
    );
    assert!(super::workspace_commands::save_document(&state, id));
    saving.wait();
    assert!(!state.get().workspace.is_dirty(id));
    assert!(
        std::fs::read(&path)
            .unwrap()
            .starts_with(&[0xef, 0xbb, 0xbf])
    );
    super::encoding::open(&state);
    super::encoding::select(&state, gbk());
    state.update(|app| {
        let mut e = app.workspace.document_editor_mut(id).unwrap();
        loog_editor_core::normal::insert(&mut e, "😀");
    });
    let before = std::fs::read(&path).unwrap();
    assert!(super::workspace_commands::save_document(&state, id));
    saving.wait();
    assert!(
        state
            .get()
            .chrome
            .toast
            .as_ref()
            .unwrap()
            .message
            .contains("cannot represent")
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(state.get().workspace.is_dirty(id));
}
#[test]
fn encoding_manual_choice_displays_errors_and_switching_back_restores_original_bytes() {
    let temp = Temp::new();
    let path = temp.0.join("switch.txt");
    files::write_encoded(&path, fixture(), gbk()).unwrap();
    let original = std::fs::read(&path).unwrap();
    let mut app = AppState::new();
    let id = open(&mut app.workspace, &path);
    let state = super::test_support::live(app);
    super::encoding::open(&state);
    super::encoding::select(&state, TextFormat::default());
    let displayed = state.get().workspace.documents[&id]
        .buffer
        .text()
        .to_owned();
    assert_ne!(displayed, fixture());
    assert!(displayed.contains('\u{fffd}'));
    assert_eq!(
        state.get().workspace.documents[&id].format(),
        TextFormat::default()
    );
    assert!(state.get().workspace.is_dirty(id));
    assert!(state.get().chrome.encoding.is_none());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    // A disk change does not replace the retained source of unsaved selections.
    files::write_encoded(&path, &format!("{}new", fixture()), gbk()).unwrap();
    super::encoding::open(&state);
    super::encoding::select(&state, gbk());
    assert_eq!(
        state.get().workspace.documents[&id].buffer.text(),
        fixture()
    );
    assert!(!state.get().workspace.is_dirty(id));
    assert_eq!(
        std::fs::read(&path).unwrap(),
        encoding::encode(&format!("{}new", fixture()), gbk()).unwrap()
    );
}
#[test]
fn encoding_rename_preserves_pending_format_and_undo_order() {
    let temp = Temp::new();
    let path = temp.0.join("old.txt");
    let new = temp.0.join("new.txt");
    files::write_encoded(&path, fixture(), gbk()).unwrap();
    let mut w = Workspace::new();
    let id = open(&mut w, &path);
    w.documents.get_mut(&id).unwrap().meta.read_override = Some(gbk());
    w.set_encoding(id, TextFormat::default()).unwrap();
    let displayed = w.documents[&id].buffer.text().to_owned();
    {
        let mut e = w.document_editor_mut(id).unwrap();
        loog_editor_core::normal::insert(&mut e, "x");
    }
    std::fs::rename(&path, &new).unwrap();
    assert!(w.move_open_path(&path, new));
    assert_eq!(w.documents[&id].format(), TextFormat::default());
    assert_eq!(
        w.documents[&id].meta.read_override,
        Some(TextFormat::default())
    );
    w.document_editor_mut(id).unwrap().undo();
    assert_eq!(w.documents[&id].buffer.text(), displayed);
    assert_eq!(w.documents[&id].format(), TextFormat::default());
    assert!(w.is_dirty(id));
    w.document_editor_mut(id).unwrap().undo();
    assert_eq!(w.documents[&id].format(), gbk());
    assert!(!w.is_dirty(id));
}

#[test]
fn encoding_full_workspace_search_returns_all_formats_with_editor_positions() {
    use crate::features::search::{Request, Results, SearchSpec};
    let temp = Temp::new();
    for (name, format) in [
        ("gbk.txt", gbk()),
        ("utf16.txt", TextFormat::new(Charset::Utf16Le, true)),
        ("utf8.txt", TextFormat::new(Charset::Utf8, true)),
    ] {
        files::write_encoded(&temp.0.join(name), fixture(), format).unwrap();
    }
    let mut results = Results::default();
    search_filesystem::run(
        Request {
            roots: vec![temp.0.clone()],
            buffers: vec![],
            spec: SearchSpec {
                query: "搜索".into(),
                include: String::new(),
                exclude: String::new(),
                case_sensitive: true,
                whole_word: false,
                regex: false,
                include_hidden: false,
                include_ignored: false,
            },
        },
        &std::sync::atomic::AtomicBool::new(false),
        |value| results = value,
    )
    .unwrap();
    assert!(results.complete);
    assert_eq!(results.matches, 3);
    assert_eq!(results.files.len(), 3);
    assert_eq!(results.skipped_encoding, 0);
    for file in results.files {
        let text = files::read_text_file(file.path.local_path().unwrap()).unwrap();
        for found in &file.matches {
            assert_eq!(&text[found.range.clone()], "搜索");
            assert_eq!(found.line, 1);
        }
    }
}

#[test]
fn encoding_picker_real_click_selects_without_writing_and_keyboard_can_cancel() {
    use lgui::{
        application::{AppView, ApplicationContext},
        core::{
            InputEvent, KeyboardEvent, LogicalKey, NamedKey, Point, PointerButton, PointerData,
            UiScale,
        },
        session::UiSession,
    };
    let temp = Temp::new();
    let path = temp.0.join("ui.txt");
    files::write_encoded(&path, fixture(), gbk()).unwrap();
    let original = std::fs::read(&path).unwrap();
    let mut app = AppState::new();
    let id = open(&mut app.workspace, &path);
    let state = super::test_support::live(app);
    let output = state.clone();
    let rect = lgui::prelude::UiRect::new(0.0, 0.0, 900.0, 800.0);
    let view: AppView = std::sync::Arc::new(move |_cx| {
        let status_rect = lgui::prelude::UiRect::new(0.0, 770.0, 900.0, 800.0);
        let status = super::chrome::status_binding(&output);
        let anchor = crate::ui::statusbar::encoding_anchor(status_rect, &status.frame());
        let bar = crate::ui::statusbar::render(status_rect, status);
        let mut root = lgui::prelude::panel(rect, lgui::prelude::VisualStyle::default()).child(bar);
        if output.get().chrome.encoding.is_some() {
            root = root.child(crate::ui::encoding_picker::render(
                rect,
                anchor,
                super::encoding::binding(&output),
            ));
        }
        root
    });
    let context = ApplicationContext::empty(Default::default());
    let _fonts = lgui::backend::install_text_environment(
        &context,
        lgui::render_skia::skia_text_system_handle(),
    );
    let mut session = UiSession::new();
    session.render_view(&view, rect, UiScale::ONE);
    let status = session
        .tree()
        .nodes()
        .iter()
        .find(|n| n.text.as_deref() == Some("GBK"))
        .expect("actual encoding status")
        .paint_bounds;
    let point = PointerData::mouse(Point::new(
        (status.left + status.right) / 2.0,
        (status.top + status.bottom) / 2.0,
    ));
    for input in [
        InputEvent::PointerDown {
            pointer: point,
            button: PointerButton::Left,
        },
        InputEvent::PointerUp {
            pointer: point,
            button: PointerButton::Left,
        },
    ] {
        let events = session.handle_input(input);
        lgui::core::dispatch_runtime_output(
            events,
            &context,
            &lgui::window::WindowId::new("encoding-status-test"),
            |action| session.handle_default_action(action),
            |_| {},
        );
    }
    assert!(
        state.get().chrome.encoding.is_some(),
        "encoding click must not open the overlaid language-server panel"
    );
    session.render_view(&view, rect, UiScale::ONE);

    assert!(!session.tree().nodes().iter().any(|node| {
        node.text
            .as_deref()
            .is_some_and(|text| text.contains("Correct Text Display"))
    }));
    let hints: Vec<_> = session
        .tree()
        .nodes()
        .iter()
        .filter(|node| node.text.as_deref() == Some("Suggested"))
        .collect();
    assert_eq!(hints.len(), 1);
    let suggested_rows: Vec<_> = session
        .tree()
        .nodes()
        .iter()
        .filter(|node| node.text.as_deref() == Some("GBK") && node.paint_bounds.bottom < 770.0)
        .collect();
    assert_eq!(
        suggested_rows.len(),
        1,
        "suggested encoding must not be duplicated in the menu"
    );
    assert!(hints[0].paint_bounds.top < suggested_rows[0].paint_bounds.bottom);
    assert!(hints[0].paint_bounds.bottom > suggested_rows[0].paint_bounds.top);

    let button = session
        .tree()
        .nodes()
        .iter()
        .find(|n| n.text.as_deref() == Some("UTF-8 BOM"))
        .expect("UTF-8 BOM choice")
        .paint_bounds;
    let pointer = PointerData::mouse(Point::new(
        (button.left + button.right) / 2.0,
        (button.top + button.bottom) / 2.0,
    ));
    for event in [
        InputEvent::PointerDown {
            pointer,
            button: PointerButton::Left,
        },
        InputEvent::PointerUp {
            pointer,
            button: PointerButton::Left,
        },
    ] {
        let events = session.handle_input(event);
        lgui::core::dispatch_runtime_output(
            events,
            &context,
            &lgui::window::WindowId::new("encoding-test"),
            |action| session.handle_default_action(action),
            |_| {},
        );
    }
    assert!(state.get().chrome.encoding.is_none());
    assert_eq!(
        state.get().workspace.documents[&id].format(),
        TextFormat::new(Charset::Utf8, true)
    );
    assert!(state.get().workspace.is_dirty(id));
    assert_eq!(std::fs::read(&path).unwrap(), original);
    super::encoding::open(&state);
    assert!(super::encoding::key(
        &state,
        &KeyboardEvent {
            key: LogicalKey::Named(NamedKey::Escape),
            ..Default::default()
        }
    ));
    assert!(state.get().chrome.encoding.is_none());
}

#[test]
fn encoding_manual_read_choice_survives_save_and_async_reload_detects_conflict() {
    let temp = Temp::new();
    let path = temp.0.join("override.txt");
    files::write_encoded(&path, fixture(), gbk()).unwrap();
    let mut w = Workspace::new();
    let id = open(&mut w, &path);
    w.documents.get_mut(&id).unwrap().meta.read_override = Some(gbk());
    w.set_encoding(id, TextFormat::default()).unwrap();
    files::write_encoded(&path, fixture(), TextFormat::default()).unwrap();
    w.mark_saved(id);
    assert_eq!(
        w.documents[&id].meta.read_override,
        Some(TextFormat::default())
    );
    let changed = format!("{}中文外部修改", fixture());
    files::write_encoded(&path, &changed, TextFormat::default()).unwrap();
    assert!(matches!(
        w.reconcile_document(id),
        ReconcileResult::Reloaded(_)
    ));
    assert_eq!(w.documents[&id].buffer.text(), changed);
    files::write_encoded(&path, fixture(), TextFormat::default()).unwrap();
    let update = w.prepare_disk_updates(std::slice::from_ref(&path), false);
    w.set_encoding(id, gbk()).unwrap();
    let results = w.apply_disk_updates(update);
    assert!(matches!(results.as_slice(), [ReconcileResult::Conflict(_)]));
    assert!(w.has_disk_conflict(id));
    assert_eq!(w.documents[&id].format(), gbk());
}

#[test]
fn encoding_switches_keep_unsaved_edits_and_undo_text_and_format_together() {
    let temp = Temp::new();
    let path = temp.0.join("edited.txt");
    std::fs::write(&path, fixture()).unwrap();
    let mut w = Workspace::new();
    let id = open(&mut w, &path);
    {
        let mut editor = w.document_editor_mut(id).unwrap();
        editor.set_cursor(editor.buffer().len());
        loog_editor_core::normal::insert(&mut editor, "edit");
    }
    let edited = w.documents[&id].buffer.text().to_owned();
    let wrong = TextFormat::new(Charset::Utf16Be, false);
    w.set_encoding(id, wrong).unwrap();
    assert_ne!(w.documents[&id].buffer.text(), edited);
    w.set_encoding(id, TextFormat::default()).unwrap();
    assert_eq!(w.documents[&id].buffer.text(), edited);
    assert!(w.is_dirty(id));
    w.document_editor_mut(id).unwrap().undo();
    assert_eq!(w.documents[&id].format(), wrong);
    w.document_editor_mut(id).unwrap().undo();
    assert_eq!(w.documents[&id].format(), TextFormat::default());
    assert_eq!(w.documents[&id].buffer.text(), edited);
    w.document_editor_mut(id).unwrap().undo();
    assert_eq!(w.documents[&id].buffer.text(), fixture());
    assert!(!w.is_dirty(id));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), fixture());
}

#[test]
fn encoding_switch_preserves_unicode_edits_that_do_not_fit_the_previous_format() {
    let temp = Temp::new();
    let path = temp.0.join("emoji.txt");
    files::write_encoded(&path, fixture(), gbk()).unwrap();
    let original = std::fs::read(&path).unwrap();
    let mut w = Workspace::new();
    let id = open(&mut w, &path);
    loog_editor_core::normal::insert(&mut w.document_editor_mut(id).unwrap(), "😀");
    let edited = w.documents[&id].buffer.text().to_owned();
    w.set_encoding(id, TextFormat::default()).unwrap();
    assert_eq!(w.documents[&id].buffer.text(), edited);
    assert_eq!(w.documents[&id].format(), TextFormat::default());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    w.document_editor_mut(id).unwrap().undo();
    assert_eq!(w.documents[&id].format(), gbk());
    assert_eq!(w.documents[&id].buffer.text(), edited);
}

#[test]
fn encoding_session_retains_bytes_behind_a_garbled_display() {
    let temp = Temp::new();
    let path = temp.0.join("recover-by-selection.txt");
    files::write_encoded(&path, fixture(), gbk()).unwrap();
    let original = std::fs::read(&path).unwrap();
    let mut app = AppState::new();
    let id = open(&mut app.workspace, &path);
    app.workspace
        .set_encoding(id, TextFormat::default())
        .unwrap();
    let layout = super::workspace_session::saved_editor_layout(&app.workspace).unwrap();
    let serialized = serde_json::to_string(&layout).unwrap();
    let mut restored = AppState::new();
    super::workspace_session::hydrate_editor_layout_with(
        &mut restored,
        Some(serde_json::from_str(&serialized).unwrap()),
        files::read_text_file,
    );
    let id = restored.workspace.active_document().unwrap();
    assert_ne!(restored.workspace.documents[&id].buffer.text(), fixture());
    restored.workspace.set_encoding(id, gbk()).unwrap();
    assert_eq!(restored.workspace.documents[&id].buffer.text(), fixture());
    assert!(!restored.workspace.is_dirty(id));
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[test]
fn encoding_reload_honors_manual_choice_and_retains_bytes_for_the_next_selection() {
    let temp = Temp::new();
    let path = temp.0.join("reload.txt");
    files::write_encoded(&path, fixture(), gbk()).unwrap();
    let mut w = Workspace::new();
    let id = open(&mut w, &path);
    w.set_encoding(id, TextFormat::new(Charset::Utf8, true))
        .unwrap();
    w.accept_disk_version(id).unwrap();
    assert_eq!(w.documents[&id].format(), TextFormat::default());
    assert_ne!(w.documents[&id].buffer.text(), fixture());
    assert!(!w.is_dirty(id));
    w.set_encoding(id, gbk()).unwrap();
    assert_eq!(w.documents[&id].buffer.text(), fixture());
    assert!(w.is_dirty(id));
}

#[test]
fn encoding_suggestion_never_overrides_the_choice_and_keyboard_uses_display_order() {
    use lgui::core::{KeyboardEvent, LogicalKey, NamedKey};
    let temp = Temp::new();
    let path = temp.0.join("suggested.txt");
    files::write_encoded(&path, fixture(), gbk()).unwrap();
    let original = std::fs::read(&path).unwrap();
    let mut app = AppState::new();
    let id = open(&mut app.workspace, &path);
    app.workspace
        .set_encoding(id, TextFormat::default())
        .unwrap();
    let state = super::test_support::live(app);
    let before = state.get().workspace.documents[&id].buffer.text_snapshot();
    let version = state.get().workspace.documents[&id].buffer.version();
    super::encoding::open(&state);
    let frame = super::encoding::binding(&state).frame().unwrap();
    assert_eq!(frame.suggested, Some(gbk()));
    assert_eq!(frame.formats[0], gbk());
    assert_eq!(frame.formats.len(), TextFormat::choices().len());
    assert_eq!(
        frame
            .formats
            .iter()
            .filter(|format| **format == gbk())
            .count(),
        1
    );
    assert_eq!(frame.current, Some(TextFormat::default()));
    assert_eq!(frame.formats[frame.selected], TextFormat::default());
    assert_eq!(
        state.get().workspace.documents[&id].buffer.version(),
        version
    );
    let event = |key| KeyboardEvent {
        key: LogicalKey::Named(key),
        ..Default::default()
    };
    // Enter on opening keeps the current encoding, even when a hint is above it.
    assert!(super::encoding::key(&state, &event(NamedKey::Enter)));
    assert_eq!(
        state.get().workspace.documents[&id].format(),
        TextFormat::default()
    );
    assert_eq!(
        state.get().workspace.documents[&id].buffer.text(),
        before.as_str()
    );
    assert!(state.get().workspace.is_dirty(id));
    super::encoding::open(&state);
    assert!(super::encoding::key(&state, &event(NamedKey::ArrowUp)));
    assert!(super::encoding::key(&state, &event(NamedKey::Enter)));
    assert_eq!(state.get().workspace.documents[&id].format(), gbk());
    assert_eq!(
        state.get().workspace.documents[&id].buffer.text(),
        fixture()
    );
    assert!(!state.get().workspace.is_dirty(id));
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[test]
fn encoding_ascii_picker_has_no_suggestion_or_reordering() {
    let temp = Temp::new();
    let path = temp.0.join("ascii-suggestion.txt");
    std::fs::write(&path, "hello").unwrap();
    let mut app = AppState::new();
    let id = open(&mut app.workspace, &path);
    let state = super::test_support::live(app);
    super::encoding::open(&state);
    let frame = super::encoding::binding(&state).frame().unwrap();
    assert_eq!(frame.suggested, None);
    assert_eq!(frame.formats, TextFormat::choices());
    assert!(!state.get().workspace.is_dirty(id));
}

#[test]
fn a_file_over_the_load_limit_opens_in_place_and_can_never_be_saved() {
    let dir = Temp::new();
    let path = dir.0.join("large.csv");
    // One byte past the limit, in short rows, with a byte order mark and CRLF breaks.
    let row = "id,中文,value\r\n";
    let body = row.repeat(files::MAX_EDITABLE_FILE_BYTES as usize / row.len() + 1);
    std::fs::write(&path, [&[0xef, 0xbb, 0xbf][..], body.as_bytes()].concat()).unwrap();
    assert!(files::is_too_large_to_load(&path));

    let mut workspace = Workspace::new();
    let id = open(&mut workspace, &path);
    let document = &workspace.documents[&id];
    assert!(document.buffer.is_file_backed());
    assert!(document.format().bom, "the byte order mark is kept for display");
    let rows = body.len() / row.len();
    assert_eq!(document.buffer.line_count(), rows + 1);
    assert_eq!(document.buffer.line_text(1), "id,中文,value");
    assert_eq!(document.buffer.line_ending(), "\r\n");
    // Opened and untouched, it is clean, and there is nothing that could be written back.
    assert!(!workspace.is_dirty(id));
    assert!(workspace.shared_save_snapshot(id).is_none());
    assert!(workspace.save_snapshot(id).is_none());
    // Reconciling with the disk neither reads the file nor reports a change.
    assert!(matches!(
        workspace.reconcile_document(id),
        ReconcileResult::Unchanged(_)
    ));
}

#[test]
fn a_file_over_the_load_limit_can_be_edited_and_undone_without_loading_it() {
    use loog_editor_core::text::EditKind;
    let dir = Temp::new();
    let path = dir.0.join("large.txt");
    let row = "some row of text\n";
    let body = row.repeat(files::MAX_EDITABLE_FILE_BYTES as usize / row.len() + 1);
    std::fs::write(&path, &body).unwrap();
    let mut workspace = Workspace::new();
    let id = open(&mut workspace, &path);
    assert!(!workspace.is_dirty(id));

    let middle = workspace.active_editor().unwrap().line_start(1_000_000);
    workspace
        .active_editor_mut()
        .unwrap()
        .replace(middle..middle, "中间", EditKind::Other)
        .unwrap();
    {
        let editor = workspace.active_editor().unwrap();
        assert_eq!(editor.line_text(1_000_000), "中间some row of text");
        assert_eq!(editor.line_text(999_999), "some row of text");
        assert_eq!(editor.len(), body.len() + "中间".len());
    }
    assert!(workspace.is_dirty(id));
    // Nothing of it can be written, and its edits are not offered to session recovery.
    assert!(workspace.shared_save_snapshot(id).is_none());

    workspace.active_editor_mut().unwrap().undo();
    assert_eq!(workspace.active_editor().unwrap().line_text(1_000_000), "some row of text");
    assert!(!workspace.is_dirty(id), "undoing every edit makes it the file again");
}

#[test]
fn a_file_backed_document_saves_by_streaming_and_then_reads_the_new_file() {
    use loog_editor_core::text::EditKind;
    let dir = Temp::new();
    let path = dir.0.join("large.csv");
    let row = "id,中文,value\r\n";
    let body = row.repeat(files::MAX_EDITABLE_FILE_BYTES as usize / row.len() + 1);
    let bom = [0xef, 0xbb, 0xbf];
    std::fs::write(&path, [&bom[..], body.as_bytes()].concat()).unwrap();
    let mut workspace = Workspace::new();
    let id = open(&mut workspace, &path);

    // Edit at the start, in the middle (a whole row) and at the very end.
    let middle = workspace.active_editor().unwrap().line_range_with_break(500_000);
    let end = workspace.active_editor().unwrap().len();
    {
        let mut editor = workspace.active_editor_mut().unwrap();
        editor.replace(end..end, "tail", EditKind::Other).unwrap();
        editor.replace(middle.clone(), "replaced row\r\n", EditKind::Other).unwrap();
        editor.replace(0..0, "head,", EditKind::Other).unwrap();
    }
    let mut expected = body.clone();
    expected.push_str("tail");
    expected.replace_range(middle, "replaced row\r\n");
    expected.insert_str(0, "head,");
    assert!(workspace.is_dirty(id));

    let saved = workspace.file_save_snapshot(id).unwrap();
    files::write_with(&path, |file| saved.pieces.write_to(file)).unwrap();
    let on_disk = std::fs::read(&path).unwrap();
    assert_eq!(&on_disk[..3], &bom, "the byte order mark is written back");
    assert!(on_disk[3..] == *expected.as_bytes(), "the file holds the edited text");
    let (file, disk) = files::open_file_backed(&path).unwrap();
    assert_eq!(disk.size, on_disk.len() as u64);
    assert!(workspace.mark_file_saved(&saved, file, disk));
    assert!(!workspace.is_dirty(id), "saved means clean again");
    {
        let editor = workspace.active_editor().unwrap();
        assert_eq!(editor.line_text(0), "head,id,中文,value");
        assert_eq!(editor.line_text(500_000), "replaced row");
        assert_eq!(editor.len(), expected.len());
    }
    // It edits and undoes as before, now against the new file.
    workspace.active_editor_mut().unwrap().replace(0..0, "z", EditKind::Other).unwrap();
    assert!(workspace.is_dirty(id));
    workspace.active_editor_mut().unwrap().undo();
    assert!(!workspace.is_dirty(id));

    // An edit made while a save was running stays unsaved.
    let saved = workspace.file_save_snapshot(id).unwrap();
    workspace.active_editor_mut().unwrap().replace(0..0, "late,", EditKind::Other).unwrap();
    files::write_with(&path, |file| saved.pieces.write_to(file)).unwrap();
    let (file, disk) = files::open_file_backed(&path).unwrap();
    assert!(workspace.mark_file_saved(&saved, file, disk));
    assert!(workspace.is_dirty(id));
    assert_eq!(workspace.active_editor().unwrap().line_text(0), "late,head,id,中文,value");
}

#[test]
fn a_file_backed_document_follows_changes_made_to_its_file_by_other_programs() {
    use loog_editor_core::text::EditKind;
    let dir = Temp::new();
    let path = dir.0.join("large.txt");
    let row = "some row of text\n";
    let body = row.repeat(files::MAX_EDITABLE_FILE_BYTES as usize / row.len() + 1);
    std::fs::write(&path, &body).unwrap();
    let mut workspace = Workspace::new();
    let id = open(&mut workspace, &path);
    let lines = workspace.active_editor().unwrap().line_count();

    // Nothing changed: nothing is read again.
    assert!(matches!(workspace.reconcile_document(id), ReconcileResult::Unchanged(_)));

    // Another program (Git, for one) rewrote the file. A document with no edits follows it.
    let mut longer = body.clone();
    longer.push_str("added by someone else\n");
    std::fs::write(&path, &longer).unwrap();
    assert!(matches!(workspace.reconcile_document(id), ReconcileResult::Reloaded(_)));
    {
        let editor = workspace.active_editor().unwrap();
        assert_eq!(editor.line_count(), lines + 1);
        assert_eq!(editor.line_text(lines - 1), "added by someone else");
    }
    assert!(!workspace.is_dirty(id));

    // With edits of its own it is a conflict, and the edits are kept until the user chooses.
    workspace.active_editor_mut().unwrap().replace(0..0, "mine ", EditKind::Other).unwrap();
    longer.push_str("and again\n");
    std::fs::write(&path, &longer).unwrap();
    assert!(matches!(workspace.reconcile_document(id), ReconcileResult::Conflict(_)));
    assert_eq!(workspace.active_editor().unwrap().line_text(0), "mine some row of text");
    assert!(workspace.documents[&id].disk_conflict);

    // Taking what is on disk drops the edits and the conflict.
    workspace.accept_disk_version(id).unwrap();
    assert_eq!(workspace.active_editor().unwrap().line_text(0), "some row of text");
    assert!(!workspace.is_dirty(id));
    assert!(!workspace.documents[&id].disk_conflict);
    assert_eq!(workspace.active_editor().unwrap().line_count(), lines + 2);

    // A file that is gone is reported, not silently kept as if it were there.
    std::fs::remove_file(&path).unwrap();
    assert!(matches!(workspace.reconcile_document(id), ReconcileResult::Missing(_)));
}

#[test]
fn a_file_backed_document_is_reloaded_through_the_watcher_path_too() {
    let dir = Temp::new();
    let path = dir.0.join("large.txt");
    let row = "some row of text\n";
    let body = row.repeat(files::MAX_EDITABLE_FILE_BYTES as usize / row.len() + 1);
    std::fs::write(&path, &body).unwrap();
    let mut workspace = Workspace::new();
    let id = open(&mut workspace, &path);
    let lines = workspace.active_editor().unwrap().line_count();

    std::fs::write(&path, format!("{body}new last line\n")).unwrap();
    // Prepared off the UI thread against a copy, then installed if nothing moved meanwhile.
    let updates = workspace.prepare_disk_updates(std::slice::from_ref(&path), false);
    assert_eq!(updates.len(), 1);
    let outcomes = workspace.apply_disk_updates(updates);
    assert!(matches!(outcomes.as_slice(), [ReconcileResult::Reloaded(_)]));
    let editor = workspace.active_editor().unwrap();
    assert_eq!(editor.line_count(), lines + 1);
    assert_eq!(editor.line_text(lines - 1), "new last line");
    assert!(!workspace.is_dirty(id));
}
