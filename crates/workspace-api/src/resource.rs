use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A persistent provider instance identity, never a runtime handle or a secret.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "EnvironmentIdentity", into = "EnvironmentIdentity")]
pub struct EnvironmentKey {
    provider: String,
    instance: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvironmentIdentity {
    provider: String,
    instance: String,
}

impl EnvironmentKey {
    pub fn new(provider: impl Into<String>, instance: impl Into<String>) -> Result<Self, String> {
        let provider = provider.into();
        let instance = instance.into();
        validate_text(&provider, 256)?;
        validate_text(&instance, 4096)?;
        Ok(Self { provider, instance })
    }
    pub fn provider(&self) -> &str {
        &self.provider
    }
    pub fn instance(&self) -> &str {
        &self.instance
    }
}

impl TryFrom<EnvironmentIdentity> for EnvironmentKey {
    type Error = String;
    fn try_from(value: EnvironmentIdentity) -> Result<Self, Self::Error> {
        Self::new(value.provider, value.instance)
    }
}
impl From<EnvironmentKey> for EnvironmentIdentity {
    fn from(value: EnvironmentKey) -> Self {
        Self {
            provider: value.provider,
            instance: value.instance,
        }
    }
}

/// The provider supplies its canonical identity. No client OS path operation
/// may normalize, join or case-fold this value.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProviderPath(String);

impl ProviderPath {
    pub fn new(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        validate_text(&value, 32 * 1024)?;
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for ProviderPath {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<ProviderPath> for String {
    fn from(value: ProviderPath) -> Self {
        value.0
    }
}

/// Local paths preserve the OS representation, including non-UTF-8 names.
/// Provider paths are opaque and are never convertible to a local Path.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResourceRef {
    Local {
        path: PathBuf,
    },
    Provided {
        environment: EnvironmentKey,
        path: ProviderPath,
    },
}
// Conversions only enter the resource domain. There is deliberately no inverse
// conversion: a provider identity must never become a client filesystem path.
impl From<PathBuf> for ResourceRef {
    fn from(path: PathBuf) -> Self {
        Self::local(path)
    }
}
impl From<&Path> for ResourceRef {
    fn from(path: &Path) -> Self {
        Self::local(path)
    }
}
impl From<&PathBuf> for ResourceRef {
    fn from(path: &PathBuf) -> Self {
        Self::local(path)
    }
}
impl From<&ResourceRef> for ResourceRef {
    fn from(resource: &ResourceRef) -> Self {
        resource.clone()
    }
}
impl ResourceRef {
    /// Presentation only; never feed this text to a local filesystem API.
    pub fn display(&self) -> impl std::fmt::Display + '_ {
        struct Display<'a>(&'a ResourceRef);
        impl std::fmt::Display for Display<'_> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match self.0 {
                    ResourceRef::Local { path } => path.display().fmt(f),
                    ResourceRef::Provided { environment, path } => {
                        write!(f, "{} · {}", environment.instance(), path.as_str())
                    }
                }
            }
        }
        Display(self)
    }
    pub fn local(path: impl Into<PathBuf>) -> Self {
        Self::Local { path: path.into() }
    }
    pub fn provided(environment: EnvironmentKey, path: impl Into<String>) -> Result<Self, String> {
        Ok(Self::Provided {
            environment,
            path: ProviderPath::new(path)?,
        })
    }
    pub fn local_path(&self) -> Option<&Path> {
        match self {
            Self::Local { path } => Some(path),
            Self::Provided { .. } => None,
        }
    }
    pub fn environment(&self) -> Option<&EnvironmentKey> {
        match self {
            Self::Local { .. } => None,
            Self::Provided { environment, .. } => Some(environment),
        }
    }
    /// An unambiguous context key, independent of display text or credentials.
    pub fn context_key(&self) -> String {
        // JSON's tagged structure and string escaping avoid delimiter collisions.
        // Local non-Unicode paths cannot be serialized by serde; their context
        // identity uses the exact encoded OS bytes rather than lossy display.
        match self {
            Self::Local { path } => {
                let mut result = String::from("local:");
                for byte in path.as_os_str().as_encoded_bytes() {
                    use std::fmt::Write;
                    write!(&mut result, "{byte:02x}").expect("writing to String");
                }
                result
            }
            Self::Provided { .. } => serde_json::to_string(self).expect("validated strings"),
        }
    }
}

fn validate_text(value: &str, max: usize) -> Result<(), String> {
    if value.is_empty() || value.len() > max || value.contains('\0') {
        Err(format!(
            "identity must contain 1..={max} UTF-8 bytes without NUL"
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identical_paths_in_distinct_environments_and_local_disk_are_distinct() {
        let a =
            ResourceRef::provided(EnvironmentKey::new("p", "a").unwrap(), "/work/main.rs").unwrap();
        let b =
            ResourceRef::provided(EnvironmentKey::new("p", "b").unwrap(), "/work/main.rs").unwrap();
        let local = ResourceRef::local("/work/main.rs");
        assert_ne!(a, b);
        assert_ne!(a.context_key(), b.context_key());
        assert_ne!(a.context_key(), local.context_key());
        assert!(a.local_path().is_none());
    }
    #[test]
    fn provider_spelling_and_case_are_not_changed_by_client_platform() {
        let env = EnvironmentKey::new("p", "one").unwrap();
        let a = ResourceRef::provided(env.clone(), "/A\\b/../c").unwrap();
        let b = ResourceRef::provided(env, "/a/b/../c").unwrap();
        assert_ne!(a, b);
        let ResourceRef::Provided { path, .. } = a else {
            panic!()
        };
        assert_eq!(path.as_str(), "/A\\b/../c");
    }
    #[test]
    fn malformed_persisted_identities_are_rejected_and_valid_ones_roundtrip() {
        let value =
            ResourceRef::provided(EnvironmentKey::new("p", "a:b").unwrap(), "/a:\"b").unwrap();
        let json = serde_json::to_string(&value).unwrap();
        assert_eq!(value, serde_json::from_str(&json).unwrap());
        assert!(
            serde_json::from_str::<EnvironmentKey>(r#"{"provider":"","instance":"a"}"#).is_err()
        );
        assert!(serde_json::from_str::<ProviderPath>(r#""\u0000""#).is_err());
    }
}
