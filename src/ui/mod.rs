//! UI layer: chrome components, each rendering a self-contained region.
//!
//! Every module exposes a `render(rect, …)` function returning an `Element`.
//! Migrated components accept data and callbacks; legacy State<AppState>
//! adapters remain registered. app/shell.rs owns layout and composition.

pub mod close_confirmation;
pub mod components;
pub mod diff_editor;
pub mod diff_selection;
#[cfg(test)]
mod diff_selection_tests;
pub mod drawer;
pub(crate) mod editor;
pub(crate) mod editor_binding;
pub(crate) mod encoding_picker;
pub mod explorer;
pub(crate) mod extensions_binding;
pub mod extensions_page;
pub(crate) mod file_error;
pub mod git;
pub(crate) mod image_preview;
pub(crate) mod image_rendering;
pub mod keymap_page;
pub mod language_details;
pub mod language_panel;
pub(crate) mod markdown;
pub mod outline;
pub mod palette;
pub mod pane_sash;
pub mod search;
pub mod settings;
pub mod statusbar;
pub mod tab_context_menu;
pub mod tab_layout;
pub mod tabs;
pub mod terminal;
pub(crate) mod text_rendering;
pub mod titlebar;
pub mod toast;
pub mod trust_prompt;
pub mod welcome;
pub mod workspace_home;

/// Give `target` keyboard focus while an overlay is open, then hand focus
/// back to the terminal or editor it was taken from. `restore_terminal` is
/// `Some` while the overlay is open, holding whether the terminal had focus.
pub(crate) fn hold_focus(
    cx: &mut lgui::prelude::RenderCx<'_, '_>,
    restore_terminal: Option<bool>,
    target: lgui::core::UiFocusHandle,
    terminal_focus: lgui::core::UiFocusHandle,
    editor_focus: lgui::core::UiFocusHandle,
) {
    use std::sync::{Arc, Mutex};

    let opened = cx.state_with(|| Arc::new(Mutex::new(None::<bool>))).get();
    let open = restore_terminal.is_some();
    cx.use_effect(open, move || {
        let was = std::mem::replace(
            &mut *opened.lock().unwrap_or_else(|error| error.into_inner()),
            restore_terminal,
        );
        if open {
            target.focus();
        } else if let Some(restore_terminal) = was {
            if restore_terminal {
                terminal_focus.focus();
            } else {
                editor_focus.focus();
            }
        }
    });
}

pub(crate) fn track_main_surface_focus(
    surface: lgui::prelude::Element,
    state: crate::ui::chrome::HomeBinding,
) -> lgui::prelude::Element {
    let focused = state.clone();
    surface
        .on_focus(move |_| {
            focused.run(crate::ui::chrome::HomeCommand::Focus(true));
        })
        .on_blur(move |_| {
            state.run(crate::ui::chrome::HomeCommand::Focus(false));
        })
}

/// A path as shown to or copied by the user. `fs::canonicalize` yields
/// verbatim paths on Windows (`\\?\D:\dir`), so drop that prefix.
#[cfg(target_os = "windows")]
pub fn display_path(path: &std::path::Path) -> String {
    let value = path.to_string_lossy();
    if let Some(unc) = value.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else if let Some(local) = value.strip_prefix(r"\\?\") {
        local.to_owned()
    } else {
        value.into_owned()
    }
}

#[cfg(not(target_os = "windows"))]
pub fn display_path(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}
pub(crate) mod geometry;
pub(crate) mod language;

pub mod device_scale;
pub mod file_icons;
pub mod fonts;
pub mod theme;
pub(crate) mod theme_package;

pub(crate) mod settings_binding;

pub(crate) mod keymap_events;

pub(crate) mod keymap_binding;

pub(crate) mod chrome;

pub(crate) mod editing_menu;
