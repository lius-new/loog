//! Structured errors of the host API (design §7).
use std::fmt;

/// Machine-readable failure category returned to extensions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    UnsupportedApi,
    PermissionDenied,
    StaleSnapshot,
    ExpiredHandle,
    Cancelled,
    DeadlineExceeded,
    LimitExceeded,
    Conflict,
    InvalidEdit,
    ExtensionFailed,
}

impl ErrorKind {
    pub const ALL: [ErrorKind; 10] = [
        Self::UnsupportedApi,
        Self::PermissionDenied,
        Self::StaleSnapshot,
        Self::ExpiredHandle,
        Self::Cancelled,
        Self::DeadlineExceeded,
        Self::LimitExceeded,
        Self::Conflict,
        Self::InvalidEdit,
        Self::ExtensionFailed,
    ];

    /// Stable kebab-case name shared with the WIT `error-kind` enum.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedApi => "unsupported-api",
            Self::PermissionDenied => "permission-denied",
            Self::StaleSnapshot => "stale-snapshot",
            Self::ExpiredHandle => "expired-handle",
            Self::Cancelled => "cancelled",
            Self::DeadlineExceeded => "deadline-exceeded",
            Self::LimitExceeded => "limit-exceeded",
            Self::Conflict => "conflict",
            Self::InvalidEdit => "invalid-edit",
            Self::ExtensionFailed => "extension-failed",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == name)
    }
}

/// An error plus whether the operation had already been committed.
///
/// An error never hides a committed side effect (design §7): when
/// `committed` is set the caller must treat the operation as having happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionError {
    pub kind: ErrorKind,
    pub message: String,
    pub committed: bool,
}

impl ExtensionError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            committed: false,
        }
    }

    pub fn after_commit(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            committed: true,
        }
    }
}

impl fmt::Display for ExtensionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind.as_str(), self.message)?;
        if self.committed {
            write!(f, " (the operation was already committed)")?;
        }
        Ok(())
    }
}

impl std::error::Error for ExtensionError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for kind in ErrorKind::ALL {
            assert_eq!(ErrorKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(ErrorKind::parse("nope"), None);
    }

    #[test]
    fn committed_errors_say_so() {
        let error = ExtensionError::after_commit(ErrorKind::Cancelled, "late cancel");
        assert!(error.to_string().contains("already committed"));
        assert!(!ExtensionError::new(ErrorKind::Cancelled, "x").committed);
    }
}
