//! Real filesystem and platform shell adapters for the Explorer ports.
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::features::explorer::ports::{DirectoryPort, FileOpsPort};
use crate::features::explorer::tree::sort_entries;
use crate::features::explorer::{DirEntry, ExplorerTargetKind};

pub(crate) struct ExplorerFilesystem;

/// Read one directory listing from disk.
pub(crate) fn read_directory(directory: &Path) -> Arc<Vec<DirEntry>> {
    ExplorerFilesystem.read_directory(directory)
}

impl DirectoryPort for ExplorerFilesystem {
    fn read_directory(&self, directory: &Path) -> Arc<Vec<DirEntry>> {
        let parent: Arc<Path> = Arc::from(directory);
        let mut entries = match fs::read_dir(directory) {
            Ok(iter) => iter
                .flatten()
                .map(|entry| {
                    DirEntry::new(
                        &parent,
                        entry.file_name().to_string_lossy().into_owned(),
                        entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false),
                    )
                })
                .collect::<Vec<_>>(),
            Err(_) => Vec::new(),
        };
        sort_entries(&mut entries);
        Arc::new(entries)
    }

    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
        fs::canonicalize(path).ok()
    }
}

impl FileOpsPort for ExplorerFilesystem {
    fn create_file(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map(|_| ())
    }

    fn create_folder(&self, path: &Path) -> std::io::Result<()> {
        fs::create_dir_all(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        fs::rename(from, to)
    }

    fn move_to_trash(&self, path: &Path) -> Result<bool, String> {
        move_to_trash(path)
    }

    fn set_file_clipboard(&self, paths: &[&Path], cut: bool) -> Result<(), String> {
        set_file_clipboard(paths, cut)
    }

    fn reveal(&self, path: &Path, kind: ExplorerTargetKind) -> std::io::Result<()> {
        reveal_in_file_manager(path, kind)
    }
}

#[cfg(target_os = "windows")]
fn move_to_trash(path: &Path) -> Result<bool, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::{
        FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_WANTNUKEWARNING, SHFILEOPSTRUCTW,
        SHFileOperationW,
    };

    let path = shell_path(path);
    let mut from = std::ffi::OsStr::new(&path)
        .encode_wide()
        .collect::<Vec<_>>();
    from.extend([0, 0]);
    let mut operation = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: from.as_ptr(),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_WANTNUKEWARNING) as u16,
        ..Default::default()
    };
    // FOF_ALLOWUNDO asks the shell to use the Recycle Bin; WANTNUKEWARNING
    // preserves a shell warning if recycling is unavailable.
    let result = unsafe { SHFileOperationW(&mut operation) };
    if result != 0 {
        return Err(format!(
            "Could not move the item to the Recycle Bin (shell error {result})."
        ));
    }
    Ok(operation.fAnyOperationsAborted == 0)
}

#[cfg(target_os = "macos")]
fn move_to_trash(path: &Path) -> Result<bool, String> {
    let status = std::process::Command::new("osascript")
        .args([
            "-e",
            "on run argv",
            "-e",
            "tell application \"Finder\" to delete POSIX file (item 1 of argv)",
            "-e",
            "end run",
            "--",
        ])
        .arg(path)
        .status()
        .map_err(|error| format!("Could not open Trash: {error}"))?;
    if status.success() {
        Ok(true)
    } else {
        Err("Could not move the item to Trash.".to_owned())
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn move_to_trash(path: &Path) -> Result<bool, String> {
    let status = std::process::Command::new("gio")
        .args(["trash", "--"])
        .arg(path)
        .status()
        .map_err(|error| format!("Could not open Trash: {error}"))?;
    if status.success() {
        Ok(true)
    } else {
        Err("Could not move the item to Trash.".to_owned())
    }
}

#[cfg(target_os = "windows")]
fn set_file_clipboard(paths: &[&Path], cut: bool) -> Result<(), String> {
    let paths: Vec<String> = paths.iter().map(|path| shell_path(path)).collect();
    let list: Vec<&str> = paths.iter().map(String::as_str).collect();
    let _clipboard = clipboard_win::Clipboard::new_attempts(10)
        .map_err(|error| format!("Could not open the clipboard: {error}"))?;
    clipboard_win::empty().map_err(|error| format!("Could not clear the clipboard: {error}"))?;
    clipboard_win::raw::set_file_list(&list)
        .map_err(|error| format!("Could not copy the folder: {error}"))?;

    let format = clipboard_win::register_format("Preferred DropEffect")
        .ok_or_else(|| "Could not register the file clipboard format.".to_owned())?;
    let effect = if cut { 2_u32 } else { 1_u32 };
    clipboard_win::raw::set_without_clear(format.get(), &effect.to_le_bytes())
        .map_err(|error| format!("Could not set the clipboard operation: {error}"))
}

#[cfg(not(target_os = "windows"))]
fn set_file_clipboard(_paths: &[&Path], _cut: bool) -> Result<(), String> {
    Err("Copying file-system items is not supported on this platform yet.".to_owned())
}

#[cfg(target_os = "windows")]
fn reveal_in_file_manager(path: &Path, kind: ExplorerTargetKind) -> std::io::Result<()> {
    let mut command = std::process::Command::new("explorer.exe");
    if kind == ExplorerTargetKind::File {
        command.arg(format!("/select,{}", shell_path(path)));
    } else {
        command.arg(shell_path(path));
    }
    command.spawn().map(|_| ())
}

#[cfg(target_os = "macos")]
fn reveal_in_file_manager(path: &Path, kind: ExplorerTargetKind) -> std::io::Result<()> {
    let mut command = std::process::Command::new("open");
    if kind == ExplorerTargetKind::File {
        command.arg("-R");
    }
    command.arg(path).spawn().map(|_| ())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn reveal_in_file_manager(path: &Path, kind: ExplorerTargetKind) -> std::io::Result<()> {
    let target = if kind == ExplorerTargetKind::File {
        path.parent().unwrap_or(path)
    } else {
        path
    };
    std::process::Command::new("xdg-open")
        .arg(target)
        .spawn()
        .map(|_| ())
}

/// The shell does not accept verbatim (`\\?\`) paths.
#[cfg(target_os = "windows")]
fn shell_path(path: &Path) -> String {
    let value = path.to_string_lossy();
    if let Some(unc) = value.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else if let Some(local) = value.strip_prefix(r"\\?\") {
        local.to_owned()
    } else {
        value.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    fn temp_directory(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "loom-explorer-fs-{label}-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn directory_read_sorts_folders_before_files() {
        let root = temp_directory("sort");
        fs::write(root.join("a.txt"), "a").unwrap();
        fs::create_dir(root.join("z-dir")).unwrap();

        let entries = ExplorerFilesystem.read_directory(&root);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["z-dir", "a.txt"]
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unreadable_directory_lists_as_empty() {
        let root = temp_directory("missing");
        fs::remove_dir_all(&root).unwrap();
        assert!(ExplorerFilesystem.read_directory(&root).is_empty());
    }

    #[test]
    fn create_file_creates_parents_and_refuses_to_overwrite() {
        let root = temp_directory("create");
        let file = root.join("nested").join("new.txt");

        ExplorerFilesystem.create_file(&file).unwrap();
        assert!(file.is_file());
        assert!(ExplorerFilesystem.create_file(&file).is_err());

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn shell_paths_hide_windows_verbatim_prefixes() {
        assert_eq!(
            shell_path(Path::new(r"\\?\D:\Documents\project")),
            r"D:\Documents\project"
        );
        assert_eq!(
            shell_path(Path::new(r"\\?\UNC\server\share\project")),
            r"\\server\share\project"
        );
    }
}
