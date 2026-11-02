//! A structure tree in the style of IntelliJ's Structure tool window: a small
//! toolbar, then one line per symbol with a chevron, a tinted kind icon, the
//! name and, fainter, its detail. The line holding the caret stays marked.
//!
//! The view reads one [`Frame`] and reports everything the person does as a
//! [`Command`]; it never touches application state.
use std::sync::Arc;

use lgui::core::{
    CursorIcon, EventPolicy, IconStyle, PointerButton, SemanticRole, Semantics, UiElement,
    UiEventKind, UiEventPayload, UiId, WheelUnit, clip, precompiled,
};
use lgui::prelude::{Element, UiRect, VisualStyle, panel, text};

use crate::features::highlighting::HighlightId;
use crate::features::outline::{Kind, Node, Outline, Row};
use crate::ui::components::scrollbar;
use crate::ui::theme;

const HEADER_H: f32 = theme::TABS_H;
const TOOLBAR_H: f32 = 28.0;
const ROW_H: f32 = 22.0;
const INDENT: f32 = 14.0;
const CHEVRON_W: f32 = 14.0;
const ICON_SIZE: f32 = 16.0;
const GAP: f32 = 4.0;
/// Extra shaping room so the final glyph is not wrapped or clipped prematurely.
const LABEL_PAD: f32 = 6.0;

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Move the caret to the node.
    Go(usize),
    Toggle(String),
    ExpandAll,
    CollapseAll,
    ToggleSort,
    /// The list's new top offset in pixels.
    Scroll(f32),
    Hover(Option<usize>),
    /// Where inside the thumb the pointer took hold of it.
    ScrollbarGrab(f32),
    ScrollbarRelease,
    ResizeStart,
    Resize(f32),
    ResizeEnd,
}

#[derive(Clone)]
pub struct Commands(Arc<dyn Fn(Command) + Send + Sync>);

impl Commands {
    pub fn new(dispatch: impl Fn(Command) + Send + Sync + 'static) -> Self {
        Self(Arc::new(dispatch))
    }

    pub fn dispatch(&self, command: Command) {
        (self.0)(command);
    }
}

pub enum Body<'a> {
    /// Nothing to list; says why.
    Message(&'a str),
    Tree {
        outline: &'a Outline,
        rows: &'a [Row],
        /// Key of the node holding the caret.
        current: Option<&'a str>,
        /// Key of a node to bring into sight.
        reveal: Option<&'a str>,
    },
}

pub struct Frame<'a> {
    pub body: Body<'a>,
    pub sorted: bool,
    pub scroll: f32,
    pub hovered: Option<usize>,
    /// The pointer is anywhere over the drawer, scrollbar included.
    pub pointer_inside: bool,
    pub scrollbar_grab: Option<f32>,
    pub resizable: bool,
    pub resizing: bool,
}

pub fn render(rect: UiRect, frame: &Frame<'_>, commands: Commands, header: Element) -> Element {
    let header_rect = UiRect::new(rect.left, rect.top, rect.right, rect.top + HEADER_H);
    let toolbar = UiRect::new(
        rect.left,
        header_rect.bottom,
        rect.right,
        header_rect.bottom + TOOLBAR_H,
    );
    let list = UiRect::new(rect.left, toolbar.bottom, rect.right, rect.bottom);
    let mut bar = panel(rect, VisualStyle::filled(theme::c().sidebar));

    // Empty space clears the hovered row; rows drawn later win hit-testing.
    let clear = commands.clone();
    bar = bar.child(
        panel(list, VisualStyle::default())
            .event_policy(EventPolicy::INTERACTIVE)
            .on_event_capture(UiEventKind::PointerMove, move |_cx, _payload| {
                clear.dispatch(Command::Hover(None));
            }),
    );

    match &frame.body {
        Body::Message(message) => {
            bar = bar.child(text(
                UiRect::new(
                    list.left + 12.0,
                    list.top + 12.0,
                    list.right - 12.0,
                    list.top + 60.0,
                ),
                message.to_string(),
                theme::sans(theme::c().text_dim, 12.0),
            ));
        }
        Body::Tree {
            outline,
            rows,
            current,
            reveal,
        } => {
            bar = tree(
                bar, list, frame, outline, rows, *current, *reveal, &commands,
            )
        }
    }

    bar = bar.child(toolbar_row(toolbar, frame, &commands));
    bar = bar.child(header);

    if frame.resizable {
        let handle_rect = UiRect::new(rect.left, rect.top, rect.left + 8.0, rect.bottom);
        let right = rect.right;
        let (down, drag, up) = (commands.clone(), commands.clone(), commands.clone());
        let handle = crate::ui::components::resize_handle::render(
            "outline-resize-handle",
            handle_rect,
            UiRect::new(rect.left, rect.top, rect.left + 1.0, rect.bottom),
            CursorIcon::ResizeHorizontal,
            frame.resizing,
        )
        .on_pointer_down_with_button(move |_cx, _p, button| {
            if button == PointerButton::Left {
                down.dispatch(Command::ResizeStart);
            }
        })
        .on_pointer_drag(move |_cx, p| {
            drag.dispatch(Command::Resize(
                (right - p.point.x).clamp(theme::SIDEBAR_MIN_W, theme::SIDEBAR_MAX_W),
            ));
        })
        .on_pointer_up(move |_cx, _p| up.dispatch(Command::ResizeEnd));
        bar = bar.child(handle);
    }
    bar
}

fn toolbar_row(rect: UiRect, frame: &Frame<'_>, commands: &Commands) -> Element {
    let mut row = panel(rect, VisualStyle::filled(theme::c().sidebar));
    let mut left = rect.left + 8.0;
    for (index, label, icon, active, command) in [
        (0, "Expand All", "expand-all", false, Command::ExpandAll),
        (
            1,
            "Collapse All",
            "search-collapse",
            false,
            Command::CollapseAll,
        ),
        (
            2,
            "Sort Alphabetically",
            "sort",
            frame.sorted,
            Command::ToggleSort,
        ),
    ] {
        let slot = UiRect::new(left, rect.top + 3.0, left + 24.0, rect.bottom - 3.0);
        left += 26.0;
        let commands = commands.clone();
        row = row.child(
            lgui::icon_button(slot, icon.to_owned(), label)
                .key(format!("outline-tool-{index}"))
                .semantics({
                    let mut semantics = Semantics::new(SemanticRole::Button).name(label);
                    semantics.state.selected = active;
                    semantics
                })
                .cursor(CursorIcon::Pointer)
                .on_click(move || commands.dispatch(command.clone())),
        );
    }
    row.child(crate::ui::components::divider::bottom(rect))
}

#[allow(clippy::too_many_arguments)]
fn tree(
    mut bar: Element,
    list: UiRect,
    frame: &Frame<'_>,
    outline: &Outline,
    rows: &[Row],
    current: Option<&str>,
    reveal: Option<&str>,
    commands: &Commands,
) -> Element {
    let content_h = rows.len() as f32 * ROW_H;
    let max_scroll = (content_h - list.height()).max(0.0);
    let mut scroll = frame.scroll.clamp(0.0, max_scroll);
    if let Some(index) = reveal.and_then(|key| {
        rows.iter()
            .position(|row| outline.node(row.node).is_some_and(|node| node.key == key))
    }) {
        // Pinned ancestors cover one row each above the revealed row.
        let top = index as f32 * ROW_H;
        let covered = f32::from(rows[index].depth) * ROW_H;
        if top - covered < scroll {
            scroll = (top - covered).max(0.0);
        } else if top + ROW_H > scroll + list.height() {
            scroll = top + ROW_H - list.height();
        }
        scroll = scroll.clamp(0.0, max_scroll);
    }

    let wheel = commands.clone();
    bar = bar.on_event(UiEventKind::Wheel, move |_cx, payload| {
        if let UiEventPayload::Wheel { delta } = payload {
            let step = match delta.unit {
                WheelUnit::Lines => delta.y * ROW_H * 3.0,
                WheelUnit::Pixels => delta.y,
            };
            wheel.dispatch(Command::Scroll((scroll - step).clamp(0.0, max_scroll)));
        }
    });

    let first = (scroll / ROW_H).floor() as usize;
    let last = (((scroll + list.height()) / ROW_H).ceil() as usize + 1).min(rows.len());
    let mut body = clip(list, 0.0, 0.0);
    for (index, row) in rows.iter().enumerate().take(last).skip(first) {
        let Some(node) = outline.node(row.node) else {
            continue;
        };
        let top = list.top + index as f32 * ROW_H - scroll;
        body = body.child(row_element(
            "body",
            list,
            top,
            index,
            row,
            node,
            current == Some(node.key.as_str()),
            frame.hovered == Some(index),
            commands,
        ));
    }
    bar = bar.child(body);

    // Branches scrolled off the top stay pinned, deepest first so that a branch
    // being pushed out slides beneath its parent.
    let mut pinned = clip(list, 0.0, 0.0);
    for (index, offset) in sticky(rows, scroll).into_iter().rev() {
        let row = &rows[index];
        let Some(node) = outline.node(row.node) else {
            continue;
        };
        let top = list.top + offset;
        pinned = pinned.child(
            panel(
                UiRect::new(list.left, top, list.right, top + ROW_H),
                VisualStyle::filled(theme::c().sidebar),
            )
            .child(row_element(
                "sticky",
                list,
                top,
                index,
                row,
                node,
                current == Some(node.key.as_str()),
                frame.hovered == Some(index),
                commands,
            )),
        );
    }
    bar = bar.child(pinned);

    // Only the thumb is drawn, and only while the list is in use, so the right
    // edge stays free for text and clicks as in the file tree.
    if max_scroll > 0.0 && (frame.pointer_inside || frame.scrollbar_grab.is_some()) {
        bar = bar.child(vertical_scrollbar(
            list, content_h, scroll, max_scroll, frame, commands,
        ));
    }
    bar
}

/// The expanded branches to pin and where, as offsets below the list's top: a
/// branch pins at the slot of its depth once scrolled past it, and is pushed up
/// by the first row after its subtree.
fn sticky(rows: &[Row], scroll: f32) -> Vec<(usize, f32)> {
    if scroll <= 0.0 {
        return Vec::new();
    }
    let open = |row: &Row| row.has_children && row.expanded;
    let mut ends = vec![rows.len(); rows.len()];
    let mut stack: Vec<usize> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        while let Some(&branch) = stack.last() {
            if rows[branch].depth < row.depth {
                break;
            }
            ends[branch] = index;
            stack.pop();
        }
        if open(row) {
            stack.push(index);
        }
    }
    let deepest = rows.iter().map(|row| row.depth).max().unwrap_or(0);
    let reach = (f32::from(deepest) + 1.0) * ROW_H;
    let mut pinned = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let natural = index as f32 * ROW_H - scroll;
        if natural > reach {
            break;
        }
        let slot = f32::from(row.depth) * ROW_H;
        if !open(row) || natural >= slot {
            continue;
        }
        let pushed = ends[index] as f32 * ROW_H - scroll - ROW_H;
        let top = slot.min(pushed);
        if top > -ROW_H {
            pinned.push((index, top));
        }
    }
    pinned
}

#[allow(clippy::too_many_arguments)]
fn row_element(
    layer: &str,
    list: UiRect,
    top: f32,
    index: usize,
    row: &Row,
    node: &Node,
    current: bool,
    hovered: bool,
    commands: &Commands,
) -> Element {
    let rect = UiRect::new(list.left, top, list.right, top + ROW_H);
    let style = if current {
        VisualStyle::filled(theme::c().selection).alpha(130)
    } else if hovered {
        VisualStyle::filled(theme::c().surface)
    } else {
        VisualStyle::default()
    };
    let left = list.left + 8.0 + f32::from(row.depth) * INDENT;
    let chevron = UiRect::new(left, top, left + CHEVRON_W, top + ROW_H);
    let icon = UiRect::new(
        chevron.right,
        top + (ROW_H - ICON_SIZE) / 2.0,
        chevron.right + ICON_SIZE,
        top + (ROW_H + ICON_SIZE) / 2.0,
    );
    let label_left = icon.right + GAP;
    let name_width = label_width(&node.name);

    let go = commands.clone();
    let hover = commands.clone();
    let node_index = row.node;
    let mut element = panel(rect, style)
        .key(format!("outline-{layer}-row-{index}"))
        .event_policy(EventPolicy::INTERACTIVE)
        .cursor(CursorIcon::Pointer)
        .semantics(Semantics::new(SemanticRole::Button).name(node.name.clone()))
        .on_pointer_move(move |_cx, _pointer| hover.dispatch(Command::Hover(Some(index))))
        .on_pointer_down_with_button(move |cx, _pointer, button| {
            if button == PointerButton::Left {
                go.dispatch(Command::Go(node_index));
                cx.stop_propagation();
            }
        });

    if row.has_children {
        let toggle = commands.clone();
        let key = node.key.clone();
        element = element.child(
            panel(chevron, VisualStyle::default())
                .event_policy(EventPolicy::INTERACTIVE)
                .on_pointer_down_with_button(move |cx, _pointer, button| {
                    if button == PointerButton::Left {
                        toggle.dispatch(Command::Toggle(key.clone()));
                        cx.stop_propagation();
                    }
                })
                .child(crate::ui::drawer::drawer_icon(
                    UiId::owned(format!("outline-{layer}-chevron-{index}")),
                    if row.expanded {
                        "chevron-down"
                    } else {
                        "chevron-right"
                    },
                    theme::icon_rect(chevron),
                    theme::c().text_dim,
                )),
        );
    }

    element = element.child(kind_badge(layer, index, node.kind, icon));

    let color = if node.deprecated {
        theme::c().text_faint
    } else {
        theme::c().text
    };
    element = element.child(label(
        UiRect::new(label_left, top + 4.0, list.right, top + ROW_H),
        &node.name,
        color,
    ));
    if let Some(detail) = &node.detail {
        let detail_left = label_left + name_width + 8.0;
        if detail_left < list.right - 24.0 {
            element = element.child(label(
                UiRect::new(detail_left, top + 4.0, list.right, top + ROW_H),
                detail,
                theme::c().text_faint,
            ));
        }
    }
    element
}

/// Shape the entire text at its natural width, then crop only at the viewport.
/// A narrow layout rect would break at spaces or CJK word boundaries first.
fn label(viewport: UiRect, content: &str, color: lgui::prelude::Color) -> Element {
    let viewport = UiRect::new(
        viewport.left,
        viewport.top,
        viewport.right.max(viewport.left),
        viewport.bottom,
    );
    clip(viewport, 0.0, 0.0).child(text(
        UiRect::new(
            viewport.left,
            viewport.top,
            viewport.left + label_width(content),
            viewport.bottom,
        ),
        content.to_owned(),
        theme::mono(color, theme::UI_SIZE),
    ))
}

fn label_width(content: &str) -> f32 {
    crate::ui::tabs::measure(content, theme::UI_SIZE, 400) + LABEL_PAD
}

/// The tinted symbol icon; headings get a small `H1`..`H6` tag instead.
fn kind_badge(layer: &str, index: usize, kind: Kind, rect: UiRect) -> Element {
    if let Kind::Heading(level) = kind {
        return text(
            UiRect::new(rect.left, rect.top + 1.0, rect.right + 2.0, rect.bottom),
            format!("H{level}"),
            theme::sans_semibold(theme::syntax_color(HighlightId::KEYWORD), 10.0),
        )
        .into();
    }
    let (icon, class) = kind_style(kind);
    precompiled(
        UiElement::icon(
            UiId::owned(format!("outline-{layer}-kind-{index}")),
            rect,
            icon,
        )
        .icon_style(IconStyle::new(theme::syntax_color(class))),
    )
}

/// The icon key and highlight class whose color tints a kind.
fn kind_style(kind: Kind) -> (&'static str, HighlightId) {
    match kind {
        Kind::Class => ("symbol-class", HighlightId::TYPE),
        Kind::Struct => ("symbol-struct", HighlightId::TYPE),
        Kind::Interface => ("symbol-interface", HighlightId::TYPE_BUILTIN),
        Kind::Enum => ("symbol-enum", HighlightId::TYPE),
        Kind::TypeParameter => ("symbol-parameter", HighlightId::TYPE),
        Kind::Method | Kind::Constructor => ("symbol-method", HighlightId::FUNCTION_METHOD),
        Kind::Function => ("symbol-method", HighlightId::FUNCTION),
        Kind::Event => ("symbol-event", HighlightId::FUNCTION),
        Kind::Operator => ("symbol-operator", HighlightId::OPERATOR),
        Kind::Field => ("symbol-field", HighlightId::PROPERTY),
        Kind::Property => ("symbol-property", HighlightId::PROPERTY),
        Kind::Variable => ("symbol-variable", HighlightId::VARIABLE),
        Kind::Key => ("symbol-key", HighlightId::PROPERTY),
        Kind::Constant | Kind::Boolean | Kind::Null => ("symbol-constant", HighlightId::CONSTANT),
        Kind::EnumMember => ("symbol-enum-member", HighlightId::CONSTANT),
        Kind::Module | Kind::Namespace | Kind::Package => ("symbol-namespace", HighlightId::MODULE),
        Kind::File => ("symbol-file", HighlightId::MODULE),
        Kind::String => ("symbol-string", HighlightId::STRING),
        Kind::Number => ("symbol-number", HighlightId::NUMBER),
        Kind::Array | Kind::Object => ("symbol-array", HighlightId::TYPE),
        Kind::Heading(_) => ("symbol-misc", HighlightId::KEYWORD),
    }
}

fn vertical_scrollbar(
    list: UiRect,
    content_h: f32,
    scroll: f32,
    max_scroll: f32,
    frame: &Frame<'_>,
    commands: &Commands,
) -> Element {
    let track = UiRect::new(
        list.right - scrollbar::SIZE,
        list.top,
        list.right,
        list.bottom,
    );
    let thumb = (track.height() * list.height() / content_h)
        .max(scrollbar::MIN_THUMB_LENGTH)
        .min(track.height());
    let travel = (track.height() - thumb).max(0.0);
    let start = track.top + travel * (scroll / max_scroll);
    let grab = frame.scrollbar_grab.unwrap_or(thumb / 2.0);
    let to_scroll = move |pointer_y: f32, grab: f32| {
        if travel <= 0.0 {
            0.0
        } else {
            ((pointer_y - track.top - grab).clamp(0.0, travel) / travel) * max_scroll
        }
    };

    let (thumb_down, thumb_drag, thumb_up) = (commands.clone(), commands.clone(), commands.clone());
    let thumb_hit = scrollbar::thumb(
        UiRect::new(
            track.left + scrollbar::INSET,
            start,
            track.right - scrollbar::INSET,
            start + thumb,
        ),
        0xb8,
    )
    .event_policy(EventPolicy::INTERACTIVE)
    .on_pointer_down_with_button(move |cx, pointer, button| {
        if button != PointerButton::Left {
            return;
        }
        thumb_down.dispatch(Command::ScrollbarGrab(pointer.point.y - start));
        cx.stop_propagation();
    })
    .on_pointer_drag(move |cx, pointer| {
        thumb_drag.dispatch(Command::Scroll(to_scroll(pointer.point.y, grab)));
        cx.stop_propagation();
    })
    .on_pointer_up(move |_cx, _pointer| thumb_up.dispatch(Command::ScrollbarRelease));

    thumb_hit
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::outline::{Item, Kind};
    use std::collections::HashSet;

    fn item(name: &str, at: usize, children: Vec<Item>) -> Item {
        Item {
            name: name.to_owned(),
            detail: None,
            kind: Kind::Function,
            deprecated: false,
            full: at..at + 1,
            select: at..at,
            children,
        }
    }

    // Rows: 0 a, 1 b, 2 c, 3 d, 4 e, 5 f (a > b > c, d; e > f)
    fn rows() -> Vec<Row> {
        let outline = Outline::new(vec![
            item(
                "a",
                0,
                vec![
                    item("b", 1, vec![item("c", 2, vec![])]),
                    item("d", 3, vec![]),
                ],
            ),
            item("e", 4, vec![item("f", 5, vec![])]),
        ]);
        outline.rows(&HashSet::new(), false)
    }

    #[test]
    fn nothing_is_pinned_at_the_top() {
        assert!(sticky(&rows(), 0.0).is_empty());
    }

    #[test]
    fn scrolled_branches_pin_at_the_slot_of_their_depth() {
        // Scrolled one row: a is above the list and b has reached its slot.
        let pinned = sticky(&rows(), ROW_H);
        assert_eq!(pinned, [(0, 0.0), (1, ROW_H)]);
    }

    #[test]
    fn a_branch_is_pushed_out_by_the_row_after_its_subtree() {
        // Row 3 (d) at the top: a pinned; b's subtree ended, so b is not pinned.
        let pinned = sticky(&rows(), 3.0 * ROW_H);
        assert_eq!(pinned, [(0, 0.0)]);
        // Row 4 (e) is 8px from the top: a, whose subtree ends at row 4, slides up.
        let pinned = sticky(&rows(), 4.0 * ROW_H - 8.0);
        assert_eq!(pinned, [(0, 8.0 - ROW_H)]);
    }

    #[test]
    fn collapsed_branches_are_never_pinned() {
        let outline = Outline::new(vec![item("a", 0, vec![item("b", 1, vec![])])]);
        let rows = outline.rows(&outline.branch_keys(), false);
        assert!(sticky(&rows, 10.0).is_empty());
    }
}
