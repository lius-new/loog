//! Update state machine: checking, an available release, its download and
//! the restart that installs it.
use super::{Asset, InstallKind, Release, Version};
use std::path::PathBuf;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Idle,
    Checking,
    Available(Release),
    Downloading {
        release: Release,
        received: u64,
    },
    /// The installer is downloaded and verified; it runs once Loog quits.
    Ready {
        release: Release,
        installer: PathBuf,
    },
}

/// What clicking the title bar button or the Settings action should do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intent {
    /// Check now; the phase is already `Checking`.
    Check,
    Download(Asset),
    OpenPage(String),
    Install(PathBuf),
}

/// The title bar button for the current phase.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Badge {
    pub label: String,
    /// Whether clicking does anything.
    pub actionable: bool,
}

/// The title bar update control.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Indicator {
    pub stage: Stage,
    /// Short text beside the icon.
    pub text: String,
    /// Full description, for accessibility.
    pub label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// A newer release can be downloaded or opened; clickable.
    Available,
    /// The installer is downloading; the value is the percent received.
    Downloading(u8),
    /// The installer is ready; clicking restarts Loog into it.
    Ready,
}

/// The Version row on the Settings page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct About {
    /// `Loog 0.1.0`, then the update status when there is one.
    pub detail: String,
    /// The link on the right; `None` in builds that do not update.
    pub action: Option<Badge>,
}

/// How the last finished check went.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum LastCheck {
    #[default]
    None,
    UpToDate,
    Failed,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Model {
    phase: Phase,
    install: Option<InstallKind>,
    last_check: LastCheck,
}

impl Model {
    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    pub fn set_install_kind(&mut self, install: Option<InstallKind>) {
        self.install = install;
    }

    /// Start a check unless one is running or a release is already known.
    pub fn begin_check(&mut self) -> bool {
        if self.phase != Phase::Idle {
            return false;
        }
        self.phase = Phase::Checking;
        true
    }

    /// Record a check result; only a release newer than `current` is offered.
    /// Errors are not shown: the next check simply tries again.
    pub fn finish_check(&mut self, result: Result<Release, String>, current: &Version) {
        if self.phase != Phase::Checking {
            return;
        }
        (self.phase, self.last_check) = match result {
            Ok(release) if release.version > *current => {
                (Phase::Available(release), LastCheck::UpToDate)
            }
            Ok(_) => (Phase::Idle, LastCheck::UpToDate),
            Err(_) => (Phase::Idle, LastCheck::Failed),
        };
    }

    pub fn about(&self, current: &Version) -> About {
        let version = format!("Loog {current}");
        if self.install.is_none() {
            return About {
                detail: format!("{version} · Updates are off in this build"),
                action: None,
            };
        }
        let check = |status: &str| About {
            detail: if status.is_empty() {
                version.clone()
            } else {
                format!("{version} · {status}")
            },
            action: Some(Badge {
                label: "Check now".to_owned(),
                actionable: true,
            }),
        };
        match &self.phase {
            Phase::Idle => check(match self.last_check {
                LastCheck::None => "",
                LastCheck::UpToDate => "Up to date",
                LastCheck::Failed => "Could not check for updates",
            }),
            Phase::Checking => About {
                detail: format!("{version} · Checking for updates…"),
                action: None,
            },
            Phase::Available(release)
            | Phase::Downloading { release, .. }
            | Phase::Ready { release, .. } => About {
                detail: format!("{version} · {} is available", release.version),
                action: self.badge(),
            },
        }
    }

    /// The asset to download for an in-place update, if this install has one.
    fn installer(&self, release: &Release) -> Option<Asset> {
        let key = self.install?.installer_asset()?;
        release.assets.get(key).cloned()
    }

    pub fn badge(&self) -> Option<Badge> {
        let (label, actionable) = match &self.phase {
            Phase::Idle | Phase::Checking => return None,
            Phase::Available(release) if self.installer(release).is_some() => {
                (format!("Update to {}", release.version), true)
            }
            Phase::Available(release) => (format!("Loog {} available", release.version), true),
            Phase::Downloading { release, received } => {
                let total = self.installer(release).map_or(0, |asset| asset.size);
                let percent = (*received * 100).checked_div(total).unwrap_or(0).min(100);
                (format!("Downloading update {percent}%"), false)
            }
            Phase::Ready { .. } => ("Restart to update".to_owned(), true),
        };
        Some(Badge { label, actionable })
    }

    pub fn indicator(&self) -> Option<Indicator> {
        let badge = self.badge()?;
        let (stage, text) = match &self.phase {
            Phase::Idle | Phase::Checking => return None,
            Phase::Available(release) => (Stage::Available, release.version.to_string()),
            Phase::Downloading { release, received } => {
                let total = self.installer(release).map_or(0, |asset| asset.size);
                let percent = (*received * 100).checked_div(total).unwrap_or(0).min(100);
                (Stage::Downloading(percent as u8), format!("{percent}%"))
            }
            Phase::Ready { .. } => (Stage::Ready, "Restart to update".to_owned()),
        };
        Some(Indicator {
            stage,
            text,
            label: badge.label,
        })
    }

    /// Advance the phase for a click on the badge or the Settings action
    /// and say what to do.
    pub fn click(&mut self) -> Option<Intent> {
        match &self.phase {
            Phase::Idle if self.install.is_some() => {
                self.phase = Phase::Checking;
                Some(Intent::Check)
            }
            Phase::Available(release) => match self.installer(release) {
                Some(asset) => {
                    self.phase = Phase::Downloading {
                        release: release.clone(),
                        received: 0,
                    };
                    Some(Intent::Download(asset))
                }
                None => Some(Intent::OpenPage(release.notes_url.clone())),
            },
            // Stays ready: if the restart is cancelled (for example from the
            // unsaved-changes dialog) the button still works next time.
            Phase::Ready { installer, .. } => Some(Intent::Install(installer.clone())),
            _ => None,
        }
    }

    pub fn download_progress(&mut self, bytes: u64) -> bool {
        match &mut self.phase {
            Phase::Downloading { received, .. } if *received != bytes => {
                *received = bytes;
                true
            }
            _ => false,
        }
    }

    /// Record the download result; a failure offers the release again.
    pub fn finish_download(&mut self, result: Result<PathBuf, String>) -> Result<(), String> {
        let Phase::Downloading { release, .. } = &self.phase else {
            return Ok(());
        };
        let release = release.clone();
        match result {
            Ok(installer) => {
                self.phase = Phase::Ready { release, installer };
                Ok(())
            }
            Err(error) => {
                self.phase = Phase::Available(release);
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn release(version: &str) -> Release {
        let mut assets = BTreeMap::new();
        assets.insert(
            "setup".to_owned(),
            Asset {
                name: "setup.exe".into(),
                url: "https://example.com/setup.exe".into(),
                sha256: "0".repeat(64),
                size: 200,
            },
        );
        Release {
            version: Version::parse(version).unwrap(),
            notes_url: "https://example.com/notes".into(),
            assets,
        }
    }

    fn checked(install: InstallKind, version: &str) -> Model {
        let mut model = Model::default();
        model.set_install_kind(Some(install));
        assert!(model.begin_check());
        assert!(!model.begin_check());
        model.finish_check(Ok(release(version)), &Version::parse("0.1.0").unwrap());
        model
    }

    #[test]
    fn only_newer_releases_are_offered() {
        assert_eq!(
            checked(InstallKind::WindowsSetup, "0.1.0").phase(),
            &Phase::Idle
        );
        assert_eq!(
            checked(InstallKind::WindowsSetup, "0.1.0-rc.1").phase(),
            &Phase::Idle
        );
        let mut failed = Model::default();
        failed.begin_check();
        failed.finish_check(Err("offline".into()), &Version::parse("0.1.0").unwrap());
        assert_eq!(failed.phase(), &Phase::Idle);
        assert!(failed.badge().is_none());
    }

    #[test]
    fn setup_installs_download_then_restart() {
        let mut model = checked(InstallKind::WindowsSetup, "0.2.0");
        assert_eq!(model.badge().unwrap().label, "Update to 0.2.0");
        let indicator = model.indicator().unwrap();
        assert_eq!(
            (indicator.stage, indicator.text.as_str()),
            (Stage::Available, "0.2.0")
        );
        assert_eq!(indicator.label, "Update to 0.2.0");
        assert!(matches!(model.click(), Some(Intent::Download(asset)) if asset.size == 200));
        assert!(model.download_progress(50));
        assert!(!model.download_progress(50));
        assert_eq!(model.badge().unwrap().label, "Downloading update 25%");
        let indicator = model.indicator().unwrap();
        assert_eq!(
            (indicator.stage, indicator.text.as_str()),
            (Stage::Downloading(25), "25%")
        );
        assert!(!model.badge().unwrap().actionable);
        assert_eq!(model.click(), None);

        model.finish_download(Ok("setup.exe".into())).unwrap();
        assert_eq!(model.badge().unwrap().label, "Restart to update");
        assert_eq!(model.indicator().unwrap().stage, Stage::Ready);
        assert_eq!(model.click(), Some(Intent::Install("setup.exe".into())));
        assert_eq!(model.badge().unwrap().label, "Restart to update");
    }

    #[test]
    fn failed_downloads_offer_the_release_again() {
        let mut model = checked(InstallKind::WindowsSetup, "0.2.0");
        model.click();
        assert_eq!(
            model.finish_download(Err("checksum".into())),
            Err("checksum".into())
        );
        assert_eq!(model.badge().unwrap().label, "Update to 0.2.0");
    }

    #[test]
    fn about_reports_the_version_and_check_results() {
        let current = Version::parse("0.1.0").unwrap();
        let mut model = Model::default();
        let about = model.about(&current);
        assert_eq!(about.detail, "Loog 0.1.0 · Updates are off in this build");
        assert_eq!(about.action, None);
        assert_eq!(model.click(), None, "builds without updates never check");

        model.set_install_kind(Some(InstallKind::WindowsPortable));
        assert_eq!(model.about(&current).detail, "Loog 0.1.0");
        assert_eq!(model.about(&current).action.unwrap().label, "Check now");
        assert_eq!(model.click(), Some(Intent::Check));
        assert_eq!(
            model.about(&current).detail,
            "Loog 0.1.0 · Checking for updates…"
        );
        assert_eq!(model.about(&current).action, None);

        model.finish_check(Err("offline".into()), &current);
        assert_eq!(
            model.about(&current).detail,
            "Loog 0.1.0 · Could not check for updates"
        );
        model.click();
        model.finish_check(Ok(release("0.1.0")), &current);
        assert_eq!(model.about(&current).detail, "Loog 0.1.0 · Up to date");
        model.click();
        model.finish_check(Ok(release("0.2.0")), &current);
        let about = model.about(&current);
        assert_eq!(about.detail, "Loog 0.1.0 · 0.2.0 is available");
        assert_eq!(about.action.unwrap().label, "Loog 0.2.0 available");
    }

    #[test]
    fn other_installs_open_the_release_page() {
        let mut model = checked(InstallKind::LinuxDeb, "0.2.0");
        assert_eq!(model.badge().unwrap().label, "Loog 0.2.0 available");
        assert_eq!(
            model.click(),
            Some(Intent::OpenPage("https://example.com/notes".into()))
        );
        assert!(matches!(model.phase(), Phase::Available(_)));
    }
}
