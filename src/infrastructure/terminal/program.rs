//! Native process names, debounced so brief commands do not churn tab labels.

use crate::features::terminal::program::ProgramCandidate;
#[cfg(any(windows, test))]
use crate::features::terminal::program::descendant_program;
#[cfg(not(windows))]
use crate::features::terminal::program::program_name;

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

    /// A sustained program PID on Windows, or the foreground PID on Unix.
    pub fn candidate(&self, shell_pid: u32, active_pid: Option<u32>) -> Option<ProgramCandidate> {
        #[cfg(windows)]
        {
            let candidate = descendant_program(shell_pid, active_pid, &self.processes)?;
            // Names are best effort. A shell wrapper or WSL transport is not a
            // reliable application name, and never determines command activity.
            let shell = self.processes.iter().find(|entry| entry.0 == shell_pid)?;
            if crate::features::terminal::program::program_name(&shell.2)
                .is_some_and(|name| name.eq_ignore_ascii_case(&candidate.name))
            {
                return None;
            }
            let mut pid = candidate.pid;
            let mut visited = Vec::new();
            loop {
                if visited.contains(&pid) {
                    return None;
                }
                visited.push(pid);
                let entry = self.processes.iter().find(|entry| entry.0 == pid)?;
                if entry.2.eq_ignore_ascii_case("wsl.exe") {
                    return None;
                }
                if pid == shell_pid {
                    return Some(candidate);
                }
                pid = entry.1;
            }
        }
        #[cfg(not(windows))]
        {
            foreground_program(shell_pid, active_pid)
        }
    }
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
