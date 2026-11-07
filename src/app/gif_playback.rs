//! One bounded, deadline-driven decoder per visible GIF. No per-frame PNG work.
use crate::app::context::AppContext;
use crate::app::image_preview::GifPreview;
use crate::domain::identity::FileId;
use crate::domain::media::{Frame, Image};
use crate::domain::view::Presentation;
use crate::infrastructure::gif_stream::Stream;
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

#[derive(Clone)]
struct Request {
    document: FileId,
    incarnation: u64,
    image: Arc<Image>,
}
struct Playing {
    request: Request,
    decoder: Stream,
    next: Frame,
    deadline: Instant,
}
impl Playing {
    fn new(request: Request) -> Result<Self, String> {
        let mut decoder =
            Stream::new(request.image.animation.as_ref().ok_or("Not a GIF")?.clone())?;
        let first = decoder.next_frame()?;
        let deadline = Instant::now() + Duration::from_millis(first.duration_ms);
        let next = decoder.next_frame()?;
        Ok(Self {
            request,
            decoder,
            next,
            deadline,
        })
    }
}
pub struct Playback {
    desired: Arc<Mutex<Vec<Request>>>,
    last: Mutex<Vec<(FileId, u64, u64)>>,
    wake: mpsc::Sender<()>,
    alive: Arc<AtomicBool>,
}
impl Playback {
    pub fn new(state: AppContext, application: lgui::ApplicationHandle) -> Self {
        let desired = Arc::new(Mutex::new(Vec::<Request>::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let (wake, receive) = mpsc::channel();
        let worker_desired = desired.clone();
        let worker_alive = alive.clone();
        let ready = Arc::new(Mutex::new(HashMap::<FileId, GifPreview>::new()));
        let posted = Arc::new(AtomicBool::new(false));
        let _ = std::thread::Builder::new()
            .name("loog-gif-playback".into())
            .spawn(move || {
                let publish = |request: &Request, frame: Frame, error: Option<String>| {
                    ready.lock().unwrap().insert(
                        request.document,
                        GifPreview {
                            incarnation: request.incarnation,
                            identity: request.image.identity,
                            frame,
                            error,
                        },
                    );
                    if posted.swap(true, Ordering::AcqRel) {
                        return;
                    }
                    let ready = ready.clone();
                    let posted = posted.clone();
                    let alive = worker_alive.clone();
                    let state = state.clone();
                    application.post(move || {
                        let updates = {
                            let mut ready = ready.lock().unwrap();
                            let updates = std::mem::take(&mut *ready);
                            posted.store(false, Ordering::Release);
                            updates
                        };
                        if !alive.load(Ordering::Acquire) {
                            return;
                        }
                        state.try_update(|app| {
                            let mut changed = false;
                            for (id, preview) in updates {
                                let matches = app.workspace.documents.get(&id).is_some_and(|doc| {
                                    doc.incarnation == preview.incarnation
                                        && doc.open_error.is_none()
                                        && doc
                                            .image
                                            .as_ref()
                                            .is_some_and(|image| image.identity == preview.identity)
                                });
                                if matches {
                                    app.editor.images.gif.insert(id, preview);
                                    changed = true;
                                }
                            }
                            changed
                        });
                    });
                };
                let mut playing = HashMap::<FileId, Playing>::new();
                let mut failed = HashMap::<FileId, (u64, u64)>::new();
                loop {
                    if !worker_alive.load(Ordering::Acquire) {
                        break;
                    }
                    let requests = worker_desired.lock().unwrap().clone();
                    let contains = |id: &FileId, incarnation: u64, identity: u64| {
                        requests.iter().any(|r| {
                            r.document == *id
                                && r.incarnation == incarnation
                                && r.image.identity == identity
                        })
                    };
                    playing.retain(|id, p| {
                        contains(id, p.request.incarnation, p.request.image.identity)
                    });
                    failed.retain(|id, (incarnation, identity)| {
                        contains(id, *incarnation, *identity)
                    });
                    for request in requests {
                        if playing.contains_key(&request.document)
                            || failed.contains_key(&request.document)
                        {
                            continue;
                        }
                        match Playing::new(request.clone()) {
                            Ok(player) => {
                                playing.insert(request.document, player);
                            }
                            Err(error) => {
                                publish(&request, request.image.frames[0].clone(), Some(error));
                                failed.insert(
                                    request.document,
                                    (request.incarnation, request.image.identity),
                                );
                            }
                        }
                    }
                    let now = Instant::now();
                    let mut broken = Vec::new();
                    for (id, player) in &mut playing {
                        if player.deadline > now {
                            continue;
                        }
                        let current = player.next.clone();
                        publish(&player.request, current.clone(), None);
                        player.deadline += Duration::from_millis(current.duration_ms);
                        match player.decoder.next_frame() {
                            Ok(next) => {
                                player.next = next;
                            }
                            Err(error) => {
                                publish(&player.request, current, Some(error));
                                broken.push(*id);
                            }
                        }
                    }
                    for id in broken {
                        if let Some(player) = playing.remove(&id) {
                            failed.insert(
                                id,
                                (player.request.incarnation, player.request.image.identity),
                            );
                        }
                    }
                    let deadline = playing.values().map(|player| player.deadline).min();
                    let event = match deadline {
                        Some(deadline) => {
                            receive.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                        }
                        None => receive
                            .recv()
                            .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
                    };
                    if matches!(event, Err(mpsc::RecvTimeoutError::Disconnected)) {
                        break;
                    }
                    // Collapse visibility updates while a frame was being decoded.
                    while receive.try_recv().is_ok() {}
                }
            });
        Self {
            desired,
            last: Mutex::new(Vec::new()),
            wake,
            alive,
        }
    }
    pub fn sync(&self, app: &crate::app::state::AppState, paused: bool) {
        let mut desired = Vec::new();
        if !paused {
            for pane in app.workspace.pane_ids() {
                let Some(view) = app
                    .workspace
                    .pane(pane)
                    .and_then(|pane| pane.active())
                    .and_then(|id| app.workspace.view(id))
                else {
                    continue;
                };
                if view.presentation != Presentation::Image {
                    continue;
                }
                let Some(id) = view.document() else {
                    continue;
                };
                let Some(doc) = app
                    .workspace
                    .documents
                    .get(&id)
                    .filter(|doc| doc.open_error.is_none())
                else {
                    continue;
                };
                let Some(image) = doc.image.as_ref().filter(|image| image.animation.is_some())
                else {
                    continue;
                };
                if !desired.iter().any(|r: &Request| r.document == id) {
                    desired.push(Request {
                        document: id,
                        incarnation: doc.incarnation,
                        image: image.clone(),
                    });
                }
            }
        }
        let key: Vec<_> = desired
            .iter()
            .map(|r| (r.document, r.incarnation, r.image.identity))
            .collect();
        let mut last = self.last.lock().unwrap();
        if *last == key {
            return;
        }
        *last = key;
        *self.desired.lock().unwrap() = desired;
        let _ = self.wake.send(());
    }
    pub fn shutdown(&self) {
        self.alive.store(false, Ordering::Release);
        self.desired.lock().unwrap().clear();
        let _ = self.wake.send(());
    }
}
impl Drop for Playback {
    fn drop(&mut self) {
        self.shutdown();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "set LOOG_GIF_BENCH_FILE to measure autoplay and two full loops"]
    fn gif_autoplay_and_loop_performance() {
        let path =
            std::env::var_os("LOOG_GIF_BENCH_FILE").expect("LOOG_GIF_BENCH_FILE is required");
        // The user opens a file in an existing editor; exclude application startup.
        let mut app = crate::app::state::AppState::new();
        let started = Instant::now();
        let (image, disk) =
            crate::infrastructure::image_filesystem::load(std::path::Path::new(&path)).unwrap();
        let first = image.frames[0].bytes.clone();
        let (width, height) = (image.width, image.height);
        let id = app.workspace.open_prepared_text(
            crate::app::workspace::PreparedTextFile::image(path.into(), image, disk),
            crate::app::workspace::OpenMode::Permanent,
        );
        let state = crate::app::test_support::live(app);
        let tasks = crate::app::test_support::UiTasks::default();
        let playback = Playback::new(state.clone(), tasks.application());
        playback.sync(&state.get(), false);
        let mut surface =
            skia_safe::surfaces::raster_n32_premul((width as i32, height as i32)).unwrap();
        let mut previous = 0;
        let mut seen = 0;
        let mut gaps = 0;
        let mut first_motion = None;
        let mut loop_seen = false;
        let deadline = Instant::now() + Duration::from_secs(15);
        while previous < 250 {
            tasks.run_all();
            if let Some(preview) = state.get().editor.images.gif.get(&id).cloned() {
                assert!(preview.error.is_none(), "{:?}", preview.error);
                let sequence = preview.frame.sequence;
                if sequence > previous {
                    let image = skia_safe::Image::from_encoded(skia_safe::Data::new_copy(
                        preview.frame.bytes.as_slice(),
                    ))
                    .unwrap();
                    surface.canvas().draw_image(&image, (0.0, 0.0), None);
                    if first_motion.is_none() {
                        first_motion = Some(started.elapsed());
                    }
                    if sequence == 125 || sequence == 250 {
                        assert_eq!(preview.frame.bytes.as_slice(), first.as_slice());
                        loop_seen = true;
                    }
                    gaps += sequence.saturating_sub(previous + 1);
                    previous = sequence;
                    seen += 1;
                }
            }
            assert!(
                Instant::now() < deadline,
                "autoplay stalled at frame {previous}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        let elapsed = started.elapsed();
        playback.shutdown();
        eprintln!(
            "GIF autoplay: first moving frame={:.2}ms, {} published/drawn frames in {:.3}s, {} missed publications, loop confirmed={}",
            first_motion.unwrap().as_secs_f64() * 1000.0,
            seen,
            elapsed.as_secs_f64(),
            gaps,
            loop_seen
        );
        assert!(loop_seen);
        assert!(first_motion.unwrap() < Duration::from_millis(100));
        assert!(
            gaps <= 5,
            "playback should follow the GIF timeline without a frame backlog"
        );
        assert!(elapsed < Duration::from_secs(12));
    }
}
