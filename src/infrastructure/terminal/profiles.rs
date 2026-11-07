//! Platform defaults and user-defined shell profiles. Discovery never launches
//! a program and custom arguments are passed directly, without shell expansion.

use crate::features::terminal::ShellKind;
use std::env;
use std::path::{Path, PathBuf};

use crate::features::terminal::profiles::{ShellOption, ShellProfile, system_shell_from};

pub fn custom(profile: &ShellProfile) -> ShellOption {
    ShellOption {
        kind: ShellKind::Custom(profile.id),
        name: profile.name.clone(),
        program: resolve_program(&profile.program),
        args: profile.args.clone(),
    }
}

pub fn options(profiles: &[ShellProfile]) -> Vec<ShellOption> {
    let mut options = defaults();
    options.extend(profiles.iter().map(custom));
    options
}

pub fn defaults() -> Vec<ShellOption> {
    #[cfg(windows)]
    let kinds = &ShellKind::ALL[..];
    #[cfg(not(windows))]
    let kinds = &[ShellKind::System][..];
    let options = kinds.iter().filter_map(|kind| builtin(*kind)).collect();
    #[cfg(windows)]
    {
        let mut options: Vec<ShellOption> = options;
        options.extend(super::wsl::options());
        options
    }
    #[cfg(not(windows))]
    options
}

pub fn builtin(kind: ShellKind) -> Option<ShellOption> {
    if matches!(kind, ShellKind::Wsl(_)) {
        #[cfg(windows)]
        return super::wsl::options().into_iter().find(|o| o.kind == kind);
        #[cfg(not(windows))]
        return None;
    }
    let program = match kind {
        ShellKind::PowerShell => ["pwsh.exe", "powershell.exe", "pwsh", "powershell"]
            .iter()
            .find_map(|p| resolve_program(Path::new(p))),
        ShellKind::Cmd => {
            let comspec = env::var_os("COMSPEC")
                .map(PathBuf::from)
                .and_then(|p| executable(&p).then_some(p));
            comspec
                .or_else(|| resolve_program(Path::new("cmd.exe")))
                .or_else(|| resolve_program(Path::new("cmd")))
        }
        ShellKind::Bash => {
            #[cfg(windows)]
            let git = find_git_bash();
            #[cfg(not(windows))]
            let git = None;
            git.or_else(|| resolve_program(Path::new("bash.exe")))
                .or_else(|| resolve_program(Path::new("bash")))
        }
        ShellKind::System => system_shell(),
        ShellKind::Custom(_) | ShellKind::Wsl(_) => None,
    }?;
    Some(ShellOption {
        kind,
        name: kind.label().into(),
        program: Some(program),
        args: Vec::new(),
    })
}

pub fn resolve_program(program: &Path) -> Option<PathBuf> {
    if program.as_os_str().is_empty() {
        return None;
    }
    if program.is_absolute() || program.components().count() > 1 {
        return executable(program).then(|| program.to_path_buf());
    }
    env::split_paths(&env::var_os("PATH")?).find_map(|directory| {
        let path = directory.join(program);
        if executable(&path) {
            return Some(path);
        }
        #[cfg(windows)]
        if path.extension().is_none() {
            let path = path.with_extension("exe");
            if executable(&path) {
                return Some(path);
            }
        }
        None
    })
}

fn executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        path.extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe") || ext.eq_ignore_ascii_case("com"))
    }
}

fn system_shell() -> Option<PathBuf> {
    system_shell_from(
        env::var_os("SHELL").map(PathBuf::from),
        login_shell(),
        executable,
    )
}

#[cfg(unix)]
fn login_shell() -> Option<PathBuf> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;
    let mut size = 4096;
    loop {
        let mut buffer = vec![0u8; size];
        let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        // getpwuid_r owns no shared static storage; copy pw_shell before the
        // buffer and passwd record are dropped.
        let error = unsafe {
            libc::getpwuid_r(
                libc::getuid(),
                entry.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if error == libc::ERANGE && size < 1_048_576 {
            size *= 2;
            continue;
        }
        if error != 0 || result.is_null() {
            return None;
        }
        let shell = unsafe { (*result).pw_shell };
        if shell.is_null() {
            return None;
        }
        let bytes = unsafe { CStr::from_ptr(shell) }.to_bytes();
        return Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)));
    }
}

#[cfg(not(unix))]
fn login_shell() -> Option<PathBuf> {
    None
}

fn candidates(kind: ShellKind) -> &'static [&'static str] {
    match kind {
        ShellKind::PowerShell => &["pwsh.exe", "powershell.exe", "pwsh", "powershell"],
        ShellKind::Cmd => &["cmd.exe", "cmd"],
        ShellKind::Bash => &["bash.exe", "bash"],
        ShellKind::System | ShellKind::Custom(_) | ShellKind::Wsl(_) => &[],
    }
}
pub(crate) fn shell_command(kind: ShellKind) -> PathBuf {
    if kind == ShellKind::System {
        return crate::infrastructure::terminal::profiles::builtin(kind)
            .and_then(|p| p.program)
            .unwrap_or_else(|| "/bin/sh".into());
    }
    #[cfg(windows)]
    if kind == ShellKind::Bash
        && let Some(git_bash) = find_git_bash()
    {
        return git_bash;
    }
    candidates(kind)
        .iter()
        .find_map(|candidate| find_executable(candidate))
        .unwrap_or_else(|| PathBuf::from(candidates(kind).first().copied().unwrap_or("")))
}

fn find_executable(command: &str) -> Option<PathBuf> {
    let command_path = Path::new(command);
    if command_path.components().count() > 1 && command_path.is_file() {
        return Some(command_path.to_owned());
    }
    env::split_paths(&env::var_os("PATH")?).find_map(|directory| {
        let candidate = directory.join(command);
        candidate.is_file().then_some(candidate)
    })
}

#[cfg(windows)]
pub(crate) fn find_git_bash() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(program_files) = env::var_os("ProgramFiles") {
        candidates.push(PathBuf::from(program_files).join("Git\\bin\\bash.exe"));
    }
    if let Some(program_files_x86) = env::var_os("ProgramFiles(x86)") {
        candidates.push(PathBuf::from(program_files_x86).join("Git\\bin\\bash.exe"));
    }
    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        candidates.push(PathBuf::from(local_app_data).join("Programs\\Git\\bin\\bash.exe"));
    }
    if let Some(git) = find_executable("git.exe")
        && let Some(root) = git.parent().and_then(Path::parent)
    {
        candidates.push(root.join("bin\\bash.exe"));
    }
    candidates.into_iter().find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::terminal::profiles::effective_default;

    #[test]
    fn unavailable_custom_default_falls_back_without_dropping_its_profile() {
        let profile = ShellProfile {
            id: 42,
            name: "Missing shell".into(),
            program: PathBuf::from("/missing-loog-shell"),
            args: vec![],
        };
        let missing = crate::infrastructure::terminal::profiles::custom(&profile);
        assert!(!missing.available());
        let available = ShellOption {
            kind: ShellKind::System,
            name: "System default".into(),
            program: Some("/bin/sh".into()),
            args: vec![],
        };
        assert_eq!(
            effective_default(ShellKind::Custom(42), &[missing.clone(), available]),
            Some(ShellKind::System)
        );
        assert_eq!(effective_default(ShellKind::Custom(42), &[missing]), None);
    }
}
