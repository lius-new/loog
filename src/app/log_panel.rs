//! The Output and Problems panels of the bottom dock.
//!
//! Output shows one channel of the [`LogHub`] at a time; Problems lists the
//! findings of open documents. Neither knows who wrote what: channels and
//! findings are data.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use lgui::core::{
    CursorIcon, EventPolicy, IconStyle, LogicalKey, NamedKey, PointerButton, SemanticRole,
    Semantics, UiElement, UiEventKind, UiEventPayload, UiFocusHandle, UiId, WheelUnit, clip,
};
use lgui::prelude::{
    Color, Element, ShadowStyle, TextAlign, UiRect, VisualStyle, group, panel, text,
};

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
    /// The lowest level the person chose; none lets each channel use its own default.
    pub min_level: Option<Level>,
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
    /// The pointer is over the list, so its scrollbar shows more strongly.
    pub hovered: bool,
    /// While the thumb is dragged: where in the thumb it was taken hold of.
    pub scrollbar_drag: Option<f32>,
}

impl Default for LogPanel {
    fn default() -> Self {
        Self {
            channel: None,
            min_level: None,
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
            hovered: false,
            scrollbar_drag: None,
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

    /// The lowest level shown for `shown`. Unless the person chose one, a
    /// channel of a process opens at everything, since what a process says on
    /// its own streams is what it is there to read; the others open at Info.
    pub fn level(&self, channels: &[ChannelInfo], shown: Option<&ChannelId>) -> Level {
        self.min_level.unwrap_or_else(|| {
            let process = shown
                .and_then(|id| channels.iter().find(|c| &c.id == id))
                .is_some_and(|c| c.group == output::Group::Process);
            if process { Level::Trace } else { Level::Info }
        })
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
        text.strip_prefix("//?/")
            .map_or(text.clone(), str::to_owned)
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
    let current = super::language_ui::target(&app, app.workspace.active_pane())
        .and_then(|t| t.resource.local_path().map(std::path::Path::to_path_buf));
    let only_current = app.log_panel.current_only;
    let mut groups = Vec::new();
    for (id, doc) in &app.workspace.documents {
        let Some(path) = doc.meta.local_path() else {
            continue;
        };
        if !doc.is_text() || (only_current && current.as_deref() != Some(path)) {
            continue;
        }
        let Some(found) = services.results(path, &doc.stamp(*id)).diagnostics else {
            continue;
        };
        if found.is_empty() {
            continue;
        }
        let text = doc.buffer.text_snapshot();
        if let Some(file) = problems::file_problems(path.to_path_buf(), &text, &found) {
            groups.push(file);
        }
    }
    // Files that are not open are what the services found by themselves, such as
    // the output of a build; an open file shows its own document's findings.
    if !only_current {
        groups.extend(services.closed_problems(&open_files(&app)));
    }
    problems::order(&mut groups);
    groups
}

/// The path to open for a file a service named. An open document keeps its own
/// spelling, so the jump finds it instead of opening the file a second time;
/// otherwise the canonical path, which is how files are opened elsewhere.
fn open_spelling(state: &AppContext, path: &std::path::Path) -> PathBuf {
    let key = problems::file_key(path);
    let app = state.get();
    app.workspace
        .documents
        .values()
        .filter_map(|doc| doc.meta.local_path())
        .find(|path| problems::file_key(path) == key)
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned()))
}

/// Every open text document, by file key.
fn open_files(app: &super::state::AppState) -> BTreeSet<String> {
    app.workspace
        .documents
        .values()
        .filter(|doc| doc.is_text())
        .filter_map(|doc| doc.meta.local_path().map(problems::file_key))
        .collect()
}

/// Problems of every severity in the open documents and the files the services
/// found by themselves, without building any rows.
pub(crate) fn counts(state: &AppContext) -> problems::Counts {
    let mut counts = problems::Counts::default();
    let Some(services) = super::language_ui::services(state) else {
        return counts;
    };
    let app = state.get();
    for found in app
        .workspace
        .documents
        .iter()
        .filter(|(_, doc)| doc.is_text())
        .filter_map(|(id, doc)| {
            services
                .results(doc.meta.local_path()?, &doc.stamp(*id))
                .diagnostics
        })
    {
        for finding in found.iter() {
            counts.add(finding.severity);
        }
    }
    let closed = services.closed_counts(&open_files(&app));
    counts.errors += closed.errors;
    counts.warnings += closed.warnings;
    counts.infos += closed.infos;
    counts
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
    let body = UiRect::new(rect.left, rect.top + theme::TABS_H, rect.right, rect.bottom);
    let (binding, _, _) = super::terminal_view::binding(&state, &terminal_tabs, &application);
    let panel_state = state.get().log_panel;
    // The dock header, as `commits_header` lays it out.
    let bar = UiRect::new(
        rect.left,
        rect.top + 1.0,
        rect.right,
        rect.top + theme::TABS_H,
    );
    let (content, toolbar): (Element, Box<dyn FnOnce(UiRect) -> Element>) = if tab
        == PanelTab::Output
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
        } else if chevron {
            // A dropdown shows that it is one even at rest.
            VisualStyle::filled(theme::c().active_line).radius(3.0)
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
    let label_right = if chevron {
        rect.right - 18.0
    } else {
        rect.right - 6.0
    };
    button = button.child(text(
        UiRect::new(
            rect.left + 8.0,
            rect.top,
            label_right.max(rect.left + 8.0),
            rect.bottom,
        ),
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

/// A toolbar spans the whole header to place its tools, but must not catch
/// the pointer: the dock switcher underneath it keeps its own hit area and cursor.
fn toolbar_root(bar: UiRect, key: &'static str) -> Element {
    Element::new(move |cx| {
        UiElement::group(cx.id, bar)
            .hit_rect(UiRect::new(bar.left, bar.top, bar.left, bar.top))
            .children(cx.children)
    })
    .key(key)
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
    let mut root = toolbar_root(bar, "log-toolbar").child(header::rule(bar, tools.rule));

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
            panel_state.min_level.is_some(),
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
            UiRect::new(
                body.left + 12.0,
                body.top + 8.0,
                body.right - 12.0,
                body.top + 28.0,
            ),
            "No output yet",
            theme::sans(c.text_faint, theme::UI_SIZE),
        ));
    };
    output::hub().mark_seen(&id);
    let visible = (body.height() / ROW_H).ceil() as usize + 2;
    let level = panel_state.level(&output::hub().channels(), Some(&id));
    let (total, _) = output::hub().window(&id, level, panel_state.needle(), 0, 0);
    let (scroll, max_scroll) =
        scroll_extent(total, body.height(), panel_state.scroll, panel_state.follow);
    let first = ((scroll - PAD_Y) / ROW_H).floor().max(0.0) as usize;
    let (_, lines) = output::hub().window(&id, level, panel_state.needle(), first, visible);

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
        let message = if line.level <= Level::Debug {
            c.text_muted
        } else {
            c.text
        };
        content = content.child(text(
            // The scrollbar draws over the text's right edge and takes no room.
            UiRect::new(x, top, body.right - 6.0, top + ROW_H),
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
            UiRect::new(
                body.left + 12.0,
                body.top + 8.0,
                body.right - 12.0,
                body.top + 28.0,
            ),
            if panel_state.needle().is_empty() {
                "No lines at this level"
            } else {
                "No lines match"
            },
            theme::sans(c.text_faint, theme::UI_SIZE),
        ));
    }
    viewport = viewport.child(scrollbar(
        body,
        state,
        Pane::Output,
        scroll,
        max_scroll,
        panel_state,
    ));
    if panel_state.find_open {
        viewport = viewport.child(find_box(body, state, total, editor_focus.clone()));
    }
    viewport
}

/// Which of the two lists a scrollbar belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pane {
    Output,
    Problems,
}

/// Scrolls `pane` to `value` pixels. Output follows its newest line again once
/// it reaches the bottom, and stops following when it leaves it.
fn scroll_to(state: &AppContext, pane: Pane, value: f32, max_scroll: f32) {
    set(state, move |p| match pane {
        Pane::Output => {
            p.scroll = value;
            p.follow = value >= max_scroll - 1.0;
        }
        Pane::Problems => p.problems_scroll = value,
    });
}

/// The overlay scrollbar of a list, as the terminal's: a transparent track along
/// the right edge that takes no room from the text, and a thumb that is faint at
/// rest and stronger while the pointer is over the list or the thumb is dragged.
/// Pressing the track jumps there with the thumb under the pointer, and a drag
/// keeps going wherever the pointer leaves to.
fn scrollbar(
    body: UiRect,
    state: &AppContext,
    pane: Pane,
    scroll: f32,
    max_scroll: f32,
    panel_state: &LogPanel,
) -> Element {
    use crate::ui::components::scrollbar as bar;
    use crate::ui::editor::view::{scroll_from_thumb, thumb_geometry};
    let track = UiRect::new(body.right - bar::SIZE, body.top, body.right, body.bottom);
    let dragging = panel_state.scrollbar_drag.is_some();
    if track.height() < 1.0 || (max_scroll <= 0.0 && !dragging) {
        return group(track);
    }
    let geometry = thumb_geometry(
        track.top,
        track.height(),
        body.height(),
        max_scroll + body.height(),
        scroll,
    );
    let travel = track.height() - geometry.length;
    let alpha = if panel_state.hovered || dragging {
        0xb8
    } else {
        0x68
    };
    // The thumb stays in the tree while it is dragged, even if the list shrinks
    // under it; an empty rectangle paints nothing.
    let thumb_rect = if max_scroll <= 0.0 {
        UiRect::new(track.right, track.top, track.right, track.top)
    } else {
        UiRect::new(
            track.left + bar::INSET,
            geometry.start,
            track.right - bar::INSET,
            geometry.start + geometry.length,
        )
    };
    let move_to = {
        let state = state.clone();
        move |pointer_y: f32, offset: f32| {
            let value = scroll_from_thumb(pointer_y - track.top - offset, travel, max_scroll);
            scroll_to(&state, pane, value, max_scroll);
        }
    };
    let press = {
        let state = state.clone();
        let move_to = move_to.clone();
        move |cx: &mut lgui::core::UiEventContext, pointer_y: f32, offset: f32, jump: bool| {
            set(&state, move |p| p.scrollbar_drag = Some(offset));
            if jump {
                move_to(pointer_y, offset);
            }
            cx.stop_propagation();
        }
    };
    let drag = {
        let state = state.clone();
        move |cx: &mut lgui::core::UiEventContext, pointer_y: f32| {
            if let Some(offset) = state.get().log_panel.scrollbar_drag
                && travel > 0.0
            {
                move_to(pointer_y, offset);
                cx.stop_propagation();
            }
        }
    };
    let release = {
        let state = state.clone();
        move |cx: &mut lgui::core::UiEventContext| {
            set(&state, |p| p.scrollbar_drag = None);
            cx.stop_propagation();
        }
    };
    let policy = EventPolicy {
        hover: true,
        press: true,
        focus: false,
    };
    let (track_press, track_drag, track_release) = (press.clone(), drag.clone(), release.clone());
    let track_hit = bar::track(track)
        .key("log-scrollbar-track")
        .event_policy(policy)
        .on_pointer_down_with_button(move |cx, pointer, button| {
            if button == PointerButton::Left {
                track_press(cx, pointer.point.y, geometry.length / 2.0, true);
            }
        })
        .on_pointer_drag(move |cx, pointer| track_drag(cx, pointer.point.y))
        .on_pointer_up(move |cx, _| track_release(cx));
    let thumb = bar::thumb(thumb_rect, alpha)
        .key("log-scrollbar-thumb")
        .event_policy(policy)
        .on_pointer_down_with_button(move |cx, pointer, button| {
            if button == PointerButton::Left {
                press(cx, pointer.point.y, pointer.point.y - geometry.start, false);
            }
        })
        .on_pointer_drag(move |cx, pointer| drag(cx, pointer.point.y))
        .on_pointer_up(move |cx, _| release(cx));
    group(track)
        .key("log-scrollbar")
        .child(track_hit)
        .child(thumb)
}

/// Follows the pointer over the whole window, so leaving the list also fades
/// the scrollbar. `body` is where the list is drawn, if one is on screen.
pub(crate) fn track_hover(root: Element, state: &AppContext, body: Option<UiRect>) -> Element {
    let Some(body) = body else {
        return root;
    };
    let state = state.clone();
    root.on_event_capture(UiEventKind::PointerMove, move |_, payload| {
        if let UiEventPayload::PointerMove { pointer } = payload {
            let hovered = body.contains(pointer.point);
            state.model().try_update(|app| {
                let changed = app.log_panel.hovered != hovered;
                app.log_panel.hovered = hovered;
                changed
            });
        }
    })
}

fn find_box(
    body: UiRect,
    state: &AppContext,
    total: usize,
    editor_focus: UiFocusHandle,
) -> Element {
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
    let close_rect = UiRect::new(
        card.right - 26.0,
        card.top + 4.0,
        card.right - 4.0,
        card.bottom - 4.0,
    );
    let count_rect = UiRect::new(
        close_rect.left - 72.0,
        card.top,
        close_rect.left - 2.0,
        card.bottom,
    );
    let mut count_style = theme::sans(c.text_muted, theme::SMALL + 1.0);
    count_style.align = TextAlign::Right;
    let field = UiRect::new(
        card.left + 8.0,
        card.top + 3.0,
        count_rect.left - 4.0,
        card.bottom - 3.0,
    );
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

/// One severity's toggle: its name, node key, icon, count, colour, and the
/// filter flag it switches.
type SeverityKind = (
    &'static str,
    &'static str,
    &'static str,
    usize,
    Color,
    fn(&mut Filter) -> &mut bool,
);

const SEVERITY_PAD: f32 = 6.0;
const SEVERITY_ICON: f32 = 16.0;

/// A severity's icon and count, which shows or hides that severity. Lit while
/// the severity is shown and dim while it is hidden; a fill appears only under
/// the pointer, so the row stays quiet.
#[allow(clippy::too_many_arguments)]
fn severity_toggle(
    key: &'static str,
    rect: UiRect,
    icon: &'static str,
    name: String,
    count: usize,
    color: Color,
    on: bool,
    click: impl Fn() + Send + Sync + 'static,
) -> Element {
    let node_id = UiId::owned(key);
    let mut semantics = Semantics::new(SemanticRole::Button).name(name);
    semantics.state.selected = on;
    let icon_slot = UiRect::new(
        rect.left + SEVERITY_PAD,
        rect.top,
        rect.left + SEVERITY_PAD + SEVERITY_ICON,
        rect.bottom,
    );
    let mut count_style = theme::mono(color, theme::SMALL);
    count_style.align = TextAlign::Left;
    let value = count.to_string();
    let count_rect = digits_centered(
        UiRect::new(icon_slot.right + 4.0, rect.top, rect.right, rect.bottom),
        &value,
        count_style,
    );
    Element::new(move |cx| {
        let flags = cx.context.interaction_flags(&node_id);
        let style = if flags.hovered || flags.pressed {
            VisualStyle::filled(theme::c().surface).radius(3.0)
        } else {
            VisualStyle::default()
        };
        UiElement::panel(node_id.clone(), rect, style).children(cx.children)
    })
    .key(key)
    .event_policy(EventPolicy::INTERACTIVE)
    .cursor(CursorIcon::Pointer)
    .semantics(semantics)
    .on_click(click)
    .child(
        Element::new(move |cx| {
            UiElement::icon(cx.id, theme::icon_rect(icon_slot), icon)
                .icon_style(IconStyle::new(color))
        })
        .key(format!("{key}-icon")),
    )
    .child(text(count_rect, value, count_style))
}

/// Logical pixels, per 10px of font size, that digits are moved down from the
/// layout's own baseline so their painted ink is centred. How far the painted
/// digits sit from the reported baseline depends on the monospace font the
/// platform resolves: measured on Windows it is one pixel; on Linux and macOS the
/// same placement was already centred to within a pixel.
const DIGIT_BASELINE_BIAS: f32 = if cfg!(windows) { 1.0 } else { 0.0 };

/// `rect` moved vertically so the digits of `value` sit on the rectangle's
/// middle line. A box centred around the font's whole line puts digits high or
/// low, because they have neither ascenders nor descenders; this places them by
/// their own height, so they line up with an icon beside them.
fn digits_centered(rect: UiRect, value: &str, style: lgui::prelude::TextStyle) -> UiRect {
    use lgui::text::{self as shaping, TextLayoutRequest};
    let Some(baseline) = shaping::layout(&TextLayoutRequest::for_style(value, rect, &style))
        .and_then(|layout| layout.lines().first().map(|line| line.baseline))
    else {
        return rect;
    };
    // Digits are about as tall as capitals: 0.72 of the font size.
    let wanted = (rect.top + rect.bottom) / 2.0 + style.height * 0.72 / 2.0;
    // The baseline the layout reports lies a little below where the glyphs are
    // painted; the rendered pixels of a 10px count were measured one pixel high
    // without this (see `the_count_is_painted_on_the_togglers_middle_line`).
    rect.translate(
        0.0,
        wanted - baseline + DIGIT_BASELINE_BIAS * style.height / 10.0,
    )
}

fn problems_toolbar(
    free: UiRect,
    bar: UiRect,
    state: &AppContext,
    panel_state: &LogPanel,
    counts: problems::Counts,
) -> Element {
    let c = theme::c();
    let tools = header::dock_tools(bar, 1);
    let mut root = toolbar_root(bar, "problems-toolbar").child(header::rule(bar, tools.rule));
    let _ = free;
    let top = bar.top + (bar.height() - 22.0) / 2.0;
    // The three severities are icons with their counts at the right end, beside the
    // tools, rather than labelled chips on the left: right to left, so the order on
    // screen is errors, warnings, information.
    let mut right = tools.slots[0].left - 6.0;
    let kinds: [SeverityKind; 3] = [
        (
            "Information",
            "problems-infos",
            "info",
            counts.infos,
            c.accent,
            |f| &mut f.infos,
        ),
        (
            "Warnings",
            "problems-warnings",
            "warning",
            counts.warnings,
            c.warning,
            |f| &mut f.warnings,
        ),
        (
            "Errors",
            "problems-errors",
            "problem-error",
            counts.errors,
            c.error_text,
            |f| &mut f.errors,
        ),
    ];
    for (name, key, icon, count, color, field) in kinds {
        let mut filter = panel_state.severities;
        let on = *field(&mut filter);
        let width = SEVERITY_PAD * 2.0
            + SEVERITY_ICON
            + 4.0
            + crate::ui::tabs::measure(&count.to_string(), theme::SMALL, 400);
        let rect = UiRect::new(right - width, top, right, top + 22.0);
        right = rect.left - 2.0;
        let click = state.clone();
        root = root.child(severity_toggle(
            key,
            rect,
            icon,
            format!("{name}: {count}"),
            count,
            if on { color } else { c.text_faint },
            on,
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
            UiRect::new(
                body.left + 12.0,
                body.top + 8.0,
                body.right - 12.0,
                body.top + 28.0,
            ),
            if groups.is_empty() {
                "No problems in the open files"
            } else {
                "Nothing matches the filter"
            },
            theme::sans(c.text_faint, theme::UI_SIZE),
        ));
    }
    let (scroll, max_scroll) = scroll_extent(
        rows.len(),
        body.height(),
        panel_state.problems_scroll,
        false,
    );
    let first = ((scroll - PAD_Y) / ROW_H).floor().max(0.0) as usize;
    let last = (first + (body.height() / ROW_H).ceil() as usize + 2).min(rows.len());
    let mut content = clip(body, 0.0, -scroll);
    let roots = state.get().explorer.model.roots().to_vec();
    for (index, row) in rows.iter().enumerate().take(last).skip(first) {
        let top = body.top + PAD_Y + index as f32 * ROW_H;
        let rect = UiRect::new(body.left, top, body.right, top + ROW_H);
        content = content.child(match row {
            Row::File {
                path,
                count,
                collapsed,
            } => {
                let name = path
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
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
                        if *collapsed {
                            "chevron-right"
                        } else {
                            "chevron-down"
                        },
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
                let (path, offset, span) = (path.clone(), problem.offset, problem.span);
                let mut message = problem
                    .message
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                if let Some(source) = &problem.source {
                    message = match &problem.code {
                        Some(code) => format!("{message}  {source}({code})"),
                        None => format!("{message}  {source}"),
                    };
                }
                data_row(
                    format!("problems-item-{index}"),
                    rect,
                    (
                        severity_icon(problem.severity),
                        severity_color(problem.severity),
                    ),
                    &message,
                    None,
                    format!("{}:{}", problem.line, problem.column),
                    c.text,
                    false,
                    move || {
                        let place = match span {
                            // A file that is not open: go there in the service's own terms.
                            Some(span) => super::language_ui::navigate::Place::span(
                                open_spelling(&jump, &path),
                                span,
                            ),
                            None => super::language_ui::navigate::Place::bytes(
                                path.clone(),
                                offset..offset,
                            ),
                        };
                        super::language_ui::navigate::go(&jump, place, editor, true);
                        // The caret shows only in the focused editor.
                        focus.focus();
                    },
                )
            }
        });
    }
    let wheel_state = state.clone();
    let viewport = backdrop
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
    viewport.child(scrollbar(
        body,
        state,
        Pane::Problems,
        scroll,
        max_scroll,
        panel_state,
    ))
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
    let text_right = (rect.right - note_w - 10.0).max(text_left);
    let style = if bold {
        theme::sans_semibold(tint, theme::UI_SIZE)
    } else {
        theme::sans(tint, theme::UI_SIZE)
    };
    let (glyph, glyph_color) = icon;
    let (icon_rect, glyph) = (
        theme::icon_rect(UiRect::new(
            rect.left + 10.0,
            rect.top,
            rect.left + 28.0,
            rect.bottom,
        )),
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
        // Over the scrollbar's lane: it overlays the row and takes no room.
        UiRect::new(
            rect.right - note_w - 8.0,
            rect.top,
            rect.right - 8.0,
            rect.bottom,
        ),
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
    let channels = output::hub().channels();
    let current = panel_state.level(&channels, panel_state.shown(&channels).as_ref());
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
                checked: current == level,
                heading: false,
                run: Some(Arc::new(move || {
                    set(&state, move |p| {
                        p.min_level = Some(level);
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
    let top = anchor
        .1
        .clamp(viewport.top, (viewport.bottom - height).max(viewport.top));
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
        let row = UiRect::new(
            card.left + 1.0,
            row_top,
            card.right - 1.0,
            row_top + MENU_ITEM_H,
        );
        let label = format!("{}{}", if item.checked { "✓ " } else { "  " }, item.label);
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
                span: None,
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
        render_session(tab, width, height, open_find, true)
            .tree()
            .nodes()
            .iter()
            .filter_map(|n| n.semantics.as_ref().and_then(|s| s.name.clone()))
            .collect()
    }

    fn render_session(
        tab: PanelTab,
        width: f32,
        height: f32,
        open_find: bool,
        menu: bool,
    ) -> lgui::session::UiSession {
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
                app.log_panel.menu = menu.then_some(Menu::Channels((20.0, 60.0)));
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
    }

    #[test]
    fn the_dock_switcher_keeps_its_hand_cursor_over_every_panel() {
        for tab in [PanelTab::Output, PanelTab::Problems] {
            let session = render_session(tab, 900.0, 260.0, false, false);
            for id in [
                "bottom-panel-terminal",
                "bottom-panel-commits",
                "bottom-panel-output",
                "bottom-panel-problems",
            ] {
                let rect = session.tree().node(&UiId::new(id)).unwrap().layout_rect;
                let centre = lgui::core::Point::new(
                    (rect.left + rect.right) / 2.0,
                    (rect.top + rect.bottom) / 2.0,
                );
                assert_eq!(
                    session.tree().cursor_at(centre),
                    Some(CursorIcon::Pointer),
                    "{id} on {tab:?}"
                );
            }
        }
    }

    #[test]
    fn both_panels_render_at_wide_and_narrow_sizes() {
        output::app_log(Level::Error, "render smoke");
        for (width, height) in [
            (900.0, 260.0),
            (520.0, 200.0),
            (300.0, 120.0),
            (900.0, 30.0),
        ] {
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
        for tool in [
            "Lowest level shown",
            "Clear this channel",
            "Filter lines",
            "More actions",
        ] {
            assert!(roomy.iter().any(|n| n == tool), "missing {tool}: {roomy:?}");
        }
        let narrow = render_names(PanelTab::Output, 520.0, 260.0, false);
        assert!(narrow.iter().any(|n| n == "Filter lines"));
        assert!(narrow.iter().any(|n| n == "More actions"));
        assert!(!narrow.iter().any(|n| n == "Clear this channel"));
    }

    #[test]
    fn problems_shows_its_three_severities_as_icons_at_the_right_end() {
        for width in [900.0, 400.0] {
            let names = render_names(PanelTab::Problems, width, 260.0, false);
            for severity in ["Errors:", "Warnings:", "Information:"] {
                assert!(
                    names.iter().any(|n| n.starts_with(severity)),
                    "{severity} at {width}: {names:?}"
                );
            }
            assert!(names.iter().any(|n| n == "Only the active file"));
        }
    }

    /// Renders the Problems header and returns, for the toggle called `name`, its
    /// rectangle and the vertical extent of the pixels its count painted.
    fn painted_digits(name: &str) -> (UiRect, (u32, u32), (u32, u32)) {
        use crate::app::state::AppState;
        use crate::app::terminal_binding::TerminalTabs;
        use lgui::application::{AppView, ApplicationContext};
        use lgui::core::{PhysicalRect, UiScale};
        use lgui::renderer::{FrameInfo, FrameReason};
        use lgui::session::UiSession;
        let context = ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        let viewport = UiRect::new(0.0, 0.0, 600.0, 140.0);
        let dock = UiRect::new(0.0, 40.0, 600.0, 140.0);
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(|| {
                let mut app = AppState::new();
                app.terminal_chrome.show_terminal = true;
                app.terminal.ui.panel_tab = PanelTab::Problems;
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
            lgui::prelude::group(viewport).child(render(
                dock,
                PanelTab::Problems,
                state,
                tabs,
                editor_focus,
                focus,
                Arc::new(ApplicationHandle::new(|task| task(), || {})),
                None,
                100.0,
                UiRect::new(0.0, 0.0, 600.0, 40.0),
            ))
        });
        let mut session = UiSession::new();
        let scale = UiScale::ONE;
        session.render_view(&view, viewport, scale);
        let toggle = session
            .tree()
            .nodes()
            .iter()
            .find(|n| n.semantics.as_ref().and_then(|s| s.name.as_deref()) == Some(name))
            .unwrap_or_else(|| panic!("missing {name}"))
            .layout_rect;
        let physical = scale.physical_rect(viewport);
        let mut surface = lgui::render_skia::SkiaSoftwareSurface::new(4 * 1024 * 1024);
        let dirty = [PhysicalRect::new(0, 0, physical.right, physical.bottom)];
        let frame = FrameInfo::new(physical, &dirty, scale, FrameReason::SceneChange, true);
        lgui::assets::backend::with_render_resources(
            &context,
            lgui::RenderResources::new(),
            || surface.draw(&session.tree().scene().project_to_physical(scale), &frame),
        )
        .unwrap();
        let width = physical.width() as usize;
        let pixel = |x: u32, y: u32| {
            let at = (y as usize * width + x as usize) * 4;
            [
                surface.pixels()[at],
                surface.pixels()[at + 1],
                surface.pixels()[at + 2],
            ]
        };
        // The count sits right of the 16px icon slot that starts 6px in; anything that
        // differs from the header's fill there is the digits.
        let (left, right) = ((toggle.left + 6.0 + 16.0 + 3.0) as u32, toggle.right as u32);
        let fill = pixel(left, toggle.top as u32 + 1);
        let mut rows = Vec::new();
        for y in toggle.top as u32..toggle.bottom as u32 {
            if (left..right).any(|x| pixel(x, y) != fill) {
                rows.push(y);
            }
        }
        let digits = (*rows.first().unwrap_or(&0), *rows.last().unwrap_or(&0));
        let mut columns = Vec::new();
        for x in left..right {
            if (toggle.top as u32..toggle.bottom as u32).any(|y| pixel(x, y) != fill) {
                columns.push(x);
            }
        }
        let ink = (
            *columns.first().unwrap_or(&0),
            *columns.last().unwrap_or(&0),
        );
        (toggle, digits, ink)
    }

    #[test]
    fn the_count_is_painted_on_the_togglers_middle_line() {
        for name in ["Errors: 0", "Warnings: 0", "Information: 0"] {
            let (toggle, (top, bottom), _) = painted_digits(name);
            let middle = (toggle.top + toggle.bottom) / 2.0;
            let painted = (top + bottom + 1) as f32 / 2.0;
            // Fonts differ by platform, so the counts are held to the nearest pixel.
            assert!(
                (painted - middle).abs() <= 0.75,
                "{name}: digits span rows {top}..={bottom}, centre {painted}; toggle {toggle:?}, middle {middle}"
            );
        }
    }

    #[test]
    fn digits_are_placed_by_their_own_height_on_the_middle_line() {
        use lgui::application::ApplicationContext;
        use lgui::text::{self as shaping, TextLayoutRequest};
        let context = ApplicationContext::empty(Default::default());
        let _fonts = lgui::backend::install_text_environment(
            &context,
            lgui::render_skia::skia_text_system_handle(),
        );
        let style = theme::mono(theme::c().text, theme::SMALL);
        assert_eq!(
            style.height,
            theme::SMALL,
            "the style's height is the font size"
        );
        let rect = UiRect::new(10.0, 0.0, 60.0, 22.0);
        let moved = digits_centered(rect, "12", style);
        let baseline = shaping::layout(&TextLayoutRequest::for_style("12", moved, &style))
            .and_then(|layout| layout.lines().first().map(|line| line.baseline))
            .expect("the text lays out");
        let expected = 11.0 + theme::SMALL * 0.72 / 2.0 + DIGIT_BASELINE_BIAS;
        assert!(
            (baseline - expected).abs() < 0.01,
            "{baseline} vs {expected}"
        );
        // Only vertical movement: the digits keep their column.
        assert_eq!((moved.left, moved.right), (rect.left, rect.right));
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
        let roots = vec![PathBuf::from(r"D:\Documents\cprojects\loog")];
        assert_eq!(
            readable_path(
                &roots,
                std::path::Path::new(r"\\?\D:\Documents\cprojects\loog\src\main.rs")
            ),
            "src/main.rs"
        );
        // A verbatim root matches a plain path too.
        let verbatim = vec![PathBuf::from(r"\\?\D:\Documents\cprojects\loog")];
        assert_eq!(
            readable_path(
                &verbatim,
                std::path::Path::new(r"D:\Documents\cprojects\loog\a.rs")
            ),
            "a.rs"
        );
        // Outside every folder: the whole path, cleaned.
        assert_eq!(
            readable_path(&roots, std::path::Path::new(r"\\?\E:\other\b.rs")),
            "E:/other/b.rs"
        );
        // A sibling folder with the same prefix is not inside the root.
        assert_eq!(
            readable_path(
                &roots,
                std::path::Path::new(r"D:\Documents\cprojects\loog2\c.rs")
            ),
            "D:/Documents/cprojects/loog2/c.rs"
        );
    }

    /// Renders the Output panel on `channel` in a 600x300 dock below a 40px editor
    /// strip, with the window-level hover tracker, and hands back what drives it.
    fn scroll_fixture(
        channel: &'static str,
    ) -> (
        lgui::application::AppView,
        std::sync::Arc<std::sync::Mutex<Option<AppContext>>>,
    ) {
        use crate::app::state::AppState;
        use crate::app::terminal_binding::TerminalTabs;
        use lgui::application::AppView;
        let exposed = Arc::new(std::sync::Mutex::new(None));
        let output = exposed.clone();
        let viewport = UiRect::new(0.0, 0.0, 600.0, 340.0);
        let dock = UiRect::new(0.0, 40.0, 600.0, 340.0);
        let body = UiRect::new(0.0, 40.0 + theme::TABS_H, 600.0, 340.0);
        let view: AppView = Arc::new(move |cx| {
            let state = AppContext::detached(cx.state_with(|| {
                let mut app = AppState::new();
                app.terminal_chrome.show_terminal = true;
                app.terminal.ui.panel_tab = PanelTab::Output;
                app.log_panel.channel = Some(ChannelId::new(channel));
                app.log_panel.min_level = Some(Level::Trace);
                app
            }));
            *output.lock().unwrap() = Some(state.clone());
            let tabs = crate::app::terminal_binding::test_store(
                cx,
                Some(state.clone()),
                TerminalTabs::new,
            );
            let id = cx.use_stable_id();
            let focus = cx.focus_handle(id);
            let editor_id = cx.use_stable_id();
            let editor_focus = cx.focus_handle(editor_id);
            let root = lgui::prelude::group(viewport).child(render(
                dock,
                PanelTab::Output,
                state.clone(),
                tabs,
                editor_focus,
                focus,
                Arc::new(ApplicationHandle::new(|task| task(), || {})),
                None,
                300.0,
                UiRect::new(0.0, 0.0, 600.0, 40.0),
            ));
            track_hover(root, &state, Some(body))
        });
        (view, exposed)
    }

    #[test]
    fn the_scrollbar_overlays_fades_with_the_pointer_and_drags_beyond_the_list() {
        use lgui::application::ApplicationContext;
        use lgui::core::{InputEvent, Point, PointerData, UiScale, dispatch_runtime_output};
        use lgui::session::UiSession;
        let long = ChannelId::new("scroll-test-long");
        let short = ChannelId::new("scroll-test-short");
        for n in 0..300 {
            output::hub().append(&long, Level::Info, &format!("line {n}"));
        }
        output::hub().append(&short, Level::Info, "only");

        let (view, exposed) = scroll_fixture("scroll-test-long");
        let viewport = UiRect::new(0.0, 0.0, 600.0, 340.0);
        let mut session = UiSession::new();
        session.render_view(&view, viewport, UiScale::ONE);
        let state = exposed.lock().unwrap().clone().unwrap();
        let context = ApplicationContext::empty(Default::default());
        let send = |session: &mut UiSession, input| {
            dispatch_runtime_output(
                session.handle_input(input),
                &context,
                &lgui::window::WindowId::new("log-scrollbar-test"),
                |action| session.handle_default_action(action),
                |_| {},
            );
            session.render_view(&view, viewport, UiScale::ONE);
        };
        // The thumb is the filled node 8px wide, inset 2px from the right edge.
        let thumb = |session: &UiSession| {
            session
                .tree()
                .nodes()
                .iter()
                .find(|node| {
                    let rect = node.layout_rect;
                    node.style.fill.is_some()
                        && (rect.right - 598.0).abs() < 0.01
                        && (rect.width() - 8.0).abs() < 0.01
                })
                .map(|node| node.layout_rect)
        };
        let body_top = 40.0 + theme::TABS_H;

        // Following the newest line puts the thumb at the bottom of a track that
        // lies along the right edge and takes no room from the text.
        let at_bottom = thumb(&session).expect("a long list has a scrollbar");
        assert!(at_bottom.right <= 600.0 && at_bottom.right > 600.0 - 12.0);
        assert!(at_bottom.bottom > 330.0, "{at_bottom:?}");
        assert!(!state.get().log_panel.hovered);

        // The scrollbar is stronger while the pointer is over the list.
        let inside = PointerData::mouse(Point::new(300.0, body_top + 40.0));
        send(&mut session, InputEvent::PointerMove(inside));
        assert!(state.get().log_panel.hovered);
        send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(Point::new(300.0, 10.0))),
        );
        assert!(!state.get().log_panel.hovered, "leaving the list fades it");

        // Taking hold of the thumb and dragging above the list scrolls to the top
        // and stops following; the drag survives the pointer leaving the list.
        let grab = PointerData::mouse(Point::new(
            (at_bottom.left + at_bottom.right) * 0.5,
            at_bottom.top + 4.0,
        ));
        send(
            &mut session,
            InputEvent::PointerDown {
                pointer: grab,
                button: PointerButton::Left,
            },
        );
        assert!(state.get().log_panel.scrollbar_drag.is_some());
        send(
            &mut session,
            InputEvent::PointerMove(PointerData::mouse(Point::new(300.0, 0.0))),
        );
        let panel = state.get().log_panel;
        assert_eq!(panel.scroll, 0.0);
        assert!(!panel.follow);
        assert!(thumb(&session).unwrap().top < at_bottom.top);
        send(
            &mut session,
            InputEvent::PointerUp {
                pointer: grab,
                button: PointerButton::Left,
            },
        );
        assert!(state.get().log_panel.scrollbar_drag.is_none());

        // Pressing the empty track jumps there, thumb under the pointer, and
        // reaching the bottom follows the newest line again.
        let press = PointerData::mouse(Point::new(599.0, 335.0));
        send(
            &mut session,
            InputEvent::PointerDown {
                pointer: press,
                button: PointerButton::Left,
            },
        );
        assert!(state.get().log_panel.follow);
        send(
            &mut session,
            InputEvent::PointerUp {
                pointer: press,
                button: PointerButton::Left,
            },
        );

        // A list that fits has no scrollbar.
        state.update(|app| app.log_panel.channel = Some(short.clone()));
        session.render_view(&view, viewport, UiScale::ONE);
        assert!(thumb(&session).is_none());
    }

    #[test]
    fn a_process_channel_opens_at_everything_until_the_person_chooses() {
        let info = |id: &str, group| ChannelInfo {
            id: ChannelId::new(id),
            title: id.into(),
            group,
            unread_error: false,
            len: 0,
        };
        let channels = [
            info("ext", output::Group::Extension),
            info("proc", output::Group::Process),
        ];
        let mut panel = LogPanel::default();
        assert_eq!(
            panel.level(&channels, Some(&ChannelId::new("ext"))),
            Level::Info
        );
        assert_eq!(
            panel.level(&channels, Some(&ChannelId::new("proc"))),
            Level::Trace
        );
        assert_eq!(panel.level(&channels, None), Level::Info);
        panel.min_level = Some(Level::Warn);
        assert_eq!(
            panel.level(&channels, Some(&ChannelId::new("proc"))),
            Level::Warn
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
