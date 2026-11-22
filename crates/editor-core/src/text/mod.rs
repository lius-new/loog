//! The editing core: document text, queries, edits, selections and history.
//!
//! * `buffer`    — the text, its line index and the queries behaviours use.
//! * `change`    — requested edits and the change sets they produce.
//! * `editor`    — `EditorMut`, the single entry that changes a document.
//! * `history`   — undo steps, typing runs and explicit groups.
//! * `lines`     — line starts and display widths kept across edits.
//! * `selection` — one view's anchor and active end.
//!
//! Behaviours (default editing, Vim) decide what a key means and express it
//! as edits and selections here; nothing else touches the text.

mod brackets;
mod buffer;
mod display;
pub use self::display::{DisplayMap, DisplayRow};
mod change;
mod editor;
mod history;
mod line_tree;
mod newline;
mod pieces;
mod source;
pub use self::pieces::PieceText;
pub use self::source::{FileIndex, IndexBuilder, MemorySource, SharedSource, TextSource};
mod lines;
mod options;
mod pairs;
pub use self::options::{ENDING_MASK, EditingOptions, FileOptions, LineEnding, normalize_endings};
pub(crate) mod multiple;
mod revision;
mod selection;
pub use self::revision::{BytePoint, RevisionEdit, TextExtent, TextRevision};

pub use buffer::TextBuffer;
pub use change::{Assoc, Change, ChangeSet, Edit, EditError};
pub use editor::{Editor, EditorMut};
pub use history::EditKind;
pub use lines::{display_width, grapheme_width};
pub use selection::Selection;
