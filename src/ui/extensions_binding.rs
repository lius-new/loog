//! Extensions page: what it shows (an immutable frame of plain values), the
//! view state it keeps, and the semantic commands it sends back.
//!
//! The page only presents. Everything that would act on an extension arrives
//! as a command the app layer decides what to do with.
use crate::domain::media::Thumbnail;
use crate::ui::components::{input::InputState, state_access::StateAccess};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DetailTab {
    #[default]
    Details,
    Contributions,
    Changelog,
}

#[derive(Clone, Default)]
pub struct ExtensionsUiState {
    /// User-selected fraction of the extension page reserved for the list.
    pub split_ratio: Option<f32>,
    pub contribution_expanded: std::collections::BTreeSet<Arc<str>>,
    pub contribution_selected: Option<Arc<str>>,
    pub contribution_scroll: f32,
    pub search: InputState,
    pub selected: Option<String>,
    pub tab: DetailTab,
    /// The extension whose installation is waiting for the user to accept what it asks for.
    pub confirming: Option<String>,
    /// The scroll of the list, of the detail body and of the facts beside a readme.
    pub list: ScrollState,
    pub detail: ScrollState,
    pub aside: ScrollState,
}

/// How far one scrolling area is scrolled, and the thumb drag in progress. The
/// offset belongs to `owner` (what the area shows); showing something else starts at
/// the top.
#[derive(Clone, Debug, Default)]
pub struct ScrollState {
    pub owner: String,
    pub offset: f32,
    pub dragging: bool,
    pub drag_offset: f32,
}

impl ScrollState {
    pub fn at(&self, owner: &str) -> f32 {
        if self.owner == owner {
            self.offset
        } else {
            0.0
        }
    }
}

impl ExtensionsUiState {
    /// Clear the search so `id` is in the list, and select it.
    pub fn show_installed(&mut self, id: String) {
        self.search.clear();
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

/// An extension on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    pub enabled: bool,
    /// It ships with Loog: it can be disabled but not uninstalled.
    pub bundled: bool,
    /// Kept at this version: no update is offered.
    pub pinned: bool,
    /// The version in use before this one, which the extension can go back to.
    pub previous: Option<String>,
    pub location: String,
    pub update: Option<Update>,
    /// It could not start: the detail leads with this.
    pub error: Option<Notice>,
    /// Part of it does not work.
    pub warning: Option<Notice>,
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
        // What the extension adds counts too, so a file type finds the language for it.
        let adds: String = self
            .contributions
            .iter()
            .filter_map(|contribution| match contribution {
                Contribution::Table(table) => Some(table),
                Contribution::Themes(_) => None,
            })
            .flat_map(|table| table.rows.iter())
            .flat_map(|row| row.iter())
            .map(|cell| format!(" {cell}"))
            .collect();
        let haystack = format!(
            "{} {} {} {} {}{adds}",
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

/// How the registry stands: what the Available list is based on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RegistryStatus {
    /// The newest index is being fetched.
    pub refreshing: bool,
    /// Why the newest index could not be used, when what is shown is older.
    pub problem: Option<String>,
    /// The index is past its expiry: nothing new is offered from it.
    pub expired: bool,
    /// Extensions being installed or updated now.
    pub installing: Vec<String>,
    /// Whether an index has been read at all.
    pub loaded: bool,
}

#[derive(Clone, Default)]
pub struct Frame {
    pub installed: Vec<Entry>,
    /// Extensions of the registry that are not installed, newest version of each.
    pub available: Vec<Entry>,
    /// What installing an extension asks for, in words, by extension id.
    pub asks: std::collections::BTreeMap<String, Vec<String>>,
    pub registry: RegistryStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Select(String),
    SetTab(DetailTab),
    ClearSearch,
    /// A button that acts on an extension. The app decides what each does.
    Act(Action, String),
    /// Pick an extension package and install it.
    Import,
    /// Fetch the newest registry index.
    Refresh,
    /// Drop the question of whether to install what an extension asks for.
    CancelInstall,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Enable,
    Disable,
    Reload,
    Uninstall,
    /// Install an extension of the registry (and what it requires).
    Install,
    /// Install the newer version of an installed extension.
    Update,
    /// Keep the installed version; no update is offered.
    Pin,
    Unpin,
    /// Go back to the version in use before this one, and stay on it.
    Rollback,
}

type IconThumbnail = dyn Fn(&IconImage, u32) -> Option<Thumbnail> + Send + Sync;

type DocumentationReader =
    dyn Fn(&Entry, DetailTab) -> Option<crate::ui::markdown::card::Document> + Send + Sync;

#[derive(Clone)]
pub struct Binding {
    pub ui: StateAccess<ExtensionsUiState>,
    read: Arc<dyn Fn() -> Frame + Send + Sync>,
    run: Arc<dyn Fn(Command) + Send + Sync>,
    documentation: Arc<DocumentationReader>,
    icon_thumbnail: Arc<IconThumbnail>,
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
            icon_thumbnail: Arc::new(|_, _| None),
            documentation: Arc::new(|entry, tab| {
                entry
                    .documentation(tab)
                    .map(|document| document.content.clone().into())
            }),
        }
    }
    pub(crate) fn with_icon_thumbnail(
        mut self,
        bind: impl Fn(&IconImage, u32) -> Option<Thumbnail> + Send + Sync + 'static,
    ) -> Self {
        self.icon_thumbnail = Arc::new(bind);
        self
    }

    pub(crate) fn icon_thumbnail(&self, image: &IconImage, edge: u32) -> Option<Thumbnail> {
        (self.icon_thumbnail)(image, edge)
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
