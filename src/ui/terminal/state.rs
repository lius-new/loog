//! Terminal interaction and draft state; no session or process ownership.
use crate::ui::components::input::InputState;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PanelTab {
    #[default]
    Terminal,
    Commits,
    Output,
    Problems,
}
impl PanelTab {
    /// Every panel of the default bottom dock, in tab order.
    pub const ALL: [Self; 4] = [Self::Terminal, Self::Commits, Self::Output, Self::Problems];
    pub fn label(self) -> &'static str {
        match self {
            Self::Terminal => "Terminal",
            Self::Commits => "Commits",
            Self::Output => "Output",
            Self::Problems => "Problems",
        }
    }
    /// Registered icon key.
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Terminal => "panel-terminal",
            Self::Commits => "panel-commits",
            Self::Output => "panel-output",
            Self::Problems => "panel-problems",
        }
    }
    /// Stable node id of the tab button.
    pub fn node_id(self) -> &'static str {
        match self {
            Self::Terminal => "bottom-panel-terminal",
            Self::Commits => "bottom-panel-commits",
            Self::Output => "bottom-panel-output",
            Self::Problems => "bottom-panel-problems",
        }
    }
    /// Panels whose body is drawn by the dock itself rather than by a terminal
    /// or the commit history.
    pub fn is_log(self) -> bool {
        matches!(self, Self::Output | Self::Problems)
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct TerminalTabContextMenuState {
    pub position: (f32, f32),
    pub target: crate::features::terminal::SessionId,
    pub kind: TerminalTabMenuKind,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalTabMenuKind {
    Actions,
    Purpose,
    /// The header's more-actions menu: rename, purpose and clear output.
    More,
    /// Every terminal in a list, for strips with more tabs than fit.
    Sessions,
}
#[derive(Clone)]
pub struct ProfileEditor {
    pub open: bool,
    pub id: Option<u64>,
    pub name: InputState,
    pub program: InputState,
    pub args: InputState,
    pub error: Option<String>,
}
/// Live text-selection drag, kept so a timer can keep scrolling while the
/// pointer rests above or below the screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SelectionDrag {
    pub session: crate::features::terminal::SessionId,
    pub content: lgui::prelude::UiRect,
    pub point: lgui::core::Point,
    pub rows: u16,
    pub cols: u16,
}
/// The session being renamed and the text typed so far.
#[derive(Clone)]
pub struct RenameEditor {
    pub target: crate::features::terminal::SessionId,
    pub input: InputState,
}
/// The find box over a terminal: the text typed and how the last search went.
#[derive(Clone)]
pub struct SearchBar {
    pub target: crate::features::terminal::SessionId,
    pub input: InputState,
    pub status: crate::features::terminal::SearchStatus,
}
#[derive(Clone, Default)]
pub struct TerminalUiState {
    pub panel_tab: PanelTab,
    pub terminal_rename: Option<RenameEditor>,
    pub terminal_search: Option<SearchBar>,
    /// Stands in for a text field's state after its editor has closed, so an
    /// event that arrives in the same frame cannot reach a missing editor.
    detached_input: InputState,
    /// Session under the pointer, used to reveal its overlay scrollbar.
    pub hovered_terminal: Option<crate::features::terminal::SessionId>,
    /// Session and pointer offset within the dragged scrollbar thumb.
    pub terminal_scrollbar_drag: Option<(crate::features::terminal::SessionId, f32)>,
    pub selecting_terminal: bool,
    pub selection_drag: Option<SelectionDrag>,
    pub terminal_focused: bool,
    pub terminal_shell_menu: bool,
    pub terminal_shell_scroll: usize,
    pub terminal_shell_selection: Option<crate::features::terminal::ShellKind>,
    pub terminal_workspace_root: Option<std::path::PathBuf>,
    pub terminal_workspace_picker: Option<WorkspacePicker>,
    pub terminal_tab_context_menu: Option<TerminalTabContextMenuState>,
    pub shell_editor: Option<ProfileEditor>,
    pub terminal_split_drag: Option<u64>,
    /// How far each scrolling tab strip is scrolled, keyed by its first tab.
    tab_scrolls: std::collections::BTreeMap<crate::features::terminal::SessionId, f32>,
}
impl TerminalUiState {
    pub fn tab_scroll(&self, strip: crate::features::terminal::SessionId) -> f32 {
        self.tab_scrolls.get(&strip).copied().unwrap_or(0.0)
    }
    pub fn set_tab_scroll(&mut self, strip: crate::features::terminal::SessionId, value: f32) {
        if value > 0.0 {
            self.tab_scrolls.insert(strip, value);
        } else {
            self.tab_scrolls.remove(&strip);
        }
    }
    /// Scrolls a strip by `delta` pixels (positive moves toward the end).
    pub fn scroll_tabs(
        &mut self,
        strip: crate::features::terminal::SessionId,
        delta: f32,
        max: f32,
    ) {
        let next = (self.tab_scroll(strip) + delta).clamp(0.0, max);
        self.set_tab_scroll(strip, next);
    }
    /// The rename field's text. Once the editor has closed, a throwaway one.
    pub fn rename_input(&self) -> &InputState {
        self.terminal_rename
            .as_ref()
            .map_or(&self.detached_input, |editor| &editor.input)
    }
    pub fn rename_input_mut(&mut self) -> &mut InputState {
        match self.terminal_rename {
            Some(ref mut editor) => &mut editor.input,
            None => &mut self.detached_input,
        }
    }
    /// The find box's text. Once the box has closed, a throwaway one.
    pub fn search_input(&self) -> &InputState {
        self.terminal_search
            .as_ref()
            .map_or(&self.detached_input, |bar| &bar.input)
    }
    pub fn search_input_mut(&mut self) -> &mut InputState {
        match self.terminal_search {
            Some(ref mut bar) => &mut bar.input,
            None => &mut self.detached_input,
        }
    }
}
#[derive(Clone)]
pub struct WorkspacePicker {
    pub target: Option<crate::features::terminal::SessionId>,
    pub shell: Option<crate::features::terminal::ShellKind>,
    pub selected: usize,
    pub scroll: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::terminal::SearchStatus;

    #[test]
    fn text_fields_survive_a_late_event_after_their_editor_closed() {
        let mut ui = TerminalUiState::default();
        // A rename or search that just ended can still receive one more event
        // from the field in the same frame; it must not find a missing editor.
        ui.rename_input_mut().set_text("late rename");
        ui.search_input_mut().set_text("late search");
        assert!(ui.terminal_rename.is_none() && ui.terminal_search.is_none());
        assert_eq!(ui.rename_input().text(), "late search");

        ui.terminal_rename = Some(RenameEditor {
            target: 1,
            input: InputState::new("name"),
        });
        ui.terminal_search = Some(SearchBar {
            target: 1,
            input: InputState::new("query"),
            status: SearchStatus::default(),
        });
        assert_eq!(ui.rename_input().text(), "name");
        assert_eq!(ui.search_input().text(), "query");
        ui.rename_input_mut().set_text("renamed");
        ui.search_input_mut().set_text("found");
        assert_eq!(ui.terminal_rename.as_ref().unwrap().input.text(), "renamed");
        assert_eq!(ui.terminal_search.as_ref().unwrap().input.text(), "found");
    }
}
