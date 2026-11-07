//! Mounted SVG and GIF workers. Snapshots own no live decoder services.
use crate::app::context::AppContext;
use crate::domain::document::DocumentStamp;
use crate::domain::identity::FileId;
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

struct Request {
    stamp: DocumentStamp,
    source: Arc<String>,
    density: u32,
}
pub struct ImageService {
    pending: Arc<Mutex<HashMap<FileId, (DocumentStamp, u32)>>>,
    queued: Arc<Mutex<HashMap<FileId, Request>>>,
    send: mpsc::Sender<Option<FileId>>,
    alive: Arc<AtomicBool>,
    gif: crate::app::gif_playback::Playback,
    paused: AtomicBool,
    state: AppContext,
    loading: super::image_loading::Loading,
}
impl ImageService {
    pub fn new(state: AppContext, application: lgui::ApplicationHandle) -> Self {
        let (send, receive) = mpsc::channel::<Option<FileId>>();
        let alive = Arc::new(AtomicBool::new(true));
        let pending = Arc::new(Mutex::new(HashMap::<FileId, (DocumentStamp, u32)>::new()));
        let worker_alive = alive.clone();
        let worker_pending = pending.clone();
        let queued = Arc::new(Mutex::new(HashMap::<FileId, Request>::new()));
        let worker_queue = queued.clone();
        let ready_state = state.clone();
        let ready_app = application.clone();
        let spawned = std::thread::Builder::new()
            .name("loog-svg-preview".into())
            .spawn(move || {
                while let Ok(Some(id)) = receive.recv() {
                    if !worker_alive.load(Ordering::Acquire) {
                        break;
                    }
                    let Some(request) = worker_queue.lock().unwrap().remove(&id) else {
                        continue;
                    };
                    if worker_pending.lock().unwrap().get(&request.stamp.document)
                        != Some(&(request.stamp, request.density))
                    {
                        continue;
                    }
                    let outcome = crate::infrastructure::image_filesystem::svg(
                        &request.source,
                        request.density as f32 / 100.0,
                    )
                    .map(Arc::new);
                    let state = ready_state.clone();
                    let alive = worker_alive.clone();
                    let pending = worker_pending.clone();
                    ready_app.post(move || {
                        if !alive.load(Ordering::Acquire)
                            || pending.lock().unwrap().get(&request.stamp.document)
                                != Some(&(request.stamp, request.density))
                        {
                            return;
                        }
                        state.try_update(|app| {
                            let Some(doc) =
                                app.workspace.documents.get(&request.stamp.document).filter(
                                    |doc| {
                                        doc.is_text()
                                            && doc.stamp(request.stamp.document) == request.stamp
                                    },
                                )
                            else {
                                return false;
                            };
                            if !crate::domain::media::is_svg(&doc.meta.path) {
                                return false;
                            }
                            let previous = app
                                .editor
                                .images
                                .svg
                                .get(&request.stamp.document)
                                .filter(|p| p.stamp.incarnation == request.stamp.incarnation)
                                .and_then(|p| p.image.clone());
                            let (image, error) = match outcome {
                                Ok(image) => (Some(image), None),
                                Err(error) => (previous, Some(error)),
                            };
                            app.editor.images.svg.insert(
                                request.stamp.document,
                                crate::app::image_preview::SvgPreview {
                                    stamp: request.stamp,
                                    density: request.density,
                                    image,
                                    error,
                                },
                            );
                            true
                        });
                    });
                }
            });
        if spawned.is_err() {
            alive.store(false, Ordering::Release);
        }
        let gif = crate::app::gif_playback::Playback::new(state.clone(), application.clone());
        let loading = super::image_loading::Loading::new(state.clone(), application);
        Self {
            pending,
            queued,
            send,
            alive,
            gif,
            paused: AtomicBool::new(false),
            state,
            loading,
        }
    }
    pub fn request_svg(
        &self,
        app: &crate::app::state::AppState,
        view: crate::domain::identity::ViewId,
    ) {
        let Some(value) = app.workspace.view(view) else {
            return;
        };
        let Some(id) = value.document() else {
            return;
        };
        let Some(doc) = app
            .workspace
            .documents
            .get(&id)
            .filter(|doc| doc.is_text() && crate::domain::media::is_svg(&doc.meta.path))
        else {
            return;
        };
        let stamp = doc.stamp(id);
        // Fit starts at device scale; manual zoom requests a denser
        // publication, which other panes can reuse.
        let density = (value.image.scale.unwrap_or(1.0).max(1.0)
            * crate::ui::device_scale::observed()
            * 100.0)
            .clamp(100.0, 1600.0)
            .ceil() as u32;
        let mut pending = self.pending.lock().unwrap();
        if pending
            .get(&id)
            .is_some_and(|(key, resolution)| *key == stamp && *resolution >= density)
        {
            return;
        }
        pending.insert(id, (stamp, density));
        drop(pending);
        let enqueued = self
            .queued
            .lock()
            .unwrap()
            .insert(
                id,
                Request {
                    stamp,
                    source: doc.buffer.text_snapshot(),
                    density,
                },
            )
            .is_none();
        if !self.alive.load(Ordering::Acquire) || (enqueued && self.send.send(Some(id)).is_err()) {
            self.queued.lock().unwrap().remove(&id);
            self.state.update(|app| {
                app.editor.images.svg.insert(
                    id,
                    crate::app::image_preview::SvgPreview {
                        stamp,
                        density,
                        image: None,
                        error: Some("SVG preview worker is unavailable".into()),
                    },
                );
            });
        }
    }
    pub fn sync(self: &Arc<Self>, app: &crate::app::state::AppState) {
        self.loading.sync(app);
        self.pending
            .lock()
            .unwrap()
            .retain(|id, _| app.workspace.documents.contains_key(id));
        self.queued
            .lock()
            .unwrap()
            .retain(|id, _| app.workspace.documents.contains_key(id));
        self.gif.sync(
            app,
            self.paused.load(Ordering::Acquire) || !self.alive.load(Ordering::Acquire),
        );
    }
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Release);
        self.gif.sync(&self.state.get(), paused);
    }
    pub fn shutdown(&self) {
        self.alive.store(false, Ordering::Release);
        self.loading.shutdown();
        self.gif.shutdown();
        let _ = self.send.send(None);
    }
}
impl Drop for ImageService {
    fn drop(&mut self) {
        self.shutdown();
    }
}
