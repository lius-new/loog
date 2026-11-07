//! Disposable GUI application used by launch-smoke.mjs instead of Loog.exe.
#![windows_subsystem = "windows"]

use std::ffi::c_void;
use std::os::windows::process::CommandExt;
use std::process::Command;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentProcess() -> *mut c_void;
    fn GetProcessMitigationPolicy(
        process: *mut c_void,
        policy: i32,
        buffer: *mut c_void,
        length: usize,
    ) -> i32;
}

fn main() {
    let mut flags = 0u32;
    // ProcessRedirectionTrustPolicy is 16. Query only; never change policy.
    let queried = unsafe {
        GetProcessMitigationPolicy(
            GetCurrentProcess(),
            16,
            (&mut flags as *mut u32).cast(),
            std::mem::size_of_val(&flags),
        )
    };
    let toolchain = std::path::Path::new(env!("LOOG_INSTALLER_PROBE_TOOLCHAIN"));
    let mut paths = vec![toolchain.to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let path = std::env::join_paths(paths).unwrap();
    let system = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap());
    let mut report = format!(
        "query={queried}\nredirection_guard={flags}\njunction_visible={}\n",
        toolchain.join("npm.cmd").is_file(),
    );
    for (name, program, args) in [
        (
            "cmd",
            system.join(r"System32\cmd.exe"),
            vec!["/d", "/c", "npm --version"],
        ),
        (
            "powershell",
            system.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"),
            vec![
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "npm --version",
            ],
        ),
    ] {
        let output = Command::new(program)
            .args(args)
            .env("PATH", &path)
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .output();
        let ok = output.as_ref().is_ok_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).trim() == "loog-junction-probe"
        });
        report.push_str(&format!("{name}_npm={ok}\n"));
        if !ok {
            report.push_str(&format!("{name}_output={output:?}\n"));
        }
    }
    std::fs::write(env!("LOOG_INSTALLER_PROBE_REPORT"), report).unwrap();
}
