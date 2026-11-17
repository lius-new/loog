//! Palette rows an extension asked the application to keep while it is not running
//! (`loog:palette/retained`).
//!
//! The extension decides what is kept and when it changes; the application stores the rows as
//! they were given, shows them while the extension is installed, enabled, declares the
//! `palette-entry` activation and is at the version that gave them, and starts the extension
//! when one is chosen. Nothing is merged, expired or interpreted here, and nothing listens for
//! an extension being removed or disabled: what is shown is worked out from the current state.
use super::*;
use crate::features::extensions::activation::{Trigger, matches as wakes};
use crate::infrastructure::extensions::retained as store;
use loog_extension_host::{PaletteEntry, PaletteEvent};

/// The most rows chosen while an extension is still starting.
const MAX_PENDING: usize = 8;

/// What is kept, read from disk the first time the palette needs it.
#[derive(Default)]
pub(super) struct Kept {
    loaded: bool,
    pub(super) by_owner: HashMap<String, store::Retained>,
}

fn entry_of(row: &store::Row) -> PaletteEntry {
    PaletteEntry {
        id: row.id.clone(),
        title: row.title.clone(),
        detail: row.detail.clone(),
        category: row.category.clone(),
    }
}

impl ExtensionService {
    fn retained_dir(&self) -> PathBuf {
        self.storage_dir.with_file_name("palette-retained")
    }

    /// Read what is kept, and tidy away what belongs to nothing that is installed.
    fn load_retained(&mut self) {
        // Before the packages are known nothing could be told apart from an orphan.
        if self.retained.loaded || self.packages.is_empty() {
            return;
        }
        self.retained.loaded = true;
        let dir = self.retained_dir();
        store::prune(&dir, |owner| self.packages.contains_key(owner));
        self.retained.by_owner = store::load(&dir)
            .into_iter()
            .filter(|(owner, _)| self.packages.contains_key(owner))
            .collect();
    }

    /// Whether the kept rows of `owner` may be used at all right now.
    fn retained_usable(&self, owner: &str) -> Option<&store::Retained> {
        let package = self.packages.get(owner)?;
        let kept = self.retained.by_owner.get(owner)?;
        (!package.disabled
            && wakes(&package.manifest, &Trigger::PaletteEntry)
            && kept.version == package.manifest.version)
            .then_some(kept)
    }

    /// The rows to show for extensions that are not running, in id order: those of an extension
    /// that is installed, enabled, declares `palette-entry` and is at the version that gave them.
    /// A running extension shows the rows it publishes itself instead.
    pub fn retained_rows(&mut self) -> Vec<(String, Vec<PaletteEntry>)> {
        self.load_retained();
        let mut owners: Vec<&String> = self.retained.by_owner.keys().collect();
        owners.sort();
        owners
            .into_iter()
            .filter(|owner| {
                self.packages
                    .get(*owner)
                    .is_some_and(|package| package.lifecycle.state() != State::Active)
            })
            .filter_map(|owner| {
                let kept = self.retained_usable(owner)?;
                (!kept.rows.is_empty())
                    .then(|| (owner.clone(), kept.rows.iter().map(entry_of).collect()))
            })
            .collect()
    }

    /// An extension handed over the rows to keep; the last request of a round is the one kept.
    /// An empty list forgets them. The extension is told nothing: a row that cannot be kept
    /// is logged, and the extension simply is not woken from the palette.
    pub(super) fn keep_retained(&mut self, id: &str, entries: Vec<PaletteEntry>) {
        let Some(package) = self.packages.get(id) else {
            return;
        };
        if !wakes(&package.manifest, &Trigger::PaletteEntry) {
            crate::features::output::extension_log(
                id,
                crate::features::output::Level::Warn,
                "palette rows are not kept: the manifest does not declare the palette-entry activation",
            );
            return;
        }
        let version = package.manifest.version.clone();
        self.load_retained();
        let dir = self.retained_dir();
        if entries.is_empty() {
            self.retained.by_owner.remove(id);
            store::forget(&dir, id);
            return;
        }
        let kept = store::Retained {
            version,
            rows: entries
                .into_iter()
                .map(|entry| store::Row {
                    id: entry.id,
                    title: entry.title,
                    detail: entry.detail,
                    category: entry.category,
                })
                .collect(),
        };
        match store::save(&dir, id, &kept) {
            Ok(()) => {
                self.retained.by_owner.insert(id.to_owned(), kept);
            }
            Err(error) => crate::features::output::extension_log(
                id,
                crate::features::output::Level::Warn,
                &format!("palette rows are not kept: {error}"),
            ),
        }
    }

    /// A row that was kept for a stopped extension was chosen: start it, then hand it the id as
    /// `picker.event.entry`. The id is the extension's own and may be out of date, so it is
    /// the extension that checks it; the application only passes on ids it was given.
    pub fn choose_retained_entry(&mut self, owner: &str, entry: &str) -> Vec<Effect> {
        self.load_retained();
        let listed = self
            .retained_usable(owner)
            .is_some_and(|kept| kept.rows.iter().any(|row| row.id == entry));
        if !listed {
            return vec![Effect::Error(
                "That palette entry is no longer available".into(),
            )];
        }
        if matches!(
            self.packages[owner].lifecycle.state(),
            State::Failed | State::Stopped
        ) && let Err(error) = self.retry(owner)
        {
            return vec![Effect::Error(format!("{owner}: {error}"))];
        }
        let package = self.packages.get_mut(owner).expect("a usable owner exists");
        let generation = package.lifecycle.generation();
        match package.lifecycle.state() {
            State::Active => vec![Effect::Submit(Job::PaletteEvent {
                id: owner.to_owned(),
                generation,
                event: PaletteEvent::Entry(entry.to_owned()),
            })],
            State::Dormant | State::Activating => {
                let pending = self
                    .pending_palette_entries
                    .entry(owner.to_owned())
                    .or_default();
                if !pending.iter().any(|waiting| waiting == entry) {
                    if pending.len() >= MAX_PENDING {
                        return vec![Effect::Error(format!(
                            "{owner}: too many palette entries waiting for it to start"
                        ))];
                    }
                    pending.push(entry.to_owned());
                }
                if package.lifecycle.state() == State::Activating {
                    return Vec::new();
                }
                if let Err(error) = package.lifecycle.activating() {
                    self.pending_palette_entries.remove(owner);
                    return vec![Effect::Error(format!("{owner}: {error}"))];
                }
                vec![Effect::Submit(Job::Load {
                    id: owner.to_owned(),
                    generation,
                    component: package.component.clone(),
                    package_dir: package.dir.clone(),
                    version: package.manifest.version.clone(),
                    cache: self.cache.clone(),
                    limits: self.guest_limits,
                    storage: storage_of(&self.grants, package)
                        .then(|| self.storage_dir.join(&package.manifest.id)),
                    files: files_of(&self.grants, package),
                    domains: domains_of(&self.grants, package),
                })]
            }
            other => vec![Effect::Error(format!(
                "{owner}: the extension is {other:?}"
            ))],
        }
    }

    /// The chosen rows that waited for `id` to start, now that it has.
    pub(super) fn palette_entries_after_start(&mut self, id: &str, generation: u64) -> Vec<Effect> {
        self.pending_palette_entries
            .remove(id)
            .unwrap_or_default()
            .into_iter()
            .map(|entry| {
                Effect::Submit(Job::PaletteEvent {
                    id: id.to_owned(),
                    generation,
                    event: PaletteEvent::Entry(entry),
                })
            })
            .collect()
    }
}
