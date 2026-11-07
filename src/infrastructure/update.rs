//! Update adapters: install detection, the signed manifest over HTTPS, the
//! verified download and applying it after Loog quits: the Windows installer
//! runs silently; on macOS and Linux a detached `sh` script swaps in the new
//! app bundle, AppImage or folder and starts Loog again.
//!
//! Packaged builds (`LOOG_UPDATES=1` at compile time, set by
//! `scripts/package`) check `releases/latest/download/update-<platform>.json`.
//! `LOOG_UPDATE_FEED=<base url>` points any build at another feed for testing.
use crate::features::update::{Asset, InstallKind, Platform, Release, parse_release};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

const FEED: &str = "https://github.com/lius-new/loog/releases/latest/download";
/// Raw Ed25519 public key; `node scripts/package/package.mjs keygen` makes
/// the pair. The private key is the `LOOG_UPDATE_SIGNING_KEY` release secret.
const PUBLIC_KEY: &str = "c9t/ZmkmU2jqqw11/GVtbxY44Oor7YZrBFsU4QqfF8k=";
/// Manifests are a few hundred bytes; anything larger is not one.
const MANIFEST_LIMIT: u64 = 64 * 1024;
const INSTALLER_LIMIT: u64 = 1024 * 1024 * 1024;

static PENDING_INSTALLER: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn platform() -> Option<Platform> {
    if cfg!(all(windows, target_arch = "x86_64")) {
        Some(Platform::WindowsX64)
    } else if cfg!(target_os = "macos") {
        Some(Platform::MacUniversal)
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some(Platform::LinuxX64)
    } else {
        None
    }
}

fn feed() -> Option<String> {
    match std::env::var("LOOG_UPDATE_FEED") {
        Ok(url) if !url.trim().is_empty() => Some(url.trim().trim_end_matches('/').to_owned()),
        _ => (option_env!("LOOG_UPDATES") == Some("1")).then(|| FEED.to_owned()),
    }
}

/// How this copy was installed; `None` when it does not check for updates.
pub fn install_kind() -> Option<InstallKind> {
    feed()?;
    platform()?;
    detect().map(|(kind, _)| kind)
}

/// The install kind and what an update replaces: the install folder, the app
/// bundle or the AppImage file.
fn detect() -> Option<(InstallKind, PathBuf)> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.to_path_buf();
    let replaceable = |kind, parent: Option<&Path>| {
        if parent.is_some_and(writable) {
            kind
        } else {
            InstallKind::Manual
        }
    };
    Some(if cfg!(windows) {
        // Inno Setup leaves its uninstaller next to the installed files.
        if dir.join("unins000.exe").is_file() {
            (InstallKind::WindowsSetup, dir)
        } else {
            (InstallKind::WindowsPortable, dir)
        }
    } else if cfg!(target_os = "macos") {
        // Loog.app/Contents/MacOS/Loog
        let app = exe.ancestors().nth(3)?.to_path_buf();
        // An app opened from Downloads runs from a read-only, randomized
        // path until it is moved to Applications.
        let bundle = app.extension().is_some_and(|ext| ext == "app")
            && !app.to_string_lossy().contains("/AppTranslocation/");
        let kind = if bundle {
            replaceable(InstallKind::MacApp, app.parent())
        } else {
            InstallKind::Manual
        };
        (kind, app)
    } else if let Some(image) = std::env::var_os("APPIMAGE").map(PathBuf::from) {
        (
            replaceable(InstallKind::LinuxAppImage, image.parent()),
            image,
        )
    } else if exe.starts_with("/opt/loog") {
        (InstallKind::LinuxDeb, dir)
    } else {
        (replaceable(InstallKind::LinuxTarball, dir.parent()), dir)
    })
}

/// Whether files can be created in `dir`; the swap happens next to the
/// install so it is a rename on one file system.
fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".loog-write-check-{}", std::process::id()));
    let created = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .is_ok();
    if created {
        let _ = std::fs::remove_file(&probe);
    }
    created
}

/// A test feed served from this machine may use plain HTTP.
fn is_loopback(url: &str) -> bool {
    ["http://127.0.0.1:", "http://localhost:", "http://[::1]:"]
        .iter()
        .any(|prefix| url.starts_with(prefix))
}

fn agent(url: &str) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(60))
        .user_agent(concat!("Loog/", env!("CARGO_PKG_VERSION")))
        // Honors HTTPS_PROXY and friends, except for a loopback test feed.
        .try_proxy_from_env(!is_loopback(url))
        .https_only(!is_loopback(url))
        .build()
}

fn get(url: &str) -> Result<ureq::Response, String> {
    agent(url).get(url).call().map_err(|error| match error {
        ureq::Error::Status(code, _) => format!("the server returned HTTP {code}"),
        error => error.to_string(),
    })
}

/// Fetch, verify and parse this platform's manifest.
pub fn check(platform: Platform) -> Result<Release, String> {
    let feed = feed().ok_or("updates are disabled in this build")?;
    let url = format!("{feed}/update-{}.json", platform.id());
    let mut body = Vec::new();
    get(&url)?
        .into_reader()
        .take(MANIFEST_LIMIT)
        .read_to_end(&mut body)
        .map_err(|error| error.to_string())?;
    let payload = verified_payload(&body, PUBLIC_KEY)?;
    parse_release(&payload, platform)
}

/// The signed payload bytes of a manifest envelope.
fn verified_payload(envelope: &[u8], public_key: &str) -> Result<Vec<u8>, String> {
    #[derive(serde::Deserialize)]
    struct Envelope {
        payload: String,
        signature: String,
    }
    let envelope: Envelope =
        serde_json::from_slice(envelope).map_err(|error| format!("invalid manifest: {error}"))?;
    let decode = |text: &str| BASE64.decode(text).map_err(|_| "invalid manifest encoding");
    let payload = decode(&envelope.payload)?;
    let signature = decode(&envelope.signature)?;
    let key = decode(public_key)?;
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
        .verify(&payload, &signature)
        .map_err(|_| "the update manifest signature is invalid".to_owned())?;
    Ok(payload)
}

/// Download `asset` into the update folder and check its size and SHA-256.
/// `progress` receives the bytes received so far, about once per percent.
pub fn download(asset: &Asset, mut progress: impl FnMut(u64)) -> Result<PathBuf, String> {
    let name = Path::new(&asset.name)
        .file_name()
        .filter(|name| *name == asset.name.as_str())
        .ok_or("invalid installer name")?;
    if asset.size > INSTALLER_LIMIT {
        return Err("the installer is too large".to_owned());
    }
    let dir = std::env::temp_dir().join("loog-update");
    // Earlier downloads are never needed again.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let target = dir.join(name);
    let partial = target.with_extension("part");

    let io = |error: std::io::Error| error.to_string();
    let mut reader = get(&asset.url)?.into_reader().take(asset.size + 1);
    let mut file = std::fs::File::create(&partial).map_err(io)?;
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = vec![0; 64 * 1024];
    let mut received = 0u64;
    let step = (asset.size / 100).max(1);
    let mut reported = 0u64;
    loop {
        let read = reader.read(&mut buffer).map_err(io)?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read]).map_err(io)?;
        digest.update(&buffer[..read]);
        received += read as u64;
        if received - reported >= step {
            reported = received;
            progress(received);
        }
    }
    file.sync_all().map_err(io)?;
    drop(file);

    let actual = hex(digest.finish().as_ref());
    if received != asset.size || actual != asset.sha256 {
        let _ = std::fs::remove_file(&partial);
        return Err("the downloaded installer does not match the release".to_owned());
    }
    std::fs::rename(&partial, &target).map_err(io)?;
    progress(received);
    Ok(target)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Apply the downloaded `installer` once Loog quits (see
/// `run_pending_installer`).
pub fn install_on_exit(installer: PathBuf) {
    *PENDING_INSTALLER.lock().unwrap() = Some(installer);
}

/// Waits for Loog (`$1`, its process id) to exit so its files are free.
const WAIT: &str = r#"set -eu
pid=$1
shift
while kill -0 "$pid" 2>/dev/null; do sleep 0.2; done
"#;

/// `$1`: the downloaded zip of Loog.app; `$2`: the installed Loog.app. The
/// new bundle is unpacked next to the old one so the swap is a rename, and
/// the old one comes back if the new one cannot be moved in.
const MAC_APP: &str = r#"archive=$1
app=$2
stage=$(mktemp -d "$(dirname "$app")/.loog-update.XXXXXX")
trap 'rm -rf "$stage"' EXIT
ditto -x -k "$archive" "$stage"
test -x "$stage/Loog.app/Contents/MacOS/Loog"
mv "$app" "$stage/previous.app"
if ! mv "$stage/Loog.app" "$app"; then
  mv "$stage/previous.app" "$app"
  exit 1
fi
rm -f "$archive"
open "$app"
"#;

/// `$1`: the downloaded AppImage; `$2`: the AppImage Loog runs from.
const APP_IMAGE: &str = r#"image=$1
target=$2
next="$target.loog-update"
cp "$image" "$next"
chmod 755 "$next"
mv -f "$next" "$target"
rm -f "$image"
nohup "$target" >/dev/null 2>&1 &
"#;

/// `$1`: the downloaded tar.gz, which holds one `Loog-<version>-…` folder;
/// `$2`: the install folder.
const TARBALL: &str = r#"archive=$1
dir=$2
stage=$(mktemp -d "$(dirname "$dir")/.loog-update.XXXXXX")
trap 'rm -rf "$stage"' EXIT
tar -xzf "$archive" -C "$stage"
set -- "$stage"/Loog-*/
new=${1%/}
test -x "$new/Loog"
mv "$dir" "$stage/previous"
if ! mv "$new" "$dir"; then
  mv "$stage/previous" "$dir"
  exit 1
fi
rm -f "$archive"
nohup "$dir/Loog" >/dev/null 2>&1 &
"#;

/// The program and arguments that apply `installer` to the install at
/// `target` once process `pid` has exited.
fn apply_command(
    kind: InstallKind,
    installer: &Path,
    target: &Path,
    pid: u32,
) -> Option<(PathBuf, Vec<OsString>)> {
    let script = match kind {
        InstallKind::WindowsSetup => {
            // Setup closes any other Loog instance, replaces the files and
            // starts Loog again (`/relaunch=1`, see `installer.iss`).
            let args = [
                "/VERYSILENT",
                "/SUPPRESSMSGBOXES",
                "/NORESTART",
                "/relaunch=1",
            ];
            return Some((installer.to_path_buf(), args.map(OsString::from).to_vec()));
        }
        InstallKind::MacApp => MAC_APP,
        InstallKind::LinuxAppImage => APP_IMAGE,
        InstallKind::LinuxTarball => TARBALL,
        InstallKind::WindowsPortable | InstallKind::LinuxDeb | InstallKind::Manual => {
            return None;
        }
    };
    let args = vec![
        OsString::from("-c"),
        OsString::from(format!("{WAIT}{script}")),
        // `$0`, used in error messages.
        OsString::from("loog-update"),
        OsString::from(pid.to_string()),
        installer.as_os_str().to_owned(),
        target.as_os_str().to_owned(),
    ];
    Some((PathBuf::from("/bin/sh"), args))
}

/// Start applying the update recorded by `install_on_exit`, after the
/// window has closed. On macOS and Linux the script's output goes to
/// `apply.log` next to the download.
pub fn run_pending_installer() {
    let Some(installer) = PENDING_INSTALLER.lock().unwrap().take() else {
        return;
    };
    let Some((kind, target)) = detect() else {
        return;
    };
    let Some((program, args)) = apply_command(kind, &installer, &target, std::process::id()) else {
        return;
    };
    let mut command = std::process::Command::new(program);
    command.args(args).stdin(std::process::Stdio::null());
    if kind != InstallKind::WindowsSetup {
        if let Ok(log) = std::fs::File::create(installer.with_file_name("apply.log")) {
            if let Ok(copy) = log.try_clone() {
                command.stdout(copy);
            }
            command.stderr(log);
        }
        // Hold no handle on a folder being replaced, and keep running after
        // Loog exits.
        command.current_dir("/");
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
    }
    let _ = command.spawn();
}

/// Open a release page in the default browser.
pub fn open_page(url: &str) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let wide = |text: &str| {
            std::ffi::OsStr::new(text)
                .encode_wide()
                .chain(Some(0))
                .collect::<Vec<u16>>()
        };
        let (verb, target) = (wide("open"), wide(url));
        // SAFETY: both strings are NUL-terminated and outlive the call.
        let result = unsafe {
            windows_sys::Win32::UI::Shell::ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
            )
        };
        // ShellExecuteW reports success with a value above 32.
        if result as usize > 32 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed test key pair (RFC 8032 test 1), not the release key.
    const TEST_PUBLIC: &str = "11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=";
    const TEST_SIGNATURE_OF_EMPTY: &str =
        "5VZDAMNgrHKQhuLMgG6CioSHfx645dl02HPgZSJJAVVfuIIVkKM7rMYeOXAc+bRr0lv18FlbviRlUUFDjnoQCw==";

    #[test]
    fn each_install_kind_applies_its_own_way() {
        let (program, args) = apply_command(
            InstallKind::WindowsSetup,
            Path::new("setup.exe"),
            Path::new("C:/Loog"),
            7,
        )
        .unwrap();
        assert_eq!(program, PathBuf::from("setup.exe"));
        assert!(args.contains(&OsString::from("/relaunch=1")));

        for (kind, script) in [
            (InstallKind::MacApp, MAC_APP),
            (InstallKind::LinuxAppImage, APP_IMAGE),
            (InstallKind::LinuxTarball, TARBALL),
        ] {
            let (program, args) =
                apply_command(kind, Path::new("/tmp/new"), Path::new("/opt/x"), 7).unwrap();
            assert_eq!(program, PathBuf::from("/bin/sh"));
            assert_eq!(args[1], OsString::from(format!("{WAIT}{script}")));
            assert_eq!(&args[3..], ["7", "/tmp/new", "/opt/x"].map(OsString::from));
        }

        for kind in [
            InstallKind::WindowsPortable,
            InstallKind::LinuxDeb,
            InstallKind::Manual,
        ] {
            assert!(apply_command(kind, Path::new("a"), Path::new("b"), 7).is_none());
        }
    }

    /// A fresh, empty folder for one test.
    #[cfg(unix)]
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("loog-update-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A stand-in for Loog that records that it was started.
    #[cfg(unix)]
    fn fake_loog(path: &Path, version: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(
            path,
            format!("#!/bin/sh\n# {version}\ntouch \"$0.launched\"\n"),
        )
        .unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Run the apply step for an already exited process and wait for the
    /// relaunched stand-in.
    #[cfg(unix)]
    fn apply(kind: InstallKind, installer: &Path, target: &Path, launched: &Path) {
        let mut exited = std::process::Command::new("true").spawn().unwrap();
        exited.wait().unwrap();
        let (program, args) = apply_command(kind, installer, target, exited.id()).unwrap();
        let output = std::process::Command::new(program)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        for _ in 0..100 {
            if launched.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("{} was not started", launched.display());
    }

    #[test]
    #[cfg(unix)]
    fn apply_scripts_are_valid_shell() {
        for script in [MAC_APP, APP_IMAGE, TARBALL] {
            let status = std::process::Command::new("/bin/sh")
                .args(["-n", "-c", &format!("{WAIT}{script}")])
                .status()
                .unwrap();
            assert!(status.success());
        }
    }

    #[test]
    #[cfg(unix)]
    fn tarball_update_replaces_the_folder_and_restarts() {
        let root = scratch("tarball");
        let install = root.join("lib").join("loog");
        std::fs::create_dir_all(&install).unwrap();
        fake_loog(&install.join("Loog"), "old");
        std::fs::write(install.join("removed-file"), "").unwrap();

        let package = root.join("package").join("Loog-9.9.9-linux-x86_64");
        std::fs::create_dir_all(&package).unwrap();
        fake_loog(&package.join("Loog"), "new");
        let archive = root.join("Loog.tar.gz");
        let tar = std::process::Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(root.join("package"))
            .arg("Loog-9.9.9-linux-x86_64")
            .status()
            .unwrap();
        assert!(tar.success());

        apply(
            InstallKind::LinuxTarball,
            &archive,
            &install,
            &install.join("Loog.launched"),
        );
        let loog = std::fs::read_to_string(install.join("Loog")).unwrap();
        assert!(loog.contains("# new"));
        assert!(!install.join("removed-file").exists());
        assert!(!archive.exists());
        let leftovers = std::fs::read_dir(root.join("lib")).unwrap().count();
        assert_eq!(leftovers, 1, "the staging folder is removed");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn appimage_update_replaces_the_file_and_restarts() {
        let root = scratch("appimage");
        let target = root.join("Loog.AppImage");
        fake_loog(&target, "old");
        let download = root.join("download.AppImage");
        std::fs::write(&download, "#!/bin/sh\n# new\ntouch \"$0.launched\"\n").unwrap();

        apply(
            InstallKind::LinuxAppImage,
            &download,
            &target,
            &root.join("Loog.AppImage.launched"),
        );
        assert!(std::fs::read_to_string(&target).unwrap().contains("# new"));
        assert!(!download.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    fn envelope(payload: &[u8], signature: &str) -> Vec<u8> {
        format!(
            r#"{{"payload":"{}","signature":"{signature}"}}"#,
            BASE64.encode(payload)
        )
        .into_bytes()
    }

    #[test]
    fn accepts_only_payloads_signed_by_the_key() {
        assert_eq!(
            verified_payload(&envelope(b"", TEST_SIGNATURE_OF_EMPTY), TEST_PUBLIC).unwrap(),
            b""
        );
        assert!(verified_payload(&envelope(b"x", TEST_SIGNATURE_OF_EMPTY), TEST_PUBLIC).is_err());
        assert!(
            verified_payload(&envelope(b"", TEST_SIGNATURE_OF_EMPTY), PUBLIC_KEY).is_err(),
            "the release key must not verify another key's signature"
        );
        assert!(verified_payload(b"not json", TEST_PUBLIC).is_err());
    }

    #[test]
    fn release_key_is_a_raw_ed25519_key() {
        assert_eq!(BASE64.decode(PUBLIC_KEY).unwrap().len(), 32);
    }

    /// Reads the real feed, or `LOOG_UPDATE_FEED`:
    /// `cargo test live_feed -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs the network"]
    fn live_feed_is_signed_by_the_release_key() {
        let platform = platform().expect("a released platform");
        let feed = feed().unwrap_or_else(|| FEED.to_owned());
        let url = format!("{feed}/update-{}.json", platform.id());
        let mut body = Vec::new();
        get(&url)
            .unwrap()
            .into_reader()
            .read_to_end(&mut body)
            .unwrap();
        let payload = verified_payload(&body, PUBLIC_KEY).unwrap();
        let release = parse_release(&payload, platform).unwrap();
        println!("{url}: {} {:?}", release.version, release.assets.keys());
    }

    #[test]
    fn only_loopback_feeds_may_use_http() {
        assert!(is_loopback("http://127.0.0.1:8080/update.json"));
        assert!(is_loopback("http://localhost:9000"));
        assert!(!is_loopback("http://127.0.0.1.example.com/"));
        assert!(!is_loopback("http://example.com/"));
    }

    #[test]
    fn hashes_print_as_lowercase_hex() {
        assert_eq!(hex(&[0x00, 0xab, 0xff]), "00abff");
    }
}
