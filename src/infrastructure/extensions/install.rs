//! Installing planned packages from the registry, and what uninstalling needs to know.
//!
//! A plan (`features::extensions::registry::plan_install`) is a list of packages in
//! the order to install them. Each is downloaded to a scratch folder, checked against
//! the size and SHA-256 the signed index lists, and imported through the same path as
//! a package the user chose, which refuses one that is not the extension and version
//! the index promised. If a step fails, the steps before it are undone, so a plan is
//! installed whole or not at all.
use super::package::{Expected, Imported, import_expecting};
use super::state::{self, Source};
use crate::features::extensions::manifest::Manifest;
use crate::features::extensions::registry::{Artifact, Installed, Step};
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Writes the package `artifact` describes to the given path.
pub type Fetch<'a> = dyn Fn(&Artifact, &Path) -> Result<(), String> + 'a;

/// What is installed under `root`, with what each version requires.
pub fn installed(root: &Path) -> BTreeMap<String, Installed> {
    state::read(root)
        .extensions
        .into_iter()
        .map(|(id, entry)| {
            let manifest = state::version_dir(root, &id, &entry.version).join("manifest.json");
            let requires = fs::read_to_string(manifest)
                .ok()
                .and_then(|text| Manifest::parse(&text).ok())
                .map(|manifest| manifest.requires)
                .unwrap_or_default();
            (
                id,
                Installed {
                    version: entry.version,
                    requires,
                    pinned: entry.pinned,
                },
            )
        })
        .collect()
}

static SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Whether the file at `path` is the package `artifact` describes.
fn verify_file(path: &Path, artifact: &Artifact) -> Result<(), String> {
    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = vec![0; 64 * 1024];
    let mut size = 0u64;
    loop {
        let read = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
        size += read as u64;
    }
    if size != artifact.size || hex(digest.finish().as_ref()) != artifact.sha256 {
        return Err("the downloaded package does not match the registry".into());
    }
    Ok(())
}

/// Install the packages of a plan, in order. Returns what was installed; on failure
/// nothing is left of this plan.
pub fn install(
    root: &Path,
    steps: &[Step],
    loom_version: &str,
    fetch: &Fetch,
) -> Result<Vec<Imported>, String> {
    fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let scratch = Scratch(root.join(format!(
        ".download-{}-{}",
        std::process::id(),
        SCRATCH.fetch_add(1, Ordering::Relaxed)
    )));
    fs::create_dir_all(&scratch.0).map_err(|e| e.to_string())?;
    let before = state::read(root);
    let mut done: Vec<Imported> = Vec::new();
    for step in steps {
        let file = scratch
            .0
            .join(format!("{}-{}.loomext", step.id, step.version));
        let result = fetch(&step.artifact, &file)
            .and_then(|()| verify_file(&file, &step.artifact))
            .and_then(|()| {
                import_expecting(
                    &file,
                    root,
                    loom_version,
                    Some(&Expected {
                        id: step.id.clone(),
                        version: step.version.clone(),
                        source: Source::Index {
                            sha256: step.artifact.sha256.clone(),
                            url: step.artifact.url.clone(),
                        },
                    }),
                )
            });
        match result {
            Ok(imported) => {
                let _ = fs::remove_file(&file);
                done.push(imported);
            }
            Err(error) => {
                for imported in done.iter().rev() {
                    let _ = state::undo_install(
                        root,
                        &imported.id,
                        before.extensions.get(&imported.id),
                        &imported.version,
                    );
                }
                return Err(format!("{} {}: {error}", step.id, step.version));
            }
        }
    }
    Ok(done)
}

/// Download `artifact` to `target`, reading no more than the size the registry gave.
pub fn download(artifact: &Artifact, target: &Path) -> Result<(), String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(60))
        .user_agent(concat!("Loom/", env!("CARGO_PKG_VERSION")))
        .https_only(true)
        .build();
    // A connection that fails to open is tried once more; an answer from the server,
    // good or bad, is taken as it is.
    let mut attempt = 0;
    let response = loop {
        match agent.get(&artifact.url).call() {
            Ok(response) => break response,
            Err(ureq::Error::Status(code, _)) => {
                return Err(format!("the server returned HTTP {code}"));
            }
            Err(error) if attempt >= 1 => return Err(error.to_string()),
            Err(_) => attempt += 1,
        }
    };
    let io = |error: std::io::Error| error.to_string();
    let mut reader = response.into_reader().take(artifact.size + 1);
    let mut file = fs::File::create(target).map_err(io)?;
    let copied = std::io::copy(&mut reader, &mut file).map_err(io)?;
    file.flush().map_err(io)?;
    if copied != artifact.size {
        return Err("the downloaded package does not match the registry".into());
    }
    Ok(())
}

/// Whether `id` may be uninstalled. An extension another one requires may not be;
/// otherwise the runtimes only `id` was using are returned, to offer for removal too.
pub fn removal_check(root: &Path, id: &str) -> Result<Vec<String>, String> {
    use crate::features::extensions::registry::{dependents, orphaned_runtimes};
    let installed = installed(root);
    let needed_by = dependents(&installed, id);
    if !needed_by.is_empty() {
        return Err(format!("{} needs it", needed_by.join(", ")));
    }
    Ok(orphaned_runtimes(&installed, id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("loom-install-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A package of extension `id` at `version`, with the root folder the format asks for.
    fn package(id: &str, version: &str, requires: &[&str]) -> Vec<u8> {
        let manifest = serde_json::json!({
            "schema": 1, "id": id, "version": version, "name": id, "publisher": "Loom",
            "description": "A test extension.", "categories": ["Other"], "license": "MIT",
            "repository": "https://example.com/x", "icon": "icon.svg", "readme": "README.md",
            "changelog": "CHANGELOG.md", "engines": { "loom": ">=0.1.0" }, "requires": requires,
        });
        let mut tar = tar::Builder::new(Vec::new());
        for (name, bytes) in [
            ("manifest.json", manifest.to_string().into_bytes()),
            ("icon.svg", b"<svg/>".to_vec()),
            ("README.md", b"# x".to_vec()),
            ("CHANGELOG.md", b"## x".to_vec()),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            tar.append_data(&mut header, format!("{id}/{name}"), bytes.as_slice())
                .unwrap();
        }
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzip.write_all(&tar.into_inner().unwrap()).unwrap();
        gzip.finish().unwrap()
    }

    fn step(id: &str, version: &str, bytes: &[u8]) -> Step {
        Step {
            id: id.into(),
            version: version.into(),
            artifact: Artifact {
                platform: "any".into(),
                url: format!("https://example.com/{id}-{version}"),
                sha256: super::super::package::sha256(bytes),
                size: bytes.len() as u64,
            },
        }
    }

    /// Serves `files` by URL.
    fn server(files: Vec<(String, Vec<u8>)>) -> impl Fn(&Artifact, &Path) -> Result<(), String> {
        move |artifact, path| {
            let bytes = &files
                .iter()
                .find(|(url, _)| *url == artifact.url)
                .ok_or("404")?
                .1;
            fs::write(path, bytes).map_err(|e| e.to_string())
        }
    }

    #[test]
    fn a_plan_is_installed_in_order_and_recorded_as_coming_from_the_index() {
        let root = temp("plan");
        let node = package("loom.runtime.node", "22.4.0", &[]);
        let bash = package("loom.lsp.bash", "1.0.0", &["loom.runtime.node@^22"]);
        let steps = [
            step("loom.runtime.node", "22.4.0", &node),
            step("loom.lsp.bash", "1.0.0", &bash),
        ];
        let fetch = server(vec![
            (steps[0].artifact.url.clone(), node),
            (steps[1].artifact.url.clone(), bash),
        ]);
        let done = install(&root, &steps, "0.1.8", &fetch).unwrap();
        assert_eq!(done.len(), 2);
        let state = state::read(&root);
        assert!(
            matches!(&state.extensions["loom.lsp.bash"].source, Source::Index { url, .. } if url.ends_with("loom.lsp.bash-1.0.0"))
        );
        assert!(
            root.join("loom.runtime.node/22.4.0/manifest.json")
                .is_file()
        );
        // The download folder is gone, and what is installed knows what it requires.
        assert!(fs::read_dir(&root).unwrap().all(|e| {
            !e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".download")
        }));
        assert_eq!(
            installed(&root)["loom.lsp.bash"].requires,
            ["loom.runtime.node@^22"]
        );
        // The runtime cannot be removed while the server uses it; the server can, and frees the runtime.
        assert!(
            removal_check(&root, "loom.runtime.node")
                .unwrap_err()
                .contains("loom.lsp.bash")
        );
        assert_eq!(
            removal_check(&root, "loom.lsp.bash").unwrap(),
            ["loom.runtime.node"]
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_failing_step_undoes_the_steps_before_it_and_restores_the_old_version() {
        let root = temp("rollback");
        // An older runtime is installed first.
        let old = package("loom.runtime.node", "22.1.0", &[]);
        let first = [step("loom.runtime.node", "22.1.0", &old)];
        let fetch = server(vec![(first[0].artifact.url.clone(), old)]);
        install(&root, &first, "0.1.8", &fetch).unwrap();

        // Updating it succeeds, then the server package is damaged in transit.
        let new = package("loom.runtime.node", "22.4.0", &[]);
        let bash = package("loom.lsp.bash", "1.0.0", &["loom.runtime.node@^22"]);
        let steps = [
            step("loom.runtime.node", "22.4.0", &new),
            step("loom.lsp.bash", "1.0.0", &bash),
        ];
        let mut damaged = bash.clone();
        damaged.push(0);
        let fetch = server(vec![
            (steps[0].artifact.url.clone(), new),
            (steps[1].artifact.url.clone(), damaged),
        ]);
        let error = install(&root, &steps, "0.1.8", &fetch).unwrap_err();
        assert!(error.contains("does not match the registry"), "{error}");

        let state = state::read(&root);
        assert_eq!(state.extensions["loom.runtime.node"].version, "22.1.0");
        assert!(!state.extensions.contains_key("loom.lsp.bash"));
        assert!(!root.join("loom.runtime.node/22.4.0").exists());
        assert!(!root.join("loom.lsp.bash").exists());
        assert!(
            root.join("loom.runtime.node/22.1.0/manifest.json")
                .is_file()
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_package_that_is_not_what_the_index_promised_is_refused() {
        let root = temp("promise");
        // The right bytes for the listed hash, but another extension inside.
        let other = package("loom.lsp.other", "1.0.0", &[]);
        let promised = step("loom.lsp.bash", "1.0.0", &other);
        let fetch = server(vec![(promised.artifact.url.clone(), other.clone())]);
        let error = install(&root, std::slice::from_ref(&promised), "0.1.8", &fetch).unwrap_err();
        assert!(
            error.contains("loom.lsp.bash") && error.contains("offered"),
            "{error}"
        );
        // Another version than the one listed.
        let wrong = step("loom.lsp.other", "2.0.0", &other);
        let fetch = server(vec![(wrong.artifact.url.clone(), other)]);
        let error = install(&root, &[wrong], "0.1.8", &fetch).unwrap_err();
        assert!(error.contains("offered"), "{error}");
        assert!(state::read(&root).extensions.is_empty());
        // A download that fails leaves nothing either.
        let missing = step("loom.lsp.gone", "1.0.0", b"x");
        let fetch = server(vec![]);
        assert!(
            install(&root, &[missing], "0.1.8", &fetch)
                .unwrap_err()
                .contains("404")
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// The whole path against the published registry: fetch and verify the signed
    /// index, plan, download, check the hash, install. Needs the network, so it is
    /// run on purpose: `cargo test --bins live_registry -- --ignored`.
    #[test]
    #[ignore]
    fn live_registry_installs_the_bundled_extensions() {
        use super::super::registry;
        use crate::features::extensions::registry::{current_platform, plan_install};
        let root = temp("live");
        let loaded = registry::current(
            &registry::cache_dir(&root),
            None,
            registry::BASE,
            registry::KEYS,
            registry::now(),
            &registry::http,
        )
        .unwrap();
        assert_eq!(
            loaded.origin,
            registry::Origin::Live,
            "{:?}",
            loaded.problem
        );
        assert!(!loaded.expired);
        let platform = current_platform().unwrap();
        let loom = env!("CARGO_PKG_VERSION");
        for id in ["loom.lang.json", "loom.lang.markdown", "loom.theme.aura"] {
            let steps = plan_install(&loaded.index, &installed(&root), id, loom, platform).unwrap();
            assert_eq!(steps.len(), 1, "{id}");
            let done = install(&root, &steps, loom, &download).unwrap();
            assert_eq!(done[0].id, id);
            assert!(done[0].directory.join("manifest.json").is_file());
        }
        // Everything is installed now, so there is nothing left to plan.
        let again = plan_install(
            &loaded.index,
            &installed(&root),
            "loom.lang.json",
            loom,
            platform,
        );
        assert!(again.unwrap().is_empty());
        // Every language the registry offers installs, and the grammars built by its
        // CI load in Loom's loader together, with no clash over names or file types.
        let languages: Vec<String> = loaded
            .index
            .extensions
            .iter()
            .filter(|entry| entry.id.starts_with("loom.lang."))
            .map(|entry| entry.id.clone())
            .collect();
        for id in &languages {
            let steps = plan_install(&loaded.index, &installed(&root), id, loom, platform).unwrap();
            install(&root, &steps, loom, &download).unwrap();
        }
        let packs = crate::infrastructure::language_packs::extension_packs(&root);
        let packages = crate::infrastructure::language_packs::Packages::from_directories(packs);
        assert!(
            packages.activity().errors.is_empty(),
            "{:?}",
            packages.activity().errors
        );
        {
            use crate::features::syntax::Resources;
            let grammars = packages.grammar_names();
            assert!(grammars.len() >= languages.len(), "{grammars:?}");
            for grammar in &grammars {
                packages
                    .language(grammar)
                    .unwrap_or_else(|error| panic!("{grammar}: {error}"));
            }
            eprintln!(
                "{} grammars from {} extensions load",
                grammars.len(),
                languages.len()
            );
        }
        // A language server brings the runtime it requires, if the registry has them.
        if loaded
            .index
            .entry("loom.lsp.bash-language-server")
            .is_some()
        {
            let steps = plan_install(
                &loaded.index,
                &installed(&root),
                "loom.lsp.bash-language-server",
                loom,
                platform,
            )
            .unwrap();
            let ids: Vec<_> = steps.iter().map(|step| step.id.as_str()).collect();
            assert_eq!(ids, ["loom.runtime.node", "loom.lsp.bash-language-server"]);
            install(&root, &steps, loom, &download).unwrap();
            let node = installed(&root);
            assert_eq!(
                crate::features::extensions::registry::dependents(&node, "loom.runtime.node"),
                ["loom.lsp.bash-language-server"]
            );
            // Keep the result to look at, when asked to.
            if let Some(keep) = std::env::var_os("LOOM_KEEP_LIVE_INSTALL") {
                let _ = fs::remove_dir_all(&keep);
                fs::rename(&root, keep).unwrap();
                return;
            }
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rolling_back_switches_to_the_previous_version_and_back_again() {
        let root = temp("back");
        for version in ["1.0.0", "1.1.0"] {
            let bytes = package("loom.lsp.bash", version, &[]);
            let steps = [step("loom.lsp.bash", version, &bytes)];
            let fetch = server(vec![(steps[0].artifact.url.clone(), bytes)]);
            install(&root, &steps, "0.1.8", &fetch).unwrap();
        }
        assert_eq!(
            state::roll_back(&root, "loom.lsp.bash").unwrap(),
            ("1.1.0".into(), "1.0.0".into())
        );
        assert_eq!(
            state::read(&root).extensions["loom.lsp.bash"].version,
            "1.0.0"
        );
        assert_eq!(
            state::roll_back(&root, "loom.lsp.bash").unwrap(),
            ("1.0.0".into(), "1.1.0".into())
        );
        assert!(state::roll_back(&root, "loom.lsp.nope").is_err());
        // A pinned extension is not offered an update, and the pin follows the version change.
        state::set_pinned(&root, "loom.lsp.bash", true).unwrap();
        assert!(installed(&root)["loom.lsp.bash"].pinned);
        state::set_pinned(&root, "loom.lsp.bash", false).unwrap();
        assert!(!installed(&root)["loom.lsp.bash"].pinned);
        assert!(state::set_pinned(&root, "loom.lsp.nope", true).is_err());
        let _ = fs::remove_dir_all(&root);
    }
}
