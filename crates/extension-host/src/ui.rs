use super::*;
#[derive(Default)]
pub(super) struct Tree {
    next: u64,
    nodes: BTreeMap<u64, UiNode>,
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
                UiParent::Mount(name) if name == "statusbar.extensions" => break,
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
            },
        );
        self.dirty = true;
        Ok(node)
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
        Some(self.nodes.values().cloned().collect())
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
}
