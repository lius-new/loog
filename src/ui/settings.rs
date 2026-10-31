//! Settings page shown in its own editor tab.
//!
//! Controls dispatch semantic commands; app-level effects persist the
//! values, so there is no separate apply/save step.

use lgui::core::{CursorIcon, EventPolicy, TextStyle, UiEventContext, WheelUnit, clip};
use lgui::prelude::{
    Element, TextAlign, UiRect, VisualStyle, component, group, panel, text,
};

use crate::features::update::About;
use crate::ui::components::input::{
    self, AccessInputOptions, InputBinding, InputState, InputStyle,
};
use crate::ui::fonts::{self, FontSettings};
use crate::ui::settings_binding::{Binding, Command, Frame, SettingsUiState};
use crate::ui::tabs::{TEXT_MARGIN, measure};
use crate::ui::theme;
use std::sync::Arc;

const MAX_PAGE_W: f32 = 720.0;
const PAGE_PAD_X: f32 = 32.0;
const PAGE_PAD_TOP: f32 = 36.0;
const HEADER_H: f32 = 64.0;
const SECTION_TITLE_H: f32 = 28.0;
const SECTION_GAP: f32 = 24.0;
const ROW_H: f32 = 58.0;
const CONTROL_GAP: f32 = 24.0;
const SWITCH_W: f32 = 30.0;
const SWITCH_H: f32 = 16.0;
const KNOB: f32 = 12.0;
const SEGMENT_W: f32 = 64.0;
const SEGMENT_H: f32 = 24.0;
const SCROLL_STEP: f32 = 24.0;
const FAMILY_INPUT_W: f32 = 220.0;
const FAMILY_INPUT_H: f32 = 26.0;
const PRESET_H: f32 = 24.0;
const PRESET_GAP: f32 = 8.0;
const FAMILY_ROW_H: f32 = ROW_H + PRESET_H + 12.0;
const STEP_BUTTON_W: f32 = 26.0;
const STEP_VALUE_W: f32 = 44.0;
const STEPPER_W: f32 = STEP_BUTTON_W * 2.0 + STEP_VALUE_W;
const STEPPER_H: f32 = 26.0;

/// Families offered as one-click choices under the font family field. They
/// are only suggestions: one that is not installed falls back to the
/// built-in monospace stack.
const FONT_PRESETS: [&str; 5] = [
    "Cascadia Code",
    "Consolas",
    "JetBrains Mono",
    "Fira Code",
    "Source Code Pro",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Setting {
    UiScale,
    EditorFontFamily,
    EditorFontSize,
    EditorFontWeight,
    EditorLineHeight,
    TerminalFontFamily,
    TerminalFontSize,
    TerminalFontWeight,
    TerminalLineHeight,
    DefaultShell,
    TerminalCursorBlink,
    SoftWrap,
    SmoothCaret,
    MatchBrackets,
    IndentGuides,
    ShowWhitespace,
    ExplorerAutoReveal,
    InlineBlame,
    SplitDiff,
    TreeView,
    VimMode,
    VimSystemClipboard,
    CheckForUpdates,
}

const SECTIONS: [(&str, &[Setting]); 6] = [
    ("APPEARANCE", &[Setting::UiScale]),
    (
        "EDITOR",
        &[
            Setting::EditorFontFamily,
            Setting::EditorFontSize,
            Setting::EditorFontWeight,
            Setting::EditorLineHeight,
            Setting::SoftWrap,
            Setting::SmoothCaret,
            Setting::MatchBrackets,
            Setting::IndentGuides,
            Setting::ShowWhitespace,
            Setting::VimMode,
            Setting::VimSystemClipboard,
        ],
    ),
    (
        "TERMINAL",
        &[
            Setting::TerminalFontFamily,
            Setting::TerminalFontSize,
            Setting::TerminalFontWeight,
            Setting::TerminalLineHeight,
            Setting::DefaultShell,
            Setting::TerminalCursorBlink,
        ],
    ),
    ("EXPLORER", &[Setting::ExplorerAutoReveal]),
    (
        "SOURCE CONTROL",
        &[Setting::InlineBlame, Setting::SplitDiff, Setting::TreeView],
    ),
    ("UPDATES", &[Setting::CheckForUpdates]),
];

impl Setting {
    fn label(self) -> &'static str {
        match self {
            Self::UiScale => "Interface scale",
            Self::EditorFontFamily | Self::TerminalFontFamily => "Font family",
            Self::EditorFontSize | Self::TerminalFontSize => "Font size",
            Self::EditorFontWeight | Self::TerminalFontWeight => "Font weight",
            Self::EditorLineHeight | Self::TerminalLineHeight => "Line height",
            Self::DefaultShell => "Default shell",
            Self::TerminalCursorBlink => "Cursor blinking",
            Self::MatchBrackets => "Matching brackets",
            Self::IndentGuides => "Indent guides",
            Self::ShowWhitespace => "Visible whitespace",
            Self::SoftWrap => "Soft wrap",
            Self::SmoothCaret => "Smooth caret movement",
            Self::ExplorerAutoReveal => "Auto reveal active file",
            Self::InlineBlame => "Inline blame",
            Self::SplitDiff => "Side-by-side diff",
            Self::TreeView => "Tree view",
            Self::VimMode => "Vim mode",
            Self::VimSystemClipboard => "Vim: use the system clipboard",
            Self::CheckForUpdates => "Check for updates",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::UiScale => "Scale the entire app, including tabs, panels and menus.",
            Self::EditorFontFamily => {
                "Font for code, diffs and change previews. Leave empty for the default."
            }
            Self::EditorFontSize => "Size of code text in pixels.",
            Self::EditorFontWeight => "Weight of code text: 400 is regular, 700 is bold.",
            Self::EditorLineHeight => "Height of each code line, as a multiple of the font size.",
            Self::TerminalFontFamily => {
                "Font for terminal text. Leave empty to use the editor font."
            }
            Self::TerminalFontSize => "Size of terminal text in pixels.",
            Self::TerminalFontWeight => "Weight of terminal text: 400 is regular, 700 is bold.",
            Self::TerminalLineHeight => {
                "Height of each terminal row, as a multiple of the font size."
            }
            Self::DefaultShell => "Used when a new terminal opens without picking a shell.",
            Self::TerminalCursorBlink => "Blink the terminal cursor while it has focus.",
            Self::MatchBrackets => "Highlight the bracket pair beside the caret.",
            Self::IndentGuides => "Show guides at the configured indentation steps.",
            Self::ShowWhitespace => "Show spaces, tabs and line endings without changing the file.",
            Self::SoftWrap => {
                "Wrap long code lines to the editor width without changing source text."
            }
            Self::SmoothCaret => "Animate the editor caret when typing, navigating or clicking.",
            Self::ExplorerAutoReveal => {
                "Expand the file tree to the active editor's file when you switch tabs."
            }
            Self::InlineBlame => "Show the last commit for the current line in the editor.",
            Self::SplitDiff => "Open diffs with the old and new versions in two columns.",
            Self::TreeView => "Group changed files by folder in Source Control.",
            Self::VimMode => "Edit with Vim modes, operators and motions.",
            Self::VimSystemClipboard => {
                "Yank, delete and put without a register name through the clipboard."
            }
            Self::CheckForUpdates => {
                "Look for a new release at startup and daily, and offer it in the title bar."
            }
        }
    }

    /// Current value of an on/off setting; `None` for choice settings.
    pub(crate) fn enabled(self, app: &Frame) -> Option<bool> {
        match self {
            Self::UiScale | Self::DefaultShell => None,
            Self::EditorFontFamily
            | Self::EditorFontSize
            | Self::EditorFontWeight
            | Self::EditorLineHeight
            | Self::TerminalFontFamily
            | Self::TerminalFontSize
            | Self::TerminalFontWeight
            | Self::TerminalLineHeight => None,
            Self::TerminalCursorBlink => Some(app.prefs.terminal_cursor_blink),
            Self::MatchBrackets => Some(app.prefs.editor.match_brackets),
            Self::IndentGuides => Some(app.prefs.editor.indent_guides),
            Self::ShowWhitespace => Some(app.prefs.editor.show_whitespace),
            Self::SoftWrap => Some(app.prefs.editor.soft_wrap),
            Self::SmoothCaret => Some(app.prefs.smooth_caret),
            Self::ExplorerAutoReveal => Some(app.prefs.explorer_auto_reveal),
            Self::InlineBlame => Some(app.prefs.git_inline_blame),
            Self::SplitDiff => Some(app.prefs.git_split_diff),
            Self::TreeView => Some(app.prefs.git_tree_view),
            Self::VimMode => Some(app.prefs.vim.enabled),
            Self::VimSystemClipboard => Some(app.prefs.vim.system_clipboard),
            Self::CheckForUpdates => Some(app.prefs.check_for_updates),
        }
    }

    fn height(self, app: &Frame) -> f32 {
        match self {
            Self::EditorFontFamily | Self::TerminalFontFamily => FAMILY_ROW_H,
            Self::DefaultShell => {
                crate::ui::terminal::settings::height(&app.profile, &app.terminal_ui)
            }
            _ => ROW_H,
        }
    }

    /// The font a font setting edits and the part of it this row controls.
    pub(crate) fn font(self) -> Option<(FontTarget, FontField)> {
        Some(match self {
            Self::EditorFontFamily => (FontTarget::Editor, FontField::Family),
            Self::EditorFontSize => (FontTarget::Editor, FontField::Size),
            Self::EditorFontWeight => (FontTarget::Editor, FontField::Weight),
            Self::EditorLineHeight => (FontTarget::Editor, FontField::LineHeight),
            Self::TerminalFontFamily => (FontTarget::Terminal, FontField::Family),
            Self::TerminalFontSize => (FontTarget::Terminal, FontField::Size),
            Self::TerminalFontWeight => (FontTarget::Terminal, FontField::Weight),
            Self::TerminalLineHeight => (FontTarget::Terminal, FontField::LineHeight),
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FontTarget {
    Editor,
    Terminal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FontField {
    Family,
    Size,
    Weight,
    LineHeight,
}

impl FontTarget {
    fn get_frame(self, frame: &Frame) -> &FontSettings {
        match self {
            Self::Editor => &frame.prefs.editor_font,
            Self::Terminal => &frame.prefs.terminal_font,
        }
    }
}

/// A line-height percentage as a multiplier, e.g. 160 → "1.6", 155 → "1.55".
fn line_height_label(percent: u16) -> String {
    let (whole, hundredths) = (percent / 100, percent % 100);
    if hundredths % 10 == 0 {
        format!("{whole}.{}", hundredths / 10)
    } else {
        format!("{whole}.{hundredths:02}")
    }
}

impl FontField {
    /// Displayed value and whether it can step down / up.
    pub(crate) fn value(self, font: &FontSettings) -> (String, bool, bool) {
        match self {
            Self::Family => (font.family.clone(), false, false),
            Self::Size => (
                font.size.to_string(),
                font.size > fonts::MIN_SIZE,
                font.size < fonts::MAX_SIZE,
            ),
            Self::Weight => (
                font.weight.to_string(),
                font.weight > fonts::MIN_WEIGHT,
                font.weight < fonts::MAX_WEIGHT,
            ),
            Self::LineHeight => (
                line_height_label(font.line_height),
                font.line_height > fonts::MIN_LINE_HEIGHT,
                font.line_height < fonts::MAX_LINE_HEIGHT,
            ),
        }
    }

    pub(crate) fn step(self, font: &FontSettings, up: bool) -> FontSettings {
        let mut font = font.clone();
        let step = |value: u16, by: u16| {
            if up {
                value.saturating_add(by)
            } else {
                value.saturating_sub(by)
            }
        };
        match self {
            Self::Family => {}
            Self::Size => font.size = step(font.size, 1),
            Self::Weight => {
                let snapped = font.weight / fonts::WEIGHT_STEP * fonts::WEIGHT_STEP;
                font.weight = if up || snapped == font.weight {
                    step(snapped, fonts::WEIGHT_STEP)
                } else {
                    snapped
                };
            }
            Self::LineHeight => {
                // Snap to the step grid so a hand-edited value lines up again.
                let snapped = font.line_height / fonts::LINE_HEIGHT_STEP * fonts::LINE_HEIGHT_STEP;
                font.line_height = if up || snapped == font.line_height {
                    step(snapped, fonts::LINE_HEIGHT_STEP)
                } else {
                    snapped
                };
            }
        }
        font.clamped()
    }
}

fn content_height(app: &Frame) -> f32 {
    let rows: f32 = SECTIONS
        .iter()
        .flat_map(|(_, settings)| settings.iter())
        .map(|setting| setting.height(app))
        .sum();
    // The trailing KEYBOARD and ABOUT sections hold one row each.
    PAGE_PAD_TOP
        + HEADER_H
        + (SECTIONS.len() + 2) as f32 * (SECTION_TITLE_H + SECTION_GAP)
        + rows
        + 2.0 * ROW_H
}

pub fn render(rect: UiRect, state: Binding) -> Element {
    let app = state.frame();
    let max_scroll = (content_height(&app) - rect.height()).max(0.0);
    let scroll_y = app.scroll.1.clamp(0.0, max_scroll);

    let page_w = (rect.width() - PAGE_PAD_X * 2.0).clamp(0.0, MAX_PAGE_W);
    let page_left = rect.left + (rect.width() - page_w) / 2.0;
    let page_right = page_left + page_w;
    let mut top = rect.top + PAGE_PAD_TOP - scroll_y;

    let wheel_state = state.clone();
    let mut page = panel(rect, VisualStyle::filled(theme::c().bg))
        .event_policy(EventPolicy::INTERACTIVE)
        .on_wheel(move |cx, delta| {
            let step = match delta.unit {
                WheelUnit::Lines => delta.y * SCROLL_STEP * 3.0,
                WheelUnit::Pixels => delta.y,
            };
            wheel_state.run(Command::Scroll {
                delta: step,
                max: max_scroll,
            });
            cx.stop_propagation();
        });

    let mut content = group(rect);
    let title_w = measure("Settings", 18.0, 700);
    content = content.child(text(
        UiRect::new(
            page_left,
            top,
            page_left + title_w + TEXT_MARGIN,
            top + 26.0,
        ),
        "Settings",
        theme::mono_bold(theme::c().text_bright, 18.0),
    ));
    content = content.child(settings_file_link(
        UiRect::new(page_left, top, page_right, top + 26.0),
        state.clone(),
    ));
    content = content.child(text(
        UiRect::new(page_left, top + 28.0, page_right, top + 48.0),
        "Changes apply immediately and are saved to settings.json.",
        theme::sans(theme::c().text_dim, theme::UI_SIZE),
    ));
    top += HEADER_H;

    for (title, settings) in SECTIONS {
        content = content.child(section_title(page_left, page_right, top, title));
        top += SECTION_TITLE_H;
        for &setting in settings {
            let row = UiRect::new(page_left, top, page_right, top + setting.height(&app));
            content = content.child(setting_row(row, setting, &app, state.clone()));
            top += setting.height(&app);
        }
        top += SECTION_GAP;
    }

    content = content.child(section_title(page_left, page_right, top, "KEYBOARD"));
    top += SECTION_TITLE_H;
    content = content.child(keymap_link_row(
        UiRect::new(page_left, top, page_right, top + ROW_H),
        state.clone(),
    ));
    top += ROW_H + SECTION_GAP;

    content = content.child(section_title(page_left, page_right, top, "ABOUT"));
    top += SECTION_TITLE_H;
    content = content.child(version_row(
        UiRect::new(page_left, top, page_right, top + ROW_H),
        app.about.clone(),
        state.clone(),
    ));

    page = page.child(content);
    clip(rect, 0.0, 0.0).child(page)
}

fn section_title(left: f32, right: f32, top: f32, title: &'static str) -> Element {
    group(UiRect::new(left, top - 1.0, right, top + SECTION_TITLE_H))
        .child(text(
            UiRect::new(left, top, right, top + SECTION_TITLE_H),
            title,
            theme::mono(theme::c().text_dim, theme::SMALL).tracking(0.8),
        ))
        .child(panel(
            UiRect::new(
                left,
                top + SECTION_TITLE_H - 1.0,
                right,
                top + SECTION_TITLE_H,
            ),
            VisualStyle::filled(theme::c().border),
        ))
}

/// "Edit in settings.json", right-aligned on the title row.
fn settings_file_link(row: UiRect, state: Binding) -> Element {
    let label = "Edit in settings.json";
    let width = measure(label, theme::UI_SIZE, 400) + TEXT_MARGIN;
    let link = UiRect::new(
        (row.right - width).max(row.left),
        row.top,
        row.right,
        row.bottom,
    );
    let mut style = theme::sans(theme::c().accent, theme::UI_SIZE);
    style.align = TextAlign::Right;
    panel(link, VisualStyle::default())
        .key("settings-open-settings-file")
        .event_policy(EventPolicy::INTERACTIVE)
        .cursor(CursorIcon::Pointer)
        .on_click(move || {
            state.run(Command::OpenUserFile);
        })
        .child(text(link, label, style))
}

/// Key bindings live on their own Keymap page; Settings links to it.
fn keymap_link_row(row: UiRect, state: Binding) -> Element {
    let link = "Open Keymap";
    let link_w = measure(link, theme::UI_SIZE, 400) + TEXT_MARGIN;
    let text_right = (row.right - link_w - CONTROL_GAP).max(row.left);
    let mut link_style = theme::sans(theme::c().accent, theme::UI_SIZE);
    link_style.align = TextAlign::Right;
    panel(row, VisualStyle::default())
        .key("settings-open-keymap")
        .event_policy(EventPolicy::INTERACTIVE)
        .cursor(CursorIcon::Pointer)
        .on_click(move || state.run(Command::OpenKeymap))
        .child(text(
            UiRect::new(row.left, row.top + 11.0, text_right, row.top + 29.0),
            "Keyboard shortcuts",
            theme::sans(theme::c().text, theme::UI_SIZE),
        ))
        .child(text(
            UiRect::new(row.left, row.top + 30.0, text_right, row.top + 48.0),
            "View every action and its key binding, and customize them in keymap.json.",
            theme::sans(theme::c().text_dim, theme::SMALL + 1.0),
        ))
        .child(text(
            UiRect::new(
                row.right - link_w,
                row.top + 11.0,
                row.right,
                row.top + 29.0,
            ),
            link,
            link_style,
        ))
        .child(panel(
            UiRect::new(row.left, row.bottom - 1.0, row.right, row.bottom),
            VisualStyle::filled(theme::c().active_line),
        ))
}

/// The running version and update status, with "Check now" or the same
/// update action as the title bar button on the right.
fn version_row(row: UiRect, about: About, state: Binding) -> Element {
    let action = about.action;
    let action_w = action.as_ref().map_or(0.0, |action| {
        measure(&action.label, theme::UI_SIZE, 400) + TEXT_MARGIN
    });
    let text_right = (row.right - action_w - CONTROL_GAP).max(row.left);
    let mut element = panel(row, VisualStyle::default())
        .child(text(
            UiRect::new(row.left, row.top + 11.0, text_right, row.top + 29.0),
            "Version",
            theme::sans(theme::c().text, theme::UI_SIZE),
        ))
        .child(text(
            UiRect::new(row.left, row.top + 30.0, text_right, row.top + 48.0),
            about.detail,
            theme::sans(theme::c().text_dim, theme::SMALL + 1.0),
        ))
        .child(panel(
            UiRect::new(row.left, row.bottom - 1.0, row.right, row.bottom),
            VisualStyle::filled(theme::c().active_line),
        ));
    if let Some(action) = action {
        let link = UiRect::new(
            row.right - action_w,
            row.top + 11.0,
            row.right,
            row.top + 29.0,
        );
        let color = if action.actionable {
            theme::c().accent
        } else {
            theme::c().text_dim
        };
        let mut style = theme::sans(color, theme::UI_SIZE);
        style.align = TextAlign::Right;
        let mut button = panel(link, VisualStyle::default()).key("settings-update-action");
        if action.actionable {
            button = button
                .event_policy(EventPolicy::INTERACTIVE)
                .cursor(CursorIcon::Pointer)
                .on_click(move |cx: &mut UiEventContext| {
                    if state.activate_update() {
                        let _ = cx.window().request_close();
                    }
                });
        }
        element = element.child(button.child(text(link, action.label, style)));
    }
    element
}

fn setting_row(row: UiRect, setting: Setting, app: &Frame, state: Binding) -> Element {
    if setting == Setting::DefaultShell {
        return crate::ui::terminal::settings::render(row, state.profile.clone());
    }
    let control_w = match setting.font() {
        Some((_, FontField::Family)) => FAMILY_INPUT_W,
        Some(_) => STEPPER_W,
        None if setting == Setting::UiScale => STEPPER_W + 62.0,
        None => SWITCH_W,
    };
    let control_left = row.right - control_w;
    let text_right = (control_left - CONTROL_GAP).max(row.left);

    let mut element = panel(row, VisualStyle::default());
    element = element.child(text(
        UiRect::new(row.left, row.top + 11.0, text_right, row.top + 29.0),
        setting.label(),
        theme::sans(theme::c().text, theme::UI_SIZE),
    ));
    element = element.child(text(
        UiRect::new(row.left, row.top + 30.0, text_right, row.top + 48.0),
        setting.description(),
        theme::sans(theme::c().text_dim, theme::SMALL + 1.0),
    ));
    element = element.child(panel(
        UiRect::new(row.left, row.bottom - 1.0, row.right, row.bottom),
        VisualStyle::filled(theme::c().active_line),
    ));

    match setting.enabled(app) {
        Some(on) => {
            // The whole row is the hit target, like a native settings list.
            element = element
                .event_policy(EventPolicy::INTERACTIVE)
                .cursor(CursorIcon::Pointer)
                .on_click(move || state.run(Command::Toggle(setting)))
                .child(switch(
                    UiRect::new(
                        control_left,
                        row.top + (row.height() - SWITCH_H) / 2.0,
                        row.right,
                        row.top + (row.height() + SWITCH_H) / 2.0,
                    ),
                    on,
                ));
        }
        None if setting == Setting::UiScale => {
            let top = row.top + (ROW_H - STEPPER_H) / 2.0;
            element = element.child(scale_picker(
                UiRect::new(control_left, top, row.right, top + STEPPER_H),
                app.prefs.ui_scale,
                state,
            ));
        }
        None => {
            let Some((target, field)) = setting.font() else {
                return element;
            };
            let control_top = row.top + (ROW_H - STEPPER_H) / 2.0;
            if field == FontField::Family {
                element = element
                    .child(family_input(
                        UiRect::new(
                            control_left,
                            control_top,
                            row.right,
                            control_top + FAMILY_INPUT_H,
                        ),
                        target,
                        state.clone(),
                    ))
                    .child(font_presets(
                        UiRect::new(
                            row.left,
                            row.top + ROW_H - 6.0,
                            row.right,
                            row.top + ROW_H - 6.0 + PRESET_H,
                        ),
                        target,
                        &target.get_frame(app).family,
                        state,
                    ));
            } else {
                element = element.child(stepper(
                    UiRect::new(
                        control_left,
                        control_top,
                        row.right,
                        control_top + STEPPER_H,
                    ),
                    target,
                    field,
                    target.get_frame(app),
                    state,
                ));
            }
        }
    }
    element
}

fn family_input(frame: UiRect, target: FontTarget, state: Binding) -> Element {
    type Read = fn(&SettingsUiState) -> &InputState;
    type Write = fn(&mut SettingsUiState) -> &mut InputState;
    let (read, write, placeholder): (Read, Write, &str) = match target {
        FontTarget::Editor => (
            |ui| &ui.editor_font_input,
            |ui| &mut ui.editor_font_input,
            "Default (Cascadia Mono)",
        ),
        FontTarget::Terminal => (
            |ui| &ui.terminal_font_input,
            |ui| &mut ui.terminal_font_input,
            "Same as editor",
        ),
    };
    theme::bordered(frame, theme::c().surface, theme::c().border, 4.0, 1.0).child(
        component(
            UiRect::new(
                frame.left + 4.0,
                frame.top + 1.0,
                (frame.right - 4.0).max(frame.left + 4.0),
                frame.bottom - 1.0,
            ),
            move |cx, &rect| {
                let id = cx.use_stable_id();
                let focus = cx.focus_handle(id.clone());
                let submit = state.clone();
                let cancel = state.clone();
                let blur = state.clone();
                let focused = state.clone();
                input::render_access(
                    rect,
                    id,
                    focus,
                    InputBinding::new_access(state.ui.clone(), read, write),
                    AccessInputOptions {
                        label: "Font family",
                        placeholder,
                        style: InputStyle {
                            text: theme::mono(theme::c().text, theme::UI_SIZE),
                            placeholder: theme::mono(theme::c().text_faint, theme::UI_SIZE),
                            caret: theme::c().text,
                            selection: theme::c().selection,
                        },
                        on_submit: Some(Arc::new(move || submit.run(Command::FontCommit(target)))),
                        on_cancel: Some(Arc::new(move || cancel.run(Command::FontCancel(target)))),
                        on_blur: Some(Arc::new(move || blur.run(Command::FontCommit(target)))),
                    },
                )
                .on_focus(move |_| focused.run(Command::FontFocus))
            },
        )
        .key(match target {
            FontTarget::Editor => "settings-editor-font-family",
            FontTarget::Terminal => "settings-terminal-font-family",
        }),
    )
}

/// One-click family choices, each drawn in its own font. The first chip
/// clears the field back to the inherited family.
fn font_presets(rect: UiRect, target: FontTarget, current: &str, state: Binding) -> Element {
    let default_label = match target {
        FontTarget::Editor => "Default",
        FontTarget::Terminal => "Same as editor",
    };
    let mut row = group(rect);
    let mut left = rect.left;
    for (family, label) in std::iter::once(("", default_label))
        .chain(FONT_PRESETS.iter().map(|family| (*family, *family)))
    {
        let width = measure(label, theme::SMALL + 1.0, 400) + 20.0;
        if left + width > rect.right {
            break;
        }
        let chip = UiRect::new(left, rect.top, left + width, rect.bottom);
        let active = current.eq_ignore_ascii_case(family);
        let (fill, border, color) = if active {
            (
                theme::c().selection,
                theme::c().accent,
                theme::c().text_bright,
            )
        } else {
            (theme::c().surface, theme::c().border, theme::c().text_muted)
        };
        let mut text_style =
            TextStyle::new(color, theme::SMALL + 1.0, 400).font_families(fonts::families(family));
        text_style.align = TextAlign::Center;
        let select_state = state.clone();
        row = row.child(
            theme::bordered(chip, fill, border, 4.0, 1.0)
                .key(format!("settings-font-preset-{target:?}-{label}"))
                .event_policy(EventPolicy::INTERACTIVE)
                .cursor(CursorIcon::Pointer)
                .on_click(move || select_state.run(Command::FontPreset(target, family.to_owned())))
                .child(text(chip, label, text_style)),
        );
        left += width + PRESET_GAP;
    }
    row
}

fn scale_picker(rect: UiRect, percent: Option<u16>, state: Binding) -> Element {
    use crate::features::settings::scale;
    let percent = scale::clamped(percent);
    let value = percent.map_or_else(|| "Auto".to_owned(), |value| format!("{value}%"));
    let stepper = UiRect::new(rect.left, rect.top, rect.left + STEPPER_W, rect.bottom);
    let mut value_style = theme::mono(theme::c().text, theme::UI_SIZE);
    value_style.align = TextAlign::Center;
    let mut element = theme::bordered(stepper, theme::c().surface, theme::c().border, 4.0, 1.0)
        .child(text(
            UiRect::new(
                stepper.left + STEP_BUTTON_W,
                rect.top,
                stepper.right - STEP_BUTTON_W,
                rect.bottom,
            ),
            value,
            value_style,
        ));
    for up in [false, true] {
        let enabled = percent.is_none_or(|value| {
            if up {
                value < scale::MAX
            } else {
                value > scale::MIN
            }
        });
        let left = if up {
            stepper.right - STEP_BUTTON_W
        } else {
            stepper.left
        };
        let button = UiRect::new(left, rect.top, left + STEP_BUTTON_W, rect.bottom);
        let mut style = theme::mono(
            if enabled {
                theme::c().text
            } else {
                theme::c().text_ghost
            },
            theme::UI_SIZE + 2.0,
        );
        style.align = TextAlign::Center;
        let mut control = panel(button, VisualStyle::default())
            .key(if up {
                "settings-ui-scale-up"
            } else {
                "settings-ui-scale-down"
            })
            .child(text(button, if up { "+" } else { "\u{2212}" }, style));
        if enabled {
            let state = state.clone();
            control = control
                .event_policy(EventPolicy::INTERACTIVE)
                .cursor(CursorIcon::Pointer)
                .on_click(move || state.run(Command::UiScaleStep(up)));
        }
        element = element.child(control);
    }
    let reset = UiRect::new(stepper.right + 8.0, rect.top, rect.right, rect.bottom);
    let mut style = theme::sans(
        if percent.is_some() {
            theme::c().accent
        } else {
            theme::c().text_ghost
        },
        theme::UI_SIZE,
    );
    style.align = TextAlign::Center;
    let mut reset = panel(reset, VisualStyle::default())
        .key("settings-ui-scale-reset")
        .child(text(reset, "Reset", style));
    if percent.is_some() {
        reset = reset
            .event_policy(EventPolicy::INTERACTIVE)
            .cursor(CursorIcon::Pointer)
            .on_click(move || state.run(Command::UiScaleReset));
    }
    element.child(reset)
}

/// `[−] value [+]` for a numeric font setting.
fn stepper(
    rect: UiRect,
    target: FontTarget,
    field: FontField,
    font: &FontSettings,
    state: Binding,
) -> Element {
    let (value, can_down, can_up) = field.value(font);
    let mut value_style = theme::mono(theme::c().text, theme::UI_SIZE);
    value_style.align = TextAlign::Center;
    let mut element =
        theme::bordered(rect, theme::c().surface, theme::c().border, 4.0, 1.0).child(text(
            UiRect::new(
                rect.left + STEP_BUTTON_W,
                rect.top,
                rect.right - STEP_BUTTON_W,
                rect.bottom,
            ),
            value,
            value_style,
        ));
    for (up, enabled) in [(false, can_down), (true, can_up)] {
        let button = if up {
            UiRect::new(
                rect.right - STEP_BUTTON_W,
                rect.top,
                rect.right,
                rect.bottom,
            )
        } else {
            UiRect::new(rect.left, rect.top, rect.left + STEP_BUTTON_W, rect.bottom)
        };
        let mut glyph_style = theme::mono(
            if enabled {
                theme::c().text
            } else {
                theme::c().text_ghost
            },
            theme::UI_SIZE + 2.0,
        );
        glyph_style.align = TextAlign::Center;
        let mut control = panel(button, VisualStyle::default())
            .key(format!(
                "settings-font-{target:?}-{field:?}-{}",
                if up { "up" } else { "down" }
            ))
            .child(text(button, if up { "+" } else { "\u{2212}" }, glyph_style));
        if enabled {
            let step_state = state.clone();
            control = control
                .event_policy(EventPolicy::INTERACTIVE)
                .cursor(CursorIcon::Pointer)
                .on_click(move || step_state.run(Command::FontStep(target, field, up)));
        }
        element = element.child(control);
    }
    element
}

fn switch(rect: UiRect, on: bool) -> Element {
    let track = if on {
        theme::c().accent
    } else {
        theme::c().text_ghost
    };
    let inset = (SWITCH_H - KNOB) / 2.0;
    let knob_left = if on {
        rect.right - inset - KNOB
    } else {
        rect.left + inset
    };
    panel(rect, VisualStyle::filled(track).radius(SWITCH_H / 2.0)).child(panel(
        UiRect::new(
            knob_left,
            rect.top + inset,
            knob_left + KNOB,
            rect.top + inset + KNOB,
        ),
        VisualStyle::filled(if on {
            theme::c().on_accent
        } else {
            theme::c().text_muted
        })
        .radius(KNOB / 2.0),
    ))
}

