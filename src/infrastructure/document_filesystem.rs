//! Bounded text IO. Manual decoding honors the choice; save conversion is strict.
use crate::features::encoding::{self, DecodedText, EncodingSettings, TextFormat};
use std::io::{self, Read, Write};
use std::path::Path;
use std::{
    fs,
    sync::atomic::{AtomicU64, Ordering},
};
pub(crate) const MAX_EDITABLE_FILE_BYTES: u64 = 64 * 1024 * 1024;
pub(crate) fn options(path: &Path) -> EncodingSettings {
    let user = super::settings_filesystem::load();
    let mut value = user.encoding.clone();
    // Nearest project settings take precedence; settings files themselves are UTF-8.
    if let Some(parent) = path.parent() {
        for ancestor in parent.ancestors() {
            let candidate = ancestor.join(".loog/settings.json");
            if candidate == path {
                continue;
            }
            if let Ok(text) = fs::read_to_string(candidate) {
                if let Ok((overrides, _)) = crate::features::settings::rules::parse_overrides(
                    &text,
                    crate::features::settings::rules::Scope::Project,
                ) {
                    value = crate::features::settings::rules::overlay(&user, &overrides).encoding;
                }
                break;
            }
        }
    }
    value
}
/// A file over this size is opened in place instead of being loaded: its text stays in the
/// file and only the lines shown are read, so it costs a few megabytes however large it is.
/// Smaller files are read whole, as every feature can then use their text.
pub(crate) const OPEN_IN_PLACE_BYTES: u64 = 16 * 1024 * 1024;
pub(crate) fn opens_in_place(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() > OPEN_IN_PLACE_BYTES)
}
/// Whether the file is over what can be loaded whole at all.
pub(crate) fn exceeds_load_limit(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() > MAX_EDITABLE_FILE_BYTES)
}
/// Open `path` as a text that stays in its file: only UTF-8 files qualify, and the whole file is
/// read once to index its lines and to check that.
pub(crate) fn open_file_backed(
    path: &Path,
) -> io::Result<(
    crate::infrastructure::file_text::FileText,
    crate::domain::document::DiskState,
)> {
    let text = super::file_text::FileText::open(path)?;
    let meta = fs::metadata(path)?;
    let state = crate::domain::document::DiskState {
        modified_time: meta.modified().ok(),
        size: meta.len(),
        content_hash: 0,
        format: TextFormat::new(
            encoding::Charset::Utf8,
            loog_editor_core::text::TextSource::has_bom(&text),
        ),
    };
    Ok((text, state))
}
/// What a file looks like on disk, without reading it: for a document whose text stays in its
/// file, which is compared by size and modification time instead of by content.
pub(crate) fn disk_state_of(
    path: &Path,
    format: TextFormat,
) -> io::Result<crate::domain::document::DiskState> {
    let meta = fs::metadata(path)?;
    Ok(crate::domain::document::DiskState {
        modified_time: meta.modified().ok(),
        size: meta.len(),
        content_hash: 0,
        format,
    })
}
/// As `open_file_backed`, for a file just written whose lines were learned while writing it, so
/// it is not read again. A file that is not the size written is opened the ordinary way.
pub(crate) fn open_written_file(
    path: &Path,
    index: loog_editor_core::text::FileIndex,
) -> io::Result<(
    crate::infrastructure::file_text::FileText,
    crate::domain::document::DiskState,
)> {
    let text = super::file_text::FileText::open_indexed(path, index)?;
    let meta = fs::metadata(path)?;
    let state = crate::domain::document::DiskState {
        modified_time: meta.modified().ok(),
        size: meta.len(),
        content_hash: 0,
        format: TextFormat::new(
            encoding::Charset::Utf8,
            loog_editor_core::text::TextSource::has_bom(&text),
        ),
    };
    Ok((text, state))
}
pub(crate) fn read_bytes(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let file = fs::File::open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    if meta.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("file is larger than {} MiB", limit / 1024 / 1024),
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file exceeds size limit",
        ));
    }
    Ok(bytes)
}
pub(crate) fn read_encoded(
    path: &Path,
    explicit: Option<TextFormat>,
    limit: u64,
) -> io::Result<(DecodedText, crate::domain::document::DiskState)> {
    let bytes = read_bytes(path, limit)?;
    let mut decoded = encoding::decode(&bytes, explicit, &options(path), limit as usize)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if explicit.is_some() {
        decoded.format.bom = match decoded.format.charset {
            encoding::Charset::Utf8 => bytes.starts_with(&[0xef, 0xbb, 0xbf]),
            encoding::Charset::Utf16Le => bytes.starts_with(&[0xff, 0xfe]),
            encoding::Charset::Utf16Be => bytes.starts_with(&[0xfe, 0xff]),
            _ => false,
        };
    }
    let state = state(path, &bytes, decoded.format);
    // Plain UTF-8 round-trips exactly, so the decoded text already is the file: keeping the raw
    // bytes would hold a second full copy for as long as the document is open. Callers treat an
    // empty `bytes` as "not retained" and fall back to encoding the text.
    decoded.bytes = if is_lossless_utf8(&bytes, &decoded) {
        std::sync::Arc::default()
    } else {
        std::sync::Arc::new(bytes)
    };
    Ok((decoded, state))
}
/// Whether `encode(decoded.text, decoded.format)` gives back exactly `bytes`.
fn is_lossless_utf8(bytes: &[u8], decoded: &DecodedText) -> bool {
    if decoded.format.charset != encoding::Charset::Utf8 {
        return false;
    }
    let body = if decoded.format.bom {
        bytes.strip_prefix(&[0xef, 0xbb, 0xbf][..])
    } else {
        Some(bytes)
    };
    body == Some(decoded.text.as_bytes())
}
pub(crate) fn state(
    path: &Path,
    bytes: &[u8],
    format: TextFormat,
) -> crate::domain::document::DiskState {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    crate::domain::document::DiskState {
        modified_time: fs::metadata(path).ok().and_then(|m| m.modified().ok()),
        size: bytes.len() as u64,
        content_hash: hasher.finish(),
        format,
    }
}
pub(crate) fn read_text_file(path: &Path) -> Result<String, String> {
    read_encoded(path, None, MAX_EDITABLE_FILE_BYTES)
        .map(|(v, _)| v.text)
        .map_err(|e| format!("Could not open {}: {e}", path.display()))
}
pub fn read_diff_file(path: &Path) -> io::Result<String> {
    read_encoded(path, None, MAX_EDITABLE_FILE_BYTES).map(|(v, _)| v.text)
}
pub fn write_encoded(path: &Path, text: &str, format: TextFormat) -> io::Result<()> {
    let bytes = encoding::encode(text, format)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    write(path, &bytes)
}
pub fn write(path: &Path, contents: &[u8]) -> io::Result<()> {
    write_with(path, |file| file.write_all(contents))
}
/// Free space on the volume holding `dir`, when it can be known.
#[cfg(windows)]
fn free_space(dir: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut available = 0u64;
    // SAFETY: `wide` is NUL terminated and `available` outlives the call; the other two
    // out-parameters are optional.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(available)
}
#[cfg(unix)]
#[allow(clippy::unnecessary_cast)] // the field types differ between platforms
fn free_space(dir: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `path` is NUL terminated and `stat` is a valid out-parameter.
    (unsafe { libc::statvfs(path.as_ptr(), &mut stat) } == 0)
        .then(|| stat.f_bavail as u64 * stat.f_frsize as u64)
}

/// Check that `needed` bytes can be written beside `path`, as a save writes a whole new file
/// there before it replaces the old one. A clear message now is better than a write that
/// fails halfway through a very large file.
pub(crate) fn ensure_room_beside(path: &Path, needed: u64) -> io::Result<()> {
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    match free_space(directory) {
        Some(free) if free < needed => Err(io::Error::other(format!(
            "there is not enough free disk space: saving writes a new copy of the file beside it, which needs {} MB, and {} MB are free",
            needed.div_ceil(1024 * 1024),
            free / (1024 * 1024)
        ))),
        _ => Ok(()),
    }
}

/// Replace `path` with what `fill` writes to a temporary file beside it, so a failure at any
/// point leaves the original untouched.
pub(crate) fn write_with(
    path: &Path,
    fill: impl FnOnce(&mut fs::File) -> io::Result<()>,
) -> io::Result<()> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    // Follow symbolic links, preserving the link rather than replacing it.
    let target = if path.is_symlink() {
        fs::canonicalize(path)?
    } else {
        /// Whether `encode(decoded.text, decoded.format)` gives back exactly `bytes`.
        fn is_lossless_utf8(bytes: &[u8], decoded: &DecodedText) -> bool {
            if decoded.format.charset != encoding::Charset::Utf8 {
                return false;
            }
            let body = if decoded.format.bom {
                bytes.strip_prefix(&[0xef, 0xbb, 0xbf][..])
            } else {
                Some(bytes)
            };
            body == Some(decoded.text.as_bytes())
        }
        path.to_path_buf()
    };
    let permissions = fs::metadata(&target).ok().map(|m| m.permissions());
    if permissions.as_ref().is_some_and(|p| p.readonly()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "file is read-only",
        ));
    }
    let parent = target.parent().unwrap_or(Path::new("."));
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    let temp = parent.join(format!(
        ".{name}.loog-{}-{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)?;
    let result = (|| {
        fill(&mut file)?;
        if let Some(p) = permissions {
            file.set_permissions(p)?;
        }
        file.sync_all()?;
        drop(file);
        replace(&temp, &target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}
#[cfg(not(windows))]
fn replace(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}
#[cfg(windows)]
fn replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW, ReplaceFileW,
    };
    let exists = to.exists();
    let from = from
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let to = to
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let replaced = if exists {
        unsafe {
            ReplaceFileW(
                to.as_ptr(),
                from.as_ptr(),
                std::ptr::null(),
                0,
                std::ptr::null(),
                std::ptr::null(),
            )
        }
    } else {
        unsafe {
            MoveFileExW(
                from.as_ptr(),
                to.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        }
    };
    if replaced == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
