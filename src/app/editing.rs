//! Resolve editing policy and route compact status bar choices.
use crate::features::settings::editing::{EditorOverrides, EditorSettings, Ending};
use crate::{
    app::state::AppState,
    domain::identity::{FileId, ViewId},
};
use loog_editor_core::text::{FileOptions, LineEnding};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Menu {
    Ending,
    Indent,
}
#[derive(Clone, Default)]
pub struct State {
    pub folding: super::folding::State,
    pub selected: usize,
    pub menu: Option<(ViewId, Menu)>,
    pub manual: HashMap<FileId, EditorOverrides>,
}
pub fn effective(app: &AppState, id: FileId) -> EditorSettings {
    let mut settings = app.preferences.values().editor;
    let Some(doc) = app.workspace.documents.get(&id) else {
        return settings;
    };
    let language = doc.meta.lang.name();
    if let Some(overrides) = app.preferences.values().editor_languages.get(language) {
        overrides.apply(&mut settings);
    }
    let FileOptions {
        indent_size_from_tab,
        indent_size,
        tab_width,
        use_tabs,
        line_ending,
    } = doc.buffer.file_options();
    EditorOverrides {
        indent_size,
        tab_width,
        use_tabs,
        new_file_line_ending: line_ending.map(|e| match e {
            LineEnding::Lf => Ending::Lf,
            LineEnding::Crlf => Ending::Crlf,
        }),
        ..Default::default()
    }
    .apply(&mut settings);
    if indent_size_from_tab {
        settings.indent_size = settings.tab_width;
    }
    if let Some(root) = app.workspace_folder_of(&doc.meta.path)
        && let Some((_, project)) = app
            .preferences
            .values()
            .project_settings
            .iter()
            .find(|(r, _)| r == root)
    {
        if let Some(value) = project.overrides.get("editor")
            && let Ok(overrides) = serde_json::from_value::<EditorOverrides>(value.clone())
        {
            overrides.apply(&mut settings);
        }
        if let Some(value) = project
            .overrides
            .get("editor_languages")
            .and_then(|v| v.get(language))
            && let Ok(overrides) = serde_json::from_value::<EditorOverrides>(value.clone())
        {
            overrides.apply(&mut settings);
        }
    }
    if let Some(overrides) = app.editor.editing.manual.get(&id) {
        overrides.apply(&mut settings);
    }
    settings
}
pub fn configure(app: &mut AppState, view: ViewId) {
    let Some(id) = app.workspace.view_document(view) else {
        return;
    };
    let settings = effective(app, id);
    if let Some(doc) = app.workspace.documents.get_mut(&id) {
        let mut options = settings.core();
        options.language = doc.meta.lang.name();
        doc.buffer.configure(options);
        if doc.buffer.needs_default_line_ending() {
            doc.buffer
                .initialize_line_ending(settings.new_file_line_ending.core());
        }
    }
}
pub fn open(app: &mut AppState, kind: Menu) {
    if let Some(view) = app
        .workspace
        .active()
        .filter(|_| app.workspace.active_editor().is_some())
    {
        configure(app, view);
        app.editor.editing.selected = 0;
        app.editor.editing.menu = Some((view, kind));
    }
}
pub fn choose(app: &mut AppState, index: usize) {
    let Some((view, kind)) = app.editor.editing.menu.take() else {
        return;
    };
    if app.workspace.active() != Some(view) {
        return;
    }
    let Some(id) = app.workspace.view_document(view) else {
        return;
    };
    match kind {
        Menu::Ending => {
            app.workspace.promote_active_preview();
            if let Some(mut editor) = app.workspace.active_editor_mut() {
                let _ = editor.set_line_ending(if index == 0 {
                    LineEnding::Lf
                } else {
                    LineEnding::Crlf
                });
            }
        }
        Menu::Indent => {
            let overrides = app.editor.editing.manual.entry(id).or_default();
            match index {
                0 => {
                    overrides.use_tabs = Some(false);
                    overrides.indent_size = Some(2);
                }
                1 => {
                    overrides.use_tabs = Some(false);
                    overrides.indent_size = Some(4);
                }
                2 => overrides.use_tabs = Some(true),
                3 => overrides.tab_width = Some(2),
                4 => overrides.tab_width = Some(4),
                5 => overrides.tab_width = Some(8),
                _ => {}
            }
            configure(app, view);
        }
    }
}

pub fn configure_all(app: &mut AppState) -> bool {
    let ids = app
        .workspace
        .documents
        .iter()
        .filter(|(_, d)| d.is_text())
        .map(|(&id, _)| id)
        .collect::<Vec<_>>();
    let mut changed = false;
    for id in ids {
        let settings = effective(app, id);
        let doc = app.workspace.documents.get_mut(&id).unwrap();
        let mut desired = settings.core();
        desired.language = doc.meta.lang.name();
        if doc.buffer.options() != desired {
            doc.buffer.configure(desired);
            changed = true;
        }
        if doc.buffer.needs_default_line_ending() {
            doc.buffer
                .initialize_line_ending(settings.new_file_line_ending.core());
            changed = true;
        }
    }
    app.editor
        .editing
        .manual
        .retain(|id, _| app.workspace.documents.contains_key(id));
    changed
}

pub fn menu_event(state: &crate::app::context::AppContext, event: crate::ui::editing_menu::Event) {
    use crate::ui::editing_menu::Event;
    let app = state.get();
    let Some((view, kind)) = app
        .editor
        .editing
        .menu
        .filter(|(view, _)| app.workspace.active() == Some(*view))
    else {
        return;
    };
    let _ = view;
    let base = if kind == Menu::Ending { 2 } else { 6 };
    let index = match event {
        Event::Select(i) => Some(i),
        Event::Accept => Some(app.editor.editing.selected),
        _ => None,
    };
    drop(app);
    if let Some(index) = index.filter(|&i| i >= base) {
        state.update(|app| app.editor.editing.menu = None);
        if index == base {
            crate::app::key_actions::open_user_settings(state);
        } else {
            crate::app::key_actions::open_project_settings(state);
        }
        return;
    }
    state.update(|app| match event {
        Event::Close => app.editor.editing.menu = None,
        Event::Select(i) => choose(app, i),
        Event::Accept => choose(app, app.editor.editing.selected),
        Event::Move(down) => {
            let count = base + 2;
            app.editor.editing.selected = if down {
                (app.editor.editing.selected + 1) % count
            } else {
                (app.editor.editing.selected + count - 1) % count
            };
        }
    });
}
pub fn key(state: &crate::app::context::AppContext, event: &lgui::core::KeyboardEvent) -> bool {
    use crate::ui::editing_menu::Event;
    use lgui::core::{LogicalKey, NamedKey};
    let action = match event.key {
        LogicalKey::Named(NamedKey::Escape) => Some(Event::Close),
        LogicalKey::Named(NamedKey::ArrowDown) => Some(Event::Move(true)),
        LogicalKey::Named(NamedKey::ArrowUp) => Some(Event::Move(false)),
        LogicalKey::Named(NamedKey::Enter) => Some(Event::Accept),
        _ => None,
    };
    if let Some(action) = action {
        menu_event(state, action);
    }
    true
}

pub fn projection(
    app: &AppState,
    view: ViewId,
    rect: lgui::prelude::UiRect,
) -> std::sync::Arc<loog_editor_core::text::DisplayMap> {
    let Some(doc) = app
        .workspace
        .view_document(view)
        .and_then(|id| app.workspace.documents.get(&id))
    else {
        return std::sync::Arc::new(loog_editor_core::text::DisplayMap::new(
            &loog_editor_core::text::TextBuffer::new(""),
            None,
            &[],
        ));
    };
    let left = rect.left
        + crate::ui::fonts::editor().gutter_w_for_lines(doc.buffer.line_count())
        + crate::ui::theme::CODE_PAD;
    let columns = doc.buffer.options().soft_wrap.then(|| {
        ((rect.right - left - crate::ui::editor::view::SCROLLBAR_SIZE)
            / crate::ui::fonts::editor().char_w)
            .floor()
            .max(4.0) as usize
    });
    crate::ui::editor::projection::cached(
        &doc.buffer,
        columns,
        &super::folding::collapsed(app, view),
    )
}
pub fn active_projection(
    app: &AppState,
    rect: lgui::prelude::UiRect,
) -> std::sync::Arc<loog_editor_core::text::DisplayMap> {
    app.workspace
        .active()
        .map(|view| projection(app, view, rect))
        .unwrap_or_else(|| {
            std::sync::Arc::new(loog_editor_core::text::DisplayMap::new(
                &loog_editor_core::text::TextBuffer::new(""),
                None,
                &[],
            ))
        })
}
pub fn toggle_wrap(app: &mut AppState) {
    let Some(view) = app.workspace.active() else {
        return;
    };
    let Some(id) = app.workspace.view_document(view) else {
        return;
    };
    let wrapped = effective(app, id).soft_wrap;
    app.editor.editing.manual.entry(id).or_default().soft_wrap = Some(!wrapped);
    configure(app, view);
}
pub fn move_displayed(app: &mut AppState, command: loog_editor_core::commands::Command) -> bool {
    use loog_editor_core::commands::Command;
    let Command::Move(movement, extend) = command else {
        return false;
    };
    let Some(view) = app.workspace.active() else {
        return false;
    };
    let rect = app
        .editor
        .language
        .rect
        .unwrap_or_else(|| lgui::prelude::UiRect::new(0.0, 0.0, 800.0, 600.0));
    let map = projection(app, view, rect);
    if !map.wrapped() && !map.folded() {
        return false;
    }
    let Some(mut editor) = app.workspace.active_editor_mut() else {
        return false;
    };
    editor.map_selections(|buffer, state| map.navigate(buffer, state, movement, extend));
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Directory(std::path::PathBuf);
    impl Directory {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "loog-editing-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn editorconfig_inheritance_project_language_and_manual_choices_have_explicit_precedence() {
        let dir = Directory::new();
        let child = dir.0.join("src");
        std::fs::create_dir(&child).unwrap();
        std::fs::write(
            dir.0.join(".editorconfig"),
            "root=true\n[*]\nindent_size=4\nend_of_line=crlf\n",
        )
        .unwrap();
        std::fs::write(
            child.join(".editorconfig"),
            "[*.rs]\nindent_style=tab\ntab_width=8\n",
        )
        .unwrap();
        let path = child.join("main.rs");
        let file_options = crate::infrastructure::editorconfig::load(&path);
        assert_eq!(file_options.indent_size, Some(4));
        assert_eq!(file_options.tab_width, Some(8));
        let mut app = AppState::new();
        app.explorer.model.restore(vec![dir.0.clone()], vec![]);
        app.preferences.values_mut().editor.indent_size = 2;
        app.preferences.values_mut().editor_languages.insert(
            "rust".into(),
            EditorOverrides {
                indent_size: Some(3),
                ..Default::default()
            },
        );
        let view = app.workspace.test_open_view(path.clone(), "{}".into());
        let id = app.workspace.view_document(view).unwrap();
        app.workspace
            .documents
            .get_mut(&id)
            .unwrap()
            .buffer
            .initialize_file_options(file_options);
        assert_eq!(effective(&app, id).indent_size, 4);
        assert!(effective(&app, id).use_tabs);
        let (overrides, errors) = crate::features::settings::rules::parse_overrides(
            r#"{"editor":{"indent_size":6},"editor_languages":{"rust":{"indent_size":7}}}"#,
            crate::features::settings::rules::Scope::Project,
        )
        .unwrap();
        assert!(errors.is_empty(), "{errors:?}");
        app.preferences.values_mut().project_settings = vec![(
            dir.0.clone(),
            crate::features::settings::project::ProjectSettings { overrides, errors },
        )];
        assert_eq!(effective(&app, id).indent_size, 7);
        open(&mut app, Menu::Indent);
        choose(&mut app, 1);
        assert_eq!(effective(&app, id).indent_size, 4);
        assert!(!effective(&app, id).use_tabs);
        assert_eq!(app.workspace.active_editor().unwrap().line_ending(), "\r\n");
        assert_eq!(app.workspace.active_editor().unwrap().text(), "{}");
        configure_all(&mut app);
        assert!(!configure_all(&mut app));
    }

    #[test]
    fn conversion_save_bytes_encoding_and_session_policy_round_trip() {
        let dir = Directory::new();
        let path = dir.0.join("text.txt");
        std::fs::write(&path, "中\n尾").unwrap();
        let mut app = AppState::new();
        let view = app.workspace.test_open_view(path.clone(), "中\n尾".into());
        let id = app.workspace.view_document(view).unwrap();
        open(&mut app, Menu::Ending);
        choose(&mut app, 1);
        assert!(app.workspace.is_view_dirty(view));
        let doc = app.workspace.documents.get(&id).unwrap();
        crate::infrastructure::document_filesystem::write_encoded(
            &path,
            doc.buffer.text(),
            doc.format(),
        )
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), "中\r\n尾".as_bytes());
        assert!(app.workspace.mark_saved(id));
        assert_eq!(
            app.workspace.documents[&id].buffer.chosen_line_ending(),
            Some(LineEnding::Crlf)
        );
        assert!(!app.workspace.is_view_dirty(view));
        app.workspace.active_editor_mut().unwrap().undo();
        assert_eq!(app.workspace.active_editor().unwrap().text(), "中\n尾");
        assert!(app.workspace.is_view_dirty(view));
        app.workspace.active_editor_mut().unwrap().redo();
        assert!(!app.workspace.is_view_dirty(view));
        let saved = crate::domain::workspace_session::SavedEncoding {
            line_ending: Some("CRLF".into()),
            current: Default::default(),
            saved: Default::default(),
            read_override: None,
            unsaved_text: None,
            saved_text: None,
            source_bytes: None,
        };
        let doc = app.workspace.documents.get_mut(&id).unwrap();
        doc.buffer = loog_editor_core::text::TextBuffer::new("");
        doc.recovery = Some(saved);
        crate::domain::workspace::restore_encoding(doc);
        assert_eq!(doc.buffer.line_ending(), "\r\n");
    }

    #[test]
    fn stale_status_menu_cannot_modify_another_tab_or_preview() {
        let mut app = AppState::new();
        let first = app
            .workspace
            .test_open_view("one.txt".into(), "a\nb".into());
        open(&mut app, Menu::Ending);
        app.workspace
            .test_open_view("two.txt".into(), "c\nd".into());
        choose(&mut app, 1);
        assert_eq!(app.workspace.active_editor().unwrap().text(), "c\nd");
        app.workspace.set_active(first);
        assert_eq!(app.workspace.active_editor().unwrap().text(), "a\nb");
        app.workspace.open_settings();
        open(&mut app, Menu::Ending);
        assert!(app.editor.editing.menu.is_none());
    }
}
