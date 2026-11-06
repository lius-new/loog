//! The Output and Problems panels of the bottom dock.
//!
//! Output shows one channel of the [`LogHub`] at a time; Problems lists the
//! findings of open documents. Neither knows who wrote what: channels and
//! findings are data.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use lgui::core::{
    CursorIcon, EventPolicy, IconStyle, LogicalKey, NamedKey, SemanticRole, Semantics, UiElement, UiEventKind,
    UiEventPayload, UiFocusHandle, UiId, WheelUnit, clip,
};
use lgui::prelude::{Color, Element, ShadowStyle, TextAlign, UiRect, VisualStyle, group, panel, text};

use lgui::ApplicationHandle;

use super::context::AppContext;
use super::terminal_binding::TerminalStore;
use crate::features::language_service::diagnostics::Severity;
use crate::features::output::{self, ChannelId, ChannelInfo, Level};
use crate::features::problems::{self, FileProblems, Filter};
use crate::ui::components::input::{
    self, AccessInputOptions, InputBinding, InputState, InputStyle,
};
use crate::ui::terminal::header;
use crate::ui::terminal::state::PanelTab;
use crate::ui::{terminal::view, theme};

pub(crate) const ROW_H: f32 = 18.0;
const PAD_Y: f32 = 4.0;
/// Narrowest header that keeps every Output tool on the bar.
const FULL_TOOLS_W: f32 = 560.0;
const MENU_W: f32 = 232.0;
const MENU_PAD: f32 = 4.0;
const MENU_ITEM_H: f32 = 22.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Menu {
    Channels((f32, f32)),
    Level((f32, f32)),
    More((f32, f32)),
}

#[derive(Clone)]
pub struct LogPanel {
    pub channel: Option<ChannelId>,
    pub min_level: Level,
    pub follow: bool,
    pub show_time: bool,
    pub find_open: bool,
    pub find: InputState,
    pub scroll: f32,
    pub severities: Filter,
    pub current_only: bool,
    pub collapsed: BTreeSet<PathBuf>,
    pub problems_scroll: f32,
    pub menu: Option<Menu>,
}

impl Default for LogPanel {
    fn default() -> Self {
        Self {
            channel: None,
            min_level: Level::Info,
            follow: true,
            show_time: true,
            find_open: false,
            find: InputState::default(),
            scroll: 0.0,
            severities: Filter::default(),
            current_only: false,
            collapsed: BTreeSet::new(),
            problems_scroll: 0.0,
            menu: None,
        }
    }
}

impl LogPanel {
    /// The channel shown: the chosen one while it exists, else the first.
    pub fn shown(&self, channels: &[ChannelInfo]) -> Option<ChannelId> {
        self.channel
            .as_ref()
            .filter(|id| channels.iter().any(|c| &c.id == *id))
            .cloned()
            .or_else(|| channels.first().map(|c| c.id.clone()))
    }

    /// The text rows are filtered by while the find box is open.
    pub fn needle(&self) -> &str {
        if self.find_open { self.find.text() } else { "" }
    }
}

/// Scroll offset to draw at, and the furthest it can go.
pub(crate) fn scroll_extent(rows: usize, height: f32, offset: f32, follow: bool) -> (f32, f32) {
    let max = (rows as f32 * ROW_H + PAD_Y * 2.0 - height).max(0.0);
    (if follow { max } else { offset.clamp(0.0, max) }, max)
}

fn level_color(level: Level) -> Color {
    let c = theme::c();
    match level {
        Level::Error => c.error_text,
        Level::Warn => c.warning,
        Level::Info => c.accent,
        Level::Debug | Level::Trace => c.text_faint,
    }
}

fn severity_color(severity: Severity) -> Color {
    let c = theme::c();
    match severity {
        Severity::Error => c.error_text,
        Severity::Warning => c.warning,
        Severity::Information => c.accent,
        Severity::Hint => c.text_faint,
    }
}

/// Registered icon key of a severity.
fn severity_icon(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "problem-error",
        Severity::Warning => "warning",
        Severity::Information | Severity::Hint => "info",
    }
}

/// A path as a person reads it: without Windows' `\\?\` prefix, forward
/// slashes, and relative to the open folder it is under.
pub(crate) fn readable_path(roots: &[PathBuf], path: &std::path::Path) -> String {
    let plain = |p: &std::path::Path| {
        let text = p.to_string_lossy().replace('\\', "/");
        text.strip_prefix("//?/").map_or(text.clone(), str::to_owned)
    };
    let full = plain(path);
    for root in roots {
        let root = plain(root);
        let root = root.trim_end_matches('/');
        if let Some(rest) = full.strip_prefix(root).and_then(|r| r.strip_prefix('/')) {
            return rest.to_owned();
        }
    }
    full
}

// ---- Findings ---------------------------------------------------------------

/// Findings of every open document, most errors first.
pub(crate) fn collect(state: &AppContext) -> Vec<FileProblems> {
    let Some(services) = super::language_ui::services(state) else {
        return Vec::new();
    };
    let app = state.get();
    let current = super::language_ui::target(&app, app.workspace.active_pane()).map(|t| t.path);
    let only_current = app.log_panel.current_only;
    let mut groups = Vec::new();
    for (id, doc) in &app.workspace.documents {
        if !doc.is_text() || (only_current && current.as_deref() != Some(doc.meta.path.as_path())) {
            continue;
        }
        let Some(found) = services.results(&doc.meta.path, &doc.stamp(*id)).diagnostics else {
            continue;
        };
        if found.is_empty() {
            continue;
        }
        let text = doc.buffer.text_snapshot();
        if let Some(file) = problems::file_problems(doc.meta.path.clone(), &text, &found) {
            groups.push(file);
        }
    }
    problems::order(&mut groups);
    groups
}

/// Errors in open documents, without building any rows.
pub(crate) fn error_count(state: &AppContext) -> usize {
    let Some(services) = super::language_ui::services(state) else {
        return 0;
    };
    let app = state.get();
    app.workspace
        .documents
        .iter()
        .filter(|(_, doc)| doc.is_text())
        .filter_map(|(id, doc)| services.results(&doc.meta.path, &doc.stamp(*id)).diagnostics)
        .map(|found| found.iter().filter(|d| d.severity == Severity::Error).count())
        .sum()
}

enum Row {
    File {
        path: PathBuf,
        count: usize,
        collapsed: bool,
    },
    Item {
        path: PathBuf,
        problem: problems::Problem,
    },
}

fn rows(groups: &[FileProblems], collapsed: &BTreeSet<PathBuf>) -> Vec<Row> {
    let mut rows = Vec::new();
    for group in groups {
        let hidden = collapsed.contains(&group.path);
        rows.push(Row::File {
            path: group.path.clone(),
            count: group.items.len(),
            collapsed: hidden,
        });
        if !hidden {
            rows.extend(group.items.iter().map(|problem| Row::Item {
                path: group.path.clone(),
                problem: problem.clone(),
            }));
        }
    }
    rows
}

// ---- Dock ------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub(crate) fn render(
    rect: UiRect,
    tab: PanelTab,
    state: AppContext,
    terminal_tabs: TerminalStore,
    editor_focus: UiFocusHandle,
    terminal_focus: UiFocusHandle,
    application: Arc<ApplicationHandle>,
    region: Option<crate::domain::application_layout::RegionId>,
    max_height: f32,
    // The editor area above the dock: where a jump reveals its target.
    editor: UiRect,
) -> Element {
    let sessions = state
        .get()
        .layout
        .application_layout
        .as_ref()
        .and_then(|layout| region.and_then(|id| layout.terminal_view(id)))
        .map_or(0, |(ids, _)| ids.len());
    let body = UiRect::new(rect.left, rect.top + theme::TABS_H, rect.right, rect.bottom);
    let (binding, _, _) = super::terminal_view::binding(&state, &terminal_tabs, &application);
    let panel_state = state.get().log_panel;
    // The dock header, as `commits_header` lays it out.
    let bar = UiRect::new(rect.left, rect.top + 1.0, rect.right, rect.top + theme::TABS_H);
    let (content, toolbar): (Element, Box<dyn FnOnce(UiRect) -> Element>) = if tab == PanelTab::Output
    {
        let channels = output::hub().channels();
        let shown = panel_state.shown(&channels);
        let content = output_body(body, &state, &panel_state, shown.clone(), &editor_focus);
        let tool_state = state.clone();
        (
            content,
            Box::new(move |free| {
                output_toolbar(free, bar, &tool_state, &panel_state, &channels, shown)
            }),
        )
    } else {
        let groups = collect(&state);
        let counts = problems::Counts::of(&groups);
        let content = problems_body(body, editor, &state, &editor_focus, &panel_state, &groups);
        let tool_state = state.clone();
        (
            content,
            Box::new(move |free| problems_toolbar(free, bar, &tool_state, &panel_state, counts)),
        )
    };
    let header = view::commits_header(
        rect,
        sessions,
        binding.clone(),
        terminal_focus,
        editor_focus,
        region.is_some(),
        toolbar,
    );
    let root = group(rect)
        .key("bottom-log-panel")
        .child(content)
        .child(header);
    if region.is_some() {
        root
    } else {
        view::resize_panel(root, rect, binding, max_height)
    }
}

// ---- Shared widgets ----------------------------------------------------------

/// A flat button holding a label and an optional trailing glyph.
fn chip(
    key: String,
    rect: UiRect,
    label: String,
    chevron: bool,
    active: bool,
    color: Color,
    click: impl Fn() + Send + Sync + 'static,
) -> Element {
    let node_id = UiId::owned(key.clone());
    let mut button = Element::new(move |cx| {
        let flags = cx.context.interaction_flags(&node_id);
        let style = if active || flags.hovered || flags.pressed {
            VisualStyle::filled(theme::c().surface).radius(3.0)
        } else {
            VisualStyle::default()
        };
        UiElement::panel(node_id.clone(), rect, style).children(cx.children)
    })
    .key(key.clone())
    .event_policy(EventPolicy::INTERACTIVE)
    .cursor(CursorIcon::Pointer)
    .semantics(Semantics::new(SemanticRole::Button).name(label.clone()))
    .on_click(click);
    let label_right = if chevron { rect.right - 18.0 } else { rect.right - 6.0 };
    button = button.child(text(
        UiRect::new(rect.left + 8.0, rect.top, label_right.max(rect.left + 8.0), rect.bottom),
        label,
        theme::sans(color, theme::UI_SIZE),
    ));
    if chevron {
        button = button.child(view::icon(
            "log-chip-chevron",
            "chevron-down",
            theme::icon_rect(UiRect::new(
                rect.right - 20.0,
                rect.top,
                rect.right - 4.0,
                rect.bottom,
            )),
            theme::c().text_muted,
        ));
    }
    button
}

fn set(state: &AppContext, change: impl FnOnce(&mut LogPanel) + Send + 'static) {
    state.model().update(move |app| change(&mut app.log_panel));
}

// ---- Output ------------------------------------------------------------------

fn output_toolbar(
    free: UiRect,
    bar: UiRect,
    state: &AppContext,
    panel_state: &LogPanel,
    channels: &[ChannelInfo],
    shown: Option<ChannelId>,
) -> Element {
    let compact = bar.width() < FULL_TOOLS_W;
    let c = theme::c();
    let tone = c.text_soft;
    // More, follow, find, then (roomy only) level and clear: right to left.
    let wanted = if compact { 3 } else { 5 };
    let tools = header::dock_tools(bar, wanted);
    let slots = tools.slots.clone();
    let tools_left = tools.left - 8.0;
    // The header's own space, so the tools sit at its right end like the terminal's.
    let mut root = group(bar)
        .key("log-toolbar")
        .child(header::rule(bar, tools.rule));

    // The channel picker takes the room the tools leave.
    let title = shown
        .as_ref()
        .and_then(|id| channels.iter().find(|c| &c.id == id))
        .map_or_else(|| "No output".to_owned(), |c| c.title.clone());
    let picker_w = (tools_left - free.left).clamp(0.0, 240.0);
    if picker_w > 40.0 {
        let top = free.top + (free.height() - 22.0) / 2.0;
        let rect = UiRect::new(free.left, top, free.left + picker_w, top + 22.0);
        let click = state.clone();
        root = root.child(chip(
            "log-channel-picker".into(),
            rect,
            title,
            true,
            matches!(panel_state.menu, Some(Menu::Channels(_))),
            c.text,
            move || {
                let anchor = (rect.left, rect.bottom + 2.0);
                set(&click, move |p| {
                    p.menu = match p.menu {
                        Some(Menu::Channels(_)) => None,
                        _ => Some(Menu::Channels(anchor)),
                    }
                });
            },
        ));
    }

    let more = slots[0];
    let more_state = state.clone();
    root = root.child(header::button(
        "log-more",
        more,
        "terminal-more",
        "More actions",
        matches!(panel_state.menu, Some(Menu::More(_))),
        tone,
        move || {
            let anchor = (more.right, more.bottom + 2.0);
            set(&more_state, move |p| {
                p.menu = match p.menu {
                    Some(Menu::More(_)) => None,
                    _ => Some(Menu::More(anchor)),
                }
            });
        },
    ));
    let follow_state = state.clone();
    root = root.child(header::button(
        "log-follow",
        slots[1],
        "arrow-down",
        "Scroll to the newest line and follow",
        panel_state.follow,
        tone,
        move || {
            set(&follow_state, |p| {
                p.follow = !p.follow;
            });
        },
    ));
    let find_state = state.clone();
    root = root.child(header::button(
        "log-find",
        slots[2],
        "search",
        "Filter lines",
        panel_state.find_open,
        tone,
        move || {
            set(&find_state, |p| {
                p.find_open = !p.find_open;
                p.menu = None;
            });
        },
    ));
    if !compact {
        let level = slots[3];
        let level_state = state.clone();
        root = root.child(header::button(
            "log-level",
            level,
            "filter",
            "Lowest level shown",
            panel_state.min_level != Level::Info,
            tone,
            move || {
                let anchor = (level.right, level.bottom + 2.0);
                set(&level_state, move |p| {
                    p.menu = match p.menu {
                        Some(Menu::Level(_)) => None,
                        _ => Some(Menu::Level(anchor)),
                    }
                });
            },
        ));
        let clear_state = state.clone();
        let target = shown;
        root = root.child(header::button(
            "log-clear",
            slots[4],
            "trash",
            "Clear this channel",
            false,
            tone,
            move || {
                if let Some(id) = &target {
                    output::hub().clear(id);
                }
                set(&clear_state, |p| p.scroll = 0.0);
            },
        ));
    }
    root
}

fn output_body(
    body: UiRect,
    state: &AppContext,
    panel_state: &LogPanel,
    shown: Option<ChannelId>,
    editor_focus: &UiFocusHandle,
) -> Element {
    let c = theme::c();
    let backdrop = panel(body, VisualStyle::filled(c.bg)).key("log-output-body");
    let Some(id) = shown else {
        return backdrop.child(text(
            UiRect::new(body.left + 12.0, body.top + 8.0, body.right - 12.0, body.top + 28.0),
            "No output yet",
            theme::sans(c.text_faint, theme::UI_SIZE),
        ));
    };
    output::hub().mark_seen(&id);
    let visible = (body.height() / ROW_H).ceil() as usize + 2;
    let (total, _) = output::hub().window(&id, panel_state.min_level, panel_state.needle(), 0, 0);
    let (scroll, max_scroll) =
        scroll_extent(total, body.height(), panel_state.scroll, panel_state.follow);
    let first = ((scroll - PAD_Y) / ROW_H).floor().max(0.0) as usize;
    let (_, lines) = output::hub().window(
        &id,
        panel_state.min_level,
        panel_state.needle(),
        first,
        visible,
    );

    let time_w = if panel_state.show_time { 74.0 } else { 0.0 };
    let level_w = 44.0;
    let mut content = clip(body, 0.0, -scroll);
    for (offset, line) in lines.iter().enumerate() {
        let top = body.top + PAD_Y + (first + offset) as f32 * ROW_H;
        let mut x = body.left + 12.0;
        if panel_state.show_time {
            content = content.child(text(
                UiRect::new(x, top, x + time_w, top + ROW_H),
                output::format_at(line.at),
                theme::mono(c.text_faint, theme::SMALL),
            ));
            x += time_w + 6.0;
        }
        content = content.child(text(
            UiRect::new(x, top, x + level_w, top + ROW_H),
            line.level.label(),
            theme::mono(level_color(line.level), theme::SMALL),
        ));
        x += level_w + 6.0;
        let message = if line.level <= Level::Debug { c.text_muted } else { c.text };
        content = content.child(text(
            UiRect::new(x, top, body.right - 14.0, top + ROW_H),
            line.text.to_string(),
            theme::mono(message, theme::UI_SIZE - 1.0),
        ));
    }
    let wheel_state = state.clone();
    let mut viewport = backdrop
        .child(content)
        .on_event(UiEventKind::Wheel, move |cx, payload| {
            if let UiEventPayload::Wheel { delta } = payload {
                let step = match delta.unit {
                    WheelUnit::Lines => delta.y * ROW_H * 3.0,
                    WheelUnit::Pixels => delta.y,
                };
                set(&wheel_state, move |p| {
                    let next = (scroll - step).clamp(0.0, max_scroll);
                    p.scroll = next;
                    p.follow = next >= max_scroll - 1.0;
                });
                cx.stop_propagation();
            }
        });
    if total == 0 {
        viewport = viewport.child(text(
            UiRect::new(body.left + 12.0, body.top + 8.0, body.right - 12.0, body.top + 28.0),
            if panel_state.needle().is_empty() {
                "No lines at this level"
            } else {
                "No lines match"
            },
            theme::sans(c.text_faint, theme::UI_SIZE),
        ));
    }
    if max_scroll > 0.0 {
        viewport = viewport.child(thumb(body, scroll, max_scroll));
    }
    if panel_state.find_open {
        viewport = viewport.child(find_box(body, state, total, editor_focus.clone()));
    }
    viewport
}

/// A passive bar showing where the view sits in a long list.
fn thumb(body: UiRect, scroll: f32, max_scroll: f32) -> Element {
    let track = UiRect::new(
        body.right - 10.0,
        body.top + 4.0,
        body.right - 2.0,
        body.bottom - 4.0,
    );
    let content = max_scroll + body.height();
    // A dock too short for a thumb gets none.
    if track.height() < 24.0 {
        return group(track);
    }
    let height = (track.height() * body.height() / content).clamp(24.0, track.height());
    let top = track.top + (track.height() - height) * (scroll / max_scroll);
    crate::ui::components::scrollbar::thumb(
        UiRect::new(track.left + 2.0, top, track.right - 2.0, top + height),
        0x90,
    )
    .key("log-scroll-thumb")
}

fn find_box(body: UiRect, state: &AppContext, total: usize, editor_focus: UiFocusHandle) -> Element {
    let c = theme::c();
    let width = 300.0_f32.min((body.width() - 28.0).max(0.0));
    let card = UiRect::new(
        body.right - 14.0 - width,
        body.top + 8.0,
        body.right - 14.0,
        body.top + 38.0,
    );
    let close_state = state.clone();
    let close_focus = editor_focus.clone();
    let close_rect = UiRect::new(card.right - 26.0, card.top + 4.0, card.right - 4.0, card.bottom - 4.0);
    let count_rect = UiRect::new(close_rect.left - 72.0, card.top, close_rect.left - 2.0, card.bottom);
    let mut count_style = theme::sans(c.text_muted, theme::SMALL + 1.0);
    count_style.align = TextAlign::Right;
    let field = UiRect::new(card.left + 8.0, card.top + 3.0, count_rect.left - 4.0, card.bottom - 3.0);
    let input_state = state.clone();
    let submit_state = state.clone();
    let cancel_state = state.clone();
    let cancel_focus = editor_focus;
    theme::bordered(card, c.surface, c.border, 6.0, 1.0)
        .shadow(
            ShadowStyle::new(Color::BLACK)
                .alpha(c.shadow_alpha)
                .offset(0.0, 2.0)
                .blur(3.0),
        )
        .key("log-find")
        .event_policy(EventPolicy::INTERACTIVE)
        .on_input(|cx, _| cx.stop_propagation())
        .on_key_down(|cx, _| cx.stop_propagation())
        .child(text(count_rect, format!("{total} lines"), count_style))
        .child(header::button(
            "log-find-close",
            close_rect,
            "close",
            "Close filter",
            false,
            c.text_soft,
            move || {
                set(&close_state, |p| {
                    p.find_open = false;
                    p.find.clear();
                });
                close_focus.focus();
            },
        ))
        .child(
            lgui::prelude::component(field, move |cx, &rect| {
                let id = cx.use_stable_id();
                let focus = cx.focus_handle(id.clone());
                let mounted = focus.clone();
                cx.use_effect((), move || mounted.focus());
                let read: fn(&super::state::AppState) -> &InputState = |app| &app.log_panel.find;
                let write: fn(&mut super::state::AppState) -> &mut InputState =
                    |app| &mut app.log_panel.find;
                let submit = submit_state.clone();
                let cancel = cancel_state.clone();
                let cancel_focus = cancel_focus.clone();
                input::render_access(
                    rect,
                    id,
                    focus,
                    InputBinding::new(input_state.model(), read, write),
                    AccessInputOptions {
                        label: "Filter output lines",
                        placeholder: "Filter",
                        style: InputStyle {
                            text: theme::mono(theme::c().text, theme::UI_SIZE),
                            placeholder: theme::mono(theme::c().text_faint, theme::UI_SIZE),
                            caret: theme::c().text,
                            selection: theme::c().selection,
                        },
                        on_submit: Some(Arc::new(move || {
                            set(&submit, |p| p.follow = false);
                        })),
                        on_cancel: Some(Arc::new(move || {
                            set(&cancel, |p| {
                                p.find_open = false;
                                p.find.clear();
                            });
                            cancel_focus.focus();
                        })),
                        on_blur: None,
                    },
                )
            })
            .key("log-find-input")
            .semantics(Semantics::new(SemanticRole::TextInput).name("Filter output lines")),
        )
}

// ---- Problems ----------------------------------------------------------------

fn problems_toolbar(
    free: UiRect,
    bar: UiRect,
    state: &AppContext,
    panel_state: &LogPanel,
    counts: problems::Counts,
) -> Element {
    let c = theme::c();
    let tools = header::dock_tools(bar, 1);
    let mut root = group(bar)
        .key("problems-toolbar")
        .child(header::rule(bar, tools.rule));
    let top = free.top + (free.height() - 22.0) / 2.0;
    let mut left = free.left;
    let compact = (tools.left - free.left) < 300.0;
    let kinds: [(&str, &'static str, usize, Color, fn(&mut Filter) -> &mut bool); 3] = [
        ("Errors", "problems-errors", counts.errors, c.error_text, |f| &mut f.errors),
        ("Warnings", "problems-warnings", counts.warnings, c.warning, |f| &mut f.warnings),
        ("Info", "problems-infos", counts.infos, c.accent, |f| &mut f.infos),
    ];
    for (name, key, count, color, field) in kinds {
        let mut filter = panel_state.severities;
        let on = *field(&mut filter);
        let label = if compact { format!("{count}") } else { format!("{name} {count}") };
        let width = crate::ui::tabs::measure(&label, theme::UI_SIZE, 400) + 22.0;
        if left + width > tools.left - 8.0 {
            break;
        }
        let rect = UiRect::new(left, top, left + width, top + 22.0);
        left = rect.right + 2.0;
        let click = state.clone();
        root = root.child(chip(
            key.into(),
            rect,
            label,
            false,
            on,
            if on { color } else { c.text_faint },
            move || {
                set(&click, move |p| {
                    let flag = field(&mut p.severities);
                    *flag = !*flag;
                });
            },
        ));
    }
    let slot = tools.slots[0];
    let click = state.clone();
    root.child(header::button(
        "problems-current",
        slot,
        "go-to-file",
        "Only the active file",
        panel_state.current_only,
        c.text_soft,
        move || {
            set(&click, |p| p.current_only = !p.current_only);
        },
    ))
}

fn problems_body(
    body: UiRect,
    editor: UiRect,
    state: &AppContext,
    editor_focus: &UiFocusHandle,
    panel_state: &LogPanel,
    groups: &[FileProblems],
) -> Element {
    let c = theme::c();
    let shown = problems::filtered(groups, panel_state.severities);
    let rows = rows(&shown, &panel_state.collapsed);
    let backdrop = panel(body, VisualStyle::filled(c.bg)).key("log-problems-body");
    if rows.is_empty() {
        return backdrop.child(text(
            UiRect::new(body.left + 12.0, body.top + 8.0, body.right - 12.0, body.top + 28.0),
            if groups.is_empty() {
                "No problems in the open files"
            } else {
                "Nothing matches the filter"
            },
            theme::sans(c.text_faint, theme::UI_SIZE),
        ));
    }
    let (scroll, max_scroll) = scroll_extent(rows.len(), body.height(), panel_state.problems_scroll, false);
    let first = ((scroll - PAD_Y) / ROW_H).floor().max(0.0) as usize;
    let last = (first + (body.height() / ROW_H).ceil() as usize + 2).min(rows.len());
    let mut content = clip(body, 0.0, -scroll);
    let roots = state.get().explorer.model.roots().to_vec();
    for (index, row) in rows.iter().enumerate().take(last).skip(first) {
        let top = body.top + PAD_Y + index as f32 * ROW_H;
        let rect = UiRect::new(body.left, top, body.right, top + ROW_H);
        content = content.child(match row {
            Row::File { path, count, collapsed } => {
                let name = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                // The folder the file is in, relative to the open folder.
                let relative = readable_path(&roots, path);
                let dir = relative
                    .rsplit_once('/')
                    .map(|(dir, _)| dir.to_owned())
                    .unwrap_or_default();
                let toggle = state.clone();
                let key_path = path.clone();
                let fold = path.clone();
                data_row(
                    format!("problems-file-{index}"),
                    rect,
                    (
                        if *collapsed { "chevron-right" } else { "chevron-down" },
                        c.text_soft,
                    ),
                    &name,
                    (!dir.is_empty()).then_some(dir),
                    format!("{count}"),
                    c.text,
                    true,
                    move || {
                        let path = fold.clone();
                        set(&toggle, move |p| {
                            if !p.collapsed.remove(&path) {
                                p.collapsed.insert(path);
                            }
                        });
                    },
                )
                .key(format!("problems-file-{}", key_path.display()))
            }
            Row::Item { path, problem } => {
                let jump = state.clone();
                let focus = editor_focus.clone();
                let (path, offset) = (path.clone(), problem.offset);
                let mut message = problem.message.lines().next().unwrap_or_default().to_owned();
                if let Some(source) = &problem.source {
                    message = match &problem.code {
                        Some(code) => format!("{message}  {source}({code})"),
                        None => format!("{message}  {source}"),
                    };
                }
                data_row(
                    format!("problems-item-{index}"),
                    rect,
                    (severity_icon(problem.severity), severity_color(problem.severity)),
                    &message,
                    None,
                    format!("{}:{}", problem.line, problem.column),
                    c.text,
                    false,
                    move || {
                        super::language_ui::navigate::go(
                            &jump,
                            super::language_ui::navigate::Place::bytes(path.clone(), offset..offset),
                            editor,
                            true,
                        );
                        // The caret shows only in the focused editor.
                        focus.focus();
                    },
                )
            }
        });
    }
    let wheel_state = state.clone();
    let mut viewport = backdrop
        .child(content)
        .on_event(UiEventKind::Wheel, move |cx, payload| {
            if let UiEventPayload::Wheel { delta } = payload {
                let step = match delta.unit {
                    WheelUnit::Lines => delta.y * ROW_H * 3.0,
                    WheelUnit::Pixels => delta.y,
                };
                set(&wheel_state, move |p| {
                    p.problems_scroll = (scroll - step).clamp(0.0, max_scroll);
                });
                cx.stop_propagation();
            }
        });
    if max_scroll > 0.0 {
        viewport = viewport.child(thumb(body, scroll, max_scroll));
    }
    viewport
}

/// One clickable row: leading text (tinted by `tint`), an optional dimmer
/// detail after it, and a right-aligned note.
#[allow(clippy::too_many_arguments)]
fn data_row(
    key: String,
    rect: UiRect,
    icon: (&'static str, Color),
    lead: &str,
    detail: Option<String>,
    note: String,
    tint: Color,
    bold: bool,
    click: impl Fn() + Send + Sync + 'static,
) -> Element {
    let c = theme::c();
    let icon_key = format!("{key}-icon");
    let node_id = UiId::owned(key.clone());
    let mut row = Element::new(move |cx| {
        let hovered = cx.context.interaction_flags(&node_id).hovered;
        UiElement::panel(
            node_id.clone(),
            rect,
            if hovered {
                VisualStyle::filled(c.active_line)
            } else {
                VisualStyle::default()
            },
        )
        .children(cx.children)
    })
    .key(key)
    .event_policy(EventPolicy::INTERACTIVE)
    .cursor(CursorIcon::Pointer)
    .semantics(Semantics::new(SemanticRole::Button).name(lead.to_owned()))
    .on_click(click);
    let note_w = crate::ui::tabs::measure(&note, theme::SMALL, 400) + 12.0;
    let lead_w = crate::ui::tabs::measure(lead, theme::UI_SIZE, if bold { 600 } else { 400 });
    let text_left = rect.left + 12.0 + 22.0;
    let text_right = (rect.right - note_w - 16.0).max(text_left);
    let style = if bold {
        theme::sans_semibold(tint, theme::UI_SIZE)
    } else {
        theme::sans(tint, theme::UI_SIZE)
    };
    let (glyph, glyph_color) = icon;
    let (icon_rect, glyph) = (
        theme::icon_rect(UiRect::new(rect.left + 10.0, rect.top, rect.left + 28.0, rect.bottom)),
        glyph,
    );
    row = row
        .child(
            Element::new(move |cx| {
                UiElement::icon(cx.id, icon_rect, glyph).icon_style(IconStyle::new(glyph_color))
            })
            .key(icon_key),
        )
        .child(text(
            UiRect::new(text_left, rect.top, text_right, rect.bottom),
            lead.to_owned(),
            style,
        ));
    if let Some(detail) = detail {
        let left = text_left + lead_w + 8.0;
        if left < text_right {
            row = row.child(text(
                UiRect::new(left, rect.top, text_right, rect.bottom),
                detail,
                theme::sans(c.text_faint, theme::SMALL + 1.0),
            ));
        }
    }
    let mut note_style = theme::mono(c.text_muted, theme::SMALL);
    note_style.align = TextAlign::Right;
    row.child(text(
        UiRect::new(rect.right - note_w - 14.0, rect.top, rect.right - 14.0, rect.bottom),
        note,
        note_style,
    ))
}

// ---- Menus -------------------------------------------------------------------

struct Item {
    label: String,
    checked: bool,
    /// A heading: drawn dim and not clickable.
    heading: bool,
    run: Option<Arc<dyn Fn() + Send + Sync>>,
}

fn items(state: &AppContext, panel_state: &LogPanel, menu: Menu, narrow: bool) -> Vec<Item> {
    let level_items = |out: &mut Vec<Item>| {
        for (label, level) in [
            ("Trace and above", Level::Trace),
            ("Debug and above", Level::Debug),
            ("Info and above", Level::Info),
            ("Warnings and above", Level::Warn),
            ("Errors only", Level::Error),
        ] {
            let state = state.clone();
            out.push(Item {
                label: label.into(),
                checked: panel_state.min_level == level,
                heading: false,
                run: Some(Arc::new(move || {
                    set(&state, move |p| {
                        p.min_level = level;
                        p.menu = None;
                    })
                })),
            });
        }
    };
    let mut out = Vec::new();
    match menu {
        Menu::Channels(_) => {
            let channels = output::hub().channels();
            let shown = panel_state.shown(&channels);
            let mut group = None;
            for info in channels {
                if group != Some(info.group) {
                    group = Some(info.group);
                    out.push(Item {
                        label: info.group.label().into(),
                        checked: false,
                        heading: true,
                        run: None,
                    });
                }
                let state = state.clone();
                let id = info.id.clone();
                out.push(Item {
                    label: if info.unread_error {
                        format!("{}  ●", info.title)
                    } else {
                        info.title.clone()
                    },
                    checked: shown.as_ref() == Some(&info.id),
                    heading: false,
                    run: Some(Arc::new(move || {
                        let id = id.clone();
                        set(&state, move |p| {
                            p.channel = Some(id);
                            p.scroll = 0.0;
                            p.follow = true;
                            p.menu = None;
                        })
                    })),
                });
            }
            if out.is_empty() {
                out.push(Item {
                    label: "No channels yet".into(),
                    checked: false,
                    heading: true,
                    run: None,
                });
            }
        }
        Menu::Level(_) => level_items(&mut out),
        Menu::More(_) => {
            let toggle = state.clone();
            out.push(Item {
                label: "Show time".into(),
                checked: panel_state.show_time,
                heading: false,
                run: Some(Arc::new(move || {
                    set(&toggle, |p| {
                        p.show_time = !p.show_time;
                        p.menu = None;
                    })
                })),
            });
            if narrow {
                let channel = panel_state.shown(&output::hub().channels());
                out.push(Item {
                    label: "Clear this channel".into(),
                    checked: false,
                    heading: false,
                    run: Some(Arc::new({
                        let state = state.clone();
                        move || {
                            if let Some(id) = &channel {
                                output::hub().clear(id);
                            }
                            set(&state, |p| {
                                p.scroll = 0.0;
                                p.menu = None;
                            })
                        }
                    })),
                });
                out.push(Item {
                    label: "Lowest level".into(),
                    checked: false,
                    heading: true,
                    run: None,
                });
                level_items(&mut out);
            }
        }
    }
    out
}

/// The open dropdown, if any, over `viewport`. `narrow` folds the header tools
/// that did not fit into the More menu.
pub(crate) fn menu_overlay(viewport: UiRect, state: &AppContext, narrow: bool) -> Option<Element> {
    let panel_state = state.get().log_panel;
    let menu = panel_state.menu?;
    let (Menu::Channels(anchor) | Menu::Level(anchor) | Menu::More(anchor)) = menu;
    let list = items(state, &panel_state, menu, narrow);
    let height = MENU_PAD * 2.0 + MENU_ITEM_H * list.len() as f32;
    let width = MENU_W.min(viewport.width().max(0.0));
    // Channels hang from their button's left edge; the others from its right.
    let left = match menu {
        Menu::Channels(_) => anchor.0,
        _ => anchor.0 - width,
    }
    .clamp(viewport.left, (viewport.right - width).max(viewport.left));
    let top = anchor.1.clamp(viewport.top, (viewport.bottom - height).max(viewport.top));
    let card = UiRect::new(left, top, left + width, top + height);
    let dismiss = state.clone();
    let escape = state.clone();
    let overlay = Element::new(move |cx| {
        UiElement::panel(cx.id, viewport, VisualStyle::default())
            .auto_focus()
            .children(cx.children)
    })
    .key("log-menu")
    .event_policy(EventPolicy::INTERACTIVE)
    .focus_scope()
    .on_pointer_down_with_button(move |cx, pointer, _| {
        if !card.contains(pointer.point) {
            set(&dismiss, |p| p.menu = None);
        }
        cx.stop_propagation();
    })
    .on_input(|cx, _| cx.stop_propagation())
    .on_key_down(move |cx, event| {
        if event.key == LogicalKey::Named(NamedKey::Escape) {
            set(&escape, |p| p.menu = None);
        }
        cx.prevent_default();
        cx.stop_propagation();
    });
    let c = theme::c();
    let mut surface = theme::bordered(card, c.surface, c.border, 6.0, 1.0).shadow(
        ShadowStyle::new(Color::BLACK)
            .alpha(c.shadow_alpha)
            .offset(0.0, 2.0)
            .blur(3.0),
    );
    for (index, item) in list.into_iter().enumerate() {
        let row_top = card.top + MENU_PAD + MENU_ITEM_H * index as f32;
        let row = UiRect::new(card.left + 1.0, row_top, card.right - 1.0, row_top + MENU_ITEM_H);
        let label = format!(
            "{}{}",
            if item.checked { "✓ " } else { "  " },
            item.label
        );
        if item.heading {
            surface = surface.child(text(
                UiRect::new(row.left + 10.0, row.top, row.right - 10.0, row.bottom),
                item.label,
                theme::sans_semibold(c.text_faint, theme::SMALL),
            ));
            continue;
        }
        let key = format!("log-menu-row-{index}");
        let node_id = UiId::owned(key.clone());
        let highlight = UiRect::new(row.left + 4.0, row.top, row.right - 4.0, row.bottom);
        let run = item.run;
        let mut button = Element::new(move |cx| {
            let hovered = cx.context.interaction_flags(&node_id).hovered;
            UiElement::panel(
                node_id.clone(),
                highlight,
                if hovered {
                    VisualStyle::filled(theme::c().selection).radius(4.0)
                } else {
                    VisualStyle::default()
                },
            )
            .hit_rect(row)
            .children(cx.children)
        })
        .key(key)
        .event_policy(EventPolicy::INTERACTIVE)
        .cursor(CursorIcon::Pointer)
        .semantics(Semantics::new(SemanticRole::Button).name(item.label))
        .child(text(
            UiRect::new(row.left + 10.0, row.top, row.right - 10.0, row.bottom),
            label,
            theme::mono(c.text, theme::SMALL),
        ));
        if let Some(run) = run {
            button = button.on_click(move || run());
        }
        surface = surface.child(button);
    }
    Some(overlay.child(surface))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_pins_to_the_bottom_and_manual_scroll_is_clamped() {
        // 100 rows of 18px in a 200px view.
        let (at, max) = scroll_extent(100, 200.0, 0.0, true);
        assert_eq!(at, max);
        assert_eq!(max, 100.0 * ROW_H + PAD_Y * 2.0 - 200.0);
        assert_eq!(scroll_extent(100, 200.0, 99_999.0, false).0, max);
        assert_eq!(scroll_extent(100, 200.0, -5.0, false).0, 0.0);
    }

    #[test]
    fn a_short_list_never_scrolls() {
        assert_eq!(scroll_extent(3, 200.0, 40.0, true), (0.0, 0.0));
        assert_eq!(scroll_extent(3, 200.0, 40.0, false), (0.0, 0.0));
    }

    #[test]
    fn the_shown_channel_falls_back_to_the_first() {
        let info = |id: &str| ChannelInfo {
            id: ChannelId::new(id),
            title: id.into(),
            group: output::Group::Extension,
            unread_error: false,
            len: 0,
        };
        let channels = [info("a"), info("b")];
        let mut panel = LogPanel::default();
        assert_eq!(panel.shown(&channels), Some(ChannelId::new("a")));
        panel.channel = Some(ChannelId::new("b"));
        assert_eq!(panel.shown(&channels), Some(ChannelId::new("b")));
        panel.channel = Some(ChannelId::new("gone"));
        assert_eq!(panel.shown(&channels), Some(ChannelId::new("a")));
        assert_eq!(panel.shown(&[]), None);
    }

    #[test]
    fn collapsed_files_hide_their_items() {
        let file = |name: &str| FileProblems {
            path: name.into(),
            items: vec![problems::Problem {
                severity: Severity::Error,
                message: "m".into(),
                source: None,
                code: None,
                line: 1,
                column: 1,
                offset: 0,
            }],
        };
        let groups = [file("a.rs"), file("b.rs")];
        assert_eq!(rows(&groups, &BTreeSet::new()).len(), 4);
        let collapsed = BTreeSet::from([PathBuf::from("a.rs")]);
        assert_eq!(rows(&groups, &collapsed).len(), 3);
    }

    /// Renders the panel for `tab` in a `width`×`height` dock and returns the
    /// node names it produced, so layout or paint panics show up at odd sizes.
    fn render_names(tab: PanelTab, width: f32, height: f32, open_find: bool) -> Vec<String> {
        use crate::app::state::AppState;
        use crate::app::terminal_binding::TerminalTabs;
        use lgui::application::{AppView, ApplicationContext};
        use lgui::core::UiScale;
        use lgui::session::UiSession;

        let context = ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        let viewport = UiRect::new(0.0, 0.0, width, height + 40.0);
        let dock = UiRect::new(0.0, 40.0, width, height + 40.0);
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(|| {
                let mut app = AppState::new();
                app.terminal_chrome.show_terminal = true;
                app.terminal.ui.panel_tab = tab;
                app.log_panel.find_open = open_find;
                app.log_panel.menu = Some(Menu::Channels((20.0, 60.0)));
                app
            }));
            let tabs = crate::app::terminal_binding::test_store(
                cx,
                Some(state.clone()),
                TerminalTabs::new,
            );
            let id = cx.use_stable_id();
            let focus = cx.focus_handle(id);
            let editor_id = cx.use_stable_id();
            let editor_focus = cx.focus_handle(editor_id);
            let overlay = menu_overlay(viewport, &state, width < FULL_TOOLS_W);
            let mut root = lgui::prelude::group(viewport).child(render(
                dock,
                tab,
                state,
                tabs,
                editor_focus,
                focus,
                Arc::new(ApplicationHandle::new(|task| task(), || {})),
                None,
                height,
                UiRect::new(0.0, 0.0, width, 40.0),
            ));
            if let Some(overlay) = overlay {
                root = root.child(overlay);
            }
            root
        });
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::new(1.0));
        session
            .tree()
            .nodes()
            .iter()
            .filter_map(|n| n.semantics.as_ref().and_then(|s| s.name.clone()))
            .collect()
    }

    #[test]
    fn both_panels_render_at_wide_and_narrow_sizes() {
        output::app_log(Level::Error, "render smoke");
        for (width, height) in [(900.0, 260.0), (520.0, 200.0), (300.0, 120.0), (900.0, 30.0)] {
            for tab in [PanelTab::Output, PanelTab::Problems] {
                let names = render_names(tab, width, height, false);
                assert!(
                    names.iter().any(|n| n == tab.label()),
                    "{tab:?} {width}x{height}: {names:?}"
                );
            }
        }
    }

    #[test]
    fn output_header_keeps_every_tool_when_roomy_and_folds_some_when_narrow() {
        let roomy = render_names(PanelTab::Output, 900.0, 260.0, false);
        for tool in ["Lowest level shown", "Clear this channel", "Filter lines", "More actions"] {
            assert!(roomy.iter().any(|n| n == tool), "missing {tool}: {roomy:?}");
        }
        let narrow = render_names(PanelTab::Output, 520.0, 260.0, false);
        assert!(narrow.iter().any(|n| n == "Filter lines"));
        assert!(narrow.iter().any(|n| n == "More actions"));
        assert!(!narrow.iter().any(|n| n == "Clear this channel"));
    }

    #[test]
    fn the_filter_box_appears_only_while_open() {
        let closed = render_names(PanelTab::Output, 900.0, 260.0, false);
        assert!(!closed.iter().any(|n| n == "Filter output lines"));
        let open = render_names(PanelTab::Output, 900.0, 260.0, true);
        assert!(open.iter().any(|n| n == "Filter output lines"), "{open:?}");
    }

    #[test]
    fn paths_lose_the_windows_prefix_and_are_relative_to_the_open_folder() {
        let roots = vec![PathBuf::from(r"D:\Documents\cprojects\loom")];
        assert_eq!(
            readable_path(&roots, std::path::Path::new(r"\\?\D:\Documents\cprojects\loom\src\main.rs")),
            "src/main.rs"
        );
        // A verbatim root matches a plain path too.
        let verbatim = vec![PathBuf::from(r"\\?\D:\Documents\cprojects\loom")];
        assert_eq!(
            readable_path(&verbatim, std::path::Path::new(r"D:\Documents\cprojects\loom\a.rs")),
            "a.rs"
        );
        // Outside every folder: the whole path, cleaned.
        assert_eq!(
            readable_path(&roots, std::path::Path::new(r"\\?\E:\other\b.rs")),
            "E:/other/b.rs"
        );
        // A sibling folder with the same prefix is not inside the root.
        assert_eq!(
            readable_path(&roots, std::path::Path::new(r"D:\Documents\cprojects\loom2\c.rs")),
            "D:/Documents/cprojects/loom2/c.rs"
        );
    }

    #[test]
    fn the_find_text_only_filters_while_the_box_is_open() {
        let mut panel = LogPanel::default();
        panel.find.set_text("boom");
        assert_eq!(panel.needle(), "");
        panel.find_open = true;
        assert_eq!(panel.needle(), "boom");
    }
}
