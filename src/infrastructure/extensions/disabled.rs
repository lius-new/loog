//! The extensions the user turned off, kept beside the installed packages so
//! they stay off after a restart.
use std::collections::BTreeSet;
use std::path::Path;

const FILE: &str = "disabled.json";

/// The ids of the disabled extensions; nothing when the file is missing or unreadable.
pub fn read(extensions: &Path) -> BTreeSet<String> {
    crate::infrastructure::session_filesystem::read_json(&extensions.join(FILE)).unwrap_or_default()
}

pub fn write(extensions: &Path, disabled: &BTreeSet<String>) -> Result<(), String> {
    crate::infrastructure::session_filesystem::write_json(&extensions.join(FILE), disabled)
        .map_err(|error| format!("{FILE}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_disabled_ids_are_kept_between_runs() {
        let dir = std::env::temp_dir().join(format!("loom-disabled-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(read(&dir).is_empty());
        let ids = BTreeSet::from(["a.one".to_owned(), "b.two".to_owned()]);
        write(&dir, &ids).unwrap();
        assert_eq!(read(&dir), ids);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
