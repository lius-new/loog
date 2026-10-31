//! Extensions that ship inside Loom.
//!
//! They are written to a folder of their own next to the user's extensions each
//! time Loom starts, so they always match the running version, and are loaded
//! like any other package. The user can disable them but not uninstall them.
use std::path::{Path, PathBuf};

/// The folder under the extensions folder that holds the bundled packages.
const FOLDER: &str = ".bundled";

struct Bundled {
    id: &'static str,
    files: &'static [(&'static str, &'static str)],
}

const PACKAGES: &[Bundled] = &[Bundled {
    id: "loom.aura",
    files: &[
        (
            "manifest.json",
            include_str!("../../../assets/extensions/loom.aura/manifest.json"),
        ),
        (
            "icon.svg",
            include_str!("../../../assets/extensions/loom.aura/icon.svg"),
        ),
        (
            "README.md",
            include_str!("../../../assets/extensions/loom.aura/README.md"),
        ),
        (
            "CHANGELOG.md",
            include_str!("../../../assets/extensions/loom.aura/CHANGELOG.md"),
        ),
        (
            "themes/aura-light.json",
            include_str!("../../../assets/extensions/loom.aura/themes/aura-light.json"),
        ),
        (
            "themes/ember.json",
            include_str!("../../../assets/extensions/loom.aura/themes/ember.json"),
        ),
        (
            "themes/high-contrast.json",
            include_str!("../../../assets/extensions/loom.aura/themes/high-contrast.json"),
        ),
    ],
}];

/// Whether a package folder is one of the bundled ones.
pub(crate) fn is_bundled(dir: &Path) -> bool {
    dir.parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name == FOLDER)
}

/// Write the bundled packages under `extensions` and return their folders, with
/// a message for each one that could not be written.
pub(crate) fn unpack(extensions: &Path) -> (Vec<PathBuf>, Vec<String>) {
    unpack_into(&extensions.join(FOLDER), PACKAGES)
}

fn unpack_into(root: &Path, packages: &[Bundled]) -> (Vec<PathBuf>, Vec<String>) {
    let mut dirs = Vec::new();
    let mut problems = Vec::new();
    for package in packages {
        let dir = root.join(package.id);
        match write(&dir, package.files) {
            Ok(()) => dirs.push(dir),
            Err(error) => problems.push(format!("{}: {error}", package.id)),
        }
    }
    (dirs, problems)
}

fn write(dir: &Path, files: &[(&str, &str)]) -> std::io::Result<()> {
    for (path, text) in files {
        let target = dir.join(path);
        // Unchanged files are left alone, so a start does not touch the disk.
        if std::fs::read_to_string(&target).is_ok_and(|have| have == *text) {
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, text)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_package_unpacks_loads_and_cannot_be_uninstalled() {
        let work = std::env::temp_dir().join(format!("loom-bundled-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&work);
        let (dirs, problems) = unpack(&work);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(dirs.len(), PACKAGES.len());

        let mut service = super::super::ExtensionService::new(work.join(".cache"));
        for dir in &dirs {
            assert!(is_bundled(dir));
            service.load_package(dir).unwrap();
        }
        let package = service.package("loom.aura").unwrap();
        assert!(package.resources.rejected.is_empty());
        assert!(service.uninstall("loom.aura").is_err());
        assert!(service.package("loom.aura").is_some());

        // A second start finds everything in place.
        let (again, problems) = unpack(&work);
        assert!(problems.is_empty());
        assert_eq!(again, dirs);
        let _ = std::fs::remove_dir_all(&work);
    }

    #[test]
    fn a_user_package_is_not_bundled() {
        assert!(!is_bundled(Path::new("extensions/loom.midnight")));
        assert!(is_bundled(Path::new("extensions/.bundled/loom.aura")));
    }
}
