//! Source Control menus: a changed file's context menu, a history commit's
//! context menu, and the commit options menu beside the commit button.

use std::path::PathBuf;

use lgui::core::{
    CursorIcon, EventPolicy, SemanticRole, Semantics, UiEventContext, UiEventKind, UiFocusHandle,
};
use lgui::prelude::{Color, Element, ShadowStyle, UiRect, VisualStyle, panel, text};
use lgui::services::ServicesContextExt;

use crate::features::git::operation::operation_text;
use crate::features::git::{ConflictSide, DiffRequest, DiffTarget, GitStoreSnapshot};
use crate::ui::git::commands::{Binding, Command, Snapshot};
use crate::ui::git::panel::{CONFLICTS, STAGED, UNSTAGED, UNTRACKED};
use crate::ui::git::state::GitPanelState;
use crate::ui::theme;

pub(crate) const MENU_W: f32 = 220.0;
pub(crate) const PAD: f32 = 4.0;
pub(crate) const ITEM_H: f32 = 22.0;
pub(crate) const SEP_H: f32 = 5.0;
pub(crate) const CHECK_W: f32 = 22.0;

#[derive(Clone, Debug, PartialEq)]
pub enum GitMenuTarget {
    /// A changed file, relative to `repository`, in drawer section `section`.
    File {
        repository: PathBuf,
        path: PathBuf,
        section: u8,
    },
    Commit,
    Submit,
    /// A commit in the history list.
    HistoryCommit {
        repository: PathBuf,
        oid: String,
    },
    Tags {
        repository: PathBuf,
        name: String,
    },
    /// A branch's linked worktree.
    Worktree {
        repository: PathBuf,
        path: PathBuf,
        removable: bool,
    },
    TagPushRemotes {
        repository: PathBuf,
        name: String,
        request: u64,
        remotes: Option<Result<Vec<String>, String>>,
    },
    /// The workspace's repositories, to choose the active one.
    Repositories,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GitMenu {
    /// Where the menu opens; it flips to stay inside the window.
    pub pos: (f32, f32),
    pub target: GitMenuTarget,
    pub hover: Option<usize>,
}

impl GitMenu {
    pub(crate) fn finish_tag_remotes(
        &mut self,
        request: u64,
        result: Result<Vec<String>, String>,
    ) -> bool {
        if let GitMenuTarget::TagPushRemotes {
            request: current,
            remotes,
            ..
        } = &mut self.target
            && *current == request
        {
            *remotes = Some(result);
            true
        } else {
            false
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuAction {
    OpenChanges,
    OpenFile,
    Stage,
    Unstage,
    Discard,
    CopyPath,
    CopyRelativePath,
    Reveal,
    ToggleAmend,
    ToggleSignoff,
    UndoCommit,
    StashAll,
    CommitAndPush,
    /// Give up the merge, rebase, cherry-pick or revert in progress, or end a bisect.
    AbortOperation,
    KeepCurrent,
    TakeIncoming,
    /// Make the `n`th repository, in snapshot order, active.
    SelectRepository(usize),
    OpenCommitChanges,
    RevertCommit,
    CreateTag,
    DeleteTag,
    ChooseTagRemote,
    PushTag(usize),
    OpenWorktree,
    AddWorktree,
    RemoveWorktree,
}

pub(crate) enum Entry {
    Item {
        label: String,
        /// `Some` for a checkable item.
        checked: Option<bool>,
        action: MenuAction,
    },
    Separator,
    Message(String),
}

pub(crate) fn item(label: &str, action: MenuAction) -> Entry {
    Entry::Item {
        label: label.into(),
        checked: None,
        action,
    }
}

/// The selected files when `(section, path)` is one of several.
fn batch_items(app: &GitPanelState, section: u8, path: &PathBuf) -> Option<Vec<(u8, PathBuf)>> {
    let selection = &app.ui.selection;
    (selection.len() > 1 && selection.contains(&(section, path.clone())))
        .then(|| selection.iter().cloned().collect())
}

pub(crate) fn entries(
    target: &GitMenuTarget,
    app: &GitPanelState,
    git: &GitStoreSnapshot,
) -> Vec<Entry> {
    match target {
        GitMenuTarget::File { section, path, .. } if batch_items(app, *section, path).is_some() => {
            // A multi-selection offers only what applies to every file.
            let items = batch_items(app, *section, path).unwrap_or_default();
            let mut entries = Vec::new();
            if items.iter().any(|(section, _)| *section != STAGED) {
                entries.push(item("Stage Changes", MenuAction::Stage));
            }
            if items.iter().any(|(section, _)| *section == STAGED) {
                entries.push(item("Unstage Changes", MenuAction::Unstage));
            }
            if items
                .iter()
                .any(|(section, _)| matches!(*section, UNSTAGED | UNTRACKED))
            {
                entries.push(item("Discard Changes", MenuAction::Discard));
            }
            entries
        }
        GitMenuTarget::File { section, .. } => {
            let mut entries = vec![
                item("Open Changes", MenuAction::OpenChanges),
                item("Open File", MenuAction::OpenFile),
                Entry::Separator,
            ];
            match *section {
                STAGED => entries.push(item("Unstage Changes", MenuAction::Unstage)),
                CONFLICTS => entries.extend([
                    item("Keep Current (Ours)", MenuAction::KeepCurrent),
                    item("Take Incoming (Theirs)", MenuAction::TakeIncoming),
                    item("Mark as Resolved", MenuAction::Stage),
                ]),
                _ => {
                    entries.push(item("Stage Changes", MenuAction::Stage));
                    entries.push(item(
                        if *section == UNTRACKED {
                            "Delete File"
                        } else {
                            "Discard Changes"
                        },
                        MenuAction::Discard,
                    ));
                }
            }
            entries.extend([
                Entry::Separator,
                item("Copy Path", MenuAction::CopyPath),
                item("Copy Relative Path", MenuAction::CopyRelativePath),
                item("Reveal in File Explorer", MenuAction::Reveal),
            ]);
            entries
        }
        // The commit message is unused mid-operation, so the button's menu
        // offers the way out instead of commit options.
        GitMenuTarget::Submit
            if git.active().is_some_and(|repository| {
                operation_text(repository.repository_state).is_some()
            }) =>
        {
            let wording = git
                .active()
                .and_then(|repository| operation_text(repository.repository_state));
            vec![item(
                wording.map_or("Abort", |wording| wording.abort_label),
                MenuAction::AbortOperation,
            )]
        }
        GitMenuTarget::Submit => {
            let mut actions = vec![
                if crate::ui::git::drawer::commit_reason(app, git).is_none() {
                    item("Commit and Push", MenuAction::CommitAndPush)
                } else {
                    Entry::Message("Commit and Push".into())
                },
                Entry::Separator,
            ];
            actions.extend(entries(&GitMenuTarget::Commit, app, git));
            actions
        }
        GitMenuTarget::Commit => vec![
            Entry::Item {
                label: "Amend Last Commit".into(),
                checked: Some(app.ui.commit_amend),
                action: MenuAction::ToggleAmend,
            },
            Entry::Item {
                label: "Sign Off".into(),
                checked: Some(app.ui.commit_signoff),
                action: MenuAction::ToggleSignoff,
            },
            Entry::Separator,
            item("Undo Last Commit", MenuAction::UndoCommit),
            item("Stash All Changes", MenuAction::StashAll),
        ],
        GitMenuTarget::HistoryCommit { .. } => vec![
            item("Open Commit Changes", MenuAction::OpenCommitChanges),
            item("New Tag…", MenuAction::CreateTag),
            Entry::Separator,
            item("Revert Commit", MenuAction::RevertCommit),
        ],
        GitMenuTarget::Tags { .. } => vec![
            item("Push Tag to Remote…", MenuAction::ChooseTagRemote),
            Entry::Separator,
            item("Delete Tag…", MenuAction::DeleteTag),
        ],
        GitMenuTarget::Worktree { removable, .. } => {
            let mut entries = vec![
                item("Open Worktree", MenuAction::OpenWorktree),
                item("Add to Workspace", MenuAction::AddWorktree),
            ];
            if *removable {
                entries.extend([
                    Entry::Separator,
                    item("Delete Worktree…", MenuAction::RemoveWorktree),
                ]);
            }
            entries
        }
        GitMenuTarget::TagPushRemotes { remotes, .. } => match remotes {
            None => vec![Entry::Message("Loading remotes…".into())],
            Some(Err(error)) => vec![Entry::Message(error.clone())],
            Some(Ok(remotes)) if remotes.is_empty() => {
                vec![Entry::Message("No remotes configured.".into())]
            }
            Some(Ok(remotes)) => remotes
                .iter()
                .enumerate()
                .map(|(index, remote)| {
                    item(&format!("Push to {remote}"), MenuAction::PushTag(index))
                })
                .collect(),
        },
        GitMenuTarget::Repositories => {
            let active = git.active().map(|repository| repository.id);
            git.repositories
                .values()
                .enumerate()
                .map(|(index, repository)| Entry::Item {
                    label: crate::ui::display_path(&repository.worktree_root),
                    checked: Some(Some(repository.id) == active),
                    action: MenuAction::SelectRepository(index),
                })
                .collect()
        }
    }
}

pub(crate) fn perform(
    action: MenuAction,
    target: &GitMenuTarget,
    cx: &mut UiEventContext,
    state: &Binding,
    store: &Snapshot,
    editor_focus: &UiFocusHandle,
    pos: (f32, f32),
) {
    if let GitMenuTarget::File {
        repository,
        path,
        section,
    } = target
    {
        if let Some(items) = batch_items(&state.get(), *section, path) {
            let in_sections = |wanted: &dyn Fn(u8) -> bool| {
                items
                    .iter()
                    .filter(|(section, _)| wanted(*section))
                    .map(|(_, path)| path.clone())
                    .collect::<Vec<_>>()
            };
            match action {
                MenuAction::Stage => {
                    state.dispatch(Command::StagePaths {
                        paths: in_sections(&|section| section != STAGED),
                    });
                }
                MenuAction::Unstage => {
                    state.dispatch(Command::UnstagePaths {
                        paths: in_sections(&|section| section == STAGED),
                    });
                }
                MenuAction::Discard => {
                    state.dispatch(Command::RequestDiscard {
                        tracked: in_sections(&|section| section == UNSTAGED),
                        untracked: in_sections(&|section| section == UNTRACKED),
                    });
                }
                _ => return,
            }
            state.update(|app| {
                app.ui.selection.clear();
                app.ui.selection_anchor = None;
            });
            return;
        }
        let absolute = repository.join(path);
        let paths = vec![path.clone()];
        match action {
            MenuAction::OpenChanges => {
                {
                    state.dispatch(Command::OpenDiff {
                        request: DiffRequest {
                            repository_root: repository.clone(),
                            path: path.clone(),
                            target: match *section {
                                STAGED => DiffTarget::HeadToIndex,
                                CONFLICTS => DiffTarget::HeadToWorktree,
                                _ => DiffTarget::IndexToWorktree,
                            },
                        },
                        untracked: *section == UNTRACKED,
                    });
                };
                editor_focus.focus();
            }
            MenuAction::OpenFile => {
                if state.dispatch(Command::OpenFile { path: absolute }) {
                    editor_focus.focus();
                }
            }
            MenuAction::Stage => {
                state.dispatch(Command::StagePaths { paths });
            }
            MenuAction::Unstage => {
                state.dispatch(Command::UnstagePaths { paths });
            }
            MenuAction::Discard => {
                if *section == UNTRACKED {
                    state.dispatch(Command::RequestDiscard {
                        tracked: Vec::new(),
                        untracked: paths,
                    });
                } else {
                    state.dispatch(Command::RequestDiscard {
                        tracked: paths,
                        untracked: Vec::new(),
                    });
                }
            }
            MenuAction::CopyPath | MenuAction::CopyRelativePath => {
                state.dispatch(Command::CopyPath {
                    path: absolute,
                    relative: action == MenuAction::CopyRelativePath,
                    clipboard: cx.application().clipboard(),
                });
            }
            MenuAction::Reveal => {
                state.dispatch(Command::Reveal { path: absolute });
            }
            MenuAction::KeepCurrent | MenuAction::TakeIncoming => {
                let side = if action == MenuAction::KeepCurrent {
                    ConflictSide::Ours
                } else {
                    ConflictSide::Theirs
                };
                state.dispatch(Command::ResolveConflict {
                    path: path.clone(),
                    side,
                });
            }
            MenuAction::ToggleAmend
            | MenuAction::ToggleSignoff
            | MenuAction::UndoCommit
            | MenuAction::StashAll
            | MenuAction::CommitAndPush
            | MenuAction::AbortOperation
            | MenuAction::SelectRepository(_)
            | MenuAction::OpenCommitChanges
            | MenuAction::RevertCommit
            | MenuAction::CreateTag
            | MenuAction::ChooseTagRemote
            | MenuAction::PushTag(_)
            | MenuAction::OpenWorktree
            | MenuAction::AddWorktree
            | MenuAction::RemoveWorktree
            | MenuAction::DeleteTag => {}
        }
        return;
    }
    if let GitMenuTarget::Worktree {
        repository, path, ..
    } = target
    {
        match action {
            MenuAction::OpenWorktree => {
                state.dispatch(Command::OpenWorktree { path: path.clone() });
            }
            MenuAction::AddWorktree => {
                state.dispatch(Command::AddWorktreeFolder { path: path.clone() });
            }
            MenuAction::RemoveWorktree => {
                state.dispatch(Command::RequestRemoveWorktree {
                    repository: repository.clone(),
                    path: path.clone(),
                    force: false,
                });
            }
            _ => {}
        }
        return;
    }
    if let GitMenuTarget::Tags { repository, name } = target {
        match action {
            MenuAction::ChooseTagRemote => {
                state.dispatch(Command::RequestPushTag {
                    root: repository.clone(),
                    name: name.clone(),
                    pos,
                });
            }
            MenuAction::DeleteTag => {
                state.dispatch(Command::RequestDeleteTag {
                    root: repository.clone(),
                    name: name.clone(),
                });
            }
            _ => {}
        }
        return;
    }
    if let GitMenuTarget::TagPushRemotes {
        repository,
        name,
        remotes: Some(Ok(remotes)),
        ..
    } = target
    {
        if let MenuAction::PushTag(index) = action
            && let Some(remote) = remotes.get(index)
        {
            state.dispatch(Command::PushTag {
                root: repository.clone(),
                name: name.clone(),
                remote: remote.clone(),
            });
        }
        return;
    }
    if let GitMenuTarget::HistoryCommit { repository, oid } = target {
        match action {
            MenuAction::CreateTag => {
                state.dispatch(Command::RequestCreateTag {
                    root: repository.clone(),
                    branch: None,
                    target: oid.clone(),
                });
            }
            MenuAction::OpenCommitChanges => {
                {
                    state.dispatch(Command::OpenCommitChanges {
                        root: repository.clone(),
                        oid: oid.clone(),
                        focus: None,
                    });
                };
                editor_focus.focus();
            }
            MenuAction::RevertCommit => {
                state.dispatch(Command::RevertCommit {
                    root: (repository).to_path_buf(),
                    oid: oid.clone(),
                });
            }
            _ => {}
        }
        return;
    }
    match action {
        MenuAction::ToggleAmend => state.update(|app| app.ui.commit_amend = !app.ui.commit_amend),
        MenuAction::ToggleSignoff => {
            state.update(|app| app.ui.commit_signoff = !app.ui.commit_signoff)
        }
        MenuAction::UndoCommit => {
            state.dispatch(Command::UndoLastCommit);
        }
        MenuAction::StashAll => {
            state.dispatch(Command::StashAll);
        }
        MenuAction::CommitAndPush => {
            state.dispatch(Command::CommitAndPush);
        }
        MenuAction::AbortOperation => {
            state.dispatch(Command::RequestAbort);
        }
        MenuAction::SelectRepository(index) => {
            let root = store
                .get()
                .repositories
                .values()
                .nth(index)
                .map(|repository| repository.worktree_root.clone());
            if let Some(root) = root {
                {
                    state.dispatch(Command::SelectRepository { root });
                };
            }
        }
        _ => {}
    }
}

pub(crate) fn measure(s: &str) -> f32 {
    crate::ui::tabs::measure(s, theme::UI_SIZE, 400)
}

pub fn render(
    viewport: UiRect,
    menu: &GitMenu,
    state: Binding,
    store: Snapshot,
    editor_focus: UiFocusHandle,
) -> Element {
    let app = state.get();
    let entries = entries(&menu.target, &app, &store.get());
    let checkable = entries.iter().any(|entry| {
        matches!(
            entry,
            Entry::Item {
                checked: Some(_),
                ..
            }
        )
    });
    let label_left = 10.0 + if checkable { CHECK_W } else { 0.0 };
    let menu_w = entries
        .iter()
        .map(|entry| match entry {
            Entry::Item { label, .. } | Entry::Message(label) => label_left + measure(label) + 24.0,
            Entry::Separator => 0.0,
        })
        .fold(MENU_W, f32::max);
    let menu_h = PAD * 2.0
        + entries
            .iter()
            .map(|entry| match entry {
                Entry::Item { .. } | Entry::Message(_) => ITEM_H,
                Entry::Separator => SEP_H,
            })
            .sum::<f32>();
    let (x, y) = menu.pos;
    let x = if x + menu_w <= viewport.right {
        x
    } else {
        x - menu_w
    };
    let y = if y + menu_h <= viewport.bottom {
        y
    } else {
        y - menu_h
    };
    let x = x.clamp(viewport.left, (viewport.right - menu_w).max(viewport.left));
    let y = y.clamp(viewport.top, (viewport.bottom - menu_h).max(viewport.top));
    let card = UiRect::new(x, y, x + menu_w, y + menu_h);

    let close_state = state.clone();
    let overlay = panel(viewport, VisualStyle::default())
        .event_policy(EventPolicy::INTERACTIVE)
        .on_pointer_down_with_button(move |_cx, pointer, _button| {
            if !card.contains(pointer.point) {
                close_state.update(|app| app.ui.menu = None);
            }
        });
    let clear_state = state.clone();
    let mut surface = theme::bordered(card, theme::c().surface, theme::c().border, 6.0, 1.0)
        .shadow(
            ShadowStyle::new(Color::BLACK)
                .alpha(theme::c().shadow_alpha)
                .offset(0.0, 2.0)
                .blur(3.0),
        )
        .event_policy(EventPolicy::INTERACTIVE)
        .on_event_capture(UiEventKind::PointerMove, move |_cx, _payload| {
            clear_state.try_update(|app| {
                let Some(menu) = app.ui.menu.as_mut() else {
                    return false;
                };
                menu.hover.take().is_some()
            });
        });

    let mut y = card.top + PAD;
    let mut index = 0usize;
    for entry in entries {
        if let Entry::Message(message) = &entry {
            surface = surface.child(text(
                UiRect::new(card.left + 10.0, y, card.right - 10.0, y + ITEM_H),
                message.clone(),
                theme::mono(theme::c().text_dim, theme::UI_SIZE),
            ));
            y += ITEM_H;
            continue;
        }
        let Entry::Item {
            label,
            checked,
            action,
        } = entry
        else {
            let line_y = y + (SEP_H - 1.0) / 2.0;
            surface = surface.child(panel(
                UiRect::new(card.left + 10.0, line_y, card.right - 10.0, line_y + 1.0),
                VisualStyle::filled(theme::c().border),
            ));
            y += SEP_H;
            continue;
        };
        let rect = UiRect::new(card.left + 1.0, y, card.right - 1.0, y + ITEM_H);
        let hovered = menu.hover == Some(index);
        let idx = index;
        let hover_state = state.clone();
        let click_state = state.clone();
        let click_store = store.clone();
        let click_focus = editor_focus.clone();
        let target = menu.target.clone();
        let pos = menu.pos;
        let mut row = panel(
            rect,
            if hovered {
                VisualStyle::filled(theme::c().selection)
            } else {
                VisualStyle::default()
            },
        )
        .event_policy(EventPolicy::INTERACTIVE)
        .semantics(Semantics::new(SemanticRole::MenuItem).name(label.clone()))
        .cursor(CursorIcon::Pointer)
        .on_event(UiEventKind::PointerMove, move |_cx, _payload| {
            hover_state.try_update(move |app| {
                let Some(menu) = app.ui.menu.as_mut() else {
                    return false;
                };
                let changed = menu.hover != Some(idx);
                menu.hover = Some(idx);
                changed
            });
        })
        .on_click(move |cx: &mut UiEventContext| {
            click_state.update(|app| app.ui.menu = None);
            perform(
                action,
                &target,
                cx,
                &click_state,
                &click_store,
                &click_focus,
                pos,
            );
        });
        let color = if hovered {
            theme::c().text_bright
        } else {
            theme::c().text_soft
        };
        if checked == Some(true) {
            row = row.child(crate::ui::drawer::drawer_icon(
                "git-menu.check",
                "check",
                UiRect::new(
                    rect.left + 10.0,
                    rect.top + 4.0,
                    rect.left + 24.0,
                    rect.top + 18.0,
                ),
                color,
            ));
        }
        row = row.child(text(
            UiRect::new(
                rect.left + label_left,
                rect.top,
                rect.right - 10.0,
                rect.bottom,
            ),
            label,
            theme::mono(color, theme::UI_SIZE),
        ));
        surface = surface.child(row);
        y += ITEM_H;
        index += 1;
    }
    overlay.child(surface)
}
