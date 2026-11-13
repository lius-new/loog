//! Persistent file-tree session and recent-workspace history.
use crate::app::context::AppContext;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::features::terminal::ShellKind;

use crate::infrastructure::session_filesystem as filesystem;
pub(crate) use filesystem::session_path;
#[cfg(all(test, windows))]
pub(crate) use filesystem::write_json;

pub use crate::domain::window_geometry::WindowGeometry;

pub use crate::domain::workspace_session::EditorLayout;
#[cfg(test)]
pub use crate::domain::workspace_session::{SavedAxis, SavedChild, SavedPaneNode};
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct WorkspaceSession {
    // COMPAT(local-only-workspace-session): Earlier sessions have no provider
    // roots. Keep them readable as empty while resource-aware sessions add
    // recovery records; remove only with that session format, no version scheduled.
    #[serde(default, deserialize_with = "read_resource_roots")]
    pub resource_roots: Vec<loog_workspace_api::session::ResourceBookmark>,
    #[serde(default)]
    pub open_folders: Vec<PathBuf>,
    #[serde(default)]
    pub recent_folders: Vec<PathBuf>,
    #[serde(default)]
    pub window: Option<WindowGeometry>,
    /// Editor panes and their restorable file tabs.
    #[serde(default)]
    pub editor_layout: Option<EditorLayout>,
    #[serde(default)]
    pub terminal_tabs: Vec<ShellKind>,
    #[serde(default)]
    pub terminal_groups: Vec<usize>,
    #[serde(default)]
    pub terminal_widths: Vec<f32>,
    #[serde(default)]
    pub terminal_purposes: Vec<Option<String>>,
    #[serde(default)]
    pub terminal_titles: Vec<Option<String>>,
    #[serde(default)]
    pub terminal_directories: Vec<Option<PathBuf>>,
    // COMPAT(local-only-terminal-session): Old files omit resource targets and
    // retain local shells. New targets restore stopped, with no input replay.
    // Remove with the old format; no removal version is scheduled.
    #[serde(default)]
    pub terminal_resources: Vec<Option<crate::features::terminal::model::ProvidedTerminal>>,
    #[serde(default)]
    pub terminal_workspace_root: Option<PathBuf>,
    #[serde(default)]
    pub active_terminal: Option<usize>,
    #[serde(default)]
    pub show_terminal: bool,
    #[serde(default)]
    pub terminal_height: Option<f32>,
    #[serde(default)]
    pub source_control_open: bool,
}

fn read_resource_roots<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<loog_workspace_api::session::ResourceBookmark>, D::Error> {
    let roots = Vec::<loog_workspace_api::session::ResourceBookmark>::deserialize(deserializer)?;
    let mut seen = std::collections::HashSet::new();
    if roots.len() > 128 || roots.iter().any(|root| !seen.insert(root.resource.clone())) {
        return Err(serde::de::Error::custom(
            "Too many or duplicate resource recovery records",
        ));
    }
    Ok(roots)
}

pub fn load() -> WorkspaceSession {
    let _guard = persistence_lock()
        .lock()
        .expect("persistence lock poisoned");
    load_checked_unlocked().unwrap_or_default()
}

/// Startup reports invalid/future sessions; update paths preserve the same file.
pub fn load_checked() -> io::Result<WorkspaceSession> {
    let _guard = persistence_lock()
        .lock()
        .expect("persistence lock poisoned");
    load_checked_unlocked()
}
fn load_checked_unlocked() -> io::Result<WorkspaceSession> {
    match session_path() {
        Some(path) => filesystem::read_session(&path),
        None => Ok(WorkspaceSession::default()),
    }
}

pub fn save(open_folders: &[PathBuf], recent_folders: &[PathBuf]) -> io::Result<()> {
    update(|session| {
        session.open_folders = open_folders.to_vec();
        session.recent_folders = recent_folders.to_vec();
    })
}

pub fn save_window_geometry(geometry: WindowGeometry) -> io::Result<()> {
    update(|session| session.window = Some(geometry))
}

pub(crate) fn save_resource_roots(
    roots: Vec<loog_workspace_api::session::ResourceBookmark>,
) -> io::Result<()> {
    if roots.len() > 128 {
        return Err(io::Error::other("Too many saved resource roots"));
    }
    for root in &roots {
        root.validate().map_err(io::Error::other)?;
    }
    let (reply, done) = std::sync::mpsc::sync_channel(1);
    roots_writer()
        .send(RootsWrite {
            roots,
            reply: Some(reply),
        })
        .map_err(|_| io::Error::other("Resource session writer stopped"))?;
    done.recv()
        .map_err(|_| io::Error::other("Resource session writer stopped"))?
}
pub(crate) fn queue_resource_roots(roots: Vec<loog_workspace_api::session::ResourceBookmark>) {
    let _ = roots_writer().send(RootsWrite { roots, reply: None });
}
struct RootsWrite {
    roots: Vec<loog_workspace_api::session::ResourceBookmark>,
    reply: Option<std::sync::mpsc::SyncSender<io::Result<()>>>,
}
fn roots_writer() -> &'static std::sync::mpsc::Sender<RootsWrite> {
    static WRITER: OnceLock<std::sync::mpsc::Sender<RootsWrite>> = OnceLock::new();
    WRITER.get_or_init(|| {
        let (sender, receiver) = std::sync::mpsc::channel::<RootsWrite>();
        std::thread::Builder::new()
            .name("loog-resource-session-writer".into())
            .spawn(move || {
                while let Ok(mut pending) = receiver.recv() {
                    while pending.reply.is_none() {
                        match receiver.recv_timeout(std::time::Duration::from_millis(100)) {
                            Ok(next) => pending = next,
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                        }
                    }
                    let result = update(|session| session.resource_roots = pending.roots);
                    if let Some(reply) = pending.reply {
                        let _ = reply.send(result);
                    }
                }
            })
            .expect("could not start resource session writer");
        sender
    })
}

pub fn save_terminal_workspace_root(root: Option<PathBuf>) -> io::Result<()> {
    update(|session| session.terminal_workspace_root = root)
}

pub fn save_editor_layout(layout: Option<&EditorLayout>) -> io::Result<()> {
    let (reply, done) = std::sync::mpsc::sync_channel(1);
    layout_writer()
        .send(LayoutWrite {
            layout: layout.cloned(),
            reply: Some(reply),
        })
        .map_err(|_| io::Error::other("Session writer stopped"))?;
    done.recv()
        .map_err(|_| io::Error::other("Session writer stopped"))?
}
pub(crate) fn queue_editor_layout(layout: Option<EditorLayout>) {
    let _ = layout_writer().send(LayoutWrite {
        layout,
        reply: None,
    });
}
struct LayoutWrite {
    layout: Option<EditorLayout>,
    reply: Option<std::sync::mpsc::SyncSender<io::Result<()>>>,
}
fn layout_writer() -> &'static std::sync::mpsc::Sender<LayoutWrite> {
    static WRITER: OnceLock<std::sync::mpsc::Sender<LayoutWrite>> = OnceLock::new();
    WRITER.get_or_init(|| {
        start_layout_writer(|layout| update(|session| session.editor_layout = layout))
    })
}
fn start_layout_writer(
    persist: impl Fn(Option<EditorLayout>) -> io::Result<()> + Send + 'static,
) -> std::sync::mpsc::Sender<LayoutWrite> {
    use std::sync::mpsc::{self, RecvTimeoutError};
    let (sender, receiver) = mpsc::channel::<LayoutWrite>();
    std::thread::Builder::new()
        .name("loog-session-writer".into())
        .spawn(move || {
            while let Ok(mut pending) = receiver.recv() {
                if pending.reply.is_none() {
                    loop {
                        match receiver.recv_timeout(std::time::Duration::from_millis(250)) {
                            Ok(next) => {
                                pending = next;
                                if pending.reply.is_some() {
                                    break;
                                }
                            }
                            Err(RecvTimeoutError::Timeout) => break,
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                    }
                }
                let result = persist(pending.layout);
                if let Some(reply) = pending.reply {
                    let _ = reply.send(result);
                }
            }
        })
        .expect("could not start session writer");
    sender
}

pub fn save_terminal_state(
    terminal_tabs: &[ShellKind],
    terminal_groups: &[usize],
    terminal_widths: &[f32],
    terminal_purposes: &[Option<String>],
    terminal_titles: &[Option<String>],
    terminal_directories: &[Option<PathBuf>],
    terminal_resources: &[Option<crate::features::terminal::model::ProvidedTerminal>],
    active_terminal: Option<usize>,
    show_terminal: bool,
    terminal_height: f32,
) -> io::Result<()> {
    update(|session| {
        session.terminal_tabs = terminal_tabs.to_vec();
        session.terminal_groups = terminal_groups.to_vec();
        session.terminal_widths = terminal_widths.to_vec();
        session.terminal_purposes = terminal_purposes.to_vec();
        session.terminal_titles = terminal_titles.to_vec();
        session.terminal_directories = terminal_directories.to_vec();
        session.terminal_resources = terminal_resources.to_vec();
        session.active_terminal = active_terminal;
        session.show_terminal = show_terminal;
        session.terminal_height = Some(terminal_height);
    })
}

pub fn save_source_control_open(source_control_open: bool) -> io::Result<()> {
    update(|session| session.source_control_open = source_control_open)
}

fn update(change: impl FnOnce(&mut WorkspaceSession)) -> io::Result<()> {
    let _guard = persistence_lock()
        .lock()
        .expect("persistence lock poisoned");
    let path = session_path().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "could not determine the user configuration directory",
        )
    })?;
    update_at(&path, change)
}

fn update_at(path: &Path, change: impl FnOnce(&mut WorkspaceSession)) -> io::Result<()> {
    let mut session = filesystem::read_session(path)?;
    change(&mut session);
    save_to(path, &session)
}

fn save_to(path: &Path, session: &WorkspaceSession) -> io::Result<()> {
    filesystem::write_session(path, session)
}

fn persistence_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

use super::state::AppState;
use super::workspace::Workspace;
use crate::domain::identity::FileId;
use crate::domain::workspace_session::{self as editor_session, PendingTab};

pub(crate) fn saved_editor_layout(workspace: &Workspace) -> Option<EditorLayout> {
    saved_layout_with_pending(workspace, None)
}

pub(crate) fn saved_app_editor_layout(app: &AppState) -> Option<EditorLayout> {
    saved_layout_with_pending(&app.workspace, app.editor.pending_file.as_ref())
}

pub(crate) fn save_editor_session(state: &AppContext) -> bool {
    let layout = saved_app_editor_layout(&state.get());
    let result = save_editor_layout(layout.as_ref())
        .and_then(|_| save_resource_roots(state.get().explorer.resources.bookmarks.clone()));
    if let Err(error) = result {
        state.update(|app| app.show_error(format!("Could not remember file tabs: {error}")));
        return false;
    }
    true
}

pub(crate) fn hydrate_editor_layout(app: &mut AppState, layout: Option<EditorLayout>) {
    restore_editor_layout_with(app, layout, |workspace, path| {
        Some(workspace.open_deferred(path))
    });
}

#[cfg(test)]
pub(crate) fn hydrate_editor_layout_with(
    app: &mut AppState,
    layout: Option<EditorLayout>,
    mut read: impl FnMut(&Path) -> Result<String, String>,
) {
    restore_editor_layout_with(app, layout, |workspace, path| {
        Some(match read(&path) {
            Ok(text) => workspace.open_path(path, text),
            Err(error) => {
                workspace.open_failed(path, error, crate::app::workspace::OpenMode::Permanent)
            }
        })
    });
}

fn restore_editor_layout_with(
    app: &mut AppState,
    layout: Option<EditorLayout>,
    mut open: impl FnMut(&mut Workspace, PathBuf) -> Option<FileId>,
) {
    let Some(layout) = layout else {
        return;
    };
    if let Err(error) = layout.validate() {
        app.show_error(format!("Could not restore editor layout: {error}"));
        return;
    }
    let mut contents = Vec::new();
    let shape = editor_session::restored_shape(layout.root, &mut contents);
    let panes = app.workspace.restore_layout(shape);
    let mut restored = Vec::<Vec<Option<crate::domain::identity::ViewId>>>::new();
    let mut associations = Vec::new();
    for (&pane, (items, active)) in panes.iter().zip(contents) {
        let mut restored_items = vec![None; items.len()];
        app.workspace.activate_pane(pane);
        let mut first = None;
        let mut active_id = None;
        for (index, saved) in items.into_iter().enumerate() {
            let resource = saved.resource();
            let existing = app.workspace.file_id_for_resource(&resource);
            let Some(document) = existing.or_else(|| match &saved {
                editor_session::SavedView::Text { path, .. } => {
                    open(&mut app.workspace, path.clone())
                }
                editor_session::SavedView::Resource {
                    resource,
                    root,
                    name,
                    encoding,
                    ..
                } => {
                    let prepared = crate::domain::workspace::PreparedTextFile::offline(
                        resource.clone(),
                        name.clone(),
                        encoding.clone(),
                    )
                    .with_provider_root(root.clone());
                    Some(app.workspace.open_prepared_text(
                        prepared,
                        crate::domain::workspace::OpenMode::Permanent,
                    ))
                }
            }) else {
                continue;
            };
            let temporary = existing.is_none().then(|| app.workspace.active()).flatten();
            if let Some(encoding) = saved.encoding() {
                if let Some(doc) = app.workspace.documents.get_mut(&document) {
                    doc.recovery = Some(encoding.clone());
                    if doc.loading.is_none()
                        && (doc.open_error.is_none() || encoding.unsaved_text.is_some())
                    {
                        crate::domain::workspace::restore_encoding(doc);
                    }
                }
            }
            let view = app.workspace.restore_view(pane, document, &saved);
            restored_items[index] = Some(view);
            if let Some(reference) = saved.association() {
                associations.push((view, reference));
            }
            if let Some(temporary) = temporary {
                app.workspace.close_item(pane, temporary);
            }
            first.get_or_insert(view);
            if active == Some(index) {
                active_id = Some(view);
            }
        }
        if let Some(id) = active_id.or(first) {
            app.workspace.activate(pane, id);
        }
        restored.push(restored_items);
    }
    for (view, reference) in associations {
        if let Some(source) = restored
            .get(reference.pane)
            .and_then(|pane| pane.get(reference.item))
            .copied()
            .flatten()
        {
            if let Some(value) = app.workspace.views.get_mut(&view) {
                value.associated_source = Some(source);
            }
        }
    }
    for &pane in &panes {
        if app.workspace.pane(pane).is_some_and(|pane| pane.is_empty()) {
            app.workspace.close_pane(pane);
        }
    }
    let focus = panes
        .get(layout.active_pane)
        .copied()
        .filter(|pane| app.workspace.pane(*pane).is_some())
        .unwrap_or_else(|| app.workspace.pane_ids()[0]);
    app.workspace.activate_pane(focus);
}
fn saved_layout_with_pending(
    workspace: &Workspace,
    pending: Option<&crate::app::file_loading::PendingFile>,
) -> Option<EditorLayout> {
    let pending = pending.map(|value| PendingTab {
        pane: value.pane,
        path: value.path.clone(),
        insert_before: match value.drop {
            Some(crate::app::state::TabDrop::Insert { index, .. }) => Some(index),
            _ => None,
        },
    });
    editor_session::capture(&workspace.core, pending.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{env, fs};

    #[test]
    fn p1_invalid_or_future_session_is_reported_and_preserved_on_update() {
        let root = env::temp_dir().join(format!("loog-p1-invalid-session-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("session.json");
        for raw in [
            r#"{"editor_layout":{"schema":99,"root":{"pane":{"items":[]}}}}"#,
            r#"{"editor_layout":{"schema":2,"root":{"pane":{"items":[],"active":7}}}}"#,
            "broken json",
        ] {
            fs::write(&path, raw).unwrap();
            let error =
                update_at(&path, |s| s.open_folders = vec!["replacement".into()]).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(fs::read_to_string(&path).unwrap(), raw);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn p1_restore_repeated_path_keeps_views_positions_and_one_deferred_load() {
        use crate::domain::view::Presentation;
        let raw = r#"{"schema":2,"root":{"pane":{"items":[{"kind":"text","path":"shared.md","source":{"cursor":3,"anchor":null,"scroll_x":2,"scroll_y":20}},{"kind":"text","path":"shared.md","presentation":"markdown","reader":{"source_byte":9,"block_fraction":0.5}}],"active":1}},"active_pane":0}"#;
        let layout = serde_json::from_str(raw).unwrap();
        let mut app = AppState::new();
        hydrate_editor_layout(&mut app, Some(layout));
        let items = app.workspace.active_items().to_vec();
        assert_eq!(items.len(), 2);
        assert_ne!(items[0], items[1]);
        let document = app.workspace.view_document(items[0]).unwrap();
        assert_eq!(app.workspace.view_document(items[1]), Some(document));
        assert_eq!(
            app.workspace.loading_files(),
            vec![(document, "shared.md".into())]
        );
        assert_eq!(app.workspace.active(), Some(items[1]));
        assert_eq!(
            app.workspace
                .view(items[0])
                .unwrap()
                .source
                .selection
                .cursor,
            3
        );
        assert_eq!(
            app.workspace.view(items[1]).unwrap().presentation,
            Presentation::Markdown
        );
        let before = saved_editor_layout(&app.workspace).unwrap();
        let incarnation = app.workspace.documents[&document].incarnation;
        assert!(app.workspace.install_restored(
            document,
            Path::new("shared.md"),
            crate::app::workspace::PreparedTextFile::new(
                "shared.md".into(),
                "abc中文\r\ntext".into()
            )
        ));
        assert_eq!(app.workspace.documents[&document].incarnation, incarnation);
        assert_eq!(app.workspace.text_snapshots().len(), 1);
        assert_eq!(before, saved_editor_layout(&app.workspace).unwrap());
        assert!(app.workspace.active_editor_mut().is_none());
        assert_eq!(app.main_surface(), crate::app::state::MainSurface::Markdown);
    }

    #[test]
    fn session_replacement_preserves_other_fields_and_is_readable_after_each_save() {
        let root = env::temp_dir().join(format!("loog-session-atomic-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("workspace.json");
        let mut session = WorkspaceSession {
            recent_folders: vec!["remember-me".into()],
            show_terminal: true,
            ..Default::default()
        };
        save_to(&path, &session).unwrap();
        session.editor_layout = Some(EditorLayout {
            root: SavedPaneNode::Pane {
                items: vec!["single-click.json".into()],
                active: Some(0),
            },
            active_pane: 0,
        });
        save_to(&path, &session).unwrap();
        let decoded: WorkspaceSession = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(decoded, session);
        assert_eq!(
            fs::read_dir(&root).unwrap().count(),
            1,
            "no temporary save should remain"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn workspace_session_round_trips_as_json() {
        let session = WorkspaceSession {
            resource_roots: vec![],
            open_folders: vec![PathBuf::from("one"), PathBuf::from("two")],
            recent_folders: vec![PathBuf::from("two"), PathBuf::from("one")],
            window: Some(WindowGeometry {
                x: Some(120),
                y: Some(80),
                width: 900.0,
                height: 600.0,
                maximized: false,
            }),
            editor_layout: Some(EditorLayout {
                root: SavedPaneNode::Split {
                    axis: SavedAxis::Horizontal,
                    children: vec![
                        SavedChild {
                            size: 0.4,
                            node: SavedPaneNode::Pane {
                                items: vec![PathBuf::from("one/main.rs").into()],
                                active: Some(0),
                            },
                        },
                        SavedChild {
                            size: 0.6,
                            node: SavedPaneNode::Pane {
                                items: vec![PathBuf::from("two/lib.rs").into()],
                                active: None,
                            },
                        },
                    ],
                },
                active_pane: 1,
            }),
            terminal_tabs: vec![
                ShellKind::PowerShell,
                ShellKind::PowerShell,
                ShellKind::Bash,
            ],
            terminal_groups: vec![0, 0, 1],
            terminal_widths: vec![1.5, 0.5, 1.0],
            terminal_purposes: vec![Some("Testing".into()), None, Some("Development".into())],
            terminal_titles: vec![Some("API".into()), None, None],
            terminal_resources: vec![None, None, None],
            terminal_directories: vec![
                Some(PathBuf::from("one")),
                None,
                Some(PathBuf::from("two")),
            ],
            active_terminal: Some(1),
            terminal_workspace_root: Some(PathBuf::from("two")),
            show_terminal: true,
            terminal_height: Some(320.0),
            source_control_open: true,
        };

        let json = serde_json::to_string(&session).unwrap();
        let decoded: WorkspaceSession = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded, session);
    }

    #[test]
    fn resource_recovery_round_trips_and_rejects_invalid_or_duplicate_records() {
        use loog_workspace_api::{
            EnvironmentKey, ProviderPath, ResourceRef, session::ResourceBookmark,
        };
        let record = ResourceBookmark {
            resource: ResourceRef::Provided {
                environment: EnvironmentKey::new("example.provider", "test").unwrap(),
                path: ProviderPath::new("opaque:root").unwrap(),
            },
            name: "Workspace".into(),
            state: "{\"profile\":\"test\"}".into(),
        };
        let session = WorkspaceSession {
            resource_roots: vec![record.clone()],
            ..Default::default()
        };
        let wire = serde_json::to_value(&session).unwrap();
        assert_eq!(
            serde_json::from_value::<WorkspaceSession>(wire.clone()).unwrap(),
            session
        );
        let mut duplicate = wire.clone();
        duplicate["resource_roots"] = serde_json::json!([record.clone(), record]);
        assert!(serde_json::from_value::<WorkspaceSession>(duplicate).is_err());
        let mut invalid = wire.clone();
        invalid["resource_roots"][0]["state"] = serde_json::json!("x".repeat(16 * 1024 + 1));
        assert!(serde_json::from_value::<WorkspaceSession>(invalid).is_err());
        let mut transient = wire;
        transient["resource_roots"][0]["scope"] = serde_json::json!(1);
        assert!(serde_json::from_value::<WorkspaceSession>(transient).is_err());
    }

    #[test]
    fn missing_fields_from_older_sessions_use_empty_lists() {
        let decoded: WorkspaceSession = serde_json::from_str("{}").unwrap();

        assert_eq!(decoded, WorkspaceSession::default());
    }
}

#[cfg(test)]
mod recovery_writer_tests {
    use super::*;
    #[test]
    fn encoding_exit_flush_supersedes_queued_recovery_without_a_later_stale_write() {
        let output = std::sync::Arc::new(Mutex::new(Vec::new()));
        let seen = output.clone();
        let writer = start_layout_writer(move |layout| {
            seen.lock().unwrap().push(layout);
            Ok(())
        });
        let old = EditorLayout {
            root: crate::domain::workspace_session::SavedPaneNode::Pane {
                items: vec!["discarded.txt".into()],
                active: Some(0),
            },
            active_pane: 0,
        };
        writer
            .send(LayoutWrite {
                layout: Some(old),
                reply: None,
            })
            .unwrap();
        let (reply, done) = std::sync::mpsc::sync_channel(1);
        writer
            .send(LayoutWrite {
                layout: None,
                reply: Some(reply),
            })
            .unwrap();
        done.recv().unwrap().unwrap();
        assert_eq!(*output.lock().unwrap(), vec![None]);
    }
}
