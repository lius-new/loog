//! Owned widget descriptions; no window IDs or rendering framework types.
use std::{collections::BTreeMap, ops::Range, sync::Arc};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Block,
    Text,
    Icon,
    Button,
    Row,
    Column,
    TextInput,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Properties {
    pub id: Arc<str>,
    pub kind: Kind,
    pub text: Option<String>,
    pub icon: Option<Arc<str>>,
    pub foreground: Option<u32>,
    pub background: Option<u32>,
    pub width: Option<f32>,
    pub height: Option<f32>,
    pub padding: f32,
    pub gap: f32,
    pub visible: bool,
    pub label: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn node(id: u64, parent: Option<u64>, visible: bool) -> FlatNode {
        FlatNode {
            node: id,
            parent,
            properties: Properties {
                id: format!("n{id}").into(),
                kind: Kind::Block,
                text: None,
                icon: None,
                foreground: None,
                background: None,
                width: None,
                height: None,
                padding: 0.0,
                gap: 0.0,
                visible,
                label: None,
            },
        }
    }
    #[test]
    fn unchanged_subtrees_are_shared_and_visibility_follows_ancestors() {
        let mut model = Model::default();
        model.begin_session(1);
        model.replace(
            1,
            "a".into(),
            1,
            vec![
                node(1, None, true),
                node(2, Some(1), true),
                node(3, None, true),
            ],
        );
        let stable = model.roots()[1].clone();
        let events = model.replace(
            1,
            "a".into(),
            1,
            vec![
                node(1, None, false),
                node(2, Some(1), true),
                node(3, None, true),
            ],
        );
        assert!(Arc::ptr_eq(&stable, &model.roots()[1]));
        assert_eq!(events.len(), 2);
        assert!(
            events
                .iter()
                .all(|(_, payload)| *payload == Payload::Visibility(false))
        );
        assert!(model.owns(&stable.origin));
        model.begin_session(2);
        assert!(!model.owns(&stable.origin));
        model.replace(2, "a".into(), 1, vec![node(1, None, true)]);
        model.remove(1, "a");
        assert_eq!(model.roots().len(), 1);
        model.replace(1, "a".into(), 1, vec![]);
        assert_eq!(model.roots().len(), 1);
        model.end_session(2);
        model.replace(2, "a".into(), 1, vec![node(1, None, true)]);
        assert!(
            model.roots().is_empty(),
            "late snapshots cannot revive an unloaded host"
        );
    }
    #[test]
    fn public_ids_cannot_collide_across_ambiguous_owner_names() {
        let first = Origin {
            owner: "a".into(),
            session: 1,
            generation: 2,
            node: 1,
            id: "1.2.n1".into(),
        };
        let second = Origin {
            owner: "a.1.2".into(),
            session: 1,
            generation: 2,
            node: 1,
            id: "n1".into(),
        };
        assert_ne!(first.public_id(), second.public_id());
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    pub owner: Arc<str>,
    pub session: u64,
    pub generation: u64,
    pub node: u64,
    pub id: Arc<str>,
}
impl Origin {
    pub fn public_id(&self) -> Arc<str> {
        format!(
            "ext.{}:{}.{}.{}.{}",
            self.owner.len(),
            self.owner,
            self.session,
            self.generation,
            self.id
        )
        .into()
    }
}
#[derive(Clone, Debug, PartialEq)]
pub enum Payload {
    Click,
    Input(String),
    Selection(Range<usize>),
    Visibility(bool),
    Focus(bool),
}
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub origin: Origin,
    pub properties: Properties,
    pub children: Vec<Arc<Node>>,
}
pub struct FlatNode {
    pub node: u64,
    pub parent: Option<u64>,
    pub properties: Properties,
}
#[derive(Clone, Default)]
struct Owner {
    generation: u64,
    roots: Vec<Arc<Node>>,
    nodes: BTreeMap<u64, Arc<Node>>,
    visible: BTreeMap<u64, bool>,
}
#[derive(Clone, Default)]
pub struct Model {
    session: u64,
    active: bool,
    owners: BTreeMap<Arc<str>, Owner>,
}
impl Model {
    pub fn roots(&self) -> Vec<Arc<Node>> {
        self.owners
            .values()
            .flat_map(|owner| owner.roots.iter().cloned())
            .collect()
    }
    pub fn owns(&self, origin: &Origin) -> bool {
        self.owners.get(&origin.owner).is_some_and(|owner| {
            owner.generation == origin.generation
                && owner
                    .nodes
                    .get(&origin.node)
                    .is_some_and(|node| node.origin == *origin)
        })
    }
    pub fn begin_session(&mut self, session: u64) {
        if session > self.session {
            self.session = session;
            self.active = true;
            self.owners.clear();
        }
    }
    pub fn end_session(&mut self, session: u64) {
        if self.session == session {
            self.active = false;
            self.owners.clear();
        }
    }
    pub fn remove(&mut self, session: u64, owner: &str) {
        if self.session == session {
            self.owners.remove(owner);
        }
    }
    pub fn replace(
        &mut self,
        session: u64,
        owner: Arc<str>,
        generation: u64,
        nodes: Vec<FlatNode>,
    ) -> Vec<(Origin, Payload)> {
        if !self.active || session != self.session {
            return vec![];
        }
        let previous = self
            .owners
            .remove(&owner)
            .filter(|entry| entry.generation == generation)
            .unwrap_or_default();
        let mut next = Owner {
            generation,
            ..Default::default()
        };
        fn build(
            node: &FlatNode,
            flat: &[FlatNode],
            owner: &Arc<str>,
            session: u64,
            generation: u64,
            previous: &Owner,
            next: &mut Owner,
            parent_visible: bool,
        ) -> Arc<Node> {
            let origin = Origin {
                owner: owner.clone(),
                session,
                generation,
                node: node.node,
                id: node.properties.id.clone(),
            };
            let visible = parent_visible && node.properties.visible;
            let children = flat
                .iter()
                .filter(|child| child.parent == Some(node.node))
                .map(|child| {
                    build(
                        child, flat, owner, session, generation, previous, next, visible,
                    )
                })
                .collect();
            let model = Node {
                origin,
                properties: node.properties.clone(),
                children,
            };
            let shared = previous
                .nodes
                .get(&node.node)
                .filter(|old| old.as_ref() == &model)
                .cloned()
                .unwrap_or_else(|| Arc::new(model));
            next.nodes.insert(node.node, shared.clone());
            next.visible.insert(node.node, visible);
            shared
        }
        for root in nodes.iter().filter(|node| node.parent.is_none()) {
            let root = build(
                root, &nodes, &owner, session, generation, &previous, &mut next, true,
            );
            next.roots.push(root);
        }
        let mut events = vec![];
        for (id, node) in &next.nodes {
            let visible = next.visible[id];
            if previous.visible.get(id).copied() != Some(visible) {
                events.push((node.origin.clone(), Payload::Visibility(visible)));
            }
        }
        for (id, node) in &previous.nodes {
            if !next.nodes.contains_key(id) {
                events.push((node.origin.clone(), Payload::Visibility(false)));
            }
        }
        self.owners.insert(owner, next);
        events
    }
}
