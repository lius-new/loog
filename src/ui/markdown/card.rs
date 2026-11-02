//! Controlled memory-document host. The application supplies async content/layout
//! and owns its interaction state; cards share the file reader and paragraph port.
use super::reader::{self, Binding, ReaderEvent, ReaderFrame};
use crate::features::markdown::content::Markup;
use crate::ui::theme;
use lgui::prelude::{Element, UiRect, VisualStyle, panel, text};
use std::sync::Arc;
pub type Painter = dyn Fn(&ReaderFrame, usize, UiRect, UiRect) -> Element + Send + Sync;
pub type ImagePainter = dyn Fn(&ReaderFrame, &crate::features::markdown::layout::ImageBox, UiRect) -> Element
    + Send
    + Sync;
#[derive(Clone)]
pub struct Frame {
    pub reader: ReaderFrame,
    pub paint: Arc<Painter>,
    pub paint_image: Arc<ImagePainter>,
}
#[derive(Default)]
pub struct Effect {
    pub selection_changed: bool,
    pub selected: Option<String>,
    pub link: Option<String>,
    pub copy: Option<String>,
}
pub struct Port {
    pub copy: Box<dyn Fn(String) + Send + Sync>,
    pub resolve: Box<dyn Fn(f32, f32) -> Result<Option<Frame>, String> + Send + Sync>,
    pub event: Box<dyn Fn(ReaderEvent) -> Effect + Send + Sync>,
}
#[derive(Clone)]
pub struct Document {
    pub content: Markup,
    pub port: Option<Arc<Port>>,
}
impl From<Markup> for Document {
    fn from(content: Markup) -> Self {
        Self {
            content,
            port: None,
        }
    }
}
impl Document {
    pub fn is_empty(&self) -> bool {
        self.content.trim().is_empty()
    }
    pub fn bind(mut self, port: Port) -> Self {
        self.port = Some(Arc::new(port));
        self
    }
}
#[derive(Clone)]
pub struct Layout {
    pub width: f32,
    pub height: f32,
    /// False while the document is still being prepared, so `height` is only a
    /// placeholder and a card sized from it would jump once the real one arrives.
    pub measured: bool,
    pub document: Document,
}
pub fn layout(document: &Document, width: f32) -> Layout {
    let resolved = document
        .port
        .as_ref()
        .map(|port| (port.resolve)(width.max(40.0), 0.0));
    let measured = !matches!(resolved, Some(Ok(None)));
    let ready = resolved.and_then(|result| result.ok().flatten());
    Layout {
        width,
        measured,
        height: ready
            .as_ref()
            .map_or(22.0, |frame| frame.reader.layout.height),
        document: document.clone(),
    }
}
#[derive(Clone, Default)]
pub struct Interaction {
    pub on_link: Option<Arc<dyn Fn(String) + Send + Sync>>,
    pub on_select: Option<Arc<dyn Fn(Option<String>) + Send + Sync>>,
}
pub fn render(
    layout: &Layout,
    left: f32,
    top: f32,
    height: f32,
    interaction: Option<&Interaction>,
) -> Element {
    let document = layout.document.clone();
    let interaction = interaction.cloned().unwrap_or_default();
    let rect = UiRect::new(left, top, left + layout.width, top + height.max(1.0));
    lgui::prelude::component(rect, move |cx, _| {
        let id = cx.use_stable_id();
        let focus = cx.focus_handle(id.clone());
        // Until the document is ready the card stays empty. A parse usually takes a
        // frame or two, and a notice that appears and goes at once reads as a flash.
        let Some(port) = document.port.clone() else {
            return panel(rect, VisualStyle::default());
        };
        let frame = match (port.resolve)(rect.width(), rect.height()) {
            Ok(Some(frame)) => frame,
            Ok(None) => return panel(rect, VisualStyle::default()),
            Err(error) => return text(rect, error, theme::sans(theme::c().warning, 12.0)).into(),
        };
        let read = frame.reader.clone();
        let paint = frame.paint.clone();
        let paint_image = frame.paint_image.clone();
        let actions = interaction.clone();
        let binding = Binding::new(
            move || read.clone(),
            move |event| {
                let effect = (port.event)(event);
                if let Some(copy) = effect.copy {
                    (port.copy)(copy);
                }
                if let Some(link) = effect.link {
                    if let Some(open) = &actions.on_link {
                        open(link);
                    }
                }
                // Host selection is a value; logical copy belongs to the reader.
                if effect.selection_changed {
                    if let Some(select) = &actions.on_select {
                        select(effect.selected);
                    }
                }
            },
            move |frame, index, bounds, clip| paint(frame, index, bounds, clip),
        )
        .with_images(move |frame, image, bounds| paint_image(frame, image, bounds));
        reader::render(id, rect, binding, focus)
    })
}
