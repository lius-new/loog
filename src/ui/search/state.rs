//! Search input, composition, focus and viewport state; owned separately from business data.
use crate::features::search::{SearchSpec, model::SearchModel};
use crate::ui::components::input::InputState;
use loog_workspace_api::ResourceRef;
use std::collections::HashSet;
use std::ops::{Deref, DerefMut};

#[derive(Clone, Default)]
pub struct SearchUiState {
    pub query: InputState,
    pub include: InputState,
    pub exclude: InputState,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    pub include_hidden: bool,
    pub include_ignored: bool,
    pub filters_open: bool,
    pub details_open: bool,

    pub refresh: u64,
    pub focus_request: u64,
    pub results_focus_request: u64,
    pub results_focused: bool,
    pub scroll_y: f32,
    pub viewport_height: f32,
    pub scrollbar_drag: Option<(f32, f32)>,
    pub selected: Option<(ResourceRef, usize)>,
    pub collapsed: HashSet<ResourceRef>,
}

impl SearchUiState {
    pub fn spec(&self) -> SearchSpec {
        SearchSpec {
            query: self.query.text().into(),
            include: self.include.text().into(),
            exclude: self.exclude.text().into(),
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
            regex: self.regex,
            include_hidden: self.include_hidden,
            include_ignored: self.include_ignored,
        }
    }

    pub fn composing(&self) -> bool {
        [&self.query, &self.include, &self.exclude]
            .iter()
            .any(|input| !input.preedit.is_empty())
    }

    pub fn blur(&mut self) {
        for input in [&mut self.query, &mut self.include, &mut self.exclude] {
            input.focused = false;
            input.preedit.clear();
            input.preedit_cursor = None;
        }
        self.results_focused = false;
    }
}

#[derive(Clone, Default)]
pub struct SearchPanelState {
    pub ui: SearchUiState,
    pub model: SearchModel,
}
impl Deref for SearchPanelState {
    type Target = SearchUiState;
    fn deref(&self) -> &Self::Target {
        &self.ui
    }
}
impl DerefMut for SearchPanelState {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.ui
    }
}
