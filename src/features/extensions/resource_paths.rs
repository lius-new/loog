//! Package/runtime resource references used by any extension-owned program.
use std::path::{Path, PathBuf};

pub fn resolve_command(
    command: &str,
    package: &Path,
    runtime: impl Fn(&str) -> Option<PathBuf>,
    windows: bool,
) -> Result<String, String> {
    resolve(command, package, runtime, windows)
}
pub fn resolve_argument(
    argument: &str,
    package: &Path,
    runtime: impl Fn(&str) -> Option<PathBuf>,
) -> Result<String, String> {
    resolve(argument, package, runtime, false)
}
fn resolve(
    value: &str,
    package: &Path,
    runtime: impl Fn(&str) -> Option<PathBuf>,
    windows: bool,
) -> Result<String, String> {
    let (base, rest) = if let Some(rest) = value.strip_prefix("${extension}/") {
        (package.to_path_buf(), rest)
    } else if let Some(after) = value.strip_prefix("${runtime.")
        && let Some((name, rest)) = after.split_once("}/")
    {
        let id = format!("loom.runtime.{name}");
        (
            runtime(&id)
                .ok_or_else(|| format!("{id} is not installed or not declared as a dependency"))?,
            rest,
        )
    } else {
        return Ok(value.to_owned());
    };
    if rest.is_empty()
        || rest
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || rest.contains(['\\', ':'])
    {
        return Err(format!(
            "`{value}` does not name a file inside the extension"
        ));
    }
    let mut path = base.join(rest);
    if windows && path.extension().is_none() {
        path.set_extension("exe");
    }
    Ok(path.to_string_lossy().into_owned())
}
