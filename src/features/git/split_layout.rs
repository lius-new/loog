//! Compact side-by-side rows and their mapping to the shared diff scroll range.
use super::diff_document::{ChangesLayout, DiffDocument, SplitDiffRow};
use super::diff_selection::DiffSide;
use std::ops::Range;

pub fn side_index(side: DiffSide) -> usize {
    match side {
        DiffSide::Old => 0,
        DiffSide::New => 1,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Projection {
    rows: [Vec<usize>; 2],
    prefix: [Vec<usize>; 2],
    pub changes: Vec<Range<usize>>,
}

impl Projection {
    pub fn new(rows: &[SplitDiffRow]) -> Self {
        let mut out = Self {
            rows: Default::default(),
            prefix: [vec![0], vec![0]],
            changes: Vec::new(),
        };
        for (index, row) in rows.iter().enumerate() {
            for (side, present) in [row.old_text.is_some(), row.new_text.is_some()]
                .into_iter()
                .enumerate()
            {
                if present || row.hunk.is_some() {
                    out.rows[side].push(index);
                }
                out.prefix[side].push(out.rows[side].len());
            }
            if row.changed {
                if let Some(last) = out.changes.last_mut().filter(|last| last.end == index) {
                    last.end += 1;
                } else {
                    out.changes.push(index..index + 1);
                }
            }
        }
        out
    }

    pub fn rows(&self, side: DiffSide) -> &[usize] {
        &self.rows[side_index(side)]
    }

    /// Fractional shared rows map continuously, including a side's empty ranges.
    pub fn position(&self, side: DiffSide, row: f32) -> f32 {
        let prefix = &self.prefix[side_index(side)];
        let end = prefix.len() - 1;
        let row = row.clamp(0.0, end as f32);
        let index = (row.floor() as usize).min(end);
        let next = (index + 1).min(end);
        prefix[index] as f32 + (prefix[next] - prefix[index]) as f32 * row.fract()
    }
}

pub struct Section<'a> {
    pub index: usize,
    pub document: &'a DiffDocument,
    pub shared: Range<usize>,
    pub shared_body: usize,
    pub top: [usize; 2],
    pub body: [usize; 2],
    pub end: [usize; 2],
    pub closed: bool,
    pub error: Option<&'a str>,
}

pub struct Layout<'a> {
    pub sections: Vec<Section<'a>>,
    pub shared_rows: usize,
    pub rows: [usize; 2],
}

impl<'a> Layout<'a> {
    pub fn new(document: &'a DiffDocument, overview: Option<&ChangesLayout>) -> Self {
        let mut out = Self {
            sections: Vec::new(),
            shared_rows: 0,
            rows: [0; 2],
        };
        if let (Some(files), Some(overview)) = (&document.sections, overview) {
            for item in &overview.sections {
                let doc = &files[item.index].document;
                let top = out.rows;
                let body = top.map(|row| row + 2);
                let closed = item.body_rows == 0;
                let end = std::array::from_fn(|side| {
                    body[side]
                        + if closed {
                            0
                        } else if doc.split_rows.is_empty() {
                            1
                        } else {
                            doc.split_projection().rows[side].len()
                        }
                });
                out.sections.push(Section {
                    index: item.index,
                    document: doc,
                    shared: item.top..item.body_top + item.body_rows,
                    shared_body: item.body_top,
                    top,
                    body,
                    end,
                    closed,
                    error: files[item.index].error.as_deref(),
                });
                out.rows = end;
            }
            out.shared_rows = overview.row_count;
        } else {
            let rows = std::array::from_fn(|side| document.split_projection().rows[side].len());
            out.shared_rows = document.split_rows.len();
            out.rows = rows;
            out.sections.push(Section {
                index: 0,
                document,
                shared: 0..out.shared_rows,
                shared_body: 0,
                top: [0; 2],
                body: [0; 2],
                end: rows,
                closed: false,
                error: None,
            });
        }
        out
    }

    fn position(&self, side: DiffSide, shared: f32) -> f32 {
        let index = side_index(side);
        let section = self
            .sections
            .partition_point(|item| item.shared.end as f32 <= shared);
        let Some(item) = self.sections.get(section) else {
            return self.rows[index] as f32;
        };
        if shared < item.shared_body as f32 {
            return item.top[index] as f32 + (shared - item.shared.start as f32).max(0.0);
        }
        let local = shared - item.shared_body as f32;
        item.body[index] as f32
            + if item.document.split_rows.is_empty() {
                local.min(1.0)
            } else {
                item.document.split_projection().position(side, local)
            }
    }

    /// Keep corresponding content near the viewport's upper third in phase.
    /// The stored scroll range stays shared so scrollbar dragging and refreshes
    /// use the same coordinate system as change navigation.
    pub fn offset(&self, side: DiffSide, scroll: f32, viewport: f32) -> f32 {
        let anchor = viewport / 3.0;
        let max_shared = (self.shared_rows as f32 - viewport).max(0.0);
        let max_side = (self.rows[side_index(side)] as f32 - viewport).max(0.0);
        (self.position(side, scroll.clamp(0.0, max_shared) + anchor) - anchor).clamp(0.0, max_side)
    }

    /// Resolve a compact side row back into the selection model's shared row.
    pub fn locate(&self, side: DiffSide, row: usize) -> Option<(usize, usize, bool)> {
        let index = side_index(side);
        let mut last = None;
        for item in &self.sections {
            if item.closed {
                continue;
            }
            let rows = item.document.split_projection().rows(side);
            let Some(&first) = rows.first() else {
                continue;
            };
            if row < item.body[index] {
                return Some((item.index, first, false));
            }
            if row < item.end[index] {
                return Some((item.index, rows[row - item.body[index]], true));
            }
            last = Some((item.index, *rows.last().unwrap(), false));
        }
        last
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::git::{DiffTarget, diff::parse_unified};

    fn document() -> DiffDocument {
        DiffDocument::from_unified("repo".into(), "a.txt".into(), DiffTarget::IndexToWorktree,
            parse_unified("diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,3 +1,5 @@\n start\n-old\n+new\n+extra\n+more\n end\n").unwrap())
    }

    #[test]
    fn compact_rows_keep_the_original_selection_indices() {
        let doc = document();
        assert_eq!(doc.split_projection().rows(DiffSide::Old), [0, 1, 4]);
        assert_eq!(doc.split_projection().rows(DiffSide::New), [0, 1, 2, 3, 4]);
        let layout = Layout::new(&doc, None);
        assert_eq!(layout.locate(DiffSide::Old, 2), Some((0, 4, true)));
        assert_eq!(layout.locate(DiffSide::New, 3), Some((0, 3, true)));
    }

    #[test]
    fn scroll_mapping_is_continuous_and_reaches_both_ends() {
        let doc = document();
        let layout = Layout::new(&doc, None);
        assert_eq!(layout.offset(DiffSide::Old, 0.0, 1.0), 0.0);
        assert_eq!(layout.offset(DiffSide::New, 0.0, 1.0), 0.0);
        assert_eq!(layout.offset(DiffSide::Old, 4.0, 1.0), 2.0);
        assert_eq!(layout.offset(DiffSide::New, 4.0, 1.0), 4.0);
        let left = layout.offset(DiffSide::Old, 2.5, 1.0);
        let right = layout.offset(DiffSide::New, 2.5, 1.0);
        assert!(right > left);
        assert!((layout.offset(DiffSide::New, 2.51, 1.0) - right).abs() < 0.02);
    }
}
