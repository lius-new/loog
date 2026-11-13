//! Repository-relative Git names. Git's protocol uses `/` on every host;
//! a backslash in target output is a filename byte, not a client separator.
use super::error::{GitError, GitErrorKind, GitResult};
use std::{
    borrow::Cow,
    ffi::{OsStr, OsString},
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GitPath {
    spelling: OsString,
    components: Vec<OsString>,
}

impl GitPath {
    pub fn from_git(bytes: &[u8]) -> GitResult<Self> {
        if bytes.len() > 32 * 1024 || bytes.contains(&0) || bytes.starts_with(b"/") {
            return Err(invalid());
        }
        let bytes = bytes.strip_suffix(b"/").unwrap_or(bytes);
        let components = if bytes.is_empty() {
            Vec::new()
        } else {
            bytes
                .split(|byte| *byte == b'/')
                .map(|part| {
                    if part.is_empty() || part == b"." || part == b".." {
                        return Err(invalid());
                    }
                    native_string(part)
                })
                .collect::<GitResult<Vec<_>>>()?
        };
        if components.len() > 128 {
            return Err(invalid());
        }
        Ok(Self::new(components))
    }
    /// Only use at the native filesystem boundary, after making a path relative.
    pub fn from_local(path: &Path) -> GitResult<Self> {
        let components = path
            .components()
            .map(|component| match component {
                Component::Normal(value) => Ok(value.to_owned()),
                _ => Err(invalid()),
            })
            .collect::<GitResult<Vec<_>>>()?;
        let result = Self::new(components);
        if result.spelling.as_encoded_bytes().len() > 32 * 1024 || result.components.len() > 128 {
            return Err(invalid());
        }
        Ok(result)
    }
    fn new(components: Vec<OsString>) -> Self {
        let mut spelling = OsString::new();
        for (index, component) in components.iter().enumerate() {
            if index > 0 {
                spelling.push("/");
            }
            spelling.push(component);
        }
        Self {
            spelling,
            components,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.components.is_empty()
    }
    pub fn as_os_str(&self) -> &OsStr {
        &self.spelling
    }
    pub fn to_str(&self) -> Option<&str> {
        self.spelling.to_str()
    }
    pub fn to_string_lossy(&self) -> Cow<'_, str> {
        self.spelling.to_string_lossy()
    }
    pub fn display(&self) -> impl std::fmt::Display + '_ {
        self.to_string_lossy()
    }
    pub fn file_name(&self) -> Option<&OsStr> {
        self.components.last().map(OsString::as_os_str)
    }
    pub fn parent(&self) -> Option<Self> {
        (!self.is_empty()).then(|| Self::new(self.components[..self.components.len() - 1].to_vec()))
    }
    pub fn components(&self) -> impl ExactSizeIterator<Item = &OsStr> + DoubleEndedIterator {
        self.components.iter().map(OsString::as_os_str)
    }
    pub fn ancestors(&self) -> impl Iterator<Item = Self> {
        std::iter::successors(Some(self.clone()), Self::parent)
    }
    pub fn starts_with(&self, parent: &Self) -> bool {
        self.components.starts_with(&parent.components)
    }
    /// Conversion is deliberately explicit and rejects components the client
    /// OS would reinterpret. Provider identities must never call this method.
    pub fn to_local_path(&self) -> GitResult<PathBuf> {
        let mut path = PathBuf::new();
        for component in &self.components {
            let mut parsed = Path::new(component).components();
            if !matches!(parsed.next(), Some(Component::Normal(value)) if value == component)
                || parsed.next().is_some()
            {
                return Err(invalid());
            }
            path.push(component);
        }
        Ok(path)
    }
    pub fn bytes(&self) -> Cow<'_, [u8]> {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            Cow::Borrowed(self.spelling.as_bytes())
        }
        #[cfg(not(unix))]
        {
            Cow::Owned(self.spelling.to_string_lossy().as_bytes().to_vec())
        }
    }
}
fn invalid() -> GitError {
    GitError::new(
        GitErrorKind::InvalidOutput,
        "Git returned an invalid or unsupported repository-relative name",
    )
}
#[cfg(unix)]
fn native_string(bytes: &[u8]) -> GitResult<OsString> {
    use std::os::unix::ffi::OsStringExt;
    Ok(OsString::from_vec(bytes.to_vec()))
}
#[cfg(not(unix))]
fn native_string(bytes: &[u8]) -> GitResult<OsString> {
    std::str::from_utf8(bytes)
        .map(OsString::from)
        .map_err(|_| invalid())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn git_names_keep_target_components_and_do_not_follow_client_separators() {
        let path = GitPath::from_git("src/中文\\file.txt".as_bytes()).unwrap();
        assert_eq!(path.components().count(), 2);
        assert_eq!(path.file_name(), Some(OsStr::new("中文\\file.txt")));
        assert_eq!(path.bytes(), "src/中文\\file.txt".as_bytes());
        assert!(path.starts_with(&GitPath::from_git(b"src").unwrap()));
        assert!(!path.starts_with(&GitPath::from_git(b"sr").unwrap()));
        assert_eq!(path.ancestors().count(), 3);
        #[cfg(windows)]
        assert!(path.to_local_path().is_err());
        for invalid in [
            b"../x".as_slice(),
            b"a/../x",
            b"/x",
            b"a//x",
            b"a/./x",
            b"x\0y",
        ] {
            assert!(GitPath::from_git(invalid).is_err());
        }
        let local = GitPath::from_local(Path::new("src").join("file.txt").as_path()).unwrap();
        assert_eq!(
            local.to_local_path().unwrap(),
            Path::new("src").join("file.txt")
        );
    }
    #[cfg(unix)]
    #[test]
    fn local_non_unicode_git_names_remain_lossless() {
        let path = GitPath::from_git(b"src/\xff").unwrap();
        assert_eq!(path.bytes(), b"src/\xff");
        assert_eq!(
            GitPath::from_local(&path.to_local_path().unwrap()).unwrap(),
            path
        );
    }
}
