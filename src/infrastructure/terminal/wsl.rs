//! Read registered distributions without starting WSL or a console process.
use super::profiles::resolve_program;
use crate::features::terminal::{ShellKind, profiles::ShellOption};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW,
    RegOpenKeyExW,
};

const DISTRIBUTIONS_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Lxss";

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        unsafe { RegCloseKey(self.0) };
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

impl Key {
    fn open(parent: HKEY, path: &str) -> Option<Self> {
        let mut key = null_mut();
        let path = wide(path);
        let status = unsafe { RegOpenKeyExW(parent, path.as_ptr(), 0, KEY_READ, &mut key) };
        (status == ERROR_SUCCESS).then(|| Self(key))
    }

    fn string(&self, name: &str) -> Option<String> {
        let name = wide(name);
        let mut bytes = 0;
        let status = unsafe {
            RegGetValueW(
                self.0,
                null(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                null_mut(),
                null_mut(),
                &mut bytes,
            )
        };
        if status != ERROR_SUCCESS || bytes == 0 || bytes > 65_536 || bytes % 2 != 0 {
            return None;
        }
        let mut value = vec![0u16; bytes as usize / 2];
        let status = unsafe {
            RegGetValueW(
                self.0,
                null(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                null_mut(),
                value.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        if status != ERROR_SUCCESS || bytes % 2 != 0 {
            return None;
        }
        value.truncate(bytes as usize / 2);
        while value.last() == Some(&0) {
            value.pop();
        }
        String::from_utf16(&value)
            .ok()
            .filter(|s| !s.contains('\0'))
    }
}

fn registered() -> Vec<(String, String)> {
    let Some(root) = Key::open(HKEY_CURRENT_USER, DISTRIBUTIONS_KEY) else {
        return Vec::new();
    };
    let mut distributions = Vec::new();
    for index in 0.. {
        // Registry key names have at most 255 UTF-16 code units.
        let mut name = [0u16; 256];
        let mut len = name.len() as u32;
        let status = unsafe {
            RegEnumKeyExW(
                root.0,
                index,
                name.as_mut_ptr(),
                &mut len,
                null(),
                null_mut(),
                null_mut(),
                null_mut(),
            )
        };
        if status != ERROR_SUCCESS {
            break;
        }
        let Ok(guid) = String::from_utf16(&name[..len as usize]) else {
            continue;
        };
        if distribution_id(&guid).is_none() {
            continue;
        }
        if let Some(key) = Key::open(root.0, &guid)
            && let Some(name) = key.string("DistributionName")
        {
            distributions.push((guid, name));
        }
    }
    distributions
}

fn distribution_id(guid: &str) -> Option<u64> {
    if guid.len() != 38
        || !guid.bytes().enumerate().all(|(index, byte)| match index {
            0 => byte == b'{',
            37 => byte == b'}',
            9 | 14 | 19 | 24 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
    {
        return None;
    }
    // Fixed FNV-1a fingerprint: persisted choices must not depend on registry
    // enumeration order, display names, or Rust's randomized hashers.
    Some(guid.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ u64::from(byte.to_ascii_lowercase())).wrapping_mul(0x100000001b3)
    }))
}

fn from_registered(program: &Path, registrations: Vec<(String, String)>) -> Vec<ShellOption> {
    let mut options = registrations
        .into_iter()
        .filter_map(|(guid, name)| {
            let id = distribution_id(&guid)?;
            if name.trim().is_empty() || name.contains(['\0', '\r', '\n']) {
                return None;
            }
            Some(ShellOption {
                kind: ShellKind::Wsl(id),
                name: format!("WSL · {name}"),
                program: Some(program.to_path_buf()),
                args: vec!["-d".into(), name],
            })
        })
        .collect::<Vec<_>>();
    options.sort_by_cached_key(|option| option.name.to_lowercase());
    options
}

pub(super) fn options() -> Vec<ShellOption> {
    let program = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .and_then(|root| resolve_program(&root.join("System32").join("wsl.exe")));
    program.map_or_else(Vec::new, |program| from_registered(&program, registered()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEBIAN: &str = "{11111111-2222-3333-4444-555555555555}";
    const UBUNTU: &str = "{aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee}";

    #[test]
    fn distribution_choices_are_stable_across_reordering_and_renaming() {
        let program = Path::new(r"C:\Windows\System32\wsl.exe");
        let original = from_registered(
            program,
            vec![
                (UBUNTU.into(), "Ubuntu".into()),
                (DEBIAN.into(), "Debian".into()),
            ],
        );
        let renamed = from_registered(
            program,
            vec![
                (DEBIAN.into(), "Debian dev".into()),
                (UBUNTU.to_uppercase(), "Ubuntu".into()),
            ],
        );
        assert_eq!(original[0].name, "WSL · Debian");
        assert_eq!(original[0].kind, renamed[0].kind);
        assert_eq!(original[1].kind, renamed[1].kind);
        assert_eq!(original[0].program.as_deref(), Some(program));
        assert_eq!(original[0].args, ["-d", "Debian"]);
        let saved = serde_json::to_string(&original[0].kind).unwrap();
        assert_eq!(
            serde_json::from_str::<ShellKind>(&saved).unwrap(),
            renamed[0].kind
        );
        let settings = crate::features::settings::rules::Settings {
            default_shell: original[0].kind,
            ..Default::default()
        };
        let saved = serde_json::to_string(&settings).unwrap();
        let restored: crate::features::settings::rules::Settings =
            serde_json::from_str(&saved).unwrap();
        assert!(restored.terminal_profiles.is_empty());
        assert_eq!(
            crate::features::terminal::profiles::effective_default(
                restored.default_shell,
                &renamed,
            ),
            Some(original[0].kind)
        );
    }

    #[test]
    fn invalid_registrations_are_skipped_and_unicode_names_are_preserved() {
        let options = from_registered(
            Path::new("wsl.exe"),
            vec![
                ("not a GUID".into(), "Invalid".into()),
                (DEBIAN.into(), " ".into()),
                (DEBIAN.into(), "bad\0name".into()),
                (UBUNTU.into(), "开发环境".into()),
            ],
        );
        assert_eq!(options.len(), 1);
        assert_eq!(options[0].name, "WSL · 开发环境");
        assert_eq!(options[0].args, ["-d", "开发环境"]);
        assert!(from_registered(Path::new("wsl.exe"), Vec::new()).is_empty());
    }

    #[test]
    #[ignore = "manual Windows smoke test requiring an installed WSL distribution"]
    fn registered_distribution_is_listed_named_and_launches_without_a_custom_profile() {
        use crate::features::terminal::TerminalSize;
        use crate::infrastructure::terminal::{
            notification::FrameNotifier, runtime::TerminalRuntime,
        };
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        let detected = options()
            .into_iter()
            .next()
            .expect("installed WSL distribution");
        eprintln!("Detected {} ({:?})", detected.name, detected.kind);
        assert!(super::super::profiles::options(&[]).contains(&detected));
        let model =
            crate::features::terminal::model::TerminalModel::restored(vec![detected.kind], None);
        let runtime = Arc::new(TerminalRuntime::default());
        runtime.synchronize(&model);
        let controller = runtime
            .handle(model.active_id().unwrap(), detected.kind)
            .test_controller();
        assert_eq!(controller.short_label(), detected.name);
        let notify = FrameNotifier::new(|| {});
        controller.restart(
            detected.kind,
            None,
            TerminalSize {
                rows: 24,
                cols: 100,
                pixel_width: 800,
                pixel_height: 480,
            },
            notify.clone(),
        );
        // Octal escapes keep the answer out of the command echoed by the PTY.
        controller.write(b"printf '\\114\\117\\117\\115\\137\\127\\123\\114\\137\\117\\113\\n'\r");
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut contents;
        loop {
            contents = controller.test_parser().screen().contents();
            if contents.contains("LOOG_WSL_OK") || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        controller.terminate(notify);
        assert!(contents.contains("LOOG_WSL_OK"), "WSL output: {contents}");
    }
}
