//! Project configuration filesystem adapter.
pub use crate::features::settings::project::*;
use crate::features::settings::rules::{self as settings_persistence, Scope};
use std::path::{Path, PathBuf};
use std::{fs, io};
/// Read `root`'s project settings; `Ok(None)` when it has none.
pub fn read(root: &Path) -> Result<Option<ProjectSettings>, String> {
    match fs::read_to_string(path(root)) {
        Ok(text) => {
            let (overrides, errors) = settings_persistence::parse_overrides(&text, Scope::Project)?;
            Ok(Some(ProjectSettings { overrides, errors }))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

/// Create `root`'s project settings file from the template if needed.
pub fn ensure(root: &Path) -> io::Result<PathBuf> {
    let path = path(root);
    if !path.exists() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, template())?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_with_a_byte_order_mark_is_read() {
        let root = std::env::temp_dir().join("loog-project-settings-bom-test");
        fs::create_dir_all(root.join(DIR_NAME)).unwrap();
        fs::write(path(&root), "\u{feff}{ \"default_shell\": \"bash\" }\n").unwrap();
        let settings = read(&root);
        fs::remove_dir_all(&root).unwrap();
        let settings = settings.unwrap().unwrap();
        assert!(settings.errors.is_empty(), "{:?}", settings.errors);
        assert_eq!(
            settings.overrides.get("default_shell"),
            Some(&"bash".into())
        );
    }
}
