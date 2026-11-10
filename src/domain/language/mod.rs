//! Language identity. Which languages exist, how files are recognised and what
//! they are called comes from installed package descriptions, not from code:
//! adding a language adds data, not a branch anywhere.
//!
//! Resolved languages own their metadata so work in flight can finish safely
//! after the catalog changes. Settings and sessions use canonical names.
mod descriptor;
mod embedded;
mod registry;
mod rules;

// The language service validates server bindings against canonical names; the
// domain-only verification package compiles this module without that reader.
#[allow(unused_imports)]
pub use self::descriptor::Highlight;
#[allow(unused_imports)]
pub(crate) use self::descriptor::is_canonical_name;
#[allow(unused_imports)]
pub use self::registry::{LanguageDefinition, PLAIN_TEXT, Registry};
// The settings and the application use these; the domain-only verification
// package compiles this module without those users.
#[allow(unused_imports)]
pub use self::rules::{
    AssociationSpec, LanguageRules, PATH_CASE_INSENSITIVE, validate as validate_association,
};

use std::hash::{Hash, Hasher};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LanguageId(u32);

impl LanguageId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// The effective language and selected variant of a document.
#[derive(Clone, Debug)]
pub struct ResolvedLanguage {
    definition: Arc<LanguageDefinition>,
    position: usize,
}

impl PartialEq for ResolvedLanguage {
    fn eq(&self, other: &Self) -> bool {
        let a = self.definition();
        let b = other.definition();
        (Arc::ptr_eq(&self.definition, &other.definition) && self.position == other.position)
            || (self.name() == other.name()
                && self.variant() == other.variant()
                && a.display_name == b.display_name
                && a.badge == b.badge
                && a.comments == b.comments
                && a.highlight == b.highlight)
    }
}
impl Eq for ResolvedLanguage {}
impl Hash for ResolvedLanguage {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.name().hash(state);
        self.variant().hash(state);
    }
}

impl ResolvedLanguage {
    pub fn definition(&self) -> &LanguageDefinition {
        &self.definition
    }

    pub fn name(&self) -> &str {
        &self.definition.name
    }

    pub fn shared_name(&self) -> Arc<str> {
        self.definition.name.clone()
    }

    pub fn badge(&self) -> &str {
        &self.definition.badge
    }

    pub fn display_name(&self) -> &str {
        &self.definition.display_name
    }

    fn variant(&self) -> &registry::LanguageVariant {
        &self.definition.variants[self.position]
    }

    pub fn variant_name(&self) -> &str {
        &self.variant().name
    }

    pub fn grammar(&self) -> Option<&Arc<str>> {
        self.variant().grammar.as_ref()
    }
}

/// Recognize the path using the current catalog.
pub fn from_path(path: &std::path::Path) -> ResolvedLanguage {
    Registry::builtin().resolve_path(path)
}

/// A built-in language by canonical name or alias, in its default variant.
#[cfg(test)]
pub fn builtin(name: &str) -> ResolvedLanguage {
    let registry = Registry::builtin();
    let id = registry
        .language_by_name(name)
        .unwrap_or_else(|| panic!("no built-in language `{name}`"));
    registry.default_resolved(id)
}
