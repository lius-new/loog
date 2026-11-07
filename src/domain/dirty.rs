//! Exact saved-text equality with a bounded edit-journal proof. No hashes or
//! strong source references are needed for the steady-state typing path.
use loog_editor_core::text::TextBuffer;
use std::{
    ops::Range,
    sync::{Arc, Weak},
};

const COMPARE_BUDGET: usize = 256 * 1024;

#[derive(Clone)]
struct Span {
    saved: Range<usize>,
    current: Range<usize>,
}

pub(crate) struct DirtyCache {
    version: u64,
    source: Weak<String>,
    saved: Weak<String>,
    dirty: bool,
    /// Everything outside these corresponding spans is known to be equal.
    span: Option<Span>,
    witness: Option<usize>,
}

impl DirtyCache {
    /// The caller has established that this buffer equals the saved text.
    pub fn clean(buffer: &TextBuffer, saved: &Arc<String>) -> Self {
        Self {
            version: buffer.version(),
            source: Arc::downgrade(&buffer.text_snapshot()),
            saved: Arc::downgrade(saved),
            dirty: false,
            span: None,
            witness: None,
        }
    }

    pub fn is_dirty(&mut self, buffer: &TextBuffer, saved: &Arc<String>) -> bool {
        let text = buffer.text_snapshot();
        let source = Arc::downgrade(&text);
        let baseline = Arc::downgrade(saved);
        let same_saved = Weak::ptr_eq(&self.saved, &baseline);
        if same_saved && self.version == buffer.version() && Weak::ptr_eq(&self.source, &source) {
            return self.dirty;
        }
        let mut proof = None;
        if same_saved
            && (!self.dirty || self.span.is_some())
            && let Some(revisions) =
                buffer.revisions_since(self.version, self.source.as_ptr() as usize)
        {
            let mut span = self.span.clone();
            let mut bounded = true;
            for revision in revisions {
                let (Some(first), Some(last)) = (revision.edits.first(), revision.edits.last())
                else {
                    continue;
                };
                let mut updated = span.take().unwrap_or(Span {
                    saved: first.old.start..last.old.end,
                    current: first.old.start..last.old.end,
                });
                let left = updated.current.start.min(first.old.start);
                let right = updated.current.end.max(last.old.end);
                if left < updated.current.start {
                    updated.saved.start = left;
                }
                if right > updated.current.end {
                    updated.saved.end += right - updated.current.end;
                }
                updated.current = revision.map(left, false)..revision.map(right, true);
                if updated.saved.len().saturating_add(updated.current.len()) > COMPARE_BUDGET {
                    bounded = false;
                    break;
                }
                span = Some(updated);
            }
            if bounded {
                proof = Some(span);
            }
        }
        let mut span = None;
        let mut witness = None;
        let dirty = if Arc::ptr_eq(&text, saved) || matches!(proof, Some(None)) {
            false
        } else if let Some(Some(compared)) = proof.filter(|p| {
            p.as_ref()
                .is_some_and(|s| s.saved.end <= saved.len() && s.current.end <= text.len())
        }) {
            let old = &saved.as_bytes()[compared.saved.clone()];
            let new = &text.as_bytes()[compared.current.clone()];
            let dirty = if old.len() != new.len() {
                true
            } else {
                witness = difference(old, new).map(|at| compared.current.start + at);
                witness.is_some()
            };
            if dirty {
                span = Some(compared);
            }
            dirty
        } else if text.len() != saved.len() {
            true
        } else if let Some(at) = self.witness.filter(|&at| {
            text.as_bytes()
                .get(at)
                .zip(saved.as_bytes().get(at))
                .is_some_and(|(a, b)| a != b)
        }) {
            witness = Some(at);
            true
        } else {
            witness = difference(text.as_bytes(), saved.as_bytes());
            witness.is_some()
        };
        self.version = buffer.version();
        self.source = source;
        self.saved = baseline;
        self.dirty = dirty;
        self.span = span;
        self.witness = witness;
        dirty
    }
}

/// Chunk comparisons use the standard byte equality implementation. Once a
/// differing chunk is found, only that small chunk needs a byte-by-byte search.
fn difference(a: &[u8], b: &[u8]) -> Option<usize> {
    debug_assert_eq!(a.len(), b.len());
    for (i, (a, b)) in a.chunks(4096).zip(b.chunks(4096)).enumerate() {
        if a != b {
            return a
                .iter()
                .zip(b)
                .position(|(a, b)| a != b)
                .map(|at| i * 4096 + at);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use crate::domain::dirty::DirtyCache;
    use loog_editor_core::text::TextBuffer;
    use loog_editor_core::text::{EditKind, EditorMut, Selection};
    use std::{ops::Range, sync::Arc};
    fn edit(buffer: &mut TextBuffer, range: Range<usize>, text: &str) {
        let mut selection = Selection::caret(0);
        EditorMut::new(buffer, &mut selection, vec![])
            .replace(range, text, EditKind::Other)
            .unwrap();
    }

    #[test]
    fn late_replacements_and_insert_delete_round_trips_keep_bounded_proofs() {
        let mut buffer = TextBuffer::new("a".repeat(1024 * 1024));
        let saved = buffer.text_snapshot();
        let mut cache = DirtyCache::clean(&buffer, &saved);
        let n = buffer.len();
        edit(&mut buffer, n - 1..n, "b");
        assert!(cache.is_dirty(&buffer, &saved));
        assert_eq!(cache.span.as_ref().unwrap().current.len(), 1);
        edit(&mut buffer, n - 1..n, "a");
        assert!(!cache.is_dirty(&buffer, &saved));
        edit(&mut buffer, n..n, "中");
        assert!(cache.is_dirty(&buffer, &saved));
        assert_eq!(cache.span.as_ref().unwrap().current.len(), 3);
        edit(&mut buffer, n..n + 3, "");
        assert!(!cache.is_dirty(&buffer, &saved));
    }

    #[test]
    fn independent_branches_and_replaced_baselines_cannot_reuse_stale_answers() {
        let mut a = TextBuffer::new("one two");
        let mut saved = a.text_snapshot();
        let mut cache = DirtyCache::clean(&a, &saved);
        let mut b = a.clone();
        edit(&mut a, 0..3, "one");
        edit(&mut b, 4..7, "end");
        assert!(!cache.is_dirty(&a, &saved));
        assert!(cache.is_dirty(&b, &saved));
        assert!(!cache.is_dirty(&a, &saved));
        saved = Arc::new("different".into());
        assert!(cache.is_dirty(&a, &saved));
        saved = Arc::new("one two".into());
        assert!(!cache.is_dirty(&a, &saved));
    }

    #[test]
    fn random_edits_undo_redo_and_journal_eviction_match_exact_text_equality() {
        let mut buffer = TextBuffer::new("中a\te\u{301}🇨🇳\r\n".repeat(40));
        let saved = buffer.text_snapshot();
        let mut cache = DirtyCache::clean(&buffer, &saved);
        let mut seed = 29u64;
        for i in 0..600 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let mut at = seed as usize % (buffer.len() + 1);
            while !buffer.text().is_char_boundary(at) {
                at -= 1;
            }
            let mut end = (at + (seed >> 32) as usize % 12).min(buffer.len());
            while !buffer.text().is_char_boundary(end) {
                end -= 1;
            }
            edit(
                &mut buffer,
                at..end,
                ["", "x", "中", "\r\n", "e\u{301}"][i % 5],
            );
            if i % 7 != 0 {
                assert_eq!(
                    cache.is_dirty(&buffer, &saved),
                    buffer.text() != saved.as_str()
                );
            }
        }
        let mut selection = Selection::caret(0);
        for _ in 0..500 {
            EditorMut::new(&mut buffer, &mut selection, vec![]).undo();
            assert_eq!(
                cache.is_dirty(&buffer, &saved),
                buffer.text() != saved.as_str()
            );
        }
        for _ in 0..100 {
            EditorMut::new(&mut buffer, &mut selection, vec![]).redo();
            assert_eq!(
                cache.is_dirty(&buffer, &saved),
                buffer.text() != saved.as_str()
            );
        }
        for _ in 0..80 {
            edit(&mut buffer, 0..0, "x");
        }
        assert_eq!(
            cache.is_dirty(&buffer, &saved),
            buffer.text() != saved.as_str()
        );
        buffer.reload(saved.as_str().into());
        assert!(!cache.is_dirty(&buffer, &saved));
    }

    #[test]
    fn cache_retains_no_strong_source_or_saved_references() {
        let buffer = TextBuffer::new("base");
        let saved = buffer.text_snapshot();
        let owners = Arc::strong_count(&saved);
        let mut cache = DirtyCache::clean(&buffer, &saved);
        assert_eq!(Arc::strong_count(&saved), owners);
        assert!(!cache.is_dirty(&buffer, &saved));
        assert_eq!(Arc::strong_count(&saved), owners);
    }

    #[test]
    fn undo_and_redo_restore_the_exact_saved_state_after_variable_length_edits() {
        let mut buffer = TextBuffer::new("a".repeat(4096));
        let saved = buffer.text_snapshot();
        let mut cache = DirtyCache::clean(&buffer, &saved);
        let mut selection = Selection::caret(0);
        for (range, text) in [(4000..4000, "中"), (5..7, "e\u{301}"), (100..120, "")] {
            EditorMut::new(&mut buffer, &mut selection, vec![])
                .replace(range, text, EditKind::Other)
                .unwrap();
            assert!(cache.is_dirty(&buffer, &saved));
        }
        for i in 0..3 {
            EditorMut::new(&mut buffer, &mut selection, vec![]).undo();
            assert_eq!(cache.is_dirty(&buffer, &saved), i != 2);
        }
        for _ in 0..3 {
            EditorMut::new(&mut buffer, &mut selection, vec![]).redo();
            assert!(cache.is_dirty(&buffer, &saved));
        }
    }
}
