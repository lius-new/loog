//! Application settings commands, font coordination and view composition.
use crate::app::context::AppContext;
use crate::app::state::AppState;
use crate::features::keymap::action::Action;
use crate::ui::components::{input::InputState, state_access::StateAccess};
#[cfg(test)]
use crate::ui::fonts;
use crate::ui::fonts::FontSettings;
#[cfg(test)]
use crate::ui::settings::FontField;
use crate::ui::settings::{self, FontTarget, Setting};
use crate::ui::settings_binding::{Binding, Command, Frame};
use lgui::prelude::{Element, UiRect};
pub fn frame(app: &AppState) -> Frame {
    Frame {
        prefs: app.settings(),
        scroll: app.workspace.active_scroll(),
        profile: crate::app::terminal_profiles::settings_frame(app),
        terminal_ui: app.terminal.ui.clone(),
        about: app
            .update
            .about(&crate::features::update::Version::current()),
    }
}
pub fn binding(state: &AppContext) -> Binding {
    let reader = state.clone();
    let writer = state.clone();
    let updater = state.clone();
    Binding::new(
        StateAccess::project(
            state.model(),
            |app| &app.settings_ui,
            |app| &mut app.settings_ui,
        ),
        crate::app::terminal_profiles::binding(state),
        move || frame(&reader.get()),
        move |command| run(&writer, command),
        move || crate::app::update::activate(&updater),
    )
}
pub fn render(rect: UiRect, state: AppContext) -> Element {
    settings::render(rect, binding(&state))
}
pub fn run(state: &AppContext, command: Command) {
    if matches!(command, Command::OpenUserFile) {
        crate::app::key_actions::open_user_settings(state);
        return;
    }
    state.update(|app| match command {
        Command::Toggle(setting) => setting.toggle(app),
        Command::UiScaleStep(up) => {
            app.preferences.values_mut().ui_scale =
                crate::features::settings::scale::step(app.preferences.values().ui_scale, up);
        }
        Command::UiScaleReset => app.preferences.values_mut().ui_scale = None,
        Command::FontPreset(target, family) => target.set_family(app, &family),
        Command::FontStep(target, field, up) => {
            let font = field.step(target.get(app), up);
            target.set(app, font);
        }
        Command::FontCommit(target) => target.commit_input(app),
        Command::FontCancel(target) => target.revert_input(app),
        Command::FontFocus => app.terminal.ui.terminal_focused = false,
        Command::Scroll { delta, max } => {
            let (x, y) = app.workspace.active_scroll();
            app.workspace
                .set_active_scroll(x, (y - delta).clamp(0.0, max));
        }
        Command::OpenKeymap => app.apply(Action::OpenKeymap),
        Command::OpenUserFile => {}
    });
}
impl Setting {
    pub(crate) fn toggle(self, app: &mut AppState) {
        match self {
            Self::UiScale | Self::DefaultShell => {}
            Self::EditorFontFamily
            | Self::EditorFontSize
            | Self::EditorFontWeight
            | Self::EditorLineHeight
            | Self::TerminalFontFamily
            | Self::TerminalFontSize
            | Self::TerminalFontWeight
            | Self::TerminalLineHeight => {}
            Self::TerminalCursorBlink => app.preferences.values_mut().terminal_cursor_blink ^= true,
            Self::MatchBrackets => app.preferences.values_mut().editor.match_brackets ^= true,
            Self::IndentGuides => app.preferences.values_mut().editor.indent_guides ^= true,
            Self::ShowWhitespace => app.preferences.values_mut().editor.show_whitespace ^= true,
            Self::SoftWrap => app.preferences.values_mut().editor.soft_wrap ^= true,
            Self::SmoothCaret => app.preferences.values_mut().smooth_caret ^= true,
            Self::ExplorerAutoReveal => app.preferences.values_mut().explorer_auto_reveal ^= true,
            Self::InlineBlame => app.git.ui.inline_blame ^= true,
            Self::SplitDiff => app.git.ui.split_diff ^= true,
            Self::TreeView => app.git.ui.tree_view ^= true,
            Self::VimMode => {
                let enabled = !app.editor.vim.options.enabled;
                crate::app::editor_vim::set_enabled(app, enabled);
            }
            Self::VimSystemClipboard => app.editor.vim.options.system_clipboard ^= true,
            Self::CheckForUpdates => app.preferences.values_mut().check_for_updates ^= true,
        }
    }
}
impl FontTarget {
    pub(crate) fn get(self, app: &AppState) -> &FontSettings {
        match self {
            Self::Editor => &app.preferences.values().editor_font,
            Self::Terminal => &app.preferences.values().terminal_font,
        }
    }

    pub(crate) fn set(self, app: &mut AppState, font: FontSettings) {
        match self {
            Self::Editor => app.set_editor_font(font),
            Self::Terminal => app.set_terminal_font(font),
        }
    }

    pub(crate) fn set_family(self, app: &mut AppState, family: &str) {
        let font = FontSettings {
            family: family.trim().to_owned(),
            ..self.get(app).clone()
        };
        self.set(app, font);
        // Normalize the field even when the stored family did not change.
        let family = self.get(app).family.clone();
        self.input_mut(app).set_text(family);
    }

    pub(crate) fn input(self, app: &AppState) -> &InputState {
        match self {
            Self::Editor => &app.settings_ui.editor_font_input,
            Self::Terminal => &app.settings_ui.terminal_font_input,
        }
    }

    pub(crate) fn input_mut(self, app: &mut AppState) -> &mut InputState {
        match self {
            Self::Editor => &mut app.settings_ui.editor_font_input,
            Self::Terminal => &mut app.settings_ui.terminal_font_input,
        }
    }

    pub(crate) fn commit_input(self, app: &mut AppState) {
        let family = self.input(app).text().to_owned();
        self.set_family(app, &family);
    }

    pub(crate) fn revert_input(self, app: &mut AppState) {
        let family = self.get(app).family.clone();
        self.input_mut(app).set_text(family);
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_scale_commands_clamp_and_reset_without_changing_fonts() {
        use crate::features::settings::scale;
        use lgui::platform::dpi::ScalePreference;
        let state = crate::app::test_support::live(AppState::new());
        assert_eq!(
            crate::app::window::scale_preference(None),
            ScalePreference::Auto
        );
        run(&state, Command::UiScaleStep(true));
        assert_eq!(state.get().settings().ui_scale, Some(110));
        assert_eq!(
            crate::app::window::scale_preference(Some(110)),
            ScalePreference::Multiplier(1.1)
        );
        for _ in 0..30 {
            run(&state, Command::UiScaleStep(true));
        }
        assert_eq!(state.get().settings().ui_scale, Some(scale::MAX));
        for _ in 0..30 {
            run(&state, Command::UiScaleStep(false));
        }
        assert_eq!(state.get().settings().ui_scale, Some(scale::MIN));
        run(&state, Command::UiScaleReset);
        assert_eq!(state.get().settings().ui_scale, None);
        run(&state, Command::UiScaleStep(false));
        assert_eq!(state.get().settings().ui_scale, Some(90));
        assert_eq!(state.get().settings().editor_font, FontSettings::EDITOR);
        assert_eq!(state.get().settings().terminal_font, FontSettings::TERMINAL);

        state.update(|app| {
            let mut settings = app.settings();
            settings.ui_scale = Some(u16::MAX);
            app.apply_settings(settings);
        });
        assert_eq!(state.get().settings().ui_scale, Some(scale::MAX));
    }

    #[test]
    fn interface_scale_controls_render_and_receive_clicks_at_different_scales() {
        use lgui::application::{AppView, ApplicationContext};
        use lgui::core::{
            InputEvent, Point, PointerButton, PointerData, UiScale, dispatch_runtime_output,
        };
        use lgui::session::UiSession;
        use std::sync::{Arc, Mutex};
        for factor in [0.8, 1.0, 1.5, 2.0] {
            let exposed = Arc::new(Mutex::new(None::<AppContext>));
            let output = exposed.clone();
            let view: AppView = Arc::new(move |cx| {
                let state = AppContext::detached(cx.state_with(AppState::new));
                *output.lock().unwrap() = Some(state.clone());
                render(cx.viewport(), state)
            });
            let context = ApplicationContext::empty(Default::default());
            let mut session = UiSession::new();
            let scale = UiScale::new(factor);
            let viewport = UiRect::new(0.0, 0.0, 1000.0 / factor, 900.0 / factor);
            session.render_view(&view, viewport, scale);
            let state = exposed.lock().unwrap().clone().unwrap();
            for (label, expected) in [("+", Some(110)), ("\u{2212}", Some(100)), ("Reset", None)] {
                let button = session
                    .tree()
                    .nodes()
                    .iter()
                    .find(|node| node.text.as_deref() == Some(label))
                    .unwrap()
                    .layout_rect;
                // Native input arrives in physical pixels and is converted by lgui.
                let center = Point::new(
                    (button.left + button.right) / 2.0,
                    (button.top + button.bottom) / 2.0,
                );
                let point = scale.logical_point(scale.physical_point(center));
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
                    let events = session.handle_input(event);
                    dispatch_runtime_output(
                        events,
                        &context,
                        &lgui::window::WindowId::new("settings-test"),
                        |action| session.handle_default_action(action),
                        |_| {},
                    );
                }
                assert_eq!(
                    state.get().settings().ui_scale,
                    expected,
                    "{label} at {factor}"
                );
                session.render_view(&view, viewport, scale);
                let value = expected.map_or_else(|| "Auto".to_owned(), |value| format!("{value}%"));
                assert!(
                    session
                        .tree()
                        .nodes()
                        .iter()
                        .any(|node| node.text.as_deref() == Some(value.as_str()))
                );
            }
        }
    }

    #[test]
    fn toggles_flip_their_backing_preference() {
        let mut app = AppState::new();
        for setting in [
            Setting::TerminalCursorBlink,
            Setting::SmoothCaret,
            Setting::ExplorerAutoReveal,
            Setting::InlineBlame,
            Setting::SplitDiff,
            Setting::TreeView,
            Setting::VimMode,
            Setting::VimSystemClipboard,
        ] {
            let before = setting.enabled(&frame(&app)).expect("on/off setting");
            setting.toggle(&mut app);
            assert_eq!(setting.enabled(&frame(&app)), Some(!before), "{setting:?}");
        }
    }

    #[test]
    fn default_shell_is_a_choice_not_a_toggle() {
        let mut app = AppState::new();
        let before = app.preferences.values().default_shell;
        assert_eq!(Setting::DefaultShell.enabled(&frame(&app)), None);
        Setting::DefaultShell.toggle(&mut app);
        assert_eq!(app.preferences.values().default_shell, before);
    }

    #[test]
    fn font_steppers_move_within_their_range() {
        let font = FontSettings::EDITOR;
        assert_eq!(FontField::Size.step(&font, true).size, 13);
        assert_eq!(FontField::Size.step(&font, false).size, 11);
        assert_eq!(FontField::LineHeight.step(&font, true).line_height, 210);

        let smallest = FontSettings {
            size: fonts::MIN_SIZE,
            line_height: fonts::MIN_LINE_HEIGHT,
            ..font.clone()
        };
        assert_eq!(FontField::Size.step(&smallest, false).size, fonts::MIN_SIZE);
        let (_, can_down, can_up) = FontField::Size.value(&smallest);
        assert!(!can_down && can_up);
        assert_eq!(
            FontField::LineHeight.step(&smallest, false).line_height,
            fonts::MIN_LINE_HEIGHT
        );

        let off_grid = FontSettings {
            line_height: 155,
            ..font.clone()
        };
        assert_eq!(
            FontField::LineHeight.step(&off_grid, false).line_height,
            150
        );
        assert_eq!(FontField::LineHeight.step(&off_grid, true).line_height, 160);
        assert_eq!(FontField::LineHeight.value(&off_grid).0, "1.55");
        assert_eq!(FontField::LineHeight.value(&font).0, "2.0");
    }

    #[test]
    fn font_weight_commands_clamp_snap_and_keep_editor_and_terminal_independent() {
        let mut app = AppState::new();
        app.workspace
            .open_path("weight.rs".into(), "fn main() {}\n".repeat(100));
        app.workspace.set_active_scroll(17.0, 240.0);
        let state = crate::app::test_support::live(app);
        for _ in 0..3 {
            run(
                &state,
                Command::FontStep(FontTarget::Editor, FontField::Weight, true),
            );
        }
        assert_eq!(state.get().settings().editor_font.weight, 700);
        assert_eq!(state.get().settings().terminal_font.weight, 400);
        assert_eq!(state.get().workspace.active_scroll(), (17.0, 240.0));
        run(
            &state,
            Command::FontStep(FontTarget::Terminal, FontField::Weight, false),
        );
        assert_eq!(state.get().settings().terminal_font.weight, 300);
        assert_eq!(state.get().settings().editor_font.weight, 700);
        for (up, limit) in [(true, fonts::MAX_WEIGHT), (false, fonts::MIN_WEIGHT)] {
            for _ in 0..12 {
                run(
                    &state,
                    Command::FontStep(FontTarget::Editor, FontField::Weight, up),
                );
            }
            let font = state.get().settings().editor_font;
            assert_eq!(font.weight, limit);
            assert_eq!(FontField::Weight.value(&font), (limit.to_string(), up, !up));
        }
        let font = FontSettings {
            weight: 450,
            ..FontSettings::EDITOR
        };
        assert_eq!(FontField::Weight.step(&font, false).weight, 400);
        assert_eq!(FontField::Weight.step(&font, true).weight, 500);
    }

    #[test]
    fn family_input_commits_trimmed_and_reverts_on_cancel() {
        let mut app = AppState::new();
        app.settings_ui.editor_font_input.set_text("  Fira Code ");
        FontTarget::Editor.commit_input(&mut app);
        assert_eq!(app.preferences.values().editor_font.family, "Fira Code");
        assert_eq!(app.settings_ui.editor_font_input.text(), "Fira Code");

        app.settings_ui.editor_font_input.set_text("Typo");
        FontTarget::Editor.revert_input(&mut app);
        assert_eq!(app.settings_ui.editor_font_input.text(), "Fira Code");

        FontTarget::Terminal.set_family(&mut app, "Consolas");
        assert_eq!(app.preferences.values().terminal_font.family, "Consolas");
        assert_eq!(app.settings_ui.terminal_font_input.text(), "Consolas");
        assert_eq!(app.preferences.values().editor_font.family, "Fira Code");
    }

    #[test]
    fn font_settings_are_choices_not_toggles() {
        let mut app = AppState::new();
        for setting in [
            Setting::EditorFontFamily,
            Setting::EditorFontSize,
            Setting::EditorFontWeight,
            Setting::EditorLineHeight,
            Setting::TerminalFontFamily,
            Setting::TerminalFontSize,
            Setting::TerminalFontWeight,
            Setting::TerminalLineHeight,
        ] {
            assert_eq!(setting.enabled(&frame(&app)), None, "{setting:?}");
            assert!(setting.font().is_some(), "{setting:?}");
            setting.toggle(&mut app);
        }
        assert_eq!(app.preferences.values().editor_font, FontSettings::EDITOR);
        assert_eq!(
            app.preferences.values().terminal_font,
            FontSettings::TERMINAL
        );
    }

    #[test]
    fn terminal_font_size_actions_preserve_editor_scroll_and_other_font_fields() {
        let mut app = AppState::new();
        app.workspace
            .open_path("zoom.rs".into(), "fn main() {}\n".repeat(100));
        app.workspace.set_active_scroll(17.0, 240.0);
        app.set_terminal_font(FontSettings {
            family: "Consolas".into(),
            line_height: 180,
            ..FontSettings::TERMINAL
        });
        app.terminal.ui.terminal_focused = true;
        app.apply(Action::IncreaseFontSize);
        assert_eq!(app.preferences.values().terminal_font.size, 13);
        app.apply(Action::DecreaseFontSize);
        assert_eq!(app.preferences.values().terminal_font.size, 12);
        for _ in 0..40 {
            app.apply(Action::IncreaseFontSize);
        }
        assert_eq!(app.preferences.values().terminal_font.size, fonts::MAX_SIZE);
        for _ in 0..40 {
            app.apply(Action::DecreaseFontSize);
        }
        assert_eq!(app.preferences.values().terminal_font.size, fonts::MIN_SIZE);
        app.apply(Action::ResetFontSize);
        assert_eq!(
            app.preferences.values().terminal_font,
            FontSettings {
                family: "Consolas".into(),
                line_height: 180,
                ..FontSettings::TERMINAL
            }
        );
        assert_eq!(app.preferences.values().editor_font, FontSettings::EDITOR);
        assert_eq!(app.workspace.active_scroll(), (17.0, 240.0));
        app.terminal.ui.terminal_focused = false;
        app.apply(Action::IncreaseFontSize);
        assert_eq!(app.preferences.values().editor_font.size, 13);
        assert_eq!(app.preferences.values().terminal_font.size, 12);
    }

    #[test]
    fn font_size_actions_zoom_and_keep_the_top_line() {
        let mut app = AppState::new();
        app.workspace.open_path(
            "zoom.rs".into(),
            "fn main() {}
"
            .repeat(100),
        );
        app.workspace.set_active_scroll(0.0, 240.0);
        app.apply(Action::IncreaseFontSize);
        assert_eq!(app.preferences.values().editor_font.size, 13);
        // 13px at 200% rounds to 26px lines: line 10 stays at the top.
        assert_eq!(app.workspace.active_scroll().1, 260.0);
        app.apply(Action::ResetFontSize);
        assert_eq!(app.preferences.values().editor_font, FontSettings::EDITOR);
        assert_eq!(app.workspace.active_scroll().1, 240.0);
        for _ in 0..40 {
            app.apply(Action::DecreaseFontSize);
        }
        assert_eq!(app.preferences.values().editor_font.size, fonts::MIN_SIZE);
    }

    #[test]
    fn settings_file_values_apply_and_round_trip() {
        let mut app = AppState::new();
        assert!(!app.apply_settings(app.settings()));

        let file = crate::features::settings::rules::parse(
            r#"{ "smooth_caret": false, "vim": { "enabled": true }, "editor_font": { "family": "Consolas", "size": 99 } }"#,
        )
        .unwrap();
        assert!(app.apply_settings(file.settings));
        assert!(!app.preferences.values().smooth_caret);
        assert!(app.editor.vim.options.enabled);
        assert_eq!(app.preferences.values().editor_font.family, "Consolas");
        assert_eq!(app.settings_ui.editor_font_input.text(), "Consolas");
        // Out-of-range values are clamped, so the next save normalizes them.
        assert_eq!(app.preferences.values().editor_font.size, fonts::MAX_SIZE);
        assert_eq!(app.settings().editor_font.size, fonts::MAX_SIZE);
    }
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    #[test]
    fn preference_snapshots_preserve_old_values_without_sharing_live_mutations() {
        let mut app = AppState::new();
        app.preferences.values_mut().terminal_profiles.push(
            crate::features::terminal::profiles::ShellProfile {
                id: 7,
                name: "before".into(),
                program: "before-shell".into(),
                args: vec!["literal argument".into()],
            },
        );
        let saved = app.clone();
        app.preferences.values_mut().terminal_profiles[0].name = "after".into();
        app.set_editor_font(FontSettings {
            family: "Consolas".into(),
            size: 14,
            ..FontSettings::EDITOR
        });
        assert_eq!(
            saved.preferences.values().terminal_profiles[0].name,
            "before"
        );
        assert_eq!(saved.preferences.values().editor_font, FontSettings::EDITOR);
        assert_eq!(app.preferences.values().terminal_profiles[0].name, "after");
        assert_eq!(app.preferences.values().editor_font.family, "Consolas");
        assert_eq!(saved.settings_ui.editor_font_input.text(), "");
        assert_eq!(app.settings_ui.editor_font_input.text(), "Consolas");
    }
}
