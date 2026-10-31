//! Language identity. Which languages exist, how files are recognised and what
//! they are called comes from installed package descriptions, not from code:
//! adding a language adds data, not a branch anywhere.
//!
//! Ids are small and `Copy` so caches can key on them cheaply. They are only
//! meaningful for the registry that issued them and are never stored; settings
//! and sessions use the canonical names.
mod descriptor;
mod embedded;
mod registry;
mod rules;

// The language service validates server bindings against canonical names; the
// domain-only verification package compiles this module without that reader.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LanguageId(u32);

/// A variant of a language, for example `tsx` of `typescript`. Ids are global to
/// the registry, not per language.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VariantId(u32);

/// A grammar resource. It names data, not a parser object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GrammarId(u32);

macro_rules! index {
    ($($id:ident),*) => {$(impl $id { pub fn index(self) -> usize { self.0 as usize } })*};
}
index!(LanguageId, VariantId, GrammarId);

/// What a document's language is right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ResolvedLanguage {
    pub language: LanguageId,
    pub variant: VariantId,
    pub grammar: Option<GrammarId>,
}

impl ResolvedLanguage {
    pub fn definition(self) -> &'static LanguageDefinition {
        Registry::builtin().definition(self.language)
    }

    /// The canonical language name, such as `rust`.
    pub fn name(self) -> &'static str {
        &self.definition().name
    }

    pub fn badge(self) -> &'static str {
        &self.definition().badge
    }

    pub fn display_name(self) -> &'static str {
        &self.definition().display_name
    }
}

/// The built-in language for a file path.
pub fn from_path(path: &std::path::Path) -> ResolvedLanguage {
    Registry::builtin().resolve_path(path)
}

pub fn grammar_names() -> Vec<&'static str> {
    Registry::builtin().grammar_names().collect()
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
