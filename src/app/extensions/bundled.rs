//! Extensions that ship with Loom.
//!
//! They are the folders in `extensions` beside the executable (staged there by
//! packaging, from the locked packages of the extensions repository), and are loaded like
//! any other package. The user can disable them but not uninstall them.
use crate::infrastructure::language_packs::bundled_extensions_directory;
use std::path::{Path, PathBuf};

/// Whether a package folder is one of the bundled ones.
pub(crate) fn is_bundled(dir: &Path) -> bool {
    match (dir.parent(), bundled_extensions_directory()) {
        (Some(parent), Some(root)) => parent == root,
        _ => false,
    }
}

/// The bundled package folders, in name order.
pub(crate) fn directories() -> Vec<PathBuf> {
    let Some(root) = bundled_extensions_directory() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut found: Vec<_> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.join("manifest.json").is_file())
        .collect();
    found.sort();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shipped_extension_loads_and_cannot_be_uninstalled() {
        let work = std::env::temp_dir().join(format!("loom-bundled-{}", std::process::id()));
        let dirs = directories();
        let names: Vec<_> = dirs
            .iter()
            .map(|dir| dir.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        // The standalone host is tested with every language extension removed.
        assert!(names.contains(&"loom.aura".to_owned()), "{names:?}");
        assert!(
            names
                .iter()
                .all(|name| name == "loom.aura" || name.starts_with("loom.lang.")),
            "{names:?}"
        );

        let mut service = super::super::ExtensionService::new(work.join(".cache"));
        for dir in &dirs {
            assert!(is_bundled(dir));
            let id = service.load_package(dir).unwrap();
            assert!(
                service.package(&id).unwrap().resources.rejected.is_empty(),
                "{id}"
            );
        }
        assert!(service.uninstall("loom.aura").is_err());
        assert!(service.uninstall("loom.lang.json").is_err());
        assert!(service.package("loom.aura").is_some());
        let _ = std::fs::remove_dir_all(&work);
    }

    #[test]
    fn a_user_package_is_not_bundled() {
        assert!(!is_bundled(Path::new("extensions/loom.midnight")));
        assert!(!is_bundled(Path::new("loom.aura")));
    }
}
