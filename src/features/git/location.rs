//! Where a repository lives, spelled as a `PathBuf`.
//!
//! The Git model keys repositories by path. A repository in a provider environment
//! uses a reserved, absolute path that names the environment by an opaque token and
//! carries the provider's own path after it, so joins, parents and prefix tests work
//! unchanged. Only this module interprets the spelling; see
//! `docs/architecture/environment-git.md`.
//!
//! - Windows: `\\?\loog-remote\<token>\<components…>`. A verbatim prefix is absolute and
//!   a file call on it fails at once, where a UNC form (`\\server\share`) would make
//!   Windows go looking for that server on the network.
//! - Elsewhere: `/.loog-remote/<token>/<components…>`.
//!
//! A component is the provider's path component with `%`, `\`, and the names `.` and
//! `..` percent-escaped, so the platform never reads a separator or a relative step
//! into it. Encoding then decoding returns the exact provider path.
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use loog_workspace_api::EnvironmentKey;

#[cfg(windows)]
const MARKER: &str = "loog-remote";
#[cfg(not(windows))]
const MARKER: &str = ".loog-remote";

/// An environment repository path taken apart: the environment token and the provider
/// path, always absolute and `/`-separated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    pub token: String,
    pub path: String,
}

/// Whether `token` can stand in a path: lowercase hex digits and `-`.
pub fn valid_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 64
        && token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte) || byte == b'-')
}

#[derive(Default)]
struct Registry {
    tokens: HashMap<EnvironmentKey, String>,
    environments: HashMap<String, EnvironmentKey>,
}

static REGISTRY: Mutex<Option<Registry>> = Mutex::new(None);

fn with_registry<T>(work: impl FnOnce(&mut Registry) -> T) -> T {
    let mut guard = REGISTRY.lock().unwrap_or_else(PoisonError::into_inner);
    work(guard.get_or_insert_with(Registry::default))
}

/// FNV-1a over the identity, which only has to spread environments apart; a clash is
/// resolved by a numeric suffix.
fn token_seed(environment: &EnvironmentKey) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in environment
        .provider()
        .bytes()
        .chain(std::iter::once(0))
        .chain(environment.instance().bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// The path token of `environment`, the same for the life of the process.
pub fn token_of(environment: &EnvironmentKey) -> String {
    with_registry(|registry| {
        if let Some(token) = registry.tokens.get(environment) {
            return token.clone();
        }
        let seed = token_seed(environment);
        let mut token = seed.clone();
        let mut counter = 0u32;
        while registry.environments.contains_key(&token) {
            counter += 1;
            token = format!("{seed}-{counter}");
        }
        registry.tokens.insert(environment.clone(), token.clone());
        registry
            .environments
            .insert(token.clone(), environment.clone());
        token
    })
}

/// The environment a token was handed out for, in this process.
pub fn environment_of(token: &str) -> Option<EnvironmentKey> {
    with_registry(|registry| registry.environments.get(token).cloned())
}

/// The path that names `provider_path` of `environment` in the Git model.
pub fn path_for(environment: &EnvironmentKey, provider_path: &str) -> PathBuf {
    encode(&token_of(environment), provider_path)
}

/// The path the Git model knows `resource` by: its local path, or the environment path of
/// a provided file. A resource Git cannot hold (an unknown environment) has none.
pub fn git_path(resource: &loog_workspace_api::ResourceRef) -> Option<PathBuf> {
    match resource {
        loog_workspace_api::ResourceRef::Provided { environment, path } => {
            Some(path_for(environment, path.as_str()))
        }
        local => local.local_path().map(Path::to_path_buf),
    }
}

/// The environment and provider path `path` names, when it is an environment path of an
/// environment this process knows.
pub fn resolve(path: &Path) -> Option<(EnvironmentKey, String)> {
    let remote = decode(path)?;
    Some((environment_of(&remote.token)?, remote.path))
}

/// The path of `provider_path` (absolute, `/`-separated) in the environment `token`.
pub fn encode(token: &str, provider_path: &str) -> PathBuf {
    debug_assert!(valid_token(token));
    let mut path = base(token);
    for component in provider_path.split('/').filter(|part| !part.is_empty()) {
        path.push(escape(component));
    }
    path
}

#[cfg(windows)]
fn base(token: &str) -> PathBuf {
    // `\\?\loog-remote` is the prefix; the root directory and the token follow it.
    PathBuf::from(format!(r"\\?\{MARKER}\{token}\"))
}

#[cfg(not(windows))]
fn base(token: &str) -> PathBuf {
    PathBuf::from(format!("/{MARKER}/{token}"))
}

/// The environment and provider path of `path`, when it is an environment repository
/// path. Anything else, including a local path that merely resembles one, is `None`.
pub fn decode(path: &Path) -> Option<Remote> {
    let mut components = path.components();
    let token = take_head(&mut components)?;
    if !valid_token(&token) {
        return None;
    }
    let mut provider = String::new();
    for component in components {
        match component {
            Component::Normal(name) => {
                provider.push('/');
                provider.push_str(&unescape(name.to_str()?)?);
            }
            _ => return None,
        }
    }
    if provider.is_empty() {
        provider.push('/');
    }
    Some(Remote {
        token,
        path: provider,
    })
}

#[cfg(windows)]
fn take_head(components: &mut std::path::Components<'_>) -> Option<String> {
    use std::path::Prefix;
    let Component::Prefix(prefix) = components.next()? else {
        return None;
    };
    let Prefix::Verbatim(name) = prefix.kind() else {
        return None;
    };
    if !name.eq_ignore_ascii_case(MARKER) {
        return None;
    }
    if !matches!(components.next()?, Component::RootDir) {
        return None;
    }
    let Component::Normal(token) = components.next()? else {
        return None;
    };
    Some(token.to_str()?.to_owned())
}

#[cfg(not(windows))]
fn take_head(components: &mut std::path::Components<'_>) -> Option<String> {
    if !matches!(components.next()?, Component::RootDir) {
        return None;
    }
    let Component::Normal(marker) = components.next()? else {
        return None;
    };
    if marker != MARKER {
        return None;
    }
    let Component::Normal(token) = components.next()? else {
        return None;
    };
    Some(token.to_str()?.to_owned())
}

fn escape(component: &str) -> String {
    if component == "." {
        return "%2E".into();
    }
    if component == ".." {
        return "%2E%2E".into();
    }
    let mut out = String::with_capacity(component.len());
    for c in component.chars() {
        match c {
            '%' => out.push_str("%25"),
            '\\' => out.push_str("%5C"),
            // Characters Windows reads specially in the first component, or refuses.
            ':' => out.push_str("%3A"),
            c if c.is_control() => out.push_str(&format!("%{:02X}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn unescape(component: &str) -> Option<String> {
    let mut out = String::with_capacity(component.len());
    let mut chars = component.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let high = chars.next()?.to_digit(16)?;
        let low = chars.next()?.to_digit(16)?;
        out.push(char::from_u32(high * 16 + low)?);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_provider_path_round_trips_exactly() {
        for provider in [
            "/",
            "/root/project",
            "/srv/a b/中文",
            "/odd/back\\slash/per%cent/co:lon",
            "/dots/./../x",
        ] {
            let path = encode("0a1b-2", provider);
            let decoded = decode(&path).unwrap_or_else(|| panic!("{path:?}"));
            assert_eq!(decoded.token, "0a1b-2");
            let normalized = if provider == "/" {
                "/".to_owned()
            } else {
                provider.to_owned()
            };
            assert_eq!(decoded.path, normalized, "{provider}");
        }
    }

    #[test]
    fn the_encoded_path_is_absolute_and_joins_like_a_path() {
        let root = encode("ab", "/home/liu/repo");
        assert!(root.is_absolute());
        let file = root.join("src").join("main.rs");
        assert!(file.starts_with(&root));
        assert_eq!(file.strip_prefix(&root).unwrap(), Path::new("src/main.rs"));
        assert_eq!(decode(&file).unwrap().path, "/home/liu/repo/src/main.rs");
        assert_eq!(decode(root.parent().unwrap()).unwrap().path, "/home/liu");
    }

    #[test]
    fn a_component_never_reads_as_a_separator_or_a_step() {
        let path = encode("ab", "/a/..\\b/..");
        let kinds: Vec<_> = path.components().skip(2).collect();
        assert!(
            kinds
                .iter()
                .all(|component| matches!(component, Component::Normal(_) | Component::RootDir)),
            "{kinds:?}"
        );
        assert_eq!(decode(&path).unwrap().path, "/a/..\\b/..");
    }

    #[test]
    fn local_paths_are_never_taken_for_environment_paths() {
        for path in [
            "relative/dir",
            "/",
            "/loog-remote/ab/x",
            "/.loog-remote",
            "C:\\work\\repo",
            "",
        ] {
            assert_eq!(decode(Path::new(path)), None, "{path}");
        }
        // The marker with an invalid token is not one either.
        #[cfg(windows)]
        let odd = PathBuf::from(r"\\?\loog-remote\Not Hex\x");
        #[cfg(not(windows))]
        let odd = PathBuf::from("/.loog-remote/Not Hex/x");
        assert_eq!(decode(&odd), None);
    }

    #[test]
    fn a_resource_is_known_to_git_by_its_local_or_environment_path() {
        use loog_workspace_api::ResourceRef;
        let local = ResourceRef::local(PathBuf::from("work").join("a.rs"));
        assert_eq!(git_path(&local), Some(PathBuf::from("work").join("a.rs")));
        let environment = EnvironmentKey::new("fixture", "git-path").unwrap();
        let provided = ResourceRef::provided(environment.clone(), "/srv/app/a.rs").unwrap();
        let path = git_path(&provided).unwrap();
        assert_eq!(path, path_for(&environment, "/srv/app/a.rs"));
        assert_eq!(resolve(&path), Some((environment, "/srv/app/a.rs".to_owned())));
    }

    #[test]
    fn tokens_are_short_lowercase_hex() {
        assert!(valid_token("0123456789abcdef-1"));
        assert!(!valid_token("") && !valid_token("ABC") && !valid_token("a/b"));
        assert!(!valid_token(&"a".repeat(65)));
    }
}
