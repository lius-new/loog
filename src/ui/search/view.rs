//! Workspace search in the existing drawer, following the reviewed HTML design.
use std::sync::Arc;

use lgui::core::{
    CursorIcon, EventPolicy, PointerButton, SemanticRole, Semantics, TextStyle, UiElement,
    UiFocusHandle, UiId, WheelUnit, clip,
};
use lgui::prelude::{Element, UiRect, VisualStyle, panel, text};
use unicode_segmentation::UnicodeSegmentation;

use super::commands::{self as search_actions, Command, Commands, FILE_ROW_H, MATCH_ROW_H};
use super::state::SearchPanelState;
use crate::features::search::{FileMatches, Results};
use crate::ui::components::input::{self, InputBinding, InputState, InputStyle};
use crate::ui::components::scrollbar;
use crate::ui::components::search_field;
use crate::ui::components::state_access::StateAccess;
use crate::ui::components::text::SingleLineText;
use crate::ui::drawer::drawer_icon;
use crate::ui::theme;

// Search uses the compact UI scale for dense result lists.
const TEXT_SIZE: f32 = theme::SMALL;
const META_SIZE: f32 = 9.0;
const INPUT_H: f32 = search_field::HEIGHT;

fn file_labels(file: &FileMatches, multiple_roots: bool) -> (String, String) {
    if let Some(labels) = &file.provider {
        let parent = labels
            .relative
            .get(..labels.relative.len().saturating_sub(1))
            .unwrap_or_default()
            .join("/");
        let context = if multiple_roots {
            if parent.is_empty() {
                labels.root_name.clone()
            } else {
                format!("{}/{parent}", labels.root_name)
            }
        } else {
            parent
        };
        return (labels.relative.join("/"), context);
    }
    let (Some(path), Some(root)) = (file.path.local_path(), file.root.local_path()) else {
        return (file.path.display().to_string(), String::new());
    };
    // Buffer paths may use Windows' verbatim prefix while search roots do not.
    // Normalize both lexically; drawing rows must not access the filesystem.
    let path = std::path::PathBuf::from(crate::ui::display_path(path));
    let root = std::path::PathBuf::from(crate::ui::display_path(root));
    let in_root = path.strip_prefix(&root).ok();
    let relative = in_root.unwrap_or(&path);
    let parent = relative.parent().unwrap_or(std::path::Path::new(""));
    let context = if multiple_roots && in_root.is_some() {
        let mut context = root
            .file_name()
            .map(std::path::PathBuf::from)
            .unwrap_or_default();
        if !parent.as_os_str().is_empty() {
            context.push(parent);
        }
        context
    } else {
        parent.to_path_buf()
    };
    let labels = (
        relative.to_string_lossy().into_owned(),
        context.to_string_lossy().into_owned(),
    );
    #[cfg(windows)]
    let labels = (labels.0.replace('\\', "/"), labels.1.replace('\\', "/"));
    labels
}

#[derive(Clone)]
pub struct Inputs {
    pub ids: [UiId; 3],
    pub focus: [UiFocusHandle; 3],
    pub results_id: UiId,
    pub results_focus: UiFocusHandle,
}

fn fit_text(value: &str, rect: UiRect, style: TextStyle) -> Element {
    // Measure and paint on a wide single line, then clip to the control bounds.
    // A narrow Skia paragraph can otherwise wrap and drop text after its first line.
    let wide = UiRect::new(rect.left, rect.top, rect.left + 100_000.0, rect.bottom);
    let measured = SingleLineText::new(value, wide, style);
    if measured.width() <= rect.width() {
        return clip(rect, 0.0, 0.0).child(measured.element());
    }
    let boundaries = value
        .grapheme_indices(true)
        .map(|(at, _)| at)
        .collect::<Vec<_>>();
    let mut low = 0;
    let mut high = boundaries.len();
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        let end = boundaries.get(mid).copied().unwrap_or(value.len());
        if SingleLineText::new(format!("{}…", &value[..end]), wide, style).width() <= rect.width()
        {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    let end = boundaries.get(low).copied().unwrap_or(value.len());
    clip(rect, 0.0, 0.0).child(text(wide, format!("{}…", &value[..end]), style))
}

fn control_panel(rect: UiRect, active: bool) -> Element {
    let style = if active {
        VisualStyle::filled(theme::c().accent).alpha(28).radius(3.0)
    } else {
        VisualStyle::default()
    };
    panel(rect, style)
        .event_policy(EventPolicy {
            hover: true,
            press: true,
            focus: false,
        })
        .cursor(CursorIcon::Pointer)
}

fn icon_button(
    rect: UiRect,
    icon: &'static str,
    name: &'static str,
    active: bool,
    click: impl Fn() + Send + Sync + 'static,
) -> Element {
    let mut semantics = Semantics::new(SemanticRole::Button).name(name);
    semantics.state.selected = active;
    control_panel(rect, active)
        .key(format!("search-button-{name}"))
        .semantics(semantics)
        .on_click(click)
        .child(drawer_icon(
            UiId::owned(format!("search-icon-{name}")),
            icon,
            theme::icon_rect(rect),
            if active {
                theme::c().accent
            } else {
                theme::c().text_soft
            },
        ))
}

fn filter_count(app: &SearchPanelState) -> usize {
    usize::from(!app.ui.include.text().trim().is_empty())
        + usize::from(!app.ui.exclude.text().trim().is_empty())
        + usize::from(app.ui.include_hidden)
        + usize::from(app.ui.include_ignored)
}

/// Each option has a 24px target, a 2px gap and a 6px right inset.
fn query_option_rects(query: UiRect) -> Option<[UiRect; 3]> {
    if query.width() < 132.0 {
        return None;
    }
    let first = query.right - 6.0 - 76.0;
    Some(std::array::from_fn(|index| {
        let left = first + index as f32 * 26.0;
        UiRect::new(left, query.top + 2.0, left + 24.0, query.bottom - 2.0)
    }))
}

pub fn render(
    rect: UiRect,
    state: StateAccess<SearchPanelState>,
    commands: Commands,
    has_roots: bool,
    multiple_roots: bool,
    inputs: Inputs,
    header_element: Element,
) -> Element {
    let app = state.get();
    let search = &app;
    let header = UiRect::new(rect.left, rect.top, rect.right, rect.top + theme::TABS_H);
    let mut root = panel(rect, VisualStyle::filled(theme::c().sidebar)).child(header_element);

    // Mode icons and collapse stay in the shared drawer header.
    if rect.width() >= 200.0 && !search.query.text().is_empty() {
        if search.model.status().running || (search.model.status().error.is_none() && has_roots) {
            let action = state.clone();
            let action_commands = commands.clone();
            root = root.child(icon_button(
                UiRect::new(
                    rect.right - 96.0,
                    rect.top,
                    rect.right - 68.0,
                    header.bottom,
                ),
                if search.model.status().running {
                    "search-stop"
                } else {
                    "refresh"
                },
                if search.model.status().running {
                    "Stop search"
                } else {
                    "Refresh search"
                },
                false,
                move || {
                    if action.get().model.status().running {
                        action_commands.dispatch(Command::Cancel);
                    } else {
                        action_commands.dispatch(Command::Refresh);
                    }
                },
            ));
        }
        let clear = commands.clone();
        let query_focus = inputs.focus[0].clone();
        root = root.child(icon_button(
            UiRect::new(
                rect.right - 64.0,
                rect.top,
                rect.right - 36.0,
                header.bottom,
            ),
            "search-clear",
            "Clear search",
            false,
            move || {
                clear.dispatch(Command::Clear);
                query_focus.focus();
            },
        ));
    }

    let left = rect.left + 12.0;
    let right = (rect.right - 12.0).max(left);
    let mut top = header.bottom + 8.0;
    let query = UiRect::new(left, top, right, top + INPUT_H);
    let options = query_option_rects(query);
    let input_right = options
        .as_ref()
        .map_or(right - 4.0, |areas| areas[0].left - 2.0);
    let submit = commands.clone();
    let cancel = commands.clone();
    let mut field_options = search_field::Options::new("Search workspace");
    field_options.trailing_width = query.right - input_right;
    field_options.invalid = search.model.status().error.is_some();
    field_options.on_submit = Some(Arc::new(move || submit.dispatch(Command::Refresh)));
    field_options.on_cancel = Some(Arc::new(move || cancel.dispatch(Command::Cancel)));
    root = root.child(search_field::render(
        query,
        inputs.ids[0].clone(),
        inputs.focus[0].clone(),
        InputBinding::new_access(state.clone(), |app| &app.ui.query, |app| &mut app.ui.query),
        field_options,
    ));
    if let Some(areas) = options {
        for (index, icon, name, active) in [
            (
                0,
                "search-case",
                "Match case (Alt+C)",
                search.case_sensitive,
            ),
            (
                1,
                "search-word",
                "Match whole word (Alt+W)",
                search.whole_word,
            ),
            (
                2,
                "search-regex",
                "Use regular expression (Alt+R)",
                search.regex,
            ),
        ] {
            let toggle = state.clone();
            root = root.child(icon_button(areas[index], icon, name, active, move || {
                toggle.update(|app| match index {
                    0 => app.ui.case_sensitive = !app.ui.case_sensitive,
                    1 => app.ui.whole_word = !app.ui.whole_word,
                    _ => app.ui.regex = !app.ui.regex,
                });
            }));
        }
    }
    top += INPUT_H;
    let disclosure = UiRect::new(left, top + 2.0, (left + 84.0).min(right), top + 26.0);
    let filters = state.clone();
    let focus = inputs.focus[0].clone();
    let filter_button = control_panel(disclosure, false)
        .key("search-filter-disclosure")
        .semantics({
            let mut semantics = Semantics::new(SemanticRole::Button).name("Filters");
            semantics.state.expanded = Some(search.filters_open);
            semantics
        })
        .on_click(move || {
            let closing_input = filters.get().filters_open
                && (filters.get().include.focused || filters.get().exclude.focused);
            filters.update(|app| search_actions::toggle_filters(&mut app.ui));
            if closing_input {
                focus.focus();
            }
        })
        .child(drawer_icon(
            "search-filter-chevron",
            if search.filters_open {
                "chevron-down"
            } else {
                "chevron-right"
            },
            UiRect::new(left, top + 8.0, left + 12.0, top + 20.0),
            theme::c().text_muted,
        ))
        .child(text(
            UiRect::new(left + 18.0, top, left + 54.0, top + 28.0),
            "Filters",
            theme::sans(theme::c().text_soft, TEXT_SIZE),
        ));
    root = root.child(filter_button);
    let active_filters = filter_count(&app);
    if active_filters > 0 {
        let badge = UiRect::new(left + 50.0, top + 7.0, left + 64.0, top + 21.0);
        root = root
            .child(panel(
                badge,
                VisualStyle::filled(theme::c().accent).alpha(24).radius(8.0),
            ))
            .child(text(
                badge,
                active_filters.to_string(),
                theme::sans(theme::c().accent, META_SIZE).centered(),
            ));
        let reset = commands.clone();
        root = root.child(icon_button(
            UiRect::new(right - 24.0, top + 2.0, right, top + 26.0),
            "search-clear",
            "Reset filters",
            false,
            move || reset.dispatch(Command::ResetFilters),
        ));
    }
    top += 28.0;
    if search.filters_open {
        for (index, label) in [(1, "Files to include"), (2, "Files to exclude")] {
            root = root.child(text(
                UiRect::new(left, top, right, top + 14.0),
                label,
                theme::sans(theme::c().text_soft, TEXT_SIZE),
            ));
            top += 14.0;
            let focused = if index == 1 {
                search.include.focused
            } else {
                search.exclude.focused
            };
            root = root.child(
                theme::bordered(
                    UiRect::new(left, top, right, top + INPUT_H),
                    theme::c().surface,
                    if focused {
                        theme::c().accent
                    } else {
                        theme::c().border
                    },
                    3.0,
                    1.0,
                )
                .child(search_input(
                    UiRect::new(
                        left + 2.0,
                        top + 1.0,
                        (right - 2.0).max(left + 2.0),
                        top + INPUT_H - 1.0,
                    ),
                    state.clone(),
                    commands.clone(),
                    inputs.ids[index].clone(),
                    inputs.focus[index].clone(),
                    index,
                )),
            );
            top += INPUT_H + 8.0;
        }
        for (index, label, checked) in [
            (0, "Search hidden files", search.include_hidden),
            (1, "Search ignored files", search.include_ignored),
        ] {
            let toggle = state.clone();
            root = root.child(checkbox(
                UiRect::new(left, top, right, top + 22.0),
                label,
                checked,
                move || {
                    toggle.update(|app| {
                        if index == 0 {
                            app.ui.include_hidden = !app.ui.include_hidden
                        } else {
                            app.ui.include_ignored = !app.ui.include_ignored
                        }
                    });
                },
            ));
            top += 22.0;
        }
        top += 6.0;
        root = root.child(panel(
            UiRect::new(left, top, right, top + 1.0),
            VisualStyle::filled(theme::c().border),
        ));
        top += 4.0;
    }

    if let Some(error) = &search.model.status().error {
        let area = UiRect::new(left, top, right, top + 26.0);
        root = root.child(control_panel(area, false).child(fit_text(
            error.lines().last().unwrap_or(error),
            area,
            theme::sans(theme::c().error_text, TEXT_SIZE),
        )));
        top += 26.0;
    } else if !search.query.text().is_empty() && has_roots {
        let prefix = if search.model.status().running {
            "Searching… "
        } else if search.model.status().cancelled {
            "Stopped · "
        } else {
            ""
        };
        let result_label = if search.model.status().results.matches == 1 {
            "result"
        } else {
            "results"
        };
        let file_label = if search.model.status().results.files.len() == 1 {
            "file"
        } else {
            "files"
        };
        let status = if rect.width() > 260.0 {
            format!(
                "{prefix}{} {result_label} in {} {file_label}",
                number(search.model.status().results.matches),
                number(search.model.status().results.files.len())
            )
        } else {
            format!(
                "{prefix}{} {result_label}",
                number(search.model.status().results.matches)
            )
        };
        let summary = UiRect::new(left, top, (right - 28.0).max(left), top + 28.0);
        root = root.child(fit_text(
            &status,
            summary,
            theme::sans(theme::c().text_soft, TEXT_SIZE),
        ));
        if !search.model.status().results.files.is_empty() {
            let collapse = state.clone();
            root = root.child(icon_button(
                UiRect::new(right - 24.0, top + 2.0, right, top + 26.0),
                "search-collapse",
                "Collapse or expand all results",
                false,
                move || {
                    collapse.update(|app| {
                        if app
                            .model
                            .status()
                            .results
                            .files
                            .iter()
                            .all(|f| app.ui.collapsed.contains(&f.path))
                        {
                            app.ui.collapsed.clear();
                        } else {
                            for file in &app.model.status().results.files {
                                app.ui.collapsed.insert(file.path.clone());
                            }
                        }
                    })
                },
            ));
        }
        top += 28.0;
        if search.model.status().results.limit_hit {
            root = root.child(fit_text(
                "Result limit reached. Narrow your search.",
                UiRect::new(left, top, right, top + 26.0),
                theme::sans(theme::c().warning, META_SIZE),
            ));
            top += 26.0;
        }
    }

    let footer_height = if search.model.status().error.is_none()
        && (search.model.status().results.skipped > 0 || search.model.status().results.errors > 0)
    {
        26.0 + if search.details_open {
            detail_lines(&search.model.status().results).len() as f32 * 18.0 + 6.0
        } else {
            0.0
        }
    } else {
        0.0
    };
    // A short window keeps the controls clipped and the results viewport ordered.
    let bottom = (rect.bottom - footer_height).max(top.min(rect.bottom));
    let viewport = UiRect::new(rect.left, (top + 2.0).min(bottom), rect.right, bottom);
    root = root.child(results(
        viewport,
        state.clone(),
        commands,
        has_roots,
        multiple_roots,
        inputs,
        search.model.status().results.clone(),
    ));
    if footer_height > 0.0 && bottom < rect.bottom {
        root = root.child(footer(
            UiRect::new(rect.left, bottom, rect.right, rect.bottom),
            &search.model.status().results,
            search.details_open,
            state.clone(),
        ));
    }
    clip(rect, 0.0, 0.0).child(root)
}

fn number(value: usize) -> String {
    let raw = value.to_string();
    let mut out = String::with_capacity(raw.len() + raw.len() / 3);
    for (index, ch) in raw.chars().enumerate() {
        if index > 0 && (raw.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn checkbox(
    rect: UiRect,
    label: &'static str,
    checked: bool,
    click: impl Fn() + Send + Sync + 'static,
) -> Element {
    let mut semantics = Semantics::new(SemanticRole::CheckBox).name(label);
    semantics.state.checked = Some(checked);
    let square = UiRect::new(rect.left, rect.top + 5.0, rect.left + 12.0, rect.top + 17.0);
    let mut row = control_panel(rect, false)
        .key(label)
        .semantics(semantics)
        .on_click(click)
        .child(theme::bordered(
            square,
            if checked {
                theme::c().accent
            } else {
                theme::c().surface
            },
            if checked {
                theme::c().accent
            } else {
                theme::c().border
            },
            2.0,
            1.0,
        ))
        .child(text(
            UiRect::new(rect.left + 20.0, rect.top, rect.right, rect.bottom),
            label,
            theme::sans(theme::c().text_soft, TEXT_SIZE),
        ));
    if checked {
        row = row.child(drawer_icon(
            UiId::owned(format!("search-check-{label}")),
            "check",
            square,
            theme::c().on_accent,
        ));
    }
    row
}

fn search_input(
    rect: UiRect,
    state: StateAccess<SearchPanelState>,
    commands: Commands,
    id: UiId,
    focus: UiFocusHandle,
    index: usize,
) -> Element {
    type Reader = fn(&SearchPanelState) -> &InputState;
    type Writer = fn(&mut SearchPanelState) -> &mut InputState;
    let (read, write): (Reader, Writer) = match index {
        1 => (|app| &app.ui.include, |app| &mut app.ui.include),
        2 => (|app| &app.ui.exclude, |app| &mut app.ui.exclude),
        _ => (|app| &app.ui.query, |app| &mut app.ui.query),
    };
    let submit = commands.clone();
    input::render_access(
        rect,
        id,
        focus,
        InputBinding::new_access(state, read, write),
        input::AccessInputOptions {
            label: match index {
                1 => "Files to include",
                2 => "Files to exclude",
                _ => "Search workspace",
            },
            placeholder: match index {
                1 => "src/**, *.rs",
                2 => "target/**, *.lock",
                _ => "Search",
            },
            style: InputStyle {
                text: theme::mono(theme::c().text, TEXT_SIZE),
                placeholder: theme::mono(theme::c().text_muted, TEXT_SIZE),
                caret: theme::c().accent,
                selection: theme::c().accent,
            },
            on_submit: Some(Arc::new(move || submit.dispatch(Command::Refresh))),
            on_cancel: Some(Arc::new(move || commands.dispatch(Command::Cancel))),
            on_blur: None,
        },
    )
}

fn detail_lines(results: &Results) -> Vec<(&'static str, usize)> {
    [
        ("Binary files", results.skipped_binary),
        ("Files larger than 4 MiB", results.skipped_large),
        ("Unsupported encoding", results.skipped_encoding),
        ("Unreadable files", results.errors),
    ]
    .into_iter()
    .filter(|(_, n)| *n > 0)
    .collect()
}

fn footer(
    rect: UiRect,
    results: &Results,
    open: bool,
    state: StateAccess<SearchPanelState>,
) -> Element {
    let mut root = panel(rect, VisualStyle::filled(theme::c().sidebar)).child(panel(
        UiRect::new(rect.left, rect.top, rect.right, rect.top + 1.0),
        VisualStyle::filled(theme::c().border),
    ));
    let label = if results.errors > 0 && results.skipped > 0 {
        format!(
            "{} skipped · {} unreadable",
            number(results.skipped),
            number(results.errors)
        )
    } else if results.errors > 0 {
        format!("{} unreadable files", number(results.errors))
    } else {
        format!("{} files skipped", number(results.skipped))
    };
    let toggle = state.clone();
    let hit = UiRect::new(
        rect.left + 12.0,
        rect.top + 2.0,
        rect.right - 12.0,
        rect.top + 24.0,
    );
    let button = control_panel(hit, false)
        .key("search-skip-details")
        .semantics(Semantics::new(SemanticRole::Button).name("Search file details"))
        .on_click(move || toggle.update(|app| app.ui.details_open = !app.ui.details_open))
        .child(drawer_icon(
            "search-details-chevron",
            if open {
                "chevron-down"
            } else {
                "chevron-right"
            },
            UiRect::new(hit.left, hit.top + 5.0, hit.left + 12.0, hit.top + 17.0),
            theme::c().text_muted,
        ))
        .child(fit_text(
            &label,
            UiRect::new(hit.left + 18.0, hit.top, hit.right, hit.bottom),
            theme::sans(
                if results.errors > 0 {
                    theme::c().warning
                } else {
                    theme::c().text_muted
                },
                META_SIZE,
            ),
        ));
    root = root.child(button);
    if open {
        for (index, (label, value)) in detail_lines(results).into_iter().enumerate() {
            let top = rect.top + 26.0 + index as f32 * 18.0;
            root = root
                .child(fit_text(
                    label,
                    UiRect::new(rect.left + 30.0, top, rect.right - 48.0, top + 18.0),
                    theme::sans(theme::c().text_soft, META_SIZE),
                ))
                .child(text(
                    UiRect::new(rect.right - 44.0, top, rect.right - 12.0, top + 18.0),
                    number(value),
                    {
                        let mut style = theme::sans(theme::c().text_soft, META_SIZE);
                        style.align = lgui::core::TextAlign::Right;
                        style
                    },
                ));
        }
    }
    root
}

/// Index file groups in O(files); allocate nodes only for rows in the viewport.
fn results(
    rect: UiRect,
    state: StateAccess<SearchPanelState>,
    commands: Commands,
    has_roots: bool,
    multiple_roots: bool,
    inputs: Inputs,
    results: Arc<Results>,
) -> Element {
    let app = state.get();
    let mut content_h = 0.0;
    let groups = results
        .files
        .iter()
        .map(|file| {
            let top = content_h;
            let expanded = !app.ui.collapsed.contains(&file.path);
            content_h += FILE_ROW_H
                + if expanded {
                    file.matches.len() as f32 * MATCH_ROW_H
                } else {
                    0.0
                };
            (top, expanded, file.clone())
        })
        .collect::<Vec<_>>();
    let max_scroll = (content_h - rect.height()).max(0.0);
    let scroll = app.ui.scroll_y.clamp(0.0, max_scroll);
    let wheel_state = state.clone();
    let focus_state = state.clone();
    let blur_state = state.clone();
    let viewport_state = state.clone();
    let mut root = Element::new(move |cx| {
        UiElement::panel(inputs.results_id, rect, VisualStyle::default()).children(cx.children)
    })
    .event_policy(EventPolicy::INTERACTIVE)
    .semantics(Semantics::new(SemanticRole::Group).name("Search results"))
    .on_focus(move |_| {
        focus_state.update(|app| {
            app.ui.results_focused = true;
            app.ui.viewport_height = rect.height();
            search_actions::reveal_selection(app);
        })
    })
    .on_blur(move |_| blur_state.update(|app| app.ui.results_focused = false))
    .on_event_capture(lgui::core::UiEventKind::PointerDown, move |_, _| {
        viewport_state.try_update(|app| {
            let changed = app.ui.viewport_height != rect.height();
            app.ui.viewport_height = rect.height();
            changed
        });
    })
    .on_wheel(move |cx, delta| {
        let step = match delta.unit {
            WheelUnit::Lines => delta.y * MATCH_ROW_H * 3.0,
            WheelUnit::Pixels => delta.y,
        };
        wheel_state.update(|app| {
            app.ui.scroll_y = (scroll - step).clamp(0.0, max_scroll);
            app.ui.viewport_height = rect.height();
        });
        cx.stop_propagation();
    });
    let mut clipped = clip(rect, 0.0, 0.0);
    for (offset, expanded, file) in groups {
        let end = offset
            + FILE_ROW_H
            + if expanded {
                file.matches.len() as f32 * MATCH_ROW_H
            } else {
                0.0
            };
        if offset >= scroll + rect.height() || end <= scroll {
            continue;
        }
        if offset + FILE_ROW_H > scroll {
            let area = UiRect::new(
                rect.left,
                rect.top + offset - scroll,
                rect.right,
                rect.top + offset - scroll + FILE_ROW_H,
            );
            let toggle = state.clone();
            let group_focus = inputs.results_focus.clone();
            let path = file.path.clone();
            let (relative, context) = file_labels(&file, multiple_roots);
            let name = file.name();
            let count = number(file.matches.len());
            let badge_w =
                (SingleLineText::new(&count, area, theme::mono(theme::c().text_soft, META_SIZE))
                    .width()
                    + 10.0)
                    .max(16.0);
            let badge = UiRect::new(
                area.right - 12.0 - badge_w,
                area.top + 4.0,
                area.right - 12.0,
                area.top + 20.0,
            );
            let name_left = area.left + 45.0;
            let available = (badge.left - 8.0 - name_left).max(0.0);
            let name_w = SingleLineText::new(
                name.as_ref(),
                area,
                theme::mono(theme::c().text_soft, TEXT_SIZE),
            )
            .width()
            .min(available);
            let name_rect = UiRect::new(name_left, area.top, name_left + name_w, area.bottom);
            let mut heading = control_panel(area, false)
                .key(format!("search-file-{}", file.path.context_key()))
                .semantics({
                    let mut semantics = Semantics::new(SemanticRole::Button).name(relative);
                    semantics.state.expanded = Some(expanded);
                    semantics
                })
                .on_click(move || {
                    group_focus.focus();
                    toggle.update(|app| {
                        if !app.ui.collapsed.remove(&path) {
                            app.ui.collapsed.insert(path.clone());
                        }
                    });
                })
                .child(drawer_icon(
                    UiId::owned(format!("search-chevron-{}", file.path.context_key())),
                    if expanded {
                        "chevron-down"
                    } else {
                        "chevron-right"
                    },
                    UiRect::new(
                        area.left + 9.0,
                        area.top + 6.0,
                        area.left + 21.0,
                        area.top + 18.0,
                    ),
                    theme::c().text_muted,
                ))
                .child(drawer_icon(
                    UiId::owned(format!("search-file-icon-{}", file.path.context_key())),
                    crate::ui::file_icons::icon_for_file(&name),
                    UiRect::new(
                        area.left + 25.0,
                        area.top + 5.0,
                        area.left + 39.0,
                        area.top + 19.0,
                    ),
                    theme::c().text_soft,
                ))
                .child(fit_text(
                    &name,
                    name_rect,
                    theme::mono(theme::c().text_soft, TEXT_SIZE),
                ))
                .child(panel(
                    badge,
                    VisualStyle::filled(theme::c().active_line).radius(8.0),
                ))
                .child(text(
                    badge,
                    count,
                    theme::mono(theme::c().text_soft, META_SIZE).centered(),
                ));
            if rect.width() > 260.0 && available - name_w > 20.0 {
                heading = heading.child(fit_text(
                    &context,
                    UiRect::new(
                        name_rect.right + 8.0,
                        area.top,
                        badge.left - 8.0,
                        area.bottom,
                    ),
                    theme::mono(theme::c().text_muted, META_SIZE),
                ));
            }
            clipped = clipped.child(heading);
        }
        if expanded {
            let first = ((scroll - offset - FILE_ROW_H) / MATCH_ROW_H)
                .floor()
                .max(0.0) as usize;
            let last = ((scroll + rect.height() - offset - FILE_ROW_H) / MATCH_ROW_H)
                .ceil()
                .max(0.0) as usize;
            for index in first..file.matches.len().min(last) {
                let top = rect.top + offset + FILE_ROW_H + index as f32 * MATCH_ROW_H - scroll;
                clipped = clipped.child(match_row(
                    UiRect::new(rect.left, top, rect.right - 12.0, top + MATCH_ROW_H),
                    commands.clone(),
                    inputs.results_focus.clone(),
                    file.clone(),
                    index,
                    app.ui.selected.as_ref() == Some(&(file.path.clone(), index)),
                ));
            }
        }
    }
    if results.matches == 0 && app.model.status().error.is_none() {
        let message = if !has_roots {
            "Open a folder to search."
        } else if app.ui.query.text().is_empty() {
            "Search across your workspace"
        } else if app.model.status().running {
            "Searching workspace…"
        } else if app.model.status().cancelled {
            "Search stopped."
        } else if results.complete {
            "No results found."
        } else {
            ""
        };
        clipped = clipped.child(fit_text(
            message,
            UiRect::new(
                rect.left + 16.0,
                rect.top + 7.0,
                (rect.right - 16.0).max(rect.left + 16.0),
                rect.top + 31.0,
            ),
            theme::sans(theme::c().text_soft, TEXT_SIZE),
        ));
    }
    root = root.child(clipped);
    if max_scroll > 0.0 && rect.height() > 0.0 {
        let thumb_h = (rect.height() * rect.height() / content_h)
            .max(scrollbar::MIN_THUMB_LENGTH)
            .min(rect.height());
        let travel = (rect.height() - thumb_h).max(0.0);
        let top = rect.top + travel * scroll / max_scroll;
        let down = state.clone();
        let drag = state.clone();
        let up = state;
        root = root.child(
            scrollbar::thumb(
                UiRect::new(rect.right - 9.0, top, rect.right - 2.0, top + thumb_h),
                100,
            )
            .on_pointer_down(move |cx, p| {
                down.update(|app| app.ui.scrollbar_drag = Some((p.point.y, scroll)));
                cx.stop_propagation();
            })
            .on_pointer_drag(move |cx, p| {
                drag.update(|app| {
                    if let Some((origin, start)) = app.ui.scrollbar_drag {
                        app.ui.scroll_y = (start
                            + (p.point.y - origin) * max_scroll / travel.max(1.0))
                        .clamp(0.0, max_scroll);
                    }
                });
                cx.stop_propagation();
            })
            .on_pointer_up(move |cx, _| {
                up.update(|app| app.ui.scrollbar_drag = None);
                cx.stop_propagation();
            }),
        );
    }
    root
}

/// Crop at grapheme boundaries and keep the first matched character visible.
fn match_preview(
    found: &crate::features::search::Match,
    bounds: UiRect,
) -> (SingleLineText, std::ops::Range<usize>) {
    let raw = found.preview.replace('\t', " ");
    let leading = raw.len() - raw.trim_start().len();
    // Remove indentation unless it contains the match itself.
    let leading = if leading <= found.highlight.start {
        leading
    } else {
        0
    };
    let value = &raw[leading..];
    let source_highlight = found.highlight.start - leading..found.highlight.end - leading;
    let style = theme::mono(theme::c().text_soft, TEXT_SIZE);
    let wide = UiRect::new(
        bounds.left,
        bounds.top,
        bounds.left + 100_000.0,
        bounds.bottom,
    );
    let measured = SingleLineText::new(value, wide, style);
    if measured.width() <= bounds.width() {
        return (measured, source_highlight);
    }
    let offsets = value
        .grapheme_indices(true)
        .map(|(offset, _)| offset)
        .chain(std::iter::once(value.len()))
        .collect::<Vec<_>>();
    let target = (measured.caret_offset(source_highlight.start) - bounds.width() * 0.25).max(0.0);
    let start = offsets
        .iter()
        .copied()
        .take_while(|offset| {
            *offset <= source_highlight.start && measured.caret_offset(*offset) <= target
        })
        .last()
        .unwrap_or(0);
    let prefix = if start > 0 { "…" } else { "" };
    let ellipsis_w = SingleLineText::new("…", bounds, style).width();
    let available = (bounds.width() - ellipsis_w * if start > 0 { 2.0 } else { 1.0 }).max(0.0);
    let origin = measured.caret_offset(start);
    let end = offsets
        .into_iter()
        .filter(|offset| *offset >= start)
        .take_while(|offset| measured.caret_offset(*offset) - origin <= available)
        .last()
        .unwrap_or(start);
    let suffix = if end < value.len() { "…" } else { "" };
    let visible = format!("{prefix}{}{suffix}", &value[start..end]);
    let highlight = prefix.len()
        + source_highlight
            .start
            .saturating_sub(start)
            .min(end - start)
        ..prefix.len() + source_highlight.end.saturating_sub(start).min(end - start);
    (SingleLineText::new(visible, wide, style), highlight)
}

fn match_row(
    rect: UiRect,
    commands: Commands,
    results_focus: UiFocusHandle,
    file: Arc<FileMatches>,
    index: usize,
    selected: bool,
) -> Element {
    let found = &file.matches[index];
    let line_rect = UiRect::new(rect.left + 27.0, rect.top, rect.left + 52.0, rect.bottom);
    let bounds = UiRect::new(line_rect.right + 10.0, rect.top, rect.right, rect.bottom);
    let (measured, highlight) = match_preview(found, bounds);
    let highlight_left = measured.caret_x(highlight.start);
    let highlight_right = measured.caret_x(highlight.end).max(highlight_left + 2.0);
    let click_file = file.clone();
    let mut row = control_panel(rect, false)
        .key(format!("search-match-{}-{index}", file.path.context_key()))
        .semantics({
            let mut semantics = Semantics::new(SemanticRole::Button).name(format!(
                "Line {}: {}",
                found.line + 1,
                found.preview
            ));
            semantics.state.selected = selected;
            semantics
        })
        .on_pointer_down_with_button(move |cx, pointer, button| {
            if button != PointerButton::Left {
                return;
            }
            results_focus.focus();
            commands.dispatch(Command::Select {
                file: click_file.clone(),
                index,
                point: (pointer.point.x, pointer.point.y),
            });
            cx.stop_propagation();
        });
    if selected {
        row = row
            .child(panel(rect, VisualStyle::filled(theme::c().active_line)))
            .child(panel(
                UiRect::new(rect.left, rect.top, rect.left + 2.0, rect.bottom),
                VisualStyle::filled(theme::c().accent),
            ));
    }
    row = row.child(text(
        line_rect,
        (found.line + 1).to_string(),
        theme::mono_right(theme::c().text_muted, META_SIZE),
    ));
    row.child(
        clip(bounds, 0.0, 0.0)
            .child(panel(
                UiRect::new(
                    highlight_left,
                    rect.top + 4.0,
                    highlight_right,
                    rect.bottom - 4.0,
                ),
                VisualStyle::filled(theme::c().accent).alpha(30).radius(2.0),
            ))
            .child(measured.element()),
    )
}

#[cfg(test)]
#[path = "../../app/search_view_tests.rs"]
mod tests;
