//! A Git change's inline peek, opened from its gutter bar: the lines it
//! removed and added, with stage, unstage, discard and change navigation.

use std::collections::BTreeMap;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

use lgui::core::{
    CursorIcon, EventPolicy, IconStyle, SemanticRole, Semantics, UiElement, UiId, clip, precompiled,
};
use lgui::prelude::{Element, TextAlign, UiRect, VisualStyle, panel, text};

use super::commands::{Binding, Command};
use crate::domain::pane_layout::PaneId;
use crate::features::git::actions::BlockAction;
use crate::features::git::diff_document::{ChangeBlock, DiffDocument, DiffRow, DiffRowKind};
use crate::features::git::{DiffTarget, LineChange};
use crate::ui::fonts;
use crate::ui::theme;

const HEADER_H: f32 = 28.0;
const MAX_ROWS: usize = 14;
const BUTTON_H: f32 = 20.0;

/// The change peeked at in a pane's editor.
#[derive(Clone, Debug, PartialEq)]
pub struct HunkPeek {
    pub pane: PaneId,
    /// The edited file.
    pub path: PathBuf,
    /// The 1-based line whose gutter bar was clicked.
    pub line: usize,
    /// `None` while loading; `Ok(None)` when no change covers the line.
    pub content: Option<Result<Option<Hunk>, String>>,
}

/// One change block of the file, as the diff tab stages or discards it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub root: PathBuf,
    pub relative: PathBuf,
    /// `HeadToIndex` for a staged change, `IndexToWorktree` otherwise.
    pub target: DiffTarget,
    pub block: ChangeBlock,
    /// The block's removed and added lines, in diff order.
    pub rows: Vec<DiffRow>,
    /// The 1-based lines the change spans in the file, for placement.
    pub lines: RangeInclusive<usize>,
}

/// The 1-based file lines `block` covers, as the gutter marks them: a
/// removal is marked on the line after it.
pub(crate) fn block_lines(block: &ChangeBlock) -> RangeInclusive<usize> {
    if block.new_lines == 0 {
        let line = block.new_start + 1;
        line..=line
    } else {
        block.new_start..=block.new_start + block.new_lines - 1
    }
}

/// The change of `document` that covers file line `line`, with its rows.
pub(crate) fn hunk_at(document: &DiffDocument, line: usize) -> Option<(ChangeBlock, Vec<DiffRow>)> {
    let starts = document.change_starts(false);
    document
        .blocks
        .iter()
        .zip(starts.iter())
        .find(|(block, _)| block_lines(block).contains(&line))
        .map(|(block, &start)| {
            let rows = document.rows[start..]
                .iter()
                .take_while(|row| matches!(row.kind, DiffRowKind::Addition | DiffRowKind::Deletion))
                .cloned()
                .collect();
            (*block, rows)
        })
}

/// Runs of adjacent changed lines, in file order, for "change i of n" and
/// the previous and next buttons.
fn change_runs(changes: &BTreeMap<usize, LineChange>) -> Vec<RangeInclusive<usize>> {
    let mut runs: Vec<RangeInclusive<usize>> = Vec::new();
    for &line in changes.keys() {
        match runs.last_mut() {
            Some(run) if *run.end() + 1 >= line => *run = *run.start()..=line,
            _ => runs.push(line..=line),
        }
    }
    runs
}

/// What the editor knows to place the peek: its rect, code left edge,
/// scroll and the file's gutter changes.
pub struct PeekContext<'a> {
    pub pane: PaneId,
    pub rect: UiRect,
    pub code_left: f32,
    pub code_right: f32,
    pub scroll_y: f32,
    pub max_y: f32,
    pub viewport_h: f32,
    pub path: &'a Path,
    pub root: &'a Path,
    pub relative: &'a Path,
    pub changes: Option<&'a BTreeMap<usize, LineChange>>,
}

pub fn render(peek: &HunkPeek, cx: PeekContext<'_>, state: &Binding) -> Element {
    let pane = cx.pane;
    let runs = cx.changes.map(change_runs).unwrap_or_default();
    let run_index = runs.iter().position(|run| run.contains(&peek.line));
    let hunk = match &peek.content {
        Some(Ok(Some(hunk))) => Some(hunk),
        _ => None,
    };
    let message = match &peek.content {
        None => Some("Loading change…".to_owned()),
        Some(Err(error)) => Some(error.clone()),
        Some(Ok(None)) => Some("This change is no longer in the file.".to_owned()),
        Some(Ok(Some(_))) => None,
    };
    let shown_rows = hunk.map_or(0, |hunk| hunk.rows.len().min(MAX_ROWS));
    let more = hunk.map_or(0, |hunk| hunk.rows.len().saturating_sub(MAX_ROWS));
    let body_rows = shown_rows.max(1) + usize::from(more > 0);
    let height = HEADER_H + body_rows as f32 * fonts::editor().line_h + 6.0;

    let lines = hunk.map_or(peek.line..=peek.line, |hunk| hunk.lines.clone());
    let line_top =
        |line: usize| cx.rect.top + (line - 1) as f32 * fonts::editor().line_h - cx.scroll_y;
    let below = line_top(*lines.end() + 1) + 2.0;
    let top = if below + height <= cx.rect.bottom {
        below
    } else {
        (line_top(*lines.start()) - height - 2.0).max(cx.rect.top + 2.0)
    };
    let card = UiRect::new(cx.code_left - 4.0, top, cx.code_right - 4.0, top + height);

    let mut root = theme::bordered(card, theme::c().surface, theme::c().border, 4.0, 1.0)
        .key(format!("hunk-peek-{}", pane.get()))
        .event_policy(EventPolicy::INTERACTIVE)
        .on_pointer_down(|cx, _pointer| cx.stop_propagation());

    // Header: which change, then its actions.
    let title = match (hunk.map(|hunk| hunk.target), run_index) {
        (Some(DiffTarget::HeadToIndex), Some(index)) => {
            format!("Staged change {} of {}", index + 1, runs.len())
        }
        (Some(_), Some(index)) => format!("Change {} of {}", index + 1, runs.len()),
        (Some(DiffTarget::HeadToIndex), None) => "Staged change".into(),
        _ => "Change".into(),
    };
    root = root.child(text(
        UiRect::new(
            card.left + 10.0,
            card.top,
            card.right - 10.0,
            card.top + HEADER_H,
        ),
        title,
        theme::mono(theme::c().text_dim, theme::SMALL),
    ));
    let mut right = card.right - 6.0;
    let button_top = card.top + (HEADER_H - BUTTON_H) / 2.0;
    let mut icon_button = |icon: &'static str, name: &'static str| {
        let rect = UiRect::new(right - BUTTON_H, button_top, right, button_top + BUTTON_H);
        right = rect.left - 2.0;
        panel(rect, VisualStyle::default().radius(3.0))
            .key(format!("hunk-peek-{}-{name}", pane.get()))
            .event_policy(EventPolicy::INTERACTIVE)
            .cursor(CursorIcon::Pointer)
            .semantics(Semantics::new(SemanticRole::Button).name(name))
            .child(precompiled(
                UiElement::icon(
                    UiId::owned(format!("hunk-peek-{}-{name}-icon", pane.get())),
                    theme::icon_rect(UiRect::new(
                        rect.left + 3.0,
                        rect.top + 3.0,
                        rect.right - 3.0,
                        rect.bottom - 3.0,
                    )),
                    icon,
                )
                .icon_style(IconStyle::new(theme::c().text_dim)),
            ))
    };

    let close_state = state.clone();
    let close = icon_button("close", "Close").on_click(move || {
        close_state.dispatch(Command::ClosePeek);
    });
    let mut header_buttons = vec![close];
    for (icon, name, step) in [
        ("arrow-down", "Next Change", 1isize),
        ("arrow-up", "Previous Change", -1),
    ] {
        let nav_state = state.clone();
        let target = run_index
            .map(|index| (index as isize + step).rem_euclid(runs.len() as isize) as usize)
            .and_then(|index| runs.get(index))
            .map(|run| *run.start());
        let path = cx.path.to_path_buf();
        let root_path = cx.root.to_path_buf();
        let relative = cx.relative.to_path_buf();
        let (max_y, viewport_h) = (cx.max_y, cx.viewport_h);
        header_buttons.push(icon_button(icon, name).on_click(move || {
            let Some(line) = target else { return };
            let y =
                ((line - 1) as f32 * fonts::editor().line_h - viewport_h / 3.0).clamp(0.0, max_y);
            nav_state.dispatch(Command::NavigatePeek {
                pane,
                path: path.clone(),
                root: root_path.clone(),
                relative: relative.clone(),
                line,
                y,
            });
        }));
    }
    for button in header_buttons {
        root = root.child(button);
    }

    if let Some(hunk) = hunk {
        // Nothing stages from here; the commit takes what is ticked in the Changes
        // list and in the full diff.
        let actions: &[(BlockAction, &'static str)] = if hunk.target == DiffTarget::HeadToIndex {
            &[]
        } else {
            &[(BlockAction::Discard, "Discard")]
        };
        for &(action, label) in actions.iter().rev() {
            let width = crate::ui::tabs::measure(label, theme::SMALL, 400) + 16.0;
            let rect = UiRect::new(right - width, button_top, right, button_top + BUTTON_H);
            right = rect.left - 4.0;
            let mut style = theme::mono(theme::c().text, theme::SMALL);
            style.align = TextAlign::Center;
            let action_state = state.clone();
            let hunk = hunk.clone();
            root = root.child(
                panel(
                    rect,
                    VisualStyle::filled(theme::c().active_line).radius(3.0),
                )
                .key(format!("hunk-peek-{}-{label}", pane.get()))
                .event_policy(EventPolicy::INTERACTIVE)
                .cursor(CursorIcon::Pointer)
                .semantics(Semantics::new(SemanticRole::Button).name(label))
                .child(text(rect, label, style))
                .on_click(move || {
                    action_state.dispatch(Command::ClosePeek);
                    if action == BlockAction::Discard {
                        action_state.dispatch(Command::RequestDiscardBlock {
                            root: hunk.root.clone(),
                            path: hunk.relative.clone(),
                            block: hunk.block,
                        });
                    } else {
                        action_state.dispatch(Command::ApplyBlock {
                            root: hunk.root.clone(),
                            path: hunk.relative.clone(),
                            target: hunk.target,
                            block: hunk.block,
                            action,
                        });
                    }
                }),
            );
        }
    }
    root = root.child(panel(
        UiRect::new(
            card.left,
            card.top + HEADER_H - 1.0,
            card.right,
            card.top + HEADER_H,
        ),
        VisualStyle::filled(theme::c().border),
    ));

    // Body: the removed and added lines.
    let body = UiRect::new(
        card.left + 1.0,
        card.top + HEADER_H,
        card.right - 1.0,
        card.bottom - 3.0,
    );
    let mut rows = clip(body, 0.0, 0.0);
    let row_rect = |index: usize| {
        let top = body.top + index as f32 * fonts::editor().line_h;
        UiRect::new(body.left, top, body.right, top + fonts::editor().line_h)
    };
    if let Some(message) = message {
        rows = rows.child(text(
            {
                let r = row_rect(0);
                UiRect::new(r.left + 10.0, r.top, r.right - 10.0, r.bottom)
            },
            message,
            theme::mono(theme::c().text_dim, theme::SMALL),
        ));
    }
    if let Some(hunk) = hunk {
        for (index, row) in hunk.rows.iter().take(MAX_ROWS).enumerate() {
            let rect = row_rect(index);
            let (bg, fg, sign) = if row.kind == DiffRowKind::Deletion {
                (theme::c().diff.del_bg, theme::c().diff.del_fg, "-")
            } else {
                (theme::c().diff.add_bg, theme::c().diff.add_fg, "+")
            };
            rows = rows
                .child(panel(rect, VisualStyle::filled(bg)))
                .child(text(
                    UiRect::new(rect.left + 8.0, rect.top, rect.left + 20.0, rect.bottom),
                    sign,
                    fonts::editor().style(fg),
                ))
                .child(text(
                    UiRect::new(rect.left + 22.0, rect.top, rect.right + 2000.0, rect.bottom),
                    row.text.replace('\t', "    "),
                    fonts::editor().style(theme::c().text),
                ));
        }
        if more > 0 {
            rows = rows.child(text(
                {
                    let r = row_rect(shown_rows);
                    UiRect::new(r.left + 10.0, r.top, r.right - 10.0, r.bottom)
                },
                format!("… {more} more line{}", if more == 1 { "" } else { "s" }),
                theme::mono(theme::c().text_dim, theme::SMALL),
            ));
        }
    }
    root.child(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::git::diff::parse_unified;

    #[test]
    fn discard_click_emits_a_hunk_command_without_an_application_root() {
        use crate::ui::git::state::GitPanelState;
        use lgui::application::{AppView, ApplicationContext};
        use lgui::core::{
            InputEvent, Point, PointerButton, PointerData, UiScale, dispatch_runtime_output,
        };
        use lgui::session::UiSession;
        use std::sync::{Arc, Mutex};

        let recorded = Arc::new(Mutex::new(Vec::new()));
        let output = recorded.clone();
        let block = ChangeBlock {
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
        };
        let viewport = UiRect::new(0.0, 0.0, 600.0, 300.0);
        let view: AppView = Arc::new(move |cx| {
            let panel = cx.state(GitPanelState::default());
            let output = output.clone();
            let binding = Binding::new(panel.into(), Default::default, move |command| {
                output.lock().unwrap().push(command);
                true
            });
            let peek = HunkPeek {
                pane: PaneId::new(1),
                path: "repo/f.rs".into(),
                line: 1,
                content: Some(Ok(Some(Hunk {
                    root: "repo".into(),
                    relative: "f.rs".into(),
                    target: DiffTarget::IndexToWorktree,
                    block,
                    rows: Vec::new(),
                    lines: 1..=1,
                }))),
            };
            render(
                &peek,
                PeekContext {
                    pane: peek.pane,
                    rect: viewport,
                    code_left: 40.0,
                    code_right: 560.0,
                    scroll_y: 0.0,
                    max_y: 0.0,
                    viewport_h: 300.0,
                    path: &peek.path,
                    root: Path::new("repo"),
                    relative: Path::new("f.rs"),
                    changes: None,
                },
                &binding,
            )
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let rect = session
            .tree()
            .nodes()
            .iter()
            .find(|node| node.semantics.as_ref().and_then(|s| s.name.as_deref()) == Some("Discard"))
            .unwrap()
            .hit_rect;
        let point = Point::new(
            (rect.left + rect.right) / 2.0,
            (rect.top + rect.bottom) / 2.0,
        );
        let context = ApplicationContext::empty(Default::default());
        for event in [
            InputEvent::PointerDown {
                pointer: PointerData::mouse(point),
                button: PointerButton::Left,
            },
            InputEvent::PointerUp {
                pointer: PointerData::mouse(point),
                button: PointerButton::Left,
            },
        ] {
            dispatch_runtime_output(
                session.handle_input(event),
                &context,
                &lgui::window::WindowId::new("git-peek-test"),
                |action| session.handle_default_action(action),
                |_| {},
            );
        }
        let commands = recorded.lock().unwrap();
        assert!(
            matches!(commands.as_slice(),[Command::ClosePeek,Command::RequestDiscardBlock{root,path,block:actual}] if root==Path::new("repo") && path==Path::new("f.rs") && *actual==block)
        );
    }

    #[test]
    fn finds_the_change_covering_a_gutter_line_with_its_rows() {
        // Replace line 2, insert after line 4, delete line 6.
        let diff = parse_unified(
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,6 +1,6 @@\n 1\n-2\n+two\n 3\n 4\n+4b\n 5\n-6\n",
        )
        .unwrap();
        let document =
            DiffDocument::from_unified("r".into(), "f".into(), DiffTarget::IndexToWorktree, diff);
        let (block, rows) = hunk_at(&document, 2).unwrap();
        assert_eq!(block_lines(&block), 2..=2);
        assert_eq!(
            rows.iter().map(|row| row.text.as_str()).collect::<Vec<_>>(),
            ["2", "two"]
        );
        assert_eq!(block_lines(&hunk_at(&document, 5).unwrap().0), 5..=5);
        // The removal of line 6 is marked on the line after it.
        let (removed, rows) = hunk_at(&document, 7).unwrap();
        assert_eq!(removed.new_lines, 0);
        assert_eq!(rows.len(), 1);
        assert!(hunk_at(&document, 3).is_none());
    }

    #[test]
    fn change_runs_join_adjacent_changed_lines() {
        let changes = BTreeMap::from([
            (1, LineChange::Added),
            (2, LineChange::Modified),
            (5, LineChange::Deleted),
            (9, LineChange::Added),
            (10, LineChange::Added),
        ]);
        assert_eq!(change_runs(&changes), [1..=2, 5..=5, 9..=10]);
    }
}
