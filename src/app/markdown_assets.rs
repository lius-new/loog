//! Runtime cache for host-approved image resources, with retained leases.
use crate::features::markdown::resources::{Address, resolve};
use crate::infrastructure::{
    markdown_resources::{self, FileStamp, LoadedImage},
    markdown_worker::{BudgetLease, CacheKind, MemoryBudget},
};
use std::{
    collections::{HashSet, VecDeque},
    path::PathBuf,
    sync::Arc,
};
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Base {
    Local(PathBuf),
    Provided {
        resource: loog_workspace_api::ResourceRef,
        scope: loog_workspace_api::ScopeId,
        session: u64,
        revision: u64,
    },
}
impl Base {
    pub fn for_view(
        app: &super::state::AppState,
        view: crate::domain::identity::ViewId,
    ) -> Option<Self> {
        let id = app.workspace.view_document(view)?;
        let document = app.workspace.documents.get(&id)?;
        Some(match &document.meta.resource {
            loog_workspace_api::ResourceRef::Local { path } => Self::Local(path.clone()),
            resource => {
                let scope = document
                    .provider
                    .as_ref()
                    .map(|(scope, _)| *scope)
                    .unwrap_or(0);
                let revision = app
                    .explorer
                    .resources
                    .root(scope)
                    .map(|root| root.watch.sequence)
                    .unwrap_or(0);
                Self::Provided {
                    resource: resource.clone(),
                    scope,
                    session: app.explorer.resources.session,
                    revision,
                }
            }
        })
    }
}
pub struct Asset {
    pub outcome: Result<LoadedImage, String>,
    watched: Option<PathBuf>,
    stamp: Option<FileStamp>,
    _lease: BudgetLease,
}
/// An image read off the render thread, waiting to be cached.
pub struct Fetched {
    key: String,
    watched: Option<PathBuf>,
    stamp: Option<FileStamp>,
    outcome: Result<LoadedImage, String>,
}
pub struct Assets {
    pub version: u64,
    values: VecDeque<(String, Arc<Asset>)>,
    loading: HashSet<String>,
    budget: MemoryBudget,
}
impl Assets {
    pub fn new(budget: MemoryBudget) -> Self {
        Self {
            version: 0,
            values: VecDeque::new(),
            loading: HashSet::new(),
            budget,
        }
    }
    fn key(base: Option<&Base>, destination: &str, remote: bool) -> String {
        format!("{base:?}|{destination}|{remote}")
    }
    pub fn get(&self, base: Option<&Base>, destination: &str, remote: bool) -> Option<Arc<Asset>> {
        let key = Self::key(base, destination, remote);
        self.values
            .iter()
            .find(|(stored, _)| *stored == key)
            .map(|(_, asset)| asset.clone())
    }
    /// Whether a load for this image is already underway.
    pub fn loading(&self, base: Option<&Base>, destination: &str, remote: bool) -> bool {
        self.loading.contains(&Self::key(base, destination, remote))
    }

    /// Record that `fetch` is running somewhere else, so it is not started twice.
    pub fn begin(&mut self, base: Option<&Base>, destination: &str, remote: bool) {
        self.loading.insert(Self::key(base, destination, remote));
    }

    /// The slow part of a load: file reads, network, decoding. It touches no
    /// cache, so it can run on any thread.
    pub fn fetch(
        base: Option<&Base>,
        destination: &str,
        remote: bool,
        context: Option<&super::context::AppContext>,
        cancelled: &dyn Fn() -> bool,
    ) -> Fetched {
        let local = match base {
            Some(Base::Local(path)) => Some(path.as_path()),
            _ => None,
        };
        let watched = match (!matches!(base, Some(Base::Provided { .. })))
            .then(|| resolve(local, destination))
            .flatten()
        {
            Some(Address::Local { path, .. })
                if remote || !crate::infrastructure::markdown_resources::network_path(&path) =>
            {
                Some(path)
            }
            _ => None,
        };
        let before = watched.as_deref().and_then(markdown_resources::stamp);
        let outcome = match base {
            Some(Base::Provided {
                resource, scope, ..
            }) => {
                if matches!(
                    crate::features::markdown::reader::classify_link(destination),
                    crate::features::markdown::reader::LinkIntent::Web
                ) {
                    markdown_resources::load(None, destination, remote)
                } else if let Some((relative, _)) =
                    crate::features::markdown::resources::provider_relative(destination)
                {
                    context
                        .ok_or_else(|| "Resource runtime is unavailable".to_owned())
                        .and_then(|context| {
                            super::resource_bytes::relative_background(
                                context,
                                *scope,
                                resource,
                                &relative,
                                8 * 1024 * 1024,
                                cancelled,
                            )
                        })
                        .and_then(|bytes| markdown_resources::decode(bytes, None))
                } else {
                    Err("Unsupported resource image address".into())
                }
            }
            _ => markdown_resources::load(local, destination, remote),
        };
        let before = outcome
            .as_ref()
            .ok()
            .and_then(|image| image.local.clone())
            .or(before);
        Fetched {
            key: Self::key(base, destination, remote),
            watched,
            stamp: before,
            outcome,
        }
    }

    /// Load synchronously and cache the result.
    pub fn load(
        &mut self,
        base: Option<&Base>,
        destination: &str,
        remote: bool,
    ) -> Option<Arc<Asset>> {
        let key = Self::key(base, destination, remote);
        if let Some(index) = self.values.iter().position(|(stored, _)| *stored == key) {
            let entry = self.values.remove(index).unwrap();
            let asset = entry.1.clone();
            self.values.push_back(entry);
            return Some(asset);
        }
        self.store(Self::fetch(base, destination, remote, None, &|| false))
    }

    /// Cache a finished `fetch`.
    pub fn store(&mut self, fetched: Fetched) -> Option<Arc<Asset>> {
        let Fetched {
            key,
            watched,
            stamp: before,
            outcome,
        } = fetched;
        self.loading.remove(&key);
        let bytes = key.capacity()
            + outcome.as_ref().map_or_else(
                |error| error.capacity(),
                |image| image.encoded.capacity() + image.width as usize * image.height as usize * 4,
            );
        let lease = loop {
            if let Some(lease) = self.budget.reserve(CacheKind::Image, bytes) {
                break lease;
            }
            self.values.pop_front()?;
        };
        let asset = Arc::new(Asset {
            outcome,
            watched,
            stamp: before,
            _lease: lease,
        });
        if self.values.len() >= 32 {
            self.values.pop_front();
        }
        self.values.push_back((key, asset.clone()));
        self.version = self.version.wrapping_add(1);
        Some(asset)
    }
    pub fn refresh(&mut self) -> bool {
        let before = self.values.len();
        self.values.retain(|(_, value)| {
            value
                .watched
                .as_deref()
                .is_none_or(|path| markdown_resources::stamp(path) == value.stamp)
        });
        if before != self.values.len() {
            self.version = self.version.wrapping_add(1);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod image_tests {
    use super::*;
    use crate::infrastructure::markdown_resources::load;
    #[test]
    fn provided_images_never_watch_or_read_a_native_absolute_path() {
        let resource = |name| {
            loog_workspace_api::ResourceRef::provided(
                loog_workspace_api::EnvironmentKey::new("example.provider", name).unwrap(),
                "opaque:doc",
            )
            .unwrap()
        };
        let first = Base::Provided {
            scope: 1,
            session: 1,
            resource: resource("first"),
            revision: 2,
        };
        let second = Base::Provided {
            scope: 1,
            session: 1,
            resource: resource("second"),
            revision: 2,
        };
        let destination = std::env::temp_dir()
            .join("native-only.png")
            .to_string_lossy()
            .into_owned();
        let fetched = Assets::fetch(Some(&first), &destination, true, None, &|| false);
        assert!(fetched.watched.is_none());
        assert!(fetched.outcome.unwrap_err().contains("Resource runtime"));
        assert_ne!(
            Assets::key(Some(&first), &destination, true),
            Assets::key(Some(&second), &destination, true)
        );
        let invalid = format!("/{}", "a".repeat(33 * 1024));
        let fetched = Assets::fetch(Some(&first), &invalid, true, None, &|| false);
        assert!(fetched.watched.is_none());
        assert!(
            fetched
                .outcome
                .unwrap_err()
                .contains("Unsupported resource")
        );
    }
    #[test]
    fn png_jpeg_dimensions_and_failed_resource_refresh_are_bounded() {
        let directory = std::env::temp_dir().join(format!(
            "loog-markdown-images-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let base = directory.join("README.md");
        for (name, format) in [
            ("image.png", image::ImageFormat::Png),
            ("image.jpg", image::ImageFormat::Jpeg),
        ] {
            image::RgbImage::from_pixel(3, 2, image::Rgb([20, 120, 220]))
                .save_with_format(directory.join(name), format)
                .unwrap();
            let loaded = load(Some(&base), name, false).unwrap();
            assert_eq!((loaded.width, loaded.height), (3, 2));
            assert!(loaded.local.is_some());
        }
        image::RgbImage::new(5000, 1)
            .save(directory.join("wide.png"))
            .unwrap();
        assert!(
            load(Some(&base), "wide.png", false)
                .unwrap_err()
                .contains("dimensions")
        );
        std::fs::write(directory.join("invalid.svg"), "<svg><g></svg>").unwrap();
        assert!(load(Some(&base), "invalid.svg", false).is_err());
        let budget = crate::infrastructure::markdown_worker::MemoryBudget::default();
        let mut assets = crate::app::markdown_assets::Assets::new(budget.clone());
        let asset_base = Base::Local(base.clone());
        let missing = assets.load(Some(&asset_base), "late.png", false).unwrap();
        assert!(missing.outcome.is_err());
        std::fs::copy(directory.join("image.png"), directory.join("late.png")).unwrap();
        assert!(assets.refresh());
        let ready = assets.load(Some(&asset_base), "late.png", false).unwrap();
        assert!(ready.outcome.is_ok());
        assert!(!Arc::ptr_eq(&missing, &ready));
        let held = budget.used()[2];
        drop(assets);
        assert!(budget.used()[2] > 0);
        drop((missing, ready));
        assert!(held > 0);
        assert_eq!(budget.used()[2], 0);
        for name in [
            "image.png",
            "image.jpg",
            "wide.png",
            "invalid.svg",
            "late.png",
        ] {
            std::fs::remove_file(directory.join(name)).unwrap();
        }
        std::fs::remove_dir(directory).unwrap();
    }
}
