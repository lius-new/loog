//! Extensions page: what it shows (an immutable frame of plain values), the
//! view state it keeps, and the semantic commands it sends back.
//!
//! The page only presents. Everything that would act on an extension arrives
//! as a command the app layer decides what to do with.
use crate::ui::components::{input::InputState, state_access::StateAccess};
use std::sync::Arc;

/// Which list the left column shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Source {
    #[default]
    Installed,
    Marketplace,
    Updates,
}

/// The state chips above the installed list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StateFilter {
    #[default]
    All,
    Enabled,
    Disabled,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DetailTab {
    #[default]
    Details,
    Contributions,
    Changelog,
    Runtime,
}

/// Category chips above the marketplace list; `None` is "All".
pub const CATEGORIES: [&str; 4] = ["Languages", "Themes", "Editing", "Linters"];

#[derive(Clone, Default)]
pub struct ExtensionsUiState {
    pub search: InputState,
    pub source: Source,
    pub filter: StateFilter,
    pub category: Option<&'static str>,
    pub selected: Option<String>,
    pub tab: DetailTab,
}

impl ExtensionsUiState {
    pub fn show_installed(&mut self, id: String) {
        self.source = Source::Installed;
        self.search.clear();
        self.filter = StateFilter::All;
        self.category = None;
        self.selected = Some(id);
        self.tab = DetailTab::Details;
    }
}

/// A block of an extension's readme.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Heading(String),
    Paragraph(String),
    Bullets(Vec<String>),
    Code(Vec<String>),
}

/// A table of what an extension adds, such as its commands or languages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    /// Icon key, title and the number of entries in the heading.
    pub icon: &'static str,
    pub title: &'static str,
    pub total: usize,
    pub head: [&'static str; 3],
    /// Whether each column is set in the monospace face.
    pub mono: [bool; 3],
    pub rows: Vec<[String; 3]>,
    /// Rows left out, shown as "and N more".
    pub more: usize,
}

/// A color theme as a small preview card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeCard {
    pub name: String,
    /// `None` when the theme loaded; otherwise why it did not.
    pub rejected: Option<String>,
    pub background: u32,
    pub foreground: u32,
    pub keyword: u32,
    pub function: u32,
    pub string: u32,
    pub comment: u32,
    pub number: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Contribution {
    Table(Table),
    Themes(Vec<ThemeCard>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub date: String,
    pub notes: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Update {
    pub version: String,
    pub notes: String,
}

/// A message the detail shows above its content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub body: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warning,
    Error,
}

/// What the runtime tab shows for an installed extension.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Runtime {
    /// What it does, in a sentence.
    pub summary: String,
    pub log: Vec<(LogLevel, String)>,
}

/// An extension on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    pub enabled: bool,
    pub location: String,
    pub update: Option<Update>,
    /// It could not start: the detail leads with this.
    pub error: Option<Notice>,
    /// Part of it does not work.
    pub warning: Option<Notice>,
    pub runtime: Runtime,
}

/// Marketplace figures for an extension.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Market {
    pub downloads: String,
    pub rating: String,
    pub license: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IconImage {
    pub identity: u64,
    /// Decoded and validated artwork, encoded as PNG for the renderer.
    pub bytes: Arc<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Documentation {
    pub content: crate::features::markdown::content::Markup,
    pub path: std::path::PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub publisher: String,
    pub version: String,
    pub tagline: String,
    pub updated: String,
    /// Icon key and the colour of its tile.
    pub icon: &'static str,
    pub icon_image: Option<IconImage>,
    pub tint: u32,
    pub categories: Vec<String>,
    pub readme: Vec<Block>,
    pub readme_document: Option<Documentation>,
    pub contributions: Vec<Contribution>,
    pub changelog: Vec<Release>,
    pub changelog_document: Option<Documentation>,
    pub installed: Option<Installed>,
    pub market: Option<Market>,
}

impl Entry {
    pub fn documentation(&self, tab: DetailTab) -> Option<&Documentation> {
        match tab {
            DetailTab::Details => self.readme_document.as_ref(),
            DetailTab::Changelog => self.changelog_document.as_ref(),
            _ => None,
        }
    }
    /// How many things the extension adds, as the Contributions tab counts them.
    pub fn contribution_count(&self) -> usize {
        self.contributions
            .iter()
            .map(|contribution| match contribution {
                Contribution::Table(table) => table.total,
                Contribution::Themes(themes) => themes.len(),
            })
            .sum()
    }

    /// Whether the extension is enabled but could not start.
    pub fn failed(&self) -> bool {
        self.installed
            .as_ref()
            .is_some_and(|installed| installed.enabled && installed.error.is_some())
    }

    pub fn matches(&self, query: &str) -> bool {
        let haystack = format!(
            "{} {} {} {} {}",
            self.name,
            self.id,
            self.tagline,
            self.categories.join(" "),
            self.publisher
        )
        .to_lowercase();
        query
            .to_lowercase()
            .split_whitespace()
            .all(|term| haystack.contains(term))
    }
}

#[derive(Clone, Default)]
pub struct Frame {
    pub installed: Vec<Entry>,
    pub marketplace: Vec<Entry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    SetSource(Source),
    SetFilter(StateFilter),
    SetCategory(Option<&'static str>),
    Select(String),
    SetTab(DetailTab),
    ClearSearch,
    /// Search the marketplace for the text typed in the installed list.
    SearchMarketplace,
    /// A button that acts on an extension. The app decides what each does.
    Act(Action, String),
    /// A footer link of the list.
    Footer(FooterAction),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Install,
    Update,
    Enable,
    Disable,
    Reload,
    ShowInFolder,
    Uninstall,
    /// From the marketplace detail to the same extension in the installed list.
    Manage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FooterAction {
    Import,
    RefreshMarketplace,
    UpdateAll,
}

#[derive(Clone)]
pub struct Binding {
    pub ui: StateAccess<ExtensionsUiState>,
    read: Arc<dyn Fn() -> Frame + Send + Sync>,
    run: Arc<dyn Fn(Command) + Send + Sync>,
    documentation:
        Arc<dyn Fn(&Entry, DetailTab) -> Option<crate::ui::markdown::card::Document> + Send + Sync>,
}

impl Binding {
    pub fn new(
        ui: StateAccess<ExtensionsUiState>,
        read: impl Fn() -> Frame + Send + Sync + 'static,
        run: impl Fn(Command) + Send + Sync + 'static,
    ) -> Self {
        Self {
            ui,
            read: Arc::new(read),
            run: Arc::new(run),
            documentation: Arc::new(|entry, tab| {
                entry
                    .documentation(tab)
                    .map(|document| document.content.clone().into())
            }),
        }
    }
    pub fn with_documentation(
        mut self,
        bind: impl Fn(&Entry, DetailTab) -> Option<crate::ui::markdown::card::Document>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.documentation = Arc::new(bind);
        self
    }
    pub fn documentation(
        &self,
        entry: &Entry,
        tab: DetailTab,
    ) -> Option<crate::ui::markdown::card::Document> {
        (self.documentation)(entry, tab)
    }
    pub fn frame(&self) -> Frame {
        (self.read)()
    }
    pub fn run(&self, command: Command) {
        (self.run)(command);
    }
}
