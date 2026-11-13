//! Runtime document identity and metadata.

use loog_workspace_api::ResourceRef;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub use crate::domain::language::ResolvedLanguage;

pub use crate::domain::identity::FileId;

#[derive(Clone, Debug)]
pub struct FileMeta {
    pub name: String,
    pub lang: ResolvedLanguage,
    /// Bumped whenever the effective language changes, so results computed for
    /// the old language are dropped even if the text did not change.
    pub language_generation: u64,
    pub resource: ResourceRef,
    /// Stable mounted root, independent of runtime scope handles.
    pub provider_root: Option<ResourceRef>,
    /// Provider-returned components below that root, never client Path parsing.
    pub provider_relative: Option<Vec<String>>,
    pub encoding_source: crate::domain::encoding::EncodingSource,
    pub read_override: Option<crate::domain::encoding::TextFormat>,
}

/// Everything that decides whether a derived result still describes a document:
/// which document, which lifetime of it, which text and which language. Compare
/// all fields; `snapshot_identity` is an allocation address that can be reused
/// after the text is freed, so it proves nothing on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DocumentStamp {
    pub document: FileId,
    pub incarnation: u64,
    pub version: u64,
    pub snapshot_identity: usize,
    pub language_generation: u64,
}

impl DocumentStamp {
    pub fn new(
        document: FileId,
        incarnation: u64,
        language_generation: u64,
        buffer: &loog_editor_core::text::TextBuffer,
    ) -> Self {
        Self {
            document,
            incarnation,
            version: buffer.version(),
            snapshot_identity: buffer.snapshot_identity(),
            language_generation,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskState {
    pub modified_time: Option<SystemTime>,
    pub size: u64,
    pub content_hash: u64,
    pub format: crate::domain::encoding::TextFormat,
}

/// Immutable input to a background save. Text shares the editor's COW snapshot.
#[derive(Clone, Debug)]
pub struct SaveSnapshot {
    pub stamp: DocumentStamp,
    pub path: PathBuf,
    pub text: std::sync::Arc<String>,
    pub disk_observation: u64,
    pub format: crate::domain::encoding::TextFormat,
}

#[derive(Clone, Debug)]
pub struct ResourceSaveSnapshot {
    pub stamp: DocumentStamp,
    pub resource: ResourceRef,
    pub scope: loog_workspace_api::ScopeId,
    pub revision: Option<String>,
    pub text: std::sync::Arc<String>,
    pub format: crate::domain::encoding::TextFormat,
}

/// A filesystem result. Missing and unreadable are distinct observations.
#[derive(Clone, Debug)]
pub enum DiskRead {
    Found {
        contents: String,
        state: DiskState,
        bytes: Option<std::sync::Arc<Vec<u8>>>,
    },
    Missing,
    Failed(String),
}

impl DiskState {
    pub fn from_contents(contents: &str) -> Self {
        Self {
            modified_time: None,
            size: contents.len() as u64,
            content_hash: content_hash(contents),
            format: Default::default(),
        }
    }

    pub fn differs_from(&self, other: &Self) -> bool {
        self.size != other.size
            || self.content_hash != other.content_hash
            || self.modified_time != other.modified_time
    }
}

pub use crate::domain::document_snapshot::content_hash;

impl FileMeta {
    pub fn local_path(&self) -> Option<&Path> {
        self.resource.local_path()
    }
    pub fn context_key(&self) -> String {
        // COMPAT(local-document-context): Existing components use the native
        // display path as their shared UI context. Provided resources use the
        // tagged resource key. Retire the local projection with the old
        // DocumentRef contract; no removal version is scheduled.
        self.local_path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| self.resource.context_key())
    }
    pub fn from_path(path: PathBuf) -> Self {
        let name = path
            .file_name()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        let lang = crate::domain::language::from_path(&path);
        Self {
            name,
            lang,
            encoding_source: Default::default(),
            read_override: None,
            language_generation: 0,
            resource: ResourceRef::local(path),
            provider_root: None,
            provider_relative: None,
        }
    }

    /// Change the language and invalidate everything derived from the old one.
    #[allow(dead_code)] // First caller: the manual language override in P2.
    pub fn set_language(&mut self, lang: ResolvedLanguage) {
        if self.lang != lang {
            self.lang = lang;
            self.language_generation += 1;
        }
    }

    pub fn directory_label(&self) -> String {
        self.resource
            .local_path()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| {
                self.resource
                    .environment()
                    .map(|e| e.instance().to_owned())
                    .unwrap_or_default()
            })
    }

    /// This is a filename hint for pure language recognition, never an IO path.
    pub fn language_hint(&self) -> &Path {
        self.resource
            .local_path()
            .unwrap_or_else(|| Path::new(&self.name))
    }

    pub fn provided(resource: ResourceRef, name: String) -> Self {
        Self {
            lang: crate::domain::language::Registry::builtin().resolve_filename(&name),
            name,
            resource,
            encoding_source: Default::default(),
            read_override: None,
            language_generation: 0,
            provider_root: None,
            provider_relative: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::domain::document::*;

    #[test]
    fn metadata_gets_its_language_from_the_path() {
        let name = |path: &str| FileMeta::from_path(path.into()).lang.name().to_owned();
        assert_eq!(name("main.RS"), "rust");
        assert_eq!(name("component.tsx"), "typescript");
        assert_eq!(name("script.mjs"), "javascript");
        assert_eq!(name("README.md"), "markdown");
        assert_eq!(name("notes.txt"), "plaintext");
    }

    #[test]
    fn changing_the_language_bumps_the_generation_only_when_it_changes() {
        let mut meta = FileMeta::from_path("a.rs".into());
        let rust = meta.lang.clone();
        meta.set_language(rust.clone());
        assert_eq!(meta.language_generation, 0);
        meta.set_language(crate::domain::language::builtin("plaintext"));
        assert_eq!(meta.language_generation, 1);
        meta.set_language(rust);
        assert_eq!(meta.language_generation, 2);
    }
}
