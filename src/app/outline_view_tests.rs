//! The Outline drawer page, driven through a real UI session.
use super::{context::AppContext, state::AppState};
use crate::ui::drawer::DrawerMode;
use lgui::{
    application::{AppView, ApplicationContext},
    core::{InputEvent, Point, PointerButton, PointerData, UiScale},
    prelude::UiRect,
    session::UiSession,
};
use std::sync::{Arc, Mutex};

const TEXT: &str = "# One\n\nintro\n\n## Two\n\nbody\n\n## Three\n";

struct Drawer {
    session: UiSession,
    view: AppView,
    state: AppContext,
    context: ApplicationContext,
    rect: UiRect,
}

impl Drawer {
    fn new(name: &str, text: &str) -> Self {
        let rect = UiRect::new(0.0, 0.0, 300.0, 400.0);
        let mut app = AppState::new();
        app.workspace.open_path(name.into(), text.into());
        app.drawer.visible = true;
        app.drawer.mode = DrawerMode::Outline;
        let initial = Arc::new(Mutex::new(Some(app)));
        let exposed = Arc::new(Mutex::new(None));
        let output = exposed.clone();
        let view: AppView = Arc::new(move |cx| {
            let state =
                AppContext::detached(cx.state_with(|| initial.lock().unwrap().take().unwrap()));
            *output.lock().unwrap() = Some(state.clone());
            super::outline_view::render(
                cx.viewport(),
                &state,
                UiRect::new(300.0, 0.0, 900.0, 400.0),
            )
        });
        let mut session = UiSession::new();
        session.render_view(&view, rect, UiScale::ONE);
        let state = exposed.lock().unwrap().clone().unwrap();
        let mut drawer = Self {
            session,
            view,
            state,
            context: ApplicationContext::empty(Default::default()),
            rect,
        };
        drawer.state.update(|_| {});
        drawer.render();
        drawer
    }

    fn render(&mut self) {
        self.session
            .render_view(&self.view, self.rect, UiScale::ONE);
    }

    fn row(&self, name: &str) -> Option<UiRect> {
        self.session
            .tree()
            .nodes()
            .iter()
            .find(|node| node.semantics.as_ref().and_then(|s| s.name.as_deref()) == Some(name))
            .map(|node| node.layout_rect)
    }

    fn click(&mut self, point: Point) {
        for event in [
            InputEvent::PointerDown {
                pointer: PointerData::mouse(point),
                button: PointerButton::Left,
            },
            InputEvent::PointerUp {
                pointer: PointerData::mouse(point),
                button: PointerButton::Left,
            },
        ] {
            let output = self.session.handle_input(event);
            lgui::core::dispatch_runtime_output(
                output,
                &self.context,
                &lgui::window::WindowId::new("outline-test"),
                |action| self.session.handle_default_action(action),
                |_| {},
            );
            self.render();
        }
    }
}

fn names(drawer: &Drawer) -> Vec<String> {
    ["One", "Two", "Three"]
        .into_iter()
        .filter(|name| drawer.row(name).is_some())
        .map(str::to_owned)
        .collect()
}

#[test]
fn markdown_headings_are_listed_and_clicking_one_moves_the_caret_to_it() {
    let mut drawer = Drawer::new("outline.md", TEXT);
    assert_eq!(names(&drawer), ["One", "Two", "Three"]);
    let two = drawer.row("Two").unwrap();
    drawer.click(Point::new(two.left + 60.0, (two.top + two.bottom) / 2.0));
    let app = drawer.state.get();
    let cursor = app.workspace.active_editor().unwrap().cursor();
    assert_eq!(cursor, TEXT.find("Two").unwrap());
}

#[test]
fn the_row_holding_the_caret_is_marked_and_collapsing_hides_children() {
    let mut drawer = Drawer::new("follow.md", TEXT);
    drawer.state.update(|app| {
        let at = TEXT.find("body").unwrap();
        app.workspace.active_editor_mut().unwrap().set_cursor(at);
    });
    drawer.render();
    let app = drawer.state.get();
    let ui = &app.editor.language.outline;
    let current = ui.current.clone().unwrap();
    assert!(current.ends_with("Heading(2):Two"), "{current}");
    drop(app);

    drawer
        .state
        .update(|app| app.editor.language.outline.collapse_all());
    drawer.render();
    assert_eq!(names(&drawer), ["One"]);
    let app = drawer.state.get();
    // The caret's row is hidden inside "One", which stands in for it.
    assert!(
        app.editor
            .language
            .outline
            .current
            .as_deref()
            .unwrap()
            .ends_with("Heading(1):One")
    );
}

#[test]
fn files_without_a_server_say_why_the_tree_is_empty() {
    let drawer = Drawer::new("plain.txt", "just text\n");
    let app = drawer.state.get();
    assert!(app.editor.language.outline.outline.is_empty());
    assert!(
        drawer
            .session
            .tree()
            .nodes()
            .iter()
            .filter_map(|node| node.text.as_deref())
            .any(|line| line.contains("structure") || line.contains("symbols"))
    );
}

#[test]
fn switching_to_another_file_replaces_the_tree() {
    let mut drawer = Drawer::new("a.md", TEXT);
    drawer.state.update(|app| {
        app.workspace
            .open_path("b.md".into(), "# Only\n".to_owned());
    });
    drawer.render();
    assert!(drawer.row("Only").is_some());
    assert_eq!(names(&drawer), Vec::<String>::new());
}

/// Writes a picture of a varied tree when `LOOG_OUTLINE_PREVIEW_DIR` is set.
#[test]
fn outline_preview_picture() {
    use crate::features::outline::{Item, Kind, Outline};
    use crate::ui::outline::view::{Body, Commands, Frame};
    use lgui::renderer::{FrameInfo, FrameReason};
    let Some(directory) = std::env::var_os("LOOG_OUTLINE_PREVIEW_DIR") else {
        return;
    };
    let item =
        |name: &str, kind: Kind, detail: Option<&str>, at: usize, children: Vec<Item>| Item {
            name: name.to_owned(),
            detail: detail.map(str::to_owned),
            kind,
            deprecated: name == "legacy",
            full: at..at + 100,
            select: at..at,
            children,
        };
    let outline = Outline::new(vec![
        item(
            "workspace",
            Kind::Module,
            None,
            0,
            vec![
                item(
                    "Workspace",
                    Kind::Struct,
                    None,
                    1,
                    vec![
                        item(
                            "documents",
                            Kind::Field,
                            Some("HashMap<FileId, Document>"),
                            2,
                            vec![],
                        ),
                        item(
                            "open_path",
                            Kind::Method,
                            Some("(&mut self, path: PathBuf)"),
                            3,
                            vec![],
                        ),
                        item("legacy", Kind::Method, Some("()"), 4, vec![]),
                    ],
                ),
                item(
                    "Visibility",
                    Kind::Enum,
                    None,
                    5,
                    vec![
                        item("Git", Kind::EnumMember, None, 6, vec![]),
                        item("Files", Kind::EnumMember, None, 7, vec![]),
                    ],
                ),
                item("Layout", Kind::Interface, None, 8, vec![]),
                item("MAX_TABS", Kind::Constant, Some("usize"), 9, vec![]),
                item(
                    "restore",
                    Kind::Function,
                    Some("(session: Session) -> Workspace"),
                    10,
                    vec![],
                ),
            ],
        ),
        item(
            "Introduction",
            Kind::Heading(1),
            None,
            200,
            vec![item("Install", Kind::Heading(2), None, 201, vec![])],
        ),
    ]);
    let collapsed = std::collections::HashSet::new();
    let rows = outline.rows(&collapsed, false);
    let current = outline.node(3).map(|node| node.key.clone());
    let rect = UiRect::new(0.0, 0.0, 300.0, 330.0);
    let context = ApplicationContext::empty(Default::default());
    context
        .resources()
        .provide(lgui::icons::IconRegistration(Arc::new(
            crate::app::svg_icons(),
        )));
    let _fonts = lgui::backend::install_text_environment(
        &context,
        lgui::render_skia::skia_text_system_handle(),
    );
    let view: AppView = Arc::new(move |cx| {
        let header = super::explorer_view::drawer_header(
            UiRect::new(0.0, 0.0, 300.0, crate::ui::theme::TABS_H),
            &AppContext::detached(cx.state_with(|| {
                let mut app = AppState::new();
                app.drawer.mode = DrawerMode::Outline;
                app
            })),
        );
        crate::ui::outline::view::render(
            rect,
            &Frame {
                body: Body::Tree {
                    outline: &outline,
                    rows: &rows,
                    current: current.as_deref(),
                    reveal: None,
                },
                sorted: false,
                scroll: 0.0,
                hovered: Some(1),
                pointer_inside: true,
                scrollbar_grab: None,
                resizable: false,
                resizing: false,
            },
            Commands::new(|_| {}),
            header,
        )
    });
    let mut session = UiSession::new();
    session.render_view(&view, rect, UiScale::ONE);
    let physical = lgui::core::PhysicalRect::new(0, 0, 300, 330);
    let frame = FrameInfo::new(physical, &[], UiScale::ONE, FrameReason::SceneChange, true);
    let mut surface = lgui::render_skia::SkiaSoftwareSurface::new(8 * 1024 * 1024);
    lgui::assets::backend::with_render_resources(
        &context,
        lgui::assets::backend::render_resources(),
        || {
            surface.draw(
                &session.tree().scene().project_to_physical(UiScale::ONE),
                &frame,
            )
        },
    )
    .unwrap();
    let mut rgba = surface.pixels().to_vec();
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    image::save_buffer(
        std::path::Path::new(&directory).join("outline.png"),
        &rgba,
        300,
        330,
        image::ColorType::Rgba8,
    )
    .unwrap();
}
