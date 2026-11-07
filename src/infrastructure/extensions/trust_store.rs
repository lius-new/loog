//! Persisted workspace trust (design §9.1).
//!
//! The store is shared between the UI (trust and revoke actions) and the
//! extension process service, which checks this store before every launch.
use crate::features::extensions::trust::{Decision, TrustedRoots};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Arc;
use std::sync::Mutex;

#[derive(Default)]
pub struct TrustStore {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    file: Option<PathBuf>,
    /// The decisions were read; before that every folder would look undecided.
    loaded: bool,
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
        inner.loaded = true;
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

    /// Remember that the user said no to `root`: it stays untrusted and is not asked about again.
    pub fn decline(&self, root: &Path) -> std::io::Result<()> {
        self.change(|roots| {
            roots.decline(root);
        })
    }

    /// Whether the stored decisions have been read, so a folder can be called undecided.
    pub fn ready(&self) -> bool {
        self.inner.lock().unwrap().loaded
    }

    /// What was decided about `path`, a folder that may or may not be open.
    pub fn decision_of(&self, path: &Path) -> Decision {
        let inner = self.inner.lock().unwrap();
        inner.roots.decision_within(path, &inner.workspace)
    }

    /// The open folders the user said no to, in the order they are open.
    pub fn declined_open(&self) -> Vec<PathBuf> {
        let inner = self.inner.lock().unwrap();
        inner
            .workspace
            .iter()
            .filter(|root| {
                inner.roots.decision_within(root, &inner.workspace) == Decision::Declined
            })
            .cloned()
            .collect()
    }

    /// The open folders nobody has decided on yet, in the order they are open.
    /// Empty until the stored decisions have been read.
    pub fn undecided(&self) -> Vec<PathBuf> {
        let inner = self.inner.lock().unwrap();
        if !inner.loaded {
            return Vec::new();
        }
        inner
            .workspace
            .iter()
            .filter(|root| {
                inner.roots.decision_within(root, &inner.workspace) == Decision::Undecided
            })
            .cloned()
            .collect()
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "loog-trust-{}-{name}/roots.json",
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
        assert!(!store.is_trusted(&root));

        store.trust(&root).unwrap();
        assert!(store.is_trusted(&root.join("src")));

        // A new process reads the same decision back.
        let again = TrustStore::default();
        again.attach(path);
        assert!(again.is_trusted(&root));
    }

    #[test]
    fn declining_takes_effect_immediately_and_persists() {
        let path = file("b");
        let _ = std::fs::remove_file(&path);
        let root = std::env::temp_dir().join("trust-project-b");
        let store = Arc::new(TrustStore::default());
        store.attach(path.clone());
        store.set_workspace(std::slice::from_ref(&root));
        store.trust(&root).unwrap();
        assert!(store.is_trusted(&root));
        store.decline(&root).unwrap();
        assert!(!store.is_trusted(&root));
        let again = TrustStore::default();
        again.attach(path);
        again.set_workspace(std::slice::from_ref(&root));
        assert!(!again.is_trusted(&root));
        assert!(again.undecided().is_empty(), "a no is remembered");
    }

    #[test]
    fn only_open_folders_nobody_decided_on_are_undecided_and_not_before_the_file_is_read() {
        let path = file("undecided");
        let _ = std::fs::remove_file(&path);
        let (one, two) = (
            std::env::temp_dir().join("trust-undecided-one"),
            std::env::temp_dir().join("trust-undecided-two"),
        );
        let store = TrustStore::default();
        store.set_workspace(&[one.clone(), two.clone()]);
        assert!(store.undecided().is_empty(), "decisions not read yet");
        store.attach(path);
        assert_eq!(store.undecided(), vec![one.clone(), two.clone()]);
        store.trust(&one).unwrap();
        assert_eq!(store.undecided(), vec![two.clone()]);
        store.decline(&two).unwrap();
        assert!(store.undecided().is_empty());
    }

    #[test]
    fn a_folder_opened_inside_a_trusted_folder_is_not_trusted_until_it_is_decided_on_its_own() {
        let outer = std::env::temp_dir().join("trust-nested-outer");
        let inner = outer.join("app");
        let store = Arc::new(TrustStore::default());
        store.trust(&outer).unwrap();
        // Before the folders are known the plain rule holds.
        assert!(store.is_trusted(&inner));

        store.set_workspace(&[outer.clone(), inner.clone()]);
        assert!(store.is_trusted(&outer.join("src")));
        assert!(!store.is_trusted(&inner.join("src")));
        store.trust(&inner).unwrap();
        assert!(store.is_trusted(&inner.join("src")));
    }

    #[test]
    fn a_store_without_a_file_still_works_in_memory() {
        let store = TrustStore::default();
        let root = std::env::temp_dir().join("trust-project-c");
        store.trust(&root).unwrap();
        assert!(store.is_trusted(&root));
    }
}
