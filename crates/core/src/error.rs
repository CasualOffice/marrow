//! Error taxonomy (Part 6 §108).
//!
//! Two rules that this module exists to enforce:
//!
//! 1. Every error carries a **stable code**. Codes appear in logs, in the
//!    index-health UI and in support conversations, so they must not drift.
//! 2. Every error carries a **cause-and-action message** (SUP-001). Generic
//!    failure text is a defect, not a style issue — future-you reading
//!    "operation failed" at 1am learns nothing.
//!
//! `POL_*` denials are deliberately never retryable. A denial that a retry can
//! defeat is not a policy.

use std::fmt;

/// Stable, machine-readable error identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Code {
    // FS_ — filesystem
    FsPermissionDenied,
    FsNotFound,
    FsLocked,
    FsPlaceholderSkipped,
    FsVolumeUnavailable,
    FsPathEscapeBlocked,
    FsNotUtf8Path,
    // PAR_ — parsing
    ParUnsupported,
    ParCorrupt,
    ParTimeout,
    ParLowYield,
    ParTruncated,
    ParBudgetExceeded,
    ParWorkerCrash,
    // IDX_ — derived indexes
    IdxCorrupt,
    IdxGenerationMismatch,
    IdxRebuildRequired,
    // DB_ — canonical storage
    DbBusy,
    DbCorrupt,
    DbMigrationFailed,
    DbDiskFull,
    DbWriterGone,
    // POL_ — policy (never retryable)
    PolDenied,
    PolApprovalRequired,
    PolClassificationBlocked,
    // CFG_ — configuration
    CfgInvalid,
    CfgUnsupportedVersion,
    // INT_ — internal invariant violation; always a bug here, never the user's
    IntInvariantViolated,
}

impl Code {
    /// Stable wire form. Never change these strings.
    pub const fn as_str(self) -> &'static str {
        use Code::*;
        match self {
            FsPermissionDenied => "FS_PERMISSION_DENIED",
            FsNotFound => "FS_NOT_FOUND",
            FsLocked => "FS_LOCKED",
            FsPlaceholderSkipped => "FS_PLACEHOLDER_SKIPPED",
            FsVolumeUnavailable => "FS_VOLUME_UNAVAILABLE",
            FsPathEscapeBlocked => "FS_PATH_ESCAPE_BLOCKED",
            FsNotUtf8Path => "FS_NOT_UTF8_PATH",
            ParUnsupported => "PAR_UNSUPPORTED",
            ParCorrupt => "PAR_CORRUPT",
            ParTimeout => "PAR_TIMEOUT",
            ParLowYield => "PAR_LOW_YIELD",
            ParTruncated => "PAR_TRUNCATED",
            ParBudgetExceeded => "PAR_BUDGET_EXCEEDED",
            ParWorkerCrash => "PAR_WORKER_CRASH",
            IdxCorrupt => "IDX_CORRUPT",
            IdxGenerationMismatch => "IDX_GENERATION_MISMATCH",
            IdxRebuildRequired => "IDX_REBUILD_REQUIRED",
            DbBusy => "DB_BUSY",
            DbCorrupt => "DB_CORRUPT",
            DbMigrationFailed => "DB_MIGRATION_FAILED",
            DbDiskFull => "DB_DISK_FULL",
            DbWriterGone => "DB_WRITER_GONE",
            PolDenied => "POL_DENIED",
            PolApprovalRequired => "POL_APPROVAL_REQUIRED",
            PolClassificationBlocked => "POL_CLASSIFICATION_BLOCKED",
            CfgInvalid => "CFG_INVALID",
            CfgUnsupportedVersion => "CFG_UNSUPPORTED_VERSION",
            IntInvariantViolated => "INT_INVARIANT_VIOLATED",
        }
    }

    /// Whether retrying the identical operation could plausibly succeed.
    ///
    /// Policy denials are always `false` — see the module note.
    pub const fn retryable(self) -> bool {
        use Code::*;
        matches!(
            self,
            FsLocked | FsVolumeUnavailable | ParTimeout | ParWorkerCrash | DbBusy
        )
    }

    /// Whether a failure isolates to one file, leaving the workspace running
    /// (FS-011). Storage and policy failures do not.
    pub const fn isolates_to_one_file(self) -> bool {
        matches!(self.class(), Class::Filesystem | Class::Parse)
    }

    pub const fn class(self) -> Class {
        use Code::*;
        match self {
            FsPermissionDenied | FsNotFound | FsLocked | FsPlaceholderSkipped
            | FsVolumeUnavailable | FsPathEscapeBlocked | FsNotUtf8Path => Class::Filesystem,
            ParUnsupported | ParCorrupt | ParTimeout | ParLowYield | ParTruncated
            | ParBudgetExceeded | ParWorkerCrash => Class::Parse,
            IdxCorrupt | IdxGenerationMismatch | IdxRebuildRequired => Class::Index,
            DbBusy | DbCorrupt | DbMigrationFailed | DbDiskFull | DbWriterGone => Class::Storage,
            PolDenied | PolApprovalRequired | PolClassificationBlocked => Class::Policy,
            CfgInvalid | CfgUnsupportedVersion => Class::Config,
            IntInvariantViolated => Class::Internal,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Filesystem,
    Parse,
    Index,
    Storage,
    Policy,
    Config,
    Internal,
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An error with a stable code and a message that names a cause and an action.
#[derive(Debug)]
pub struct Error {
    code: Code,
    /// What went wrong and what to do about it. Shown to a human.
    message: String,
    /// Diagnostic detail. May name a path, so it is never sent anywhere.
    context: Option<String>,
    source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
}

impl Error {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            context: None,
            source: None,
        }
    }

    pub fn with_context(mut self, ctx: impl Into<String>) -> Self {
        self.context = Some(ctx.into());
        self
    }

    pub fn with_source(mut self, src: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(src));
        self
    }

    pub fn code(&self) -> Code {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn context(&self) -> Option<&str> {
        self.context.as_deref()
    }

    pub fn retryable(&self) -> bool {
        self.code.retryable()
    }

    /// Convenience for the invariant checks the store relies on.
    pub fn invariant(what: impl Into<String>) -> Self {
        Self::new(Code::IntInvariantViolated, what)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)?;
        if let Some(c) = &self.context {
            write!(f, " ({c})")?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|b| b.as_ref() as &(dyn std::error::Error + 'static))
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        use std::io::ErrorKind as K;
        let (code, msg) = match e.kind() {
            K::NotFound => (
                Code::FsNotFound,
                "File no longer exists; it will be re-checked on the next reconciliation.",
            ),
            K::PermissionDenied => (
                Code::FsPermissionDenied,
                "No permission to read this file. Grant access, or exclude it from the workspace.",
            ),
            _ => (
                Code::FsLocked,
                "File could not be read; it may be locked by another process. It will be retried.",
            ),
        };
        Error::new(code, msg).with_source(e)
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_denials_are_never_retryable() {
        for c in [
            Code::PolDenied,
            Code::PolApprovalRequired,
            Code::PolClassificationBlocked,
        ] {
            assert!(!c.retryable(), "{c} must not be retryable");
        }
    }

    #[test]
    fn parse_and_fs_failures_isolate_to_one_file() {
        assert!(Code::ParCorrupt.isolates_to_one_file());
        assert!(Code::FsPermissionDenied.isolates_to_one_file());
        assert!(!Code::DbCorrupt.isolates_to_one_file());
        assert!(!Code::PolDenied.isolates_to_one_file());
    }

    #[test]
    fn codes_are_unique_and_screaming_snake() {
        let all = [
            Code::FsPermissionDenied,
            Code::FsNotFound,
            Code::FsLocked,
            Code::FsPlaceholderSkipped,
            Code::FsVolumeUnavailable,
            Code::FsPathEscapeBlocked,
            Code::FsNotUtf8Path,
            Code::ParUnsupported,
            Code::ParCorrupt,
            Code::ParTimeout,
            Code::ParLowYield,
            Code::ParTruncated,
            Code::ParBudgetExceeded,
            Code::ParWorkerCrash,
            Code::IdxCorrupt,
            Code::IdxGenerationMismatch,
            Code::IdxRebuildRequired,
            Code::DbBusy,
            Code::DbCorrupt,
            Code::DbMigrationFailed,
            Code::DbDiskFull,
            Code::DbWriterGone,
            Code::PolDenied,
            Code::PolApprovalRequired,
            Code::PolClassificationBlocked,
            Code::CfgInvalid,
            Code::CfgUnsupportedVersion,
            Code::IntInvariantViolated,
        ];
        let set: std::collections::HashSet<&str> = all.iter().map(|c| c.as_str()).collect();
        assert_eq!(set.len(), all.len(), "duplicate error code string");
        for c in all {
            let s = c.as_str();
            assert!(s.contains('_'), "{s} must be PREFIX_NAME");
            assert!(
                s.chars()
                    .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_'),
                "{s} must be SCREAMING_SNAKE_CASE"
            );
        }
    }

    #[test]
    fn messages_name_an_action_not_just_a_failure() {
        // SUP-001. A message that only restates the code is a defect.
        let e: Error = std::io::Error::from(std::io::ErrorKind::PermissionDenied).into();
        assert_eq!(e.code(), Code::FsPermissionDenied);
        assert!(e.message().len() > 30, "message must explain, not label");
    }
}
