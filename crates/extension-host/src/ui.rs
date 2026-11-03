use super::*;
#[derive(Default)]
pub(super) struct Tree {
    next: u64,
    nodes: BTreeMap<u64, UiNode>,
    sequence: Vec<u64>,
    sibling_orders: BTreeMap<u64, (u64, u8, i32)>,
    dirty: bool,
    operations: usize,
}
impl Tree {
    pub fn begin_round(&mut self) {
        self.operations = 0;
    }
    fn budget(&mut self) -> Result<(), HostError> {
        if self.operations >= 64 {
            return Err(host_error(
                ErrorKind::LimitExceeded,
                "UI operation budget exceeded for this round",
            ));
        }
        self.operations += 1;
        Ok(())
    }
    fn validate(
        &self,
        description: &UiDescription,
        replacing: Option<u64>,
    ) -> Result<(), HostError> {
        let bytes = |d: &UiDescription| {
            d.id.len()
                + d.text.as_ref().map_or(0, String::len)
                + d.icon.as_ref().map_or(0, String::len)
                + d.label.as_ref().map_or(0, String::len)
        };
        if description.id.is_empty()
            || description.id.len() > 128
            || description.id.contains('\0')
            || description.text.as_ref().is_some_and(|s| s.len() > 4096)
            || description.icon.as_ref().is_some_and(|s| s.len() > 256)
            || description.label.as_ref().is_some_and(|s| s.len() > 4096)
        {
            return Err(host_error(
                ErrorKind::LimitExceeded,
                "UI identifiers or text exceed their budget",
            ));
        }
        if [description.width, description.height]
            .into_iter()
            .flatten()
            .any(|n| !n.is_finite() || !(0.0..=4096.0).contains(&n))
            || [description.padding, description.gap]
                .into_iter()
                .any(|n| !n.is_finite() || !(0.0..=128.0).contains(&n))
        {
            return Err(host_error(ErrorKind::InvalidEdit, "invalid UI dimensions"));
        }
        if self
            .nodes
            .iter()
            .any(|(id, node)| Some(*id) != replacing && node.description.id == description.id)
        {
            return Err(host_error(ErrorKind::Conflict, "duplicate UI identifier"));
        }
        let total: usize = self
            .nodes
            .iter()
            .filter(|(id, _)| Some(**id) != replacing)
            .map(|(_, n)| bytes(&n.description))
            .sum();
        if total + bytes(description) > 64 * 1024 {
            return Err(host_error(
                ErrorKind::LimitExceeded,
                "UI text budget exceeded",
            ));
        }
        Ok(())
    }
    pub fn create(
        &mut self,
        parent: UiParent,
        description: UiDescription,
    ) -> Result<u64, HostError> {
        self.budget()?;
        self.validate(&description, None)?;
        if self.nodes.len() >= 256 {
            return Err(host_error(
                ErrorKind::LimitExceeded,
                "UI node budget exceeded",
            ));
        }
        let mut ancestor = &parent;
        let mut depth = 1;
        loop {
            match ancestor {
                // COMPAT(legacy-ui-mount): Accept the old statusbar mount for existing components.
                // New roots use ui.detached plus loom:ui/tree relative placement.
                // Remove only the statusbar alternative when legacy mount support ends;
                // no removal version has been scheduled. ui.detached remains required.
                UiParent::Mount(name)
                    if name == "statusbar.extensions" || name == "ui.detached" =>
                {
                    break;
                }
                UiParent::Mount(_) => {
                    return Err(host_error(
                        ErrorKind::PermissionDenied,
                        "that host node is not an extension mount",
                    ));
                }
                UiParent::Node(id) => {
                    let node = self.nodes.get(id).ok_or_else(|| {
                        host_error(
                            ErrorKind::ExpiredHandle,
                            "UI parent does not belong to this instance",
                        )
                    })?;
                    if !matches!(
                        node.description.kind,
                        UiKind::Block | UiKind::Row | UiKind::Column | UiKind::Button
                    ) {
                        return Err(host_error(
                            ErrorKind::InvalidEdit,
                            "this widget cannot contain children",
                        ));
                    }
                    ancestor = &node.parent;
                    depth += 1;
                    if depth > 16 {
                        return Err(host_error(
                            ErrorKind::LimitExceeded,
                            "UI depth budget exceeded",
                        ));
                    }
                }
            }
        }
        self.next += 1;
        let node = self.next;
        self.nodes.insert(
            node,
            UiNode {
                node,
                parent,
                description,
                placement: None,
            },
        );
        self.sequence.push(node);
        self.dirty = true;
        Ok(node)
    }
    pub fn place(&mut self, node: u64, placement: UiPlacement) -> Result<(), HostError> {
        self.budget()?;
        if !self.nodes.contains_key(&node) {
            return Err(host_error(
                ErrorKind::ExpiredHandle,
                "UI node does not belong to this instance",
            ));
        }
        let mut parent = UiParent::Mount("ui.detached".into());
        let mut root_placement = Some(placement.clone());
        let mut sibling = None;
        match &placement.target {
            UiTarget::PublicId(id) => {
                if id.is_empty()
                    || id.len() > 256
                    || id.contains('\0')
                    || id.starts_with("ext.")
                    || id == "ui.detached"
                {
                    return Err(host_error(
                        ErrorKind::InvalidEdit,
                        "invalid public UI target",
                    ));
                }
            }
            UiTarget::Node(target) => {
                let target_node = self.nodes.get(target).ok_or_else(|| {
                    host_error(
                        ErrorKind::ExpiredHandle,
                        "UI target does not belong to this instance",
                    )
                })?;
                let mut pending = vec![*target];
                let mut seen = std::collections::BTreeSet::new();
                while let Some(current) = pending.pop() {
                    if current == node {
                        return Err(host_error(
                            ErrorKind::InvalidEdit,
                            "UI placement would create a cycle",
                        ));
                    }
                    if !seen.insert(current) {
                        continue;
                    }
                    if let Some(current) = self.nodes.get(&current) {
                        if let UiParent::Node(parent) = current.parent {
                            pending.push(parent);
                        }
                        if let Some(UiPlacement {
                            target: UiTarget::Node(target),
                            ..
                        }) = &current.placement
                        {
                            pending.push(*target);
                        }
                    }
                }
                if matches!(placement.relation, UiRelation::Append) {
                    if !matches!(
                        target_node.description.kind,
                        UiKind::Block | UiKind::Row | UiKind::Column | UiKind::Button
                    ) {
                        return Err(host_error(
                            ErrorKind::InvalidEdit,
                            "this widget cannot contain children",
                        ));
                    }
                    parent = UiParent::Node(*target);
                    root_placement = None;
                } else if target_node.placement.is_none() {
                    parent = target_node.parent.clone();
                    root_placement = None;
                }
                sibling = Some(*target);
            }
        }
        // Reparenting an existing subtree must obey the same depth limit as
        // creation. Validate the proposed graph before changing either map.
        for candidate in self.nodes.keys() {
            let mut current = Some(*candidate);
            let mut depth = 0;
            while let Some(id) = current {
                depth += 1;
                if depth > 16 {
                    return Err(host_error(
                        ErrorKind::LimitExceeded,
                        "UI depth budget exceeded",
                    ));
                }
                let proposed = if id == node {
                    &parent
                } else {
                    &self.nodes[&id].parent
                };
                current = match proposed {
                    UiParent::Node(id) => Some(*id),
                    _ => None,
                };
            }
        }
        if let Some(target) = sibling {
            self.sibling_orders.remove(&node);
            self.sequence.retain(|id| *id != node);
            let side = match placement.relation {
                UiRelation::Before => 0,
                UiRelation::After => 1,
                UiRelation::Append => 2,
            };
            self.sibling_orders
                .insert(node, (target, side, placement.order));
            let mut group: Vec<_> = self
                .sibling_orders
                .iter()
                .filter(|(_, (anchor, relation, _))| *anchor == target && *relation == side)
                .map(|(id, (_, _, order))| (*id, *order))
                .collect();
            group.sort_by(|(a, ao), (b, bo)| {
                (ao, self.nodes[a].description.id.as_str())
                    .cmp(&(bo, self.nodes[b].description.id.as_str()))
            });
            self.sequence
                .retain(|id| !group.iter().any(|(member, _)| member == id));
            let index = if matches!(placement.relation, UiRelation::Append) {
                self.sequence.len()
            } else {
                self.sequence.iter().position(|id| *id == target).unwrap()
                    + usize::from(matches!(placement.relation, UiRelation::After))
            };
            self.sequence
                .splice(index..index, group.into_iter().map(|(id, _)| id));
        } else {
            self.sibling_orders.remove(&node);
        }
        let current = self.nodes.get_mut(&node).unwrap();
        current.parent = parent;
        current.placement = root_placement;
        self.dirty = true;
        Ok(())
    }

    pub fn update(&mut self, node: u64, description: UiDescription) -> Result<(), HostError> {
        self.budget()?;
        let old = self.nodes.get(&node).ok_or_else(|| {
            host_error(
                ErrorKind::ExpiredHandle,
                "UI node does not belong to this instance",
            )
        })?;
        if old.description.id != description.id {
            return Err(host_error(
                ErrorKind::InvalidEdit,
                "UI identity cannot be changed",
            ));
        }
        if !matches!(
            description.kind,
            UiKind::Block | UiKind::Row | UiKind::Column | UiKind::Button
        ) && self
            .nodes
            .values()
            .any(|n| matches!(n.parent, UiParent::Node(id) if id == node))
        {
            return Err(host_error(
                ErrorKind::InvalidEdit,
                "a parent widget must keep its children",
            ));
        }
        self.validate(&description, Some(node))?;
        self.nodes.get_mut(&node).unwrap().description = description;
        self.dirty = true;
        Ok(())
    }
    pub fn remove(&mut self, node: u64) -> Result<(), HostError> {
        self.budget()?;
        if !self.nodes.contains_key(&node) {
            return Err(host_error(
                ErrorKind::ExpiredHandle,
                "UI node does not belong to this instance",
            ));
        }
        let mut gone = std::collections::BTreeSet::from([node]);
        loop {
            let descendants = self
                .nodes
                .values()
                .filter_map(|n| match n.parent {
                    UiParent::Node(parent) if gone.contains(&parent) && !gone.contains(&n.node) => {
                        Some(n.node)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            if descendants.is_empty() {
                break;
            }
            gone.extend(descendants);
        }
        self.nodes.retain(|id, _| !gone.contains(id));
        self.sequence.retain(|id| !gone.contains(id));
        self.sibling_orders
            .retain(|id, (target, _, _)| !gone.contains(id) && !gone.contains(target));
        self.dirty = true;
        Ok(())
    }
    pub fn dirty(&self) -> bool {
        self.dirty
    }
    pub fn take(&mut self) -> Option<Vec<UiNode>> {
        if !std::mem::take(&mut self.dirty) {
            return None;
        }
        Some(
            self.sequence
                .iter()
                .filter_map(|id| self.nodes.get(id).cloned())
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn props(id: &str) -> UiDescription {
        UiDescription {
            id: id.into(),
            kind: UiKind::Block,
            text: None,
            icon: None,
            foreground: None,
            background: Some(0x123abc),
            width: None,
            height: None,
            padding: 0.0,
            gap: 0.0,
            visible: true,
            label: None,
        }
    }
    #[test]
    fn mounts_ownership_identity_and_invalid_updates_are_checked_atomically() {
        let mut tree = Tree::default();
        assert!(
            tree.create(UiParent::Mount("statusbar".into()), props("x"))
                .is_err()
        );
        assert!(tree.create(UiParent::Node(99), props("x")).is_err());
        let parent = tree
            .create(UiParent::Mount("statusbar.extensions".into()), props("x"))
            .unwrap();
        let child = tree.create(UiParent::Node(parent), props("child")).unwrap();
        assert!(tree.create(UiParent::Node(parent), props("child")).is_err());
        let mut invalid = props("x");
        invalid.width = Some(f32::NAN);
        assert!(tree.update(parent, invalid).is_err());
        assert!(tree.update(parent, props("renamed")).is_err());
        assert_eq!(tree.nodes[&parent].description.id, "x");
        tree.remove(parent).unwrap();
        assert!(tree.nodes.is_empty());
        assert!(tree.update(child, props("child")).is_err());
    }
    #[test]
    fn operations_and_depth_have_independent_budgets() {
        let mut tree = Tree::default();
        let mut parent = UiParent::Mount("statusbar.extensions".into());
        for index in 0..16 {
            let id = tree.create(parent, props(&format!("n{index}"))).unwrap();
            parent = UiParent::Node(id);
        }
        assert!(tree.create(parent, props("too-deep")).is_err());
        tree.begin_round();
        for _ in 0..64 {
            tree.update(1, props("n0")).unwrap();
        }
        assert!(tree.update(1, props("n0")).is_err());
        tree.begin_round();
        assert!(tree.update(1, props("n0")).is_ok());
    }

    #[test]
    fn owned_append_orders_by_priority_and_identity_across_moves() {
        let mut tree = Tree::default();
        let root = tree
            .create(UiParent::Mount("ui.detached".into()), props("root"))
            .unwrap();
        let z = tree.create(UiParent::Node(root), props("z")).unwrap();
        let b = tree.create(UiParent::Node(root), props("b")).unwrap();
        let a = tree.create(UiParent::Node(root), props("a")).unwrap();
        for (node, order) in [(z, 10), (b, -1), (a, -1)] {
            tree.place(
                node,
                UiPlacement {
                    target: UiTarget::Node(root),
                    relation: UiRelation::Append,
                    order,
                },
            )
            .unwrap();
        }
        let children = |snapshot: Vec<UiNode>| {
            snapshot
                .into_iter()
                .filter(|node| matches!(node.parent, UiParent::Node(id) if id == root))
                .map(|node| node.description.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(children(tree.take().unwrap()), ["a", "b", "z"]);
        tree.place(
            z,
            UiPlacement {
                target: UiTarget::Node(root),
                relation: UiRelation::Append,
                order: -2,
            },
        )
        .unwrap();
        assert_eq!(children(tree.take().unwrap()), ["z", "a", "b"]);
        tree.remove(root).unwrap();
        assert!(tree.nodes.is_empty());
    }

    #[test]
    fn relative_placement_checks_ownership_cycles_and_preserves_identity() {
        let mut tree = Tree::default();
        let root = tree
            .create(UiParent::Mount("ui.detached".into()), props("root"))
            .unwrap();
        let child = tree.create(UiParent::Node(root), props("child")).unwrap();
        let public = UiPlacement {
            target: UiTarget::PublicId("toolbar.any-component".into()),
            relation: UiRelation::Before,
            order: 10,
        };
        tree.place(root, public).unwrap();
        assert!(
            tree.place(
                root,
                UiPlacement {
                    target: UiTarget::Node(child),
                    relation: UiRelation::Append,
                    order: 0
                }
            )
            .is_err()
        );
        assert!(
            tree.place(
                child,
                UiPlacement {
                    target: UiTarget::Node(999),
                    relation: UiRelation::After,
                    order: 0
                }
            )
            .is_err()
        );
        assert!(
            tree.place(
                child,
                UiPlacement {
                    target: UiTarget::PublicId("ext.5:other.status".into()),
                    relation: UiRelation::Before,
                    order: 0
                }
            )
            .is_err()
        );
        let sibling = tree
            .create(UiParent::Mount("ui.detached".into()), props("sibling"))
            .unwrap();
        tree.place(
            sibling,
            UiPlacement {
                target: UiTarget::Node(root),
                relation: UiRelation::Before,
                order: 0,
            },
        )
        .unwrap();
        assert!(
            tree.place(
                root,
                UiPlacement {
                    target: UiTarget::Node(sibling),
                    relation: UiRelation::After,
                    order: 0
                }
            )
            .is_err()
        );
        let snapshot = tree.take().unwrap();
        assert!(
            snapshot
                .iter()
                .any(|node| node.node == root && node.placement.is_some())
        );
        assert!(tree.nodes.contains_key(&child));
        tree.remove(root).unwrap();
        assert!(!tree.nodes.contains_key(&child));
        assert!(tree.nodes.contains_key(&sibling));
    }
}
