//! Stable editor session DTOs and pure snapshot/shape conversions.
use crate::domain::identity::{PaneId, ViewId};
use crate::domain::pane_layout::{Axis, Child, PaneNode};
use crate::domain::view::{DocumentView, Presentation, ReaderPosition};
use crate::domain::workspace::Workspace;
use loog_editor_core::text::Selection;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SavedAxis {
    Horizontal,
    Vertical,
}

/// One node of the saved pane tree.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum SavedPaneNode {
    Pane {
        items: Vec<SavedView>,
        #[serde(default)]
        active: Option<usize>,
    },
    Split {
        axis: SavedAxis,
        children: Vec<SavedChild>,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SavedChild {
    /// Fraction of the parent split.
    pub size: f32,
    pub node: SavedPaneNode,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EditorLayout {
    pub root: SavedPaneNode,
    /// Index of the focused pane in reading order.
    pub active_pane: usize,
}

/// Stable data only: runtime document/view identities never cross a session boundary.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SavedView {
    Resource {
        resource: loog_workspace_api::ResourceRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        root: Option<loog_workspace_api::ResourceRef>,
        name: String,
        #[serde(default)]
        source: SavedSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encoding: Option<SavedEncoding>,
    },
    Text {
        path: PathBuf,
        #[serde(default)]
        presentation: SavedPresentation,
        #[serde(default)]
        source: SavedSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encoding: Option<SavedEncoding>,
        #[serde(default)]
        reader: SavedReader,
        #[serde(default, skip_serializing_if = "SavedImage::is_default")]
        image: SavedImage,
    },
}
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SavedEncoding {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_ending: Option<String>,
    pub current: crate::domain::encoding::TextFormat,
    pub saved: crate::domain::encoding::TextFormat,
    #[serde(default)]
    pub read_override: Option<crate::domain::encoding::TextFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsaved_text: Option<std::sync::Arc<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_text: Option<std::sync::Arc<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_bytes: Option<std::sync::Arc<Vec<u8>>>,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SavedPresentation {
    #[default]
    Source,
    Markdown,
    Image,
}
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SavedImage {
    pub scale: Option<f32>,
    pub x: f32,
    pub y: f32,
    #[serde(default)]
    pub background: String,
    #[serde(default, skip_serializing)]
    pub paused: bool,
}
impl Default for SavedImage {
    fn default() -> Self {
        Self {
            scale: None,
            x: 0.0,
            y: 0.0,
            background: "checkerboard".into(),
            paused: false,
        }
    }
}
impl SavedImage {
    fn is_default(&self) -> bool {
        self.scale.is_none()
            && self.x == 0.0
            && self.y == 0.0
            && ["", "checkerboard"].contains(&self.background.as_str())
    }
}
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SavedSource {
    pub cursor: usize,
    pub anchor: Option<usize>,
    pub scroll_x: f32,
    pub scroll_y: f32,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SavedReader {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_view: Option<SavedViewRef>,
    pub source_byte: usize,
    pub block_fraction: f32,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SavedViewRef {
    pub pane: usize,
    pub item: usize,
}
impl SavedView {
    pub fn association(&self) -> Option<SavedViewRef> {
        match self {
            Self::Text { reader, .. } => reader.source_view,
            Self::Resource { .. } => None,
        }
    }
    pub fn local_path(&self) -> Option<&std::path::Path> {
        match self {
            Self::Text { path, .. } => Some(path),
            Self::Resource { .. } => None,
        }
    }
    #[cfg(test)]
    pub fn path(&self) -> &std::path::Path {
        self.local_path().expect("local session fixture")
    }
    pub fn resource(&self) -> loog_workspace_api::ResourceRef {
        match self {
            Self::Text { path, .. } => loog_workspace_api::ResourceRef::local(path.clone()),
            Self::Resource { resource, .. } => resource.clone(),
        }
    }
    pub fn encoding(&self) -> Option<&SavedEncoding> {
        match self {
            Self::Text { encoding, .. } | Self::Resource { encoding, .. } => encoding.as_ref(),
        }
    }
    pub fn apply(&self, view: &mut DocumentView) {
        if let Self::Resource { source, .. } = self {
            view.presentation = Presentation::Source;
            view.source.selection = Selection {
                cursor: source.cursor,
                anchor: source.anchor,
                preferred_column: None,
                wrap_affinity: Default::default(),
            };
            view.source.scroll_x = source.scroll_x;
            view.source.scroll_y = source.scroll_y;
            return;
        }
        let Self::Text {
            presentation,
            source,
            reader,
            image,
            ..
        } = self
        else {
            unreachable!()
        };
        view.presentation = match presentation {
            SavedPresentation::Source => Presentation::Source,
            SavedPresentation::Markdown => Presentation::Markdown,
            SavedPresentation::Image => Presentation::Image,
        };
        view.source.selection = Selection {
            cursor: source.cursor,
            anchor: source.anchor,
            preferred_column: None,
            wrap_affinity: Default::default(),
        };
        view.source.scroll_x = source.scroll_x;
        view.source.scroll_y = source.scroll_y;
        view.reader = ReaderPosition {
            source_byte: reader.source_byte,
            block_fraction: reader.block_fraction,
        };
        view.image = crate::domain::media::Position {
            scale: image.scale,
            x: image.x,
            y: image.y,
            background: match image.background.as_str() {
                "light" => crate::domain::media::Background::Light,
                "dark" => crate::domain::media::Background::Dark,
                _ => crate::domain::media::Background::Checkerboard,
            },
        };
    }
    fn capture(
        meta: &crate::domain::document::FileMeta,
        view: &DocumentView,
        association: Option<SavedViewRef>,
    ) -> Self {
        let Some(path) = meta.local_path() else {
            return Self::Resource {
                resource: meta.resource.clone(),
                name: meta.name.clone(),
                encoding: None,
                root: meta.provider_root.clone(),
                source: SavedSource {
                    cursor: view.source.selection.cursor,
                    anchor: view.source.selection.anchor,
                    scroll_x: view.source.scroll_x,
                    scroll_y: view.source.scroll_y,
                },
            };
        };
        Self::Text {
            path: path.to_path_buf(),
            encoding: None,
            presentation: match view.presentation {
                Presentation::Source => SavedPresentation::Source,
                Presentation::Markdown => SavedPresentation::Markdown,
                Presentation::Image => SavedPresentation::Image,
            },
            source: SavedSource {
                cursor: view.source.selection.cursor,
                anchor: view.source.selection.anchor,
                scroll_x: view.source.scroll_x,
                scroll_y: view.source.scroll_y,
            },
            reader: SavedReader {
                source_view: association,
                source_byte: view.reader.source_byte,
                block_fraction: view.reader.block_fraction,
            },
            image: SavedImage {
                scale: view.image.scale,
                x: view.image.x,
                y: view.image.y,
                paused: false,
                background: match view.image.background {
                    crate::domain::media::Background::Checkerboard => "checkerboard",
                    crate::domain::media::Background::Light => "light",
                    crate::domain::media::Background::Dark => "dark",
                }
                .into(),
            },
        }
    }
}
impl From<PathBuf> for SavedView {
    fn from(path: PathBuf) -> Self {
        Self::Text {
            path,
            encoding: None,
            presentation: SavedPresentation::default(),
            source: SavedSource::default(),
            reader: SavedReader::default(),
            image: SavedImage::default(),
        }
    }
}
impl From<&str> for SavedView {
    fn from(path: &str) -> Self {
        PathBuf::from(path).into()
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LayoutV2 {
    schema: u32,
    root: SavedPaneNode,
    #[serde(default)]
    active_pane: usize,
}
fn contains_resources(node: &SavedPaneNode) -> bool {
    match node {
        SavedPaneNode::Pane { items, .. } => items
            .iter()
            .any(|item| matches!(item, SavedView::Resource { .. })),
        SavedPaneNode::Split { children, .. } => {
            children.iter().any(|child| contains_resources(&child.node))
        }
    }
}
// Frozen legacy DTO: the unversioned format identified tabs by path.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyLayout {
    root: LegacyNode,
    #[serde(default)]
    active_pane: usize,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum LegacyNode {
    Pane {
        items: Vec<PathBuf>,
        #[serde(default)]
        active: Option<PathBuf>,
    },
    Split {
        axis: SavedAxis,
        children: Vec<LegacyChild>,
    },
}
#[derive(Deserialize)]
struct LegacyChild {
    size: f32,
    node: LegacyNode,
}
impl LegacyNode {
    fn migrate(self) -> SavedPaneNode {
        match self {
            Self::Pane { items, active } => {
                let active = active.and_then(|p| items.iter().position(|v| *v == p));
                SavedPaneNode::Pane {
                    items: items.into_iter().map(Into::into).collect(),
                    active,
                }
            }
            Self::Split { axis, children } => SavedPaneNode::Split {
                axis,
                children: children
                    .into_iter()
                    .map(|c| SavedChild {
                        size: c.size,
                        node: c.node.migrate(),
                    })
                    .collect(),
            },
        }
    }
}
impl Serialize for EditorLayout {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.validate().map_err(serde::ser::Error::custom)?;
        LayoutV2 {
            schema: if contains_resources(&self.root) { 3 } else { 2 },
            root: self.root.clone(),
            active_pane: self.active_pane,
        }
        .serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for EditorLayout {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        let result = if value.get("schema").is_some() {
            let wire: LayoutV2 = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
            // COMPAT(local-layout-v2): Read existing local-only v2 sessions.
            // Resource sessions use v3 so old applications reject them rather
            // than lose tabs. Remove v2 only with an explicit session migration;
            // no removal version is scheduled.
            if !matches!(wire.schema, 2 | 3) || wire.schema == 2 && contains_resources(&wire.root) {
                return Err(serde::de::Error::custom("unsupported editor layout schema"));
            }
            Self {
                root: wire.root,
                active_pane: wire.active_pane,
            }
        } else {
            let wire: LegacyLayout =
                serde_json::from_value(value).map_err(serde::de::Error::custom)?;
            Self {
                root: wire.root.migrate(),
                active_pane: wire.active_pane,
            }
        };
        result.validate().map_err(serde::de::Error::custom)?;
        Ok(result)
    }
}
impl EditorLayout {
    pub fn validate(&self) -> Result<(), &'static str> {
        fn node(
            n: &SavedPaneNode,
            depth: usize,
            panes: &mut usize,
            views: &mut usize,
        ) -> Result<(), &'static str> {
            if depth > 32 {
                return Err("editor layout exceeds depth limit");
            }
            match n {
                SavedPaneNode::Pane { items, active } => {
                    *panes += 1;
                    *views += items.len();
                    if *panes > 128 || *views > 10000 {
                        return Err("editor layout exceeds size limit");
                    }
                    if active.is_some_and(|i| i >= items.len()) {
                        return Err("invalid active view index");
                    }
                    for item in items {
                        if let SavedView::Resource {
                            resource,
                            root,
                            name,
                            source,
                            ..
                        } = item
                        {
                            if resource.local_path().is_some()
                                || name.is_empty()
                                || name.len() > 32 * 1024
                                || name.contains('\0')
                                || !source.scroll_x.is_finite()
                                || source.scroll_x < 0.0
                                || !source.scroll_y.is_finite()
                                || source.scroll_y < 0.0
                            {
                                return Err("invalid resource view");
                            }
                            if root
                                .as_ref()
                                .is_some_and(|root| root.environment() != resource.environment())
                            {
                                return Err("resource root belongs to a different environment");
                            }
                            continue;
                        }
                        let SavedView::Text {
                            path,
                            source,
                            reader,
                            image,
                            ..
                        } = item
                        else {
                            unreachable!()
                        };
                        if path.as_os_str().is_empty() || path.to_string_lossy().contains('\0') {
                            return Err("invalid document path");
                        }
                        if !source.scroll_x.is_finite()
                            || source.scroll_x < 0.0
                            || !source.scroll_y.is_finite()
                            || source.scroll_y < 0.0
                            || !reader.block_fraction.is_finite()
                            || !(0.0..=1.0).contains(&reader.block_fraction)
                            || !image.x.is_finite()
                            || !image.y.is_finite()
                            || image
                                .scale
                                .is_some_and(|s| !s.is_finite() || !(0.05..=16.0).contains(&s))
                            || !["", "checkerboard", "light", "dark"]
                                .contains(&image.background.as_str())
                        {
                            return Err("invalid view position");
                        }
                    }
                }
                SavedPaneNode::Split { children, .. } => {
                    if children.len() < 2 || children.len() > 128 {
                        return Err("invalid split child count");
                    }
                    let mut total = 0.0;
                    for child in children {
                        if !child.size.is_finite() || child.size <= 0.0 {
                            return Err("invalid split size");
                        }
                        total += child.size;
                        node(&child.node, depth + 1, panes, views)?;
                    }
                    if !total.is_finite() {
                        return Err("invalid split total");
                    }
                }
            }
            Ok(())
        }
        let mut panes = 0;
        let mut views = 0;
        node(&self.root, 0, &mut panes, &mut views)?;
        if self.active_pane >= panes {
            return Err("invalid active pane index");
        }
        let mut saved = Vec::new();
        fn collect<'a>(node: &'a SavedPaneNode, out: &mut Vec<&'a Vec<SavedView>>) {
            match node {
                SavedPaneNode::Pane { items, .. } => out.push(items),
                SavedPaneNode::Split { children, .. } => {
                    for child in children {
                        collect(&child.node, out);
                    }
                }
            }
        }
        collect(&self.root, &mut saved);
        for pane in &saved {
            for view in pane.iter() {
                if let Some(reference) = view.association() {
                    let Some(target) = saved
                        .get(reference.pane)
                        .and_then(|pane| pane.get(reference.item))
                    else {
                        return Err("invalid associated source view");
                    };
                    if !matches!(
                        view,
                        SavedView::Text {
                            presentation: SavedPresentation::Markdown | SavedPresentation::Image,
                            ..
                        }
                    ) || !matches!(
                        target,
                        SavedView::Text {
                            presentation: SavedPresentation::Source,
                            ..
                        }
                    ) || view.resource() != target.resource()
                    {
                        return Err("incompatible associated source view");
                    }
                }
            }
        }
        Ok(())
    }
}

pub struct PendingTab {
    pub pane: PaneId,
    pub path: PathBuf,
    pub insert_before: Option<usize>,
}

pub fn capture<D: Clone, K: Clone + Ord>(
    workspace: &Workspace<D, K>,
    pending: Option<&PendingTab>,
) -> Option<EditorLayout> {
    let mut leaf_index = 0;
    let mut active_pane = None;
    let root = save_pane_node(
        workspace,
        workspace.layout(),
        pending,
        &mut leaf_index,
        &mut active_pane,
    )?;
    Some(EditorLayout {
        root,
        active_pane: active_pane.unwrap_or(0),
    })
}

fn save_pane_node<D: Clone, K: Clone + Ord>(
    workspace: &Workspace<D, K>,
    node: &PaneNode,
    pending: Option<&PendingTab>,
    leaf_index: &mut usize,
    active_pane: &mut Option<usize>,
) -> Option<SavedPaneNode> {
    match node {
        PaneNode::Leaf(id) => {
            let pane = workspace.pane(*id)?;
            let restorable = |view: ViewId| workspace.is_file(view);
            let saved = |id: ViewId| {
                let mut saved = SavedView::capture(
                    workspace.view_meta(id)?,
                    workspace.view(id)?,
                    source_reference(workspace, id, pending),
                );
                if let Some(document) = workspace
                    .view_document(id)
                    .and_then(|id| workspace.documents.get(&id))
                {
                    let (SavedView::Text { encoding, .. } | SavedView::Resource { encoding, .. }) =
                        &mut saved;
                    let dirty = document.is_dirty();
                    *encoding = if !document.is_text() {
                        document.recovery.clone()
                    } else if dirty
                        || document.meta.local_path().is_none()
                        || document.format() != Default::default()
                        || document.meta.read_override.is_some()
                        || document.buffer.chosen_line_ending().is_some()
                    {
                        Some(SavedEncoding {
                            line_ending: document
                                .buffer
                                .chosen_line_ending()
                                .map(|e| e.label().to_owned()),
                            current: document.format(),
                            saved: document.saved_format,
                            read_override: document.meta.read_override.map(|_| document.format()),
                            unsaved_text: dirty.then(|| document.buffer.text_snapshot()),
                            saved_text: (dirty || document.meta.local_path().is_none())
                                .then(|| document.saved_text.clone()),
                            source_bytes: dirty.then(|| document.encoding_bytes.clone()).flatten(),
                        })
                    } else {
                        None
                    };
                }
                Some(saved)
            };
            let mut items = pane
                .items()
                .iter()
                .copied()
                .filter(|file| restorable(*file))
                .filter_map(saved)
                .collect::<Vec<_>>();
            let pending = pending.filter(|p| p.pane == *id);
            if let Some(pending) = pending
                && !items
                    .iter()
                    .any(|item| item.local_path() == Some(pending.path.as_path()))
            {
                let index = if let Some(index) = pending.insert_before {
                    pane.items()
                        .iter()
                        .take(index)
                        .filter(|file| workspace.is_file(**file))
                        .count()
                } else {
                    items.len()
                };
                items.insert(index.min(items.len()), pending.path.clone().into());
            }
            if items.is_empty() {
                return None;
            }
            if pending.is_some() || (active_pane.is_none() && *id == workspace.active_pane()) {
                *active_pane = Some(*leaf_index);
            }
            *leaf_index += 1;
            let active = pending
                .and_then(|p| {
                    items
                        .iter()
                        .position(|v| v.local_path() == Some(p.path.as_path()))
                })
                .or_else(|| {
                    let active = pane.active()?;
                    pane.items()
                        .iter()
                        .filter(|id| restorable(**id))
                        .position(|id| *id == active)
                });
            Some(SavedPaneNode::Pane { items, active })
        }
        PaneNode::Split { axis, children } => {
            let mut saved = children
                .iter()
                .filter_map(|child| {
                    save_pane_node(workspace, &child.node, pending, leaf_index, active_pane).map(
                        |node| SavedChild {
                            size: child.size,
                            node,
                        },
                    )
                })
                .collect::<Vec<_>>();
            match saved.len() {
                0 => None,
                1 => saved.pop().map(|child| child.node),
                _ => {
                    let total: f32 = saved.iter().map(|child| child.size).sum();
                    for child in &mut saved {
                        child.size /= total.max(f32::EPSILON);
                    }
                    Some(SavedPaneNode::Split {
                        axis: match axis {
                            Axis::Horizontal => SavedAxis::Horizontal,
                            Axis::Vertical => SavedAxis::Vertical,
                        },
                        children: saved,
                    })
                }
            }
        }
    }
}

pub fn restored_shape(
    node: SavedPaneNode,
    contents: &mut Vec<(Vec<SavedView>, Option<usize>)>,
) -> PaneNode {
    match node {
        SavedPaneNode::Pane { items, active } => {
            let leaf = PaneId::new(contents.len() as u64);
            contents.push((items, active));
            PaneNode::Leaf(leaf)
        }
        SavedPaneNode::Split { axis, children } if !children.is_empty() => PaneNode::Split {
            axis: match axis {
                SavedAxis::Horizontal => Axis::Horizontal,
                SavedAxis::Vertical => Axis::Vertical,
            },
            children: children
                .into_iter()
                .map(|child| Child {
                    size: child.size,
                    node: restored_shape(child.node, contents),
                })
                .collect(),
        },
        SavedPaneNode::Split { .. } => {
            let leaf = PaneId::new(contents.len() as u64);
            contents.push((Vec::new(), None));
            PaneNode::Leaf(leaf)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::domain::document::DiskState;
    use crate::domain::pane_layout::Direction;
    use crate::domain::workspace::{AppPage, OpenMode, PreparedTextFile, Workspace};
    use crate::domain::workspace_session::{self, PendingTab, SavedPaneNode};

    fn open(workspace: &mut Workspace<(), ()>, path: &str) {
        workspace.open_prepared_text(
            PreparedTextFile::with_disk_state(
                path.into(),
                "text".into(),
                DiskState::from_contents("text"),
            ),
            OpenMode::Permanent,
        );
    }

    #[test]
    fn pending_snapshot_counts_only_file_tabs_before_its_insertion() {
        let mut workspace = Workspace::<(), ()>::new();
        open(&mut workspace, "first.txt");
        workspace.open_page(AppPage::Settings);
        open(&mut workspace, "second.txt");
        let pending = PendingTab {
            pane: workspace.active_pane(),
            path: "queued.txt".into(),
            insert_before: Some(2),
        };
        let layout = workspace_session::capture(&workspace, Some(&pending)).unwrap();
        let SavedPaneNode::Pane { items, active } = layout.root else {
            panic!("one pane");
        };
        assert_eq!(
            items,
            ["first.txt".into(), "queued.txt".into(), "second.txt".into()]
        );
        assert_eq!(active, Some(1));
        assert_eq!(layout.active_pane, 0);
        assert_eq!(
            workspace.active_meta().unwrap().local_path(),
            Some(std::path::Path::new("second.txt"))
        );
    }

    #[test]
    fn pruning_empty_panes_preserves_focus_and_restorable_tree_order() {
        let mut workspace = Workspace::<(), ()>::new();
        open(&mut workspace, "first.txt");
        let left = workspace.active_pane();
        let empty = workspace.split_empty(left, Direction::Right).unwrap();
        let last = workspace.split_empty(empty, Direction::Down).unwrap();
        workspace.activate_pane(last);
        open(&mut workspace, "last.txt");
        let layout = workspace_session::capture(&workspace, None).unwrap();
        assert_eq!(layout.active_pane, 1);
        let encoded = serde_json::to_string(&layout).unwrap();
        let decoded: workspace_session::EditorLayout = serde_json::from_str(&encoded).unwrap();
        let mut tabs = Vec::new();
        let tree = workspace_session::restored_shape(decoded.root, &mut tabs);
        assert_eq!(tree.leaves().len(), 2);
        assert_eq!(tabs[0].0, ["first.txt".into()]);
        assert_eq!(tabs[1].0, ["last.txt".into()]);
        let mut restored = Workspace::<(), ()>::new();
        assert_eq!(restored.restore_layout(tree).len(), 2);
    }
}

#[cfg(test)]
mod migration_tests {
    use crate::domain::workspace_session::*;
    use serde_json::json;

    #[test]
    fn p1_legacy_shape_migrates_once_and_only_writes_typed_schema_two() {
        let legacy = json!({"root":{"split":{"axis":"horizontal","children":[{"size":0.4,"node":{"pane":{"items":["main.rs","中文.md"],"active":"中文.md"}}},{"size":0.6,"node":{"pane":{"items":["lib.rs"],"active":"lib.rs"}}}]}},"active_pane":1});
        let layout: EditorLayout = serde_json::from_value(legacy).unwrap();
        assert_eq!(layout.active_pane, 1);
        let encoded = serde_json::to_value(&layout).unwrap();
        assert_eq!(encoded["schema"], 2);
        assert_eq!(
            encoded["root"]["split"]["children"][0]["node"]["pane"]["active"],
            1
        );
        assert_eq!(
            encoded["root"]["split"]["children"][0]["node"]["pane"]["items"][1]["kind"],
            "text"
        );
        assert_eq!(
            serde_json::from_value::<EditorLayout>(encoded).unwrap(),
            layout
        );
    }

    #[test]
    fn p1_schema_two_round_trip_preserves_repeated_path_modes_and_both_positions() {
        let raw = json!({"schema":2,"root":{"pane":{"items":[
            {"kind":"text","path":"中文.md","presentation":"source","source":{"cursor":6,"anchor":0,"scroll_x":8.0,"scroll_y":33.0}},
            {"kind":"text","path":"中文.md","presentation":"markdown","reader":{"source_byte":12,"block_fraction":0.7}}
        ],"active":1}},"active_pane":0});
        let layout: EditorLayout = serde_json::from_value(raw).unwrap();
        let encoded = serde_json::to_string(&layout).unwrap();
        assert!(!encoded.contains("incarnation"));
        assert!(!encoded.contains("view_id"));
        assert_eq!(
            serde_json::from_str::<EditorLayout>(&encoded).unwrap(),
            layout
        );
        let SavedPaneNode::Pane { items, active } = layout.root else {
            panic!()
        };
        assert_eq!(items[0].path(), items[1].path());
        assert_eq!(active, Some(1));
    }

    #[test]
    fn p1_invalid_or_future_layout_never_falls_back_to_legacy_defaults() {
        let base = json!({"schema":2,"root":{"pane":{"items":[{"kind":"text","path":"a.md"}],"active":0}},"active_pane":0});
        let mut bad = Vec::new();
        for version in [json!(1), json!(4), json!("2"), serde_json::Value::Null] {
            let mut v = base.clone();
            v["schema"] = version;
            bad.push(v);
        }
        let mut v = base.clone();
        v["active_pane"] = json!(1);
        bad.push(v);
        let mut v = base.clone();
        v["root"]["pane"]["active"] = json!(1);
        bad.push(v);
        let mut v = base.clone();
        v["root"]["pane"]["items"][0]["kind"] = json!("html");
        bad.push(v);
        let mut v = base.clone();
        v["root"]["pane"]["items"][0]["path"] = json!("");
        bad.push(v);
        let mut v = base.clone();
        v["root"]["pane"]["items"][0]["presentation"] = json!("unknown");
        bad.push(v);
        let mut v = base.clone();
        v["root"]["pane"]["items"][0]["reader"] = json!({"source_byte":0,"block_fraction":1.1});
        bad.push(v);
        let mut v = base.clone();
        v["root"]["pane"]["items"][0]["source"] =
            json!({"cursor":0,"anchor":null,"scroll_x":-1,"scroll_y":0});
        bad.push(v);
        for value in bad {
            assert!(
                serde_json::from_value::<EditorLayout>(value.clone()).is_err(),
                "{value}"
            );
        }
        for size in [0.0, -1.0] {
            let value = json!({"schema":2,"root":{"split":{"axis":"horizontal","children":[{"size":size,"node":{"pane":{"items":[],"active":null}}},{"size":1,"node":{"pane":{"items":[],"active":null}}}]}},"active_pane":0});
            assert!(serde_json::from_value::<EditorLayout>(value).is_err());
        }
    }
}

fn source_reference<D: Clone, K: Clone + Ord>(
    workspace: &Workspace<D, K>,
    view: ViewId,
    pending: Option<&PendingTab>,
) -> Option<SavedViewRef> {
    let value = workspace.view(view)?;
    if !matches!(
        value.presentation,
        Presentation::Markdown | Presentation::Image
    ) {
        return None;
    }
    let source = value.associated_source?;
    if !workspace
        .view(source)
        .is_some_and(|source| source.accepts_text() && source.document() == value.document())
    {
        return None;
    }
    let source_pane = workspace.pane_of(source)?;
    let panes = workspace
        .pane_ids()
        .into_iter()
        .filter(|pane| {
            workspace
                .pane(*pane)
                .is_some_and(|pane| pane.items().iter().any(|id| workspace.is_file(*id)))
                || pending.is_some_and(|p| p.pane == *pane)
        })
        .collect::<Vec<_>>();
    let pane = panes.iter().position(|pane| *pane == source_pane)?;
    let actual = workspace.pane(source_pane)?;
    let mut item = actual
        .items()
        .iter()
        .filter(|id| workspace.is_file(**id))
        .position(|id| *id == source)?;
    if let Some(pending) = pending.filter(|pending| {
        pending.pane == source_pane
            && !actual
                .items()
                .iter()
                .filter_map(|id| workspace.view_meta(*id))
                .any(|meta| meta.local_path() == Some(pending.path.as_path()))
    }) {
        let insertion = pending.insert_before.map_or(usize::MAX, |before| {
            actual
                .items()
                .iter()
                .take(before)
                .filter(|id| workspace.is_file(**id))
                .count()
        });
        if insertion <= item {
            item += 1;
        }
    }
    Some(SavedViewRef { pane, item })
}
