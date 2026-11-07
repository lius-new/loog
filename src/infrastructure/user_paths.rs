//! Shared Windows user-data root for configuration, sessions and downloads.
use std::{ffi::OsString, path::PathBuf};

pub(crate) fn windows_root(get: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    get("APPDATA")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .map(|path| path.join("Loog"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_uses_roaming_for_all_user_data_without_a_local_fallback() {
        let roaming = std::env::temp_dir().join("loog-roaming");
        let local = std::env::temp_dir().join("loog-local");
        assert_eq!(
            windows_root(|key| match key {
                "APPDATA" => Some(roaming.clone().into_os_string()),
                "LOCALAPPDATA" => Some(local.clone().into_os_string()),
                _ => None,
            }),
            Some(roaming.join("Loog"))
        );
        assert_eq!(
            windows_root(|key| (key == "LOCALAPPDATA").then(|| local.clone().into_os_string())),
            None
        );
        assert_eq!(windows_root(|_| Some("relative".into())), None);
        assert_eq!(windows_root(|_| Some("".into())), None);
    }
}
