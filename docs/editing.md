# Editing

## Files and tabs

- UTF-8 text editing with grapheme-safe movement and deletion (including Chinese,
  combining marks and Emoji); LF and CRLF line endings are preserved.
- Click a file in the tree to open or reactivate its source tab. Views of a file
  share its text, undo/redo history and dirty state. Each view keeps its own
  selection and scroll position. Copying a tab creates a new view; moving a tab
  preserves the view and its position.
- Open UTF-8 text files up to 64 MiB from the Explorer, file dialog or launch
  arguments. Workspace search has a separate 4 MiB per-file limit.
- On Windows, drag files from another application into an editor pane or its
  tab strip to open them as permanent tabs in that pane. Multiple files open in
  order; existing tabs and unsaved edits are preserved. Folders are ignored.
- Interactive file opens prepare text and indexes on a background worker while
  retaining the current editor. A target tab reserves its icon slot; only opens
  taking more than 300 ms show a loading indicator. Close that tab to cancel;
  choosing another file or existing tab supersedes the pending open.

Saving writes a snapshot on a background worker, including encoding and disk
flushes. The unsaved marker clears only after a successful write; edits made
while saving remain unsaved. Repeated saves run in order. Save-and-close and
Vim `:wq` wait for completion, and a failed save keeps the file open.

## Sessions

- Restarting restores every file tab left open, including single-click previews
  and a pending file load, in its saved pane and tab order. Normal window close
  saves the final snapshot before exiting; Save/Don't Save exit decisions keep
  the tab list intact until that snapshot is written. Unreadable files are
  skipped on restore; unsaved text still uses the existing Save/Don't Save prompt.
- Startup restores tab metadata, order, panes and selection before reading any
  file contents. A background worker prepares the selected documents first;
  surviving tabs are filled in place without changing focus. Restoring tabs are
  remembered on exit and cannot edit or save their empty staging buffers.
- Sessions save typed view records in editor-layout schema 2, including each
  view's source position. Repeated paths share one restored text document and
  one background load. The previous unversioned format is read and migrated on
  the next save. An invalid or unsupported session is reported and preserved.

## Syntax highlighting

All supported languages, including Markdown and JSON, ship as independent
dynamic language packages: the editor loads their parsers and queries when
their content becomes visible. Opening Markdown does not load the languages of
off-screen code fences. Installed package updates take effect after restarting
Loog. Packages are discovered in application resources and the user data directory.
Plain text works without packages. See [Language packages](languages.md).

Colours come from a syntax tree built with Tree-sitter for Rust, JavaScript
(including JSX), TypeScript, TSX, C#, JSON, CSS, HTML and Markdown. Because the whole file is parsed, a
comment or string that starts above the screen is coloured correctly, and
unfinished code keeps its colours instead of turning plain.

The parse runs in the background and never blocks typing. While a new version
is still being parsed, the previous colours stay on screen, moved across the
edits you made, and are replaced as soon as the parser catches up. Switching to
another tab shows its colours at once.

Code embedded in another language is coloured by its own grammar: `<script>`
and `<style>` in HTML, and fenced code blocks in Markdown (```rust, ```js, and
so on; an unknown language keeps the Markdown colours). Embedding goes up to four
levels deep, and only the parts on screen are parsed, with at most 64 embedded
regions and 4 MiB of embedded text parsed at once.

Files larger than 8 MiB and file types without a grammar are drawn as plain text.
Which language a file has comes from its name, or from the
[file associations](settings.md#file-associations) in your settings.

## Large files

Large documents use plain text rendering and suppress automatic Vim match
highlighting. Long lines are rendered in horizontal viewport slices, without
truncating the file; sparse Unicode indexes keep cursor lookup local. Line
widths, dirty-state checks, syntax colors and text layouts are cached.

## Mouse and input

- Click to place the caret, drag to select, Shift-click to extend the selection,
  double-click to select a word, and triple-click or click a line number to select
  a line. Dragging line numbers selects whole lines. Holding a drag beyond the
  viewport scrolls continuously.
- Right-click for Undo, Redo, Cut, Copy, Paste and Select All. Clipboard failures
  leave the source text intact.
- Mouse wheel scrolls vertically; Shift-wheel scrolls horizontally.
- Chinese IME preedit text is displayed at the caret, with the native candidate
  window anchored there. Only committed input changes the document.

## Keyboard shortcuts

| Shortcut | Action |
| --- | --- |
| Arrow keys | Move by character or line; vertical movement remembers the desired column |
| Ctrl + Left / Right | Move by word |
| Home / End | Move to line start / end |
| Ctrl + Home / End | Move to document start / end |
| Page Up / Page Down | Move by one viewport |
| Shift + any movement above | Extend selection from its anchor |
| Ctrl + A | Select all |
| Ctrl + C / X / V | Copy / cut / paste |
| Ctrl + Z / Y | Undo / redo (Ctrl + Shift + Z also redoes) |
| Backspace / Delete | Delete the selection or previous / next character |
| Ctrl + Backspace / Delete | Delete the selection or previous / next word |
| Enter | Insert a newline with the current indentation |
| Tab / Shift + Tab | Indent / outdent; a selection applies to all selected lines |
| Esc | Close the editor menu, or clear the selection |
| Ctrl + S | Save the active file |

Indentation uses two spaces; existing tabs display at four-column tab stops.
Typing replaces the selection. Consecutive typing/deletion is grouped for undo;
movement, paste, indentation, saving and focus changes separate undo groups.
Editor shortcuts apply while the editor has focus; the terminal keeps its own
keyboard handling. On macOS, Command takes the place of Ctrl.

Every shortcut can be changed; see [Settings and key bindings](settings.md#key-bindings).

## File icons

The entire workspace tree uses a focused subset of the open-source Material
Icon Theme, including closed folders, open folders, and file types. Exact
filenames such as `Cargo.toml` and `Dockerfile` take priority over extensions;
compound extensions are matched longest-first, and unknown files use a generic
document icon. Attribution, source revision, and the upstream MIT license are
stored with the SVG assets under
[`assets/icon-themes/material/`](../assets/icon-themes/material/).

## Language servers

For languages with a language server (Rust uses rust-analyzer), Loog shows the
server's errors and warnings as underlines in the editor and their counts in the
status bar, and draws the server's semantic highlighting over the syntax colours
once it has analysed the current text. Nothing changes if the server is missing
or still starting: the syntax colours stay. See [settings](settings.md) to choose
another program or turn a server off.

Without a completion provider, document word suggestions use identifiers near
the caret. After a 30 ms typing pause, a background scan reads at most 256 KiB
and keeps up to 5,000 distinct identifiers of 3 or more characters and at most
128 UTF-8 bytes. The list is reused while typing the same word; identifiers
farther away or beyond these limits may not appear. Editing, moving to another
view, or closing the list invalidates pending results.

Click the server name in the status bar to open the language server panel: it
shows each running server, what it is busy with (for example indexing a large
project, during which errors may not appear yet), its recent messages, and a
button to restart the servers.

## Delimiter pairing

Typing an opening bracket wraps a selection or inserts a matching closer. Only
automatically inserted closers are skipped; Backspace inside an empty automatic
pair removes both characters. Undo and redo restore pair provenance. Selections
keep their direction when wrapped. Paste and IME composition commits stay literal.
Quotes pair in code languages, with conservative string/comment handling; Rust
single quotes remain literal for lifetimes. Plain text and Markdown pair brackets
only. When a lexical prefix exceeds 64 KiB, typing falls back to literal insertion
to bound work on large files.

## Line commands

Alt+Up/Down moves the current or selected lines. Shift+Alt+Up/Down duplicates
them above/below. Ctrl+Shift+K deletes lines; Ctrl+/ toggles line comments using
the language package's line marker (Cmd replaces Ctrl on macOS). Mixed comments
are all commented; toggle again to restore them. Blank lines in a selection
are skipped. Selections retain their direction and exclude a final line whose
start is the selection endpoint. Each command is one undo step.

The actions editor::MoveLineUp/Down, editor::DuplicateLineUp/Down,
editor::DeleteLine and editor::ToggleComment can be rebound in keymap.json.
They apply in source Editor contexts, including explicit bindings in Vim;
terminal, preview and text-diff views keep their own contexts.

## Reading aids

Settings → Editor provides Matching brackets, Indent guides and Visible
whitespace. They can also be configured as editor.match_brackets,
editor.indent_guides and editor.show_whitespace, including project and language
overrides. Guides use the actual indentation and tab display widths; whitespace
markers show spaces, tabs and line endings without editing the text. Matching
uses a cached lexical index with conservative comment/string handling. It skips
files larger than 128 KiB or with more than 32,768 matched pairs so large-file
viewing stays bounded. Incomplete or crossed delimiters remain unhighlighted.

## Soft wrap

Alt+Z toggles soft wrap for the current document. Settings → Editor → Soft wrap
and editor.soft_wrap choose the default; project and language overrides also
apply. Wrapping never inserts line breaks. Arrows, Home/End, selection, mouse
hits, scrolling, source cards and IME use the same displayed-row map. At a wrap
boundary the caret can remain at the previous row's end or the next row's start.
Vim's logical-line motions retain their normal meaning.

Documents over 1 MiB or 100,000 displayed rows use the existing horizontal-scroll
view to bound reflow memory and work. The status preference is retained. Display
geometry caches retain immutable source snapshots and are bounded per UI thread.

## Folding

Click a gutter triangle to fold a function, class or code block; click it again
or the inline folded-lines label to expand. Ctrl+Shift+[ / ] folds/unfolds the
current region; Ctrl+K Ctrl+0 folds all, Ctrl+K Ctrl+J unfolds all (Cmd on macOS).
These editor::Fold/Unfold/FoldAll/UnfoldAll actions can be rebound.

Ranges come from the current Tree-sitter tree, with a debounced LSP fallback.
Every result is checked against document identity, incarnation, text version
and language generation. Folding is independent in split views. Edits outside
a folded region map its boundaries; edits inside it or missing revision history
expand it. Search and jumps expand hidden targets. Vertical navigation skips
hidden lines; entering one through a logical motion expands it. Folding uses
the same display map as soft wrap, including hit testing and scrolling.

Folding uses the same 1 MiB/100,000-row large-file fallback as wrapping. Ranges
are bounded and crossing server ranges are rejected. Fold state is temporary
for the open views and does not change the file or create text undo entries.

## Multiple cursors

Ctrl+D (Cmd+D on macOS) selects the word at the caret, then adds the next
matching occurrence, wrapping at the end of the document. An existing custom
selection searches for that exact text. Ctrl+Alt+Up/Down adds a cursor on the
previous/next displayed row, including wrapped rows. These actions can be rebound
as editor::SelectNextOccurrence, editor::AddCursorAbove and editor::AddCursorBelow.

Alt+click adds or removes a cursor; Shift+Alt+drag selects a rectangle across
displayed rows. Tabs, wide characters and grapheme clusters use the same geometry
as painting. Rectangles stop at line ends and skip folded lines. Up to 256 cursors
are supported per view. A plain click or Escape returns to the primary cursor.
Overlapping selections merge; adjacent nonempty selections remain independent.
Selections are temporary and are not restored from a saved session.

Typing, delimiter pairs, Enter and deletion apply to all selections as one atomic
edit. Indentation, comments and line movement handle each touched line once.
Undo/redo restores every selection. Each split view keeps its own selections while
following edits to the shared text. Copy joins nonempty selections in source order;
Cut leaves empty carets intact. Paste distributes lines when their count exactly
matches the number of cursors; otherwise it inserts the full text at each cursor,
using the document's line endings.

IME preedit is shown at the primary caret; committing inserts the literal text at
all selections. Automatic completion and on-type formatting pause during multiple
cursor editing. Manually requesting completion keeps only the primary cursor, so
completion edits and snippet tab stops have an explicit target. Adding a cursor
ends a snippet session and cancels its pending completion list. Saving/formatting
maps all selections through the resulting document edits. Vim's normal-mode
bindings retain their meaning. Explicit extra-cursor commands enter Insert mode
when Vim is enabled.

## Vim and editor enhancements

Vim uses the same editor preferences and editing rules. Insert mode supports
automatic delimiter pairs, generated-closer skipping, pair deletion and wrapping
an IDE selection. Composition commits and pasted text stay literal, including
when replayed with a count, a macro or the dot command. Replace mode overwrites
text with its usual backspace restoration.

| Feature | Vim behavior |
| --- | --- |
| Indentation | Enter, Tab, Shift+Tab, o/O and >>/<< use the configured indentation and language rules |
| Line endings | Newlines and register pastes use LF/CRLF preferences; status-bar conversion and :w use the same buffer and encoding |
| Line operations | Alt+Up/Down, Shift+Alt+Up/Down, delete-line and comment shortcuts work in Normal, Visual and Insert modes; Visual ranges include their last selected row |
| Reading aids | Bracket matching, indentation guides and whitespace display use the same settings in every mode |
| Soft wrap | Alt+Z toggles wrapping; j/k retain logical-line movement and gj/gk move by displayed rows; page and screen motions use displayed geometry |
| Folding | zc/zo close/open a region and zM/zR close/open all regions; j/k skip folded lines and jumps reveal hidden targets |
| Multiple cursors | Ctrl+Alt+Up/Down, Alt+click and Shift+Alt+drag enter Insert mode; Ctrl/Cmd+D selects occurrences there; Visual Block I/A edits its eligible lines immediately |
| Language services | Completion and snippet tab stops work in Insert mode; manual completion enters it; Escape ends the snippet and cancels outstanding completion when leaving Insert |

Normal-mode Ctrl+D keeps Vim's half-page movement. An explicit IDE line command
is an independent undo step, and subsequent typing starts a fresh insert group.
Escape ends a multiple-cursor insert at its primary cursor. Save formatting maps
all selections; undo during Insert restores them and subsequent typing remains
repeatable. Core editing and display features also work without a language server.

Server-provided annotations refresh when a provider becomes ready, registers new
capabilities or restarts, even when the text and caret stay unchanged.
