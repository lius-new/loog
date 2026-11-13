use std::fmt;
use std::io;

pub type GitResult<T> = Result<T, GitError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitErrorKind {
    RuntimeUnavailable,
    NotRepository,
    Authentication,
    PermissionDenied,
    Network,
    Tls,
    HostKey,
    NonFastForward,
    Conflict,
    Locked,
    HookRejected,
    IdentityMissing,
    DirtyWorktree,
    Cancelled,
    TimedOut,
    UnknownOutcome,
    Unsupported,
    InvalidOutput,
    Io,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitError {
    pub kind: GitErrorKind,
    pub message: String,
    pub detail: Option<String>,
    pub exit_code: Option<i32>,
}

impl GitError {
    pub fn new(kind: GitErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            detail: None,
            exit_code: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(redact(detail.into()));
        self
    }

    pub fn from_stderr(stderr: &[u8], exit_code: Option<i32>) -> Self {
        let detail = redact(String::from_utf8_lossy(stderr).trim());
        let lower = detail.to_ascii_lowercase();
        let (kind, message) = if lower.contains("not a git repository") {
            (
                GitErrorKind::NotRepository,
                "The folder is not a Git repository.",
            )
        } else if lower.contains("authentication failed")
            || lower.contains("could not read username")
            || lower.contains("could not read password")
        {
            (GitErrorKind::Authentication, "Git authentication failed.")
        } else if lower.contains("permission denied") || lower.contains("access denied") {
            (GitErrorKind::PermissionDenied, "Permission was denied.")
        } else if lower.contains("could not resolve host")
            || lower.contains("network is unreachable")
            || lower.contains("failed to connect")
        {
            (GitErrorKind::Network, "The remote could not be reached.")
        } else if lower.contains("ssl") || lower.contains("tls") || lower.contains("certificate") {
            (
                GitErrorKind::Tls,
                "The secure connection could not be verified.",
            )
        } else if lower.contains("host key verification failed") {
            (GitErrorKind::HostKey, "SSH host-key verification failed.")
        } else if lower.contains("non-fast-forward") || lower.contains("fetch first") {
            (
                GitErrorKind::NonFastForward,
                "The remote contains changes that are not local.",
            )
        } else if lower.contains("conflict") || lower.contains("unmerged") {
            (
                GitErrorKind::Conflict,
                "The operation has conflicts to resolve.",
            )
        } else if lower.contains("index.lock") || lower.contains("another git process") {
            (
                GitErrorKind::Locked,
                "The repository is locked by another Git process.",
            )
        } else if lower.contains("please tell me who you are")
            || lower.contains("empty ident name")
            || lower.contains("unable to auto-detect email address")
        {
            (
                GitErrorKind::IdentityMissing,
                "Git user name or email is not configured.",
            )
        } else if lower.contains("would be overwritten")
            || lower.contains("contains modified or untracked files")
            || lower.contains("local changes") && lower.contains("commit")
        {
            (
                GitErrorKind::DirtyWorktree,
                "Local changes prevent this operation.",
            )
        } else if lower.contains("hook declined") || lower.contains("hook failed") {
            (
                GitErrorKind::HookRejected,
                "A Git hook rejected the operation.",
            )
        } else {
            (GitErrorKind::Other, "Git operation failed.")
        };
        Self {
            kind,
            message: message.to_owned(),
            detail: (!detail.is_empty()).then_some(detail),
            exit_code,
        }
    }

    pub fn user_message(&self) -> String {
        self.detail
            .as_deref()
            .and_then(|detail| detail.lines().find(|line| !line.trim().is_empty()))
            .map_or_else(
                || self.message.clone(),
                |detail| format!("{} {detail}", self.message),
            )
    }
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(detail) = &self.detail {
            write!(f, " {detail}")?;
        }
        Ok(())
    }
}

impl std::error::Error for GitError {}

impl From<io::Error> for GitError {
    fn from(error: io::Error) -> Self {
        Self::new(GitErrorKind::Io, error.to_string())
    }
}

pub fn redact(value: impl AsRef<str>) -> String {
    let output = value
        .as_ref()
        .split_inclusive('\n')
        .map(|line| {
            if line
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("authorization:")
            {
                if line.ends_with('\n') {
                    "Authorization: <redacted>\n"
                } else {
                    "Authorization: <redacted>"
                }
            } else {
                line
            }
        })
        .collect::<String>();
    redact_url_credentials(&output)
}

fn redact_url_credentials(input: &str) -> String {
    let mut output = input.to_owned();
    let mut search_from = 0;
    while let Some(scheme_offset) = output[search_from..].find("://") {
        let auth_start = search_from + scheme_offset + 3;
        let auth_end = output[auth_start..]
            .find(['/', ' ', '\r', '\n'])
            .map_or(output.len(), |offset| auth_start + offset);
        if let Some(at_offset) = output[auth_start..auth_end].rfind('@') {
            let at = auth_start + at_offset;
            output.replace_range(auth_start..at, "<redacted>");
            search_from = auth_start + "<redacted>@".len();
        } else {
            search_from = auth_end;
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_common_failures() {
        assert_eq!(
            GitError::from_stderr(b"fatal: Authentication failed", Some(128)).kind,
            GitErrorKind::Authentication
        );
        assert_eq!(
            GitError::from_stderr(b"fatal: Unable to create '.git/index.lock'", Some(128)).kind,
            GitErrorKind::Locked
        );
    }

    #[test]
    fn redacts_url_credentials_and_headers() {
        let value = redact("Authorization: Bearer secret\nhttps://me:token@example.test/x");
        assert!(!value.contains("secret"));
        assert!(!value.contains("token"));
        assert!(value.contains("<redacted>"));
    }
}
