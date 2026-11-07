//! Loads and applies inline Git hunk previews outside the view.
use crate::app::context::AppContext;
use crate::domain::pane_layout::PaneId;
use crate::features::git::DiffTarget;
use crate::features::git::diff_document::DiffDocument;
use crate::ui::git::hunk_peek::{Hunk, HunkPeek, block_lines, hunk_at};
use std::path::{Path, PathBuf};
use std::thread;
/// Read the unstaged change at `line`, or else the staged one.
fn load(
    root: &Path,
    relative: &Path,
    line: usize,
) -> crate::features::git::GitResult<Option<Hunk>> {
    let service = crate::infrastructure::git::service()?;
    for target in [DiffTarget::IndexToWorktree, DiffTarget::HeadToIndex] {
        let diff = service.backend().parsed_full_diff(
            root,
            target,
            std::slice::from_ref(&relative.to_path_buf()),
        )?;
        let document =
            DiffDocument::from_unified(root.to_path_buf(), relative.to_path_buf(), target, diff);
        if let Some((block, rows)) = hunk_at(&document, line) {
            return Ok(Some(Hunk {
                root: root.to_path_buf(),
                relative: relative.to_path_buf(),
                target,
                block,
                rows,
                lines: block_lines(&block),
            }));
        }
    }
    Ok(None)
}

/// Open the peek of the change at `line` in `pane`, or close it when it
/// already shows that change.
pub fn toggle(
    state: &AppContext,
    pane: PaneId,
    path: PathBuf,
    root: PathBuf,
    relative: PathBuf,
    line: usize,
) {
    let mut open = true;
    state.update(|app| {
        if let Some(peek) = &app.editor.editor_hunk_peek
            && peek.pane == pane
            && peek.path == path
            && (peek.line == line
                || matches!(&peek.content, Some(Ok(Some(hunk))) if hunk.lines.contains(&line)))
        {
            app.editor.editor_hunk_peek = None;
            open = false;
        }
    });
    if open {
        open_at(state, pane, path, root, relative, line);
    }
}

pub(crate) fn open_at(
    state: &AppContext,
    pane: PaneId,
    path: PathBuf,
    root: PathBuf,
    relative: PathBuf,
    line: usize,
) {
    let Some(application) = state.services().and_then(|runtime| runtime.application()) else {
        return;
    };
    let peek = HunkPeek {
        pane,
        path,
        line,
        content: None,
    };
    let pending = peek.clone();
    state.update(move |app| app.editor.editor_hunk_peek = Some(pending));
    let worker_state = state.clone();
    let _ = thread::Builder::new()
        .name("loog-git-hunk-peek".into())
        .spawn(move || {
            let result = load(&root, &relative, line).map_err(|error| error.user_message());
            application.post(move || {
                worker_state.update(move |app| {
                    // Keep only the answer to the peek still open.
                    if let Some(current) = app.editor.editor_hunk_peek.as_mut()
                        && current.pane == peek.pane
                        && current.path == peek.path
                        && current.line == peek.line
                        && current.content.is_none()
                    {
                        current.content = Some(result);
                    }
                });
            });
        });
}
