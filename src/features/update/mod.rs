//! Application updates: release versions, the signed update manifest and the
//! update state shown in the title bar. Network, signatures and installers
//! live in `infrastructure/update`.
mod model;
mod version;

pub use self::model::{About, Indicator, Intent, Model, Stage};
pub use self::version::Version;

use serde::Deserialize;
use std::collections::BTreeMap;

/// The release feed a build reads, one manifest per platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    WindowsX64,
    MacUniversal,
    LinuxX64,
}

impl Platform {
    /// Name used in the manifest file (`update-<id>.json`) and its payload.
    pub fn id(self) -> &'static str {
        match self {
            Self::WindowsX64 => "windows-x86_64",
            Self::MacUniversal => "macos-universal",
            Self::LinuxX64 => "linux-x86_64",
        }
    }
}

/// How this copy of Loog was installed, which decides how it can update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallKind {
    /// Per-user Inno Setup install: downloads and runs the new installer.
    WindowsSetup,
    WindowsPortable,
    MacApp,
    LinuxDeb,
    LinuxAppImage,
    LinuxTarball,
    /// An install Loog cannot replace itself: in a read-only folder, or a
    /// macOS app run from a translocated path. Updates open the release page.
    Manual,
}

impl InstallKind {
    /// The manifest asset that updates this install in place, if any; the
    /// others open the release page instead.
    pub fn installer_asset(self) -> Option<&'static str> {
        match self {
            Self::WindowsSetup => Some("setup"),
            Self::MacApp => Some("zip"),
            Self::LinuxAppImage => Some("appimage"),
            Self::LinuxTarball => Some("tarball"),
            Self::WindowsPortable | Self::LinuxDeb | Self::Manual => None,
        }
    }
}

/// One downloadable file of a release.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    /// Lowercase hex SHA-256 of the file.
    pub sha256: String,
    pub size: u64,
}

/// A release offered to one platform, read from the signed manifest payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    /// GitHub release page with the notes and every package.
    pub notes_url: String,
    pub assets: BTreeMap<String, Asset>,
}

#[derive(Deserialize)]
struct Payload {
    schema: u32,
    platform: String,
    version: String,
    notes_url: String,
    #[serde(default)]
    assets: BTreeMap<String, Asset>,
}

/// Parse a verified manifest payload. The payload names its platform so a
/// manifest signed for one platform cannot be served to another.
pub fn parse_release(payload: &[u8], platform: Platform) -> Result<Release, String> {
    let payload: Payload =
        serde_json::from_slice(payload).map_err(|error| format!("invalid manifest: {error}"))?;
    if payload.schema != 1 {
        return Err(format!("unsupported manifest schema {}", payload.schema));
    }
    if payload.platform != platform.id() {
        return Err(format!(
            "manifest is for {}, not {}",
            payload.platform,
            platform.id()
        ));
    }
    let version = Version::parse(&payload.version)
        .ok_or_else(|| format!("invalid release version \"{}\"", payload.version))?;
    if !payload.notes_url.starts_with("https://") {
        return Err("release notes must use https".to_owned());
    }
    for (key, asset) in &payload.assets {
        let hex = asset.sha256.len() == 64
            && asset
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if !asset.url.starts_with("https://") || !hex {
            return Err(format!("invalid asset \"{key}\""));
        }
    }
    Ok(Release {
        version,
        notes_url: payload.notes_url,
        assets: payload.assets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn payload(platform: &str, version: &str) -> String {
        format!(
            r#"{{"schema":1,"platform":"{platform}","version":"{version}",
            "notes_url":"https://github.com/lius-new/loog/releases/tag/v{version}",
            "assets":{{"setup":{{"name":"Loog-setup.exe","url":"https://example.com/a.exe","sha256":"{SHA}","size":42}}}}}}"#
        )
    }

    #[test]
    fn parses_a_release_for_its_platform() {
        let release = parse_release(
            payload("windows-x86_64", "0.2.0").as_bytes(),
            Platform::WindowsX64,
        )
        .unwrap();
        assert_eq!(release.version, Version::parse("0.2.0").unwrap());
        assert_eq!(release.assets["setup"].size, 42);
    }

    #[test]
    fn rejects_other_platforms_and_bad_assets() {
        assert!(
            parse_release(
                payload("linux-x86_64", "0.2.0").as_bytes(),
                Platform::WindowsX64
            )
            .unwrap_err()
            .contains("linux-x86_64")
        );
        let insecure =
            payload("windows-x86_64", "0.2.0").replace("https://example", "http://example");
        assert!(parse_release(insecure.as_bytes(), Platform::WindowsX64).is_err());
        let short = payload("windows-x86_64", "0.2.0").replace(SHA, "abc");
        assert!(parse_release(short.as_bytes(), Platform::WindowsX64).is_err());
        assert!(parse_release(b"{}", Platform::WindowsX64).is_err());
    }

    #[test]
    fn package_managed_and_manual_installs_open_the_release_page() {
        assert_eq!(InstallKind::WindowsSetup.installer_asset(), Some("setup"));
        assert_eq!(InstallKind::MacApp.installer_asset(), Some("zip"));
        assert_eq!(
            InstallKind::LinuxAppImage.installer_asset(),
            Some("appimage")
        );
        assert_eq!(InstallKind::LinuxTarball.installer_asset(), Some("tarball"));
        assert_eq!(InstallKind::WindowsPortable.installer_asset(), None);
        assert_eq!(InstallKind::LinuxDeb.installer_asset(), None);
        assert_eq!(InstallKind::Manual.installer_asset(), None);
    }
}
