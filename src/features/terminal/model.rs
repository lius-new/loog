//! Pure terminal session metadata. Runtime resources belong to the application service.
use crate::features::terminal::{SessionId, ShellKind, TERMINAL_PURPOSES};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProvidedTerminal {
    pub root: loog_workspace_api::ResourceRef,
    pub cwd: loog_workspace_api::ProviderPath,
    pub name: String,
}

#[derive(Clone)]
pub struct TerminalMetadata {
    id: SessionId,
    /// Sessions with the same group share a tab and render side by side.
    group_id: u64,
    width_weight: f32,
    shell: ShellKind,
    cwd: Option<PathBuf>,
    /// A name the user typed; replaces the shell's short label in the tab.
    title: Option<String>,
    purpose: Option<String>,
    running_program: Option<String>,
    provided: Option<ProvidedTerminal>,
    start_provided: bool,
}

/// Longest custom session name kept, in characters.
const TITLE_MAX_CHARS: usize = 48;

impl TerminalMetadata {
    pub fn provided(&self) -> Option<&ProvidedTerminal> {
        self.provided.as_ref()
    }
    pub fn start_provided(&self) -> bool {
        self.start_provided
    }
    pub fn title(&self) -> Option<String> {
        self.title.clone()
    }
    pub fn id(&self) -> SessionId {
        self.id
    }
    pub fn group_id(&self) -> u64 {
        self.group_id
    }
    pub fn width_weight(&self) -> f32 {
        self.width_weight
    }
    pub fn shell(&self) -> ShellKind {
        self.shell
    }
    pub fn cwd(&self) -> Option<PathBuf> {
        self.cwd.clone()
    }
    pub fn purpose(&self) -> Option<String> {
        self.purpose.clone()
    }
    pub fn running_program(&self) -> Option<String> {
        self.running_program.clone()
    }
}
#[derive(Clone)]
pub struct TerminalModel {
    tabs: Vec<TerminalMetadata>,
    active_id: Option<u64>,
    next_id: u64,
}

impl TerminalModel {
    pub fn new() -> Self {
        let mut tabs = Self::empty();
        tabs.add(ShellKind::default());
        tabs
    }

    pub fn restored(shells: Vec<ShellKind>, active_index: Option<usize>) -> Self {
        Self::restored_with_groups(shells, active_index, &[])
    }

    pub fn restored_with_groups(
        shells: Vec<ShellKind>,
        active_index: Option<usize>,
        groups: &[usize],
    ) -> Self {
        let mut tabs = Self::empty();
        for (index, shell) in shells.into_iter().enumerate() {
            let id = tabs.add(shell);
            if index > 0
                && groups.get(index).is_some()
                && groups.get(index) == groups.get(index - 1)
            {
                let group = tabs.tabs[index - 1].group_id;
                tabs.tabs.last_mut().unwrap().group_id = group;
            }
            debug_assert_eq!(tabs.active_id, Some(id));
        }
        if let Some(active_id) = active_index
            .and_then(|index| tabs.tabs.get(index))
            .map(|tab| tab.id)
        {
            tabs.active_id = Some(active_id);
        }
        tabs
    }

    fn empty() -> Self {
        Self {
            tabs: Vec::new(),
            active_id: None,
            next_id: 1,
        }
    }

    pub fn tabs(&self) -> &[TerminalMetadata] {
        &self.tabs
    }

    pub fn active_id(&self) -> Option<u64> {
        self.active_id
    }

    pub fn active(&self) -> Option<&TerminalMetadata> {
        let active_id = self.active_id?;
        self.tabs.iter().find(|tab| tab.id == active_id)
    }

    pub fn active_group(&self) -> Vec<TerminalMetadata> {
        let Some(active) = self.active() else {
            return Vec::new();
        };
        self.tabs
            .iter()
            .filter(|tab| tab.group_id == active.group_id)
            .cloned()
            .collect()
    }

    /// Project the region's sessions while preserving their side-by-side groups.
    pub fn region_view(&self, ids: &[u64], active: Option<u64>) -> Self {
        let tabs = ids
            .iter()
            .filter_map(|id| self.tabs.iter().find(|tab| tab.id == *id))
            .cloned()
            .collect::<Vec<_>>();
        let active_id = self
            .active_id
            .filter(|id| ids.contains(id))
            .or(active.filter(|id| ids.contains(id)))
            .or_else(|| tabs.first().map(|t| t.id));
        Self {
            tabs,
            active_id,
            next_id: self.next_id,
        }
    }

    /// Moving a session out of a side-by-side group does not restart it.
    pub fn detach_group(&mut self, id: u64) {
        if let Some(group) = self
            .tabs
            .iter()
            .find(|tab| tab.id == id)
            .map(|tab| tab.group_id)
            && let Some(other) = self
                .tabs
                .iter()
                .find(|tab| tab.id != id && tab.group_id == group)
                .map(|tab| tab.id)
        {
            for tab in &mut self.tabs {
                if tab.group_id == group && tab.id != id {
                    tab.group_id = other;
                }
            }
        }
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            tab.group_id = tab.id;
            tab.width_weight = 1.0;
        }
    }

    pub fn saved_groups(&self) -> Vec<usize> {
        let mut group = 0;
        self.tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                if index > 0 && self.tabs[index - 1].group_id != tab.group_id {
                    group += 1;
                }
                group
            })
            .collect()
    }

    pub fn widths(&self) -> Vec<f32> {
        self.tabs.iter().map(|tab| tab.width_weight).collect()
    }

    pub fn purposes(&self) -> Vec<Option<String>> {
        self.tabs.iter().map(|tab| tab.purpose.clone()).collect()
    }

    pub fn titles(&self) -> Vec<Option<String>> {
        self.tabs.iter().map(|tab| tab.title.clone()).collect()
    }

    pub fn restore_titles(&mut self, titles: &[Option<String>]) {
        for (tab, title) in self.tabs.iter_mut().zip(titles) {
            tab.title = normalized_title(title.as_deref());
        }
    }

    /// `None`, or a name that is blank once trimmed, restores the automatic name.
    pub fn set_title(&mut self, id: u64, title: Option<&str>) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            tab.title = normalized_title(title);
        }
    }

    pub fn directories(&self) -> Vec<Option<PathBuf>> {
        self.tabs.iter().map(|tab| tab.cwd.clone()).collect()
    }

    pub fn restore_directories(&mut self, directories: &[Option<PathBuf>]) {
        for (tab, cwd) in self.tabs.iter_mut().zip(directories) {
            tab.cwd = cwd.clone();
        }
    }

    pub fn update_directories(&mut self, directories: &[(u64, Option<PathBuf>)]) {
        for (id, cwd) in directories {
            if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == *id) {
                if tab.provided.is_none() {
                    tab.cwd = cwd.clone();
                }
            }
        }
    }

    pub fn restore_purposes(&mut self, purposes: &[Option<String>]) {
        for (tab, purpose) in self.tabs.iter_mut().zip(purposes) {
            tab.purpose = normalized_purpose(purpose.as_deref());
        }
    }

    pub fn set_purpose(&mut self, id: u64, purpose: Option<&str>) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            tab.purpose = normalized_purpose(purpose);
        }
    }

    pub fn update_programs(&mut self, programs: &[(u64, Option<String>)]) {
        for (id, program) in programs {
            if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == *id) {
                tab.running_program = program.clone();
            }
        }
    }

    pub fn restore_widths(&mut self, widths: &[f32]) {
        for (tab, width) in self.tabs.iter_mut().zip(widths) {
            if width.is_finite() && *width > 0.0 {
                tab.width_weight = width.clamp(0.001, 1000.0);
            }
        }
    }

    pub fn split(&mut self, target: u64) -> Option<u64> {
        let index = self.tabs.iter().position(|tab| tab.id == target)?;
        let group_id = self.tabs[index].group_id;
        let shell = self.tabs[index].shell;
        let cwd = self.tabs[index].cwd.clone();
        let provided = self.tabs[index].provided.clone();
        let width_weight = self.tabs[index].width_weight * 0.5;
        self.tabs[index].width_weight = width_weight;
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.tabs.insert(
            index + 1,
            TerminalMetadata {
                id,
                group_id,
                width_weight,
                shell,
                cwd,
                title: None,
                purpose: None,
                running_program: None,
                provided,
                start_provided: true,
            },
        );
        self.active_id = Some(id);
        Some(id)
    }

    pub fn shells(&self) -> Vec<ShellKind> {
        self.tabs.iter().map(|tab| tab.shell).collect()
    }

    pub fn active_index(&self) -> Option<usize> {
        let active_id = self.active_id?;
        self.tabs.iter().position(|tab| tab.id == active_id)
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn add(&mut self, shell: ShellKind) -> u64 {
        self.add_at(shell, None)
    }

    pub fn add_at(&mut self, shell: ShellKind, cwd: Option<PathBuf>) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.tabs.push(TerminalMetadata {
            id,
            group_id: id,
            width_weight: 1.0,
            shell,
            cwd,
            title: None,
            purpose: None,
            running_program: None,
            provided: None,
            start_provided: false,
        });
        self.active_id = Some(id);
        id
    }

    pub fn add_provided(&mut self, target: ProvidedTerminal) -> u64 {
        let id = self.add(ShellKind::System);
        let tab = self.tabs.last_mut().unwrap();
        tab.provided = Some(target);
        tab.start_provided = true;
        id
    }
    pub fn resources(&self) -> Vec<Option<ProvidedTerminal>> {
        self.tabs.iter().map(|tab| tab.provided.clone()).collect()
    }
    pub fn restore_resources(&mut self, targets: &[Option<ProvidedTerminal>]) {
        for (tab, target) in self.tabs.iter_mut().zip(targets) {
            tab.provided = target.clone();
            tab.start_provided = false;
            if target.is_some() {
                tab.cwd = None;
            }
        }
    }
    pub fn update_resources(&mut self, targets: &[Option<ProvidedTerminal>]) {
        for (tab, target) in self.tabs.iter_mut().zip(targets) {
            if tab.provided.is_some() {
                tab.provided = target.clone();
            }
        }
    }

    pub fn select(&mut self, id: u64) {
        if self.tabs.iter().any(|tab| tab.id == id) {
            self.active_id = Some(id);
        }
    }

    pub fn resize_pair(&mut self, left: u64, right: u64, fraction: f32) {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == left) else {
            return;
        };
        let Some(next) = self.tabs.get(index + 1) else {
            return;
        };
        if next.id != right || next.group_id != self.tabs[index].group_id {
            return;
        }
        let total = self.tabs[index].width_weight + next.width_weight;
        let fraction = fraction.clamp(0.001, 0.999);
        self.tabs[index].width_weight = total * fraction;
        self.tabs[index + 1].width_weight = total * (1.0 - fraction);
    }

    pub fn close(&mut self, id: u64) -> Option<TerminalMetadata> {
        let index = self.tabs.iter().position(|tab| tab.id == id)?;
        let removed = self.tabs.remove(index);
        if self.active_id == Some(id) {
            self.active_id = self
                .tabs
                .iter()
                .skip(index)
                .find(|tab| tab.group_id == removed.group_id)
                .or_else(|| {
                    self.tabs
                        .iter()
                        .rev()
                        .find(|tab| tab.group_id == removed.group_id)
                })
                .or_else(|| self.tabs.get(index).or_else(|| self.tabs.last()))
                .map(|tab| tab.id);
        }
        Some(removed)
    }
}

fn normalized_title(title: Option<&str>) -> Option<String> {
    let title = title.map(str::trim).filter(|value| !value.is_empty())?;
    Some(title.chars().take(TITLE_MAX_CHARS).collect())
}

fn normalized_purpose(purpose: Option<&str>) -> Option<String> {
    let purpose = purpose.map(str::trim).filter(|value| !value.is_empty())?;
    // Older saved custom purposes use the general preset under the new policy.
    Some(
        if TERMINAL_PURPOSES.contains(&purpose) {
            purpose
        } else {
            "Other"
        }
        .to_owned(),
    )
}

impl Default for TerminalModel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restored_provider_terminals_keep_identity_but_do_not_start_or_use_local_directories() {
        use loog_workspace_api::{EnvironmentKey, ProviderPath, ResourceRef};
        let target = ProvidedTerminal {
            root: ResourceRef::provided(
                EnvironmentKey::new("example.provider", "one").unwrap(),
                "/work",
            )
            .unwrap(),
            cwd: ProviderPath::new("/work/中文").unwrap(),
            name: "Example".into(),
        };
        let mut tabs = TerminalModel::restored(vec![], None);
        tabs.add_provided(target.clone());
        assert!(tabs.active().unwrap().start_provided());
        let wire = serde_json::to_string(&tabs.resources()).unwrap();
        let mut restored = TerminalModel::restored(tabs.shells(), Some(0));
        restored.restore_directories(&[Some("C:/unrelated".into())]);
        restored.restore_resources(
            &serde_json::from_str::<Vec<Option<ProvidedTerminal>>>(&wire).unwrap(),
        );
        let tab = restored.active().unwrap();
        assert!(!tab.start_provided());
        assert_eq!(tab.provided(), Some(&target));
        assert!(tab.cwd().is_none());
    }
    #[test]
    fn terminal_tabs_add_select_and_close_neighboring_sessions() {
        let mut tabs = TerminalModel::new();
        let first = tabs.active_id().expect("initial terminal tab");
        let second = tabs.add(ShellKind::Bash);
        assert_eq!(tabs.tabs().len(), 2);
        assert_eq!(tabs.active_id(), Some(second));
        assert_eq!(tabs.active().map(|tab| tab.shell), Some(ShellKind::Bash));

        tabs.select(first);
        assert_eq!(tabs.active_id(), Some(first));
        tabs.close(first);
        assert_eq!(tabs.active_id(), Some(second));
        tabs.close(second);
        assert!(tabs.is_empty());
        assert_eq!(tabs.active_id(), None);
    }

    #[test]
    fn saved_custom_purposes_migrate_to_other_and_only_presets_are_kept() {
        let mut tabs = TerminalModel::restored(vec![ShellKind::Bash; 3], None);
        tabs.restore_purposes(&[Some("Local testing".into()), Some("Server".into()), None]);
        assert_eq!(
            tabs.purposes(),
            vec![Some("Other".into()), Some("Server".into()), None]
        );
        let first = tabs.tabs()[0].id;
        for &purpose in TERMINAL_PURPOSES {
            tabs.set_purpose(first, Some(purpose));
            assert_eq!(tabs.tabs()[0].purpose.as_deref(), Some(purpose));
        }
        tabs.set_purpose(first, Some("arbitrary custom name"));
        assert_eq!(tabs.tabs()[0].purpose.as_deref(), Some("Other"));
    }

    #[test]
    fn custom_titles_are_trimmed_capped_restored_and_cleared_when_blank() {
        let mut tabs = TerminalModel::restored(vec![ShellKind::Bash; 3], None);
        let first = tabs.tabs()[0].id;
        tabs.set_title(first, Some("  API server  "));
        assert_eq!(tabs.tabs()[0].title().as_deref(), Some("API server"));
        tabs.set_title(first, Some(&"长".repeat(100)));
        assert_eq!(tabs.tabs()[0].title().unwrap().chars().count(), 48);
        tabs.set_title(first, Some("   "));
        assert_eq!(tabs.tabs()[0].title(), None);

        tabs.restore_titles(&[Some("web".into()), None, Some(" db ".into())]);
        assert_eq!(
            tabs.titles(),
            vec![Some("web".into()), None, Some("db".into())]
        );
        // A split starts with the automatic name, like its purpose.
        let split = tabs.split(first).unwrap();
        assert_eq!(
            tabs.tabs()
                .iter()
                .find(|tab| tab.id() == split)
                .unwrap()
                .title(),
            None
        );
        // A title never changes the purpose.
        assert!(tabs.purposes().iter().all(Option::is_none));
    }

    #[test]
    fn grouped_terminal_layout_round_trips_and_legacy_sessions_stay_separate() {
        let mut original = TerminalModel::new();
        let first = original.active_id().unwrap();
        let split = original.split(first).unwrap();
        original.add_at(ShellKind::Bash, Some("workspace".into()));
        original.select(split);
        original.resize_pair(first, split, 0.75);
        let mut restored = TerminalModel::restored_with_groups(
            original.shells(),
            original.active_index(),
            &original.saved_groups(),
        );
        restored.restore_widths(&original.widths());
        assert_eq!(restored.widths(), original.widths());
        assert_eq!(restored.saved_groups(), vec![0, 0, 1]);
        assert_eq!(restored.active_index(), Some(1));
        assert_eq!(restored.active_group().len(), 2);
        let region = restored.region_view(&[first, split], Some(first));
        assert_eq!(region.active_group().len(), 2);
        assert_eq!(region.widths(), original.widths()[..2]);
        let legacy = TerminalModel::restored(original.shells(), original.active_index());
        assert_eq!(legacy.saved_groups(), vec![0, 1, 2]);
    }

    #[test]
    fn terminal_tab_can_request_its_own_working_directory() {
        let mut tabs = TerminalModel::restored(Vec::new(), None);
        let cwd = PathBuf::from("workspace").join("nested");

        tabs.add_at(ShellKind::default(), Some(cwd.clone()));

        assert_eq!(tabs.active().and_then(|tab| tab.cwd.as_ref()), Some(&cwd));
    }

    #[test]
    fn terminal_tabs_restore_shell_order_and_active_index() {
        let tabs = TerminalModel::restored(
            vec![ShellKind::Cmd, ShellKind::Bash, ShellKind::PowerShell],
            Some(1),
        );

        assert_eq!(
            tabs.shells(),
            vec![ShellKind::Cmd, ShellKind::Bash, ShellKind::PowerShell]
        );
        assert_eq!(tabs.active_index(), Some(1));
        assert_eq!(tabs.active().map(|tab| tab.shell), Some(ShellKind::Bash));

        let empty = TerminalModel::restored(Vec::new(), None);
        assert!(empty.is_empty());
        assert_eq!(empty.active_index(), None);
    }
}
