# Git

Press `Ctrl+Shift+G` (or `Command+Shift+G`) to open Source Control. Loog discovers
repositories for every workspace root and direct nested repository, then keeps
branch, upstream, ahead/behind, staged, working-tree, untracked, rename and
conflict state refreshed in the background. The title bar, status bar, file
tree, editor gutter and Source Control drawer consume the same immutable
repository snapshot.

## Source Control

The Source Control drawer supports staging and unstaging, commit, fetch, pull
and push. The Git backend also provides typed operations for hunk patches,
branches, tags, history, blame, stash, worktrees, remotes, merge/rebase/
cherry-pick recovery, submodules, LFS and sparse checkout. Git operations run
outside the UI thread, are serialized per repository, classify common errors,
and redact credentials from diagnostic output.

Source Control has a file filter and repository summary above the changes.
Each file row shows its file-type icon and Git status. Check a file to stage it,
or uncheck its staged row to unstage it. File rows still open diffs and drag into
editor panes; the staging checkbox never starts a drag. Conflicts retain their
explicit resolution actions.

In the changes list, filenames and status letters are green for added or untracked
files, yellow/amber for modified files, and red for deleted files. Deleted filenames
also have a thin strike-through; folder paths and file-type icons keep their usual
appearance. These styles remain visible when a row is hovered or selected, and
describe that row's staged or working-tree change. Conflicts keep their explicit
status without a deletion strike-through.
The Explorer uses the same change colors for file and folder labels and file
status letters, with ignored paths remaining muted and conflicts distinct from
ordinary deletions.

The commit composer stays at the bottom for both short and long change lists.
It accepts multiline text: Enter inserts a newline and Ctrl+Enter (Command+Enter
on macOS) commits. Amend is available beside the message label; the split
button's menu offers **Commit and Push** plus the existing commit options.
Commit is disabled when the message or staged changes are missing, conflicts
remain, or Git is busy. Amend can reuse the previous message without staged
changes. A repository with no changes says **Working tree clean.**

## Git panel

Choose **Commits** in the bottom panel's **Terminal / Commits** switch to open
Git history below any editor or Settings page. Switching categories preserves
terminal sessions, output and the history's scroll position. History can also
open when no terminal sessions exist. `Ctrl+Shift+H` (or `Command+Shift+H`)
shows or hides the bottom panel with **Commits** selected. The panel shares its
height and collapse control with Terminal, without a second History tab bar.

To the right of the switch, the Commits header offers a **Branch / File** scope,
a search box and a commit count. **Branch** lists the commits of the chosen
branch; **File · name** lists only the commits that touched the file in the
active editor tab, following renames, and follows the tab as it changes (it is
unavailable when no file of the repository is open). The search box narrows the
loaded commits to those whose subject, author, email or hash contain every word
typed; `Esc` clears it. A narrowed or file-scoped list is drawn without graph
lanes. The count shows the commits listed, with a `+` while more pages remain.

When the panel is at least 900px wide, a navigator fills the left side.
An icon rail along its edge switches between
branches (Local and Remote folders), tags and stashes, each with its count; only
the chosen kind is listed. Choose a
branch or tag to show its commits; the current branch returns to HEAD. Choose a
stash to list its files in the detail pane, where **Apply**, **Pop** and **Drop**
live. Right-click a tag to push or delete it. A branch checked out in another
worktree shows a folder icon whose menu opens the worktree, adds it to the
workspace or deletes it. Narrower panels prioritize the commit list and show
the detail pane when space allows.

The detail pane puts the changed files first, with the commit or stash summary
below a divider at the bottom. The summary takes at most 30% of the pane and 120px;
its full text scrolls independently instead of being truncated. Both regions use
the Git panel's overlay scrollbars, shown on overflow while hovered or dragged.
Folders
that hold a single folder join into one row, fold shut when clicked, and stay
pinned at the top while their files scroll.
Filenames and status letters use the same change colors and deletion strike-through
as the Source Control drawer. The summary's insertion count is green as well.

Each group title has a **+** button: **BRANCHES** opens the branch picker, where a
new name creates a branch and **New Worktree** checks it out in its own folder;
**TAGS** tags the selected commit (HEAD when none is selected); **STASHES**
stashes the current changes. Branch names with `/` group into folders that fold
shut, and a remote's branches sit under its name.

Drag the panel's top edge to resize it. Collapsing and reopening keeps
the chosen height and category. Hovering a commit or tag shows its details card, and
the rows keep the same actions and context menus as before. History loads only
while **Commits** is visible.

## Diffs

Drag a changed file from Source Control onto an editor pane to open its
single-file diff, keeping the staged or working-tree comparison shown in the
list. Dropping near a pane edge creates a split; dropping on a tab strip inserts
the diff at the marker. Releasing outside the editor cancels the drag. Clicking
a changed file continues to open All Changes at that file's section.

## Worktrees

The branch picker groups worktrees beneath their checked-out branch. Branch rows
show the worktree count and expand or collapse the directory list. Each worktree
has its own path and actions; choosing one adds it to the workspace and selects
its repository. The existing folders stay available in the repository switcher.

In the branch picker, select an available branch and choose **New Worktree**.
Typing a new branch name also offers this action. The dialog shows the branch and
destination folder, defaulting to a sibling such as `../loog-feature-x`;
**Browse…** changes its parent folder. Creation adds the worktree to the workspace
and selects its repository.

The **Worktrees** tab, shown in narrow panels, lists each worktree with its branch,
current marker and lock status. Hover a row to open, add or delete it. Deletion asks for confirmation; modified or untracked files
require a separate force-delete confirmation. The main, current and locked
worktrees cannot be deleted from this section.

## Open buffers

Open editor buffers are reconciled after Git operations: clean files reload
automatically, while unsaved buffers are never overwritten and are marked when
their disk version changes.

## Git runtime

The Windows packages contain a managed Git runtime (MinGit) under
`runtime/git`, with a SHA-256 manifest that Loog verifies before using it.
Loog prefers that runtime and falls back to the `git` on PATH when no managed
runtime is present, which is always the case for the macOS and Linux packages.
Advanced users and development builds can select a custom runtime through
`GitRuntimeManager`.
