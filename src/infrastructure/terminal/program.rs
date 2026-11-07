//! Native process names, debounced so brief commands do not churn tab labels.

use crate::features::terminal::program::ProgramCandidate;
#[cfg(not(windows))]
use crate::features::terminal::program::program_name;
#[cfg(windows)]
use crate::features::terminal::program::{descendant_program, session_program};

pub struct ProgramProbe {
    #[cfg(windows)]
    processes: Vec<(u32, u32, String)>,
}

impl ProgramProbe {
    pub fn new() -> Self {
        Self {
            #[cfg(windows)]
            processes: windows_processes(),
        }
    }

    pub fn candidate(&self, shell_pid: u32, foreground: Option<u32>) -> Option<ProgramCandidate> {
        #[cfg(windows)]
        {
            let _ = foreground;
            descendant_program(shell_pid, &self.processes)
        }
        #[cfg(not(windows))]
        {
            foreground_program(shell_pid, foreground)
        }
    }

    #[cfg(windows)]
    pub fn session_candidate(
        &self,
        members: &[u32],
        baseline: &[u32],
        preferred: Option<u32>,
    ) -> Option<ProgramCandidate> {
        session_program(members, baseline, preferred, &self.processes)
    }
}

/// Observe console membership in a separate process, so the editor's console
/// and arbitrary applications' process creation rules remain untouched.
#[cfg(windows)]
pub struct SessionConsole {
    child: std::process::Child,
    members: std::sync::Arc<std::sync::Mutex<Option<Vec<u32>>>>,
    pub baseline: Vec<u32>,
}

#[cfg(windows)]
impl SessionConsole {
    #[cfg(test)]
    pub(crate) fn test_pid(&self) -> u32 {
        self.child.id()
    }

    pub fn new(root: u32) -> Option<Self> {
        use std::io::{BufRead, BufReader};
        use std::os::windows::process::CommandExt;
        use std::process::Stdio;
        use std::sync::{Arc, Mutex, mpsc};
        use std::time::Duration;
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

        let executable = console_observer_path()?;
        let mut child = std::process::Command::new(executable)
            .arg(root.to_string())
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let output = child.stdout.take()?;
        let members = Arc::new(Mutex::new(None));
        let observed = members.clone();
        let (ready, initial) = mpsc::sync_channel(2);
        let mut observer = Self {
            child,
            members,
            baseline: Vec::new(),
        };
        std::thread::Builder::new()
            .name("loog-terminal-members".into())
            .spawn(move || {
                for (index, line) in BufReader::new(output)
                    .lines()
                    .map_while(Result::ok)
                    .enumerate()
                {
                    let ids = line
                        .split_whitespace()
                        .map(str::parse::<u32>)
                        .collect::<Result<Vec<_>, _>>();
                    let Ok(ids) = ids else {
                        break;
                    };
                    *observed.lock().unwrap() = Some(ids.clone());
                    if index < 2 {
                        let _ = ready.send(ids);
                    }
                }
                *observed.lock().unwrap() = None;
            })
            .ok()?;
        // Hold user input until a second snapshot includes the terminal's startup
        // processes. This identifies terminal infrastructure without name lists.
        let first = initial.recv_timeout(Duration::from_secs(2)).ok()?;
        let mut baseline = initial.recv_timeout(Duration::from_secs(2)).ok()?;
        baseline.extend(first);
        baseline.sort_unstable();
        baseline.dedup();
        observer.baseline = baseline;
        Some(observer)
    }

    pub fn members(&self) -> Option<Vec<u32>> {
        self.members.lock().unwrap().clone()
    }
}

#[cfg(windows)]
impl Drop for SessionConsole {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(windows)]
fn console_observer_path() -> Option<std::path::PathBuf> {
    use std::sync::OnceLock;
    static PATH: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
    PATH.get_or_init(|| {
        let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/loog-console-probe.exe"));
        let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
        let version = digest.as_ref()[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let directory = std::env::temp_dir().join("loog-terminal-observers");
        std::fs::create_dir_all(&directory).ok()?;
        let path = directory.join(format!("console-{version}.exe"));
        if std::fs::read(&path).ok().as_deref() == Some(bytes.as_slice()) {
            return Some(path);
        }
        let staging = directory.join(format!("console-{version}-{}.tmp", std::process::id()));
        std::fs::write(&staging, bytes).ok()?;
        if std::fs::rename(&staging, &path).is_err() {
            let _ = std::fs::remove_file(&staging);
            if std::fs::read(&path).ok().as_deref() != Some(bytes.as_slice()) {
                return None;
            }
        }
        Some(path)
    })
    .clone()
}

#[cfg(windows)]
fn windows_processes() -> Vec<(u32, u32, String)> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    // ToolHelp owns a snapshot handle; close it even when no matching child exists.
    let processes = unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Vec::new();
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut processes = Vec::new();
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        while more {
            let length = entry
                .szExeFile
                .iter()
                .position(|ch| *ch == 0)
                .unwrap_or(entry.szExeFile.len());
            processes.push((
                entry.th32ProcessID,
                entry.th32ParentProcessID,
                String::from_utf16_lossy(&entry.szExeFile[..length]),
            ));
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
        processes
    };
    processes
}

#[cfg(not(windows))]
fn foreground_program(shell_pid: u32, foreground: Option<u32>) -> Option<ProgramCandidate> {
    let pid = foreground.filter(|pid| *pid != 0 && *pid != shell_pid)?;
    #[cfg(target_os = "linux")]
    let executable = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    #[cfg(not(target_os = "linux"))]
    let executable = {
        let output = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "comm="])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8(output.stdout).ok()?
    };
    Some(ProgramCandidate {
        pid,
        name: program_name(&executable)?,
    })
}
