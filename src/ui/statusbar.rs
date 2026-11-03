//! Status bar: cursor position, indent mode, encoding, and language badge,
//! grouped at the right edge. Panel toggles live in the title bar.
//!
//! Items are laid out from measured text widths so every gap is uniform, and
//! each text rect carries a small right margin so glyphs are never clipped by
//! the rect bounds.
//!
//! With no active file there is no cursor position or language to report, so
//! only the universal editor settings (indent mode, encoding) remain.

use lgui::core::{SemanticRole, Semantics};
use lgui::prelude::{Element, TextStyle, UiRect, VisualStyle, panel, text};

use crate::ui::chrome::{StatusBinding, StatusCommand, StatusFrame};
use crate::ui::theme;

/// Padding between the bar's right edge and the last item.
const EDGE_PAD: f32 = 12.0;
/// Uniform gap between adjacent items.
const GAP: f32 = 10.0;
/// Extra right margin inside each text rect so the last glyph is not clipped.
const TEXT_MARGIN: f32 = 6.0;

/// Natural width of `s` in the monospace status bar font at `weight`,
/// measured with the renderer's text system so it matches the rendered
/// pixels. Measuring with the default sans font underestimated monospace
/// labels, and a too-narrow rect wraps text such as `feat/keymap` at the `/`.
fn measure(s: &str, weight: i32) -> f32 {
    super::tabs::measure(s, theme::SMALL, weight)
}

/// The native file-status items in order.
struct Items {
    items: Vec<(String, TextStyle)>,
    cursor: Option<usize>,
    encoding: Option<usize>,
    ending: Option<usize>,
    indent: Option<usize>,
    /// The language badge, which opens the highlight panel.
    badge: Option<usize>,
}

impl Items {
    fn width_of(&self, index: usize) -> f32 {
        let (label, style) = &self.items[index];
        measure(label, style.weight)
    }

    fn total(&self) -> f32 {
        (0..self.items.len()).map(|i| self.width_of(i)).sum::<f32>()
            + GAP * self.items.len().saturating_sub(1) as f32
    }

    fn left(&self, rect: UiRect) -> f32 {
        rect.right - EDGE_PAD - self.total()
    }
}

pub fn editing_anchor(rect: UiRect, frame: &StatusFrame, ending: bool) -> Option<UiRect> {
    let items = status_items(frame);
    let index = if ending { items.ending? } else { items.indent? };
    let x = items.left(rect) + (0..index).map(|i| items.width_of(i) + GAP).sum::<f32>();
    Some(UiRect::new(
        x,
        rect.top,
        x + items.width_of(index),
        rect.bottom,
    ))
}

/// The encoding label's measured bounds, used to anchor its menu above it.
pub fn encoding_anchor(rect: UiRect, frame: &StatusFrame) -> Option<UiRect> {
    let items = status_items(frame);
    let index = items.encoding?;
    let mut x = items.left(rect);
    for preceding in 0..index {
        x += items.width_of(preceding) + GAP;
    }
    Some(UiRect::new(
        x,
        rect.top,
        x + items.width_of(index),
        rect.bottom,
    ))
}

pub fn render(rect: UiRect, state: StatusBinding) -> Element {
    let s = state.frame();
    let items = status_items(&s);
    use lgui::core::{
        Axis, Dimension, DockSide, EdgeInsets, FlexItem, FlexLayout, Justify, LayoutSpec, Overflow,
        SizeConstraints,
    };
    let height = rect.height().max(0.0);
    let row = |width| UiRect::new(0.0, 0.0, width, height);
    let vim_width = s.vim.as_ref().map_or(0.0, |(mode, status)| {
        let labels = vim_status_items(*mode, status.clone());
        labels
            .iter()
            .map(|(label, style)| measure(label, style.weight) + TEXT_MARGIN)
            .sum::<f32>()
            + GAP * labels.len().saturating_sub(1) as f32
    });
    let available =
        (rect.width() - EDGE_PAD * 2.0 - vim_width - if vim_width > 0.0 { GAP } else { 0.0 })
            .max(0.0);
    let mut settings = lgui::core::group(row(items.total()))
        .layout(LayoutSpec::Flex(FlexLayout {
            gap: GAP,
            ..Default::default()
        }))
        .size_constraints(
            SizeConstraints::new(Dimension::Content, Dimension::Points(height))
                .max_size(lgui::core::Size::new(available, height)),
        )
        .overflow(Overflow::Clip)
        .dock(DockSide::Right);
    for index in 0..items.items.len() {
        let width = items.width_of(index);
        let (label, style) = items.items[index].clone();
        let mut slot =
            lgui::core::group(row(width)).child(text(row(width + TEXT_MARGIN), label, style));
        if items.cursor == Some(index) {
            slot = slot.public_id("statusbar.cursor-position");
        }
        let command = if items.encoding == Some(index) {
            Some((
                "statusbar.encoding",
                "Change file encoding",
                StatusCommand::Encoding,
            ))
        } else if items.badge == Some(index) {
            Some((
                "statusbar.highlighting",
                "Highlighting",
                StatusCommand::HighlightMenu,
            ))
        } else if items.ending == Some(index) {
            Some((
                "statusbar.line-ending",
                "Change line ending",
                StatusCommand::LineEnding,
            ))
        } else if items.indent == Some(index) {
            Some((
                "statusbar.indentation",
                "Change indentation",
                StatusCommand::Indent,
            ))
        } else {
            None
        };
        if let Some((id, name, command)) = command {
            use lgui::core::{
                InteractionRole, NodeDescription, NodeProperties, SemanticAction, UiEventPayload,
                UiNodeKind,
            };
            let (label, style) = items.items[index].clone();
            let mut properties = NodeProperties::new(UiNodeKind::Group, row(width));
            properties.public_id = Some(id.into());
            properties.interaction = InteractionRole::Button;
            properties.semantics = Some(
                Semantics::new(SemanticRole::Button)
                    .name(name)
                    .value(label.clone())
                    .action(SemanticAction::Click),
            );
            let mut label_properties =
                NodeProperties::new(UiNodeKind::Text, row(width + TEXT_MARGIN));
            label_properties.text = Some(label);
            label_properties.text_style = Some(style);
            let description = NodeDescription::new("item", properties)
                .child(NodeDescription::new("label", label_properties));
            let state = state.clone();
            slot = lgui::core::described_with_events(
                row(width),
                format!("statusbar.item.{index}"),
                vec![description],
                Some(std::sync::Arc::new(move |event| {
                    if event.node.as_ref() == id && matches!(event.payload, UiEventPayload::Click) {
                        state.run(command.clone());
                    }
                })),
            )
            .cursor(lgui::core::CursorIcon::Pointer);
        }
        settings = settings.child(slot.flex_item(FlexItem {
            shrink: 0.0,
            ..Default::default()
        }));
    }
    let mut vim_nodes = Vec::new();
    if let Some((mode, status)) = s.vim {
        for (index, (label, style)) in vim_status_items(mode, status).into_iter().enumerate() {
            let width = measure(&label, style.weight) + TEXT_MARGIN;
            let mut properties =
                lgui::core::NodeProperties::new(lgui::core::UiNodeKind::Text, row(width));
            properties.text = Some(label);
            properties.text_style = Some(style);
            vim_nodes.push(lgui::core::NodeDescription::new(
                format!("item.{index}"),
                properties,
            ));
        }
    }
    let vim = lgui::core::described(row(0.0), "statusbar.vim", vim_nodes)
        .layout(LayoutSpec::Flex(FlexLayout {
            gap: GAP,
            ..Default::default()
        }))
        .size_constraints(SizeConstraints::new(
            Dimension::Content,
            Dimension::Points(height),
        ))
        .dock(DockSide::Left);
    // COMPAT(legacy-ui-mount): Render existing extensions using statusbar.extensions.
    // New extensions place their roots through the generic layer and public targets.
    // Remove this mount and its Legacy filter when legacy mount support ends;
    // no removal version has been scheduled.
    let mount = lgui::core::group(row(0.0))
        .public_id("statusbar.extensions")
        .layout(LayoutSpec::Flex(FlexLayout {
            axis: Axis::Horizontal,
            justify: Justify::End,
            ..Default::default()
        }))
        .dock(DockSide::Fill)
        .overflow(Overflow::Clip)
        .children(
            s.extension_widgets
                .into_iter()
                .filter(|node| {
                    node.attachment == crate::features::extensions::ui::Attachment::Legacy
                })
                .map(|node| {
                    let state = state.clone();
                    crate::ui::extension_widgets::render(
                        node,
                        std::sync::Arc::new(move |origin, payload| {
                            state.run(StatusCommand::ExtensionWidget { origin, payload })
                        }),
                    )
                }),
        );
    panel(rect, VisualStyle::filled(theme::c().sidebar))
        .key("statusbar")
        .public_id("statusbar")
        .child(
            lgui::core::group(rect)
                .overflow(Overflow::Clip)
                .layout(LayoutSpec::Dock {
                    padding: EdgeInsets {
                        left: EDGE_PAD,
                        right: EDGE_PAD,
                        top: 0.0,
                        bottom: 0.0,
                    },
                    gap: Dimension::Points(GAP),
                })
                .child(settings)
                .child(vim)
                .child(mount),
        )
        .child(crate::ui::components::divider::top(rect))
}

fn status_items(s: &StatusFrame) -> Items {
    let mut items = Vec::new();
    let mut cursor = None;
    let mut encoding = None;
    let mut ending = None;
    let mut indent = None;
    let mut badge = None;
    if !s.pending.is_empty() {
        items.push((
            format!(
                "{} …",
                crate::features::keymap::keystroke::Keystroke::sequence_label(&s.pending)
            ),
            theme::mono_bold(theme::c().accent, theme::SMALL),
        ));
    }
    for note in &s.language {
        let style = match note.level {
            crate::ui::chrome::NoteLevel::Quiet => theme::mono(theme::c().text_dim, theme::SMALL),
            crate::ui::chrome::NoteLevel::Warning => {
                theme::mono_bold(theme::c().warning, theme::SMALL)
            }
            crate::ui::chrome::NoteLevel::Error => theme::mono_bold(theme::c().error, theme::SMALL),
        };
        items.push((note.text.clone(), style));
    }
    if let Some(file) = &s.file {
        if file.missing {
            items.push((
                "Deleted on Disk — Save Recreates".into(),
                theme::mono_bold(theme::c().error, theme::SMALL),
            ));
        } else if file.conflict {
            items.push((
                "Changed on Disk".into(),
                theme::mono_bold(theme::c().warning, theme::SMALL),
            ));
        }
        if let Some((line, col)) = file.cursor {
            cursor = Some(items.len());
            items.push((
                format!("Ln {}, Col {}", line + 1, col + 1),
                theme::mono(theme::c().text_muted, theme::SMALL),
            ));
            indent = Some(items.len());
            items.push((
                file.indent.clone(),
                theme::mono(theme::c().text_dim, theme::SMALL),
            ));
            ending = Some(items.len());
            items.push((
                file.line_ending.clone(),
                theme::mono(theme::c().text_dim, theme::SMALL),
            ));
            encoding = Some(items.len());
            items.push((
                file.encoding.clone(),
                theme::mono(theme::c().text_dim, theme::SMALL),
            ));
        } else if file.is_diff {
            items.push((
                "Text Diff".into(),
                theme::mono(theme::c().text_dim, theme::SMALL),
            ));
        }
        if file.is_file || file.is_diff {
            badge = Some(items.len());
            items.push((
                file.lang.badge().into(),
                theme::mono_bold(file.lang.badge_color(), theme::SMALL),
            ));
        }
    } else {
        items.push((
            "Spaces: 2".into(),
            theme::mono(theme::c().text_dim, theme::SMALL),
        ));
        items.push((
            "UTF-8".into(),
            theme::mono(theme::c().text_dim, theme::SMALL),
        ));
    }
    Items {
        items,
        cursor,
        encoding,
        ending,
        indent,
        badge,
    }
}

/// Vim's mode, then its command line, message or pending keys, and macro
/// recording, left to right.
fn vim_status_items(
    mode: loom_editor_core::vim::Mode,
    status: loom_editor_core::vim::Status,
) -> Vec<(String, TextStyle)> {
    use loom_editor_core::vim::Mode;
    let mode_color = match mode {
        Mode::Normal => theme::c().text_muted,
        Mode::Insert => theme::c().accent,
        Mode::Replace => theme::c().error,
        Mode::Visual(_) => theme::c().warning,
    };
    let mut items = vec![(
        format!("-- {} --", status.mode),
        theme::mono_bold(mode_color, theme::SMALL),
    )];
    if let Some(line) = status.command_line {
        items.push((line, theme::mono(theme::c().text_bright, theme::SMALL)));
    } else if let Some(message) = status.message {
        let color = if message.error {
            theme::c().error
        } else {
            theme::c().text_soft
        };
        items.push((message.text, theme::mono(color, theme::SMALL)));
    }
    if !status.pending.is_empty() {
        items.push((
            status.pending,
            theme::mono_bold(theme::c().accent, theme::SMALL),
        ));
    }
    if let Some(register) = status.recording {
        items.push((
            format!("recording @{register}"),
            theme::mono(theme::c().warning, theme::SMALL),
        ));
    }
    items
}

#[cfg(test)]
fn vim_items(vim: &loom_editor_core::vim::Vim) -> Vec<(String, TextStyle)> {
    vim_status_items(vim.mode(), vim.status())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::chrome::{FileStatus, NoteLevel, StatusNote};
    use lgui::{
        application::{AppView, ApplicationContext},
        core::{InputEvent, Point, PointerButton, PointerData, UiScale},
        session::UiSession,
    };
    use std::sync::{Arc, Mutex};

    fn click(session: &mut UiSession, context: &ApplicationContext, x: f32, y: f32) {
        let pointer = PointerData::mouse(Point::new(x, y));
        for input in [
            InputEvent::PointerDown {
                pointer,
                button: PointerButton::Left,
            },
            InputEvent::PointerUp {
                pointer,
                button: PointerButton::Left,
            },
        ] {
            let output = session.handle_input(input);
            lgui::core::dispatch_runtime_output(
                output,
                context,
                &lgui::window::WindowId::new("statusbar-hit-test"),
                |action| session.handle_default_action(action),
                |_| {},
            );
        }
    }

    #[test]
    fn native_file_controls_keep_their_click_targets() {
        for width in [500.0, 1000.0] {
            let rect = UiRect::new(0.0, 0.0, width, 28.0);
            let lang = crate::domain::language::builtin("rust");
            use crate::features::extensions::ui::{
                Attachment, Kind, Node, Origin, Properties, Relation,
            };
            let widgets: Vec<_> = ["go", "rust", "typescript"]
                .into_iter()
                .map(|owner| {
                    Arc::new(Node {
                        origin: Origin {
                            owner: owner.into(),
                            session: 1,
                            generation: 1,
                            node: 1,
                            id: "status".into(),
                        },
                        attachment: Attachment::Relative {
                            target: "statusbar.cursor-position".into(),
                            relation: Relation::Before,
                            order: 100,
                        },
                        properties: Properties {
                            id: "status".into(),
                            kind: Kind::Block,
                            text: None,
                            icon: None,
                            foreground: None,
                            background: Some(0x246842),
                            width: Some(220.0),
                            height: Some(28.0),
                            padding: 0.0,
                            gap: 0.0,
                            visible: true,
                            label: Some(owner.into()),
                        },
                        children: vec![],
                    })
                })
                .collect();
            let frame = StatusFrame {
                extension_widgets: widgets.clone(),
                pending: Vec::new(),
                vim: None,
                file: Some(FileStatus {
                    indent: "Spaces: 2".into(),
                    line_ending: "LF".into(),
                    encoding: "UTF-8".into(),
                    lang,
                    missing: false,
                    conflict: false,
                    cursor: Some((0, 0)),
                    is_diff: false,
                    is_file: true,
                }),
                language: vec![StatusNote {
                    text: "2 diagnostics".into(),
                    level: NoteLevel::Warning,
                }],
            };
            let commands = Arc::new(Mutex::new(Vec::new()));
            let seen = commands.clone();
            let snapshot = frame.clone();
            let binding = StatusBinding::new(
                move || snapshot.clone(),
                move |command| {
                    seen.lock().unwrap().push(match command {
                        StatusCommand::ExtensionWidget { .. } => "extension",
                        StatusCommand::HighlightMenu => "highlighting",
                        StatusCommand::Encoding => "encoding",
                        StatusCommand::LineEnding => "ending",
                        StatusCommand::Indent => "indent",
                    });
                },
            );
            let roots = widgets.clone();
            let view: AppView = Arc::new(move |_| {
                lgui::core::group(rect)
                    .child(render(rect, binding.clone()))
                    .child(crate::ui::extension_widgets::layer(
                        rect,
                        roots.clone(),
                        Arc::new(|_, _| {}),
                    ))
            });
            let context = ApplicationContext::empty(Default::default());
            let _fonts = lgui::backend::install_text_environment(
                &context,
                lgui::render_skia::skia_text_system_handle(),
            );
            let mut session = UiSession::new();
            session.render_view(&view, rect, UiScale::ONE);
            let cursor = session
                .tree()
                .public_node("statusbar.cursor-position")
                .unwrap()
                .bounds;
            for widget in widgets {
                let bounds = session
                    .tree()
                    .public_node(&widget.origin.public_id())
                    .unwrap()
                    .bounds;
                assert!(
                    bounds.right <= cursor.left,
                    "extension overlapped cursor at window width {width}"
                );
                assert!(
                    bounds.left >= 0.0,
                    "extension escaped the status bar at window width {width}"
                );
            }
            for label in ["2 diagnostics", "Ln 1, Col 1"] {
                let bounds = session
                    .tree()
                    .nodes()
                    .iter()
                    .find(|node| node.text.as_deref() == Some(label))
                    .unwrap()
                    .paint_bounds;
                click(
                    &mut session,
                    &context,
                    (bounds.left + bounds.right) / 2.0,
                    14.0,
                );
                assert!(
                    commands.lock().unwrap().is_empty(),
                    "{label} opened a panel"
                );
            }
            // The language badge opens the highlight panel.
            let badge = session
                .tree()
                .nodes()
                .iter()
                .find(|node| node.text.as_deref() == Some(lang.badge()))
                .unwrap()
                .paint_bounds;
            let badge_point = Point::new((badge.left + badge.right) / 2.0, 14.0);
            assert_eq!(
                session.tree().hit_test(badge_point).map(|hit| hit.id),
                session
                    .tree()
                    .nodes()
                    .iter()
                    .find(|node| node.public_id.as_deref() == Some("statusbar.highlighting"))
                    .map(|node| node.id.clone()),
                "badge bounds: {badge:?}; public node: {:?}",
                session.tree().public_node("statusbar.highlighting")
            );
            click(
                &mut session,
                &context,
                (badge.left + badge.right) / 2.0,
                14.0,
            );
            assert_eq!(
                commands.lock().unwrap().drain(..).collect::<Vec<_>>(),
                ["highlighting"]
            );
            for (ending, expected) in [(true, "ending"), (false, "indent")] {
                let bounds = editing_anchor(rect, &frame, ending).unwrap();
                click(
                    &mut session,
                    &context,
                    (bounds.left + bounds.right) / 2.0,
                    14.0,
                );
                assert_eq!(*commands.lock().unwrap(), [expected]);
                commands.lock().unwrap().clear();
            }
            let bounds = session
                .tree()
                .nodes()
                .iter()
                .find(|node| node.text.as_deref() == Some("UTF-8"))
                .unwrap()
                .paint_bounds;
            click(
                &mut session,
                &context,
                (bounds.left + bounds.right) / 2.0,
                14.0,
            );
            assert_eq!(*commands.lock().unwrap(), ["encoding"]);
            commands.lock().unwrap().clear();
            click(&mut session, &context, 2.0, 14.0);
            assert!(commands.lock().unwrap().is_empty());
        }
    }
}
