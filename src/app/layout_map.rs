//! The miniature itself is the drop surface. Hit tests and fills share rectangles.
use crate::app::context::AppContext;
use crate::app::state::AppState;
#[cfg(test)]
use crate::app::terminal_binding::TerminalTabs;
use crate::domain::application_layout::{ApplicationLayout, Content, Placement, Visibility};
use crate::domain::pane_layout::{Axis, Direction};
use crate::ui::terminal::layout_binding::{MapScene, MapSignal, StripDrag, TerminalDrag, Zone};
#[cfg(test)]
use crate::ui::theme;
use lgui::core::{CursorIcon, Observable, ObservableListener, Point};
#[cfg(test)]
use lgui::core::{LogicalKey, NamedKey};
#[cfg(test)]
use lgui::prelude::VisualStyle;
use lgui::prelude::{Element, UiRect};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Share of the content area the miniature may occupy on each axis.
const MAP_FRACTION: f32 = 0.56;
/// Window-edge drop bands sit just outside the miniature's silhouette.
const OUTER_BAND: f32 = 18.0;
/// Half-width of the gutter reserved for each internal divider.
const DIVIDER_HALF: f32 = 5.0;
/// Depth of a cell's side drop bands before its centre takes over.
const EDGE_BAND: f32 = 22.0;
/// Cell corner radius; also used by the centre-drop highlight.
const CELL_RADIUS: f32 = 6.0;

/// The miniature's drag state, kept outside `AppState`. Changing it re-renders
/// only its subscribers (the overlay and the tab cursors), never the app.
#[derive(Clone, Default)]
pub struct MapStore(Arc<MapInner>);
#[derive(Default)]
struct MapInner {
    drag: Mutex<Option<TerminalDrag>>,
    revision: AtomicU64,
    listeners: Mutex<Vec<(u64, ObservableListener)>>,
    next_listener: AtomicU64,
}

impl MapStore {
    pub fn with<R>(&self, f: impl FnOnce(Option<&TerminalDrag>) -> R) -> R {
        f(self.0.drag.lock().expect("map store poisoned").as_ref())
    }
    /// Mutate the drag; subscribers are notified only when `f` reports a
    /// visible change. Plain pointer bookkeeping returns false.
    pub fn update(&self, f: impl FnOnce(&mut Option<TerminalDrag>) -> bool) -> bool {
        let changed = f(&mut self.0.drag.lock().expect("map store poisoned"));
        if changed {
            self.0.revision.fetch_add(1, Ordering::AcqRel);
            let listeners = self
                .0
                .listeners
                .lock()
                .expect("map listeners poisoned")
                .iter()
                .map(|(_, l)| l.clone())
                .collect::<Vec<_>>();
            for listener in listeners {
                listener();
            }
        }
        changed
    }
    pub fn snapshot(&self) -> Option<TerminalDrag> {
        self.with(|d| d.cloned())
    }
    pub fn is_dragging(&self) -> bool {
        self.with(|d| d.is_some())
    }
    pub fn is_active(&self) -> bool {
        self.with(|d| d.is_some_and(|d| d.active))
    }
    pub fn cancel(&self) -> bool {
        self.update(|d| d.take().is_some())
    }
    pub fn observable(&self) -> Observable<MapSignal> {
        let read = self.clone();
        let subscribe = self.clone();
        Observable::new(
            Arc::as_ptr(&self.0) as u64,
            move || MapSignal {
                revision: read.0.revision.load(Ordering::Acquire),
                cursor: drag_cursor(&read),
            },
            move |listener| {
                let inner = subscribe.0.clone();
                let id = inner.next_listener.fetch_add(1, Ordering::Relaxed);
                inner
                    .listeners
                    .lock()
                    .expect("map listeners poisoned")
                    .push((id, listener));
                Box::new(move || {
                    inner
                        .listeners
                        .lock()
                        .expect("map listeners poisoned")
                        .retain(|(i, _)| *i != id);
                })
            },
        )
    }
}
/// Borrow the app state without invalidating it.
fn with_app<R>(state: &AppContext, f: impl FnOnce(&mut AppState) -> R) -> R {
    let mut out = None;
    state.try_update(|app| {
        out = Some(f(app));
        false
    });
    out.expect("state update ran")
}
pub fn visibility(app: &AppState) -> Visibility {
    Visibility {
        git: app.source_control.visible,
        files: app.drawer.visible,
        terminal: app.terminal_chrome.show_terminal,
    }
}
pub fn begin(
    app: &mut AppState,
    map: &MapStore,
    session: u64,
    point: Point,
    strip: Option<StripDrag>,
) {
    map.update(|d| {
        *d = Some(TerminalDrag {
            session,
            strip,
            reorder: None,
            origin: point,
            point,
            active: false,
            scene: None,
            hover: None,
            release: None,
        });
        true
    });
    app.terminal.ui.terminal_tab_context_menu = None;
    app.terminal.ui.terminal_shell_menu = false;
}
fn contains(rect: UiRect, point: Point) -> bool {
    point.x >= rect.left && point.x < rect.right && point.y >= rect.top && point.y < rect.bottom
}
impl MapScene {
    pub fn hit(&self, point: Point) -> Option<usize> {
        self.zones.iter().position(|z| contains(z.rect, point))
    }
    fn build(
        layout: &ApplicationLayout,
        session: u64,
        viewport: UiRect,
        visible: Visibility,
        sessions: Vec<u64>,
    ) -> Self {
        let snapshot = layout.snapshot(viewport.into(), visible);
        let width = viewport.width() * MAP_FRACTION;
        let height = viewport.height() * MAP_FRACTION;
        let scale = ((width - 2.0 * OUTER_BAND).max(1.0) / viewport.width().max(1.0))
            .min((height - 2.0 * OUTER_BAND).max(1.0) / viewport.height().max(1.0));
        let left = (viewport.left + viewport.right - viewport.width() * scale) / 2.0;
        let top = (viewport.top + viewport.bottom - viewport.height() * scale) / 2.0;
        let project = |r: UiRect| {
            UiRect::new(
                left + (r.left - viewport.left) * scale,
                top + (r.top - viewport.top) * scale,
                left + (r.right - viewport.left) * scale,
                top + (r.bottom - viewport.top) * scale,
            )
        };
        let silhouette = project(viewport);
        let mut zones = vec![];
        // Internal boundaries reserve a gutter, ordered by smaller scope.
        let mut dividers = snapshot.dividers.clone();
        dividers.sort_by(|a, b| {
            (a.parent.width() * a.parent.height())
                .total_cmp(&(b.parent.width() * b.parent.height()))
        });
        let mut divider_rects = vec![];
        for divider in &dividers {
            let scope = project(divider.parent.into());
            let r = match divider.axis {
                Axis::Horizontal => {
                    let x = left + (divider.position - viewport.left) * scale;
                    UiRect::new(x - DIVIDER_HALF, scope.top, x + DIVIDER_HALF, scope.bottom)
                }
                Axis::Vertical => {
                    let y = top + (divider.position - viewport.top) * scale;
                    UiRect::new(scope.left, y - DIVIDER_HALF, scope.right, y + DIVIDER_HALF)
                }
            };
            divider_rects.push((divider.axis, r));
            zones.push(Zone {
                rect: r,
                placement: Placement::Between {
                    split: divider.split,
                    before: divider.before,
                    after: divider.after,
                },
                center: false,
                plan: None,
            });
        }
        for (dir, r) in [
            (
                Direction::Left,
                UiRect::new(
                    silhouette.left - OUTER_BAND,
                    silhouette.top,
                    silhouette.left,
                    silhouette.bottom,
                ),
            ),
            (
                Direction::Right,
                UiRect::new(
                    silhouette.right,
                    silhouette.top,
                    silhouette.right + OUTER_BAND,
                    silhouette.bottom,
                ),
            ),
            (
                Direction::Up,
                UiRect::new(
                    silhouette.left,
                    silhouette.top - OUTER_BAND,
                    silhouette.right,
                    silhouette.top,
                ),
            ),
            (
                Direction::Down,
                UiRect::new(
                    silhouette.left,
                    silhouette.bottom,
                    silhouette.right,
                    silhouette.bottom + OUTER_BAND,
                ),
            ),
        ] {
            zones.push(Zone {
                rect: r,
                placement: Placement::Outer(dir),
                center: false,
                plan: None,
            });
        }
        let mut cells = vec![];
        for (id, content, real) in snapshot.regions {
            let mut r = project(real.into());
            // The cell cannot claim its neighbour's divider band.
            for (axis, band) in &divider_rects {
                match axis {
                    Axis::Horizontal if r.top < band.bottom && r.bottom > band.top => {
                        if (r.left - (band.left + band.right) / 2.0).abs() < 0.1 {
                            r.left = band.right;
                        }
                        if (r.right - (band.left + band.right) / 2.0).abs() < 0.1 {
                            r.right = band.left;
                        }
                    }
                    Axis::Vertical if r.left < band.right && r.right > band.left => {
                        if (r.top - (band.top + band.bottom) / 2.0).abs() < 0.1 {
                            r.top = band.bottom;
                        }
                        if (r.bottom - (band.top + band.bottom) / 2.0).abs() < 0.1 {
                            r.bottom = band.top;
                        }
                    }
                    _ => {}
                }
            }
            let ew = EDGE_BAND.min(r.width() * 0.26);
            let eh = EDGE_BAND.min(r.height() * 0.26);
            for (dir, zone) in [
                (
                    Direction::Left,
                    UiRect::new(r.left, r.top, r.left + ew, r.bottom),
                ),
                (
                    Direction::Right,
                    UiRect::new(r.right - ew, r.top, r.right, r.bottom),
                ),
                (
                    Direction::Up,
                    UiRect::new(r.left + ew, r.top, r.right - ew, r.top + eh),
                ),
                (
                    Direction::Down,
                    UiRect::new(r.left + ew, r.bottom - eh, r.right - ew, r.bottom),
                ),
            ] {
                zones.push(Zone {
                    rect: zone,
                    placement: Placement::Side(id, dir),
                    center: false,
                    plan: None,
                });
            }
            zones.push(Zone {
                rect: UiRect::new(r.left + ew, r.top + eh, r.right - ew, r.bottom - eh),
                placement: if matches!(content, Content::Terminal { .. }) {
                    Placement::Tabs(id)
                } else {
                    Placement::Side(id, Direction::Right)
                },
                center: true,
                plan: None,
            });
            cells.push((id, content, r));
        }
        for zone in &mut zones {
            zone.plan = layout.plan(
                session,
                &zone.placement,
                viewport.into(),
                visible,
                sessions.clone(),
            );
        }
        Self {
            version: layout.version,
            viewport,
            visibility: visible,
            sessions,
            cells,
            zones,
        }
    }
}
pub fn refresh(app: &AppState, map: &MapStore, viewport: UiRect, sessions: Vec<u64>) -> bool {
    let visible = visibility(app);
    let Some(layout) = &app.layout.application_layout else {
        return false;
    };
    map.update(|slot| {
        let Some(drag) = slot else {
            return false;
        };
        if !layout.terminal_visible(drag.session, visible) {
            *slot = None;
            return true;
        }
        let stale = drag.scene.as_ref().is_none_or(|s| {
            s.version != layout.version
                || s.viewport != viewport
                || s.visibility != visible
                || s.sessions != sessions
        });
        if stale {
            drag.scene = Some(MapScene::build(
                layout,
                drag.session,
                viewport,
                visible,
                sessions,
            ));
        }
        let hover = drag.scene.as_ref().and_then(|s| s.hit(drag.point));
        let changed = drag.hover != hover;
        drag.hover = hover;
        stale || changed
    })
}
pub fn update_pointer(
    app: &AppState,
    map: &MapStore,
    point: Point,
    viewport: UiRect,
    sessions: Vec<u64>,
) -> bool {
    // Only a visible change notifies the overlay; plain movement inside the
    // same zone is recorded silently.
    let activated = map.update(|slot| {
        let Some(drag) = slot.as_mut().filter(|d| d.release.is_none()) else {
            return false;
        };
        drag.point = point;
        if drag.active {
            return false;
        }
        let moved = (point.x - drag.origin.x).hypot(point.y - drag.origin.y) >= 5.0;
        // Staying inside the pressed strip reorders its tabs; the map only
        // opens once the pointer leaves it.
        if let Some(strip) = drag.strip.as_ref().filter(|s| s.holds(point)) {
            if drag.reorder.is_none() && !moved {
                return false;
            }
            let target = Some(strip.target(drag.session, point.x));
            let changed = drag.reorder != target;
            drag.reorder = target;
            return changed;
        }
        if moved {
            drag.reorder = None;
            drag.active = true;
        }
        moved
    });
    let (pending, reordering) = map.with(|d| {
        (
            d.is_some_and(|d| d.release.is_none()),
            d.is_some_and(|d| d.reorder.is_some()),
        )
    });
    pending && ((!reordering && refresh(app, map, viewport, sessions)) | activated)
}
/// Return whether a drag was consumed and which session should receive focus.
pub fn finish(
    app: &mut AppState,
    map: &MapStore,
    point: Point,
    viewport: UiRect,
    sessions: &[u64],
) -> (bool, Option<u64>) {
    let mut taken = None;
    map.update(|slot| {
        taken = slot.take();
        taken.is_some()
    });
    let Some(drag) = taken else {
        return (false, None);
    };
    if !drag.active {
        if let Some(index) = drag.reorder {
            if let Some(layout) = &mut app.layout.application_layout {
                layout.reorder_terminal(drag.session, index);
            }
            return (true, Some(drag.session));
        }
        return (false, Some(drag.session));
    }
    let visible = visibility(app);
    let plan = drag
        .scene
        .as_ref()
        .filter(|s| s.hit(point).is_some())
        .and_then(|s| drag.hover.and_then(|i| s.zones[i].plan.clone()));
    if let Some(plan) = plan
        && let Some(layout) = &mut app.layout.application_layout
        && layout.commit(plan, viewport.into(), visible, sessions)
    {
        return (true, Some(drag.session));
    }
    (true, None)
}
pub fn drag_cursor(map: &MapStore) -> CursorIcon {
    map.with(|d| match d.filter(|d| d.active || d.reorder.is_some()) {
        Some(d) if d.reorder.is_some() => CursorIcon::Move,
        Some(d)
            if d.scene
                .as_ref()
                .and_then(|s| d.hover.map(|i| &s.zones[i]))
                .is_some_and(|z| z.plan.is_some()) =>
        {
            CursorIcon::Move
        }
        Some(_) => CursorIcon::NotAllowed,
        None => CursorIcon::Pointer,
    })
}

pub(crate) fn binding(
    state: &AppContext,
    map: &MapStore,
    tabs: &crate::app::terminal_binding::TerminalStore,
) -> crate::ui::terminal::layout_binding::MapBinding {
    use crate::ui::terminal::layout_binding::{MapBinding, MapCommand, MapOutcome};
    let inspect = map.clone();
    let observed = map.clone();
    let operated = map.clone();
    let target = state.clone();
    let sessions = tabs.clone();
    let sash = state.clone();
    MapBinding::new(
        move |read| inspect.with(read),
        move || observed.observable(),
        move |command| {
            let ids = sessions
                .get()
                .tabs()
                .iter()
                .map(|t| t.id)
                .collect::<Vec<_>>();
            let result = match command {
                MapCommand::Cancel => {
                    return MapOutcome {
                        consumed: operated.cancel(),
                        focus: false,
                    };
                }
                MapCommand::Begin {
                    session,
                    point,
                    strip,
                } => {
                    if sessions.get().tabs().iter().any(|tab| tab.id == session) {
                        target.update(|app| {
                            if app.layout.application_layout.is_some() {
                                begin(app, &operated, session, point, strip);
                            }
                        });
                    }
                    return MapOutcome::default();
                }
                MapCommand::Pointer { point, viewport } => {
                    with_app(&target, |app| {
                        update_pointer(app, &operated, point, viewport, ids.clone());
                    });
                    return MapOutcome {
                        consumed: operated.is_active(),
                        focus: false,
                    };
                }
                MapCommand::Release { point, viewport } => {
                    if operated.is_active() {
                        operated.update(|drag| {
                            if let Some(drag) = drag {
                                drag.release = Some(point);
                            }
                            true
                        });
                        (true, None)
                    } else if operated.with(|d| d.is_some_and(|d| d.reorder.is_some())) {
                        // A reorder changes the layout, so it must invalidate the app.
                        let mut result = (true, None);
                        target.update(|app| {
                            result = finish(app, &operated, point, viewport, &ids.clone());
                        });
                        if let Some(id) = result.1 {
                            sessions.update(|tabs| tabs.select(id));
                        }
                        return MapOutcome {
                            consumed: result.0,
                            focus: result.1.is_some(),
                        };
                    } else {
                        with_app(&target, |app| {
                            finish(app, &operated, point, viewport, &ids.clone())
                        })
                    }
                }
                MapCommand::Finish {
                    point,
                    viewport,
                    hovered,
                } => {
                    if !hovered {
                        return MapOutcome {
                            consumed: operated.cancel(),
                            focus: false,
                        };
                    }
                    let mut result = (true, None);
                    target.try_update(|app| {
                        result = finish(app, &operated, point, viewport, &ids.clone());
                        result.1.is_some()
                    });
                    result
                }
                MapCommand::DividerStart(divider) => {
                    target.update(|app| app.layout.application_sash_drag = Some(divider));
                    return MapOutcome::default();
                }
                MapCommand::DividerDrag(point) => {
                    target.try_update(|app| {
                        let Some(drag) = &app.layout.application_sash_drag else {
                            return false;
                        };
                        let visible = visibility(app);
                        let Some(layout) = &mut app.layout.application_layout else {
                            return false;
                        };
                        layout.resize(
                            drag,
                            match drag.axis {
                                Axis::Horizontal => point.x,
                                Axis::Vertical => point.y,
                            },
                            visible,
                        )
                    });
                    return MapOutcome::default();
                }
                MapCommand::DividerEnd => {
                    target.update(|app| app.layout.application_sash_drag = None);
                    return MapOutcome::default();
                }
            };
            if let Some(id) = result.1 {
                sessions.update(|tabs| {
                    if result.0 {
                        tabs.detach_group(id);
                    }
                    tabs.select(id);
                });
            }
            MapOutcome {
                consumed: result.0,
                focus: result.1.is_some(),
            }
        },
        move |divider| {
            sash.get()
                .layout
                .application_sash_drag
                .as_ref()
                .is_some_and(|d| {
                    d.split == divider.split
                        && d.before == divider.before
                        && d.after == divider.after
                })
        },
    )
}
pub(crate) fn attach(
    root: Element,
    state: AppContext,
    map: MapStore,
    tabs: crate::app::terminal_binding::TerminalStore,
    viewport: UiRect,
    focus: lgui::core::UiFocusHandle,
) -> Element {
    crate::ui::terminal::layout::attach(root, binding(&state, &map, &tabs), viewport, focus)
}
pub(crate) fn render(
    viewport: UiRect,
    state: AppContext,
    map: MapStore,
    tabs: crate::app::terminal_binding::TerminalStore,
    focus: lgui::core::UiFocusHandle,
) -> Element {
    crate::ui::terminal::layout::render(viewport, binding(&state, &map, &tabs), focus)
}
pub(crate) fn render_divider(
    divider: crate::domain::application_layout::Divider,
    state: AppContext,
) -> Element {
    // Dividers use only sash commands; the session port stays empty.
    let reader = state.clone();
    let writer = state.clone();
    let map = crate::ui::terminal::layout_binding::MapBinding::new(
        |read| read(None),
        || MapStore::default().observable(),
        move |command| {
            use crate::ui::terminal::layout_binding::MapCommand;
            match command {
                MapCommand::DividerStart(divider) => {
                    writer.update(|app| app.layout.application_sash_drag = Some(divider))
                }
                MapCommand::DividerDrag(point) => {
                    writer.try_update(|app| {
                        let Some(drag) = &app.layout.application_sash_drag else {
                            return false;
                        };
                        let visible = visibility(app);
                        let Some(layout) = &mut app.layout.application_layout else {
                            return false;
                        };
                        layout.resize(
                            drag,
                            match drag.axis {
                                Axis::Horizontal => point.x,
                                Axis::Vertical => point.y,
                            },
                            visible,
                        )
                    });
                }
                MapCommand::DividerEnd => {
                    writer.update(|app| app.layout.application_sash_drag = None)
                }
                _ => {}
            }
            crate::ui::terminal::layout_binding::MapOutcome::default()
        },
        move |divider| {
            reader
                .get()
                .layout
                .application_sash_drag
                .as_ref()
                .is_some_and(|d| {
                    d.split == divider.split
                        && d.before == divider.before
                        && d.after == divider.after
                })
        },
    );
    crate::ui::terminal::layout::render_divider(divider, map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::terminal_runtime::TerminalDisplay;
    fn center(r: UiRect) -> Point {
        Point::new((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0)
    }
    fn setup() -> (AppState, MapStore, UiRect) {
        let r = UiRect::new(0.0, 0.0, 1200.0, 800.0);
        let mut app = AppState::new();
        app.terminal_chrome.show_terminal = true;
        app.source_control.visible = true;
        let mut layout = ApplicationLayout::initial(r.into(), 220.0, 220.0, 240.0);
        layout.sync(&[(1, 1), (2, 2)], Some(1));
        // The default strip remains region 4; a deliberately detached tab is 7.
        let plan = layout
            .plan(
                2,
                &Placement::Side(4, Direction::Right),
                r.into(),
                visibility(&app),
                vec![1, 2],
            )
            .unwrap();
        layout = plan.layout;
        app.layout.application_layout = Some(layout);
        let map = MapStore::default();
        begin(&mut app, &map, 1, Point::new(10.0, 600.0), None);
        update_pointer(&app, &map, center(r), r, vec![1, 2]);
        (app, map, r)
    }
    fn scene_of(map: &MapStore) -> MapScene {
        map.snapshot().unwrap().scene.unwrap()
    }
    #[test]
    fn all_map_cells_and_bands_have_explicit_targets_and_matching_geometry() {
        let (_, map, _) = setup();
        let scene = scene_of(&map);
        for (id, content, rect) in &scene.cells {
            let hit = &scene.zones[scene.hit(center(*rect)).unwrap()];
            let expected = if matches!(content, Content::Terminal { .. }) {
                Placement::Tabs(*id)
            } else {
                Placement::Side(*id, Direction::Right)
            };
            assert_eq!(hit.placement, expected);
            assert!(contains(hit.rect, center(*rect)));
        }
        for (index, zone) in scene.zones.iter().enumerate() {
            let i = scene.hit(center(zone.rect)).unwrap();
            let hit = &scene.zones[i];
            // A larger divider's midpoint can itself be a T junction. Its
            // smaller-scope divider wins there; both use the same fill/hit.
            if matches!(zone.placement, Placement::Between { .. })
                && matches!(hit.placement, Placement::Between { .. })
            {
                assert!(i <= index);
            } else {
                assert_eq!(hit.placement, zone.placement);
            }
            assert!(contains(hit.rect, center(zone.rect)));
        }
        assert!(scene.hit(Point::new(50.0, 50.0)).is_none());
        let sole = scene
            .zones
            .iter()
            .find(|z| z.placement == Placement::Side(4, Direction::Right))
            .unwrap();
        assert!(sole.plan.is_none());
    }
    #[test]
    fn movement_inside_a_zone_notifies_nobody_and_zone_changes_notify_only_the_map() {
        let (app, map, r) = setup();
        let revision = || map.observable().read().revision;
        let scene = scene_of(&map);
        let body = center(scene.zones.iter().find(|z| z.center).unwrap().rect);
        update_pointer(&app, &map, body, r, vec![1, 2]);
        let before = revision();
        let nudge = Point::new(body.x + 1.0, body.y + 1.0);
        assert!(!update_pointer(&app, &map, nudge, r, vec![1, 2]));
        assert_eq!(revision(), before);
        assert_eq!(map.snapshot().unwrap().point, nudge);
        let edge = scene
            .zones
            .iter()
            .find(|z| z.placement == Placement::Outer(Direction::Left))
            .unwrap()
            .rect;
        assert!(update_pointer(&app, &map, center(edge), r, vec![1, 2]));
        assert_eq!(revision(), before + 1);
        assert_eq!(drag_cursor(&map), CursorIcon::Move);
    }
    #[test]
    fn release_commits_cached_preview_even_when_release_point_moves_to_other_zone() {
        let (mut app, map, r) = setup();
        let original = app.layout.application_layout.clone();
        let target = scene_of(&map)
            .zones
            .iter()
            .find(|z| z.placement == Placement::Side(3, Direction::Right))
            .unwrap()
            .clone();
        update_pointer(&app, &map, center(target.rect), r, vec![1, 2]);
        assert_eq!(app.layout.application_layout, original);
        let elsewhere = scene_of(&map)
            .zones
            .iter()
            .find(|z| z.placement == Placement::Outer(Direction::Left))
            .unwrap()
            .rect;
        assert_eq!(
            finish(&mut app, &map, center(elsewhere), r, &[1, 2]),
            (true, Some(1))
        );
        assert_eq!(
            app.layout.application_layout,
            Some(target.plan.unwrap().layout)
        );
    }
    #[test]
    fn outside_invalid_and_stale_release_cancel_without_mutation() {
        for mode in 0..3 {
            let (mut app, map, r) = setup();
            let original = app.layout.application_layout.clone();
            let scene = scene_of(&map);
            let zone = scene
                .zones
                .iter()
                .find(|z| {
                    z.placement
                        == if mode == 1 {
                            Placement::Side(4, Direction::Right)
                        } else {
                            Placement::Outer(Direction::Left)
                        }
                })
                .unwrap()
                .rect;
            let point = center(zone);
            update_pointer(&app, &map, point, r, vec![1, 2]);
            let (point, ids) = match mode {
                0 => (Point::new(20.0, 20.0), vec![1, 2]),
                2 => (point, vec![2]),
                _ => (point, vec![1, 2]),
            };
            assert_eq!(finish(&mut app, &map, point, r, &ids), (true, None));
            assert_eq!(app.layout.application_layout, original);
            assert!(!map.is_dragging());
        }
    }
    #[test]
    fn resize_reprojects_targets_and_drag_threshold_keeps_clicks_light() {
        let (mut app, map, r) = setup();
        let before = map.snapshot().unwrap().scene;
        let resized = UiRect::new(0.0, 0.0, 1500.0, 600.0);
        assert!(refresh(&app, &map, resized, vec![1, 2]));
        let after = &scene_of(&map);
        assert_ne!(&before.unwrap().cells, &after.cells);
        assert_eq!(after.viewport, resized);
        begin(&mut app, &map, 1, Point::new(10.0, 10.0), None);
        update_pointer(&app, &map, Point::new(12.0, 12.0), r, vec![1, 2]);
        assert!(!map.snapshot().unwrap().active);
        assert!(map.snapshot().unwrap().scene.is_some());
        assert_eq!(
            finish(&mut app, &map, Point::new(12.0, 12.0), r, &[1, 2]),
            (false, Some(1))
        );
    }

    #[test]
    fn t_junction_uses_smaller_split_scope() {
        let (mut app, map, r) = setup();
        let layout = app.layout.application_layout.as_ref().unwrap();
        let plan = layout
            .plan(
                1,
                &Placement::Side(1, Direction::Down),
                r.into(),
                visibility(&app),
                vec![1, 2],
            )
            .unwrap();
        app.layout.application_layout = Some(plan.layout);
        begin(&mut app, &map, 2, Point::new(30.0, 600.0), None);
        update_pointer(&app, &map, center(r), r, vec![1, 2]);
        let scene = scene_of(&map);
        let nested = scene.zones.iter().find(|z| matches!(z.placement, Placement::Between { split, .. } if split != 5 && split != 6)).unwrap();
        let junction = Point::new(
            nested.rect.left + 0.25,
            (nested.rect.top + nested.rect.bottom) / 2.0,
        );
        assert_eq!(
            scene.zones[scene.hit(junction).unwrap()].placement,
            nested.placement
        );
    }

    fn strip_app() -> (AppState, UiRect, StripDrag) {
        let r = UiRect::new(0.0, 0.0, 1200.0, 800.0);
        let mut app = AppState::new();
        app.terminal_chrome.show_terminal = true;
        let mut layout = ApplicationLayout::initial(r.into(), 220.0, 220.0, 240.0);
        layout.sync(&[(1, 1), (2, 2), (3, 3)], Some(1));
        app.layout.application_layout = Some(layout);
        let strip = StripDrag {
            rect: UiRect::new(0.0, 600.0, 400.0, 630.0),
            tabs: vec![(1, 10.0, 110.0), (2, 114.0, 214.0), (3, 218.0, 318.0)],
        };
        (app, r, strip)
    }
    fn strip_order(app: &AppState) -> Vec<u64> {
        let layout = app.layout.application_layout.as_ref().unwrap();
        layout
            .terminal_view(layout.terminal_region(1).unwrap())
            .unwrap()
            .0
    }

    #[test]
    fn dragging_inside_the_strip_reorders_without_opening_the_map() {
        let (mut app, r, strip) = strip_app();
        let map = MapStore::default();
        begin(&mut app, &map, 1, Point::new(50.0, 615.0), Some(strip));
        // Below the threshold nothing happens, so a click stays a click.
        update_pointer(&app, &map, Point::new(52.0, 615.0), r, vec![1, 2, 3]);
        assert_eq!(map.snapshot().unwrap().reorder, None);
        // Past tab 2's centre but short of tab 3's: final index 1.
        update_pointer(&app, &map, Point::new(170.0, 640.0), r, vec![1, 2, 3]);
        let drag = map.snapshot().unwrap();
        assert!(!drag.active);
        assert_eq!(drag.reorder, Some(1));
        update_pointer(&app, &map, Point::new(350.0, 615.0), r, vec![1, 2, 3]);
        assert_eq!(map.snapshot().unwrap().reorder, Some(2));
        assert_eq!(
            finish(&mut app, &map, Point::new(350.0, 615.0), r, &[1, 2, 3]),
            (true, Some(1))
        );
        assert_eq!(strip_order(&app), vec![2, 3, 1]);
        assert!(!map.is_dragging());
    }

    #[test]
    fn marker_appears_only_where_the_tab_would_actually_move() {
        let (_, _, strip) = strip_app();
        // Tab 2 stays put: no marker on either side of its own slot.
        assert_eq!(strip.marker_x(2, 1), None);
        // Moving right shows the right edge of the slot it moves into.
        assert_eq!(strip.marker_x(1, 1), Some(214.0));
        assert_eq!(strip.marker_x(1, 2), Some(318.0));
        // Moving left shows the left edge.
        assert_eq!(strip.marker_x(3, 1), Some(114.0));
        assert_eq!(strip.marker_x(3, 0), Some(10.0));
    }

    #[test]
    fn leaving_the_strip_opens_the_map_and_never_reorders() {
        let (mut app, r, strip) = strip_app();
        let map = MapStore::default();
        begin(&mut app, &map, 1, Point::new(50.0, 615.0), Some(strip));
        update_pointer(&app, &map, Point::new(170.0, 615.0), r, vec![1, 2, 3]);
        assert_eq!(map.snapshot().unwrap().reorder, Some(1));
        update_pointer(&app, &map, Point::new(170.0, 300.0), r, vec![1, 2, 3]);
        let drag = map.snapshot().unwrap();
        assert!(drag.active);
        assert_eq!(drag.reorder, None);
        assert!(drag.scene.is_some());
        finish(&mut app, &map, Point::new(170.0, 300.0), r, &[1, 2, 3]);
        assert_eq!(strip_order(&app), vec![1, 2, 3]);
    }

    #[test]
    fn dropping_a_reorder_on_its_own_slot_changes_nothing() {
        let (mut app, r, strip) = strip_app();
        let version = app.layout.application_layout.as_ref().unwrap().version;
        let map = MapStore::default();
        begin(&mut app, &map, 2, Point::new(160.0, 615.0), Some(strip));
        update_pointer(&app, &map, Point::new(175.0, 615.0), r, vec![1, 2, 3]);
        assert_eq!(map.snapshot().unwrap().reorder, Some(1));
        finish(&mut app, &map, Point::new(175.0, 615.0), r, &[1, 2, 3]);
        let layout = app.layout.application_layout.as_ref().unwrap();
        assert_eq!(layout.version, version);
        assert_eq!(strip_order(&app), vec![1, 2, 3]);
    }

    #[test]
    fn shared_tab_strip_is_one_map_cell_and_detached_tabs_get_their_own_cell() {
        let (_, _, r) = setup();
        let map = MapStore::default();
        let mut app = AppState::new();
        app.terminal_chrome.show_terminal = true;
        let mut layout = ApplicationLayout::initial(r.into(), 220.0, 220.0, 240.0);
        layout.sync(&[(1, 1), (2, 1)], Some(1));
        assert_eq!(
            layout
                .snapshot(r.into(), visibility(&app))
                .regions
                .iter()
                .filter(|(_, c, _)| matches!(c, Content::Terminal { .. }))
                .count(),
            1
        );
        let plan = layout
            .plan(
                1,
                &Placement::Side(1, Direction::Right),
                r.into(),
                visibility(&app),
                vec![1, 2],
            )
            .unwrap();
        layout = plan.layout;
        app.layout.application_layout = Some(layout);
        begin(&mut app, &map, 1, Point::new(30.0, 575.0), None);
        update_pointer(&app, &map, center(r), r, vec![1, 2]);
        let scene = scene_of(&map);
        let terminals = scene
            .cells
            .iter()
            .filter(|(_, content, _)| matches!(content, Content::Terminal { .. }))
            .collect::<Vec<_>>();
        assert_eq!(terminals.len(), 2);
        assert_ne!(terminals[0].0, terminals[1].0);
        assert_ne!(terminals[0].2, terminals[1].2);
        for (id, _, rect) in terminals {
            let zone = &scene.zones[scene.hit(center(*rect)).unwrap()];
            assert_eq!(zone.placement, Placement::Tabs(*id));
        }
    }

    #[test]
    fn real_terminal_pointer_capture_moves_individual_items_cancels_and_keeps_input_focus() {
        use lgui::application::{AppView, ApplicationContext};
        use lgui::core::{
            EventPolicy, InputEvent, PointerButton, PointerData, UiElement, UiId, UiScale,
            dispatch_runtime_output,
        };
        use lgui::prelude::group;
        use lgui::session::UiSession;
        use std::sync::{Arc, Mutex};
        let viewport = UiRect::new(0.0, 0.0, 1200.0, 800.0);
        let exposed = Arc::new(Mutex::new(
            None::<(AppContext, crate::app::terminal_binding::TerminalStore)>,
        ));
        let output = exposed.clone();
        let application = Arc::new(lgui::ApplicationHandle::new(|task| task(), || {}));
        let map = MapStore::default();
        let view_map = map.clone();
        let view: AppView = Arc::new(move |cx| {
            let tabs = crate::app::terminal_binding::test_store(cx, None, || {
                let mut t = TerminalTabs::new();
                t.split(1);
                t.select(1);
                t
            });
            let state = AppContext::detached(cx.state_with(|| {
                let mut a = AppState::new();
                a.terminal_chrome.show_terminal = true;
                a.source_control.visible = true;
                a.layout.application_layout = Some(ApplicationLayout::initial(
                    viewport.into(),
                    220.0,
                    220.0,
                    240.0,
                ));
                a
            }));
            *output.lock().unwrap() = Some((state.clone(), tabs.clone()));
            let t = tabs.get();
            let ids = t.tabs().iter().map(|t| t.id).collect::<Vec<_>>();
            state.update(|app| {
                app.layout.application_layout.as_mut().unwrap().sync(
                    &t.tabs()
                        .iter()
                        .map(|t| (t.id, t.group_id))
                        .collect::<Vec<_>>(),
                    t.active_id(),
                );
                refresh(app, &view_map, viewport, ids);
            });
            let app = state.get();
            let snapshot = app
                .layout
                .application_layout
                .as_ref()
                .unwrap()
                .snapshot(viewport.into(), visibility(&app));
            let id = UiId::new("layout-terminal-focus");
            let focus = cx.focus_handle(id.clone());
            let editor_id = UiId::new("layout-editor-focus");
            let editor_focus = cx.focus_handle(editor_id.clone());
            let mut root = attach(
                group(viewport),
                state.clone(),
                view_map.clone(),
                tabs.clone(),
                viewport,
                focus.clone(),
            );
            for (region, content, r) in snapshot.regions {
                if matches!(content, Content::Terminal { .. }) {
                    root = root.child(crate::app::terminal_view::render_region(
                        r.into(),
                        state.clone(),
                        view_map.clone(),
                        editor_focus.clone(),
                        focus.clone(),
                        id.clone(),
                        tabs.clone(),
                        application.clone(),
                        true,
                        region,
                    ));
                } else {
                    let element_id = if matches!(content, Content::Editor) {
                        editor_id.clone()
                    } else {
                        UiId::owned(format!("test-area-{region}"))
                    };
                    root = root.child(
                        Element::new(move |cx| {
                            UiElement::panel(element_id, r.into(), VisualStyle::default())
                                .children(cx.children)
                        })
                        .event_policy(EventPolicy::INTERACTIVE),
                    );
                }
            }
            for d in snapshot.dividers {
                root = root.child(render_divider(d, state.clone()));
            }
            root = root.child(crate::app::chrome::status(
                UiRect::new(0.0, 776.0, 1200.0, 800.0),
                state.clone(),
            ));
            // The title bar's terminal toggle, in the test's old toggle spot.
            let title = crate::app::chrome::title_binding(
                &state,
                Some((editor_focus.clone(), focus.clone(), tabs.clone())),
            );
            root = root.child(
                Element::new(move |_cx| {
                    UiElement::panel(
                        UiId::new("layout-terminal-toggle"),
                        UiRect::new(0.0, 776.0, 34.0, 800.0),
                        VisualStyle::default(),
                    )
                })
                .event_policy(EventPolicy {
                    hover: true,
                    press: true,
                    focus: false,
                })
                .on_click(move || title.toggle(crate::ui::chrome::PanelToggle::Terminal)),
            );
            root.child(render(
                viewport,
                state.clone(),
                view_map.clone(),
                tabs.clone(),
                focus.clone(),
            ))
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let (state, tabs) = exposed.lock().unwrap().clone().unwrap();
        let context = ApplicationContext::empty(Default::default());
        let send = |session: &mut UiSession, input| {
            dispatch_runtime_output(
                session.handle_input(input),
                &context,
                &lgui::window::WindowId::new("layout-test"),
                |a| session.handle_default_action(a),
                |_| {},
            );
            session.render_view(&view, viewport, UiScale::ONE);
            // The release frame distinguishes normal Up from the framework's
            // synthetic Up on cancellation, then paints the committed model.
            session.render_view(&view, viewport, UiScale::ONE);
        };
        let down = |p| InputEvent::PointerDown {
            pointer: PointerData::mouse(p),
            button: PointerButton::Left,
        };
        let up = |p| InputEvent::PointerUp {
            pointer: PointerData::mouse(p),
            button: PointerButton::Left,
        };
        // The idle preview must not mask real dividers or terminal cursors.
        let app = state.get();
        let snapshot = app
            .layout
            .application_layout
            .as_ref()
            .unwrap()
            .snapshot(viewport.into(), visibility(&app));
        assert!(!snapshot.dividers.is_empty());
        for divider in snapshot.dividers {
            let point = match divider.axis {
                Axis::Horizontal => Point::new(
                    divider.position,
                    (divider.parent.top + divider.parent.bottom) / 2.0,
                ),
                Axis::Vertical => Point::new(
                    (divider.parent.left + divider.parent.right) / 2.0,
                    divider.position,
                ),
            };
            let expected = match divider.axis {
                Axis::Horizontal => CursorIcon::ResizeHorizontal,
                Axis::Vertical => CursorIcon::ResizeVertical,
            };
            assert_eq!(session.tree().cursor_at(point), Some(expected));
            send(
                &mut session,
                InputEvent::PointerMove(PointerData::mouse(point)),
            );
            let line_id = UiId::owned(format!(
                "resize-line-application-sash-{}-{}-{}",
                divider.split, divider.before, divider.after
            ));
            assert_eq!(
                session.tree().node(&line_id).unwrap().style.fill,
                Some(theme::c().border)
            );
            assert!(state.get().layout.application_sash_drag.is_none());
            assert!(!map.is_dragging());
            send(
                &mut session,
                InputEvent::PointerMove(PointerData::mouse(Point::new(600.0, 300.0))),
            );
            assert_eq!(
                session.tree().node(&line_id).unwrap().style.fill,
                Some(theme::c().border)
            );
        }
        assert_eq!(
            session.tree().cursor_at(Point::new(400.0, 650.0)),
            Some(CursorIcon::Text)
        );
        // The real app renders every terminal as a region; only the default bottom
        // one carries the panel tools, a detached group does not.
        let maximize_buttons = session
            .tree()
            .nodes()
            .iter()
            .filter(|n| {
                n.semantics.as_ref().and_then(|s| s.name.as_deref()) == Some("Maximize terminal")
            })
            .count();
        assert_eq!(maximize_buttons, 1);
        let select = session
            .tree()
            .node(&UiId::new("terminal-tab-select-1"))
            .unwrap()
            .hit_rect;
        let header = Point::new(
            (select.left + select.right) / 2.0,
            (select.top + select.bottom) / 2.0,
        );
        // An ordinary click retains text-input focus without invoking layout.
        send(&mut session, down(header));
        send(&mut session, up(header));
        assert!(!map.is_dragging());
        assert_eq!(tabs.get().active_id(), Some(1));
        let controllers = tabs
            .get()
            .tabs()
            .iter()
            .map(|t| t.controller.clone())
            .collect::<Vec<_>>();
        for c in &controllers {
            c.begin_selection(0, 0);
            c.extend_selection(0, 2);
        }
        send(&mut session, InputEvent::TextInput("one".into()));
        assert!(controllers[0].snapshot().selection.is_empty());
        assert!(!controllers[1].snapshot().selection.is_empty());
        send(&mut session, down(header));
        send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(Point::new(600.0, 400.0))),
        );
        assert!(map.snapshot().unwrap().active);
        let scene = scene_of(&map);
        let target = scene
            .zones
            .iter()
            .find(|z| z.placement == Placement::Outer(Direction::Left))
            .unwrap();
        let target_point = center(target.rect);
        send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(target_point)),
        );
        let expected = scene_of(&map)
            .zones
            .into_iter()
            .find(|z| z.placement == Placement::Outer(Direction::Left))
            .unwrap()
            .plan
            .unwrap();
        send(&mut session, up(target_point));
        assert_eq!(
            state.get().layout.application_layout.unwrap(),
            expected.layout
        );
        assert_eq!(tabs.get().tabs().len(), 2);
        for c in &controllers {
            c.begin_selection(0, 0);
            c.extend_selection(0, 2);
        }
        send(&mut session, InputEvent::TextInput("after move".into()));
        assert!(controllers[0].snapshot().selection.is_empty());
        assert!(!controllers[1].snapshot().selection.is_empty());
        // The real toggle hides the default region without disturbing input
        // focus, process identity, or drag gestures in the detached region.
        let toggle = Point::new(17.0, 789.0);
        send(&mut session, down(toggle));
        send(&mut session, up(toggle));
        assert!(!state.get().default_terminal_open());
        assert_eq!(tabs.get().active_id(), Some(1));
        assert!(
            session
                .tree()
                .node(&UiId::new("layout-terminal-focus"))
                .is_some()
        );
        controllers[0].begin_selection(0, 0);
        controllers[0].extend_selection(0, 2);
        send(
            &mut session,
            InputEvent::TextInput("while default is hidden".into()),
        );
        assert!(controllers[0].snapshot().selection.is_empty());
        let original = state.get().layout.application_layout.clone();
        // The detached region's tab sits at the top left, after its category capsule.
        let detached_tab = session
            .tree()
            .nodes()
            .iter()
            .filter(|n| n.id.as_str().starts_with("terminal-tab-select-") && n.hit_rect.top < 40.0)
            .min_by(|a, b| a.hit_rect.left.total_cmp(&b.hit_rect.left))
            .map(|n| center(n.hit_rect))
            .expect("a tab in the detached region");
        send(&mut session, down(detached_tab));
        send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(Point::new(600.0, 400.0))),
        );
        assert!(map.is_active());
        send(
            &mut session,
            InputEvent::Keyboard(lgui::core::KeyboardEvent {
                state: lgui::core::KeyState::Down,
                key: LogicalKey::Named(NamedKey::Escape),
                ..Default::default()
            }),
        );
        assert!(!map.is_dragging());
        assert_eq!(state.get().layout.application_layout, original);
        send(&mut session, up(Point::new(600.0, 400.0)));
        send(&mut session, down(toggle));
        send(&mut session, up(toggle));
        assert!(state.get().default_terminal_open());
        assert_eq!(tabs.get().active_id(), Some(2));
        // Restore the moved session's focus for the following drag checks.
        send(&mut session, down(detached_tab));
        send(&mut session, up(detached_tab));
        // Start from its new header; leaving the window cancels the captured press.
        let topology = state.get().layout.application_layout.unwrap().root;
        let sash_down = Point::new(600.0, 400.0);
        let sash_up = Point::new(520.0, 400.0);
        send(&mut session, down(sash_down));
        assert!(state.get().layout.application_sash_drag.is_some());
        send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(sash_up)),
        );
        send(&mut session, up(sash_up));
        let resized = state.get();
        assert!(resized.layout.application_sash_drag.is_none());
        assert_eq!(
            resized.layout.application_layout.as_ref().unwrap().root,
            topology
        );
        let snapshot = resized
            .layout
            .application_layout
            .as_ref()
            .unwrap()
            .snapshot(viewport.into(), visibility(&resized));
        let left = snapshot
            .regions
            .iter()
            .find(|(_, c, _)| matches!(c, Content::Terminal { tabs, .. } if tabs.contains(&1)))
            .unwrap()
            .2;
        assert!((left.width() - 520.0).abs() < 0.01);
        let body_point = Point::new(left.left + 100.0, left.top + 100.0);
        let hit = session.tree().hit_test(body_point).unwrap();
        assert_eq!(hit.id.as_str(), "terminal-session-1");
        send(&mut session, down(body_point));
        send(&mut session, up(body_point));
        send(
            &mut session,
            InputEvent::TextInput("after left resize".into()),
        );
        assert_eq!(
            controllers[0]
                .test_controller()
                .test_writes()
                .last()
                .map(Vec::as_slice),
            Some(b"after left resize".as_slice()),
            "a resized left terminal must still receive input after clicking its body"
        );
        let moved = detached_tab;
        send(&mut session, down(moved));
        send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(Point::new(600.0, 400.0))),
        );
        let before = state.get().layout.application_layout.clone();
        send(
            &mut session,
            InputEvent::PointerLeave(PointerData::mouse(Point::new(-1.0, 400.0))),
        );
        assert!(!map.is_dragging());
        assert_eq!(state.get().layout.application_layout, before);
        // Cancellation at a valid miniature point must also cancel: lgui
        // translates TouchPhase::Cancelled to PointerUp with that last point.
        send(&mut session, down(moved));
        send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(Point::new(600.0, 400.0))),
        );
        let scene = scene_of(&map);
        let valid = center(
            scene
                .zones
                .iter()
                .find(|z| z.placement == Placement::Side(4, Direction::Right))
                .unwrap()
                .rect,
        );
        send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(valid)),
        );
        send(
            &mut session,
            InputEvent::Touch {
                pointer: PointerData::mouse(valid),
                phase: lgui::core::TouchPhase::Cancelled,
            },
        );
        assert!(!map.is_dragging());
        assert_eq!(state.get().layout.application_layout, before);
        send(&mut session, down(moved));
        send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(Point::new(600.0, 400.0))),
        );
        send(
            &mut session,
            InputEvent::Keyboard(lgui::core::KeyboardEvent {
                state: lgui::core::KeyState::Down,
                key: LogicalKey::Named(NamedKey::Escape),
                ..Default::default()
            }),
        );
        assert!(!map.is_dragging());
        assert_eq!(state.get().layout.application_layout, before);
        send(&mut session, up(Point::new(600.0, 400.0)));
        // Place beside the second terminal through that terminal's own cell.
        send(&mut session, down(moved));
        send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(Point::new(600.0, 400.0))),
        );
        let scene = scene_of(&map);
        let adjacent = scene
            .zones
            .iter()
            .find(|z| z.placement == Placement::Side(4, Direction::Right))
            .unwrap();
        let adjacent_point = center(adjacent.rect);
        send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(adjacent_point)),
        );
        send(&mut session, up(adjacent_point));
        assert_eq!(
            state
                .get()
                .layout
                .application_layout
                .as_ref()
                .unwrap()
                .terminal_region(1),
            Some(7)
        );
        assert_eq!(
            state
                .get()
                .layout
                .application_layout
                .as_ref()
                .unwrap()
                .leaves()
                .iter()
                .filter(|(_, c)| matches!(c, Content::Terminal { .. }))
                .count(),
            2
        );
        assert_eq!(tabs.get().tabs().len(), 2);
        // The + button of a moved item creates in its own region.
        let app = state.get();
        let layout = app.layout.application_layout.as_ref().unwrap();
        let snapshot = layout.snapshot(viewport.into(), visibility(&app));
        let moved_rect = snapshot.regions.iter().find(|r| r.0 == 7).unwrap().2;
        let plus = session
            .tree()
            .nodes()
            .iter()
            .filter(|n| {
                n.semantics.as_ref().and_then(|s| s.name.as_deref()) == Some("New terminal")
            })
            .map(|n| center(n.layout_rect))
            .find(|point| {
                (moved_rect.left..=moved_rect.right).contains(&point.x)
                    && (moved_rect.top..=moved_rect.bottom).contains(&point.y)
            })
            .expect("the moved terminal's + button");
        send(&mut session, down(plus));
        send(&mut session, up(plus));
        assert_eq!(tabs.get().tabs().len(), 3);
        let app = state.get();
        let layout = app.layout.application_layout.as_ref().unwrap();
        let snapshot = layout.snapshot(viewport.into(), visibility(&app));
        assert_eq!(layout.terminal_region(3), Some(7));
        assert_eq!(
            snapshot.regions.iter().find(|r| r.0 == 7).unwrap().2,
            moved_rect
        );
        assert_eq!(layout.terminal_view(4), Some((vec![2], Some(2))));
        // Return the moved session to the default group via its center target.
        let moved_tab = session
            .tree()
            .nodes()
            .iter()
            .filter(|n| {
                n.id.as_str().starts_with("terminal-tab-select-")
                    && n.hit_rect.left >= moved_rect.left
                    && n.hit_rect.right <= moved_rect.right
                    && n.hit_rect.top < moved_rect.top + 40.0
                    && n.hit_rect.top >= moved_rect.top
            })
            .min_by(|a, b| a.hit_rect.left.total_cmp(&b.hit_rect.left))
            .map(|n| center(n.hit_rect))
            .expect("a tab in the moved region");
        send(&mut session, down(moved_tab));
        send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(Point::new(600.0, 400.0))),
        );
        let scene = scene_of(&map);
        let target = center(
            scene
                .zones
                .iter()
                .find(|z| z.placement == Placement::Tabs(4))
                .unwrap()
                .rect,
        );
        send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(target)),
        );
        send(&mut session, up(target));
        assert_eq!(
            state
                .get()
                .layout
                .application_layout
                .as_ref()
                .unwrap()
                .terminal_view(4),
            Some((vec![2, 1], Some(1)))
        );
        send(&mut session, down(toggle));
        send(&mut session, up(toggle));
        assert!(!state.get().default_terminal_open());
        assert!(
            session
                .tree()
                .node(&UiId::new("layout-terminal-focus"))
                .is_none()
        );
        // Move every session out, then use the real toggle to create a new
        // default session instead of hiding or relocating the existing ones.
        send(&mut session, down(toggle));
        send(&mut session, up(toggle));
        state.update(|app| {
            let v = visibility(app);
            let layout = app.layout.application_layout.as_mut().unwrap();
            for id in [1, 2, 3] {
                let plan = layout
                    .plan(
                        id,
                        &Placement::Outer(Direction::Up),
                        viewport.into(),
                        v,
                        vec![1, 2, 3],
                    )
                    .unwrap();
                assert!(layout.commit(plan, viewport.into(), v, &[1, 2, 3]));
            }
        });
        session.render_view(&view, viewport, UiScale::ONE);
        assert!(!state.get().default_terminal_open());
        let detached = state.get().layout.application_layout.unwrap();
        let ownership = [1, 2, 3].map(|id| detached.terminal_region(id).unwrap());
        send(&mut session, down(toggle));
        send(&mut session, up(toggle));
        assert!(state.get().default_terminal_open());
        assert_eq!(tabs.get().tabs().len(), 4);
        assert_eq!(tabs.get().active_id(), Some(4));
        let app = state.get();
        let layout = app.layout.application_layout.as_ref().unwrap();
        assert_eq!(layout.default_terminal_active(), Some(4));
        assert_eq!(
            [1, 2, 3].map(|id| layout.terminal_region(id).unwrap()),
            ownership
        );
        send(&mut session, down(toggle));
        send(&mut session, up(toggle));
        let app = state.get();
        let layout = app.layout.application_layout.as_ref().unwrap();
        for id in [1, 2, 3] {
            assert!(layout.terminal_visible(id, visibility(&app)));
        }
        assert!(!layout.terminal_visible(4, visibility(&app)));
    }
}
