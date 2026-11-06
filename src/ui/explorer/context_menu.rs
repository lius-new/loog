//! Context menu overlay for the file drawer and its rows.
//!
//! Rows carry an explicit target; an empty-space invocation falls back to the
//! workspace root of the active file. Items are grouped by separators. A
//! chosen item closes the menu and leaves the action to the application.

use std::path::{Path, PathBuf};

use lgui::core::{CursorIcon, EventPolicy, UiEventKind};
use lgui::prelude::{Color, Element, ShadowStyle, UiRect, VisualStyle, panel, text};
use lgui::text::measure_width;
use unicode_width::UnicodeWidthStr;

use super::commands::{Command, Commands, MenuAction};
use super::state::ExplorerPanelState;
use crate::features::explorer::model::ExplorerModel;
use crate::features::explorer::{ExplorerContextTarget, ExplorerTargetKind};
use crate::ui::components::state_access::StateAccess;
use crate::ui::theme;

const MENU_PAD: f32 = 4.0; // padding above/below the item list
const ITEM_H: f32 = 22.0; // item row height
const ITEM_PAD_X: f32 = 10.0; // horizontal padding inside an item
const SEP_H: f32 = 5.0; // separator row height
const SHORTCUT_GAP: f32 = 18.0; // min gap between a label and its shortcut
const TEXT_MARGIN: f32 = 6.0; // right margin so the last glyph is not clipped
const MENU_BORDER: f32 = 1.0; // menu border width

fn action_enabled(
    action: MenuAction,
    has_target: bool,
    has_item_target: bool,
    can_remove: bool,
) -> bool {
    match action {
        MenuAction::AddFolder => true,
        MenuAction::RemoveFolder => can_remove,
        MenuAction::Cut | MenuAction::Copy | MenuAction::Rename | MenuAction::Delete => {
            has_item_target
        }
        _ => has_target,
    }
}

/// One row of the menu; `Separator` renders a hairline divider.
enum Entry {
    Item {
        label: &'static str,
        shortcut: Option<&'static str>,
        action: MenuAction,
    },
    Separator,
}

const fn item(label: &'static str, action: MenuAction) -> Entry {
    Entry::Item {
        label,
        shortcut: None,
        action,
    }
}

const DIRECTORY_ENTRIES: &[Entry] = &[
    item("New File", MenuAction::NewFile),
    item("New Folder", MenuAction::NewFolder),
    item("Refresh Explorer", MenuAction::RefreshExplorer),
    Entry::Separator,
    item("Reveal in File Explorer", MenuAction::RevealInFileExplorer),
    item("Open in Terminal", MenuAction::OpenTerminal),
    Entry::Separator,
    item("Cut", MenuAction::Cut),
    item("Copy", MenuAction::Copy),
    Entry::Separator,
    item("Add Folder From Workspace", MenuAction::AddFolder),
    item("Remove Folder From Workspace", MenuAction::RemoveFolder),
    Entry::Separator,
    item("Copy Path", MenuAction::CopyPath),
    item("Copy Relative Path", MenuAction::CopyRelativePath),
    Entry::Separator,
    item("Rename", MenuAction::Rename),
    item("Delete", MenuAction::Delete),
];

const FILE_ENTRIES: &[Entry] = &[
    item("Reveal in File Explorer", MenuAction::RevealInFileExplorer),
    item("Open in Terminal", MenuAction::OpenTerminal),
    Entry::Separator,
    item("Cut", MenuAction::Cut),
    item("Copy", MenuAction::Copy),
    Entry::Separator,
    item("Copy Path", MenuAction::CopyPath),
    item("Copy Relative Path", MenuAction::CopyRelativePath),
    Entry::Separator,
    item("Rename", MenuAction::Rename),
    item("Delete", MenuAction::Delete),
];

/// Shown on a multi-selection: only actions that apply to every item.
const BATCH_ENTRIES: &[Entry] = &[
    item("Cut", MenuAction::Cut),
    item("Copy", MenuAction::Copy),
    Entry::Separator,
    item("Copy Path", MenuAction::CopyPath),
    item("Copy Relative Path", MenuAction::CopyRelativePath),
    Entry::Separator,
    item("Delete", MenuAction::Delete),
];

/// Appended to a Markdown file's menu.
const MARKDOWN_ENTRIES: &[Entry] = &[
    Entry::Separator,
    item("Preview Markdown", MenuAction::PreviewMarkdown),
];
const SVG_ENTRIES: &[Entry] = &[
    Entry::Separator,
    item("Toggle SVG Preview", MenuAction::ToggleSvg),
];

/// The entries for `target`: directory actions, or file actions plus the
/// Markdown preview for Markdown files.
fn entries_for(target: Option<&ExplorerContextTarget>) -> Vec<&'static Entry> {
    match target {
        Some(target) if target.kind == ExplorerTargetKind::File => {
            let markdown = crate::features::markdown::is_markdown_path(&target.path);
            let svg = crate::domain::media::is_svg(&target.path);
            FILE_ENTRIES
                .iter()
                .chain(markdown.then_some(MARKDOWN_ENTRIES).into_iter().flatten())
                .chain(svg.then_some(SVG_ENTRIES).into_iter().flatten())
                .collect()
        }
        _ => DIRECTORY_ENTRIES.iter().collect(),
    }
}

/// The empty-space menu acts on the root containing the active file. Falling
/// back to the first root keeps its behavior deterministic before a file is
/// opened. The longest match wins when workspace roots are nested.
fn context_root(model: &ExplorerModel, active_path: Option<&Path>) -> Option<PathBuf> {
    active_path
        .and_then(|active| model.root_for_path(active))
        .or_else(|| model.roots().first())
        .cloned()
}

fn context_target(
    panel: &ExplorerPanelState,
    active_path: Option<&Path>,
) -> Option<ExplorerContextTarget> {
    panel.ui.menu_target.clone().or_else(|| {
        context_root(&panel.model, active_path).map(|path| ExplorerContextTarget {
            path,
            kind: ExplorerTargetKind::Directory,
        })
    })
}

/// Natural width of `s` at the menu text size. The renderer's generic text
/// measurement does not select the menu's monospace family, so keep the
/// monospace column width as a floor to avoid clipping long labels.
fn measure(s: &str) -> f32 {
    let bounds = UiRect::new(0.0, 0.0, 10_000.0, theme::UI_SIZE);
    let mono_width = UnicodeWidthStr::width(s) as f32 * theme::UI_CHAR_W;
    measure_width(s, bounds, theme::UI_SIZE, 400).map_or(mono_width, |width| width.max(mono_width))
}

/// Renders the right-click menu anchored at `pos` (screen coords), flipped and
/// clamped so it stays inside `rect` (the viewport).
pub fn render(
    rect: UiRect,
    pos: (f32, f32),
    state: StateAccess<ExplorerPanelState>,
    active_path: Option<&Path>,
    commands: Commands,
) -> Element {
    let s = state.get();
    let target = context_target(&s, active_path);
    let has_item_target = s.ui.menu_target.is_some();
    let batch = s.ui.selection.len() > 1
        && target.as_ref().is_some_and(|target| {
            target.kind == ExplorerTargetKind::File && s.ui.selection.contains(&target.path)
        });
    let entries: Vec<&'static Entry> = if batch {
        BATCH_ENTRIES.iter().collect()
    } else {
        entries_for(target.as_ref())
    };
    let can_remove = target.as_ref().is_some_and(|target| {
        target.kind == ExplorerTargetKind::Directory && s.model.is_root(&target.path)
    });

    // Size the menu to its widest entry (label + optional shortcut).
    let content_w = entries
        .iter()
        .map(|e| match e {
            Entry::Item {
                label, shortcut, ..
            } => {
                let mut w = measure(label);
                if let Some(sc) = shortcut {
                    w += SHORTCUT_GAP + measure(sc);
                }
                w
            }
            Entry::Separator => 0.0,
        })
        .fold(0.0, f32::max);
    let menu_w = content_w + ITEM_PAD_X * 2.0 + TEXT_MARGIN;

    let menu_h = MENU_PAD * 2.0
        + entries
            .iter()
            .map(|e| match e {
                Entry::Item { .. } => ITEM_H,
                Entry::Separator => SEP_H,
            })
            .sum::<f32>();

    // Anchor at the cursor, flipping when it would overflow the window.
    let x = if pos.0 + menu_w <= rect.right {
        pos.0
    } else {
        pos.0 - menu_w
    };
    let y = if pos.1 + menu_h <= rect.bottom {
        pos.1
    } else {
        pos.1 - menu_h
    };
    let x = x.clamp(rect.left, (rect.right - menu_w).max(rect.left));
    let y = y.clamp(rect.top, (rect.bottom - menu_h).max(rect.top));
    let card = UiRect::new(x, y, x + menu_w, y + menu_h);

    // Invisible backdrop: a press outside the menu closes it. Presses inside
    // the menu are left to the items (so a click can both act and close).
    let st_close = state.clone();
    let overlay = panel(rect, VisualStyle::default())
        .event_policy(EventPolicy::INTERACTIVE)
        .on_pointer_down_with_button(move |_cx, p, _button| {
            if !card.contains(p.point) {
                st_close.update(|panel| panel.ui.dismiss_menu());
            }
        });

    // Menu surface. A capture-phase move clears the hover highlight whenever
    // the pointer is over the surface but not on an item (padding, separators).
    let st_clear = state.clone();
    let mut surface = theme::bordered(
        card,
        theme::c().surface,
        theme::c().border,
        6.0,
        MENU_BORDER,
    )
    .shadow(
        ShadowStyle::new(Color::BLACK)
            .alpha(theme::c().shadow_alpha)
            .offset(0.0, 2.0)
            .blur(3.0),
    )
    .event_policy(EventPolicy::INTERACTIVE)
    .on_event_capture(UiEventKind::PointerMove, move |_cx, _p| {
        st_clear.try_update(|panel| {
            let changed = panel.ui.menu_hover.is_some();
            if changed {
                panel.ui.menu_hover = None;
            }
            changed
        });
    });

    // Rows.
    let mut y = card.top + MENU_PAD;
    let mut index = 0usize;
    for entry in entries {
        match entry {
            Entry::Separator => {
                let line_y = y + (SEP_H - 1.0) / 2.0;
                surface = surface.child(panel(
                    UiRect::new(
                        card.left + ITEM_PAD_X,
                        line_y,
                        card.right - ITEM_PAD_X,
                        line_y + 1.0,
                    ),
                    VisualStyle::filled(theme::c().border),
                ));
                y += SEP_H;
            }
            Entry::Item {
                label,
                shortcut,
                action,
            } => {
                let enabled =
                    action_enabled(*action, target.is_some(), has_item_target, can_remove);
                let item_rect = UiRect::new(
                    card.left + MENU_BORDER,
                    y,
                    card.right - MENU_BORDER,
                    y + ITEM_H,
                );
                let hovered = enabled && s.ui.menu_hover == Some(index);
                let idx = index;
                let label = *label;
                let action = *action;

                let st_hover = state.clone();
                let st_click = state.clone();
                let click_commands = commands.clone();
                let action_target = target.clone();
                let mut item = panel(
                    item_rect,
                    if hovered {
                        VisualStyle::filled(theme::c().selection)
                    } else {
                        VisualStyle::default()
                    },
                )
                .event_policy(EventPolicy::INTERACTIVE)
                .cursor(if enabled {
                    CursorIcon::Pointer
                } else {
                    CursorIcon::Default
                })
                .on_event(UiEventKind::PointerMove, move |_cx, _p| {
                    st_hover.try_update(move |panel| {
                        let changed = if enabled {
                            panel.ui.menu_hover != Some(idx)
                        } else {
                            panel.ui.menu_hover.is_some()
                        };
                        panel.ui.menu_hover = enabled.then_some(idx);
                        changed
                    });
                })
                .on_click(move || {
                    if !enabled {
                        return;
                    }
                    st_click.update(|panel| panel.ui.dismiss_menu());
                    click_commands.dispatch(Command::Menu {
                        action,
                        target: action_target.clone(),
                    });
                });

                // Label (left-aligned).
                let label_rect = UiRect::new(
                    item_rect.left + ITEM_PAD_X,
                    item_rect.top,
                    item_rect.right - ITEM_PAD_X,
                    item_rect.bottom,
                );
                let label_color = if hovered {
                    theme::c().text_bright
                } else if !enabled {
                    theme::c().text_dim
                } else {
                    theme::c().text_soft
                };
                item = item.child(text(
                    label_rect,
                    label,
                    theme::mono(label_color, theme::UI_SIZE),
                ));

                // Shortcut (right-aligned, dimmed).
                if let Some(sc) = *shortcut {
                    let sc_w = measure(sc);
                    let sc_rect = UiRect::new(
                        item_rect.right - ITEM_PAD_X - sc_w - TEXT_MARGIN,
                        item_rect.top,
                        item_rect.right - ITEM_PAD_X,
                        item_rect.bottom,
                    );
                    item = item.child(text(
                        sc_rect,
                        sc,
                        theme::mono_right(theme::c().text_dim, theme::UI_SIZE),
                    ));
                }

                surface = surface.child(item);
                y += ITEM_H;
                index += 1;
            }
        }
    }

    overlay.child(surface)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel_with_roots(roots: &[&str]) -> ExplorerPanelState {
        let mut panel = ExplorerPanelState::default();
        panel
            .model
            .restore(roots.iter().map(PathBuf::from).collect(), Vec::new());
        panel
    }

    #[test]
    fn context_root_falls_back_to_the_first_workspace_folder() {
        let panel = panel_with_roots(&["first", "second"]);

        assert_eq!(
            context_root(&panel.model, None),
            Some(PathBuf::from("first"))
        );
    }

    #[test]
    fn context_root_prefers_the_deepest_root_containing_the_active_file() {
        let panel = panel_with_roots(&["workspace", "workspace/nested"]);

        assert_eq!(
            context_root(
                &panel.model,
                Some(Path::new("workspace/nested/src/main.rs"))
            ),
            Some(PathBuf::from("workspace/nested"))
        );
    }

    #[test]
    fn only_add_folder_is_enabled_without_a_workspace() {
        assert!(action_enabled(MenuAction::AddFolder, false, false, false));
        for action in [
            MenuAction::RevealInFileExplorer,
            MenuAction::CopyPath,
            MenuAction::CopyRelativePath,
            MenuAction::NewFile,
            MenuAction::NewFolder,
            MenuAction::OpenTerminal,
        ] {
            assert!(!action_enabled(action, false, false, false), "{action:?}");
        }
        assert!(!action_enabled(
            MenuAction::RemoveFolder,
            true,
            false,
            false
        ));
        assert!(action_enabled(MenuAction::RemoveFolder, true, true, true));
        assert!(!action_enabled(MenuAction::Rename, true, false, false));
        assert!(action_enabled(MenuAction::Rename, true, true, false));
    }

    #[test]
    fn an_explicit_directory_target_takes_priority_over_the_background_root() {
        let mut panel = panel_with_roots(&["workspace"]);
        panel.ui.menu_target = Some(ExplorerContextTarget {
            path: PathBuf::from("workspace/src"),
            kind: ExplorerTargetKind::Directory,
        });

        assert_eq!(
            context_target(&panel, None),
            Some(ExplorerContextTarget {
                path: PathBuf::from("workspace/src"),
                kind: ExplorerTargetKind::Directory,
            })
        );
        assert_eq!(
            panel.model.root_for_path(Path::new("workspace/src")),
            Some(&PathBuf::from("workspace"))
        );
    }

    #[test]
    fn file_menu_contains_only_file_actions() {
        let labels = FILE_ENTRIES
            .iter()
            .filter_map(|entry| match entry {
                Entry::Item { label, .. } => Some(*label),
                Entry::Separator => None,
            })
            .collect::<Vec<_>>();

        assert!(labels.contains(&"Reveal in File Explorer"));
        assert!(labels.contains(&"Cut"));
        assert!(labels.contains(&"Copy"));
        assert!(labels.contains(&"Rename"));
        assert!(labels.contains(&"Delete"));
        assert!(!labels.contains(&"New File"));
        assert!(!labels.contains(&"Add Folder From Workspace"));
        assert!(!labels.contains(&"Remove Folder From Workspace"));
    }

    #[test]
    fn only_markdown_files_get_the_preview_entry_and_it_comes_last() {
        let file = |name: &str| ExplorerContextTarget {
            path: PathBuf::from(name),
            kind: ExplorerTargetKind::File,
        };
        let last = |entries: Vec<&Entry>| {
            entries.into_iter().rev().find_map(|entry| match entry {
                Entry::Item { label, .. } => Some(*label),
                Entry::Separator => None,
            })
        };
        assert_eq!(
            last(entries_for(Some(&file("a/readme.MD")))),
            Some("Preview Markdown")
        );
        assert_eq!(last(entries_for(Some(&file("a/main.rs")))), Some("Delete"));
        let folder = ExplorerContextTarget {
            path: PathBuf::from("a/b.md"),
            kind: ExplorerTargetKind::Directory,
        };
        assert_eq!(last(entries_for(Some(&folder))), Some("Delete"));
    }

    #[test]
    fn long_menu_labels_reserve_at_least_their_monospace_width() {
        let label = "Remove Folder From Workspace";
        let mono_width = UnicodeWidthStr::width(label) as f32 * theme::UI_CHAR_W;

        assert!(measure(label) >= mono_width);
    }
    #[test]
    fn svg_toggle_is_only_in_svg_file_menus() {
        for (name, kind, expected) in [
            ("logo.svg", ExplorerTargetKind::File, true),
            ("logo.SVG", ExplorerTargetKind::File, true),
            ("photo.png", ExplorerTargetKind::File, false),
            ("readme.md", ExplorerTargetKind::File, false),
            ("folder.svg", ExplorerTargetKind::Directory, false),
        ] {
            let target = ExplorerContextTarget {
                path: name.into(),
                kind,
            };
            assert_eq!(
                entries_for(Some(&target)).iter().any(|entry| matches!(
                    entry,
                    Entry::Item {
                        action: MenuAction::ToggleSvg,
                        ..
                    }
                )),
                expected
            );
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn copied_windows_paths_hide_the_verbatim_prefix() {
        assert_eq!(
            crate::ui::display_path(Path::new(r"\\?\D:\Documents\project")),
            r"D:\Documents\project"
        );
        assert_eq!(
            crate::ui::display_path(Path::new(r"\\?\UNC\server\share\project")),
            r"\\server\share\project"
        );
        assert_eq!(
            crate::ui::display_path(Path::new(r"D:\Documents\project")),
            r"D:\Documents\project"
        );
    }
}
