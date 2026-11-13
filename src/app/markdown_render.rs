//! Runtime-owned layout worker. UI projections contain immutable values only.
use crate::domain::view::ReaderPosition;
use crate::features::markdown::{
    layout::*,
    model::SourceByte,
    reader::{ReadingAnchor, Viewport},
};
use crate::infrastructure::markdown_worker::Consumer;
use crate::infrastructure::markdown_worker::{BudgetLease, CacheKind, MemoryBudget, Publication};
use crate::ui::markdown::{
    native::{Ink, NativeBackend},
    reader::{FrameKey, ParagraphGeometry},
};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

#[derive(Clone, Debug, PartialEq)]
pub struct RequestKey {
    pub parse: u64,
    pub width: f32,
    pub height: f32,
    pub position: ReaderPosition,
    pub ink: Ink,
    pub density: f32,
    pub font_size: f32,
    pub line_height: f32,
    pub base: Option<super::markdown_assets::Base>,
    pub remote: bool,
    /// Lay out as a page (as the Markdown preview does) even though the request
    /// comes from a card, such as the readme of an extension.
    pub page: bool,
    pub fragment: Option<String>,
    pub horizontal: Vec<(crate::features::markdown::model::NodeId, f32)>,
}
#[derive(Clone)]
pub struct Request {
    pub view: Consumer,
    pub publication: Arc<Publication>,
    pub key: RequestKey,
}
#[derive(Clone)]
pub struct Tile {
    pub paragraph: usize,
    pub top: f32,
    pub left: f32,
    pub width: f32,
    pub height: f32,
    pub density: f32,
    /// Content hash of `png`; a pointer would collide once the allocation is reused.
    pub hash: u64,
    pub png: Arc<Vec<u8>>,
}
pub struct Rendered {
    pub key: RequestKey,
    pub frame: FrameKey,
    pub layout: Arc<DocumentLayout>,
    pub viewport: Viewport,
    pub geometry: Arc<[ParagraphGeometry]>,
    pub tiles: Arc<[Tile]>,
    pub images: std::collections::HashMap<
        crate::features::markdown::model::NodeId,
        Arc<super::markdown_assets::Asset>,
    >,
    _layout: Arc<LayoutEntry>,
    _lease: BudgetLease,
}
struct LayoutEntry {
    publication: Arc<Publication>,
    width: f32,
    ink: Ink,
    resources: u64,
    base: Option<super::markdown_assets::Base>,
    remote: bool,
    page: bool,
    layout: Arc<DocumentLayout>,
    _lease: BudgetLease,
}
#[derive(Clone)]
pub enum Status {
    Pending,
    Ready(Arc<Rendered>),
    Failed(String),
}
#[derive(Default)]
struct Queue {
    wanted: HashMap<Consumer, RequestKey>,
    pending: VecDeque<Request>,
    ready: HashMap<Consumer, Status>,
    last: HashMap<Consumer, Request>,
    clock: u64,
    /// Images finished by loader threads, waiting to enter the render thread's cache.
    completed: Vec<super::markdown_assets::Fetched>,
    /// Loader threads running now.
    loading: usize,
}
struct Shared {
    context: super::context::AppContext,
    queue: Mutex<Queue>,
    wake: Condvar,
    alive: AtomicBool,
    budget: MemoryBudget,
    notify: Box<dyn Fn() + Send + Sync>,
    #[cfg(test)]
    before_image_load: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}
pub struct RenderWorker {
    shared: Arc<Shared>,
}
impl RenderWorker {
    pub fn new(
        budget: MemoryBudget,
        context: super::context::AppContext,
        notify: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let shared = Arc::new(Shared {
            context,
            queue: Mutex::default(),
            wake: Condvar::new(),
            alive: AtomicBool::new(true),
            budget,
            notify: Box::new(notify),
            #[cfg(test)]
            before_image_load: Mutex::default(),
        });
        let worker = shared.clone();
        thread::Builder::new()
            .name("loog-markdown-layout".into())
            .spawn(move || run(worker))
            .expect("Markdown layout worker");
        Self { shared }
    }
    #[cfg(test)]
    pub(super) fn before_image_load(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *self.shared.before_image_load.lock().unwrap() = Some(hook);
    }
    pub fn request(&self, request: Request) -> Status {
        if !self.shared.alive.load(Ordering::Acquire) {
            return Status::Failed("Markdown service stopped".into());
        }
        let mut queue = self.shared.queue.lock().unwrap();
        if queue.wanted.get(&request.view) == Some(&request.key) {
            return compatible_status(queue.ready.get(&request.view), &request.key);
        }
        if queue.wanted.len() >= 64 && !queue.wanted.contains_key(&request.view) {
            return Status::Failed("Markdown view budget exceeded".into());
        }
        queue.pending.retain(|old| old.view != request.view);
        queue.wanted.insert(request.view, request.key.clone());
        queue.last.insert(request.view, request.clone());
        let previous = compatible_status(queue.ready.get(&request.view), &request.key);
        queue.pending.push_back(request);
        drop(queue);
        self.shared.wake.notify_one();
        previous
    }
    pub fn latest(&self, view: Consumer) -> Option<Arc<Rendered>> {
        match self.shared.queue.lock().unwrap().ready.get(&view) {
            Some(Status::Ready(value)) => Some(value.clone()),
            _ => None,
        }
    }
    pub fn is_current(&self, view: Consumer, frame: FrameKey) -> bool {
        self.latest(view).is_some_and(|value| value.frame == frame)
    }
    pub fn retain(&self, views: &std::collections::HashSet<Consumer>) {
        let mut queue = self.shared.queue.lock().unwrap();
        queue.wanted.retain(|view, _| views.contains(view));
        queue.last.retain(|view, _| views.contains(view));
        queue
            .pending
            .retain(|request| views.contains(&request.view));
        let retired = queue
            .ready
            .extract_if(|view, _| !views.contains(view))
            .collect::<Vec<_>>();
        drop(queue);
        crate::infrastructure::ui_dispatch::retire(retired);
    }
    pub fn shutdown(&self) {
        self.shared.alive.store(false, Ordering::Release);
        let mut queue = self.shared.queue.lock().unwrap();
        queue.wanted.clear();
        queue.last.clear();
        let pending = std::mem::take(&mut queue.pending);
        let ready = std::mem::take(&mut queue.ready);
        drop(queue);
        self.shared.wake.notify_all();
        crate::infrastructure::ui_dispatch::retire((pending, ready));
    }
}
impl Drop for RenderWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}
/// Geometry is bounded to the raster overscan window. A newly scrolled host
/// may keep old pixels briefly, but must not hit-test an unrelated text window.
pub(super) fn geometry_for(
    result: &Rendered,
    viewport: Viewport,
    horizontal: &HashMap<crate::features::markdown::model::NodeId, f32>,
) -> Arc<[ParagraphGeometry]> {
    let same_horizontal = horizontal.iter().all(|(node, value)| {
        result
            .key
            .horizontal
            .iter()
            .find(|(id, _)| id == node)
            .map_or(0.0, |(_, offset)| *offset)
            == *value
    }) && result
        .key
        .horizontal
        .iter()
        .all(|(node, value)| horizontal.get(node).copied().unwrap_or(0.0) == *value);
    if same_horizontal
        && viewport.top >= result.viewport.top - 256.0
        && viewport.top + viewport.height <= result.viewport.top + result.viewport.height + 256.0
    {
        result.geometry.clone()
    } else {
        Arc::from([])
    }
}
fn compatible_status(status: Option<&Status>, key: &RequestKey) -> Status {
    match status {
        Some(Status::Ready(old))
            if old.key.parse == key.parse
                && old.key.width == key.width
                && old.key.ink == key.ink
                && old.key.base == key.base
                && old.key.remote == key.remote
                && old.key.page == key.page
                && old.key.font_size == key.font_size
                && old.key.line_height == key.line_height =>
        {
            Status::Ready(old.clone())
        }
        Some(Status::Failed(message)) => Status::Failed(message.clone()),
        _ => Status::Pending,
    }
}
fn current(shared: &Shared, request: &Request) -> bool {
    shared.alive.load(Ordering::Acquire)
        && shared.queue.lock().unwrap().wanted.get(&request.view) == Some(&request.key)
}
fn run(shared: Arc<Shared>) {
    // Reserve the bounded native paragraph cache conservatively. Skia's temporary
    // allocator is measured separately from the retained Rust/result ledger.
    let native = shared.budget.reserve(CacheKind::Layout, 16 * 1024 * 1024);
    let mut assets = super::markdown_assets::Assets::new(shared.budget.clone());
    let mut snippets = crate::features::syntax::SnippetHighlighter::new(
        crate::infrastructure::language_packs::resources(),
    );
    let mut syntax_generation = snippets.generation();
    let mut backend = NativeBackend::default();
    let mut layouts = VecDeque::<Arc<LayoutEntry>>::new();
    loop {
        let request = {
            let mut queue = shared.queue.lock().unwrap();
            loop {
                if !shared.alive.load(Ordering::Acquire) {
                    return;
                }
                if !queue.completed.is_empty() {
                    // New image sizes change the layout, which the cache sees as
                    // a new resource version; render every view again.
                    for fetched in std::mem::take(&mut queue.completed) {
                        assets.store(fetched);
                    }
                    for request in queue.last.values().cloned().collect::<Vec<_>>() {
                        queue.pending.retain(|old| old.view != request.view);
                        queue.pending.push_back(request);
                    }
                }
                if let Some(request) = queue.pending.pop_front() {
                    break request;
                }
                let (next, timeout) = shared
                    .wake
                    .wait_timeout(queue, std::time::Duration::from_secs(1))
                    .unwrap();
                queue = next;
                if timeout.timed_out() {
                    drop(queue);
                    let generation = snippets.generation();
                    let changed = assets.refresh() || generation != syntax_generation;
                    syntax_generation = generation;
                    if changed {
                        layouts.clear();
                    }
                    queue = shared.queue.lock().unwrap();
                    if changed {
                        let retired = std::mem::take(&mut queue.ready);
                        let requests = queue.last.values().cloned().collect::<Vec<_>>();
                        queue.pending.extend(requests);
                        drop(queue);
                        drop(retired);
                        (shared.notify)();
                        queue = shared.queue.lock().unwrap();
                    }
                }
            }
        };
        if !current(&shared, &request) {
            continue;
        }
        let result = if native.is_some() {
            render(
                &shared,
                &request,
                &mut backend,
                &mut layouts,
                &mut assets,
                &mut snippets,
            )
        } else {
            Err("Markdown native layout memory budget exceeded".into())
        };
        let mut queue = shared.queue.lock().unwrap();
        if !shared.alive.load(Ordering::Acquire)
            || queue.wanted.get(&request.view) != Some(&request.key)
        {
            continue;
        }
        queue.ready.insert(
            request.view,
            match result {
                Ok(result) => Status::Ready(Arc::new(result)),
                Err(message) => Status::Failed(message),
            },
        );
        drop(queue);
        (shared.notify)();
    }
}
/// Loader threads running at once; more would only compete for the network.
const MAX_LOADERS: usize = 4;
/// Read one image on its own thread. The result joins the render thread's
/// cache through `Queue::completed`, which also wakes the worker.
fn start_load(
    shared: &Arc<Shared>,
    assets: &mut super::markdown_assets::Assets,
    base: Option<&super::markdown_assets::Base>,
    destination: &str,
    remote: bool,
) {
    if assets.loading(base, destination, remote) {
        return;
    }
    {
        let mut queue = shared.queue.lock().unwrap();
        if queue.loading >= MAX_LOADERS {
            // Retried by the render that follows any finished load.
            return;
        }
        queue.loading += 1;
    }
    assets.begin(base, destination, remote);
    let (base, destination) = (base.cloned(), destination.to_owned());
    let worker = shared.clone();
    let spawned = thread::Builder::new()
        .name("loog-markdown-image".into())
        .spawn(move || {
            #[cfg(test)]
            {
                let hook = worker.before_image_load.lock().unwrap().clone();
                if let Some(hook) = hook {
                    hook();
                }
            }
            let fetched = super::markdown_assets::Assets::fetch(
                base.as_ref(),
                &destination,
                remote,
                Some(&worker.context),
                &|| !worker.alive.load(Ordering::Acquire),
            );
            let mut queue = worker.queue.lock().unwrap();
            queue.loading -= 1;
            queue.completed.push(fetched);
            drop(queue);
            worker.wake.notify_one();
        });
    if spawned.is_err() {
        shared.queue.lock().unwrap().loading -= 1;
    }
}
fn render(
    shared: &Arc<Shared>,
    request: &Request,
    backend: &mut NativeBackend,
    layouts: &mut VecDeque<Arc<LayoutEntry>>,
    assets: &mut super::markdown_assets::Assets,
    snippets: &mut crate::features::syntax::SnippetHighlighter,
) -> Result<Rendered, String> {
    let key = &request.key;
    let density = key.density;
    if !density.is_finite() || density <= 0.0 {
        return Err("Invalid Markdown device scale".into());
    }
    // Bound overscan in physical pixels, without silently reducing text density.
    let overscan = 128.0 / density;
    let document = request
        .publication
        .outcome
        .as_ref()
        .map_err(|failure| format!("Markdown parse limit: {:?}", failure.reason))?
        .clone();
    if !key.width.is_finite()
        || !key.height.is_finite()
        || key.width > 4096.0
        || key.height > 4096.0
    {
        return Err("Markdown viewport exceeds layout budget".into());
    }
    backend.ink = key.ink;
    let entry = if let Some(index) = layouts.iter().position(|entry| {
        entry.publication.key == request.publication.key
            && entry.publication.revision == key.parse
            && entry.width == key.width
            && entry.ink == key.ink
            && entry.layout.style.scale == key.font_size / 16.0
            && entry.layout.style.line_height == key.line_height
            && entry.resources == assets.version
            && entry.base == key.base
            && entry.remote == key.remote
            && entry.page == key.page
    }) {
        let entry = layouts.remove(index).unwrap();
        layouts.push_back(entry.clone());
        entry
    } else {
        // A card is the compact layout of hovers and completion details; a page
        // is the one of the preview. A card may ask for the page layout.
        let compact = matches!(request.view, Consumer::Card(_)) && !key.page;
        let style = LayoutStyle {
            width: key.width,
            scale: key.font_size / 16.0,
            line_height: key.line_height,
            padding: if compact { 0.0 } else { 12.0 },
            gap: if compact { 8.0 } else { 12.0 },
            card: compact,
            ..Default::default()
        };
        let states = document
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| {
                if let crate::features::markdown::model::NodeKind::Image { destination, .. } =
                    &node.kind
                {
                    // No cached asset means the image is still loading.
                    let asset = assets.get(key.base.as_ref(), destination, key.remote)?;
                    let state = match &asset.outcome {
                        Ok(image) => ImageState::Loaded(ImageSize {
                            width: image.width as f32,
                            height: image.height as f32,
                        }),
                        Err(_) => ImageState::Failed,
                    };
                    Some((crate::features::markdown::model::NodeId(index), state))
                } else {
                    None
                }
            })
            .collect();
        let layout = layout_with_images(
            document,
            style,
            LayoutLimits {
                paragraph_bytes: 16 * 1024,
                ..Default::default()
            },
            backend,
            || !current(shared, request),
            &states,
        )
        .map_err(|failure| {
            format!(
                "Markdown layout unavailable: {:?}. Toggle the preview to read the complete source.",
                failure.reason
            )
        })?;
        let bytes = layout.retained_bytes();
        let lease = loop {
            if let Some(lease) = shared.budget.reserve(CacheKind::Layout, bytes) {
                break lease;
            }
            if layouts.pop_front().is_none() {
                return Err("Markdown layout memory budget exceeded".into());
            }
        };
        let entry = Arc::new(LayoutEntry {
            publication: request.publication.clone(),
            width: key.width,
            ink: key.ink,
            resources: assets.version,
            base: key.base.clone(),
            remote: key.remote,
            page: key.page,
            layout: Arc::new(layout),
            _lease: lease,
        });
        if layouts.len() >= 8 {
            layouts.pop_front();
        }
        layouts.push_back(entry.clone());
        entry
    };
    let layout = &entry.layout;
    let mut viewport = Viewport::new(key.height);
    viewport.restore(
        layout,
        ReadingAnchor {
            source: SourceByte(key.position.source_byte),
            fraction: key.position.block_fraction,
        },
    );
    if let Some(fragment) = &key.fragment {
        if let Some((_, node)) = crate::features::markdown::resources::headings(&layout.document)
            .iter()
            .find(|(slug, _)| slug == fragment)
        {
            viewport.restore(
                layout,
                ReadingAnchor {
                    source: layout.document.nodes[node.0].source.start,
                    fraction: 0.0,
                },
            );
        }
    }
    let mut images = HashMap::new();
    for image in &layout.images {
        if image.rect.bottom < viewport.top || image.rect.top > viewport.top + viewport.height {
            continue;
        }
        if let crate::features::markdown::model::NodeKind::Image { destination, .. } =
            &layout.document.nodes[image.node.0].kind
        {
            // Text must not wait for files or the network: an image that is not
            // cached yet loads on its own thread and the view renders again once
            // it arrives; until then it keeps its placeholder.
            match assets.get(key.base.as_ref(), destination, key.remote) {
                Some(asset) => {
                    images.insert(image.node, asset);
                }
                None => start_load(shared, assets, key.base.as_ref(), destination, key.remote),
            }
        }
    }
    let mut geometry = Vec::new();
    let mut tiles = Vec::new();
    let mut bytes = 0usize;
    let mut glyph_total = 0usize;
    for box_index in viewport.visible(layout, 256.0) {
        for paragraph in layout.boxes[box_index].paragraphs.clone() {
            if !current(shared, request) {
                return Err("Cancelled".into());
            }
            let placed = &layout.paragraphs[paragraph];
            let offset = layout
                .scroll_container(paragraph)
                .and_then(|node| {
                    key.horizontal
                        .iter()
                        .find(|(id, _)| *id == node)
                        .map(|(_, value)| value.clamp(0.0, layout.max_scroll(node)))
                })
                .unwrap_or(0.0);
            if placed.wrap
                && (placed.x + placed.viewport_width - offset <= 0.0
                    || placed.x - offset >= key.width)
            {
                continue;
            }
            let mut r = placed.request(&layout.document, layout.style);
            let mut highlighted = Vec::new();
            let muted = placed.spans.iter().any(|span| span.style.muted);
            if let crate::features::markdown::model::NodeKind::CodeBlock {
                language: Some(language),
                ..
            } = &layout.document.nodes[placed.node.0].kind
            {
                let cancel = AtomicBool::new(!current(shared, request));
                let tokens = snippets.highlight(language, r.text, &cancel);
                if !tokens.is_empty() {
                    for token in tokens {
                        highlighted.push(StyleSpan {
                            bytes: token.range,
                            style: InlineStyle {
                                code: true,
                                muted,
                                foreground: token.class.map(|class| key.ink.syntax[class.index()]),
                                ..Default::default()
                            },
                        });
                    }
                }
            }
            if !highlighted.is_empty() {
                r.spans = &highlighted;
            }
            let top = ((viewport.top - overscan - placed.y).max(0.0) * density).floor() / density;
            let bottom = (viewport.top + viewport.height + overscan - placed.y)
                .min(placed.summary.height)
                .ceil();
            if bottom <= top {
                continue;
            }
            let left = if placed.wrap {
                (offset - placed.x).max(0.0)
            } else {
                offset
            };
            let width = if placed.wrap {
                (placed.viewport_width - left)
                    .min(key.width - (placed.x - offset).max(0.0))
                    .max(1.0)
            } else {
                placed.viewport_width.min(key.width - placed.x).max(1.0)
            };
            // A chip's padding reaches past the text column; the tile covers it.
            let (left, width) = if placed.wrap && r.spans.iter().any(|span| span.style.chip) {
                (left - 4.0, width + 8.0)
            } else {
                (left, width)
            };
            let mut y = top;
            while y < bottom {
                // Keep the native tile allocation bounded even at large device scales.
                let tile_height = (4_194_304.0 / (width * density).ceil().max(1.0))
                    .floor()
                    .min(256.0 * density)
                    .max(1.0)
                    / density;
                let height = (bottom - y).min(tile_height);
                let png = backend
                    .tile(&r, left, y, width, height, density)
                    .map_err(|reason| format!("Markdown paint: {reason:?}"))?;
                bytes = bytes
                    .saturating_add(png.capacity())
                    .saturating_add((width * height * density * density * 4.0).ceil() as usize);
                if bytes > 24 * 1024 * 1024 {
                    return Err("Markdown visible raster budget exceeded".into());
                }
                tiles.push(Tile {
                    paragraph,
                    top: y,
                    left,
                    width,
                    height,
                    density,
                    hash: {
                        use std::hash::{Hash, Hasher};
                        let mut hasher = std::collections::hash_map::DefaultHasher::new();
                        png.hash(&mut hasher);
                        hasher.finish()
                    },
                    png,
                });
                y += height;
            }
            let mut glyphs = Vec::new();
            for line in &placed.summary.lines {
                if line.top + line.height < top || line.top > bottom {
                    continue;
                }
                let visible = if !placed.wrap {
                    backend
                        .visible_scalars(&r, left, left + width, line.top + line.height * 0.5)
                        .map_err(|reason| format!("Markdown visible range: {reason:?}"))?
                } else {
                    line.scalars.clone()
                };
                let mut start = visible.start.0.max(line.scalars.start.0);
                let end = visible.end.0.min(line.scalars.end.0);
                while start < end {
                    let candidate = (start + 512).min(end);
                    let byte = placed
                        .index
                        .byte(ScalarIndex(candidate))
                        .ok_or("Invalid paragraph coordinate")?;
                    let floor = crate::features::markdown::reader::grapheme_floor(r.text, byte);
                    let next = placed
                        .index
                        .scalar(floor)
                        .ok_or("Invalid grapheme coordinate")?
                        .0;
                    if next <= start {
                        return Err("Markdown grapheme exceeds geometry budget".into());
                    }
                    let part = backend
                        .geometry(&r, ScalarIndex(start)..ScalarIndex(next), 512)
                        .map_err(|reason| format!("Markdown geometry: {reason:?}"))?;
                    glyph_total += part.len();
                    if glyph_total > 8192 {
                        return Err("Markdown visible geometry budget exceeded".into());
                    }
                    glyphs.extend(part);
                    start = next;
                }
            }
            bytes =
                bytes.saturating_add(glyphs.capacity() * std::mem::size_of::<GraphemeGeometry>());
            geometry.push(ParagraphGeometry {
                paragraph,
                glyphs: glyphs.into(),
            });
        }
    }
    let lease = shared
        .budget
        .reserve(CacheKind::Image, bytes)
        .ok_or("Markdown retained frame budget exceeded")?;
    let frame = {
        let mut queue = shared.queue.lock().unwrap();
        queue.clock = queue.clock.wrapping_add(1);
        FrameKey {
            parse: key.parse,
            layout: queue.clock,
        }
    };
    Ok(Rendered {
        key: key.clone(),
        frame,
        layout: layout.clone(),
        viewport,
        geometry: geometry.into(),
        tiles: tiles.into(),
        images,
        _layout: entry,
        _lease: lease,
    })
}
