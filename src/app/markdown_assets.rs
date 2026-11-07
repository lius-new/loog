//! Runtime cache for host-approved image resources, with retained leases.
use crate::features::markdown::resources::{Address, resolve};
use crate::infrastructure::{
    markdown_resources::{self, FileStamp, LoadedImage},
    markdown_worker::{BudgetLease, CacheKind, MemoryBudget},
};
use std::{
    collections::{HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
};
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
    fn key(base: Option<&Path>, destination: &str, remote: bool) -> String {
        format!("{base:?}|{destination}|{remote}")
    }
    pub fn get(&self, base: Option<&Path>, destination: &str, remote: bool) -> Option<Arc<Asset>> {
        let key = Self::key(base, destination, remote);
        self.values
            .iter()
            .find(|(stored, _)| *stored == key)
            .map(|(_, asset)| asset.clone())
    }
    /// Whether a load for this image is already underway.
    pub fn loading(&self, base: Option<&Path>, destination: &str, remote: bool) -> bool {
        self.loading.contains(&Self::key(base, destination, remote))
    }

    /// Record that `fetch` is running somewhere else, so it is not started twice.
    pub fn begin(&mut self, base: Option<&Path>, destination: &str, remote: bool) {
        self.loading.insert(Self::key(base, destination, remote));
    }

    /// The slow part of a load: file reads, network, decoding. It touches no
    /// cache, so it can run on any thread.
    pub fn fetch(base: Option<&Path>, destination: &str, remote: bool) -> Fetched {
        let watched = match resolve(base, destination) {
            Some(Address::Local { path, .. })
                if remote || !crate::infrastructure::markdown_resources::network_path(&path) =>
            {
                Some(path)
            }
            _ => None,
        };
        let before = watched.as_deref().and_then(markdown_resources::stamp);
        let outcome = markdown_resources::load(base, destination, remote);
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
        base: Option<&Path>,
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
        self.store(Self::fetch(base, destination, remote))
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
        let missing = assets.load(Some(&base), "late.png", false).unwrap();
        assert!(missing.outcome.is_err());
        std::fs::copy(directory.join("image.png"), directory.join("late.png")).unwrap();
        assert!(assets.refresh());
        let ready = assets.load(Some(&base), "late.png", false).unwrap();
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
