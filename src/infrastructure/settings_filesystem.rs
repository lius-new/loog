//! User settings filesystem adapter; parsing and patching are pure feature rules.
pub use crate::features::settings::rules::*;
use crate::infrastructure::session_filesystem::config_path;
use std::sync::{Mutex, OnceLock};
use std::{fs, io};
/// The result of reading `settings.json`.
#[allow(clippy::large_enum_variant)]
pub enum UserSettings {
    Missing,
    Loaded(LoadOutcome),
    /// The file as a whole is unusable; nothing in it applies.
    Invalid(String),
}
pub fn read() -> UserSettings {
    let _guard = settings_lock().lock().expect("settings lock poisoned");
    let Some(path) = config_path(FILE_NAME) else {
        return UserSettings::Missing;
    };
    match fs::read_to_string(&path) {
        Ok(contents) => match parse(&contents) {
            Ok(outcome) => UserSettings::Loaded(outcome),
            Err(error) => UserSettings::Invalid(error),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => UserSettings::Missing,
        Err(error) => UserSettings::Invalid(error.to_string()),
    }
}

/// The settings to start with; problems are reported by the first reload.
pub fn load() -> Settings {
    match read() {
        UserSettings::Loaded(outcome) => outcome.settings,
        UserSettings::Missing | UserSettings::Invalid(_) => Settings::default(),
    }
}

/// Write the members of `settings` that differ from what the file holds.
/// A file that does not parse is left alone so a half-finished hand edit
/// is never overwritten.
pub fn save(settings: &Settings) -> io::Result<()> {
    let _guard = settings_lock().lock().expect("settings lock poisoned");
    let path = config_path(FILE_NAME).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "could not determine the user configuration directory",
        )
    })?;
    save_at(&path, settings)
}

fn save_at(path: &std::path::Path, settings: &Settings) -> io::Result<()> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if *settings == Settings::default() {
                return Ok(());
            }
            template()
        }
        Err(error) => return Err(error),
    };
    let invalid = |error: String| io::Error::new(io::ErrorKind::InvalidData, error);
    let current = parse(&text).map_err(invalid)?.settings;
    let patched = patch(&text, &current, settings)
        .ok_or_else(|| invalid("expected an object of settings".to_owned()))?;
    if patched == text && path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, &patched)?;
    *last_written().lock().expect("settings lock poisoned") = Some(patched);
    Ok(())
}

/// Whether `text` is exactly what `save` last wrote, so the watcher can
/// skip reloading the app's own writes.
pub fn is_own_write(text: &str) -> bool {
    last_written()
        .lock()
        .expect("settings lock poisoned")
        .as_deref()
        == Some(text)
}

/// Create `settings.json` from the template if it does not exist yet.
pub fn ensure_user_file() -> io::Result<std::path::PathBuf> {
    let _guard = settings_lock().lock().expect("settings lock poisoned");
    let path = config_path(FILE_NAME).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "could not determine the user configuration directory",
        )
    })?;
    if !path.exists() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, template())?;
    }
    Ok(path)
}

fn last_written() -> &'static Mutex<Option<String>> {
    static LAST: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    LAST.get_or_init(Default::default)
}

fn settings_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Ignore only the exact contents written by this process; the UI never reads native files.
pub fn current_file_is_own_write() -> bool {
    config_path(FILE_NAME)
        .and_then(|path| fs::read_to_string(path).ok())
        .is_some_and(|text| is_own_write(&text))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provider_settings_are_written_and_reloaded_without_losing_other_preferences() {
        let path = std::env::temp_dir().join(format!(
            "loog-server-settings-{}.json",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let original = r#"// user preferences
{
  "smooth_caret": false,
  "languages": {"rust": {"providers":{"formatting":["example.format"]}}}
}
"#;
        fs::write(&path, original).unwrap();
        let mut settings = parse(original).unwrap().settings;
        settings.languages.get_mut("rust").unwrap().format_on_save = true;
        save_at(&path, &settings).unwrap();
        let written = fs::read_to_string(&path).unwrap();
        assert!(written.starts_with("// user preferences"));
        assert_eq!(parse(&written).unwrap().settings, settings);
        let invalid = "{ unfinished user edit";
        fs::write(&path, invalid).unwrap();
        assert!(save_at(&path, &settings).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), invalid);
        let _ = fs::remove_file(path);
    }
}
