//! The reference navigator beside the history: branches, remotes, tags and
//! stashes. Choosing one decides what the history and its detail pane show.

use std::collections::{BTreeMap, HashSet};

use lgui::core::{
    AnimProperty, AnimationBinding, CursorIcon, EventPolicy, PointerButton, SemanticRole,
    Semantics, UiElement, UiId,
};
use lgui::prelude::{Color, Element, UiRect, VisualStyle, panel, text};

use crate::features::git::{BranchInfo, RepositorySnapshot, StashInfo, TagInfo};
use crate::ui::git::commands::{Binding, Command, Snapshot};
use crate::ui::git::menu::{GitMenu, GitMenuTarget};
use crate::ui::git::panel::{ScrollRegion, scroll_region};
use crate::ui::git::state::NavView;
use crate::ui::git::tree_guides::TreeGuides;
use crate::ui::{tabs, theme};

/// The icon rail down the navigator's left edge.
pub(crate) const RAIL_W: f32 = 30.0;
pub(crate) const WIDTH: f32 = 291.0;
const ROW_H: f32 = 22.0;
const HEADING_H: f32 = 24.0;
const PAD: f32 = 8.0;
const INDENT: f32 = 12.0;

// Native windows schedule hover redraws through animation bindings. The fill
// reads the hover flag directly, so it changes on the next frame without a fade.
const HOVER_FRAME: AnimationBinding =
    AnimationBinding::new(AnimProperty::Hover, 0.0, 1.0).duration(1.0, 1.0);

/// The prefix that marks a tag in `GitUiState::history_scope`.
pub(crate) const TAG_PREFIX: &str = "tag:";

/// Branches grouped by the `/` in their names, so `feature/a` and `feature/b`
/// share a `feature` folder and a remote's branches sit under its name.
#[derive(Default)]
struct Node<'a> {
    folders: BTreeMap<String, Node<'a>>,
    leaves: Vec<(String, &'a BranchInfo)>,
    count: usize,
}

impl<'a> Node<'a> {
    fn insert(&mut self, segments: &[&str], branch: &'a BranchInfo) {
        self.count += 1;
        match segments {
            [] => {}
            [name] => self.leaves.push(((*name).to_owned(), branch)),
            [folder, rest @ ..] => self
                .folders
                .entry((*folder).to_owned())
                .or_default()
                .insert(rest, branch),
        }
    }

    /// Whether `name` is one of the branches in this folder or below it.
    fn contains(&self, name: &str) -> bool {
        self.leaves.iter().any(|(_, info)| info.name == name)
            || self.folders.values().any(|child| child.contains(name))
    }

    fn flatten(
        &self,
        kind: &str,
        path: &str,
        depth: usize,
        collapsed: &HashSet<String>,
        selected: Option<&str>,
        out: &mut Vec<Entry<'a>>,
    ) {
        for (name, child) in &self.folders {
            let path = if path.is_empty() {
                name.clone()
            } else {
                format!("{path}/{name}")
            };
            let key = format!("{kind}{path}");
            let expanded = !collapsed.contains(&key);
            out.push(Entry::Folder {
                key,
                name: name.clone(),
                depth,
                expanded,
                count: child.count,
                holds_selection: !expanded && selected.is_some_and(|name| child.contains(name)),
            });
            if expanded {
                child.flatten(kind, &path, depth + 1, collapsed, selected, out);
            }
        }
        let mut leaves = self.leaves.clone();
        leaves.sort_by(|a, b| a.0.cmp(&b.0));
        out.extend(leaves.into_iter().map(|(display, info)| Entry::Branch {
            info,
            depth,
            display,
        }));
    }
}

/// A top-level `Local` or `Remote` folder, holding its branches as a tree.
fn group<'a>(
    key: &str,
    name: &str,
    kind: &str,
    branches: impl Iterator<Item = &'a BranchInfo>,
    collapsed: &HashSet<String>,
    selected: Option<&str>,
    out: &mut Vec<Entry<'a>>,
) {
    let mut root = Node::default();
    for branch in branches {
        let segments = branch.name.split('/').collect::<Vec<_>>();
        root.insert(&segments, branch);
    }
    if root.count == 0 {
        return;
    }
    let expanded = !collapsed.contains(key);
    out.push(Entry::Folder {
        key: key.to_owned(),
        name: name.to_owned(),
        depth: 0,
        expanded,
        count: root.count,
        holds_selection: !expanded && selected.is_some_and(|name| root.contains(name)),
    });
    if expanded {
        root.flatten(kind, "", 1, collapsed, selected, out);
    }
}

/// What a heading's `+` button creates.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Create {
    Branch,
    Tag,
    Stash,
}

#[derive(Clone)]
enum Entry<'a> {
    Heading(&'static str, Option<Create>),
    Folder {
        key: String,
        name: String,
        depth: usize,
        expanded: bool,
        count: usize,
        holds_selection: bool,
    },
    Branch {
        info: &'a BranchInfo,
        depth: usize,
        display: String,
    },
    Tag(&'a TagInfo),
    Stash(&'a StashInfo),
    Empty(&'static str),
}

impl Entry<'_> {
    fn height(&self) -> f32 {
        match self {
            Self::Heading(..) => HEADING_H,
            _ => ROW_H,
        }
    }
}

fn entries<'a>(
    branches: &'a [BranchInfo],
    repository: &'a RepositorySnapshot,
    collapsed: &HashSet<String>,
    selected: Option<&str>,
    view: NavView,
) -> Vec<Entry<'a>> {
    let mut entries = Vec::new();
    match view {
        NavView::Branches => {
            entries.push(Entry::Heading("BRANCHES", Some(Create::Branch)));
            group(
                LOCAL,
                "Local",
                "b:",
                branches.iter().filter(|branch| !branch.remote),
                collapsed,
                selected,
                &mut entries,
            );
            // A remote's own HEAD alias repeats its default branch.
            group(
                REMOTE,
                "Remote",
                "r:",
                branches
                    .iter()
                    .filter(|branch| branch.remote && !branch.name.ends_with("/HEAD")),
                collapsed,
                selected,
                &mut entries,
            );
            if entries.len() == 1 {
                entries.push(Entry::Empty("No branches yet."));
            }
        }
        NavView::Tags => {
            entries.push(Entry::Heading("TAGS", Some(Create::Tag)));
            if repository.tags.is_empty() {
                entries.push(Entry::Empty("No tags yet."));
            }
            entries.extend(repository.tags.iter().map(Entry::Tag));
        }
        NavView::Stashes => {
            entries.push(Entry::Heading("STASHES", Some(Create::Stash)));
            if repository.stashes.is_empty() {
                entries.push(Entry::Empty("No stashes."));
            }
            entries.extend(repository.stashes.iter().map(Entry::Stash));
        }
    }
    entries
}

/// Folder keys of the two branch groups; remotes start folded.
pub(crate) const LOCAL: &str = "g:local";
pub(crate) const REMOTE: &str = "g:remote";

impl Entry<'_> {
    /// How deep the entry sits in the branch tree; other kinds are flat.
    fn depth(&self) -> usize {
        match self {
            Self::Folder { depth, .. } | Self::Branch { depth, .. } => *depth,
            _ => 0,
        }
    }
}

/// The folders to pin as `(row, top)`: each one scrolled above its slot whose
/// contents still reach it, stacked by depth and pushed up as its contents end.
fn pinned_folders(
    rows: &[Entry<'_>],
    tops: &[f32],
    rows_bottom: f32,
    view_top: f32,
) -> Vec<(usize, f32)> {
    let items = rows
        .iter()
        .map(|row| (matches!(row, Entry::Folder { .. }), row.depth()))
        .collect::<Vec<_>>();
    pin_slots(&items, tops, rows_bottom, view_top, ROW_H)
}

/// Pin folders over a list of `(is_folder, depth)` rows with the given `tops`.
pub(crate) fn pin_slots(
    items: &[(bool, usize)],
    tops: &[f32],
    rows_bottom: f32,
    view_top: f32,
    row_h: f32,
) -> Vec<(usize, f32)> {
    let mut pinned = Vec::new();
    for (index, (is_folder, depth)) in items.iter().enumerate() {
        if !is_folder {
            continue;
        }
        let slot = view_top + *depth as f32 * row_h;
        let end = items[index + 1..]
            .iter()
            .position(|(_, next)| next <= depth)
            .map_or(rows_bottom, |offset| tops[index + 1 + offset]);
        if tops[index] < slot && end > slot {
            pinned.push((index, slot.min(end - row_h)));
        }
    }
    pinned
}

/// The part of the navigator that scrolls: below the rail's width and the heading.
pub(crate) fn body_rect(rect: UiRect) -> UiRect {
    UiRect::new(
        rect.left + RAIL_W,
        rect.top + PAD + HEADING_H,
        rect.right,
        rect.bottom,
    )
}

pub(crate) fn render(
    rect: UiRect,
    repository: &RepositorySnapshot,
    branches: &[BranchInfo],
    scope: Option<&str>,
    stash: Option<&str>,
    tag_target: Option<&str>,
    view: NavView,
    collapsed: &HashSet<String>,
    scroll: f32,
    state: &Binding,
    store: &Snapshot,
) -> Element {
    let list = UiRect::new(rect.left + RAIL_W, rect.top, rect.right, rect.bottom);
    // The title and its create button stay put; only what is below them scrolls.
    let heading = UiRect::new(
        list.left,
        list.top + PAD,
        list.right,
        list.top + PAD + HEADING_H,
    );
    let body = UiRect::new(list.left, heading.bottom, list.right, list.bottom);
    let current = branches
        .iter()
        .find(|branch| branch.current)
        .map(|branch| branch.name.as_str());
    // Choosing a stash leaves the history on its branch, which stops being the
    // selection; the stash is.
    let selected = scope.or(current).filter(|_| stash.is_none());
    let mut title = None;
    let mut rows = Vec::new();
    for entry in entries(branches, repository, collapsed, selected, view) {
        if let Entry::Heading(name, create) = entry {
            title = Some(heading_row(
                heading, name, create, repository, tag_target, state, store,
            ));
        } else {
            rows.push(entry);
        }
    }
    // Row tops in content space, before scrolling.
    let mut tops = Vec::with_capacity(rows.len());
    let mut y = body.top;
    for row in &rows {
        tops.push(y);
        y += row.height();
    }
    let rows_bottom = y;
    let content_bottom = rows_bottom + PAD;
    let max_scroll = (content_bottom - body.bottom).max(0.0);
    let scroll = scroll.clamp(0.0, max_scroll);
    let (view_top, view_bottom) = (body.top + scroll, body.bottom + scroll);
    let visible = tops.partition_point(|&top| top + ROW_H < view_top)
        ..tops.partition_point(|&top| top <= view_bottom);

    // A folder scrolled past the top stays pinned there, below the folders
    // that contain it, for as long as its contents are in view.
    let pinned = pinned_folders(&rows, &tops, rows_bottom, view_top);

    let build = |entry: Entry<'_>, row: UiRect| -> Element {
        match entry {
            Entry::Heading(title, create) => {
                heading_row(row, title, create, repository, tag_target, state, store)
            }
            Entry::Folder {
                key,
                name,
                depth,
                expanded,
                count,
                holds_selection,
            } => folder_row(
                row,
                key,
                name,
                depth,
                expanded,
                count,
                holds_selection,
                state,
            ),
            Entry::Branch {
                info,
                depth,
                display,
            } => branch_row(
                row,
                repository,
                info,
                &display,
                depth,
                selected == Some(info.name.as_str()),
                state,
            ),
            Entry::Tag(tag) => tag_row(
                row,
                repository,
                tag,
                scope.and_then(|scope| scope.strip_prefix(TAG_PREFIX)) == Some(tag.name.as_str())
                    && stash.is_none(),
                state,
            ),
            Entry::Stash(info) => stash_row(
                row,
                repository,
                info,
                stash == Some(info.oid.as_str()),
                state,
            ),
            Entry::Empty(message) => text(
                UiRect::new(row.left + 14.0, row.top, row.right - 8.0, row.bottom),
                message,
                theme::mono(theme::c().text_ghost, theme::SMALL),
            )
            .into(),
        }
    };

    let mut content = Vec::new();
    for index in visible.clone() {
        let entry = &rows[index];
        let top = tops[index];
        let height = entry.height();
        if pinned.iter().any(|(pinned, _)| *pinned == index) {
            continue;
        }
        content.push(build(
            entry.clone(),
            UiRect::new(list.left, top, list.right, top + height),
        ));
    }
    let guides = TreeGuides::new(body, scroll, inner(body).left + 8.0, INDENT);
    guides.append(
        &mut content,
        visible
            .clone()
            .map(|index| (tops[index], rows[index].depth())),
        tops.get(visible.end).copied().unwrap_or(rows_bottom),
    );
    // Drawn last and opaque, so what scrolls beneath cannot show through.
    for (index, top) in pinned {
        let slot = UiRect::new(list.left, top, list.right, top + ROW_H);
        content.push(
            guides.pinned(
                panel(slot, VisualStyle::filled(theme::c().sidebar))
                    .child(build(rows[index].clone(), slot)),
                slot,
                rows[index].depth(),
            ),
        );
    }
    panel(rect, VisualStyle::default())
        .key("git-history-branches")
        .child(rail(
            UiRect::new(rect.left, rect.top, rect.left + RAIL_W, rect.bottom),
            view,
            [
                branches.iter().filter(|branch| !branch.remote).count(),
                repository.tags.len(),
                repository.stashes.len(),
            ],
            state,
        ))
        .child(scroll_region(
            body,
            content,
            content_bottom,
            scroll,
            max_scroll,
            state.clone(),
            ScrollRegion::Navigator,
        ))
        .children(title)
}

/// A selectable row: its background follows the selection and the pointer.
fn row(
    key: String,
    rect: UiRect,
    selected: bool,
    name: String,
    on_click: impl Fn() + Send + Sync + 'static,
) -> Element {
    let mut semantics = Semantics::new(SemanticRole::Button).name(name);
    semantics.state.selected = selected;
    Element::new(move |cx| {
        let hovered = cx.context.interaction_flags(&cx.id).hovered
            || cx.children.iter().any(|child| {
                let node = child.node();
                node.event_policy.hover && cx.context.interaction_flags(&node.id).hovered
            });
        UiElement::panel(cx.id, rect, hover_fill(hovered, selected).radius(3.0))
            .children(cx.children)
    })
    .key(key)
    .event_policy(EventPolicy::INTERACTIVE)
    .animation(HOVER_FRAME)
    .semantics(semantics)
    .cursor(CursorIcon::Pointer)
    .on_click(on_click)
}

fn hover_fill(hovered: bool, selected: bool) -> VisualStyle {
    if hovered {
        VisualStyle::filled(theme::c().surface)
    } else if selected {
        VisualStyle::filled(theme::c().active_line)
    } else {
        VisualStyle::default()
    }
}

fn color(selected: bool) -> Color {
    if selected {
        theme::c().accent
    } else {
        theme::c().text_muted
    }
}

/// A row's icon and label, the label leaving `trailing` pixels free.
fn label(
    rect: UiRect,
    icon: &'static str,
    key: &str,
    value: &str,
    selected: bool,
    trailing: f32,
) -> (Element, Element) {
    let color = color(selected);
    (
        crate::ui::drawer::drawer_icon(
            UiId::owned(format!("git-history-ref-icon-{key}")),
            icon,
            theme::icon_rect(UiRect::new(
                rect.left + 4.0,
                rect.top + 3.0,
                rect.left + 18.0,
                rect.bottom - 3.0,
            )),
            color,
        ),
        text(
            UiRect::new(rect.left + 22.0, rect.top, rect.right, rect.bottom),
            tabs::ellipsize(value, rect.width() - 26.0 - trailing, theme::SMALL, 400),
            theme::mono(color, theme::SMALL),
        )
        .into(),
    )
}

fn inner(rect: UiRect) -> UiRect {
    UiRect::new(rect.left + 6.0, rect.top, rect.right - 7.0, rect.bottom)
}

fn branch_row(
    rect: UiRect,
    repository: &RepositorySnapshot,
    branch: &BranchInfo,
    display: &str,
    depth: usize,
    selected: bool,
    state: &Binding,
) -> Element {
    let inner = inner(rect);
    let root = repository.worktree_root.clone();
    // The current branch is HEAD, which needs no scope of its own.
    let name = (!branch.current).then(|| branch.name.clone());
    let click_state = state.clone();
    // Another checkout of this branch: open it, add it, or delete it.
    let has_worktree = branch
        .worktree_path
        .as_ref()
        .is_some_and(|path| *path != repository.worktree_root);
    let indented = UiRect::new(
        inner.left + depth as f32 * INDENT,
        inner.top,
        inner.right,
        inner.bottom,
    );
    let (icon, text_element) = label(
        indented,
        if branch.remote { "cloud" } else { "git-branch" },
        &branch.name,
        display,
        selected,
        if has_worktree { 18.0 } else { 0.0 },
    );
    let open_menu = {
        let state = state.clone();
        let repository = root.clone();
        let name = branch.name.clone();
        let remote = branch.remote;
        move |x: f32, y: f32| {
            state.try_update(|app| {
                let Some((_, branches)) = app
                    .ui
                    .history_branches
                    .as_ref()
                    .filter(|(root, _)| *root == repository)
                else {
                    return false;
                };
                let Some(branch) = branches
                    .iter()
                    .find(|branch| branch.name == name && branch.remote == remote)
                else {
                    return false;
                };
                let checkout = crate::ui::git::branch_picker::checkout_branch(branch, branches);
                app.ui.clear_hover_cards();
                app.ui.menu = Some(GitMenu {
                    pos: (x, y),
                    target: GitMenuTarget::Branch {
                        repository: repository.clone(),
                        checkout,
                    },
                    hover: None,
                });
                true
            });
        }
    };
    let worktree_menu = has_worktree.then(|| open_menu.clone());
    let mut element = row(
        format!("git-history-branch-{}", branch.name),
        inner,
        selected,
        format!("Show commits of {}", branch.name),
        move || {
            click_state.dispatch(Command::SelectHistoryBranch {
                root: root.clone(),
                name: name.clone(),
            });
        },
    )
    .on_pointer_down_with_button(move |cx, pointer, button| {
        if button == PointerButton::Right {
            open_menu(pointer.point.x, pointer.point.y);
            cx.stop_propagation();
        }
    })
    .child(icon)
    .child(text_element);
    if let Some(open_menu) = worktree_menu {
        let icon_rect = UiRect::new(
            inner.right - 20.0,
            inner.top + 2.0,
            inner.right - 2.0,
            inner.bottom - 2.0,
        );
        element = element.child(
            panel(icon_rect, VisualStyle::default().radius(3.0))
                .key(format!("git-history-worktree-{}", branch.name))
                .event_policy(EventPolicy::INTERACTIVE)
                .animation(HOVER_FRAME)
                .semantics(
                    Semantics::new(SemanticRole::Button)
                        .name(format!("Worktree of {}", branch.name)),
                )
                .cursor(CursorIcon::Pointer)
                .on_click(move || open_menu(icon_rect.left, icon_rect.bottom))
                .child(crate::ui::drawer::drawer_icon(
                    UiId::owned(format!("git-history-worktree-icon-{}", branch.name)),
                    "folder",
                    theme::icon_rect(icon_rect),
                    theme::c().text_faint,
                )),
        );
    }
    element
}

fn tag_row(
    rect: UiRect,
    repository: &RepositorySnapshot,
    tag: &TagInfo,
    selected: bool,
    state: &Binding,
) -> Element {
    let inner = inner(rect);
    let root = repository.worktree_root.clone();
    let name = format!("{TAG_PREFIX}{}", tag.name);
    let click_state = state.clone();
    let click_root = root.clone();
    let (icon, text_element) = label(inner, "git-ref-tag", &tag.name, &tag.name, selected, 0.0);
    let element = row(
        format!("git-history-tag-{}", tag.name),
        inner,
        selected,
        format!("Show commits of tag {}", tag.name),
        move || {
            click_state.dispatch(Command::SelectHistoryBranch {
                root: click_root.clone(),
                name: Some(name.clone()),
            });
        },
    )
    .child(icon)
    .child(text_element);
    crate::ui::git::panel::tag_context_menu(element, &root, tag.name.clone(), state)
}

fn stash_row(
    rect: UiRect,
    repository: &RepositorySnapshot,
    stash: &StashInfo,
    selected: bool,
    state: &Binding,
) -> Element {
    let inner = inner(rect);
    let (oid, root) = (stash.oid.clone(), repository.worktree_root.clone());
    let click_state = state.clone();
    let (icon, text_element) = label(
        inner,
        "git-stash",
        &stash.oid,
        &stash.message,
        selected,
        0.0,
    );
    row(
        format!("git-history-stash-{}", stash.oid),
        inner,
        selected,
        format!("Show stash {}", stash.message),
        move || {
            click_state.dispatch(Command::SelectHistoryStash {
                root: root.clone(),
                oid: oid.clone(),
            });
        },
    )
    .child(icon)
    .child(text_element)
}

/// A branch folder: click to fold or unfold what is inside it.
fn folder_row(
    rect: UiRect,
    key: String,
    name: String,
    depth: usize,
    expanded: bool,
    count: usize,
    holds_selection: bool,
    state: &Binding,
) -> Element {
    let inner = inner(rect);
    let left = inner.left + depth as f32 * INDENT;
    let toggle = state.clone();
    let id = key.clone();
    let label = format!("{name}  {count}");
    row(
        format!("git-history-folder-{key}"),
        inner,
        false,
        format!("{name} folder"),
        move || {
            toggle.update(|app| {
                if !app.ui.history_collapsed.remove(&id) {
                    app.ui.history_collapsed.insert(id.clone());
                }
            });
        },
    )
    .child(crate::ui::drawer::drawer_icon(
        UiId::owned(format!("git-history-folder-icon-{key}")),
        if expanded {
            "chevron-down"
        } else {
            "chevron-right"
        },
        theme::icon_rect(UiRect::new(
            left + 2.0,
            inner.top + 5.0,
            left + 14.0,
            inner.bottom - 5.0,
        )),
        theme::c().text_faint,
    ))
    .child(text(
        UiRect::new(left + 18.0, inner.top, inner.right, inner.bottom),
        tabs::ellipsize(&label, inner.right - left - 22.0, theme::SMALL, 400),
        theme::mono(theme::c().text_muted, theme::SMALL),
    ))
    // The selected branch is folded away; keep a trace of where it is.
    .child(text(
        UiRect::new(
            inner.right - 16.0,
            inner.top,
            inner.right - 4.0,
            inner.bottom,
        ),
        if holds_selection { "\u{25cf}" } else { "" },
        theme::mono(theme::c().accent, theme::SMALL),
    ))
}

/// A group title, with the button that creates a new member of the group.
fn heading_row(
    rect: UiRect,
    title: &'static str,
    create: Option<Create>,
    repository: &RepositorySnapshot,
    tag_target: Option<&str>,
    state: &Binding,
    store: &Snapshot,
) -> Element {
    let mut heading = panel(rect, VisualStyle::default()).child(text(
        UiRect::new(rect.left + 14.0, rect.top, rect.right - 30.0, rect.bottom),
        title,
        theme::mono(theme::c().text_ghost, theme::SMALL),
    ));
    let Some(create) = create else {
        return heading;
    };
    let idle = store.get().operation.is_none();
    let (name, enabled) = match create {
        Create::Branch => ("New Branch or Worktree", idle),
        Create::Tag => ("New Tag at Selected Commit", idle && tag_target.is_some()),
        Create::Stash => ("Stash Changes", idle && !repository.files.is_empty()),
    };
    let button = UiRect::new(
        rect.right - 26.0,
        rect.top + 2.0,
        rect.right - 6.0,
        rect.bottom - 2.0,
    );
    let (state, store) = (state.clone(), store.clone());
    let root = repository.worktree_root.clone();
    let target = tag_target.map(str::to_owned);
    let mut semantics = Semantics::new(SemanticRole::Button).name(name);
    semantics.state.disabled = !enabled;
    heading = heading.child(
        Element::new(move |cx| {
            let hovered = enabled && cx.context.interaction_flags(&cx.id).hovered;
            UiElement::panel(
                cx.id,
                button,
                if hovered {
                    VisualStyle::filled(theme::c().surface).radius(3.0)
                } else {
                    VisualStyle::default().radius(3.0)
                },
            )
            .children(cx.children)
        })
        .key(format!("git-history-create-{title}"))
        .event_policy(EventPolicy::INTERACTIVE)
        .animation(HOVER_FRAME)
        .semantics(semantics)
        .cursor(if enabled {
            CursorIcon::Pointer
        } else {
            CursorIcon::Default
        })
        .on_click(move || {
            if !enabled {
                return;
            }
            match create {
                Create::Branch => {
                    crate::ui::git::branch_picker::open(&state, &store, rect.left);
                }
                Create::Tag => {
                    if let Some(target) = &target {
                        state.dispatch(Command::RequestCreateTag {
                            root: root.clone(),
                            branch: None,
                            target: target.clone(),
                        });
                    }
                }
                Create::Stash => {
                    state.dispatch(Command::StashAll);
                }
            }
        })
        .child(crate::ui::drawer::drawer_icon(
            UiId::owned(format!("git-history-create-icon-{title}")),
            "plus",
            theme::icon_rect(button),
            if enabled {
                theme::c().text_muted
            } else {
                theme::c().text_ghost
            },
        )),
    );
    heading
}

/// The icon strip that picks which kind of reference the navigator lists.
fn rail(rect: UiRect, view: NavView, counts: [usize; 3], state: &Binding) -> Element {
    let views = [
        (NavView::Branches, "git-branch", "Branches"),
        (NavView::Tags, "git-ref-tag", "Tags"),
        (NavView::Stashes, "git-stash", "Stashes"),
    ];
    let mut rail = panel(rect, VisualStyle::default());
    for (index, ((nav, icon, name), count)) in views.into_iter().zip(counts).enumerate() {
        let selected = nav == view;
        let size = rect.width() - 6.0;
        let center_y = rect.top + PAD + 14.0 + index as f32 * 32.0;
        let top = center_y - size / 2.0;
        let button = UiRect::new(rect.left + 3.0, top, rect.right - 3.0, top + size);
        let color = if selected {
            theme::c().accent
        } else {
            theme::c().text_muted
        };
        let click_state = state.clone();
        let mut semantics = Semantics::new(SemanticRole::Button).name(format!("{name} ({count})"));
        semantics.state.selected = selected;
        rail = rail.child(
            Element::new(move |cx| {
                let hovered = cx.context.interaction_flags(&cx.id).hovered;
                UiElement::panel(cx.id, button, hover_fill(hovered, selected).radius(4.0))
                    .children(cx.children)
            })
            .key(format!("git-history-nav-{name}"))
            .event_policy(EventPolicy::INTERACTIVE)
            .animation(HOVER_FRAME)
            .semantics(semantics)
            .cursor(CursorIcon::Pointer)
            .on_click(move || {
                click_state.update(|app| {
                    app.ui.history_nav = nav;
                    app.ui.history_branch_scroll = 0.0;
                });
            })
            .child(crate::ui::drawer::drawer_icon(
                UiId::owned(format!("git-history-nav-icon-{name}")),
                icon,
                theme::icon_rect(button),
                color,
            )),
        );
    }
    rail
}

#[cfg(test)]
mod tests {
    use super::*;

    fn branch(name: &str, remote: bool) -> BranchInfo {
        BranchInfo {
            name: name.into(),
            remote,
            current: false,
            oid: String::new(),
            upstream: None,
            worktree_path: None,
        }
    }

    fn shape(entries: &[Entry<'_>]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| match entry {
                Entry::Folder {
                    name,
                    depth,
                    expanded,
                    count,
                    ..
                } => format!(
                    "{}{name}/ {count}{}",
                    " ".repeat(*depth),
                    if *expanded { "" } else { " (folded)" }
                ),
                Entry::Branch { display, depth, .. } => format!("{}{display}", " ".repeat(*depth)),
                _ => String::new(),
            })
            .collect()
    }

    #[test]
    fn slashes_in_branch_names_become_folders_with_folders_first() {
        let branches = [
            branch("main", false),
            branch("feature/b", false),
            branch("feature/a", false),
            branch("fix/deep/x", false),
        ];
        let mut out = Vec::new();
        group(
            LOCAL,
            "Local",
            "b:",
            branches.iter(),
            &HashSet::new(),
            None,
            &mut out,
        );
        assert_eq!(
            shape(&out),
            [
                "Local/ 4",
                " feature/ 2",
                "  a",
                "  b",
                " fix/ 1",
                "  deep/ 1",
                "   x",
                " main"
            ]
        );
    }

    #[test]
    fn remote_branches_group_under_their_remote_and_folders_can_fold() {
        let branches = [
            branch("origin/main", true),
            branch("origin/feature/a", true),
        ];
        let collapsed = HashSet::from(["r:origin/feature".to_owned()]);
        let mut out = Vec::new();
        group(
            REMOTE,
            "Remote",
            "r:",
            branches.iter(),
            &collapsed,
            None,
            &mut out,
        );
        assert_eq!(
            shape(&out),
            ["Remote/ 2", " origin/ 2", "  feature/ 1 (folded)", "  main"]
        );
    }

    #[test]
    fn scrolled_past_folders_pin_by_depth_until_their_contents_end() {
        let leaf = branch("a", false);
        let folder = |name: &str, depth| Entry::Folder {
            key: name.into(),
            name: name.into(),
            depth,
            expanded: true,
            count: 1,
            holds_selection: false,
        };
        let branch_row = |depth| Entry::Branch {
            info: &leaf,
            depth,
            display: "a".into(),
        };
        // Local/ (0), feature/ (1), a, b (2), main (1), Remote/ (0)
        let rows = [
            folder("Local", 0),
            folder("feature", 1),
            branch_row(2),
            branch_row(2),
            branch_row(1),
            folder("Remote", 0),
        ];
        let tops: Vec<f32> = (0..rows.len()).map(|i| i as f32 * ROW_H).collect();
        let bottom = rows.len() as f32 * ROW_H;
        // Nothing scrolled away yet.
        assert!(pinned_folders(&rows, &tops, bottom, 0.0).is_empty());
        // Local and feature are above the top, still holding rows in view; feature
        // is pushed up because its last row ends 88px down.
        let pinned = pinned_folders(&rows, &tops, bottom, 2.5 * ROW_H);
        assert_eq!(pinned, vec![(0, 2.5 * ROW_H), (1, 3.0 * ROW_H)]);
        // Past feature's last row, only Local stays pinned.
        let pinned = pinned_folders(&rows, &tops, bottom, 4.2 * ROW_H);
        assert_eq!(
            pinned.iter().map(|(index, _)| *index).collect::<Vec<_>>(),
            vec![0]
        );
    }

    #[test]
    fn a_folded_group_marks_that_it_holds_the_selected_branch() {
        let branches = [branch("origin/main", true), branch("origin/dev", true)];
        let collapsed = HashSet::from([REMOTE.to_owned()]);
        for (selected, marked) in [
            (Some("origin/dev"), true),
            (Some("other"), false),
            (None, false),
        ] {
            let mut out = Vec::new();
            group(
                REMOTE,
                "Remote",
                "r:",
                branches.iter(),
                &collapsed,
                selected,
                &mut out,
            );
            assert_eq!(out.len(), 1);
            assert!(matches!(
                out[0],
                Entry::Folder { holds_selection, .. } if holds_selection == marked
            ));
        }
    }
}
