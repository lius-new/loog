//! The extension registry index: what the published index says and the rules for
//! trusting and using it. No file system, network or signature code lives here;
//! `infrastructure::extensions::registry` fetches, verifies and caches.
//!
//! The index lists every published extension and, for each version, the package
//! per platform. It is signed as a whole (the signature is checked before this
//! module sees it) and every package is checked against the SHA-256 it lists.
use super::manifest::valid_id;
use super::range::{Range, parse_version};
use serde::Deserialize;

pub const SCHEMA: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Index {
    pub schema: u32,
    pub generated: String,
    pub expires: String,
    /// Grows with every published index; a lower one than already seen is refused.
    pub sequence: u64,
    pub key_id: String,
    pub extensions: Vec<Entry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Entry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub publisher: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub license: String,
    /// What a language extension recognizes, so a file's extension can be matched
    /// to it without downloading anything.
    #[serde(default)]
    pub languages: Vec<LanguageInfo>,
    /// The languages a language server extension serves.
    #[serde(default)]
    pub serves: Vec<String>,
    /// What the list shows before anything is installed. All optional: an index
    /// published without them still works.
    #[serde(default)]
    pub icon: Option<Artwork>,
    #[serde(default)]
    pub readme: Option<String>,
    #[serde(default)]
    pub changelog: Option<String>,
    /// Newest first.
    pub versions: Vec<VersionEntry>,
}

/// An icon carried by the index: SVG source as text, or PNG as base64.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Artwork {
    pub format: String,
    pub data: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LanguageInfo {
    pub name: String,
    #[serde(default)]
    pub file_types: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct VersionEntry {
    pub version: String,
    #[serde(default)]
    pub engines: Option<super::manifest::Engines>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub recommends: Vec<String>,
    pub artifacts: Vec<Artifact>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Artifact {
    /// `any`, or `<os>-<arch>` such as `windows-x86_64`.
    pub platform: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

/// Seconds since 1970-01-01 of an RFC 3339 time in UTC, as the index writes them
/// (`2026-11-01T00:00:00.000Z`).
pub fn parse_time(text: &str) -> Option<i64> {
    let text = text.strip_suffix('Z')?;
    let (date, time) = text.split_once('T')?;
    let number = |part: &str| part.parse::<i64>().ok();
    let mut date = date.split('-');
    let (year, month, day) = (
        number(date.next()?)?,
        number(date.next()?)?,
        number(date.next()?)?,
    );
    let mut time = time.split(':');
    let (hour, minute) = (number(time.next()?)?, number(time.next()?)?);
    let seconds = time.next()?;
    let second = number(seconds.split('.').next()?)?;
    if date.next().is_some()
        || time.next().is_some()
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..24).contains(&hour)
        || !(0..60).contains(&minute)
        || !(0..=60).contains(&second)
    {
        return None;
    }
    // Days since the epoch of a proleptic Gregorian date.
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let shifted = (month + 9) % 12;
    let day_of_year = (153 * shifted + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

impl Index {
    /// Read an index whose signature has already been checked. Everything in it is
    /// checked again here, because a signed file can still be malformed.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let index: Index = serde_json::from_slice(bytes)
            .map_err(|error| format!("the index is not valid: {error}"))?;
        index.validate()?;
        Ok(index)
    }

    fn validate(&self) -> Result<(), String> {
        let bad = |message: String| Err(format!("the index is not valid: {message}"));
        if self.schema != SCHEMA {
            return bad(format!("unsupported schema {}", self.schema));
        }
        if self.sequence == 0 {
            return bad("sequence must be a positive integer".into());
        }
        match (parse_time(&self.generated), parse_time(&self.expires)) {
            (Some(generated), Some(expires)) if expires > generated => {}
            _ => {
                return bad(
                    "generated and expires must be times, expiring after generation".into(),
                );
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        for entry in &self.extensions {
            if !valid_id(&entry.id) || !seen.insert(entry.id.as_str()) {
                return bad(format!("invalid or repeated extension id {:?}", entry.id));
            }
            if entry.versions.is_empty() {
                return bad(format!("{} has no versions", entry.id));
            }
            for version in &entry.versions {
                let here = |message: &str| format!("{} {}: {message}", entry.id, version.version);
                if parse_version(&version.version).is_none() {
                    return bad(here("invalid version"));
                }
                if version
                    .engines
                    .as_ref()
                    .is_some_and(|engines| Range::parse(&engines.loom).is_none())
                {
                    return bad(here("invalid engines.loom"));
                }
                if version.artifacts.is_empty() {
                    return bad(here("no packages"));
                }
                let mut platforms = std::collections::BTreeSet::new();
                for artifact in &version.artifacts {
                    let good_hash = artifact.sha256.len() == 64
                        && artifact
                            .sha256
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
                    if !valid_platform(&artifact.platform)
                        || !platforms.insert(artifact.platform.as_str())
                        || !good_hash
                        || artifact.size == 0
                        || !artifact.url.starts_with("https://")
                    {
                        return bad(here(&format!("invalid package for {}", artifact.platform)));
                    }
                }
                if platforms.contains("any") && platforms.len() > 1 {
                    return bad(here("a version is for any platform or for named ones"));
                }
            }
        }
        Ok(())
    }

    /// Whether the index is past the time it says it is good until.
    pub fn is_expired(&self, now: i64) -> bool {
        parse_time(&self.expires).is_none_or(|expires| now >= expires)
    }

    pub fn entry(&self, id: &str) -> Option<&Entry> {
        self.extensions.iter().find(|entry| entry.id == id)
    }

    /// The extensions that recognize files named with `file_type`: a suffix such as
    /// `rs` or `d.ts`, without the dot.
    pub fn for_file_type(&self, file_type: &str) -> Vec<&Entry> {
        self.extensions
            .iter()
            .filter(|entry| {
                entry.languages.iter().any(|language| {
                    language.file_types.iter().any(|known| {
                        known
                            .trim_start_matches('.')
                            .eq_ignore_ascii_case(file_type)
                    })
                })
            })
            .collect()
    }
}

/// `any`, or an `<os>-<arch>` pair of the platforms Loom builds for.
pub fn valid_platform(platform: &str) -> bool {
    matches!(
        platform,
        "any"
            | "windows-x86_64"
            | "windows-aarch64"
            | "linux-x86_64"
            | "linux-aarch64"
            | "macos-x86_64"
            | "macos-aarch64"
    )
}

/// The platform of the running process. A process under translation reports the
/// architecture it was built for, which is the one its packages must match.
pub fn current_platform() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "windows-x86_64",
        ("windows", "aarch64") => "windows-aarch64",
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "aarch64") => "linux-aarch64",
        ("macos", "x86_64") => "macos-x86_64",
        ("macos", "aarch64") => "macos-aarch64",
        _ => return None,
    })
}

impl Entry {
    /// The newest version a Loom of `loom_version` can run, with the package for
    /// `platform` (or the one for any platform).
    pub fn best_for(
        &self,
        loom_version: &str,
        platform: &str,
    ) -> Option<(&VersionEntry, &Artifact)> {
        self.best_matching(loom_version, platform, |_| true)
    }

    /// [`Self::best_for`] among the versions `accept` allows.
    pub fn best_matching(
        &self,
        loom_version: &str,
        platform: &str,
        accept: impl Fn(&VersionEntry) -> bool,
    ) -> Option<(&VersionEntry, &Artifact)> {
        let loom = parse_version(loom_version)?;
        self.versions
            .iter()
            .filter(|version| accept(version))
            .filter(|version| {
                version.engines.as_ref().is_none_or(|engines| {
                    Range::parse(&engines.loom).is_some_and(|range| range.matches(loom))
                })
            })
            .filter_map(|version| {
                let artifact = version
                    .artifacts
                    .iter()
                    .find(|a| a.platform == platform)
                    .or_else(|| version.artifacts.iter().find(|a| a.platform == "any"))?;
                Some((version, artifact))
            })
            // The file says newest first; do not depend on it.
            .max_by_key(|(version, _)| parse_version(&version.version))
    }
}

/// What is installed of one extension, as far as planning needs to know.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    pub version: String,
    /// `id@range` entries from its manifest.
    pub requires: Vec<String>,
    /// Kept at this version: no update is offered.
    pub pinned: bool,
}

/// One package to fetch and install.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub id: String,
    pub version: String,
    pub artifact: Artifact,
}

const MAX_DEPTH: usize = 16;

fn satisfies(range: Option<&Range>, version: &str) -> bool {
    range.is_none_or(|range| parse_version(version).is_some_and(|v| range.matches(v)))
}

/// What to install so that `id` is at its newest version this Loom can run, with
/// everything it `requires` that is not installed (or not at an acceptable version).
/// The steps are in the order to install them: a requirement before what needs it.
/// An empty plan means there is nothing to do. Nothing is planned that would leave an
/// installed extension without a version of something it requires.
pub fn plan_install(
    index: &Index,
    installed: &std::collections::BTreeMap<String, Installed>,
    id: &str,
    loom_version: &str,
    platform: &str,
) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    plan_one(
        index,
        installed,
        (loom_version, platform),
        id,
        None,
        &mut steps,
        &mut Vec::new(),
    )?;
    for step in &steps {
        let Some(version) = parse_version(&step.version) else {
            continue;
        };
        for (other, info) in installed {
            if steps.iter().any(|planned| planned.id == *other) {
                continue;
            }
            for requirement in &info.requires {
                if let Some((required, range)) = super::manifest::parse_requirement(requirement)
                    && required == step.id
                    && !range.matches(version)
                {
                    return Err(format!(
                        "{other} needs {requirement}, which {} {} would break",
                        step.id, step.version
                    ));
                }
            }
        }
    }
    Ok(steps)
}

fn plan_one(
    index: &Index,
    installed: &std::collections::BTreeMap<String, Installed>,
    (loom_version, platform): (&str, &str),
    id: &str,
    range: Option<&Range>,
    steps: &mut Vec<Step>,
    path: &mut Vec<String>,
) -> Result<(), String> {
    let wanted = range.is_some();
    if wanted
        && let Some(have) = installed.get(id)
        && satisfies(range, &have.version)
    {
        return Ok(());
    }
    if let Some(planned) = steps.iter().find(|step| step.id == id) {
        return if satisfies(range, &planned.version) {
            Ok(())
        } else {
            Err(format!(
                "{id} {} does not satisfy what another extension needs",
                planned.version
            ))
        };
    }
    if path.iter().any(|known| known == id) || path.len() >= MAX_DEPTH {
        return Err(format!("{id} requires itself ({})", path.join(" -> ")));
    }
    let entry = index
        .entry(id)
        .ok_or_else(|| format!("{id} is not in the registry"))?;
    // What is installed and needs `id` keeps working: only versions it accepts.
    let limits = limits_on(installed, id);
    let (version, artifact) = entry
        .best_matching(loom_version, platform, |v| {
            satisfies(range, &v.version) && limits.iter().all(|l| satisfies(Some(l), &v.version))
        })
        .ok_or_else(|| {
            format!("no version of {id} fits Loom {loom_version} on {platform} and what needs it")
        })?;
    path.push(id.to_owned());
    for requirement in &version.requires {
        let (required, needed) =
            super::manifest::parse_requirement(requirement).ok_or_else(|| {
                format!(
                    "{id} {}: invalid requirement {requirement}",
                    version.version
                )
            })?;
        plan_one(
            index,
            installed,
            (loom_version, platform),
            required,
            Some(&needed),
            steps,
            path,
        )?;
    }
    path.pop();
    // The one asked for, already at this version, needs nothing more.
    if !wanted
        && installed
            .get(id)
            .is_some_and(|have| have.version == version.version)
    {
        return Ok(());
    }
    steps.push(Step {
        id: id.to_owned(),
        version: version.version.clone(),
        artifact: artifact.clone(),
    });
    Ok(())
}

/// The version ranges the installed extensions put on `id`.
fn limits_on(installed: &std::collections::BTreeMap<String, Installed>, id: &str) -> Vec<Range> {
    installed
        .iter()
        .filter(|(other, _)| other.as_str() != id)
        .flat_map(|(_, have)| &have.requires)
        .filter_map(|requirement| super::manifest::parse_requirement(requirement))
        .filter(|(required, _)| *required == id)
        .map(|(_, range)| range)
        .collect()
}

/// The installed extensions the index has a newer version of, as `(id, from, to)`.
/// Pinned ones are left out.
pub fn updates(
    index: &Index,
    installed: &std::collections::BTreeMap<String, Installed>,
    loom_version: &str,
    platform: &str,
) -> Vec<(String, String, String)> {
    installed
        .iter()
        .filter(|(_, have)| !have.pinned)
        .filter_map(|(id, have)| {
            let limits = limits_on(installed, id);
            let (best, _) = index
                .entry(id)?
                .best_matching(loom_version, platform, |v| {
                    limits.iter().all(|l| satisfies(Some(l), &v.version))
                })?;
            (parse_version(&best.version)? > parse_version(&have.version)?)
                .then(|| (id.clone(), have.version.clone(), best.version.clone()))
        })
        .collect()
}

/// The installed extensions that require `id`.
pub fn dependents(
    installed: &std::collections::BTreeMap<String, Installed>,
    id: &str,
) -> Vec<String> {
    installed
        .iter()
        .filter(|(other, _)| other.as_str() != id)
        .filter(|(_, have)| {
            have.requires.iter().any(|requirement| {
                super::manifest::parse_requirement(requirement)
                    .is_some_and(|(required, _)| required == id)
            })
        })
        .map(|(other, _)| other.clone())
        .collect()
}

/// The runtimes `removing` requires that nothing else installed requires: they can
/// be offered for removal along with it.
pub fn orphaned_runtimes(
    installed: &std::collections::BTreeMap<String, Installed>,
    removing: &str,
) -> Vec<String> {
    let Some(have) = installed.get(removing) else {
        return Vec::new();
    };
    have.requires
        .iter()
        .filter_map(|requirement| super::manifest::parse_requirement(requirement))
        .map(|(required, _)| required)
        .filter(|required| {
            required.starts_with("loom.runtime.") && installed.contains_key(*required)
        })
        .filter(|required| {
            dependents(installed, required)
                .iter()
                .all(|other| other == removing)
        })
        .map(str::to_owned)
        .collect()
}

/// Whether `new` may replace what was already seen: an index older than one seen
/// before is a replay of a superseded file.
pub fn check_sequence(seen: u64, new: &Index) -> Result<(), String> {
    if new.sequence < seen {
        return Err(format!(
            "the index is older (sequence {}) than one already seen ({seen})",
            new.sequence
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX: &str = r#"{
      "schema": 1, "generated": "2026-11-01T00:00:00.000Z", "expires": "2026-12-01T00:00:00.000Z",
      "sequence": 3, "keyId": "index-2026-1",
      "extensions": [
        { "id": "loom.lang.rust", "name": "Rust", "languages": [{ "name": "rust", "fileTypes": ["rs"] }],
          "versions": [
            { "version": "2.0.0", "engines": { "loom": ">=0.3.0" },
              "artifacts": [{ "platform": "any", "url": "https://x/2.tgz", "sha256": "SHA", "size": 5 }] },
            { "version": "1.1.0", "engines": { "loom": ">=0.1.8 <0.3" },
              "artifacts": [{ "platform": "any", "url": "https://x/1.1.tgz", "sha256": "SHA", "size": 5 }] },
            { "version": "1.0.0",
              "artifacts": [{ "platform": "any", "url": "https://x/1.tgz", "sha256": "SHA", "size": 5 }] }
          ] },
        { "id": "loom.runtime.node", "name": "Node", "versions": [
            { "version": "22.1.0", "artifacts": [
              { "platform": "windows-x86_64", "url": "https://x/w.tgz", "sha256": "SHA", "size": 9 },
              { "platform": "linux-x86_64", "url": "https://x/l.tgz", "sha256": "SHA", "size": 9 }] }] }
      ]
    }"#;

    fn index() -> Index {
        Index::parse(INDEX.replace("SHA", &"a".repeat(64)).as_bytes()).unwrap()
    }

    #[test]
    fn times_are_read_as_utc_seconds() {
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_time("2000-03-01T00:00:00.000Z"), Some(951_868_800));
        assert_eq!(parse_time("2026-11-01T00:00:00.000Z"), Some(1_793_491_200));
        for bad in [
            "2026-11-01",
            "2026-13-01T00:00:00Z",
            "2026-11-01T25:00:00Z",
            "x",
            "2026-11-01T00:00:00+01:00",
        ] {
            assert_eq!(parse_time(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_best_version_is_the_newest_this_loom_can_run_for_this_platform() {
        let index = index();
        let rust = index.entry("loom.lang.rust").unwrap();
        let pick = |loom: &str| {
            rust.best_for(loom, "linux-x86_64")
                .map(|(v, _)| v.version.as_str())
        };
        assert_eq!(pick("0.2.0"), Some("1.1.0"));
        assert_eq!(pick("0.3.1"), Some("2.0.0"));
        // 1.0.0 states no requirement, so it runs everywhere; 1.1.0 is newer where allowed.
        assert_eq!(pick("0.1.0"), Some("1.0.0"));
        let node = index.entry("loom.runtime.node").unwrap();
        assert!(node.best_for("0.2.0", "windows-x86_64").is_some());
        assert!(node.best_for("0.2.0", "macos-aarch64").is_none());
    }

    #[test]
    fn file_types_find_the_extensions_that_recognize_them() {
        let index = index();
        let found = index.for_file_type("RS");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "loom.lang.rust");
        assert!(index.for_file_type("py").is_empty());
    }

    #[test]
    fn expiry_and_sequence_rules() {
        let index = index();
        let issued = parse_time("2026-11-01T00:00:00Z").unwrap();
        assert!(!index.is_expired(issued + 86_400));
        assert!(index.is_expired(parse_time("2026-12-01T00:00:00Z").unwrap()));
        assert!(
            check_sequence(3, &index).is_ok(),
            "the same index again is fine"
        );
        assert!(check_sequence(2, &index).is_ok());
        assert!(check_sequence(4, &index).unwrap_err().contains("older"));
    }

    const SERVERS: &str = r#"{
      "schema": 1, "generated": "2026-11-01T00:00:00.000Z", "expires": "2026-12-01T00:00:00.000Z",
      "sequence": 1, "keyId": "k",
      "extensions": [
        { "id": "loom.runtime.node", "name": "Node", "versions": [
            { "version": "24.0.0", "artifacts": [{ "platform": "linux-x86_64", "url": "https://x/n24", "sha256": "SHA", "size": 9 }] },
            { "version": "22.4.0", "artifacts": [{ "platform": "linux-x86_64", "url": "https://x/n22", "sha256": "SHA", "size": 9 }] }] },
        { "id": "loom.lsp.pyright", "name": "Pyright", "versions": [
            { "version": "2.0.0", "requires": ["loom.runtime.node@^22"],
              "artifacts": [{ "platform": "linux-x86_64", "url": "https://x/p2", "sha256": "SHA", "size": 9 }] },
            { "version": "1.0.0", "requires": ["loom.runtime.node@^22"],
              "artifacts": [{ "platform": "linux-x86_64", "url": "https://x/p1", "sha256": "SHA", "size": 9 }] }] },
        { "id": "loom.lsp.bash", "name": "Bash", "versions": [
            { "version": "1.0.0", "requires": ["loom.runtime.node@^22"],
              "artifacts": [{ "platform": "linux-x86_64", "url": "https://x/b1", "sha256": "SHA", "size": 9 }] }] },
        { "id": "loom.lsp.loop-a", "name": "A", "versions": [
            { "version": "1.0.0", "requires": ["loom.lsp.loop-b@^1"],
              "artifacts": [{ "platform": "any", "url": "https://x/a", "sha256": "SHA", "size": 9 }] }] },
        { "id": "loom.lsp.loop-b", "name": "B", "versions": [
            { "version": "1.0.0", "requires": ["loom.lsp.loop-a@^1"],
              "artifacts": [{ "platform": "any", "url": "https://x/b", "sha256": "SHA", "size": 9 }] }] }
      ]
    }"#;

    fn servers() -> Index {
        Index::parse(SERVERS.replace("SHA", &"c".repeat(64)).as_bytes()).unwrap()
    }

    fn have(items: &[(&str, &str, &[&str])]) -> std::collections::BTreeMap<String, Installed> {
        items
            .iter()
            .map(|(id, version, requires)| {
                (
                    (*id).to_owned(),
                    Installed {
                        version: (*version).to_owned(),
                        requires: requires.iter().map(|r| (*r).to_owned()).collect(),
                        pinned: false,
                    },
                )
            })
            .collect()
    }

    fn ids(steps: &[Step]) -> Vec<String> {
        steps
            .iter()
            .map(|s| format!("{} {}", s.id, s.version))
            .collect()
    }

    #[test]
    fn a_server_is_planned_with_the_runtime_it_requires_first() {
        let index = servers();
        let plan =
            |installed, id: &str| plan_install(&index, &installed, id, "0.2.0", "linux-x86_64");
        // The runtime that fits ^22 is chosen, not the newest one.
        assert_eq!(
            ids(&plan(have(&[]), "loom.lsp.pyright").unwrap()),
            ["loom.runtime.node 22.4.0", "loom.lsp.pyright 2.0.0"]
        );
        // A second server shares the runtime that is already there.
        let node = have(&[("loom.runtime.node", "22.1.0", &[])]);
        assert_eq!(
            ids(&plan(node.clone(), "loom.lsp.bash").unwrap()),
            ["loom.lsp.bash 1.0.0"]
        );
        // A runtime too new for what is asked is replaced only where nothing is broken.
        let wrong = have(&[("loom.runtime.node", "20.0.0", &[])]);
        assert_eq!(plan(wrong, "loom.lsp.bash").unwrap().len(), 2);
        // Nothing to do for what is already current.
        let current = have(&[
            ("loom.lsp.bash", "1.0.0", &["loom.runtime.node@^22"]),
            ("loom.runtime.node", "22.4.0", &[]),
        ]);
        assert!(plan(current, "loom.lsp.bash").unwrap().is_empty());
        // Unknown, unavailable on this platform, and circular requirements are errors.
        assert!(
            plan(have(&[]), "loom.lsp.nope")
                .unwrap_err()
                .contains("not in the registry")
        );
        let error = plan_install(
            &index,
            &have(&[]),
            "loom.lsp.pyright",
            "0.2.0",
            "windows-x86_64",
        )
        .unwrap_err();
        assert!(error.contains("no version"), "{error}");
        assert!(
            plan(have(&[]), "loom.lsp.loop-a")
                .unwrap_err()
                .contains("requires itself")
        );
    }

    #[test]
    fn a_runtime_is_not_replaced_by_one_an_installed_extension_cannot_use() {
        let index = servers();
        let installed = have(&[
            ("loom.runtime.node", "22.1.0", &[]),
            ("loom.lsp.bash", "1.0.0", &["loom.runtime.node@^22"]),
        ]);
        // Asking for the runtime itself moves it to 22.4.0, not 24.0.0, which the server rejects.
        let steps = plan_install(
            &index,
            &installed,
            "loom.runtime.node",
            "0.2.0",
            "linux-x86_64",
        )
        .unwrap();
        assert_eq!(ids(&steps), ["loom.runtime.node 22.4.0"]);
        // With the server gone, nothing holds the runtime back.
        let alone = have(&[("loom.runtime.node", "22.1.0", &[])]);
        let steps =
            plan_install(&index, &alone, "loom.runtime.node", "0.2.0", "linux-x86_64").unwrap();
        assert_eq!(ids(&steps), ["loom.runtime.node 24.0.0"]);
    }

    #[test]
    fn updates_skip_pinned_extensions_and_dependents_and_orphans_are_found() {
        let index = servers();
        let mut installed = have(&[
            ("loom.runtime.node", "22.1.0", &[]),
            ("loom.lsp.pyright", "1.0.0", &["loom.runtime.node@^22"]),
            ("loom.lsp.bash", "1.0.0", &["loom.runtime.node@^22"]),
        ]);
        let found = updates(&index, &installed, "0.2.0", "linux-x86_64");
        assert!(found.contains(&("loom.lsp.pyright".into(), "1.0.0".into(), "2.0.0".into())));
        // The servers hold the runtime to what they accept.
        assert!(found.contains(&("loom.runtime.node".into(), "22.1.0".into(), "22.4.0".into())));
        installed.get_mut("loom.lsp.pyright").unwrap().pinned = true;
        assert!(
            updates(&index, &installed, "0.2.0", "linux-x86_64")
                .iter()
                .all(|(id, _, _)| id != "loom.lsp.pyright")
        );

        assert_eq!(
            dependents(&installed, "loom.runtime.node"),
            ["loom.lsp.bash", "loom.lsp.pyright"]
        );
        assert!(dependents(&installed, "loom.lsp.bash").is_empty());
        // The runtime is still needed by the other server.
        assert!(orphaned_runtimes(&installed, "loom.lsp.bash").is_empty());
        installed.remove("loom.lsp.pyright");
        assert_eq!(
            orphaned_runtimes(&installed, "loom.lsp.bash"),
            ["loom.runtime.node"]
        );
    }

    #[test]
    fn a_malformed_index_is_refused_even_when_it_is_signed() {
        let good = INDEX.replace("SHA", &"a".repeat(64));
        for (from, to) in [
            ("\"schema\": 1", "\"schema\": 2"),
            ("\"sequence\": 3", "\"sequence\": 0"),
            ("2026-12-01T00:00:00.000Z", "2026-10-01T00:00:00.000Z"),
            ("https://x/2.tgz", "http://x/2.tgz"),
            ("\"size\": 5", "\"size\": 0"),
            ("loom.runtime.node", "loom.lang.rust"),
            ("\"platform\": \"linux-x86_64\"", "\"platform\": \"any\""),
            (
                "\"platform\": \"windows-x86_64\"",
                "\"platform\": \"plan9-x86_64\"",
            ),
            (">=0.1.8 <0.3", "soon"),
        ] {
            assert!(
                Index::parse(good.replacen(from, to, 1).as_bytes()).is_err(),
                "{from} -> {to}"
            );
        }
        assert!(Index::parse(good.replace("a".repeat(64).as_str(), "short").as_bytes()).is_err());
    }
}
