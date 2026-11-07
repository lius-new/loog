//! Vim through the real event path: focus, the keymap's capture listener,
//! key events, and text input that the platform only sends for keys no
//! handler prevented.
use crate::app::context::AppContext;

use std::sync::{Arc, Mutex};

use lgui::application::{AppView, ApplicationContext};
use lgui::core::{
    ImeEvent, InputEvent, KeyModifiers, KeyState, KeyboardEvent, LogicalKey, NamedKey, Point,
    PointerButton, PointerData, SemanticRole, UiScale, dispatch_runtime_output,
};
use lgui::prelude::{UiRect, group};
use lgui::session::UiSession;

use crate::app::state::AppState;
use crate::features::git::GitStoreSnapshot;
use crate::model::pane_layout::PaneId;
use crate::ui::theme;
use loog_editor_core::vim::Mode;

struct Ui {
    tasks: crate::app::test_support::UiTasks,
    session: UiSession,
    view: AppView,
    app: ApplicationContext,
    state: AppContext,
    viewport: UiRect,
}

impl Ui {
    fn new(contents: &str) -> Self {
        Self::new_with_home(contents, None)
    }

    fn new_with_home(contents: &str, workspace_home: Option<bool>) -> Self {
        Self::create(contents, workspace_home, |_| {})
    }
    fn configured(contents: &str, setup: impl Fn(&mut AppState) + Send + Sync + 'static) -> Self {
        Self::create(contents, None, setup)
    }
    fn code(contents: &str, setup: impl Fn(&mut AppState) + Send + Sync + 'static) -> Self {
        let source = contents.to_owned();
        Self::configured(contents, move |app| {
            app.workspace
                .open_path("vim-features.rs".into(), source.clone());
            setup(app);
        })
    }
    fn create(
        contents: &str,
        workspace_home: Option<bool>,
        setup: impl Fn(&mut AppState) + Send + Sync + 'static,
    ) -> Self {
        let exposed = Arc::new(Mutex::new(None::<AppContext>));
        let output = exposed.clone();
        let viewport = UiRect::new(0.0, 0.0, 600.0, 300.0);
        let app = ApplicationContext::empty(Default::default());
        let view_app = app.clone();
        let tasks = crate::app::test_support::UiTasks::default();
        let runtime = Arc::new(crate::app::runtime::AppRuntime::default());
        runtime.set_application(tasks.application());
        let contents = contents.to_owned();
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state({
                let mut app = AppState::new();
                if let Some(project) = workspace_home {
                    if project {
                        app.explorer
                            .model
                            .restore(vec!["focus-test-project".into()], Vec::new());
                    }
                } else {
                    app.workspace.open_path("vim.txt".into(), contents.clone());
                    app.editor.vim.options.enabled = true;
                }
                setup(&mut app);
                crate::app::editing::configure_all(&mut app);
                app
            }))
            .with_runtime(&runtime);
            let git_store = cx.state(GitStoreSnapshot::default());
            let terminals = crate::app::terminal_binding::test_store(
                cx,
                Some(state.clone()),
                crate::app::terminal_binding::TerminalTabs::new,
            );
            *output.lock().unwrap() = Some(state.clone());
            let id = cx.use_stable_id();
            let focus = cx.focus_handle(id.clone());
            let terminal_id = cx.use_stable_id();
            let terminal_focus = cx.focus_handle(terminal_id);
            let env = crate::app::key_actions::KeyEnv {
                state: state.clone(),
                git_store: git_store.clone(),
                application: view_app.clone(),
                editor_focus: focus.clone(),
                editor_rect: viewport,
                terminal: None,
            };
            let surface = if state.get().workspace.active_editor().is_some() {
                crate::ui::editor::view::render(
                    PaneId::new(1),
                    viewport,
                    crate::app::editor_view::binding(
                        state.clone(),
                        git_store,
                        PaneId::new(1),
                        viewport,
                    ),
                    id,
                    focus,
                )
            } else if workspace_home == Some(true) {
                crate::app::chrome::workspace_home(
                    viewport,
                    state.clone(),
                    id,
                    focus,
                    terminal_focus,
                    terminals,
                )
            } else {
                crate::app::chrome::welcome(viewport, state.clone(), id, focus)
            };
            crate::app::key_actions::attach(group(viewport).child(surface), env)
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let state = exposed.lock().unwrap().clone().unwrap();
        let mut ui = Self {
            tasks,
            session,
            view,
            app,
            state,
            viewport,
        };
        // Focus the editor with a click at the start of the text.
        let lines = ui
            .state
            .get()
            .workspace
            .active_editor()
            .map_or(1, |buffer| buffer.line_count());
        let point = PointerData::mouse(Point::new(
            crate::ui::fonts::editor().gutter_w_for_lines(lines) + theme::CODE_PAD,
            5.0,
        ));
        ui.send(InputEvent::PointerDown {
            pointer: point,
            button: PointerButton::Left,
        });
        ui.send(InputEvent::PointerUp {
            pointer: point,
            button: PointerButton::Left,
        });
        ui
    }

    /// Dispatch one input event; returns whether a handler prevented its
    /// default action.
    fn send(&mut self, input: InputEvent) -> bool {
        let events = self.session.handle_input(input);
        let mut prevented = false;
        let session = &mut self.session;
        dispatch_runtime_output(
            events,
            &self.app,
            &lgui::window::WindowId::new("vim-test"),
            |action| session.handle_default_action(action),
            |cx| prevented |= cx.default_prevented(),
        );
        self.session
            .render_view(&self.view, self.viewport, UiScale::ONE);
        prevented
    }

    /// Press a key the way the platform reports it: the key event, then its
    /// text unless a handler prevented the key's default action.
    fn press(&mut self, key: LogicalKey, modifiers: KeyModifiers) {
        let text = match &key {
            LogicalKey::Character(text) if !modifiers.ctrl() && !modifiers.meta() => {
                Some(text.to_string())
            }
            _ => None,
        };
        let prevented = self.send(InputEvent::Keyboard(KeyboardEvent {
            state: KeyState::Down,
            key,
            modifiers,
            ..Default::default()
        }));
        if !prevented && let Some(text) = text {
            self.send(InputEvent::TextInput(text));
        }
    }

    /// Type Vim notation: characters, `<Esc>`, `<CR>`, `<BS>` and `<C-x>`.
    fn keys(&mut self, keys: &str) {
        for key in loog_editor_core::vim::Key::parse(keys) {
            use loog_editor_core::vim::Key;
            let (logical, modifiers) = match key {
                Key::Char(c) => {
                    let shift = c.is_uppercase();
                    (
                        LogicalKey::Character(c.to_string()),
                        if shift {
                            KeyModifiers::SHIFT
                        } else {
                            KeyModifiers::empty()
                        },
                    )
                }
                Key::Ctrl(c) => (LogicalKey::Character(c.to_string()), KeyModifiers::CONTROL),
                Key::Esc => (LogicalKey::Named(NamedKey::Escape), KeyModifiers::empty()),
                Key::Enter => (LogicalKey::Named(NamedKey::Enter), KeyModifiers::empty()),
                Key::Backspace => (
                    LogicalKey::Named(NamedKey::Backspace),
                    KeyModifiers::empty(),
                ),
                Key::Tab => (LogicalKey::Named(NamedKey::Tab), KeyModifiers::empty()),
                Key::Left => (
                    LogicalKey::Named(NamedKey::ArrowLeft),
                    KeyModifiers::empty(),
                ),
                other => panic!("no key event for {other:?}"),
            };
            self.press(logical, modifiers);
        }
    }

    fn undo(&mut self) {
        let modifiers = if cfg!(target_os = "macos") {
            KeyModifiers::META
        } else {
            KeyModifiers::CONTROL
        };
        self.press(LogicalKey::Character("z".into()), modifiers);
    }

    fn text(&self) -> String {
        self.state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .text()
            .to_owned()
    }

    fn cursor(&self) -> usize {
        self.state.get().workspace.active_editor().unwrap().cursor()
    }

    fn mode(&self) -> Mode {
        self.state.get().editor.vim.mode()
    }

    /// The editor's semantic role, which decides whether the platform input
    /// method is enabled.
    fn role(&self) -> SemanticRole {
        let tree = self.session.tree();
        let focused = self
            .session
            .runtime()
            .interaction_state()
            .focused
            .clone()
            .expect("the editor has focus");
        tree.node(&focused)
            .and_then(|node| node.semantics.as_ref())
            .map(|semantics| semantics.role)
            .expect("the editor has semantics")
    }
}

#[test]
fn first_file_inherits_focus_from_welcome() {
    first_file_inherits_focus(false);
}

#[test]
fn first_file_inherits_focus_from_workspace_home() {
    first_file_inherits_focus(true);
}

fn first_file_inherits_focus(project: bool) {
    let mut ui = Ui::new_with_home("", Some(project));
    let focused = ui.session.runtime().interaction_state().focused.clone();
    assert!(
        focused.is_some(),
        "the home surface owns focus before opening a file"
    );
    ui.state.update(|app| {
        app.workspace.open_path("first.txt".into(), "abc".into());
    });
    ui.session.render_view(&ui.view, ui.viewport, UiScale::ONE);
    assert_eq!(ui.session.runtime().interaction_state().focused, focused);
    assert!(
        ui.state.get().editor.focused,
        "the first editor must draw its caret and accept editor keys"
    );
    ui.press(
        LogicalKey::Named(NamedKey::ArrowRight),
        KeyModifiers::empty(),
    );
    assert_eq!(ui.cursor(), 1);
    ui.send(InputEvent::TextInput("X".into()));
    assert_eq!(ui.text(), "aXbc");
}

#[test]
fn commands_do_not_type_and_insert_mode_does() {
    let mut ui = Ui::new("hello world");
    assert_eq!(ui.mode(), Mode::Normal);
    assert_eq!(
        ui.role(),
        SemanticRole::Group,
        "no input method in Normal mode"
    );
    ui.keys("w");
    assert_eq!((ui.text().as_str(), ui.cursor()), ("hello world", 6));
    ui.keys("i");
    assert_eq!(ui.mode(), Mode::Insert);
    assert_eq!(ui.role(), SemanticRole::TextInput);
    assert_eq!(ui.text(), "hello world", "the i itself is not typed");
    ui.keys("big ");
    assert_eq!(ui.text(), "hello big world");
    ui.keys("<Esc>");
    assert_eq!((ui.mode(), ui.cursor()), (Mode::Normal, 9));
    ui.keys("u");
    assert_eq!(ui.text(), "hello world", "the insert is one undo step");
    ui.keys("<C-r>");
    assert_eq!(ui.text(), "hello big world");
    ui.keys("0dw");
    assert_eq!(ui.text(), "big world");
    ui.keys("x.");
    assert_eq!(ui.text(), "g world");
}

#[test]
fn insert_mode_keys_go_to_vim_and_are_repeated() {
    let mut ui = Ui::new("a\nb");
    ui.keys("A12<BS>3<CR>x<Esc>");
    assert_eq!(ui.text(), "a13\nx\nb");
    ui.keys("j.");
    assert_eq!(ui.text(), "a13\nx\nb13\nx");
    ui.keys("uu");
    assert_eq!(ui.text(), "a\nb");
}

#[test]
fn input_method_text_in_insert_mode_then_normal_mode_commands() {
    let mut ui = Ui::new("end");
    ui.keys("i");
    ui.send(InputEvent::Ime(ImeEvent::Preedit {
        text: "nihao".into(),
        cursor: Some(0..5),
    }));
    // Keys belong to the composition while it shows text.
    ui.keys("<Esc>");
    assert_eq!(ui.mode(), Mode::Insert);
    ui.send(InputEvent::Ime(ImeEvent::Preedit {
        text: String::new(),
        cursor: None,
    }));
    ui.send(InputEvent::Ime(ImeEvent::Commit("你好".into())));
    assert_eq!(ui.text(), "你好end");
    ui.keys("<Esc>");
    assert_eq!(ui.mode(), Mode::Normal);
    ui.keys("x");
    assert_eq!(
        ui.text(),
        "你end",
        "no character was lost or typed as a command"
    );
    ui.keys("u");
    assert_eq!(ui.text(), "你好end");
    ui.keys("u");
    assert_eq!(ui.text(), "end");
}

#[test]
fn application_shortcuts_and_vim_keys_share_the_keyboard() {
    let mut ui = Ui::new("one\ntwo");
    // Ctrl+V starts a visual block instead of pasting.
    ui.keys("<C-v>j");
    assert_eq!(
        ui.mode(),
        Mode::Visual(loog_editor_core::vim::VisualKind::Block)
    );
    ui.keys("d");
    assert_eq!(ui.text(), "ne\nwo");
    // The platform's undo shortcut still reaches the application.
    ui.undo();
    assert_eq!(ui.text(), "one\ntwo");
    assert_eq!(ui.mode(), Mode::Normal);
}

#[test]
fn turning_vim_off_restores_default_editing() {
    let mut ui = Ui::new("text");
    ui.keys("A!");
    assert_eq!(ui.mode(), Mode::Insert);
    ui.state
        .update(|app| crate::app::editor_vim::set_enabled(app, false));
    ui.session.render_view(&ui.view, ui.viewport, UiScale::ONE);
    assert_eq!(ui.role(), SemanticRole::TextInput);
    ui.keys("jk");
    assert_eq!(ui.text(), "text!jk");
    ui.keys("<Esc>");
    // Default editing: the undo shortcut undoes typing, and the insert session's
    // undo step was closed when Vim turned off.
    ui.undo();
    assert_eq!(ui.text(), "text!");
}

impl Ui {
    /// The painted caret rect and the motion offset still applied to it.
    fn caret(&self) -> (UiRect, (f32, f32)) {
        let tree = self.session.tree();
        let node = tree.node(&lgui::core::UiId::new("editor-caret-1")).unwrap();
        let layer = tree.node(node.parent.as_ref().unwrap()).unwrap();
        let transform = layer.compositing_layer.unwrap().transform;
        (
            node.layout_rect,
            (transform.translation_x(), transform.translation_y()),
        )
    }

    fn advance(&mut self, elapsed_ms: f32) {
        self.session.advance(elapsed_ms);
        self.session
            .render_view(&self.view, self.viewport, UiScale::ONE);
    }

    fn settle(&mut self) {
        self.session.advance(0.0);
        self.advance(100.0);
        assert_eq!(self.caret().1, (0.0, 0.0));
    }
}

#[test]
fn smooth_caret_animates_every_vim_shape_without_sliding_on_mode_switches() {
    let mut ui = Ui::new("hello world\nsecond line");
    ui.settle();
    let (start, _) = ui.caret();
    assert!(start.width() > 2.0, "Normal mode paints a block");

    ui.keys("w");
    let (target, offset) = ui.caret();
    assert!(target.left > start.left);
    assert_eq!(
        target.left + offset.0,
        start.left,
        "motions retarget without jumping"
    );
    ui.session.advance(0.0);
    ui.advance(40.0);
    let (_, offset) = ui.caret();
    assert!(offset.0 < 0.0 && target.left + offset.0 > start.left);
    ui.settle();

    ui.keys("i");
    let (bar, offset) = ui.caret();
    assert_eq!(offset, (0.0, 0.0), "a shape change alone does not move");
    assert_eq!((bar.left, bar.width()), (target.left, 2.0));
    assert!(bar.top > target.top);

    ui.keys("<Esc>");
    let (block, offset) = ui.caret();
    assert!(block.left < bar.left && block.width() > 2.0);
    assert_eq!(offset.1, 0.0, "switching shapes never slides vertically");
    assert_eq!(
        block.left + offset.0,
        bar.left,
        "leaving Insert mode animates the step left"
    );
    ui.settle();

    ui.keys("vl");
    assert_eq!(
        ui.caret().1,
        (0.0, 0.0),
        "the caret stays on the edge of a Visual selection"
    );
    ui.keys("<Esc>");
    ui.settle();

    ui.state
        .update(|app| app.preferences.values_mut().smooth_caret = false);
    ui.keys("j");
    assert_eq!(ui.caret().1, (0.0, 0.0), "disabled motion snaps");
}

impl Ui {
    fn redraw(&mut self) {
        self.session
            .render_view(&self.view, self.viewport, UiScale::ONE);
    }
    fn selections(&self) -> Vec<loog_editor_core::text::Selection> {
        self.state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .selections()
    }
}

#[test]
fn vim_editor_pairs_wrap_selections_skip_closers_and_keep_ime_literal() {
    use loog_editor_core::text::Selection;
    let mut ui = Ui::code("", |_| {});
    ui.keys("i(");
    assert_eq!(ui.text(), "()");
    assert_eq!(ui.cursor(), 1);
    ui.keys("<BS>[{\"");
    assert_eq!(ui.text(), "[{\"\"}]");
    ui.keys("\"}]");
    assert_eq!(ui.text(), "[{\"\"}]");
    assert_eq!(ui.cursor(), 6);
    ui.send(InputEvent::Ime(ImeEvent::Preedit {
        text: "symbols".into(),
        cursor: None,
    }));
    ui.send(InputEvent::Ime(ImeEvent::Commit("(中文".into())));
    assert_eq!(ui.text(), "[{\"\"}](中文");
    ui.keys("<Esc>u");
    assert_eq!(ui.text(), "");
    let mut ui = Ui::code("hello", |_| {});
    ui.keys("i");
    ui.state.update(|app| {
        app.workspace
            .active_editor_mut()
            .unwrap()
            .set_selection(Selection::spanning(0, 5))
    });
    ui.keys("(");
    assert_eq!(ui.text(), "(hello)");
    assert_eq!(ui.selections()[0].range(), Some(1..6));
    let mut ui = Ui::code("", |app| {
        app.preferences.values_mut().editor.auto_pairs = false
    });
    ui.keys("i(");
    assert_eq!(ui.text(), "(");
}

#[test]
fn vim_editor_tab_smart_enter_line_endings_and_write_share_preferences() {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "loog-vim-features-{}-{}.rs",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let target = path.clone();
    let mut ui = Ui::configured("", move |app| {
        app.workspace.open_path(target.clone(), String::new());
        let prefs = &mut app.preferences.values_mut().editor;
        prefs.use_tabs = true;
        prefs.indent_size = 4;
        prefs.tab_width = 8;
        prefs.new_file_line_ending = crate::features::settings::editing::Ending::Crlf;
    });
    ui.keys("i{<CR>x<Esc>");
    assert_eq!(ui.text(), "{\r\n\tx\r\n}");
    assert_eq!(
        ui.state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .options()
            .tab_width,
        8
    );
    ui.keys(">>");
    assert_eq!(ui.text(), "{\r\n\t\tx\r\n}");
    ui.keys("<<");
    assert_eq!(ui.text(), "{\r\n\tx\r\n}");
    ui.keys(":w<CR>");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while ui.state.get().editor.saving.pending > 0 {
        ui.tasks.run_all();
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(std::fs::read(&path).unwrap(), ui.text().as_bytes());
    ui.keys("i");
    ui.state.update(|app| {
        crate::app::editing::open(app, crate::app::editing::Menu::Ending);
        crate::app::editing::choose(app, 0);
    });
    assert_eq!(ui.text(), "{\n\tx\n}");
    ui.keys("YZ<Esc>u");
    assert_eq!(ui.text(), "{\n\tx\n}");
    ui.keys("u");
    assert_eq!(ui.text(), "{\r\n\tx\r\n}");
    let _ = std::fs::remove_file(path);
}

#[test]
fn vim_editor_line_commands_keep_visual_rows_and_separate_insert_undo() {
    let mut ui = Ui::code("one\ntwo\nthree", |app| {
        app.preferences.values_mut().editor.indent_size = 4
    });
    ui.keys("iX");
    ui.press(LogicalKey::Named(NamedKey::ArrowDown), KeyModifiers::ALT);
    assert_eq!(ui.text(), "two\nXone\nthree");
    assert_eq!(ui.mode(), Mode::Insert);
    ui.undo();
    assert_eq!(ui.text(), "Xone\ntwo\nthree");
    ui.keys("Y<Esc>u");
    assert_eq!(ui.text(), "Xone\ntwo\nthree");
    ui.keys("u");
    assert_eq!(ui.text(), "one\ntwo\nthree");
    ui.keys("ggVj");
    let secondary = if cfg!(target_os = "macos") {
        KeyModifiers::META
    } else {
        KeyModifiers::CONTROL
    };
    ui.press(LogicalKey::Character("/".into()), secondary);
    assert_eq!(ui.text(), "// one\n// two\nthree");
    ui.keys("<Esc>u");
    assert_eq!(ui.text(), "one\ntwo\nthree");
    ui.keys("ggVj");
    ui.press(
        LogicalKey::Named(NamedKey::ArrowDown),
        KeyModifiers::ALT | KeyModifiers::SHIFT,
    );
    assert_eq!(ui.text(), "one\ntwo\none\ntwo\nthree");
    ui.undo();
    assert_eq!(ui.text(), "one\ntwo\nthree");
}

#[test]
fn vim_editor_multiple_cursors_and_visual_blocks_apply_pairs_ime_and_prefix_delete() {
    let mut ui = Ui::code("one\none", |_| {});
    ui.press(
        LogicalKey::Named(NamedKey::ArrowDown),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    );
    assert_eq!(ui.mode(), Mode::Insert);
    assert_eq!(ui.selections().len(), 2);
    ui.press(LogicalKey::Named(NamedKey::End), KeyModifiers::empty());
    ui.keys("(");
    assert_eq!(ui.text(), "one()\none()");
    ui.send(InputEvent::Ime(ImeEvent::Preedit {
        text: "zhong".into(),
        cursor: None,
    }));
    ui.send(InputEvent::Ime(ImeEvent::Commit("中文(".into())));
    assert_eq!(ui.text(), "one(中文()\none(中文()");
    ui.undo();
    assert_eq!(ui.text(), "one\none");
    assert_eq!(ui.selections().len(), 2);
    ui.keys("x<C-u>");
    assert_eq!(ui.text(), "\n");
    ui.keys("<Esc>");
    assert_eq!(ui.mode(), Mode::Normal);
    assert_eq!(ui.selections().len(), 1);
    let mut ui = Ui::code("\n\n", |_| {});
    ui.keys("<C-v>jjI(");
    assert_eq!(ui.text(), "()\n()\n()");
    assert_eq!(ui.selections().len(), 3);
    ui.keys("<BS>");
    assert_eq!(ui.text(), "\n\n");
    ui.keys("<Esc>");
    assert_eq!(ui.selections().len(), 1);
}

#[test]
fn vim_editor_display_motions_folding_and_reading_aids_use_the_shared_map() {
    let source = format!("{}\nnext", "one two three four ".repeat(60));
    let mut ui = Ui::code(&source, |app| {
        app.preferences.values_mut().editor.soft_wrap = true;
        app.preferences.values_mut().editor.show_whitespace = true;
    });
    let map = crate::app::editing::active_projection(&ui.state.get(), ui.viewport);
    assert!(map.wrapped());
    ui.keys("gj");
    let app = ui.state.get();
    let editor = app.workspace.active_editor().unwrap();
    assert_eq!(editor.line_col().0, 0);
    assert_eq!(
        map.caret_row(
            &editor,
            editor.cursor(),
            editor.selection_state().wrap_affinity
        ),
        1
    );
    drop(app);
    ui.keys("gk");
    assert_eq!(ui.cursor(), 0);
    ui.keys("j");
    assert_eq!(
        ui.state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .line_col()
            .0,
        1
    );
    ui.keys("k<C-d>");
    assert_eq!(
        ui.state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .line_col()
            .0,
        0
    );
    assert!(ui.state.get().workspace.active_scroll().1 > 0.0);
    let mut ui = Ui::code("fn f() {\n    body();\n}\ntail", |app| {
        let id = app.workspace.active_document().unwrap();
        let doc = &app.workspace.documents[&id];
        let stamp = doc.stamp(id);
        let range = 0..doc.buffer.line_start(3);
        crate::app::folding::publish_syntax(app, stamp, Some(Arc::from([range])));
        app.preferences.values_mut().editor.show_whitespace = true;
    });
    ui.keys("zc");
    assert!(crate::app::editing::active_projection(&ui.state.get(), ui.viewport).folded());
    ui.keys("j");
    assert_eq!(
        ui.state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .line_col()
            .0,
        3
    );
    assert!(crate::app::editing::active_projection(&ui.state.get(), ui.viewport).folded());
    ui.keys("kzo");
    assert!(!crate::app::editing::active_projection(&ui.state.get(), ui.viewport).folded());
    ui.keys("zMzR");
    assert!(!crate::app::editing::active_projection(&ui.state.get(), ui.viewport).folded());
    ui.keys("f(");
    ui.redraw();
    assert!(
        ui.session.tree().nodes().iter().any(|n| n
            .semantics
            .as_ref()
            .and_then(|s| s.name.as_deref())
            == Some("Matching bracket"))
    );
    assert!(
        ui.state
            .get()
            .workspace
            .active_editor()
            .unwrap()
            .options()
            .indent_guides
    );
}

#[test]
fn vim_write_and_quit_waits_for_disk_and_keeps_the_tab_on_failure() {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    for case in 0..3 {
        let fail = case == 1;
        let root = std::env::temp_dir().join(format!(
            "loog-vim-wq-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = if fail {
            root.join("missing/file.txt")
        } else {
            root.join("file.txt")
        };
        let target = path.clone();
        let mut ui = Ui::configured("", move |app| {
            app.workspace.open_path(target.clone(), "before".into());
        });
        let id = ui.state.get().workspace.active_document().unwrap();
        let (release, receive) = std::sync::mpsc::channel::<()>();
        ui.state
            .services()
            .unwrap()
            .save(Box::new(move || {
                let _ = receive.recv();
            }))
            .unwrap();
        ui.keys("A!<Esc>:wq<CR>");
        assert_eq!(ui.state.get().editor.saving.pending, 1);
        assert!(ui.state.get().workspace.documents.contains_key(&id));
        let switched = if case == 2 {
            ui.state.update(|app| {
                app.workspace
                    .split(
                        app.workspace.active_pane(),
                        crate::domain::pane_layout::Direction::Right,
                    )
                    .unwrap();
            });
            ui.state.get().workspace.active()
        } else {
            None
        };
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while ui.state.get().editor.saving.pending > 0 {
            ui.tasks.run_all();
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(
            ui.state.get().workspace.documents.contains_key(&id),
            fail || switched.is_some()
        );
        if let Some(view) = switched {
            assert_eq!(
                ui.state.get().workspace.active(),
                Some(view),
                "late :wq must not close a different view of the same document"
            );
        }
        if fail {
            assert!(ui.state.get().workspace.is_dirty(id));
        } else {
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "before!");
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
