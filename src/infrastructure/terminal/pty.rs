//! Platform PTY process adapters, retaining the existing launch and quoting policy.
#[cfg(windows)]
use crate::features::terminal::ShellKind;
use crate::features::terminal::TerminalSize;
#[cfg(not(windows))]
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
#[cfg(windows)]
use std::env;
use std::io::{Read, Write};
use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;
#[cfg(windows)]
use std::sync::Arc;

pub(super) struct RunningProcess {
    pub(super) _integration: crate::infrastructure::terminal::integration::ShellIntegration,
    pub(super) pid: u32,
    #[cfg(windows)]
    pub(super) console: Option<Arc<crate::infrastructure::terminal::program::SessionConsole>>,
    #[cfg(windows)]
    pub(super) process: conpty::Process,
    #[cfg(not(windows))]
    pub(super) master: Box<dyn MasterPty + Send>,
    pub(super) writer: Box<dyn Write + Send>,
    #[cfg(not(windows))]
    pub(super) killer: Box<dyn ChildKiller + Send + Sync>,
    pub(super) size: TerminalSize,
}

/// Send terminal input without waiting for the application to consume it.
pub(super) fn write_input(writer: &mut dyn Write, bytes: &[u8]) -> std::io::Result<()> {
    writer.write_all(bytes)?;
    // ConPTY input is unbuffered. Its flush calls FlushFileBuffers, which waits
    // for the pipe reader to consume input and can hang the UI indefinitely.
    #[cfg(not(windows))]
    writer.flush()?;
    Ok(())
}

pub(super) struct SpawnedShell {
    pub(super) process: RunningProcess,
    pub(super) reader: Box<dyn Read + Send>,
    #[cfg(not(windows))]
    pub(super) child: Box<dyn portable_pty::Child + Send + Sync>,
}

#[cfg(windows)]
fn spawn_shell(
    shell: ShellKind,
    cwd: Option<&Path>,
    size: TerminalSize,
) -> Result<SpawnedShell, String> {
    let program = crate::infrastructure::terminal::profiles::shell_command(shell);
    spawn_shell_program(shell, cwd, size, &program)
}

#[cfg(windows)]
pub(super) fn spawn_shell_option(
    option: &crate::features::terminal::profiles::ShellOption,
    cwd: Option<&Path>,
    size: TerminalSize,
) -> Result<SpawnedShell, String> {
    spawn_shell_program_args(
        option.kind,
        cwd,
        size,
        option
            .program
            .as_deref()
            .ok_or("Shell program is unavailable")?,
        &option.args,
    )
}

#[cfg(windows)]
pub(super) fn spawn_shell_program(
    shell: ShellKind,
    cwd: Option<&Path>,
    size: TerminalSize,
    program: &Path,
) -> Result<SpawnedShell, String> {
    spawn_shell_program_args(shell, cwd, size, program, &[])
}

#[cfg(windows)]
fn spawn_shell_program_args(
    _shell: ShellKind,
    cwd: Option<&Path>,
    size: TerminalSize,
    program: &Path,
    args: &[String],
) -> Result<SpawnedShell, String> {
    // conpty 0.7 concatenates Command arguments without quoting. Quote paths
    // explicitly, including executables installed under Program Files.
    let mut executable = std::ffi::OsString::from("\"");
    executable.push(program.as_os_str());
    executable.push("\"");
    let mut command = std::process::Command::new(executable);
    let integration = configure_std_command(&mut command, program, args, cwd)?;

    let mut options = conpty::ProcessOptions::default();
    options.set_console_size(Some((
        size.cols.min(i16::MAX as u16) as i16,
        size.rows.min(i16::MAX as u16) as i16,
    )));
    let mut process = options
        .spawn(command)
        .map_err(|error| format!("{error} ({})", program.display()))?;
    let reader = process.output().map_err(|error| error.to_string())?;
    let writer = process.input().map_err(|error| error.to_string())?;
    let console =
        crate::infrastructure::terminal::program::SessionConsole::new(process.pid()).map(Arc::new);
    Ok(SpawnedShell {
        process: RunningProcess {
            _integration: integration,
            pid: process.pid(),
            console,
            process,
            writer: Box::new(writer),
            size,
        },
        reader: Box::new(reader),
    })
}

#[cfg(windows)]
fn configure_std_command(
    command: &mut std::process::Command,
    program: &Path,
    args: &[String],
    cwd: Option<&Path>,
) -> Result<crate::infrastructure::terminal::integration::ShellIntegration, String> {
    let cwd = cwd.map(shell_friendly_path);
    let integration = crate::infrastructure::terminal::integration::ShellIntegration::for_launch(
        program,
        args,
        cwd.as_deref(),
    )
    .map_err(|error| format!("shell integration: {error}"))?;
    for arg in integration.launch_args(args) {
        command.arg(quote_windows_argument(&arg));
    }
    if let Some(cwd) = &cwd {
        command.current_dir(cwd);
    }
    // conpty builds an explicit environment block when any override exists,
    // so copy the inherited environment before adding terminal capabilities.
    command.envs(env::vars_os());
    command.envs(integration.env.iter().cloned());
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command.env("TERM_PROGRAM", "loog");
    Ok(integration)
}

#[cfg(windows)]
fn quote_windows_argument(arg: &std::ffi::OsStr) -> std::ffi::OsString {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    // WSL parses switches from the raw command line and treats a quoted
    // switch as a Linux command. Only quote arguments that need it.
    if !arg.is_empty()
        && !arg
            .encode_wide()
            .any(|unit| matches!(unit, 9..=13 | 32 | 34))
    {
        return arg.to_os_string();
    }
    let mut quoted = vec![34u16];
    let mut slashes = 0;
    for unit in arg.encode_wide() {
        if unit == 92 {
            slashes += 1;
            continue;
        }
        quoted.extend(std::iter::repeat_n(
            92,
            if unit == 34 { slashes * 2 + 1 } else { slashes },
        ));
        quoted.push(unit);
        slashes = 0;
    }
    quoted.extend(std::iter::repeat_n(92, slashes * 2));
    quoted.push(34);
    std::ffi::OsString::from_wide(&quoted)
}

/// `fs::canonicalize` yields verbatim paths (`\\?\D:\dir`). Shells keep that
/// form as their location, so PowerShell would prompt with
/// `Microsoft.PowerShell.Core\FileSystem::\\?\D:\dir`; drop the prefix.
#[cfg(windows)]
pub(crate) fn shell_friendly_path(path: &Path) -> PathBuf {
    let Some(value) = path.to_str() else {
        return path.to_path_buf();
    };
    if let Some(unc) = value.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{unc}"))
    } else if let Some(local) = value.strip_prefix(r"\\?\")
        && local.as_bytes().get(1) == Some(&b':')
    {
        PathBuf::from(local)
    } else {
        path.to_path_buf()
    }
}

#[cfg(not(windows))]
pub(super) fn spawn_shell_option(
    option: &crate::features::terminal::profiles::ShellOption,
    cwd: Option<&Path>,
    size: TerminalSize,
) -> Result<SpawnedShell, String> {
    let pty = native_pty_system();
    let pty_size = PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: size.pixel_width,
        pixel_height: size.pixel_height,
    };
    let pair = pty.openpty(pty_size).map_err(|error| error.to_string())?;
    let program = option
        .program
        .as_deref()
        .ok_or("Shell program is unavailable")?;
    let mut command = CommandBuilder::new(program);
    let integration = crate::infrastructure::terminal::integration::ShellIntegration::for_launch(
        program,
        &option.args,
        cwd,
    )
    .map_err(|error| format!("shell integration: {error}"))?;
    command.args(integration.launch_args(&option.args));
    for (key, value) in &integration.env {
        command.env(key, value);
    }
    if let Some(cwd) = cwd {
        command.cwd(cwd.as_os_str());
    }
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command.env("TERM_PROGRAM", "loog");

    let child = pair
        .slave
        .spawn_command(command)
        .map_err(|error| format!("{} ({})", error, program.display()))?;
    drop(pair.slave);
    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|error| error.to_string())?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|error| error.to_string())?;
    let killer = child.clone_killer();
    Ok(SpawnedShell {
        process: RunningProcess {
            _integration: integration,
            pid: child.process_id().unwrap_or(0),
            master: pair.master,
            writer,
            killer,
            size,
        },
        reader,
        child,
    })
}

#[cfg(windows)]
pub(super) fn resize_process(
    process: &mut RunningProcess,
    size: TerminalSize,
) -> Result<(), String> {
    process
        .process
        .resize(
            size.cols.min(i16::MAX as u16) as i16,
            size.rows.min(i16::MAX as u16) as i16,
        )
        .map_err(|error| error.to_string())
}

#[cfg(not(windows))]
pub(super) fn resize_process(
    process: &mut RunningProcess,
    size: TerminalSize,
) -> Result<(), String> {
    process
        .master
        .resize(PtySize {
            rows: size.rows,
            cols: size.cols,
            pixel_width: size.pixel_width,
            pixel_height: size.pixel_height,
        })
        .map_err(|error| error.to_string())
}

#[cfg(windows)]
pub(super) fn terminate_process(process: &mut RunningProcess) {
    let _ = process.process.exit(1);
}

#[cfg(not(windows))]
pub(super) fn terminate_process(process: &mut RunningProcess) {
    let _ = process.killer.kill();
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::ffi::{OsStr, OsString};
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    #[test]
    fn terminal_input_handles_short_writes_without_flushing_the_pipe() {
        #[derive(Default)]
        struct UnbufferedInput {
            bytes: Vec<u8>,
            interrupted: bool,
        }
        impl Write for UnbufferedInput {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if !self.interrupted {
                    self.interrupted = true;
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
                let count = bytes.len().min(3);
                self.bytes.extend_from_slice(&bytes[..count]);
                Ok(count)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                panic!("ConPTY flush must not wait for its input reader");
            }
        }

        let mut writer = UnbufferedInput::default();
        // Keyboard input, mouse reports and bracketed paste share this path.
        for bytes in [
            b"hello\r".as_slice(),
            b"\x1b[<64;14;8M".as_slice(),
            "\u{1b}[200~\u{4e2d}\u{6587}\r\npaste\u{1b}[201~".as_bytes(),
        ] {
            write_input(&mut writer, bytes).unwrap();
        }
        assert_eq!(
            writer.bytes,
            "hello\r\u{1b}[<64;14;8M\u{1b}[200~\u{4e2d}\u{6587}\r\npaste\u{1b}[201~".as_bytes()
        );
    }

    #[test]
    fn terminal_input_preserves_pipe_write_errors() {
        struct BrokenInput;
        impl Write for BrokenInput {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                panic!("a failed write must not flush");
            }
        }
        assert_eq!(
            write_input(&mut BrokenInput, b"input").unwrap_err().kind(),
            std::io::ErrorKind::BrokenPipe
        );
    }

    #[test]
    fn wsl_switches_remain_unquoted_in_the_conpty_command_line() {
        let mut command = std::process::Command::new("wsl.exe");
        let _integration = configure_std_command(
            &mut command,
            Path::new("wsl.exe"),
            &["-d".into(), "Debian".into(), "--cd".into(), "~".into()],
            None,
        )
        .unwrap();
        // conpty joins these strings directly, without further escaping.
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["-d", "Debian", "--cd", "~"].map(OsStr::new)
        );
    }

    #[test]
    fn windows_arguments_round_trip_through_the_system_parser() {
        let args = [
            "",
            "-d",
            "Debian",
            "hello world",
            "with\ttab",
            "with\"quote",
            r"C:\Program Files\shell\",
            r"C:\plain\",
            r#"backslash\"quote"#,
            "中文目录",
        ];
        let mut command_line = OsString::from("program.exe");
        for arg in args {
            command_line.push(" ");
            command_line.push(quote_windows_argument(OsStr::new(arg)));
        }
        let command_line = command_line.encode_wide().chain([0]).collect::<Vec<_>>();
        let mut count = 0;
        unsafe {
            let argv = windows_sys::Win32::UI::Shell::CommandLineToArgvW(
                command_line.as_ptr(),
                &mut count,
            );
            assert!(!argv.is_null());
            let parsed = std::slice::from_raw_parts(argv, count as usize)
                .iter()
                .skip(1)
                .map(|&value| {
                    let mut len = 0;
                    while *value.add(len) != 0 {
                        len += 1;
                    }
                    OsString::from_wide(std::slice::from_raw_parts(value, len))
                })
                .collect::<Vec<_>>();
            windows_sys::Win32::Foundation::LocalFree(argv.cast());
            assert_eq!(parsed, args.map(OsString::from));
        }
    }
}
