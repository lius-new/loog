use super::*;

/// What an extension asked the host to do with one node's look while it is composited.
/// `revision` changes with every request, so a consumer can tell something is new.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct UiMotion {
    pub revision: u64,
    /// Native monotonic start time; not part of the guest ABI or serialized data.
    #[serde(skip)]
    pub started_at: Option<std::time::Instant>,
    pub resting: Vec<(MotionProperty, f32)>,
    pub smoothing: Vec<MotionSmoothing>,
    /// The revision it was started at, and the animation itself.
    pub animation: Option<(u64, MotionAnimation)>,
}

const MAX_ANIMATED_NODES: usize = 16;
const MAX_KEYFRAMES: usize = 16;
const MAX_MOTION_MS: u32 = 3_600_000;

fn motion_error(message: &str) -> HostError {
    host_error(ErrorKind::InvalidEdit, message)
}

fn value_allowed(property: MotionProperty, value: f32) -> bool {
    value.is_finite()
        && match property {
            MotionProperty::Rotation => value.abs() <= 36_000.0,
            MotionProperty::ScaleX | MotionProperty::ScaleY => (0.0..=64.0).contains(&value),
            MotionProperty::TranslateX | MotionProperty::TranslateY => value.abs() <= 10_000.0,
            MotionProperty::Opacity => (0.0..=1.0).contains(&value),
        }
}

fn easing_allowed(easing: &MotionEasing) -> bool {
    match easing {
        MotionEasing::Linear => true,
        MotionEasing::CubicBezier((x1, y1, x2, y2)) => {
            (0.0..=1.0).contains(x1)
                && (0.0..=1.0).contains(x2)
                && y1.is_finite()
                && y2.is_finite()
                && y1.abs() <= 4.0
                && y2.abs() <= 4.0
        }
    }
}

fn check_animation(animation: &MotionAnimation) -> Result<(), HostError> {
    if animation.tracks.is_empty() || animation.tracks.len() > 6 {
        return Err(motion_error("an animation needs one to six tracks"));
    }
    if animation.duration_ms == 0
        || animation.duration_ms > MAX_MOTION_MS
        || animation.delay_ms > MAX_MOTION_MS
        || animation
            .iterations
            .is_some_and(|count| count == 0 || count > 1_000_000)
    {
        return Err(host_error(
            ErrorKind::LimitExceeded,
            "animation timing exceeds its budget",
        ));
    }
    for (index, track) in animation.tracks.iter().enumerate() {
        if animation.tracks[..index]
            .iter()
            .any(|other| other.property == track.property)
        {
            return Err(motion_error("an animation has one track per property"));
        }
        let keys = &track.keyframes;
        if keys.len() < 2 || keys.len() > MAX_KEYFRAMES {
            return Err(motion_error("a track needs two to sixteen keyframes"));
        }
        if keys.first().map(|key| key.offset) != Some(0.0)
            || keys.last().map(|key| key.offset) != Some(1.0)
            || keys
                .windows(2)
                .any(|pair| !(pair[0].offset <= pair[1].offset))
            || keys
                .iter()
                .any(|key| !value_allowed(track.property, key.value))
        {
            return Err(motion_error(
                "keyframes must run from offset 0 to 1 in order, with values in range",
            ));
        }
        if !easing_allowed(&track.easing) {
            return Err(motion_error("invalid easing curve"));
        }
    }
    Ok(())
}

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
                motion: UiMotion::default(),
                shared: None,
            },
        );
        self.sequence.push(node);
        self.dirty = true;
        Ok(node)
    }
    /// Publish only an owned root; shared keys never grant cross-instance mutation.
    pub fn publish_shared(
        &mut self,
        node: u64,
        mut contribution: UiContribution,
    ) -> Result<(), HostError> {
        self.budget()?;
        if contribution.key.is_empty()
            || contribution.key.len() > 128
            || contribution.key.contains('\0')
        {
            return Err(host_error(ErrorKind::InvalidEdit, "invalid shared UI key"));
        }
        if let Some(contexts) = &mut contribution.contexts {
            if contexts.len() > 256
                || contexts
                    .iter()
                    .any(|value| value.is_empty() || value.len() > 4096 || value.contains('\0'))
                || contexts.iter().map(String::len).sum::<usize>() > 32768
            {
                return Err(host_error(
                    ErrorKind::LimitExceeded,
                    "shared UI contexts exceed their budget",
                ));
            }
            contexts.sort();
            contexts.dedup();
        }
        let root = self.nodes.get_mut(&node).ok_or_else(|| {
            host_error(
                ErrorKind::ExpiredHandle,
                "UI node does not belong to this instance",
            )
        })?;
        if !matches!(&root.parent, UiParent::Mount(name) if name == "ui.detached")
            || !matches!(
                &root.placement,
                Some(UiPlacement {
                    target: UiTarget::PublicId(_),
                    ..
                })
            )
        {
            return Err(host_error(
                ErrorKind::InvalidEdit,
                "shared UI requires a root placed against a public target",
            ));
        }
        root.shared = Some(contribution);
        self.dirty = true;
        Ok(())
    }

    /// Count one operation and find the node a motion request is about.
    fn motion_node(&mut self, node: u64) -> Result<&mut UiNode, HostError> {
        self.budget()?;
        self.nodes.get_mut(&node).ok_or_else(|| {
            host_error(
                ErrorKind::ExpiredHandle,
                "UI node does not belong to this instance",
            )
        })
    }
    pub fn animate(&mut self, node: u64, animation: MotionAnimation) -> Result<(), HostError> {
        self.animate_at(node, animation, std::time::Instant::now())
    }
    fn animate_at(
        &mut self,
        node: u64,
        animation: MotionAnimation,
        now: std::time::Instant,
    ) -> Result<(), HostError> {
        check_animation(&animation)?;
        let animated = self
            .nodes
            .values()
            .filter(|other| {
                other.node != node
                    && other
                        .motion
                        .animation
                        .as_ref()
                        .is_some_and(|(_, animation)| {
                            animation.iterations.is_none_or(|iterations| {
                                let duration = u64::from(animation.delay_ms)
                                    + u64::from(animation.duration_ms) * u64::from(iterations);
                                other.motion.started_at.is_some_and(|start| {
                                    now.saturating_duration_since(start)
                                        < std::time::Duration::from_millis(duration)
                                })
                            })
                        })
            })
            .count();
        if animated >= MAX_ANIMATED_NODES {
            return Err(host_error(
                ErrorKind::LimitExceeded,
                "too many nodes are animated at once",
            ));
        }
        let target = self.motion_node(node)?;
        target.motion.revision += 1;
        target.motion.animation = Some((target.motion.revision, animation));
        target.motion.started_at = Some(now);
        self.dirty = true;
        Ok(())
    }
    pub fn stop_motion(&mut self, node: u64) -> Result<(), HostError> {
        let target = self.motion_node(node)?;
        if target.motion.animation.take().is_some() {
            target.motion.started_at = None;
            target.motion.revision += 1;
            self.dirty = true;
        }
        Ok(())
    }
    pub fn set_resting(
        &mut self,
        node: u64,
        property: MotionProperty,
        value: f32,
    ) -> Result<(), HostError> {
        if !value_allowed(property, value) {
            return Err(motion_error("a motion value is out of range"));
        }
        let target = self.motion_node(node)?;
        match target
            .motion
            .resting
            .iter_mut()
            .find(|(existing, _)| *existing == property)
        {
            Some((_, current)) => *current = value,
            None => target.motion.resting.push((property, value)),
        }
        target.motion.revision += 1;
        self.dirty = true;
        Ok(())
    }
    pub fn smooth(&mut self, node: u64, smoothing: MotionSmoothing) -> Result<(), HostError> {
        if smoothing.duration_ms > MAX_MOTION_MS || !easing_allowed(&smoothing.easing) {
            return Err(motion_error("invalid smoothing"));
        }
        let target = self.motion_node(node)?;
        target
            .motion
            .smoothing
            .retain(|existing| existing.property != smoothing.property);
        if smoothing.duration_ms > 0 {
            target.motion.smoothing.push(smoothing);
        }
        target.motion.revision += 1;
        self.dirty = true;
        Ok(())
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
        if self.nodes[&node].shared.is_some() && root_placement.is_none() {
            return Err(host_error(
                ErrorKind::InvalidEdit,
                "a shared root must remain placed against a public target",
            ));
        }
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
    fn shared_publication_is_owned_bounded_and_atomic() {
        let mut tree = Tree::default();
        let root = tree
            .create(UiParent::Mount("ui.detached".into()), props("region"))
            .unwrap();
        let contribution = || UiContribution {
            key: "example.region.v1".into(),
            contexts: Some(vec!["b".into(), "a".into(), "a".into()]),
            priority: 7,
        };
        assert!(
            tree.publish_shared(root, contribution()).is_err(),
            "requires explicit placement"
        );
        tree.place(
            root,
            UiPlacement {
                target: UiTarget::PublicId("native.anchor".into()),
                relation: UiRelation::Before,
                order: 10,
            },
        )
        .unwrap();
        assert!(tree.publish_shared(999, contribution()).is_err());
        tree.publish_shared(root, contribution()).unwrap();
        assert_eq!(
            tree.nodes[&root]
                .shared
                .as_ref()
                .unwrap()
                .contexts
                .as_ref()
                .unwrap(),
            &["a", "b"]
        );
        let mut bad = contribution();
        bad.key.clear();
        assert!(tree.publish_shared(root, bad).is_err());
        let mut bad = contribution();
        bad.contexts = Some(vec!["large".repeat(1000)]);
        assert!(tree.publish_shared(root, bad).is_err());
        assert_eq!(tree.nodes[&root].shared.as_ref().unwrap().priority, 7);
        tree.remove(root).unwrap();
        assert!(tree.nodes.is_empty());
    }

    fn spin() -> MotionAnimation {
        MotionAnimation {
            tracks: vec![MotionTrack {
                property: MotionProperty::Rotation,
                keyframes: vec![
                    MotionKeyframe {
                        offset: 0.0,
                        value: 0.0,
                    },
                    MotionKeyframe {
                        offset: 1.0,
                        value: 360.0,
                    },
                ],
                easing: MotionEasing::Linear,
            }],
            duration_ms: 900,
            delay_ms: 0,
            iterations: None,
            direction: MotionDirection::Normal,
            hold: false,
        }
    }

    #[test]
    fn an_animation_is_checked_whole_and_replaces_the_running_one() {
        let mut tree = Tree::default();
        let node = tree
            .create(UiParent::Mount("ui.detached".into()), props("icon"))
            .unwrap();
        tree.animate(node, spin()).unwrap();
        let started = tree.nodes[&node].motion.animation.as_ref().unwrap().0;
        tree.animate(node, spin()).unwrap();
        assert!(tree.nodes[&node].motion.animation.as_ref().unwrap().0 > started);

        let mut bad = spin();
        bad.tracks[0].keyframes[1].offset = 0.5; // never reaches 1
        assert!(tree.animate(node, bad).is_err());
        let mut bad = spin();
        bad.tracks[0].keyframes[1].value = f32::NAN;
        assert!(tree.animate(node, bad).is_err());
        let mut bad = spin();
        bad.duration_ms = 0;
        assert!(tree.animate(node, bad).is_err());
        let mut bad = spin();
        bad.tracks.push(bad.tracks[0].clone()); // two tracks for one property
        assert!(tree.animate(node, bad).is_err());
        let mut bad = spin();
        bad.tracks[0].easing = MotionEasing::CubicBezier((2.0, 0.0, 0.5, 1.0));
        assert!(tree.animate(node, bad).is_err());
        // A refused request leaves the running animation alone.
        assert!(tree.nodes[&node].motion.animation.is_some());

        tree.stop_motion(node).unwrap();
        assert!(tree.nodes[&node].motion.animation.is_none());
        assert!(tree.animate(99, spin()).is_err());
    }

    #[test]
    fn resting_values_and_smoothing_are_bounded_and_per_property() {
        let mut tree = Tree::default();
        let node = tree
            .create(UiParent::Mount("ui.detached".into()), props("switch"))
            .unwrap();
        tree.set_resting(node, MotionProperty::TranslateX, 12.0)
            .unwrap();
        tree.set_resting(node, MotionProperty::TranslateX, 14.0)
            .unwrap();
        assert_eq!(tree.nodes[&node].motion.resting.len(), 1);
        assert!(
            tree.set_resting(node, MotionProperty::Opacity, 1.5)
                .is_err()
        );
        assert!(
            tree.set_resting(node, MotionProperty::ScaleX, -1.0)
                .is_err()
        );
        let smoothing = |duration_ms| MotionSmoothing {
            property: MotionProperty::TranslateX,
            duration_ms,
            easing: MotionEasing::Linear,
        };
        tree.smooth(node, smoothing(150)).unwrap();
        tree.smooth(node, smoothing(200)).unwrap();
        assert_eq!(tree.nodes[&node].motion.smoothing.len(), 1);
        assert_eq!(tree.nodes[&node].motion.smoothing[0].duration_ms, 200);
        tree.smooth(node, smoothing(0)).unwrap();
        assert!(tree.nodes[&node].motion.smoothing.is_empty());
    }

    #[test]
    fn only_a_few_nodes_animate_at_once_and_each_request_counts_as_an_operation() {
        let mut tree = Tree::default();
        let mut nodes = Vec::new();
        for index in 0..17 {
            nodes.push(
                tree.create(
                    UiParent::Mount("ui.detached".into()),
                    props(&format!("n{index}")),
                )
                .unwrap(),
            );
            tree.begin_round();
        }
        for node in &nodes[..16] {
            tree.animate(*node, spin()).unwrap();
        }
        assert!(tree.animate(nodes[16], spin()).is_err());
        // Replacing an animation does not count as a new animated node.
        tree.animate(nodes[0], spin()).unwrap();
        tree.begin_round();
        for _ in 0..64 {
            tree.stop_motion(nodes[0]).unwrap();
        }
        assert!(tree.stop_motion(nodes[0]).is_err());
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

    #[test]
    fn finished_finite_animations_release_slots_without_losing_the_held_request() {
        use std::time::{Duration, Instant};
        let mut tree = Tree::default();
        let started = Instant::now();
        let mut nodes = Vec::new();
        for index in 0..17 {
            nodes.push(
                tree.create(
                    UiParent::Mount("ui.detached".into()),
                    props(&format!("finite-{index}")),
                )
                .unwrap(),
            );
        }
        let finite = || MotionAnimation {
            duration_ms: 100,
            delay_ms: 50,
            iterations: Some(2),
            hold: true,
            ..spin()
        };
        tree.begin_round();
        for node in &nodes[..16] {
            tree.animate_at(*node, finite(), started).unwrap();
        }
        assert!(
            tree.animate_at(nodes[16], spin(), started + Duration::from_millis(249))
                .is_err()
        );
        tree.animate_at(nodes[16], spin(), started + Duration::from_millis(250))
            .unwrap();
        assert!(
            tree.nodes[&nodes[0]]
                .motion
                .animation
                .as_ref()
                .unwrap()
                .1
                .hold
        );
        assert_eq!(tree.nodes[&nodes[0]].motion.started_at, Some(started));
    }
}
