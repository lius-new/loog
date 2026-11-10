//! Atomic edits for a bounded, normalized set of view-local selections.
use super::{Assoc, ChangeSet, Edit, EditKind, EditorMut, Selection, TextBuffer};
use std::ops::Range;
pub const LIMIT: usize = 256;

pub fn normalize(
    buffer: &TextBuffer,
    primary: Selection,
    others: Vec<Selection>,
) -> (Selection, Vec<Selection>) {
    let mut entries: Vec<_> = std::iter::once(primary)
        .chain(others)
        .take(LIMIT)
        .enumerate()
        .map(|(i, s)| {
            (
                Selection {
                    wrap_affinity: s.wrap_affinity,
                    ..buffer.clamp(s)
                },
                i == 0,
            )
        })
        .collect();
    entries.sort_by_key(|(s, _)| {
        let r = range(*s);
        (r.start, r.end)
    });
    let mut merged: Vec<(Selection, bool)> = Vec::new();
    for (s, main) in entries {
        if let Some((last, owns)) = merged.last_mut() {
            let a = range(*last);
            let b = range(s);
            if b.start < a.end || b.start == a.end && (a.is_empty() || b.is_empty()) {
                let model = if main { s } else { *last };
                let lo = a.start.min(b.start);
                let hi = a.end.max(b.end);
                *last = if lo == hi {
                    Selection::caret(lo)
                } else if model.anchor.is_some_and(|a| a > model.cursor) {
                    Selection::spanning(hi, lo)
                } else {
                    Selection::spanning(lo, hi)
                };
                last.wrap_affinity = model.wrap_affinity;
                *owns |= main;
                continue;
            }
        }
        merged.push((s, main));
    }
    let i = merged.iter().position(|(_, main)| *main).unwrap();
    let primary = merged.remove(i).0;
    (primary, merged.into_iter().map(|(s, _)| s).collect())
}
pub fn range(s: Selection) -> Range<usize> {
    s.range().unwrap_or(s.cursor..s.cursor)
}

#[derive(Clone, Copy)]
struct Point {
    old: usize,
    extra: usize,
    after: bool,
}
impl Point {
    fn old(old: usize) -> Self {
        Self {
            old,
            extra: 0,
            after: true,
        }
    }
    fn inserted(old: usize, extra: usize) -> Self {
        Self {
            old,
            extra,
            after: false,
        }
    }
    fn resolve(self, changes: &ChangeSet) -> usize {
        changes.map(
            self.old,
            if self.after {
                Assoc::After
            } else {
                Assoc::Before
            },
        ) + self.extra
    }
}
struct Plan {
    edits: Vec<Edit>,
    cursor: Point,
    anchor: Option<Point>,
    pair: Option<(Point, Point)>,
}
impl Plan {
    fn replace(s: Selection, value: &str) -> Self {
        Self::at(range(s), value, value.len())
    }
    fn at(r: Range<usize>, value: &str, caret: usize) -> Self {
        let cursor = Point::inserted(r.start, caret);
        Self {
            edits: vec![Edit::replace(r, value)],
            cursor,
            anchor: None,
            pair: None,
        }
    }
    fn keep(s: Selection) -> Self {
        Self {
            edits: vec![],
            cursor: Point::old(s.cursor),
            anchor: s.anchor.map(Point::old),
            pair: None,
        }
    }
}
fn apply(editor: &mut EditorMut<'_>, kind: EditKind, mut plans: Vec<Plan>) {
    let mut edits: Vec<_> = plans
        .iter_mut()
        .flat_map(|p| std::mem::take(&mut p.edits))
        .collect();
    edits.sort_by_key(|e| (e.range.start, e.range.end));
    let mut unique: Vec<Edit> = Vec::new();
    for edit in edits {
        if let Some(last) = unique.last_mut() {
            if *last == edit {
                continue;
            }
            if last.text.is_empty() && edit.text.is_empty() && edit.range.start <= last.range.end {
                last.range.end = last.range.end.max(edit.range.end);
                continue;
            }
        }
        unique.push(edit);
    }
    let resolve = |p: &Plan, c: &ChangeSet, b: &TextBuffer| {
        let at = p.cursor.resolve(c).min(b.len());
        let at = if b.is_grapheme_boundary(at) {
            at
        } else {
            b.next_grapheme(at)
        };
        Selection {
            cursor: at,
            anchor: p.anchor.map(|p| p.resolve(c)),
            preferred_column: None,
            wrap_affinity: Default::default(),
        }
    };
    let Ok(changes) = editor.edit(kind, unique, |c, b| resolve(&plans[0], c, b)) else {
        return;
    };
    let mut states = plans.iter().map(|p| resolve(p, &changes, editor.buffer()));
    let primary = states.next().unwrap();
    let extra = states.collect();
    // Install without breaking consecutive typing's undo grouping.
    editor.install_selections(primary, extra);
    if !changes.is_empty() {
        for p in &plans {
            if let Some((a, b)) = p.pair {
                editor.track_pair(a.resolve(&changes), b.resolve(&changes));
            }
        }
        editor.finish_multiple();
    }
}
pub fn insert(editor: &mut EditorMut<'_>, text: &str, kind: EditKind) {
    let plans = editor
        .selections()
        .into_iter()
        .map(|s| Plan::replace(s, text))
        .collect();
    apply(editor, kind, plans);
}
pub fn paste(editor: &mut EditorMut<'_>, text: &str) {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let parts: Vec<_> = normalized.split('\n').collect();
    let states = editor.selections();
    let mut order: Vec<_> = (0..states.len()).collect();
    order.sort_by_key(|&i| range(states[i]).start);
    let mut rank = vec![0; states.len()];
    for (i, &index) in order.iter().enumerate() {
        rank[index] = i;
    }
    let whole = normalized.replace('\n', editor.line_ending());
    let plans = states
        .into_iter()
        .enumerate()
        .map(|(i, s)| {
            Plan::replace(
                s,
                if parts.len() == rank.len() {
                    parts[rank[i]]
                } else {
                    &whole
                },
            )
        })
        .collect();
    apply(editor, EditKind::Other, plans);
    editor.break_undo_group();
}
pub fn erase(editor: &mut EditorMut<'_>, forward: bool, word: bool) {
    let plans = editor
        .selections()
        .into_iter()
        .map(|s| {
            let c = s.cursor;
            let r = s.range().unwrap_or_else(|| {
                if !forward && !word && editor.is_empty_auto_pair(c) {
                    c - 1..c + 1
                } else {
                    match (forward, word) {
                        (true, true) => c..editor.word_right(c),
                        (true, false) => c..editor.next_grapheme(c),
                        (false, true) => editor.word_left(c)..c,
                        (false, false) => editor.prev_grapheme(c)..c,
                    }
                }
            });
            Plan::at(r, "", 0)
        })
        .collect();
    apply(
        editor,
        if word {
            EditKind::Other
        } else if forward {
            EditKind::Delete
        } else {
            EditKind::Backspace
        },
        plans,
    );
}
pub fn type_text(editor: &mut EditorMut<'_>, text: &str, language: &str) {
    let mut chars = text.chars();
    let Some(ch) = chars.next() else {
        return;
    };
    if chars.next().is_some() {
        insert(editor, text, EditKind::Typing);
        return;
    }
    if !matches!(ch, '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'') {
        insert(editor, text, EditKind::Typing);
        return;
    }
    let plans = editor
        .selections()
        .into_iter()
        .map(|s| {
            let r = range(s);
            let c = s.cursor;
            if r.is_empty()
                && editor.is_auto_close(c)
                && editor.text()[c..].starts_with(ch)
                && matches!(ch, ')' | ']' | '}' | '\'' | '"')
            {
                return Plan::keep(Selection::caret(c + ch.len_utf8()));
            }
            let start = editor.line_start(editor.line_of(c));
            let prefix = &editor.text()[start..c];
            if r.is_empty()
                && matches!(ch, '}' | ']' | ')')
                && !prefix.is_empty()
                && editor.selections().iter().all(|other| {
                    other.cursor == c || editor.line_of(other.cursor) != editor.line_of(c)
                })
                && prefix.chars().all(|c| c == ' ' || c == '\t')
                && crate::normal::code_context(editor.text(), c, language)
            {
                let removed = if prefix.ends_with('\t') {
                    1
                } else {
                    prefix.len().min(editor.options().indent_size)
                };
                return Plan::at(c - removed..c, text, text.len());
            }
            let close = match ch {
                '(' => ')',
                '[' => ']',
                '{' => '}',
                '"' if crate::normal::quotes(language) => '"',
                '\'' if crate::normal::quotes(language)
                    && (language != "rust" || !r.is_empty()) =>
                {
                    '\''
                }
                _ => return Plan::replace(s, text),
            };
            let prev = editor.text()[..r.start].chars().next_back();
            let next = editor.text()[r.end..].chars().next();
            if !editor.options().auto_pairs
                || !crate::normal::code_context(editor.text(), r.start, language)
                || language == "rust" && prev == Some('\'')
                || matches!(ch, '\'' | '"')
                    && prev.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '\\')
                || r.is_empty()
                    && next.is_some_and(|c| {
                        !c.is_whitespace() && !matches!(c, ')' | ']' | '}' | ',' | ';' | ':')
                    })
            {
                return Plan::replace(s, text);
            }
            let content = &editor.text()[r.clone()];
            let end = content.len() + 1;
            let mut p = Plan::at(r.clone(), &format!("{ch}{content}{close}"), 1);
            if !r.is_empty() {
                let (a, c) = if s.anchor.is_some_and(|a| a > s.cursor) {
                    (end, 1)
                } else {
                    (1, end)
                };
                p.cursor = Point::inserted(r.start, c);
                p.anchor = Some(Point::inserted(r.start, a));
            }
            p.pair = Some((Point::inserted(r.start, 0), Point::inserted(r.start, end)));
            p
        })
        .collect();
    apply(editor, EditKind::Other, plans);
    editor.break_undo_group();
}
pub fn enter(editor: &mut EditorMut<'_>) {
    let plans = editor
        .selections()
        .into_iter()
        .map(|s| {
            let r = range(s);
            let start = editor.line_start(editor.line_of(r.start));
            let prefix = &editor.text()[start..r.start];
            let indent: String = prefix
                .chars()
                .take_while(|&c| c == ' ' || c == '\t')
                .collect();
            let a = prefix.trim_end().chars().next_back();
            let b = editor.text()[r.end..]
                .trim_start_matches([' ', '\t'])
                .chars()
                .next();
            let code =
                crate::normal::code_context(editor.text(), r.start, &editor.options().language);
            let extra = if code
                && (matches!(a, Some('{' | '[' | '('))
                    || a == Some(':')
                        && matches!(editor.options().language.as_ref(), "python" | "yaml"))
            {
                editor.options().indent()
            } else {
                String::new()
            };
            let nl = editor.line_ending();
            let mut value = format!("{nl}{indent}{extra}");
            let caret = value.len();
            if code
                && matches!(
                    (a, b),
                    (Some('{'), Some('}')) | (Some('['), Some(']')) | (Some('('), Some(')'))
                )
            {
                value.push_str(&format!("{nl}{indent}"));
            }
            Plan::at(r, &value, caret)
        })
        .collect();
    apply(editor, EditKind::Other, plans);
    editor.break_undo_group();
}
pub fn indent(editor: &mut EditorMut<'_>, outdent: bool) {
    let states = editor.selections();
    let indent = editor.options().indent();
    if !outdent && states.iter().all(|s| s.range().is_none()) {
        insert(editor, &indent, EditKind::Other);
        editor.break_undo_group();
        return;
    }
    let mut rows = std::collections::BTreeSet::new();
    for s in &states {
        let r = range(*s);
        let first = editor.line_of(r.start);
        let last = editor.line_of(r.end).saturating_sub(usize::from(
            r.end > r.start && r.end == editor.line_start(editor.line_of(r.end)),
        ));
        rows.extend(first..=last);
    }
    let edits = rows
        .into_iter()
        .filter_map(|row| {
            let at = editor.line_start(row);
            if !outdent {
                Some(Edit::insert(at, &indent))
            } else {
                let line = editor.line(row);
                let n = if line.starts_with('\t') {
                    1
                } else {
                    line.chars()
                        .take(editor.options().indent_size)
                        .take_while(|&c| c == ' ')
                        .count()
                };
                (n > 0).then(|| Edit::delete(at..at + n))
            }
        })
        .collect();
    let mut plans: Vec<_> = states.into_iter().map(Plan::keep).collect();
    plans[0].edits = edits;
    apply(editor, EditKind::Other, plans);
    editor.break_undo_group();
}

fn touched(buffer: &TextBuffer, s: Selection) -> (usize, usize) {
    let r = range(s);
    let first = buffer.line_of(r.start);
    let last = buffer.line_of(r.end);
    (
        first,
        last.saturating_sub(usize::from(
            r.end > r.start && r.end == buffer.line_start(last),
        )),
    )
}
pub fn lines(editor: &mut EditorMut<'_>, operation: crate::normal::LineOperation) {
    use crate::normal::LineOperation::*;
    let states = editor.selections();
    let mut groups: Vec<(usize, usize)> = Vec::new();
    let mut intervals: Vec<_> = states.iter().map(|&s| touched(editor, s)).collect();
    intervals.sort();
    for (first, last) in intervals {
        if let Some(g) = groups.last_mut()
            && first <= g.1 + 1
        {
            g.1 = g.1.max(last);
        } else {
            groups.push((first, last));
        }
    }
    let mut plans: Vec<_> = states.iter().copied().map(Plan::keep).collect();
    for (first, last) in groups {
        let indices: Vec<_> = states
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                let (a, b) = touched(editor, **s);
                a >= first && b <= last
            })
            .map(|(i, _)| i)
            .collect();
        let start = editor.line_start(first);
        let end = if last + 1 < editor.line_count() {
            editor.line_start(last + 1)
        } else {
            editor.len()
        };
        let owner = indices[0];
        match operation {
            Delete => {
                let from = if last + 1 == editor.line_count() && first > 0 {
                    editor.line_end(first - 1)
                } else {
                    start
                };
                plans[owner].edits.push(Edit::delete(from..end));
                for &i in &indices {
                    plans[i].cursor = Point::inserted(from, 0);
                    plans[i].anchor = None;
                }
            }
            CopyUp | CopyDown => {
                let down = operation == CopyDown;
                let eof = last + 1 == editor.line_count();
                let nl = editor.line_ending();
                let block = &editor.text()[start..end];
                let value = if eof {
                    if down {
                        format!("{nl}{block}")
                    } else {
                        format!("{block}{nl}")
                    }
                } else {
                    block.into()
                };
                let at = if down { end } else { start };
                let extra = if down && eof { nl.len() } else { 0 };
                plans[owner].edits.push(Edit::insert(at, value));
                for &i in &indices {
                    let s = states[i];
                    let point = |p: usize| Point::inserted(at, extra + p.saturating_sub(start));
                    plans[i].cursor = point(s.cursor);
                    plans[i].anchor = s.anchor.map(point);
                }
            }
            MoveUp | MoveDown => {
                let down = operation == MoveDown;
                if !down && first == 0 || down && last + 1 >= editor.line_count() {
                    continue;
                }
                let low = if down { first } else { first - 1 };
                let high = if down { last + 1 } else { last };
                let from = editor.line_start(low);
                let to = if high + 1 < editor.line_count() {
                    editor.line_start(high + 1)
                } else {
                    editor.len()
                };
                let contents: Vec<_> = (low..=high).map(|row| editor.line(row)).collect();
                let endings: Vec<_> = (low..=high)
                    .map(|row| {
                        let next = if row + 1 < editor.line_count() {
                            editor.line_start(row + 1)
                        } else {
                            editor.len()
                        };
                        &editor.text()[editor.line_end(row)..next]
                    })
                    .collect();
                let mut order: Vec<_> = (0..contents.len()).collect();
                if down {
                    order.rotate_right(1);
                } else {
                    order.rotate_left(1);
                }
                let mut origins = vec![0; contents.len()];
                let mut value = String::new();
                let mut boundary = 0;
                for (position, &original) in order.iter().enumerate() {
                    origins[original] = value.len();
                    value.push_str(contents[original]);
                    value.push_str(endings[position]);
                    if low + original == last {
                        boundary = value.len();
                    }
                }
                for &i in &indices {
                    let point = |p: usize| {
                        if p == end && last + 1 < editor.line_count() {
                            return Point::inserted(from, boundary);
                        }
                        let row = editor.line_of(p);
                        let col = p - editor.line_start(row);
                        Point::inserted(
                            from,
                            origins[row - low] + col.min(contents[row - low].len()),
                        )
                    };
                    plans[i].cursor = point(states[i].cursor);
                    plans[i].anchor = states[i].anchor.map(point);
                }
                plans[owner].edits.push(Edit::replace(from..to, value));
            }
        }
    }
    apply(editor, EditKind::Other, plans);
    editor.break_undo_group();
}
pub fn comment(editor: &mut EditorMut<'_>, prefix: &str) {
    if prefix.is_empty() {
        return;
    }
    let states = editor.selections();
    let mut rows = std::collections::BTreeSet::new();
    for &s in &states {
        let (a, b) = touched(editor, s);
        rows.extend(a..=b);
    }
    let single = rows.len() == 1;
    let targets: Vec<_> = rows
        .into_iter()
        .filter_map(|row| {
            let line = editor.line(row);
            let rest = line.trim_start_matches([' ', '\t']);
            (!rest.is_empty() || single).then_some((
                editor.line_start(row) + line.len() - rest.len(),
                rest.starts_with(prefix),
            ))
        })
        .collect();
    let remove = !targets.is_empty() && targets.iter().all(|t| t.1);
    let edits = targets
        .into_iter()
        .map(|(at, _)| {
            if remove {
                let n =
                    prefix.len() + usize::from(editor.text()[at + prefix.len()..].starts_with(' '));
                Edit::delete(at..at + n)
            } else {
                Edit::insert(at, format!("{prefix} "))
            }
        })
        .collect();
    let mut plans: Vec<_> = states.into_iter().map(Plan::keep).collect();
    plans[0].edits = edits;
    apply(editor, EditKind::Other, plans);
    editor.break_undo_group();
}

#[cfg(test)]
mod tests;

/// Vim Ctrl+U uses the same atomic selection planner as other deletion commands.
pub(crate) fn erase_prefix(editor: &mut EditorMut<'_>) {
    let plans = editor
        .selections()
        .into_iter()
        .map(|s| {
            let row = editor.line_of(s.cursor);
            let start = editor.line_start(row);
            let indent = start + editor.line(row).len()
                - editor.line(row).trim_start_matches([' ', '\t']).len();
            let from = if s.cursor > indent { indent } else { start };
            Plan::at(s.range().unwrap_or(from..s.cursor), "", 0)
        })
        .collect();
    apply(editor, EditKind::Other, plans);
}
