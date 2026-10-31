//! Runtime card host: source ownership, async shared parsing and controlled input.
use super::{
    markdown_render::{Request, RequestKey, Status},
    markdown_service::MarkdownService,
};
use crate::{
    domain::{identity::ViewId, view::ReaderPosition},
    features::markdown::{
        content::Markup,
        model::*,
        reader::{InteractionState, ReadingAnchor},
    },
    infrastructure::markdown_worker::{Consumer, ContentOrigin, Demand, ParseKey, ParseStatus},
    ui::{
        markdown::{
            card::{Document, Effect, Frame, Port},
            native::Ink,
            reader::{ReaderEvent, ReaderFrame},
        },
        theme,
    },
};
use std::{
    collections::HashSet,
    sync::{Arc, atomic::Ordering},
};
pub(super) struct Card {
    pub view: ViewId,
    pub input_generation: u64,
    content: Markup,
    key: ParseKey,
    interaction: InteractionState,
    position: ReaderPosition,
    pub focused: bool,
    height: f32,
    extension: Option<(String, std::path::PathBuf)>,
}
impl MarkdownService {
    pub(super) fn refresh_demands(&self) {
        let mut demands = self.files.lock().unwrap().clone();
        let cards = self.cards.lock().unwrap();
        demands.extend(cards.iter().map(|(&owner, card)| Demand {
            consumer: Consumer::Card(owner),
            key: card.key,
            source: card.content.text.clone(),
            active: true,
        }));
        let clients = demands.iter().map(|demand| demand.consumer).collect();
        drop(cards);
        self.worker.set_demands(demands);
        self.render.retain(&clients);
    }
    pub fn bind_card(
        self: &Arc<Self>,
        view: ViewId,
        input_generation: u64,
        slot: u8,
        document: Document,
    ) -> Document {
        self.bind_document(view, input_generation, slot, document, None)
    }
    pub fn bind_extension_card(
        self: &Arc<Self>,
        view: ViewId,
        input_generation: u64,
        id: &str,
        tab: crate::ui::extensions_binding::DetailTab,
        document: &crate::ui::extensions_binding::Documentation,
    ) -> Document {
        let slot = if tab == crate::ui::extensions_binding::DetailTab::Details {
            4
        } else {
            5
        };
        self.bind_document(
            view,
            input_generation,
            slot,
            document.content.clone().into(),
            Some((id.to_owned(), document.path.clone())),
        )
    }
    fn bind_document(
        self: &Arc<Self>,
        view: ViewId,
        input_generation: u64,
        slot: u8,
        document: Document,
        extension: Option<(String, std::path::PathBuf)>,
    ) -> Document {
        let owner = view.get().wrapping_mul(16).wrapping_add(slot as u64);
        let content = document.content.clone();
        let identity = content.identity.clone();
        {
            let mut cards = self.cards.lock().unwrap();
            if cards.get(&owner).is_none_or(|card| {
                card.content.identity != content.identity
                    || card.input_generation != input_generation
            }) {
                let revision = self.card_revision.fetch_add(1, Ordering::Relaxed);
                cards.insert(
                    owner,
                    Card {
                        view,
                        input_generation,
                        content: content.clone(),
                        key: ParseKey {
                            origin: ContentOrigin::Markup { owner, revision },
                            generation: 0,
                            format: content.format,
                            syntax: SyntaxOptions::default(),
                            limits: if extension.is_some() {
                                ParseLimits {
                                    source_bytes:
                                        crate::features::extensions::manifest::MAX_DOCUMENT_BYTES
                                            as usize,
                                    ..ParseLimits::automatic()
                                }
                            } else {
                                ParseLimits {
                                    source_bytes: 64 * 1024,
                                    display_bytes: 256 * 1024,
                                    events: 20_000,
                                    nodes: 20_000,
                                    retained_bytes: 4 * 1024 * 1024,
                                    ..Default::default()
                                }
                            },
                        },
                        interaction: InteractionState::default(),
                        position: ReaderPosition::default(),
                        focused: false,
                        height: 480.0,
                        extension: extension.clone(),
                    },
                );
            }
        }
        self.refresh_demands();
        let read = self.clone();
        let write = self.clone();
        let copying = self.clone();
        let event_identity = identity.clone();
        document.bind(Port {
            copy: Box::new(move |text| {
                if let Err(error) = copying.clipboard.write_text(&text) {
                    copying.copy_errors.send(error.to_string());
                }
            }),
            resolve: Box::new(move |width, height| {
                read.card_frame(owner, &identity, width, height)
            }),
            event: Box::new(move |event| {
                let mut effect = write.card_event(owner, &event_identity, event);
                if let Some((_, base)) = &extension {
                    if let Some(link) = effect.link.take() {
                        write.open_extension_link(view, base, &link);
                    }
                }
                write.schedule_drag(Consumer::Card(owner));
                effect
            }),
        })
    }
    pub fn retain_cards(&self, view: ViewId, slots: &HashSet<u8>) {
        self.cards
            .lock()
            .unwrap()
            .retain(|owner, card| card.view != view || slots.contains(&((*owner % 16) as u8)));
        self.refresh_demands();
    }
    fn card_frame(
        &self,
        owner: u64,
        identity: &Arc<[u64]>,
        width: f32,
        height: f32,
    ) -> Result<Option<Frame>, String> {
        let (key, position, selection, focused, horizontal, height, scrollbar_dragging, base) = {
            let mut cards = self.cards.lock().unwrap();
            let card = cards
                .get_mut(&owner)
                .filter(|card| card.content.identity == *identity)
                .ok_or("Documentation superseded")?;
            if height > 0.0 {
                card.height = height.clamp(1.0, 2048.0);
            }
            (
                card.key,
                card.position,
                card.interaction.selection,
                card.focused,
                card.interaction.horizontal.clone(),
                card.height,
                card.interaction.scrollbar.is_some(),
                card.extension.as_ref().map(|(_, path)| path.clone()),
            )
        };
        let publication = match self.worker.query(Consumer::Card(owner), key) {
            ParseStatus::Ready(value) => value,
            ParseStatus::Declined(reason) => return Err(reason.message().into()),
            ParseStatus::Pending => return Ok(None),
            ParseStatus::Stopped => return Err("Documentation service stopped".into()),
        };
        let ink = Ink::current();
        let (font_size, line_height) = if base.is_some() {
            let app = self.context.get();
            let settings = &app.preferences.values().markdown;
            let size = settings.font_size as f32;
            (size, (settings.line_height as f32).max(size * 1.2))
        } else {
            (theme::UI_SIZE, 18.0)
        };
        let result = match self.render.request(Request {
            view: Consumer::Card(owner),
            key: RequestKey {
                parse: publication.revision,
                // The measuring call passes the layout width and the painting call the
                // width of the rect built from it, which can differ by float error;
                // as separate keys they evicted each other and the card flickered.
                width: width.max(40.0).round(),
                height: height.clamp(1.0, 2048.0),
                position,
                ink,
                density: crate::ui::device_scale::observed(),
                font_size,
                line_height,
                remote: base.is_some(),
                // An extension's readme reads like a Markdown page, not a hover card.
                page: base.is_some(),
                base,
                fragment: None,
                horizontal: {
                    let mut entries = horizontal
                        .iter()
                        .map(|(id, value)| (*id, *value))
                        .collect::<Vec<_>>();
                    entries.sort_by_key(|(id, _)| id.0);
                    entries
                },
            },
            publication,
        }) {
            Status::Ready(value) => value,
            Status::Pending => return Ok(None),
            Status::Failed(error) => return Err(error),
        };
        let mut viewport = result.viewport;
        viewport.restore(
            &result.layout,
            ReadingAnchor {
                source: SourceByte(position.source_byte),
                fraction: position.block_fraction,
            },
        );
        let reader = ReaderFrame {
            scrollbar_dragging,
            key: result.frame,
            document: result.layout.document.clone(),
            layout: result.layout.clone(),
            viewport,
            selection,
            geometry: super::markdown_render::geometry_for(&result, viewport, &horizontal),
            geometry_key: result.frame,
            horizontal,
            focused,
            extend_selection: false,
        };
        let paint = result.clone();
        let images = result.clone();
        Ok(Some(Frame {
            reader,
            paint: Arc::new(move |_, paragraph, bounds, _| {
                super::markdown_view::paint(&paint, paragraph, bounds)
            }),
            paint_image: Arc::new(move |frame, image, bounds| {
                super::markdown_view::paint_image(&images, frame, image, bounds)
            }),
        }))
    }
    fn card_event(&self, owner: u64, identity: &Arc<[u64]>, event: ReaderEvent) -> Effect {
        let mut effect = Effect::default();
        let result = self.render.latest(Consumer::Card(owner));
        let app = self.context.get();
        let mut cards = self.cards.lock().unwrap();
        if !cards.get(&owner).is_some_and(|card| {
            card.content.identity == *identity && current_host(&app, owner, card)
        }) {
            return effect;
        }
        drop(app);
        if matches!(&event, ReaderEvent::Focus(true)) {
            for card in cards.values_mut() {
                card.focused = false;
            }
        }
        let Some(card) = cards
            .get_mut(&owner)
            .filter(|card| card.content.identity == *identity)
        else {
            return effect;
        };
        if let ReaderEvent::Focus(focused) = event {
            card.focused = focused;
            if !focused {
                crate::features::markdown::reader::cancel_gesture(&mut card.interaction);
            }
            return effect;
        }
        let Some(result) = result else {
            if event.ends_gesture() {
                crate::features::markdown::reader::cancel_gesture(&mut card.interaction);
                if matches!(event, ReaderEvent::Focus(false)) {
                    card.focused = false;
                }
            }
            return effect;
        };
        if !matches!(self.worker.query(Consumer::Card(owner),card.key),ParseStatus::Ready(ref publication) if publication.revision==result.frame.parse)
        {
            if event.ends_gesture() {
                crate::features::markdown::reader::cancel_gesture(&mut card.interaction);
                if matches!(event, ReaderEvent::Focus(false)) {
                    card.focused = false;
                }
            }
            return effect;
        }
        let Some(event) = event.on_frame(result.frame) else {
            return effect;
        };
        let mut viewport = result.viewport;
        viewport.restore(
            &result.layout,
            ReadingAnchor {
                source: SourceByte(card.position.source_byte),
                fraction: card.position.block_fraction,
            },
        );
        let frame = ReaderFrame {
            scrollbar_dragging: card.interaction.scrollbar.is_some(),
            key: result.frame,
            document: result.layout.document.clone(),
            layout: result.layout.clone(),
            viewport,
            selection: card.interaction.selection,
            geometry: super::markdown_render::geometry_for(
                &result,
                viewport,
                &card.interaction.horizontal,
            ),
            geometry_key: result.frame,
            horizontal: card.interaction.horizontal.clone(),
            focused: card.focused,
            extend_selection: false,
        };
        let (viewport, output) =
            crate::features::markdown::reader::reduce(&mut card.interaction, &frame, event);
        if let Some(focused) = output.focus {
            card.focused = focused;
        }
        let anchor = viewport.anchor(&result.layout);
        card.position = ReaderPosition {
            source_byte: anchor.source.0,
            block_fraction: anchor.fraction,
        };
        effect.selection_changed = output.selection_changed;
        effect.selected = card
            .interaction
            .selection
            .and_then(|selection| selection.copy(&result.layout.document, result.frame.parse))
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        effect.copy = output.copy;
        effect.link = output
            .link
            .and_then(|id| match &result.layout.document.nodes[id.0].kind {
                NodeKind::Link { destination, .. } => Some(destination.clone()),
                _ => None,
            });
        if card.extension.is_some() {
            if let Some(fragment) = effect.link.as_ref().and_then(|link| link.strip_prefix('#')) {
                if let Some((_, node)) =
                    crate::features::markdown::resources::headings(&result.layout.document)
                        .iter()
                        .find(|(slug, _)| slug == fragment)
                {
                    card.position = ReaderPosition {
                        source_byte: result.layout.document.nodes[node.0].source.start.0,
                        block_fraction: 0.0,
                    };
                    effect.link = None;
                }
            }
        }
        drop(cards);
        self.notify.send(());
        effect
    }
    fn open_extension_link(&self, view: ViewId, base: &std::path::Path, link: &str) {
        use crate::features::markdown::resources::{Address, resolve};
        match resolve(Some(base), link) {
            Some(Address::Web(url)) => super::language_ui::open_link(&self.context, url),
            Some(Address::Local { path, .. }) if path.is_dir() => {
                super::explorer::reveal_folder(&self.context, &path)
            }
            Some(Address::Local { path, fragment }) => {
                let pane = self.context.get().workspace.pane_of(view);
                if let Some(pane) = pane {
                    super::markdown_view::open_local(&self.context, pane, path, fragment);
                }
            }
            Some(Address::Anchor(fragment)) => self
                .context
                .update(|app| app.show_error(format!("Heading not found: {fragment}"))),
            None => self
                .context
                .update(|app| app.show_error("Unsupported Markdown link")),
        }
    }
    pub fn card_key_action(
        &self,
        action: crate::features::keymap::action::Action,
        clipboard: &dyn lgui::services::Clipboard,
    ) -> Result<bool, String> {
        use crate::features::keymap::action::Action;
        let target = self
            .cards
            .lock()
            .unwrap()
            .iter()
            .find(|(_, card)| card.focused)
            .map(|(&owner, card)| (owner, card.content.identity.clone()));
        let Some((owner, identity)) = target else {
            return Ok(false);
        };
        let Some(result) = self.render.latest(Consumer::Card(owner)) else {
            return Ok(false);
        };
        let event = match action {
            Action::Copy => ReaderEvent::Copy { key: result.frame },
            Action::SelectAll => ReaderEvent::SelectAll { key: result.frame },
            Action::Cancel => ReaderEvent::ClearSelection { key: result.frame },
            _ => return Ok(action.editor_command(1).is_some()),
        };
        let effect = self.card_event(owner, &identity, event);
        if let Some(value) = effect.copy {
            clipboard
                .write_text(&value)
                .map_err(|error| error.to_string())?;
        }
        Ok(true)
    }
}

pub(super) fn current_host(app: &super::state::AppState, owner: u64, card: &Card) -> bool {
    let Some(view) = app
        .workspace
        .view(card.view)
        .filter(|view| view.input_generation == card.input_generation)
    else {
        return false;
    };
    let Some(pane) = app.workspace.pane_of(view.id).filter(|pane| {
        app.workspace
            .pane(*pane)
            .is_some_and(|pane| pane.active() == Some(view.id))
    }) else {
        return false;
    };
    if let Some((id, _)) = &card.extension {
        use crate::ui::extensions_binding::DetailTab;
        return view.target
            == crate::domain::view::ViewTarget::Builtin(crate::domain::view::AppPage::Extensions)
            && app
                .extensions_ui
                .selected
                .as_ref()
                .is_none_or(|selected| selected == id)
            && matches!(
                (owner % 16, app.extensions_ui.tab),
                (4, DetailTab::Details) | (5, DetailTab::Changelog)
            );
    }
    if !view.accepts_text() {
        return false;
    }
    match owner % 16 {
        1 => view
            .document()
            .and_then(|id| {
                app.workspace
                    .documents
                    .get(&id)
                    .map(|document| document.stamp(id))
            })
            .and_then(|stamp| app.editor.language.hover.card_for(pane, &stamp))
            .is_some_and(|hover| hover.documentation.identity == card.content.identity),
        2 => app
            .editor
            .language
            .completion
            .as_ref()
            .filter(|session| session.view == view.id)
            .and_then(|session| {
                session
                    .shown
                    .get(session.selected)
                    .and_then(|shown| session.data.entries.get(shown.entry))
            })
            .and_then(|entry| entry.documentation.as_ref())
            .is_some_and(|doc| doc.identity == card.content.identity),
        3 => app
            .editor
            .language
            .signature
            .as_ref()
            .filter(|session| session.view == view.id)
            .and_then(super::language_ui::signature::view)
            .and_then(|view| view.docs)
            .is_some_and(|doc| doc.identity == card.content.identity),
        _ => false,
    }
}
impl MarkdownService {
    pub(super) fn card_drag_active(&self, owner: u64) -> bool {
        let cards = self.cards.lock().unwrap();
        let Some(card) = cards.get(&owner) else {
            return false;
        };
        card.focused
            && card.interaction.pressed.is_some()
            && card.interaction.drag_point.is_some_and(|point| {
                let Some(result) = self.render.latest(Consumer::Card(owner)) else {
                    return false;
                };
                let mut viewport = result.viewport;
                viewport.height = card.height;
                viewport.restore(
                    &result.layout,
                    crate::features::markdown::reader::ReadingAnchor {
                        source: crate::features::markdown::model::SourceByte(
                            card.position.source_byte,
                        ),
                        fraction: card.position.block_fraction,
                    },
                );
                let (vertical, horizontal) = crate::features::markdown::reader::drag_scroll_delta(
                    &result.layout,
                    viewport,
                    point,
                );
                vertical != 0.0 || horizontal.is_some()
            })
    }
    pub(super) fn tick_card_drag(&self, owner: u64) {
        let Some(result) = self.render.latest(Consumer::Card(owner)) else {
            return;
        };
        let target = {
            let cards = self.cards.lock().unwrap();
            cards.get(&owner).and_then(|card| {
                card.interaction
                    .drag_point
                    .map(|point| (card.content.identity.clone(), point))
            })
        };
        let Some((identity, point)) = target else {
            return;
        };
        let Ok(Some(frame)) =
            self.card_frame(owner, &identity, result.key.width, result.key.height)
        else {
            return;
        };
        let hit = crate::features::markdown::reader::hit_at(
            &frame.reader.layout,
            &frame.reader.geometry,
            &frame.reader.horizontal,
            frame.reader.viewport,
            point,
        );
        let effect = self.card_event(
            owner,
            &identity,
            ReaderEvent::PointerDrag {
                key: frame.reader.key,
                hit,
                point,
            },
        );
        if effect.selection_changed {
            self.context.update(|app| match owner % 16 {
                1 => app.editor.language.hover.selection = effect.selected,
                2 => {
                    if let Some(session) = app.editor.language.completion.as_ref() {
                        app.editor.language.docs_selection =
                            effect.selected.map(|text| (session.selected, text));
                    }
                }
                3 => {
                    if let Some(session) = app.editor.language.signature.as_ref() {
                        app.editor.language.signature_selection =
                            effect.selected.map(|text| (session.revision, text));
                    }
                }
                _ => {}
            });
        }
    }
}
