//! Data and operation adapters for the single Explorer tree. No layout or UI nodes.
use super::commands::{Command, Decoration, Frame, MenuAction};
use super::state::ExplorerPanelState;
use crate::features::explorer::{ExplorerContextTarget, ExplorerCreateKind, ExplorerTargetKind};
use crate::features::resource_tree::{EditKind, Target};
use lgui::core::PointerButton;
use loog_workspace_api::{FileInfo, FileKind, ResourceRef};
use std::path::PathBuf;

#[derive(Clone, Debug)]
enum Origin {
    Local(PathBuf),
    Provided(Target, Option<FileInfo>),
    Action(Command),
}

#[derive(Clone, Debug)]
pub(super) struct Entry {
    pub key: String,
    pub name: String,
    pub directory: bool,
    pub resource: Option<ResourceRef>,
    origin: Origin,
}

impl Entry {
    pub fn local(path: PathBuf, name: String, directory: bool) -> Self {
        Self {
            key: path.to_string_lossy().into_owned(),
            name,
            directory,
            resource: Some(ResourceRef::local(path.clone())),
            origin: Origin::Local(path),
        }
    }

    fn provided(
        root: &crate::features::resource_tree::Root,
        target: Target,
        file: Option<FileInfo>,
    ) -> Self {
        let resource = ResourceRef::Provided {
            environment: root.descriptor.resource.environment().unwrap().clone(),
            path: target.path.clone(),
        };
        Self {
            key: format!("{}:{}", target.scope, resource.context_key()),
            name: target.name.clone(),
            directory: target.kind == FileKind::Directory,
            resource: Some(resource),
            origin: Origin::Provided(target, file),
        }
    }

    fn action(key: String, name: String, command: Command) -> Self {
        Self {
            key,
            name,
            directory: false,
            resource: None,
            origin: Origin::Action(command),
        }
    }

    pub fn expanded(&self, panel: &ExplorerPanelState) -> bool {
        match &self.origin {
            Origin::Local(_) => panel.model.is_expanded(&self.key),
            Origin::Provided(target, _) => panel
                .resources
                .root(target.scope)
                .and_then(|root| root.directories.get(&target.path))
                .is_some_and(|dir| dir.expanded),
            Origin::Action(_) => false,
        }
    }

    pub fn children(&self, panel: &ExplorerPanelState, depth: usize) -> Vec<Self> {
        match &self.origin {
            Origin::Local(path) => panel
                .model
                .listing(&self.key)
                .map(|entries| {
                    entries
                        .iter()
                        .map(|entry| {
                            Self::local(path.join(&entry.name), entry.name.clone(), entry.is_dir)
                        })
                        .collect()
                })
                .unwrap_or_default(),
            Origin::Provided(target, _) if depth < 64 => {
                let Some(root) = panel.resources.root(target.scope) else {
                    return vec![];
                };
                let Some(directory) = root.directories.get(&target.path) else {
                    return vec![];
                };
                let mut result = Vec::new();
                if target.parent.is_none()
                    && let Some(error) = &root.watch.error
                {
                    result.push(Self::action(
                        format!("{}:watch", self.key),
                        format!("Monitoring paused: {error} — Refresh"),
                        Command::ResourceAction {
                            action: MenuAction::RefreshExplorer,
                            target: target.clone(),
                        },
                    ));
                }
                if directory.pending.is_some()
                    || directory.error.is_some()
                    || directory.cursor.is_some()
                {
                    let label = directory
                        .error
                        .as_ref()
                        .map(|error| format!("{error} — Retry"))
                        .unwrap_or_else(|| {
                            if directory.pending.is_some() {
                                "Loading…".into()
                            } else {
                                "Load more…".into()
                            }
                        });
                    result.push(Self::action(
                        format!("{}:status", self.key),
                        label,
                        Command::ResourceDirectory {
                            scope: target.scope,
                            path: target.path.clone(),
                            more: directory.error.is_none(),
                        },
                    ));
                }
                let mut files: Vec<_> = directory.entries.iter().collect();
                files.sort_by(|a, b| {
                    (b.kind == FileKind::Directory)
                        .cmp(&(a.kind == FileKind::Directory))
                        .then_with(|| a.name.cmp(&b.name))
                });
                result.extend(files.into_iter().map(|file| {
                    Self::provided(
                        root,
                        Target {
                            scope: target.scope,
                            path: file.path.clone(),
                            parent: Some(target.path.clone()),
                            name: file.name.clone(),
                            kind: file.kind.clone(),
                        },
                        Some(file.clone()),
                    )
                }));
                result
            }
            _ => vec![],
        }
    }

    pub fn renaming(&self, panel: &ExplorerPanelState) -> bool {
        match &self.origin {
            Origin::Local(path) => panel
                .model
                .rename_request()
                .is_some_and(|edit| edit.path == *path),
            Origin::Provided(target, _) => panel.resources.edit.as_ref().is_some_and(|edit| {
                edit.kind == EditKind::Rename
                    && edit.target.scope == target.scope
                    && edit.target.path == target.path
            }),
            _ => false,
        }
    }

    pub fn creating(&self, panel: &ExplorerPanelState) -> Option<ExplorerCreateKind> {
        match &self.origin {
            Origin::Local(path) => panel
                .model
                .create_request()
                .filter(|edit| edit.parent == *path)
                .map(|edit| edit.kind),
            Origin::Provided(target, _) => panel
                .resources
                .edit
                .as_ref()
                .filter(|edit| edit.target.scope == target.scope && edit.target.path == target.path)
                .and_then(|edit| match edit.kind {
                    EditKind::NewFile => Some(ExplorerCreateKind::File),
                    EditKind::NewFolder => Some(ExplorerCreateKind::Folder),
                    _ => None,
                }),
            _ => None,
        }
    }

    pub fn selected(&self, panel: &ExplorerPanelState, frame: &Frame<'_>) -> bool {
        self.resource
            .as_ref()
            .is_some_and(|resource| frame.active_resource == Some(resource))
            || matches!(&self.origin, Origin::Local(path) if panel.ui.selection.contains(path) || frame.active_path == Some(path.as_path()))
    }

    pub fn decoration(&self, frame: &Frame<'_>) -> Decoration {
        match &self.origin {
            Origin::Local(path) => (frame.decorate)(path),
            _ => Decoration::default(),
        }
    }

    pub fn press(&self, point: (f32, f32), button: PointerButton) -> Option<Command> {
        match (&self.origin, button) {
            (Origin::Local(path), PointerButton::Right) => Some(Command::OpenMenu {
                point,
                target: Some(ExplorerContextTarget {
                    path: path.clone(),
                    kind: if self.directory {
                        ExplorerTargetKind::Directory
                    } else {
                        ExplorerTargetKind::File
                    },
                }),
            }),
            (Origin::Local(_), PointerButton::Left) if self.directory => {
                Some(Command::ToggleDirectory(self.key.clone()))
            }
            (Origin::Local(path), PointerButton::Left) => Some(Command::PressFile {
                path: path.clone(),
                point,
            }),
            (Origin::Local(path), PointerButton::Middle) if !self.directory => {
                Some(Command::OpenPinned(path.clone()))
            }
            (Origin::Provided(target, _), PointerButton::Right) => Some(Command::ResourceMenu {
                point,
                target: target.clone(),
            }),
            (Origin::Provided(target, _), PointerButton::Left) if self.directory => {
                Some(Command::ResourceDirectory {
                    scope: target.scope,
                    path: target.path.clone(),
                    more: false,
                })
            }
            (Origin::Provided(target, Some(file)), PointerButton::Left) => {
                Some(Command::PressResourceFile {
                    scope: target.scope,
                    file: file.clone(),
                    point,
                })
            }
            (Origin::Provided(target, Some(file)), PointerButton::Middle) => {
                Some(Command::ResourceFile {
                    scope: target.scope,
                    file: file.clone(),
                })
            }
            (Origin::Action(command), PointerButton::Left) => Some(command.clone()),
            _ => None,
        }
    }

    pub fn release(&self, point: (f32, f32)) -> Option<Command> {
        match &self.origin {
            Origin::Provided(target, Some(file)) if !self.directory => {
                Some(Command::ReleaseResourceFile {
                    scope: target.scope,
                    file: file.clone(),
                    point,
                })
            }
            _ => None,
        }
    }
}

pub(super) fn roots(panel: &ExplorerPanelState) -> Vec<Entry> {
    let mut entries: Vec<_> = panel
        .model
        .roots()
        .iter()
        .map(|path| {
            Entry::local(
                path.clone(),
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned()),
                true,
            )
        })
        .collect();
    for root in &panel.resources.roots {
        let ResourceRef::Provided { path, .. } = &root.descriptor.resource else {
            continue;
        };
        entries.push(Entry::provided(
            root,
            Target {
                scope: root.descriptor.scope,
                path: path.clone(),
                parent: None,
                name: root.descriptor.name.clone(),
                kind: FileKind::Directory,
            },
            None,
        ));
    }
    for bookmark in &panel.resources.bookmarks {
        if panel
            .resources
            .roots
            .iter()
            .any(|root| root.descriptor.resource == bookmark.resource)
        {
            continue;
        }
        entries.push(Entry::action(
            bookmark.resource.context_key(),
            format!("{} · Offline — Reconnect", bookmark.name),
            Command::ResourceRecovery {
                resource: bookmark.resource.clone(),
                forget: false,
            },
        ));
        entries.push(Entry::action(
            format!("{}:remove", bookmark.resource.context_key()),
            "Remove from workspace".into(),
            Command::ResourceRecovery {
                resource: bookmark.resource.clone(),
                forget: true,
            },
        ));
    }
    entries
}

pub(super) fn supports_action(target: Option<&Target>, action: MenuAction) -> bool {
    if target.is_none() {
        return action != MenuAction::WorkspaceTrust;
    }
    matches!(
        action,
        MenuAction::NewFile
            | MenuAction::NewFolder
            | MenuAction::RefreshExplorer
            | MenuAction::WorkspaceTrust
            | MenuAction::OpenTerminal
            | MenuAction::CopyPath
            | MenuAction::Rename
            | MenuAction::Delete
    )
}

pub(super) fn action_enabled(
    panel: &ExplorerPanelState,
    target: Option<&Target>,
    action: MenuAction,
) -> Option<bool> {
    let target = target?;
    let root = panel.resources.root(target.scope);
    let write = root.is_some_and(|root| {
        root.descriptor
            .capabilities
            .contains(&loog_workspace_api::Capability::FilesWrite)
    }) && !panel.resources.busy.contains(&target.scope);
    Some(match action {
        MenuAction::OpenTerminal => root.is_some_and(|root| {
            root.descriptor
                .capabilities
                .contains(&loog_workspace_api::Capability::Terminal)
        }),
        MenuAction::CopyPath | MenuAction::RefreshExplorer | MenuAction::WorkspaceTrust => true,
        MenuAction::Rename | MenuAction::Delete => write && target.parent.is_some(),
        _ => write,
    })
}

pub(super) fn menu_action(
    action: MenuAction,
    provided: Option<&Target>,
    local: Option<&ExplorerContextTarget>,
) -> Command {
    if let Some(target) = provided {
        Command::ResourceAction {
            action,
            target: target.clone(),
        }
    } else {
        Command::Menu {
            action,
            target: local.cloned(),
        }
    }
}

pub(super) fn deletion_name(panel: &ExplorerPanelState) -> Option<&str> {
    panel
        .resources
        .edit
        .as_ref()
        .filter(|edit| edit.kind == EditKind::Delete)
        .map(|edit| edit.target.name.as_str())
}
