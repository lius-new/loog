//! Persistent blocks of line metadata. Byte offsets are subtree sums, so an
//! edit never shifts every later line or copies an unchanged snapshot's blocks.
use super::lines::Position;
use std::{ops::Range, sync::Arc};

pub(super) const BLOCK: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Line {
    pub bytes: usize,
    pub width: u32,
    pub crlf: bool,
    pub positions: Option<Arc<Vec<Position>>>,
}
impl Line {
    pub fn positions(&self) -> &[Position] {
        self.positions.as_deref().map_or(&[], Vec::as_slice)
    }
}

#[derive(Clone, Debug)]
enum Kind {
    Leaf {
        lines: Vec<Line>,
        starts: Vec<usize>,
    },
    /// A leaf of ordinary short lines keeps only its totals. Its lines are read back from the
    /// text when needed, so a document costs a few bytes per block rather than per line.
    Packed,
    Branch(Arc<Node>, Arc<Node>),
}

/// A leaf found by row or byte, with where it starts. `data` is `None` for a packed leaf.
pub(super) struct Located<'a> {
    pub base_row: usize,
    pub base_byte: usize,
    pub rows: usize,
    pub bytes: usize,
    pub data: Option<(&'a [Line], &'a [usize])>,
}

const NEEDS_TEXT: &str = "a packed block was read without the text it was packed from";
#[derive(Clone, Debug)]
struct Node {
    rows: usize,
    bytes: usize,
    width: u32,
    crlf: usize,
    height: usize,
    kind: Kind,
}
type Link = Option<Arc<Node>>;

fn leaf(lines: Vec<Line>) -> Arc<Node> {
    let mut bytes = 0;
    let starts = lines
        .iter()
        .map(|line| {
            let at = bytes;
            bytes += line.bytes;
            at
        })
        .collect();
    Arc::new(Node {
        rows: lines.len(),
        bytes,
        width: lines.iter().map(|l| l.width).max().unwrap_or(0),
        crlf: lines.iter().filter(|l| l.crlf).count(),
        height: 1,
        kind: Kind::Leaf { lines, starts },
    })
}
/// The leaf without its lines, unless a line carries checkpoints that cannot be recomputed cheaply.
fn pack(node: &Arc<Node>) -> Arc<Node> {
    match &node.kind {
        Kind::Leaf { lines, .. } if lines.iter().all(|line| line.positions.is_none()) => {
            Arc::new(Node {
                rows: node.rows,
                bytes: node.bytes,
                width: node.width,
                crlf: node.crlf,
                height: 1,
                kind: Kind::Packed,
            })
        }
        _ => node.clone(),
    }
}
fn branch(left: Arc<Node>, right: Arc<Node>) -> Arc<Node> {
    Arc::new(Node {
        rows: left.rows + right.rows,
        bytes: left.bytes + right.bytes,
        width: left.width.max(right.width),
        crlf: left.crlf + right.crlf,
        height: 1 + left.height.max(right.height),
        kind: Kind::Branch(left, right),
    })
}
fn balanced(left: Arc<Node>, right: Arc<Node>) -> Arc<Node> {
    if left.height > right.height + 1 {
        let Kind::Branch(a, b) = &left.kind else {
            unreachable!()
        };
        if a.height >= b.height {
            return branch(a.clone(), branch(b.clone(), right));
        }
        let Kind::Branch(b1, b2) = &b.kind else {
            unreachable!()
        };
        return branch(branch(a.clone(), b1.clone()), branch(b2.clone(), right));
    }
    if right.height > left.height + 1 {
        let Kind::Branch(a, b) = &right.kind else {
            unreachable!()
        };
        if b.height >= a.height {
            return branch(branch(left, a.clone()), b.clone());
        }
        let Kind::Branch(a1, a2) = &a.kind else {
            unreachable!()
        };
        return branch(branch(left, a1.clone()), branch(a2.clone(), b.clone()));
    }
    branch(left, right)
}
fn join(left: Link, right: Link) -> Link {
    let (Some(left), Some(right)) = (&left, &right) else {
        return left.or(right);
    };
    if left.height > right.height + 1 {
        let Kind::Branch(a, b) = &left.kind else {
            unreachable!()
        };
        return Some(balanced(
            a.clone(),
            join(Some(b.clone()), Some(right.clone())).unwrap(),
        ));
    }
    if right.height > left.height + 1 {
        let Kind::Branch(a, b) = &right.kind else {
            unreachable!()
        };
        return Some(balanced(
            join(Some(left.clone()), Some(a.clone())).unwrap(),
            b.clone(),
        ));
    }
    Some(branch(left.clone(), right.clone()))
}
fn split(root: Link, row: usize) -> (Link, Link) {
    let Some(node) = root else {
        return (None, None);
    };
    if row == 0 {
        return (None, Some(node));
    }
    if row == node.rows {
        return (Some(node), None);
    }
    match &node.kind {
        Kind::Leaf { lines, .. } => (
            Some(leaf(lines[..row].to_vec())),
            Some(leaf(lines[row..].to_vec())),
        ),
        // Callers split at block boundaries only, which never reach inside a leaf.
        Kind::Packed => unreachable!("{NEEDS_TEXT}"),
        Kind::Branch(left, right) => {
            if row < left.rows {
                let (a, b) = split(Some(left.clone()), row);
                (a, join(b, Some(right.clone())))
            } else {
                let (a, b) = split(Some(right.clone()), row - left.rows);
                (join(Some(left.clone()), a), b)
            }
        }
    }
}

fn located<'a>(node: &'a Node, kind: &'a Kind, base_row: usize, base_byte: usize) -> Located<'a> {
    Located {
        base_row,
        base_byte,
        rows: node.rows,
        bytes: node.bytes,
        data: match kind {
            Kind::Leaf { lines, starts } => Some((lines.as_slice(), starts.as_slice())),
            _ => None,
        },
    }
}
fn build(nodes: &[Arc<Node>]) -> Link {
    match nodes.len() {
        0 => None,
        1 => Some(nodes[0].clone()),
        n => Some(branch(
            build(&nodes[..n / 2]).unwrap(),
            build(&nodes[n / 2..]).unwrap(),
        )),
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct LineTree {
    root: Link,
}
impl PartialEq for LineTree {
    fn eq(&self, other: &Self) -> bool {
        self.iter().eq(other.iter())
    }
}
impl Eq for LineTree {}
impl LineTree {
    pub fn from_lines(lines: impl IntoIterator<Item = Line>) -> Self {
        let lines: Vec<_> = lines.into_iter().collect();
        let count = lines.len().div_ceil(BLOCK);
        let mut remaining = lines.len();
        let mut lines = lines.into_iter();
        let mut leaves = Vec::new();
        // Distribute rows evenly rather than leaving a tiny last block at an
        // internal splice boundary. Every multi-block rebuild is at least half full.
        for i in 0..count {
            let size = remaining.div_ceil(count - i);
            let block: Vec<_> = lines.by_ref().take(size).collect();
            remaining -= size;
            leaves.push(leaf(block));
        }
        Self {
            root: build(&leaves),
        }
    }
    /// `count` blocks made one at a time and packed at once, so a large text never holds its
    /// per-line data all at the same time. `block(i)` gives the lines of block `i`.
    pub fn from_packed_blocks(count: usize, mut block: impl FnMut(usize) -> Vec<Line>) -> Self {
        let leaves: Vec<_> = (0..count).map(|i| pack(&leaf(block(i)))).collect();
        Self {
            root: build(&leaves),
        }
    }
    /// Pack the leaves that touch `rows`, after edits left them with their lines in memory.
    pub fn pack_range(&mut self, rows: Range<usize>) {
        fn visit(node: &mut Arc<Node>, base: usize, rows: &Range<usize>) {
            if base + node.rows <= rows.start || base >= rows.end {
                return;
            }
            match &node.kind {
                Kind::Leaf { .. } => *node = pack(node),
                Kind::Packed => {}
                Kind::Branch(..) => {
                    let Kind::Branch(left, right) = &mut Arc::make_mut(node).kind else {
                        unreachable!()
                    };
                    let split = base + left.rows;
                    visit(left, base, rows);
                    visit(right, split, rows);
                }
            }
        }
        if let Some(root) = self.root.as_mut() {
            visit(root, 0, &rows);
        }
    }
    /// Bring a packed leaf's lines back into memory. `read` gets the leaf's first byte, its byte
    /// length and whether it is the last leaf, and returns its lines read from the text.
    pub fn unpack(&mut self, row: usize, read: impl FnOnce(usize, usize, bool) -> Vec<Line>) {
        let found = self.locate_row(row);
        if found.data.is_some() {
            return;
        }
        let (base_row, base_byte, rows, bytes) =
            (found.base_row, found.base_byte, found.rows, found.bytes);
        let lines = read(base_byte, bytes, base_row + rows == self.len());
        assert_eq!(lines.len(), rows, "a packed block's lines were read back wrongly");
        fn visit(node: &mut Arc<Node>, row: usize, new: Arc<Node>) {
            if let Kind::Branch(..) = node.kind {
                let Kind::Branch(left, right) = &mut Arc::make_mut(node).kind else {
                    unreachable!()
                };
                if row < left.rows {
                    visit(left, row, new);
                } else {
                    visit(right, row - left.rows, new);
                }
            } else {
                *node = new;
            }
        }
        visit(self.root.as_mut().unwrap(), row, leaf(lines));
    }
    /// The leaf holding `row`.
    pub fn locate_row(&self, mut row: usize) -> Located<'_> {
        let (mut base_row, mut base_byte) = (0, 0);
        let mut node = self.root.as_deref().unwrap();
        loop {
            match &node.kind {
                Kind::Branch(left, right) => {
                    if row < left.rows {
                        node = left;
                    } else {
                        row -= left.rows;
                        base_row += left.rows;
                        base_byte += left.bytes;
                        node = right;
                    }
                }
                kind => return located(node, kind, base_row, base_byte),
            }
        }
    }
    /// The leaf holding byte `offset`; the end of the text belongs to the last leaf.
    pub fn locate_byte(&self, mut offset: usize) -> Located<'_> {
        let (mut base_row, mut base_byte) = (0, 0);
        let mut node = self.root.as_deref().unwrap();
        loop {
            match &node.kind {
                Kind::Branch(left, right) => {
                    if offset < left.bytes {
                        node = left;
                    } else {
                        offset -= left.bytes;
                        base_row += left.rows;
                        base_byte += left.bytes;
                        node = right;
                    }
                }
                kind => return located(node, kind, base_row, base_byte),
            }
        }
    }
    pub fn len(&self) -> usize {
        self.root.as_ref().map_or(0, |n| n.rows)
    }
    pub fn max_width(&self) -> usize {
        self.root.as_ref().map_or(0, |n| n.width as usize)
    }
    pub fn crlf_count(&self) -> usize {
        self.root.as_ref().map_or(0, |n| n.crlf)
    }
    /// A line's data. Only for leaves with their lines in memory.
    pub fn get(&self, row: usize) -> &Line {
        let found = self.locate_row(row);
        &found.data.expect(NEEDS_TEXT).0[row - found.base_row]
    }
    #[cfg(test)]
    pub fn start(&self, row: usize) -> usize {
        let found = self.locate_row(row);
        found.base_byte + found.data.expect(NEEDS_TEXT).1[row - found.base_row]
    }
    #[cfg(test)]
    pub fn line_of(&self, offset: usize) -> usize {
        let found = self.locate_byte(offset);
        let (_, starts) = found.data.expect(NEEDS_TEXT);
        found.base_row
            + starts
                .partition_point(|&s| s <= offset - found.base_byte)
                .saturating_sub(1)
    }
    pub fn extend(&mut self, lines: Vec<Line>) {
        self.root = join(self.root.take(), Self::from_lines(lines).root);
    }
    pub fn replace(&mut self, range: Range<usize>, replacement: Self) {
        if self.len() == 0 {
            assert_eq!(range, 0..0);
            *self = replacement;
            return;
        }
        if range.len() == 1 && replacement.len() == 1 {
            let replacement = replacement.get(0).clone();
            self.update_line(range.start, |line| *line = replacement);
            return;
        }
        // Include neighbouring blocks so repeated insertions do not leave a new
        // tiny leaf behind on every edit. More distant blocks stay shared.
        let mut first = self.locate_row(range.start.min(self.len() - 1)).base_row;
        let last = self.locate_row(
            range
                .end
                .saturating_sub(1)
                .max(range.start)
                .min(self.len() - 1),
        );
        let mut end = last.base_row + last.rows;
        if first > 0 {
            first = self.locate_row(first - 1).base_row;
        }
        if end < self.len() {
            let next = self.locate_row(end);
            end = next.base_row + next.rows;
        }
        let rows = (first..range.start)
            .map(|row| self.get(row).clone())
            .chain(replacement.iter().cloned())
            .chain((range.end..end).map(|row| self.get(row).clone()));
        let rebuilt = Self::from_lines(rows);
        let (before, rest) = split(self.root.take(), first);
        let (_, after) = split(rest, end - first);
        self.root = join(join(before, rebuilt.root), after);
    }
    pub fn update_line(&mut self, row: usize, update: impl FnOnce(&mut Line)) {
        fn visit(node: &mut Arc<Node>, row: usize, update: impl FnOnce(&mut Line)) {
            let node = Arc::make_mut(node);
            match &mut node.kind {
                Kind::Leaf { lines, starts } => {
                    update(&mut lines[row]);
                    let mut bytes = 0;
                    for (line, start) in lines.iter().zip(starts) {
                        *start = bytes;
                        bytes += line.bytes;
                    }
                    node.bytes = bytes;
                    node.width = lines.iter().map(|l| l.width).max().unwrap_or(0);
                    node.crlf = lines.iter().filter(|l| l.crlf).count();
                }
                Kind::Packed => unreachable!("{NEEDS_TEXT}"),
                Kind::Branch(left, right) => {
                    if row < left.rows {
                        visit(left, row, update);
                    } else {
                        visit(right, row - left.rows, update);
                    }
                    node.bytes = left.bytes + right.bytes;
                    node.width = left.width.max(right.width);
                    node.crlf = left.crlf + right.crlf;
                }
            }
        }
        visit(self.root.as_mut().unwrap(), row, update);
    }
    /// Leaves holding their lines in memory (the rest keep only their totals).
    #[cfg(test)]
    pub fn unpacked_leaves(&self) -> usize {
        fn count(node: &Node) -> usize {
            match &node.kind {
                Kind::Leaf { .. } => 1,
                Kind::Packed => 0,
                Kind::Branch(left, right) => count(left) + count(right),
            }
        }
        self.root.as_deref().map_or(0, count)
    }
    pub fn iter(&self) -> Iter<'_> {
        Iter {
            stack: self.root.as_deref().into_iter().collect(),
            current: [].iter(),
        }
    }
}
pub(super) struct Iter<'a> {
    stack: Vec<&'a Node>,
    current: std::slice::Iter<'a, Line>,
}
impl<'a> Iterator for Iter<'a> {
    type Item = &'a Line;
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(line) = self.current.next() {
                return Some(line);
            }
            match &self.stack.pop()?.kind {
                Kind::Leaf { lines, .. } => self.current = lines.iter(),
                Kind::Packed => unreachable!("{NEEDS_TEXT}"),
                Kind::Branch(left, right) => {
                    self.stack.push(right);
                    self.stack.push(left);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(i: usize) -> Line {
        Line {
            bytes: i % 13 + 1,
            width: (i % 97) as u32,
            crlf: i.is_multiple_of(3),
            positions: None,
        }
    }
    #[test]
    fn a_local_edit_shares_distant_blocks_with_older_snapshots() {
        let mut tree = LineTree::from_lines((0..10000).map(row));
        let old = tree.clone();
        tree.replace(0..1, LineTree::from_lines([row(77)]));
        assert_eq!(old.get(0), &row(0));
        assert_eq!(tree.get(0), &row(77));
        assert!(std::ptr::eq(old.get(9999), tree.get(9999)));
        verify(tree.root.as_ref().unwrap());
    }

    #[test]
    fn repeated_front_insertions_keep_blocks_compact_and_balanced() {
        let mut tree = LineTree::from_lines((0..1000).map(row));
        for i in 0..2000 {
            tree.replace(0..0, LineTree::from_lines([row(i)]));
        }
        fn leaves(node: &Node) -> usize {
            match &node.kind {
                Kind::Leaf { .. } | Kind::Packed => 1,
                Kind::Branch(a, b) => leaves(a) + leaves(b),
            }
        }
        assert_eq!(tree.len(), 3000);
        assert!(leaves(tree.root.as_ref().unwrap()) <= 2 * tree.len().div_ceil(BLOCK));
        assert_eq!(tree.get(0), &row(1999));
        assert_eq!(tree.get(2999), &row(999));
        verify(tree.root.as_ref().unwrap());
    }
    fn verify(node: &Node) {
        match &node.kind {
            Kind::Leaf { lines, starts } => {
                assert!(!lines.is_empty() && lines.len() <= BLOCK);
                assert_eq!(starts.len(), lines.len());
                assert_eq!(node.height, 1);
            }
            Kind::Packed => assert_eq!(node.height, 1),
            Kind::Branch(a, b) => {
                verify(a);
                verify(b);
                assert!(a.height.abs_diff(b.height) <= 1);
                assert_eq!(node.height, 1 + a.height.max(b.height));
                assert_eq!(node.rows, a.rows + b.rows);
                assert_eq!(node.bytes, a.bytes + b.bytes);
                assert_eq!(node.width, a.width.max(b.width));
                assert_eq!(node.crlf, a.crlf + b.crlf);
            }
        }
    }
    #[test]
    fn splices_preserve_snapshots_and_aggregate_queries() {
        let mut flat: Vec<_> = (0..10000).map(row).collect();
        let original = LineTree::from_lines(flat.clone());
        let mut tree = original.clone();
        let mut seed = 7u64;
        for i in 0..200 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let start = seed as usize % flat.len();
            let end = (start + (seed >> 32) as usize % 70).min(flat.len());
            let inserted: Vec<_> = (0..i % 47).map(|n| row(n + i)).collect();
            tree.replace(start..end, LineTree::from_lines(inserted.clone()));
            flat.splice(start..end, inserted);
            assert!(tree.iter().eq(flat.iter()));
            assert_eq!(
                tree.max_width(),
                flat.iter().map(|l| l.width as usize).max().unwrap_or(0)
            );
            assert_eq!(tree.crlf_count(), flat.iter().filter(|l| l.crlf).count());
            let mut offset = 0;
            for (r, line) in flat.iter().enumerate() {
                assert_eq!(tree.start(r), offset);
                assert_eq!(tree.line_of(offset), r);
                assert_eq!(tree.get(r), line);
                offset += line.bytes;
            }
            verify(tree.root.as_ref().unwrap());
        }
        assert!(
            original
                .iter()
                .eq((0..10000).map(row).collect::<Vec<_>>().iter())
        );
    }
}
