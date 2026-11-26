use super::*;
use crate::app::terminal_binding::{TerminalTabs, test_store};
use crate::domain::application_layout::ApplicationLayout;
use lgui::application::{AppView, ApplicationContext};
use lgui::core::{
    InputEvent, KeyboardEvent, LogicalKey, NamedKey, Point, PointerButton, PointerData, UiId,
    UiScale, dispatch_runtime_output,
};
use lgui::prelude::{component, group};
use lgui::session::UiSession;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

type Exposed = Arc<Mutex<Option<(AppContext, TerminalStore, DragStore)>>>;

struct Harness {
    view: AppView,
    viewport: UiRect,
    source: UiRect,
    session: UiSession,
    context: ApplicationContext,
    state: AppContext,
    tabs: TerminalStore,
    drag: DragStore,
    renders: Arc<AtomicUsize>,
}

impl Harness {
    fn new() -> Self {
        let viewport = UiRect::new(0.0, 0.0, 1200.0, 800.0);
        let source = UiRect::new(0.0, 30.0, 1200.0, 776.0);
        let exposed: Exposed = Arc::new(Mutex::new(None));
        let output = exposed.clone();
        let renders = Arc::new(AtomicUsize::new(0));
        let counted = renders.clone();
        let view: AppView = Arc::new(move |_| {
            let output = output.clone();
            let counted = counted.clone();
            component(viewport, move |cx, _| {
                counted.fetch_add(1, Ordering::Relaxed);
                let state = AppContext::detached(cx.state_with(|| {
                    let mut app = crate::app::state::AppState::new();
                    app.source_control.visible = true;
                    app.drawer.visible = true;
                    app.terminal_chrome.show_terminal = true;
                    app.layout.visibility_popup = Some(false);
                    let mut layout = ApplicationLayout::initial(source.into(), 220.0, 220.0, 240.0);
                    layout.sync(&[(1, 1), (2, 1)], Some(1));
                    app.layout.application_layout = Some(layout);
                    app
                }));
                let tabs = test_store(cx, None, || {
                    let mut tabs = TerminalTabs::new();
                    tabs.split(1);
                    tabs
                });
                let pointer = cx.signal(Point::new(0.0, 0.0));
                let drag = cx.state_with(|| DragStore::new(pointer)).get();
                *output.lock().unwrap() = Some((state.clone(), tabs.clone(), drag.clone()));
                let focus = (
                    cx.focus_handle(UiId::new("drag-editor")),
                    cx.focus_handle(UiId::new("drag-terminal")),
                    tabs.clone(),
                );
                let binding = binding(&state, &tabs, &drag, source, focus);
                let anchor =
                    crate::ui::titlebar::layout_toggle_rect(UiRect::new(0.0, 0.0, 1200.0, 30.0));
                crate::ui::layout_visibility::attach(
                    group(viewport),
                    viewport,
                    anchor,
                    binding.clone(),
                )
                .child(crate::ui::layout_visibility::render(
                    viewport, anchor, binding,
                ))
            })
            .key("layout-drag-test-app")
        });
        let context = ApplicationContext::empty(Default::default());
        context
            .resources()
            .provide(lgui::icons::IconRegistration(Arc::new(
                crate::app::svg_icons(),
            )));
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        session.runtime().run_effects();
        let (state, tabs, drag) = exposed.lock().unwrap().clone().unwrap();
        Self {
            view,
            viewport,
            source,
            session,
            context,
            state,
            tabs,
            drag,
            renders,
        }
    }

    fn render(&mut self) {
        self.session
            .render_view(&self.view, self.viewport, UiScale::ONE);
        self.session.runtime().run_effects();
    }

    fn send(&mut self, event: InputEvent) {
        dispatch_runtime_output(
            self.session.handle_input(event),
            &self.context,
            &lgui::window::WindowId::new("layout-drag-test"),
            |action| self.session.handle_default_action(action),
            |_| {},
        );
        self.render();
    }

    fn cell(&self, id: u64) -> UiRect {
        self.session
            .tree()
            .node(&UiId::owned(format!("layout-visibility-cell-{id}")))
            .unwrap()
            .layout_rect
    }

    fn down(&mut self, point: Point) {
        self.send(InputEvent::PointerMove(PointerData::mouse(point)));
        self.send(InputEvent::PointerDown {
            pointer: PointerData::mouse(point),
            button: PointerButton::Left,
        });
    }

    fn up(&mut self, point: Point) {
        self.send(InputEvent::PointerUp {
            pointer: PointerData::mouse(point),
            button: PointerButton::Left,
        });
    }

    fn move_to(&mut self, point: Point) {
        self.send(InputEvent::PointerMove(PointerData::mouse(point)));
    }

    fn layout(&self) -> ApplicationLayout {
        self.state.get().layout.application_layout.unwrap()
    }

    fn floating(&self) -> Option<UiRect> {
        let node = self
            .session
            .tree()
            .node(&UiId::new("layout-region-floating"))?;
        Some(
            node.compositing_layer
                .unwrap()
                .transform
                .transformed_bounds(node.layout_rect),
        )
    }

    fn save(&self, name: &str) {
        let Some(dir) = std::env::var_os("LOOG_LAYOUT_PREVIEW_DIR") else {
            return;
        };
        use lgui::core::PhysicalRect;
        use lgui::renderer::{FrameInfo, FrameReason};
        let physical = PhysicalRect::new(
            0,
            0,
            self.viewport.width() as i32,
            self.viewport.height() as i32,
        );
        let dirty = [physical];
        let frame = FrameInfo::new(
            physical,
            &dirty,
            UiScale::ONE,
            FrameReason::SceneChange,
            true,
        );
        let mut surface = lgui::render_skia::SkiaSoftwareSurface::new(16 * 1024 * 1024);
        lgui::assets::backend::with_render_resources(
            &self.context,
            lgui::assets::backend::render_resources(),
            || {
                surface.draw(
                    &self
                        .session
                        .tree()
                        .scene()
                        .project_to_physical(UiScale::ONE),
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
            std::path::Path::new(&dir).join(format!("{name}.png")),
            &rgba,
            physical.width() as u32,
            physical.height() as u32,
            image::ColorType::Rgba8,
        )
        .unwrap();
    }
}

fn body(rect: UiRect) -> Point {
    Point::new(rect.left + 8.0, rect.top + 8.0)
}
fn center(rect: UiRect) -> Point {
    Point::new(
        (rect.left + rect.right) * 0.5,
        (rect.top + rect.bottom) * 0.5,
    )
}

#[test]
fn regions_float_and_swap_in_an_animated_private_draft_until_drop() {
    let mut h = Harness::new();
    let _fonts = lgui::backend::install_text_environment(
        &h.context,
        lgui::render_skia::skia_text_system_handle(),
    );
    let original = h.layout();
    let tabs = h
        .tabs
        .get()
        .tabs()
        .iter()
        .map(|tab| tab.id)
        .collect::<Vec<_>>();
    let left = h.cell(2);
    let right = h.cell(3);
    h.save("layout-idle");
    h.down(body(left));
    assert!(
        h.floating().is_some(),
        "pressing the region must lift it immediately"
    );
    assert_eq!(h.layout(), original);
    let target = center(right);
    h.move_to(target);
    let preview = h.drag.snapshot().unwrap();
    assert_ne!(preview.preview.root, original.root);
    assert_eq!(
        h.layout(),
        original,
        "hovering must not change the real layout"
    );
    assert!(h.floating().unwrap().left > left.left);
    h.save("layout-swapping");
    std::thread::sleep(std::time::Duration::from_millis(75));
    let tick = h.session.advance(70.0);
    assert!(
        tick.animation_changed,
        "neighbors must animate without another pointer move"
    );
    h.render();
    let app_renders = h.renders.load(Ordering::Relaxed);
    std::thread::sleep(std::time::Duration::from_millis(90));
    h.session.advance(200.0);
    h.render();
    assert_eq!(
        h.renders.load(Ordering::Relaxed),
        app_renders,
        "retained motion rebuilt the application"
    );
    h.save("layout-preview");
    // Returning to the vacated slot reverses the preview, without touching the app.
    h.move_to(center(left));
    assert_eq!(h.drag.snapshot().unwrap().preview.root, original.root);
    h.move_to(target);
    let expected = h.drag.snapshot().unwrap().preview;
    h.up(target);
    assert!(h.drag.snapshot().is_none());
    assert!(h.floating().is_none());
    assert_eq!(h.layout().root, expected.root);
    assert_eq!(h.layout().version, original.version + 1);
    assert_eq!(
        h.tabs
            .get()
            .tabs()
            .iter()
            .map(|tab| tab.id)
            .collect::<Vec<_>>(),
        tabs
    );
    assert_eq!(
        h.layout().default_terminal_region(),
        original.default_terminal_region()
    );
    assert!(h.state.get().layout.visibility_popup.is_some());
    h.session.advance(200.0);
    h.render();
    h.save("layout-dropped");
}

#[test]
fn passing_over_other_regions_never_leaves_them_displaced() {
    let mut h = Harness::new();
    let original = h.layout();
    // Slot centers are fixed before dragging; animated cells move afterwards.
    let (middle, far) = (center(h.cell(1)), center(h.cell(3)));
    h.down(body(h.cell(2)));
    // Cross region 1 on the way to region 3.
    h.move_to(middle);
    h.move_to(far);
    let mut expected = original.clone();
    assert!(expected.swap_regions(2, 3));
    assert_eq!(h.drag.snapshot().unwrap().preview.root, expected.root);
    h.up(far);
    assert_eq!(h.layout().root, expected.root);
}

#[test]
fn escape_and_outside_drop_cancel_and_the_floating_region_stays_inside_the_popup() {
    let mut h = Harness::new();
    let original = h.layout();
    let left = h.cell(2);
    h.down(body(left));
    h.move_to(center(h.cell(3)));
    h.send(InputEvent::Keyboard(KeyboardEvent {
        key: LogicalKey::Named(NamedKey::Escape),
        ..Default::default()
    }));
    assert!(h.drag.snapshot().is_none());
    assert!(h.floating().is_none());
    assert_eq!(h.layout(), original);
    assert!(h.state.get().layout.visibility_popup.is_some());
    h.up(center(left));
    h.down(body(h.cell(2)));
    h.move_to(center(h.cell(3)));
    let area = h.drag.snapshot().unwrap().area;
    let outside = Point::new(h.viewport.left + 2.0, h.viewport.bottom - 2.0);
    h.move_to(outside);
    let floating = h.floating().unwrap();
    assert!(floating.left >= area.left - 0.01 && floating.right <= area.right + 0.01);
    assert!(floating.top >= area.top - 0.01 && floating.bottom <= area.bottom + 0.01);
    assert!(
        h.state.get().layout.visibility_popup.is_some(),
        "dragging outside must retain capture"
    );
    h.up(outside);
    assert!(h.drag.snapshot().is_none());
    assert_eq!(h.layout(), original);
    assert!(h.state.get().layout.visibility_popup.is_none());
}

#[test]
fn swaps_near_minimum_sized_slot_edges_do_not_oscillate_or_follow_animation() {
    let mut h = Harness::new();
    h.state.update(|app| {
        app.layout
            .application_layout
            .as_mut()
            .unwrap()
            .sizes
            .insert(5, vec![0.05, 0.55, 0.4]);
    });
    h.render();
    let original = h.layout();
    let target = body(h.cell(1));
    h.down(body(h.cell(2)));
    h.move_to(target);
    let expected = h.drag.snapshot().unwrap().preview.root;
    assert_ne!(expected, original.root);
    for delta in [0.0, 1.0, 2.0, 1.0, 0.0] {
        h.move_to(Point::new(target.x + delta, target.y));
        assert_eq!(h.drag.snapshot().unwrap().preview.root, expected);
        assert_eq!(h.layout(), original);
    }
    h.up(target);
    assert_eq!(h.layout().root, expected);
}

#[test]
fn stale_drafts_never_overwrite_visibility_changes_or_closed_sessions() {
    for visibility_change in [false, true] {
        let mut h = Harness::new();
        h.down(body(h.cell(2)));
        let target = center(h.cell(3));
        h.move_to(target);
        h.state.update(|app| {
            if visibility_change {
                app.drawer.visible = false;
            } else {
                app.layout
                    .application_layout
                    .as_mut()
                    .unwrap()
                    .sync(&[], None);
            }
        });
        let current = h.layout();
        h.render();
        h.up(target);
        assert_eq!(h.layout(), current);
        assert!(h.drag.snapshot().is_none());
        assert!(!h.state.get().drawer.visible || !visibility_change);
    }
}

#[test]
fn body_clicks_do_not_commit_and_hidden_regions_keep_their_identity_when_swapped() {
    let mut h = Harness::new();
    let initial = h.layout();
    let point = body(h.cell(2));
    h.down(point);
    h.up(point);
    assert_eq!(h.layout(), initial);
    toggle_region(&h.state, 1);
    h.render();
    let original = h.layout();
    h.down(body(h.cell(1)));
    let target = center(h.cell(3));
    h.move_to(target);
    assert_eq!(h.layout(), original);
    h.up(target);
    let app = h.state.get();
    let layout = app.layout.application_layout.as_ref().unwrap();
    assert!(!layout.region_visible(1, crate::app::terminal::visibility(&app)));
    assert!(layout.region_visible(3, crate::app::terminal::visibility(&app)));
    assert_ne!(layout.root, original.root);
    // Resizing the viewport during a gesture invalidates that draft too.
    let drag = h.drag.clone();
    let point = body(h.cell(2));
    h.down(point);
    let before = h.layout();
    let resized = UiRect::new(
        h.source.left,
        h.source.top,
        h.source.right - 200.0,
        h.source.bottom,
    );
    drag.release(&h.state, resized, point);
    assert_eq!(h.layout(), before);
    assert!(drag.snapshot().is_none());
}
