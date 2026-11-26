//! A private layout draft. Pointer motion never updates the application layout.
use super::*;
use crate::domain::application_layout::{ApplicationLayout, Visibility};
use lgui::core::{Observable, ObservableListener, Point, Signal};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub(super) struct Draft {
    pub base: ApplicationLayout,
    pub preview: ApplicationLayout,
    pub visibility: Visibility,
    pub source: UiRect,
    pub area: UiRect,
    pub region: u64,
    pub origin: UiRect,
    pub pressed: Point,
}

impl Draft {
    pub fn current(&self, app: &crate::app::state::AppState, source: UiRect) -> bool {
        self.source == source
            && self.visibility == crate::app::terminal::visibility(app)
            && app.layout.application_layout.as_ref() == Some(&self.base)
            && app.layout.visibility_popup.is_some()
    }
}

#[derive(Clone, Default)]
pub(crate) struct DragStore(Arc<Inner>);

#[derive(Default)]
struct Inner {
    draft: Mutex<Option<Draft>>,
    pointer: Option<Signal<Point>>,
    revision: AtomicU64,
    next_listener: AtomicU64,
    listeners: Mutex<Vec<(u64, ObservableListener)>>,
}

impl DragStore {
    pub fn new(pointer: Signal<Point>) -> Self {
        Self(Arc::new(Inner {
            pointer: Some(pointer),
            ..Default::default()
        }))
    }

    pub fn active(&self) -> bool {
        self.0
            .draft
            .lock()
            .expect("layout draft poisoned")
            .is_some()
    }

    pub fn pointer(&self) -> Option<Signal<Point>> {
        self.0.pointer.clone()
    }

    pub(super) fn snapshot(&self) -> Option<Draft> {
        self.0.draft.lock().expect("layout draft poisoned").clone()
    }

    fn update(&self, change: impl FnOnce(&mut Option<Draft>) -> bool) {
        let changed = change(&mut self.0.draft.lock().expect("layout draft poisoned"));
        if changed {
            self.0.revision.fetch_add(1, Ordering::AcqRel);
            let listeners = self
                .0
                .listeners
                .lock()
                .expect("layout listeners poisoned")
                .iter()
                .map(|(_, listener)| listener.clone())
                .collect::<Vec<_>>();
            for listener in listeners {
                listener();
            }
        }
    }

    pub fn cancel(&self) {
        self.update(|draft| draft.take().is_some());
    }

    pub fn observable(&self) -> Observable<u64> {
        let read = self.clone();
        let listen = self.clone();
        Observable::new(
            Arc::as_ptr(&self.0) as u64,
            move || read.0.revision.load(Ordering::Acquire),
            move |listener| {
                let inner = listen.0.clone();
                let id = inner.next_listener.fetch_add(1, Ordering::Relaxed);
                inner
                    .listeners
                    .lock()
                    .expect("layout listeners poisoned")
                    .push((id, listener));
                Box::new(move || {
                    inner
                        .listeners
                        .lock()
                        .expect("layout listeners poisoned")
                        .retain(|(key, _)| *key != id)
                })
            },
        )
    }

    pub(super) fn begin(
        &self,
        state: &AppContext,
        source: UiRect,
        region: u64,
        point: Point,
        area: UiRect,
    ) {
        self.cancel();
        let app = state.get();
        let Some(layout) = &app.layout.application_layout else {
            return;
        };
        if app.layout.visibility_popup.is_none() || !area.contains(point) {
            return;
        }
        let Some((_, _, origin)) = layout
            .overview(source.into())
            .regions
            .into_iter()
            .find(|(id, _, _)| *id == region)
        else {
            return;
        };
        if let Some(pointer) = &self.0.pointer {
            pointer.set(point);
        }
        self.update(|draft| {
            *draft = Some(Draft {
                base: layout.clone(),
                preview: layout.clone(),
                visibility: crate::app::terminal::visibility(&app),
                source,
                area,
                region,
                origin: origin.into(),
                pressed: point,
            });
            true
        });
    }

    pub(super) fn move_pointer(&self, state: &AppContext, source: UiRect, point: Point) {
        let app = state.get();
        if let Some(pointer) = &self.0.pointer {
            pointer.set(point);
        }
        self.update(|draft| {
            let Some(d) = draft else {
                return false;
            };
            if !d.current(&app, source) {
                *draft = None;
                return true;
            }
            if !d.area.contains(point) {
                return false;
            }
            let at = Point::new(
                source.left + (point.x - d.area.left) / d.area.width().max(1.0) * source.width(),
                source.top + (point.y - d.area.top) / d.area.height().max(1.0) * source.height(),
            );
            // Hit-test the gesture's fixed slots, never an animated neighbor.
            // Swapped content can have different minima and move the slot edges.
            let target = d
                .base
                .overview(source.into())
                .regions
                .iter()
                .find(|(_, _, rect)| {
                    at.x >= rect.left && at.x < rect.right && at.y >= rect.top && at.y < rect.bottom
                })
                .map(|(id, _, _)| *id);
            // Over a gap, keep the current preview.
            let Some(target) = target else {
                return false;
            };
            // Always derive the preview from the untouched base: one swap of the
            // dragged region with the region owning the hovered slot. Regions the
            // pointer merely passes over are never left displaced.
            let mut next = d.base.clone();
            next.swap_regions(d.region, target);
            if next.root == d.preview.root {
                return false;
            }
            d.preview = next;
            true
        });
    }

    pub(super) fn release(&self, state: &AppContext, source: UiRect, point: Point) {
        self.move_pointer(state, source, point);
        let mut released = None;
        self.update(|draft| {
            released = draft.take();
            released.is_some()
        });
        let Some(mut d) = released else {
            return;
        };
        if !d.area.contains(point) || d.preview.root == d.base.root {
            return;
        }
        state.try_update(|app| {
            if !d.current(app, source) {
                return false;
            }
            // One drop is one layout revision, regardless of intermediate swaps.
            d.preview.version = d.base.version + 1;
            app.layout.application_layout = Some(d.preview);
            true
        });
    }
}
