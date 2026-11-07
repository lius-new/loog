//! Status bar: cursor position, indent mode, encoding, and language badge,
//! grouped at the right edge. Panel toggles live in the title bar.
//!
//! Items are laid out from measured text widths so every gap is uniform, and
//! each text rect carries a small right margin so glyphs are never clipped by
//! the rect bounds.
//!
//! With no active file there is no cursor position or language to report, so
//! only the universal editor settings (indent mode, encoding) remain.

use lgui::core::{CursorIcon, EventPolicy, SemanticRole, Semantics};
use lgui::prelude::{Element, TextStyle, UiRect, VisualStyle, panel, text};

use crate::features::language_services_menu::State as ServiceState;
use crate::ui::chrome::{StatusBinding, StatusCommand, StatusFrame};
use crate::ui::theme;

/// Padding between the bar's right edge and the last item.
const EDGE_PAD: f32 = 12.0;
/// Uniform gap between adjacent items.
const GAP: f32 = 10.0;
/// Extra right margin inside each text rect so the last glyph is not clipped.
const TEXT_MARGIN: f32 = 6.0;
/// The room of the language services icon, and the gap after it.
const SERVICE_ICON: f32 = 12.0;
const SERVICE_ICON_ROOM: f32 = SERVICE_ICON + 6.0;

/// The problem counts' icon, and the gap between it and its number.
const PROBLEM_ICON: f32 = 14.0;
const PROBLEM_ICON_GAP: f32 = 4.0;

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
    /// How the language services are doing: an icon and a phrase, which open their panel.
    service: Option<(usize, ServiceState)>,
}

impl Items {
    fn width_of(&self, index: usize) -> f32 {
        let (label, style) = &self.items[index];
        let icon = match self.service {
            Some((at, _)) if at == index => SERVICE_ICON_ROOM,
            _ => 0.0,
        };
        icon + measure(label, style.weight)
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
    let problems = s.problems.map(problem_entries);
    let has_problems = problems.is_some();
    let problems_width = problems.as_ref().map_or(0.0, |entries| {
        entries
            .iter()
            .map(|entry| problem_width(entry.2))
            .sum::<f32>()
            + GAP * (entries.len() - 1) as f32
    });
    let available = (rect.width()
        - EDGE_PAD * 2.0
        - vim_width
        - if vim_width > 0.0 { GAP } else { 0.0 }
        - problems_width
        - if problems_width > 0.0 { GAP } else { 0.0 })
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
        if let Some((at, service)) = items.service
            && at == index
        {
            settings = settings.child(
                service_entry(height, width, label, style, service, state.clone()).flex_item(
                    FlexItem {
                        shrink: 0.0,
                        ..Default::default()
                    },
                ),
            );
            continue;
        }
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
    let mut problem_group = lgui::core::group(row(problems_width))
        .public_id("statusbar.problems")
        .layout(LayoutSpec::Flex(FlexLayout {
            gap: GAP,
            ..Default::default()
        }))
        .size_constraints(SizeConstraints::new(
            Dimension::Content,
            Dimension::Points(height),
        ))
        .dock(DockSide::Left);
    for (name, key, count, icon, color) in problems.into_iter().flatten() {
        problem_group = problem_group.child(
            problem_entry(height, name, key, count, icon, color, state.clone()).flex_item(
                FlexItem {
                    shrink: 0.0,
                    ..Default::default()
                },
            ),
        );
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
                .children(has_problems.then_some(problem_group))
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
    let mut service = None;
    // The first item: how the language services do for this file, in words every
    // service shares.
    if let Some(state) = s.language_services {
        let palette = theme::c();
        let style = match state {
            ServiceState::Failed => theme::mono_bold(palette.error, theme::SMALL),
            ServiceState::Starting | ServiceState::Working => {
                theme::mono(palette.text_muted, theme::SMALL)
            }
            ServiceState::Ready => theme::mono(palette.text_dim, theme::SMALL),
            ServiceState::Idle | ServiceState::Stopped => {
                theme::mono(palette.text_faint, theme::SMALL)
            }
        };
        service = Some((items.len(), state));
        items.push((format!("Language Services: {}", state.label()), style));
    }
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
        service,
    }
}

// Material Design Icons (Pictogrammers), Apache-2.0: a quarter arc that turns while a service
// prepares or works, and a circle with a mark for each other state.
const LOADING_ICON: &str = "svg-path:24x24:M12,4V2A10,10 0 0,0 2,12H4A8,8 0 0,1 12,4Z";
const READY_ICON: &str = "svg-path:24x24:M12 2C6.5 2 2 6.5 2 12S6.5 22 12 22 22 17.5 22 12 17.5 2 12 2M10 17L5 12L6.41 10.59L10 14.17L17.59 6.58L19 8L10 17Z";
const ERROR_ICON: &str = "svg-path:24x24:M13,13H11V7H13M13,17H11V15H13M12,2A10,10 0 0,0 2,12A10,10 0 0,0 12,22A10,10 0 0,0 22,12A10,10 0 0,0 12,2Z";
const STOPPED_ICON: &str = "svg-path:24x24:M12,2A10,10 0 0,0 2,12A10,10 0 0,0 12,22A10,10 0 0,0 22,12A10,10 0 0,0 12,2M12,4C16.41,4 20,7.59 20,12C20,16.41 16.41,20 12,20C7.59,20 4,16.41 4,12C4,7.59 7.59,4 12,4M9,9V15H15V9";

/// The icon of a state that is not turning.
fn service_icon(state: ServiceState) -> &'static str {
    match state {
        ServiceState::Ready => READY_ICON,
        ServiceState::Failed => ERROR_ICON,
        ServiceState::Idle | ServiceState::Stopped => STOPPED_ICON,
        ServiceState::Starting | ServiceState::Working => LOADING_ICON,
    }
}

/// The color of a service state's icon.
fn service_color(state: ServiceState) -> lgui::prelude::Color {
    let palette = theme::c();
    match state {
        ServiceState::Ready => palette.git.added,
        ServiceState::Working => palette.accent,
        ServiceState::Starting => palette.warning,
        ServiceState::Failed => palette.error,
        ServiceState::Idle | ServiceState::Stopped => palette.text_faint,
    }
}

/// The language services item: the state's icon, its words, and a click that opens the panel.
fn service_entry(
    height: f32,
    width: f32,
    label: String,
    style: TextStyle,
    state: ServiceState,
    binding: StatusBinding,
) -> Element {
    let color = service_color(state);
    let middle = height / 2.0;
    let box_ = UiRect::new(
        0.0,
        middle - SERVICE_ICON / 2.0,
        SERVICE_ICON,
        middle + SERVICE_ICON / 2.0,
    );
    let icon = if matches!(state, ServiceState::Starting | ServiceState::Working) {
        crate::ui::components::loading_indicator::render_icon(
            box_,
            color,
            "statusbar-language-services".into(),
            LOADING_ICON,
        )
    } else {
        crate::ui::tabs::icon(
            lgui::core::UiId::new("statusbar-language-services-icon"),
            service_icon(state),
            box_,
            color,
        )
    };
    panel(UiRect::new(0.0, 0.0, width, height), VisualStyle::default())
        .public_id("statusbar.language-services")
        .event_policy(EventPolicy::INTERACTIVE)
        .cursor(CursorIcon::Pointer)
        .semantics(
            Semantics::new(SemanticRole::Button)
                .name("Language services")
                .value(label.clone()),
        )
        .on_click(move || binding.run(StatusCommand::LanguageServices))
        .child(icon)
        .child(text(
            UiRect::new(SERVICE_ICON_ROOM, 0.0, width + TEXT_MARGIN, height),
            label,
            style,
        ))
}

/// Errors, warnings and information, left to right: name, node id, count, icon
/// key and color. A severity with none found is drawn quietly.
fn problem_entries(
    counts: crate::ui::chrome::ProblemCounts,
) -> [(
    &'static str,
    &'static str,
    usize,
    &'static str,
    lgui::prelude::Color,
); 3] {
    let palette = theme::c();
    let tint = |count: usize, color| if count > 0 { color } else { palette.text_faint };
    [
        (
            "Errors",
            "statusbar-problems-errors",
            counts.errors,
            "problem-error",
            tint(counts.errors, palette.error_text),
        ),
        (
            "Warnings",
            "statusbar-problems-warnings",
            counts.warnings,
            "warning",
            tint(counts.warnings, palette.warning),
        ),
        (
            "Information",
            "statusbar-problems-infos",
            counts.infos,
            "info",
            tint(counts.infos, palette.accent),
        ),
    ]
}

fn problem_width(count: usize) -> f32 {
    PROBLEM_ICON + PROBLEM_ICON_GAP + measure(&count.to_string(), 400)
}

/// One severity: its icon and number, which show or hide the Problems panel.
fn problem_entry(
    height: f32,
    name: &'static str,
    key: &'static str,
    count: usize,
    icon: &'static str,
    color: lgui::prelude::Color,
    binding: StatusBinding,
) -> Element {
    use lgui::core::{IconStyle, UiElement};
    let width = problem_width(count);
    let middle = height / 2.0;
    let icon_box = UiRect::new(
        0.0,
        middle - PROBLEM_ICON / 2.0,
        PROBLEM_ICON,
        middle + PROBLEM_ICON / 2.0,
    );
    let value = count.to_string();
    panel(UiRect::new(0.0, 0.0, width, height), VisualStyle::default())
        .key(key)
        .public_id(key)
        .event_policy(EventPolicy::INTERACTIVE)
        .cursor(CursorIcon::Pointer)
        .semantics(Semantics::new(SemanticRole::Button).name(format!("{name}: {count}")))
        .on_click(move || binding.run(StatusCommand::Problems))
        .child(
            Element::new(move |cx| {
                UiElement::icon(cx.id, icon_box, icon).icon_style(IconStyle::new(color))
            })
            .key(format!("{key}-icon")),
        )
        .child(text(
            UiRect::new(
                PROBLEM_ICON + PROBLEM_ICON_GAP,
                0.0,
                width + TEXT_MARGIN,
                height,
            ),
            value,
            theme::mono(color, theme::SMALL),
        ))
}

/// Vim's mode, then its command line, message or pending keys, and macro
/// recording, left to right.
fn vim_status_items(
    mode: loog_editor_core::vim::Mode,
    status: loog_editor_core::vim::Status,
) -> Vec<(String, TextStyle)> {
    use loog_editor_core::vim::Mode;
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
fn vim_items(vim: &loog_editor_core::vim::Vim) -> Vec<(String, TextStyle)> {
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
                        motion: Default::default(),
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
                language_services: None,
                problems: None,
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
                        StatusCommand::LanguageServices => "language services",
                        StatusCommand::Problems => "problems",
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

    /// The block a language client draws: a white mark with an icon, then a solid
    /// state block holding spacers and the text. It must be as wide as its text.
    #[test]
    fn an_extension_block_is_as_wide_as_its_text() {
        use crate::features::extensions::ui::{
            Attachment, Kind, Node, Origin, Properties, Relation,
        };
        let build = |id: &'static str,
                     kind: Kind,
                     edit: &dyn Fn(&mut Properties),
                     children: Vec<Arc<Node>>,
                     attachment: Attachment| {
            let mut properties = Properties {
                id: id.into(),
                kind,
                text: None,
                icon: None,
                foreground: None,
                background: None,
                width: None,
                height: Some(24.0),
                padding: 0.0,
                gap: 0.0,
                visible: true,
                label: None,
            };
            edit(&mut properties);
            Arc::new(Node {
                origin: Origin {
                    owner: "probe".into(),
                    session: 1,
                    generation: 1,
                    node: 1,
                    id: id.into(),
                },
                attachment,
                properties,
                motion: Default::default(),
                children,
            })
        };
        let spacer = |id: &'static str, width: f32| {
            build(
                id,
                Kind::Text,
                &|p| {
                    p.text = Some(String::new());
                    p.width = Some(width);
                },
                vec![],
                Attachment::Legacy,
            )
        };
        let icon = build(
            "icon",
            Kind::Icon,
            &|p| {
                p.icon = Some("sync".into());
                p.foreground = Some(0x818cf8);
                p.width = Some(14.0);
                p.height = Some(14.0);
            },
            vec![],
            Attachment::Legacy,
        );
        let mark = build(
            "mark",
            Kind::Row,
            &|p| {
                p.background = Some(0xffffff);
                p.padding = 5.0;
            },
            vec![icon],
            Attachment::Legacy,
        );
        let labeled = |words: &str| {
            build(
                "text",
                Kind::Text,
                &|p| {
                    p.text = Some(words.into());
                    p.foreground = Some(0xffffff);
                },
                vec![],
                Attachment::Legacy,
            )
        };
        let make_root = |children: Vec<Arc<Node>>| {
            let state = build(
                "state",
                Kind::Row,
                &|p| p.background = Some(0x818cf8),
                children,
                Attachment::Legacy,
            );
            build(
                "root",
                Kind::Row,
                &|_| {},
                vec![mark.clone(), state],
                Attachment::Relative {
                    target: "statusbar.cursor-position".into(),
                    relation: Relation::Before,
                    order: 100,
                },
            )
        };
        let with = |words: &str| {
            make_root(vec![
                spacer("lead", 10.0),
                labeled(words),
                spacer("trail", 10.0),
            ])
        };
        let frame = StatusFrame {
            extension_widgets: Vec::new(),
            pending: Vec::new(),
            vim: None,
            file: Some(FileStatus {
                indent: "Spaces: 2".into(),
                line_ending: "LF".into(),
                encoding: "UTF-8".into(),
                lang: crate::domain::language::builtin("rust"),
                missing: false,
                conflict: false,
                cursor: Some((0, 0)),
                is_diff: false,
                is_file: true,
            }),
            // A long note on the left leaves the right side little room.
            language: vec![StatusNote {
                text: "Rust syntax is available".into(),
                level: NoteLevel::Quiet,
            }],
            language_services: None,
            problems: None,
        };
        let binding = StatusBinding::new(move || frame.clone(), |_| {});
        let context = ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        // The widths of the text and of the whole block in a bar `width` wide.
        let widths = |width: f32, words: &str| {
            let rect = UiRect::new(0.0, 0.0, width, 28.0);
            let roots = vec![with(words)];
            let binding = binding.clone();
            let view: AppView = Arc::new(move |_| {
                lgui::core::group(rect)
                    .child(render(rect, binding.clone()))
                    .child(crate::ui::extension_widgets::layer(
                        rect,
                        roots.clone(),
                        Arc::new(|_, _| {}),
                    ))
            });
            let mut session = UiSession::new();
            session.render_view(&view, rect, UiScale::ONE);
            let width_of = |id: &str| {
                let bounds = session
                    .tree()
                    .public_node(&format!("ext.5:probe.1.1.{id}"))
                    .unwrap()
                    .bounds;
                bounds.right - bounds.left
            };
            (width_of("text"), width_of("root"))
        };
        for words in [
            "Language: Ready",
            "Language: Working",
            "Language: Stopped",
            "Language: Error",
        ] {
            let (natural_text, natural_block) = widths(1000.0, words);
            // Only the shaped advance belongs to layout; paragraph paint slack
            // must not inflate the text slot or the gap to its neighbour.
            assert!(
                (natural_text - measure(words, 400)).abs() < 0.5,
                "{words}: the layout box is {natural_text} wide, the shaped text needs {}",
                measure(words, 400)
            );
            // Short of room, the block keeps the width its text needs.
            let (text, block) = widths(440.0, words);
            assert!(
                (text - natural_text).abs() < 0.5 && (block - natural_block).abs() < 0.5,
                "{words}: text {text} of {natural_text}, block {block} of {natural_block}"
            );
        }
    }

    /// The icon of the language block turns inside the real bar, among its native items.
    #[test]
    fn a_turning_icon_in_the_bar_is_drawn_whole_in_every_frame() {
        use crate::features::extensions::motion::{Animation, Ease, Motion, Prop, Track};
        use crate::features::extensions::ui::{
            Attachment, Kind, Node, Origin, Properties, Relation,
        };
        use lgui::assets::RenderResources;
        use lgui::core::PhysicalRect;
        use lgui::render_skia::SkiaSoftwareSurface;
        use lgui::renderer::{FrameInfo, FrameReason};
        use std::sync::Mutex;
        let build = |id: &'static str,
                     kind: Kind,
                     edit: &dyn Fn(&mut Properties),
                     motion: Motion,
                     children: Vec<Arc<Node>>,
                     attachment: Attachment| {
            let mut properties = Properties {
                id: id.into(),
                kind,
                text: None,
                icon: None,
                foreground: None,
                background: None,
                width: None,
                height: Some(24.0),
                padding: 0.0,
                gap: 0.0,
                visible: true,
                label: None,
            };
            edit(&mut properties);
            Arc::new(Node {
                origin: Origin {
                    owner: "probe".into(),
                    session: 1,
                    generation: 1,
                    node: 1,
                    id: id.into(),
                },
                attachment,
                properties,
                motion,
                children,
            })
        };
        let turning = Motion {
            revision: 1,
            animation: Some((
                1,
                Arc::new(Animation {
                    tracks: vec![Track {
                        prop: Prop::Rotation,
                        keys: vec![(0.0, 0.0), (1.0, 360.0)],
                        ease: Ease::Linear,
                    }],
                    duration_ms: 900,
                    delay_ms: 0,
                    iterations: None,
                    alternate: false,
                    hold: false,
                }),
            )),
            ..Default::default()
        };
        let icon = build(
            "icon",
            Kind::Icon,
            &|p| {
                p.icon = Some("sync".into());
                p.foreground = Some(0x818cf8);
                p.width = Some(14.0);
                p.height = Some(14.0);
            },
            turning,
            vec![],
            Attachment::Legacy,
        );
        let label = build(
            "text",
            Kind::Text,
            &|p| {
                p.text = Some("Language: Working".into());
                p.foreground = Some(0xffffff);
            },
            Motion::default(),
            vec![],
            Attachment::Legacy,
        );
        let space = |id, width| {
            build(
                id,
                Kind::Text,
                &|p| {
                    p.width = Some(width);
                    p.text = Some(String::new());
                },
                Motion::default(),
                vec![],
                Attachment::Legacy,
            )
        };
        let root = build(
            "root",
            Kind::Row,
            &|p| p.background = Some(0xffffff),
            Motion::default(),
            // Match the actual clients' flat 24px status row.
            vec![
                space("leading", 10.0),
                icon,
                space("gap", 6.0),
                label,
                space("trailing", 0.0),
            ],
            Attachment::Shared {
                key: "example.state.v1".into(),
                contexts: None,
                priority: 10,
                placement: Box::new(Attachment::Relative {
                    target: "statusbar.cursor-position".into(),
                    relation: Relation::Before,
                    order: 100,
                }),
            },
        );
        let (width, height) = (700_u32, 28_u32);
        let rect = UiRect::new(0.0, 0.0, width as f32, height as f32);
        let frame = StatusFrame {
            extension_widgets: Vec::new(),
            pending: Vec::new(),
            vim: None,
            file: Some(FileStatus {
                indent: "Spaces: 2".into(),
                line_ending: "LF".into(),
                encoding: "UTF-8".into(),
                lang: crate::domain::language::builtin("rust"),
                missing: false,
                conflict: false,
                cursor: Some((39, 24)),
                is_diff: false,
                is_file: true,
            }),
            language: Vec::new(),
            language_services: None,
            problems: None,
        };
        let binding = StatusBinding::new(move || frame.clone(), |_| {});
        let roots = Arc::new(Mutex::new(vec![root]));
        let read = roots.clone();
        let view: AppView = Arc::new(move |_| {
            let binding = binding.clone();
            lgui::core::component(read.lock().unwrap().clone(), move |_, roots| {
                lgui::core::group(rect).child(render(rect, binding)).child(
                    crate::ui::extension_widgets::layer(rect, roots.clone(), Arc::new(|_, _| {})),
                )
            })
        });
        let context = ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        context
            .resources()
            .provide(lgui::icons::IconRegistration(Arc::new(
                lgui::icons::SvgIconRegistry::new().with_icon("sync", icondata::VsSync),
            )));
        // Displays are scaled, and frames are not all as long as the nominal one.
        for factor in [1.0_f32, 1.25, 1.5, 2.0] {
            let scale = UiScale::new(factor);
            let (pw, ph) = (
                (width as f32 * factor).ceil() as u32,
                (height as f32 * factor).ceil() as u32,
            );
            let mut session = UiSession::new();
            let mut commit = session.render_view(&view, rect, scale);
            let viewport = PhysicalRect::new(0, 0, pw as i32, ph as i32);
            let mut incremental = SkiaSoftwareSurface::new_opaque(64 << 20);
            let mut whole = SkiaSoftwareSurface::new_opaque(64 << 20);
            let mut damage = vec![viewport];
            let (mut differing, mut shown) = (Vec::new(), Vec::new());
            let mut angles = Vec::new();
            for index in 0..130_u32 {
                if index > 0 {
                    let elapsed = [16.0, 17.0, 36.0, 16.0, 45.0][index as usize % 5];
                    let output = session.advance(elapsed);
                    if let Some(bounds) = output.dirty_bounds {
                        session.invalidations_mut().invalidate_rect(bounds);
                    }
                    // Idle animation first, then progress, then pointer activity, then idle again.
                    if (30..60).contains(&index) {
                        let mut next = (*roots.lock().unwrap()[0]).clone();
                        let mut text = (*next.children[3]).clone();
                        text.properties.text =
                            Some(format!("Language: Indexing {} files", index * 37));
                        next.children[3] = Arc::new(text);
                        *roots.lock().unwrap() = vec![Arc::new(next)];
                    }
                    if (60..90).contains(&index) {
                        let icon = session
                            .tree()
                            .public_node("ext.5:probe.1.1.icon")
                            .unwrap()
                            .bounds;
                        let point = if index % 2 == 0 {
                            lgui::core::Point::new(icon.left + 7.0, icon.top + 7.0)
                        } else {
                            lgui::core::Point::new(1.0, 1.0)
                        };
                        let pointer = session.handle_input(lgui::core::InputEvent::PointerMove(
                            lgui::core::PointerData::mouse(point),
                        ));
                        if let Some(bounds) = pointer.dirty_bounds {
                            session.invalidations_mut().invalidate_rect(bounds);
                        }
                    }
                    commit = session.render_view(&view, rect, scale);
                    let mut rects: Vec<UiRect> = commit.damage.dirty.rects().to_vec();
                    rects.extend(output.dirty_bounds);
                    damage = if commit.damage.dirty.is_full() {
                        vec![viewport]
                    } else {
                        rects
                            .iter()
                            .map(|r| {
                                PhysicalRect::new(
                                    (r.left * factor).floor() as i32,
                                    (r.top * factor).floor() as i32,
                                    (r.right * factor).ceil() as i32,
                                    (r.bottom * factor).ceil() as i32,
                                )
                            })
                            .collect()
                    };
                }
                angles.push(
                    // The icon is the only composited layer in this view.
                    session
                        .tree()
                        .nodes()
                        .iter()
                        .find_map(|node| node.compositing_layer)
                        .map(|layer| layer.transform.rotation_degrees_f32()),
                );
                let label = session
                    .tree()
                    .public_node("ext.5:probe.1.1.text")
                    .unwrap()
                    .bounds;
                let cursor = session
                    .tree()
                    .public_node("statusbar.cursor-position")
                    .unwrap()
                    .bounds;
                assert!(
                    (cursor.left - label.right - GAP).abs() < 0.1,
                    "scale {factor}, frame {index}: shared entry must use only the native gap"
                );
                let scene = commit.scene.project_to_physical(scale);
                let reference = session.tree().scene().project_to_physical(scale);
                let part = FrameInfo::new(
                    viewport,
                    &damage,
                    scale,
                    FrameReason::SceneChange,
                    index == 0,
                );
                let all = [viewport];
                let full = FrameInfo::new(viewport, &all, scale, FrameReason::SceneChange, true);
                lgui::assets::backend::with_render_resources(
                    &context,
                    RenderResources::new(),
                    || {
                        incremental.draw(&scene, &part).expect("draw");
                        whole.draw(&reference, &full).expect("draw");
                    },
                );
                differing.push(
                    incremental
                        .pixels()
                        .iter()
                        .zip(whole.pixels())
                        .filter(|(a, b)| a != b)
                        .count(),
                );
                let mark = session
                    .tree()
                    .public_node("ext.5:probe.1.1.icon")
                    .expect("the block is placed")
                    .bounds
                    .inflate(3.0, 3.0);
                let pixels = incremental.pixels();
                let (left, right) = ((mark.left * factor) as u32, (mark.right * factor) as u32);
                let (top, bottom) = ((mark.top * factor) as u32, (mark.bottom * factor) as u32);
                // How much ink the icon puts on the white mark, partial pixels included: it
                // is the same at every angle when the turned icon is resampled well.
                shown.push(
                    (top..bottom)
                        .flat_map(|y| (left..right).map(move |x| (x, y)))
                        .map(|(x, y)| 255 - pixels[((y * pw + x) * 4 + 1) as usize] as usize)
                        .sum::<usize>(),
                );
            }
            assert!(
                differing.iter().all(|count| *count == 0),
                "scale {factor}: bytes differing from a full repaint, per frame: {differing:?}"
            );
            assert!(
                angles
                    .windows(2)
                    .all(|pair| pair[0].is_some() && pair[0] != pair[1]),
                "scale {factor}: the icon stopped turning; angle per frame: {angles:?}"
            );
            let (least, most) = (shown.iter().min().unwrap(), shown.iter().max().unwrap());
            assert!(
                *least * 10 >= *most * 6,
                "scale {factor}: the icon is not whole in every frame; pixels per frame: {shown:?}"
            );
        }
    }

    /// The three severities sit at the left end of the bar, and pressing any of them
    /// asks for the Problems panel.
    #[test]
    fn the_problem_counts_show_at_the_left_and_open_the_problems_panel() {
        use crate::ui::chrome::ProblemCounts;
        let context = ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        let rect = UiRect::new(0.0, 0.0, 700.0, 28.0);
        let frame = StatusFrame {
            extension_widgets: Vec::new(),
            pending: Vec::new(),
            vim: None,
            file: None,
            language: Vec::new(),
            language_services: None,
            problems: Some(ProblemCounts {
                errors: 3,
                warnings: 12,
                infos: 0,
            }),
        };
        let commands = Arc::new(Mutex::new(Vec::new()));
        let seen = commands.clone();
        let binding = StatusBinding::new(
            move || frame.clone(),
            move |command| {
                if matches!(command, StatusCommand::Problems) {
                    seen.lock().unwrap().push("problems");
                }
            },
        );
        let view: AppView = Arc::new(move |_| render(rect, binding.clone()));
        let mut session = UiSession::new();
        session.render_view(&view, rect, UiScale::ONE);
        let mut left = 0.0_f32;
        for (id, value) in [
            ("statusbar-problems-errors", "3"),
            ("statusbar-problems-warnings", "12"),
            ("statusbar-problems-infos", "0"),
        ] {
            let bounds = session
                .tree()
                .public_node(id)
                .unwrap_or_else(|| panic!("`{id}` is not drawn"))
                .bounds;
            assert!(bounds.left >= left, "{id} is out of order");
            assert!(bounds.left < 100.0, "{id} is not at the left end");
            left = bounds.right;
            assert!(
                session
                    .tree()
                    .nodes()
                    .iter()
                    .any(|node| node.text.as_deref() == Some(value)),
                "{id} shows no `{value}`"
            );
            commands.lock().unwrap().clear();
            click(
                &mut session,
                &context,
                (bounds.left + bounds.right) / 2.0,
                14.0,
            );
            assert_eq!(*commands.lock().unwrap(), ["problems"], "{id}");
        }
    }

    /// The language services item is the first of the bar's right side, says one phrase
    /// for every service, and opens the panel when pressed.
    #[test]
    fn the_language_services_item_shows_the_shared_state_and_opens_the_panel() {
        use crate::features::language_services_menu::State;
        let context = ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        let rect = UiRect::new(0.0, 0.0, 700.0, 28.0);
        for (state, label) in [
            (State::Ready, "Language Services: Ready"),
            (State::Working, "Language Services: Working"),
            (State::Failed, "Language Services: Error"),
            (State::Idle, "Language Services: Idle"),
        ] {
            let frame = StatusFrame {
                extension_widgets: Vec::new(),
                pending: Vec::new(),
                vim: None,
                file: Some(FileStatus {
                    indent: "Spaces: 2".into(),
                    line_ending: "LF".into(),
                    encoding: "UTF-8".into(),
                    lang: crate::domain::language::builtin("rust"),
                    missing: false,
                    conflict: false,
                    cursor: Some((0, 0)),
                    is_diff: false,
                    is_file: true,
                }),
                language: Vec::new(),
                language_services: Some(state),
                problems: None,
            };
            let commands = Arc::new(Mutex::new(Vec::new()));
            let seen = commands.clone();
            let binding = StatusBinding::new(
                move || frame.clone(),
                move |command| {
                    if matches!(command, StatusCommand::LanguageServices) {
                        seen.lock().unwrap().push("language services");
                    }
                },
            );
            let view: AppView = Arc::new(move |_| render(rect, binding.clone()));
            let mut session = UiSession::new();
            session.render_view(&view, rect, UiScale::ONE);
            let text = session
                .tree()
                .nodes()
                .iter()
                .find(|node| node.text.as_deref() == Some(label))
                .unwrap_or_else(|| panic!("`{label}` is not drawn"))
                .paint_bounds;
            let cursor = session
                .tree()
                .nodes()
                .iter()
                .find(|node| node.text.as_deref() == Some("Ln 1, Col 1"))
                .unwrap()
                .paint_bounds;
            assert!(
                text.right <= cursor.left,
                "it comes before the cursor position"
            );
            click(&mut session, &context, (text.left + text.right) / 2.0, 14.0);
            assert_eq!(*commands.lock().unwrap(), ["language services"], "{label}");
        }
        // No service answers for the file: no item.
        let frame = StatusFrame {
            extension_widgets: Vec::new(),
            pending: Vec::new(),
            vim: None,
            file: None,
            language: Vec::new(),
            language_services: None,
            problems: None,
        };
        let binding = StatusBinding::new(move || frame.clone(), |_| {});
        let view: AppView = Arc::new(move |_| render(rect, binding.clone()));
        let mut session = UiSession::new();
        session.render_view(&view, rect, UiScale::ONE);
        assert!(session.tree().nodes().iter().all(|node| {
            !node
                .text
                .as_deref()
                .is_some_and(|text| text.starts_with("Language Services"))
        }));
    }
}
