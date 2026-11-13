//! Workspace trust and package grants (design §9, §9.1).
//!
//! Trust answers "may project content take part in executable workflows";
//! a grant answers "may this verified package use this capability". Both are
//! needed for project-driven execution and neither substitutes for the other.
//! There is no third layer: a folder is decided once (trusted or declined) when
//! it is opened, and that decision is the only per-folder check.
use std::collections::BTreeSet;

/// A verified package: the self-declared id plus a content digest. A changed
/// digest is a different identity, so grants never follow a swapped package.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PackageIdentity {
    pub id: String,
    pub digest: String,
}

/// A root identity that survives remounts (verified by the caller), as
/// opposed to the runtime `RootId`.
pub type StableRoot = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootTrust {
    Trusted,
    /// New, nested or unverified roots start here.
    Untrusted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Denial {
    PackageNotGranted,
    RootNotTrusted,
    /// The operation needs a target root and none was given.
    NoTargetRoot,
}

/// What the user has said about a folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Trusted,
    /// The user was asked and said no; the answer is remembered, so no asking again.
    Declined,
    /// Not asked yet.
    Undecided,
}

/// Workspace roots the user has decided on (design §9.1).
///
/// Roots start untrusted; trust is never inferred from a parent or sibling
/// folder, so a folder is trusted only when it is under a root that was
/// explicitly trusted. A declined root stays untrusted and is not asked about again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrustedRoots {
    roots: BTreeSet<std::path::PathBuf>,
    declined: BTreeSet<std::path::PathBuf>,
    resources: BTreeSet<loog_workspace_api::ResourceRef>,
    declined_resources: BTreeSet<loog_workspace_api::ResourceRef>,
}

impl TrustedRoots {
    /// Provider identities are exact; host platform path rules never apply.
    pub fn decide_resource(&mut self, root: &loog_workspace_api::ResourceRef, trusted: bool) {
        if root.environment().is_none() {
            return;
        }
        if trusted {
            self.declined_resources.remove(root);
            self.resources.insert(root.clone());
        } else {
            self.resources.remove(root);
            self.declined_resources.insert(root.clone());
        }
    }

    pub fn resource_decision(&self, root: &loog_workspace_api::ResourceRef) -> Decision {
        if self.resources.contains(root) {
            Decision::Trusted
        } else if self.declined_resources.contains(root) {
            Decision::Declined
        } else {
            Decision::Undecided
        }
    }

    pub fn trust(&mut self, root: &std::path::Path) -> bool {
        let was_declined = self.declined.remove(root);
        self.roots.insert(root.to_path_buf()) || was_declined
    }

    /// Remember a "no". A root that was trusted stops being trusted.
    pub fn decline(&mut self, root: &std::path::Path) -> bool {
        let was_trusted = self.roots.remove(root);
        self.declined.insert(root.to_path_buf()) || was_trusted
    }

    /// Stop trusting `root` without remembering a "no".
    pub fn revoke(&mut self, root: &std::path::Path) -> bool {
        self.roots.remove(root)
    }

    /// The decision for the open folder `path`, given the open `workspace` folders.
    pub fn decision_within(
        &self,
        path: &std::path::Path,
        workspace: &[std::path::PathBuf],
    ) -> Decision {
        if self.is_trusted_within(path, workspace) {
            Decision::Trusted
        } else if self.declined.contains(path) {
            Decision::Declined
        } else {
            Decision::Undecided
        }
    }

    /// Whether `path` is inside a trusted root (component-wise, so
    /// `/work/app2` is not covered by a trust on `/work/app`).
    pub fn is_trusted(&self, path: &std::path::Path) -> bool {
        self.roots.iter().any(|root| path.starts_with(root))
    }

    /// Whether `path` is trusted when the open workspace folders are `workspace`.
    ///
    /// The innermost open folder that holds `path` decides. Trusting a folder
    /// that is itself open covers that folder only: a deeper open folder does not
    /// inherit it (design §9.1), so adding one never widens what runs. Trusting a
    /// folder that is not open (a parent) covers every open folder below it. A
    /// path outside every open folder falls back to [`Self::is_trusted`].
    pub fn is_trusted_within(
        &self,
        path: &std::path::Path,
        workspace: &[std::path::PathBuf],
    ) -> bool {
        let innermost = workspace
            .iter()
            .filter(|root| path.starts_with(root))
            .max_by_key(|root| root.components().count());
        let Some(root) = innermost else {
            return self.is_trusted(path);
        };
        self.roots.iter().any(|trusted| {
            root.starts_with(trusted) && (trusted == root || !workspace.contains(trusted))
        })
    }

    pub fn trust_of(&self, path: &std::path::Path) -> RootTrust {
        if self.is_trusted(path) {
            RootTrust::Trusted
        } else {
            RootTrust::Untrusted
        }
    }

    pub fn roots(&self) -> impl Iterator<Item = &std::path::Path> {
        self.roots.iter().map(std::path::PathBuf::as_path)
    }

    pub fn to_json(&self) -> String {
        let list = |roots: &BTreeSet<std::path::PathBuf>| -> Vec<String> {
            roots
                .iter()
                .map(|root| root.to_string_lossy().into_owned())
                .collect()
        };
        serde_json::json!({ "trusted": list(&self.roots), "declined": list(&self.declined),
            "resource_trusted": self.resources, "resource_declined": self.declined_resources })
        .to_string()
    }

    /// Tolerant of a missing or damaged file: that means nothing is trusted.
    pub fn from_json(text: &str) -> Self {
        let value = serde_json::from_str::<serde_json::Value>(text).ok();
        let read = |key: &str| -> BTreeSet<std::path::PathBuf> {
            value
                .as_ref()
                .and_then(|value| value.get(key))
                .and_then(|list| list.as_array())
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .filter_map(|entry| entry.as_str().map(std::path::PathBuf::from))
                .filter(|path| path.is_absolute())
                .collect()
        };
        let roots = read("trusted");
        let declined = read("declined")
            .into_iter()
            .filter(|path| !roots.contains(path))
            .collect();
        // COMPAT(local-only-trust-file): Existing files have only local lists.
        // Missing resource lists stay undecided while exact resource identities
        // are added. Remove with the old format; no removal version is scheduled.
        let read_resources = |key: &str| -> BTreeSet<loog_workspace_api::ResourceRef> {
            value
                .as_ref()
                .and_then(|value| value.get(key))
                .and_then(|list| list.as_array())
                .filter(|list| list.len() <= 4096)
                .into_iter()
                .flatten()
                .filter_map(|entry| {
                    serde_json::from_value::<loog_workspace_api::ResourceRef>(entry.clone()).ok()
                })
                .filter(|root| root.environment().is_some())
                .collect()
        };
        let resources = read_resources("resource_trusted");
        let declined_resources = read_resources("resource_declined")
            .into_iter()
            .filter(|root| !resources.contains(root))
            .collect();
        Self {
            roots,
            declined,
            resources,
            declined_resources,
        }
    }
}

/// Capabilities that run programs on the machine need root-level approval.
pub fn needs_root(capability: &str) -> bool {
    capability == "process.execute-local"
}

#[derive(Default)]
pub struct Grants {
    package: BTreeSet<(PackageIdentity, String)>,
}

impl Grants {
    pub fn grant_package(&mut self, package: &PackageIdentity, capability: &str) {
        self.package
            .insert((package.clone(), capability.to_owned()));
    }

    /// Effective permission is the intersection of every applicable layer.
    pub fn authorize(
        &self,
        package: &PackageIdentity,
        capability: &str,
        target: Option<(&str, RootTrust)>,
    ) -> Result<(), Denial> {
        if !self
            .package
            .contains(&(package.clone(), capability.to_owned()))
        {
            return Err(Denial::PackageNotGranted);
        }
        if !needs_root(capability) {
            return Ok(());
        }
        let (_, trust) = target.ok_or(Denial::NoTargetRoot)?;
        if trust != RootTrust::Trusted {
            return Err(Denial::RootNotTrusted);
        }
        Ok(())
    }
}

/// Project settings fields that decide what gets executed. In an untrusted
/// root they are shown as pending information, never used as an execution basis.
pub fn is_execution_sensitive(field: &str) -> bool {
    const SENSITIVE: &[&str] = &[
        "command", "args", "env", "shell", "program", "path", "install", "download",
    ];
    let leaf = field.rsplit('.').next().unwrap_or(field);
    SENSITIVE.contains(&leaf)
}

/// Whether a project-provided value may be used as-is.
pub fn project_value_usable(field: &str, trust: RootTrust) -> bool {
    trust == RootTrust::Trusted || !is_execution_sensitive(field)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(digest: &str) -> PackageIdentity {
        PackageIdentity {
            id: "example.check".into(),
            digest: digest.into(),
        }
    }

    const EXEC: &str = "process.execute-local";

    #[test]
    fn resource_trust_is_persisted_by_exact_environment_and_root_identity() {
        use loog_workspace_api::{EnvironmentKey, ResourceRef};
        let resource = |instance, path| {
            ResourceRef::provided(
                EnvironmentKey::new("example.provider", instance).unwrap(),
                path,
            )
            .unwrap()
        };
        let root = resource("one", "/project");
        let child = resource("one", "/project/nested");
        let other = resource("two", "/project");
        let mut roots = TrustedRoots::default();
        roots.decide_resource(&root, true);
        assert_eq!(roots.resource_decision(&root), Decision::Trusted);
        assert_eq!(roots.resource_decision(&child), Decision::Undecided);
        assert_eq!(roots.resource_decision(&other), Decision::Undecided);
        assert!(!roots.is_trusted(std::path::Path::new("/project")));
        roots.decide_resource(&child, false);
        assert_eq!(TrustedRoots::from_json(&roots.to_json()), roots);
        roots.decide_resource(&root, false);
        assert_eq!(roots.resource_decision(&root), Decision::Declined);
        assert_eq!(TrustedRoots::from_json(&roots.to_json()), roots);
        assert_eq!(
            TrustedRoots::from_json(r#"{"trusted":[],"declined":[]}"#).resource_decision(&root),
            Decision::Undecided
        );
    }

    #[test]
    fn execution_needs_the_package_grant_and_a_trusted_root_and_nothing_more() {
        let mut grants = Grants::default();
        let p = pkg("sha256:a");
        assert_eq!(
            grants.authorize(&p, EXEC, Some(("A", RootTrust::Trusted))),
            Err(Denial::PackageNotGranted)
        );
        grants.grant_package(&p, EXEC);
        assert_eq!(grants.authorize(&p, EXEC, None), Err(Denial::NoTargetRoot));
        assert_eq!(
            grants.authorize(&p, EXEC, Some(("A", RootTrust::Untrusted))),
            Err(Denial::RootNotTrusted)
        );
        assert_eq!(
            grants.authorize(&p, EXEC, Some(("A", RootTrust::Trusted))),
            Ok(())
        );
    }

    #[test]
    fn a_declined_folder_is_remembered_and_trusting_it_later_replaces_the_no() {
        let root = std::env::temp_dir().join("trust-declined");
        let open = vec![root.clone()];
        let mut roots = TrustedRoots::default();
        assert_eq!(roots.decision_within(&root, &open), Decision::Undecided);
        roots.decline(&root);
        assert_eq!(roots.decision_within(&root, &open), Decision::Declined);
        assert!(!roots.is_trusted_within(&root, &open));
        assert_eq!(TrustedRoots::from_json(&roots.to_json()), roots);
        roots.trust(&root);
        assert_eq!(roots.decision_within(&root, &open), Decision::Trusted);
        // Declining a trusted folder takes the trust away.
        roots.decline(&root);
        assert_eq!(roots.decision_within(&root, &open), Decision::Declined);
        // A file that lists a folder as both keeps the trust.
        let both = format!(
            r#"{{"trusted":[{0}],"declined":[{0}]}}"#,
            serde_json::to_string(&root.to_string_lossy()).unwrap()
        );
        assert_eq!(
            TrustedRoots::from_json(&both).decision_within(&root, &open),
            Decision::Trusted
        );
    }

    #[test]
    fn the_innermost_open_folder_decides_and_a_deeper_folder_inherits_nothing() {
        use std::path::PathBuf;
        let outer = PathBuf::from(if cfg!(windows) { "C:/w" } else { "/w" });
        let inner = outer.join("app");
        let open = vec![outer.clone(), inner.clone()];
        let mut trusted = TrustedRoots::default();
        trusted.trust(&outer);
        // Trusting the outer open folder covers it and its plain subfolders only.
        assert!(trusted.is_trusted_within(&outer.join("src"), &open));
        assert!(!trusted.is_trusted_within(&inner, &open));
        assert!(!trusted.is_trusted_within(&inner.join("src"), &open));
        // The deeper folder needs its own decision, which does not widen the outer one.
        trusted.trust(&inner);
        assert!(trusted.is_trusted_within(&inner.join("src"), &open));
        trusted.revoke(&outer);
        assert!(!trusted.is_trusted_within(&outer, &open));
        assert!(trusted.is_trusted_within(&inner, &open));

        // A trusted folder that is not open covers every open folder below it.
        let mut parent = TrustedRoots::default();
        parent.trust(&outer);
        let below = vec![inner.clone(), outer.join("lib")];
        assert!(parent.is_trusted_within(&inner, &below));
        assert!(parent.is_trusted_within(&outer.join("lib").join("x"), &below));
        // Outside every open folder the plain rule applies.
        assert!(parent.is_trusted_within(&outer.join("other"), &below));
        assert!(!parent.is_trusted_within(
            &PathBuf::from(if cfg!(windows) { "C:/z" } else { "/z" }),
            &below
        ));
    }

    #[test]
    fn a_changed_digest_loses_every_grant() {
        let mut grants = Grants::default();
        grants.grant_package(&pkg("sha256:a"), "documents.read");
        assert_eq!(
            grants.authorize(&pkg("sha256:b"), "documents.read", None),
            Err(Denial::PackageNotGranted)
        );
    }

    #[test]
    fn non_execution_capabilities_need_only_the_package_grant() {
        let mut grants = Grants::default();
        let p = pkg("sha256:a");
        grants.grant_package(&p, "documents.read");
        assert_eq!(
            grants.authorize(&p, "documents.read", Some(("A", RootTrust::Untrusted))),
            Ok(())
        );
    }

    #[test]
    fn trust_covers_a_root_and_what_is_under_it_but_not_its_siblings() {
        let base = std::env::temp_dir();
        let (app, sibling, nested) = (
            base.join("work/app"),
            base.join("work/app2"),
            base.join("work/app/crates/a"),
        );
        let mut roots = TrustedRoots::default();
        assert!(!roots.is_trusted(&app), "new roots start untrusted");
        roots.trust(&app);
        assert!(roots.is_trusted(&app) && roots.is_trusted(&nested));
        assert!(
            !roots.is_trusted(&sibling),
            "component-wise, not a string prefix"
        );
        assert!(
            !roots.is_trusted(&base.join("work")),
            "trust never flows upward"
        );
        roots.revoke(&app);
        assert!(!roots.is_trusted(&nested));
    }

    #[test]
    fn the_trust_file_round_trips_and_a_damaged_one_trusts_nothing() {
        let root = std::env::temp_dir().join("trusted-root");
        let mut roots = TrustedRoots::default();
        roots.trust(&root);
        assert_eq!(TrustedRoots::from_json(&roots.to_json()), roots);
        assert_eq!(TrustedRoots::from_json("not json"), TrustedRoots::default());
        assert_eq!(
            TrustedRoots::from_json(r#"{"trusted":["relative/path",3]}"#),
            TrustedRoots::default()
        );
    }

    #[test]
    fn execution_sensitive_project_fields_are_pending_in_untrusted_roots() {
        assert!(!project_value_usable(
            "lsp.rust.command",
            RootTrust::Untrusted
        ));
        assert!(!project_value_usable(
            "tasks.check.args",
            RootTrust::Untrusted
        ));
        assert!(project_value_usable("editor.tabSize", RootTrust::Untrusted));
        assert!(project_value_usable("lsp.rust.command", RootTrust::Trusted));
    }
}
