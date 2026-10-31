//! Persisted workspace trust (design §9.1).
//!
//! The store is shared between the UI (trust and revoke actions) and the
//! language server manager, which asks before starting a process.
use crate::features::extensions::trust::TrustedRoots;
use crate::infrastructure::lsp::manager::StartGate;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct TrustStore {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    file: Option<PathBuf>,
    roots: TrustedRoots,
    /// The open workspace folders; the innermost one that holds a path decides
    /// whether it is trusted.
    workspace: Vec<PathBuf>,
}

impl TrustStore {
    /// Use `file` for persistence and read what it holds. A missing or
    /// damaged file leaves everything untrusted.
    pub fn attach(&self, file: PathBuf) {
        let roots = std::fs::read_to_string(&file)
            .map(|text| TrustedRoots::from_json(&text))
            .unwrap_or_default();
        let mut inner = self.inner.lock().unwrap();
        inner.roots = roots;
        inner.file = Some(file);
    }

    pub fn is_trusted(&self, path: &Path) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.roots.is_trusted_within(path, &inner.workspace)
    }

    /// The folders that are open now. Trust is judged against them.
    pub fn set_workspace(&self, folders: &[PathBuf]) {
        let mut inner = self.inner.lock().unwrap();
        if inner.workspace != folders {
            inner.workspace = folders.to_vec();
        }
    }

    pub fn trust(&self, root: &Path) -> std::io::Result<()> {
        self.change(|roots| {
            roots.trust(root);
        })
    }

    pub fn revoke(&self, root: &Path) -> std::io::Result<()> {
        self.change(|roots| {
            roots.revoke(root);
        })
    }

    fn change(&self, edit: impl FnOnce(&mut TrustedRoots)) -> std::io::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        edit(&mut inner.roots);
        let Some(file) = inner.file.clone() else {
            return Ok(());
        };
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let partial = file.with_extension("json.partial");
        std::fs::write(&partial, inner.roots.to_json())?;
        std::fs::rename(&partial, &file)
    }

    /// The gate the language server manager consults before starting a process.
    pub fn start_gate(self: &Arc<Self>) -> StartGate {
        let store = self.clone();
        StartGate(Arc::new(move |root| {
            if store.is_trusted(root) {
                Ok(())
            } else {
                Err(format!(
                    "{} is not trusted, so language servers are not started; run `workspace::TrustWorkspace` to allow them",
                    root.display()
                ))
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "loom-trust-{}-{name}/roots.json",
            std::process::id()
        ))
    }

    #[test]
    fn untrusted_by_default_then_trusted_and_persisted() {
        let path = file("a");
        let _ = std::fs::remove_file(&path);
        let root = std::env::temp_dir().join("trust-project");
        let store = Arc::new(TrustStore::default());
        store.attach(path.clone());
        let gate = store.start_gate();
        assert!((gate.0)(&root).unwrap_err().contains("not trusted"));

        store.trust(&root).unwrap();
        assert!((gate.0)(&root.join("src")).is_ok());

        // A new process reads the same decision back.
        let again = TrustStore::default();
        again.attach(path);
        assert!(again.is_trusted(&root));
    }

    #[test]
    fn revoking_takes_effect_immediately_and_persists() {
        let path = file("b");
        let root = std::env::temp_dir().join("trust-project-b");
        let store = Arc::new(TrustStore::default());
        store.attach(path.clone());
        store.trust(&root).unwrap();
        let gate = store.start_gate();
        assert!((gate.0)(&root).is_ok());
        store.revoke(&root).unwrap();
        assert!((gate.0)(&root).is_err());
        let again = TrustStore::default();
        again.attach(path);
        assert!(!again.is_trusted(&root));
    }

    #[test]
    fn a_folder_opened_inside_a_trusted_folder_is_not_trusted_until_it_is_decided_on_its_own() {
        let outer = std::env::temp_dir().join("trust-nested-outer");
        let inner = outer.join("app");
        let store = Arc::new(TrustStore::default());
        store.trust(&outer).unwrap();
        let gate = store.start_gate();
        // Before the folders are known the plain rule holds.
        assert!((gate.0)(&inner).is_ok());

        store.set_workspace(&[outer.clone(), inner.clone()]);
        assert!((gate.0)(&outer.join("src")).is_ok());
        assert!(
            (gate.0)(&inner.join("src"))
                .unwrap_err()
                .contains("not trusted")
        );
        store.trust(&inner).unwrap();
        assert!((gate.0)(&inner.join("src")).is_ok());
    }

    #[test]
    fn a_store_without_a_file_still_works_in_memory() {
        let store = TrustStore::default();
        let root = std::env::temp_dir().join("trust-project-c");
        store.trust(&root).unwrap();
        assert!(store.is_trusted(&root));
    }
}
