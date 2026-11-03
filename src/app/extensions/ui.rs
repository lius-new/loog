use super::*;
use crate::features::extensions::ui::{
    Attachment, FlatNode, Kind, Origin, Payload, Properties, Relation,
};
use loom_extension_host::{
    UiEvent, UiEventPayload, UiKind, UiNode, UiParent, UiRelation, UiTarget,
};
use std::sync::Arc;
impl ExtensionService {
    pub(super) fn ui_changed(
        &mut self,
        app: &mut AppState,
        id: &str,
        generation: u64,
        nodes: Vec<UiNode>,
    ) -> Vec<Effect> {
        if !self
            .packages
            .get(id)
            .is_some_and(|p| p.lifecycle.accepts(generation))
        {
            return vec![];
        }
        let public_ids: std::collections::HashMap<_, _> = nodes
            .iter()
            .map(|node| {
                (
                    node.node,
                    Origin {
                        owner: id.into(),
                        session: self.ui_session,
                        generation,
                        node: node.node,
                        id: node.description.id.clone().into(),
                    }
                    .public_id(),
                )
            })
            .collect();
        let nodes = nodes
            .into_iter()
            .map(|node| {
                let p = node.description;
                let attachment = match node.placement {
                    Some(placement) => {
                        let target = match placement.target {
                            UiTarget::PublicId(target) => Some(target.into()),
                            UiTarget::Node(target) => public_ids.get(&target).cloned(),
                        };
                        target.map_or(Attachment::Detached, |target| Attachment::Relative {
                            target,
                            relation: match placement.relation {
                                UiRelation::Before => Relation::Before,
                                UiRelation::After => Relation::After,
                                UiRelation::Append => Relation::Append,
                            },
                            order: placement.order,
                        })
                    }
                    None if matches!(&node.parent, UiParent::Mount(name) if name == "ui.detached") => Attachment::Detached,
                    // COMPAT(legacy-ui-mount): Old statusbar roots have no placement metadata.
                    // New roots use Detached/Relative; owned children still take this branch.
                    // Remove the legacy root fallback when mount support ends, preserving
                    // owned-child handling. No removal version has been scheduled.
                    None => Attachment::Legacy,
                };
                FlatNode {
                    node: node.node,
                    attachment,
                    parent: match node.parent {
                        UiParent::Node(id) => Some(id),
                        UiParent::Mount(_) => None,
                    },
                    properties: Properties {
                        id: p.id.into(),
                        kind: match p.kind {
                            UiKind::Block => Kind::Block,
                            UiKind::Text => Kind::Text,
                            UiKind::Icon => Kind::Icon,
                            UiKind::Button => Kind::Button,
                            UiKind::Row => Kind::Row,
                            UiKind::Column => Kind::Column,
                            UiKind::TextInput => Kind::TextInput,
                        },
                        text: p.text,
                        icon: p.icon.map(Into::into),
                        foreground: p.foreground,
                        background: p.background,
                        width: p.width,
                        height: p.height,
                        padding: p.padding,
                        gap: p.gap,
                        visible: p.visible,
                        label: p.label,
                    },
                }
            })
            .collect();
        let events = app
            .extension_widgets
            .replace(self.ui_session, id.into(), generation, nodes);
        let mut effects = vec![];
        for (origin, payload) in events {
            effects.extend(self.queue_ui_event(origin, payload));
        }
        effects
    }
    pub fn ui_event(&mut self, app: &AppState, origin: Origin, payload: Payload) -> Vec<Effect> {
        if origin.session != self.ui_session || !app.extension_widgets.owns(&origin) {
            return vec![];
        }
        self.queue_ui_event(origin, payload)
    }
    fn queue_ui_event(&mut self, origin: Origin, payload: Payload) -> Vec<Effect> {
        if !self
            .packages
            .get(origin.owner.as_ref())
            .is_some_and(|p| p.lifecycle.accepts(origin.generation))
        {
            return vec![];
        }
        let payload = match payload {
            Payload::Click => UiEventPayload::Click,
            Payload::Input(text) if text.len() <= 4096 => UiEventPayload::Input(text),
            Payload::Input(_) => return vec![],
            Payload::Selection(range) => {
                UiEventPayload::Selection(loom_extension_host::ByteRange {
                    start: range.start as u64,
                    end: range.end as u64,
                })
            }
            Payload::Visibility(visible) => UiEventPayload::Visibility(visible),
            Payload::Focus(focused) => UiEventPayload::Focus(focused),
        };
        let entry = self
            .ui_events
            .entry(origin.owner.to_string())
            .or_insert_with(|| (origin.generation, Arc::default()));
        if entry.0 != origin.generation {
            *entry = (origin.generation, Arc::default());
        }
        let mut queue = entry.1.lock().unwrap();
        let notify = queue.is_empty();
        // Continuous state events are coalesced. Clicks remain ordered; the
        // bounded queue also covers a guest temporarily slower than UI input.
        if !matches!(payload, UiEventPayload::Click) {
            let kind = std::mem::discriminant(&payload);
            queue.retain(|event| {
                event.node != origin.node || std::mem::discriminant(&event.payload) != kind
            });
        }
        if queue.len() >= 512 {
            queue.remove(0);
        }
        queue.push(UiEvent {
            node: origin.node,
            id: origin.public_id().to_string(),
            payload,
        });
        drop(queue);
        if notify {
            vec![Effect::Submit(Job::UiEvents {
                id: origin.owner.to_string(),
                generation: origin.generation,
                events: entry.1.clone(),
            })]
        } else {
            vec![]
        }
    }
}
