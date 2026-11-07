//! Shell startup integration and working-directory validation.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Validate at process start (not on the UI thread for every output frame).
pub fn starting_directory(requested: Option<&Path>, fallback: Option<&Path>) -> Option<PathBuf> {
    requested
        .filter(|path| path.is_absolute() && path.is_dir())
        .map(Path::to_path_buf)
        .or_else(|| {
            fallback
                .filter(|path| path.is_absolute() && path.is_dir())
                .map(Path::to_path_buf)
        })
        .or_else(|| std::env::current_dir().ok())
}

pub struct ShellIntegration {
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
    script: Option<PathBuf>,
}

impl ShellIntegration {
    pub fn for_program(program: &Path, cwd: Option<&Path>) -> std::io::Result<Self> {
        Self::for_launch(program, &[], cwd)
    }

    pub fn for_launch(
        program: &Path,
        args: &[String],
        cwd: Option<&Path>,
    ) -> std::io::Result<Self> {
        let mut integration = Self {
            args: Vec::new(),
            env: Vec::new(),
            script: None,
        };
        if let Some(cwd) = cwd {
            integration
                .env
                .push(("LOOG_TERMINAL_CWD".into(), cwd.as_os_str().to_owned()));
        }
        let family = program
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        // Explicit commands, scripts, and startup-file options must retain
        // their own behavior instead of receiving another startup command.
        let owns_startup = args.iter().any(|arg| {
            let lower = arg.to_ascii_lowercase();
            match family.as_str() {
                "pwsh" | "powershell" => {
                    lower.len() > 1
                        && ["-command", "-encodedcommand", "-file", "-commandwithargs"]
                            .iter()
                            .any(|flag| flag.starts_with(&lower))
                        || lower == "-ec"
                        || lower.ends_with(".ps1")
                }
                "bash" => {
                    matches!(
                        arg.as_str(),
                        "--login" | "--norc" | "--noprofile" | "--rcfile" | "--init-file"
                    ) || arg.starts_with("--rcfile=")
                        || arg.starts_with("--init-file=")
                        || (arg.starts_with('-')
                            && !arg.starts_with("--")
                            && arg[1..].contains(['c', 'l']))
                        || !arg.starts_with('-')
                }
                "zsh" | "fish" | "sh" | "dash" | "ksh" => {
                    !arg.starts_with('-')
                        || (arg.starts_with('-')
                            && !arg.starts_with("--")
                            && arg[1..].contains('c'))
                        || arg == "--command"
                        || arg.starts_with("--command=")
                        || matches!(arg.as_str(), "--no-config" | "--no-rcs" | "-f")
                }
                _ => false,
            }
        });
        if owns_startup {
            return Ok(integration);
        }
        match family.as_str() {
            "pwsh" | "powershell" => {
                use base64::Engine;
                let bytes = POWERSHELL
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>();
                integration.args = ["-NoLogo", "-NoExit", "-EncodedCommand"]
                    .map(OsString::from)
                    .to_vec();
                integration.args.push(
                    base64::engine::general_purpose::STANDARD
                        .encode(bytes)
                        .into(),
                );
            }
            "cmd" => {
                let prompt = std::env::var("PROMPT").unwrap_or_else(|_| "$P$G".into());
                integration
                    .env
                    .push(("PROMPT".into(), format!(r"$E]9;9;$P$E\{prompt}").into()));
                // CMD starts in the process cwd; no command interpolation is needed.
            }
            "bash" => {
                static NEXT: AtomicU64 = AtomicU64::new(1);
                let path = std::env::temp_dir().join(format!(
                    "loog-bash-{}-{}.sh",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                // create_new avoids following an existing path in a shared temp directory.
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut file = options.open(&path)?;
                integration.script = Some(path.clone());
                use std::io::Write;
                file.write_all(BASH.as_bytes())?;
                let path = path.to_string_lossy().replace('\\', "/");
                integration.args = vec!["--rcfile".into(), path.into(), "-i".into()];
            }
            "zsh" => {
                static NEXT: AtomicU64 = AtomicU64::new(1);
                let dir = std::env::temp_dir().join(format!(
                    "loog-zsh-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                let mut builder = std::fs::DirBuilder::new();
                builder.recursive(false);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt;
                    builder.mode(0o700);
                }
                builder.create(&dir)?;
                integration.script = Some(dir.clone());
                std::fs::write(dir.join(".zshenv"), ZSH_ENV)?;
                std::fs::write(
                    dir.join(".zprofile"),
                    "ZDOTDIR=$LOOG_ZDOTDIR\n[[ -r $ZDOTDIR/.zprofile ]] && source $ZDOTDIR/.zprofile\nLOOG_ZDOTDIR=${ZDOTDIR:-$HOME}\nZDOTDIR=$LOOG_INTEGRATION_DIR\n",
                )?;
                std::fs::write(dir.join(".zshrc"), ZSH_RC)?;
                integration.env.push((
                    "LOOG_ZDOTDIR".into(),
                    std::env::var_os("ZDOTDIR")
                        .or_else(|| std::env::var_os("HOME"))
                        .unwrap_or_default(),
                ));
                integration
                    .env
                    .push(("LOOG_INTEGRATION_DIR".into(), dir.as_os_str().to_owned()));
                integration
                    .env
                    .push(("ZDOTDIR".into(), dir.into_os_string()));
                integration.args = vec!["-l".into(), "-i".into()];
            }
            "fish" => {
                integration.args = vec!["-i".into(), "-C".into(), "function __loog_report_cwd --on-event fish_prompt; printf '\\e]9;9;%s\\a' \"$PWD\"; end".into()];
            }
            "sh" | "dash" | "ksh" => {
                static NEXT: AtomicU64 = AtomicU64::new(1);
                let path = std::env::temp_dir().join(format!(
                    "loog-sh-{}-{}.sh",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                use std::io::Write;
                let mut file = options.open(&path)?;
                integration.script = Some(path.clone());
                file.write_all(SH.as_bytes())?;
                if let Some(original) = std::env::var_os("ENV") {
                    integration.env.push(("LOOG_ORIGINAL_ENV".into(), original));
                }
                integration.env.push(("ENV".into(), path.into_os_string()));
                integration.args = vec!["-i".into()];
            }
            _ => {}
        }
        Ok(integration)
    }

    pub fn launch_args(&self, args: &[String]) -> Vec<OsString> {
        // Bash requires long options before short options. Keep its rcfile
        // option ahead of user flags and the interactive flag at the end.
        if self.args.first().is_some_and(|arg| arg == "--rcfile") {
            self.args[..2]
                .iter()
                .cloned()
                .chain(args.iter().map(OsString::from))
                .chain(self.args[2..].iter().cloned())
                .collect()
        } else {
            args.iter()
                .map(OsString::from)
                .chain(self.args.iter().cloned())
                .collect()
        }
    }
}

impl Drop for ShellIntegration {
    fn drop(&mut self) {
        if let Some(path) = &self.script {
            if path.is_dir() {
                let _ = std::fs::remove_dir_all(path);
            } else {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

const POWERSHELL: &str = r#"
if ($env:LOOG_TERMINAL_CWD) {
    Set-Location -LiteralPath $env:LOOG_TERMINAL_CWD
    Remove-Item Env:LOOG_TERMINAL_CWD
}
$global:__LoogOriginalPrompt = $function:prompt
function global:prompt {
    $loogPrompt = & $global:__LoogOriginalPrompt
    if ($PWD.Provider.Name -eq 'FileSystem') {
        $Host.UI.Write(([char]27).ToString() + ']9;9;"' + $PWD.ProviderPath + '"' + [char]7)
    }
    $loogPrompt
}
"#;

const ZSH_ENV: &str = r#"
ZDOTDIR=$LOOG_ZDOTDIR
[[ -r $LOOG_ZDOTDIR/.zshenv ]] && source $LOOG_ZDOTDIR/.zshenv
LOOG_ZDOTDIR=${ZDOTDIR:-$HOME}
ZDOTDIR=$LOOG_INTEGRATION_DIR
"#;

const ZSH_RC: &str = r#"
ZDOTDIR=$LOOG_ZDOTDIR
[[ -r $ZDOTDIR/.zshrc ]] && source $ZDOTDIR/.zshrc
function __loog_report_cwd() { printf '\033]9;9;%s\007' "$PWD"; }
precmd_functions=(${precmd_functions:#__loog_report_cwd} __loog_report_cwd)
unset LOOG_INTEGRATION_DIR LOOG_ZDOTDIR LOOG_TERMINAL_CWD
"#;

const SH: &str = r#"
if [ -n "$LOOG_ORIGINAL_ENV" ] && [ -r "$LOOG_ORIGINAL_ENV" ]; then . "$LOOG_ORIGINAL_ENV"; fi
# Bash running as sh (macOS /bin/sh) counts unmarked prompt escapes as visible
# text and splits long ones with carriage returns, so report outside PS1 there.
if [ -n "$BASH_VERSION" ]; then
    __loog_report_cwd() {
        __loog_status=$?
        printf '\033]9;9;%s\007' "$PWD"
        return "$__loog_status"
    }
    PROMPT_COMMAND="__loog_report_cwd${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
else
    PS1='$(printf "\033]9;9;%s\007" "$PWD")'"${PS1:-\$ }"
fi
unset LOOG_ORIGINAL_ENV LOOG_TERMINAL_CWD
"#;

// Read the same profiles as the former --login launch, then install the hook
// after custom prompt setup. No user profile files are modified.
const BASH: &str = r#"
if [ -r /etc/profile ]; then . /etc/profile; fi
for __loog_profile in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
    if [ -r "$__loog_profile" ]; then . "$__loog_profile"; break; fi
done
unset __loog_profile
if [ -n "$LOOG_TERMINAL_CWD" ]; then
    __loog_cwd=$LOOG_TERMINAL_CWD
    if command -v cygpath >/dev/null 2>&1; then __loog_cwd=$(cygpath -u "$__loog_cwd"); fi
    builtin cd -- "$__loog_cwd"
    unset __loog_cwd LOOG_TERMINAL_CWD
fi
__loog_report_cwd() {
    local __loog_status=$? __loog_path=$PWD
    if command -v cygpath >/dev/null 2>&1; then __loog_path=$(cygpath -w "$PWD"); fi
    builtin printf '\033]9;9;%s\007' "$__loog_path"
    return "$__loog_status"
}
if [[ $(declare -p PROMPT_COMMAND 2>/dev/null) == 'declare -a '* ]]; then
    PROMPT_COMMAND=(__loog_report_cwd "${PROMPT_COMMAND[@]}")
else
    PROMPT_COMMAND="__loog_report_cwd${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
fi
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_startup_commands_are_not_replaced_by_integration_commands() {
        for (program, args) in [
            ("bash", vec!["-lc", "printf test"]),
            ("bash", vec!["--login"]),
            ("bash", vec!["--norc"]),
            ("bash", vec!["--rcfile", "/custom/startup"]),
            ("pwsh", vec!["-NoProfile", "-Command", "Write-Output test"]),
            ("powershell", vec!["-enc", "custom-script"]),
            ("zsh", vec!["-c", "printf test"]),
            ("fish", vec!["--no-config"]),
            ("sh", vec!["startup.sh"]),
        ] {
            let args = args.into_iter().map(String::from).collect::<Vec<_>>();
            let integration =
                ShellIntegration::for_launch(Path::new(program), &args, None).unwrap();
            assert_eq!(
                integration.launch_args(&args),
                args.iter().map(OsString::from).collect::<Vec<_>>(),
                "{program}"
            );
            assert!(integration.script.is_none());
        }
        let args = vec!["-i".into()];
        let integration = ShellIntegration::for_launch(Path::new("bash"), &args, None).unwrap();
        let launched = integration.launch_args(&args);
        assert_eq!(launched[0], "--rcfile");
        assert_eq!(launched[2], "-i");
    }

    #[cfg(unix)]
    #[test]
    fn native_bash_and_sh_report_directories_after_cd() {
        use crate::features::terminal::directory::DirectoryReports;
        use portable_pty::{CommandBuilder, PtySize, native_pty_system};
        use std::io::{Read, Write};
        use std::time::Duration;
        for program in ["/bin/bash", "/bin/sh"] {
            if !Path::new(program).is_file() {
                continue;
            }
            let root = std::env::temp_dir().join(format!(
                "loog-unix-cwd-{}-{}",
                std::process::id(),
                Path::new(program).file_name().unwrap().to_string_lossy()
            ));
            let moved = root.join("中文 moved folder");
            std::fs::create_dir_all(&moved).unwrap();
            // macOS temporary directories can be reached through /var aliases,
            // while the shell reports their physical /private/var path.
            let root = root.canonicalize().unwrap();
            let moved = root.join("中文 moved folder");
            let args = if program.ends_with("bash") {
                vec!["-i".into()]
            } else {
                Vec::new()
            };
            let integration =
                ShellIntegration::for_launch(Path::new(program), &args, Some(&root)).unwrap();
            let pair = native_pty_system()
                .openpty(PtySize {
                    rows: 24,
                    cols: 80,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .unwrap();
            let mut command = CommandBuilder::new(program);
            command.args(integration.launch_args(&args));
            command.cwd(&root);
            command.env("HOME", &root);
            command.env("TERM", "xterm-256color");
            for (key, value) in &integration.env {
                command.env(key, value);
            }
            let mut child = pair.slave.spawn_command(command).unwrap();
            drop(pair.slave);
            let mut reader = pair.master.try_clone_reader().unwrap();
            let mut writer = pair.master.take_writer().unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let captured = output.clone();
            let reading = std::thread::spawn(move || {
                let mut reports = DirectoryReports::default();
                let mut bytes = [0; 4096];
                while let Ok(count) = reader.read(&mut bytes) {
                    if count == 0 {
                        break;
                    }
                    let mut captured = captured.lock().unwrap();
                    let remaining = (16 * 1024_usize).saturating_sub(captured.len());
                    captured.extend_from_slice(&bytes[..count.min(remaining)]);
                    drop(captured);
                    if let Some(path) = reports.process(&bytes[..count]) {
                        let _ = tx.send(path);
                    }
                }
            });
            let initial = rx.recv_timeout(Duration::from_secs(10));
            writeln!(writer, "cd '{}'", moved.display()).unwrap();
            writer.flush().unwrap();
            let changed = rx.recv_timeout(Duration::from_secs(10));
            let _ = child.kill();
            let _ = child.wait();
            drop(writer);
            drop(pair.master);
            let _ = reading.join();
            std::fs::remove_dir_all(&root).unwrap();
            let output = output.lock().unwrap();
            let output = String::from_utf8_lossy(&output);
            assert_eq!(
                initial.unwrap_or_else(|error| {
                    panic!("{program} initial directory: {error}; output: {output:?}")
                }),
                root,
                "{program} initial directory"
            );
            assert_eq!(
                changed.unwrap_or_else(|error| {
                    panic!("{program} changed directory: {error}; output: {output:?}")
                }),
                moved,
                "{program} changed directory"
            );
        }
    }

    #[test]
    fn invalid_or_missing_saved_directories_use_workspace_then_process_directory() {
        let workspace = std::env::temp_dir();
        let missing = workspace.join(format!("loog-missing-cwd-{}", std::process::id()));
        assert_eq!(
            starting_directory(Some(&workspace), Some(&missing)),
            Some(workspace.clone())
        );
        for requested in [None, Some(missing.as_path()), Some(Path::new("relative"))] {
            assert_eq!(
                starting_directory(requested, Some(&workspace)),
                Some(workspace.clone())
            );
        }
        assert_eq!(
            starting_directory(Some(&missing), None),
            std::env::current_dir().ok()
        );
    }
}
