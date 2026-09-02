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

/// Declares the whole code table in one place.
///
/// The enum, its wire strings, its class and its retryability are generated
/// from a single list, and so is [`Code::ALL`]. That last one is the point:
/// the hand-maintained list this replaced had already gone stale —
/// `DB_SCHEMA_TOO_NEW` existed for weeks with no test covering it, because
/// adding a variant and forgetting the test list is silent.
macro_rules! codes {
    ($( $(#[$meta:meta])* $variant:ident => $wire:literal, $class:ident, $retryable:literal; )*) => {
        /// Stable, machine-readable error identity.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Code {
            $( $(#[$meta])* $variant, )*
        }

        impl Code {
            /// Every code. Generated, so it cannot fall behind the enum.
            pub const ALL: &'static [Code] = &[ $( Code::$variant, )* ];

            /// Stable wire form. Never change these strings.
            pub const fn as_str(self) -> &'static str {
                match self { $( Code::$variant => $wire, )* }
            }

            /// Whether retrying the identical operation could plausibly succeed.
            ///
            /// Policy denials are always `false` — see the module note.
            pub const fn retryable(self) -> bool {
                match self { $( Code::$variant => $retryable, )* }
            }

            pub const fn class(self) -> Class {
                match self { $( Code::$variant => Class::$class, )* }
            }

            /// Parse a wire string back. Unknown strings are `None` rather
            /// than a catch-all variant: a code we do not know is a version
            /// mismatch, and silently mapping it to `INT_INVARIANT_VIOLATED`
            /// would hide that.
            pub fn from_wire(s: &str) -> Option<Code> {
                match s { $( $wire => Some(Code::$variant), )* _ => None }
            }
        }

        impl serde::Serialize for Code {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> serde::Deserialize<'de> for Code {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Code, D::Error> {
                let s = <std::borrow::Cow<'de, str>>::deserialize(d)?;
                Code::from_wire(&s).ok_or_else(|| {
                    serde::de::Error::custom(format!("unknown error code {s:?}"))
                })
            }
        }
    };
}

codes! {
    // FS_ — filesystem
    FsPermissionDenied      => "FS_PERMISSION_DENIED",       Filesystem, false;
    FsNotFound              => "FS_NOT_FOUND",               Filesystem, false;
    FsLocked                => "FS_LOCKED",                  Filesystem, true;
    FsPlaceholderSkipped    => "FS_PLACEHOLDER_SKIPPED",     Filesystem, false;
    FsVolumeUnavailable     => "FS_VOLUME_UNAVAILABLE",      Filesystem, true;
    FsPathEscapeBlocked     => "FS_PATH_ESCAPE_BLOCKED",     Filesystem, false;
    FsNotUtf8Path           => "FS_NOT_UTF8_PATH",           Filesystem, false;

    // PAR_ — parsing
    ParUnsupported          => "PAR_UNSUPPORTED",            Parse, false;
    ParCorrupt              => "PAR_CORRUPT",                Parse, false;
    ParTimeout              => "PAR_TIMEOUT",                Parse, true;
    ParLowYield             => "PAR_LOW_YIELD",              Parse, false;
    ParTruncated            => "PAR_TRUNCATED",              Parse, false;
    ParBudgetExceeded       => "PAR_BUDGET_EXCEEDED",        Parse, false;
    ParWorkerCrash          => "PAR_WORKER_CRASH",           Parse, true;

    // IDX_ — derived indexes
    IdxCorrupt              => "IDX_CORRUPT",                Index, false;
    IdxGenerationMismatch   => "IDX_GENERATION_MISMATCH",    Index, false;
    IdxRebuildRequired      => "IDX_REBUILD_REQUIRED",       Index, false;

    // DB_ — canonical storage
    DbBusy                  => "DB_BUSY",                    Storage, true;
    DbCorrupt               => "DB_CORRUPT",                 Storage, false;
    DbMigrationFailed       => "DB_MIGRATION_FAILED",        Storage, false;
    DbDiskFull              => "DB_DISK_FULL",               Storage, false;
    DbWriterGone            => "DB_WRITER_GONE",             Storage, false;
    /// The database was written by a newer build. §107 requires refusing to
    /// open it rather than guessing at columns we do not know about.
    DbSchemaTooNew          => "DB_SCHEMA_TOO_NEW",          Storage, false;

    // MOD_ — the model runtime (Part 8 §142).
    //
    // The resource refusals are retryable because they describe a moment, not
    // a fact: memory frees, the fans catch up, the queue drains. The rest are
    // facts about this model on this machine, and retrying changes nothing.
    /// Downloading it is a different operation, so retrying this one is a loop.
    ModNotInstalled         => "MOD_NOT_INSTALLED",          Model, false;
    /// The circuit breaker is open (§142.4). Retryable, but the supervisor —
    /// not the caller — decides when the cooldown has elapsed.
    ModSuspended            => "MOD_SUSPENDED",              Model, true;
    ModInsufficientMemory   => "MOD_INSUFFICIENT_MEMORY",    Model, true;
    ModThermalThrottled     => "MOD_THERMAL_THROTTLED",      Model, true;
    ModOnBattery            => "MOD_ON_BATTERY",             Model, true;
    ModQueueFull            => "MOD_QUEUE_FULL",             Model, true;
    /// The asker went away before the request ran (SUP-006).
    ModDeadlineExpired      => "MOD_DEADLINE_EXPIRED",       Model, false;
    ModCancelled            => "MOD_CANCELLED",              Model, false;
    ModWorkerCrash          => "MOD_WORKER_CRASH",           Model, true;
    /// A download's SHA-256 did not match (SUP-014). The bytes on disk are
    /// wrong and must be discarded before anything is retried.
    ModIntegrityFailed      => "MOD_INTEGRITY_FAILED",       Model, false;
    ModScratchExceeded      => "MOD_SCRATCH_EXCEEDED",       Model, false;
    /// The model cannot do what was asked — no tool support, no thinking
    /// budget (GEN-013).
    ModUnsupportedCapability => "MOD_UNSUPPORTED_CAPABILITY", Model, false;

    // NET_ — egress (Part 9). Reaching outward is a different kind of failure
    // from a volume that will not mount, and a log that conflates them makes
    // "why did this not work" harder than it needs to be.
    NetUnreachable          => "NET_UNREACHABLE",     Network, true;
    NetTimeout              => "NET_TIMEOUT",         Network, true;
    /// The server answered, and not with success. Not retryable: the same
    /// request gets the same answer.
    NetBadStatus            => "NET_BAD_STATUS",      Network, false;
    /// A provider is throttling this key. Distinct from `NET_BAD_STATUS`
    /// precisely because it **is** retryable — the identical request succeeds
    /// in a minute — and folding it into the code that means "asking again
    /// changes nothing" would make the one recoverable remote failure look
    /// permanent.
    NetRateLimited          => "NET_RATE_LIMITED",    Network, true;

    // ACT_ — a write the user asked for, refused. Never retryable as-is: the
    // caller has to look at what changed and decide again.
    /// The file changed since the caller read it — the stale-version check. The user has
    /// it open in their editor, and the write would discard their work.
    ActStaleVersion         => "ACT_STALE_VERSION",   Action, false;
    /// The target exists and the caller asked to create, not replace. There is
    /// deliberately no unconditional-overwrite request to fall back to.
    ActAlreadyExists        => "ACT_ALREADY_EXISTS",  Action, false;
    /// The name is not one this system will create — a NUL, a control
    /// character, a reserved device name, an over-long component.
    ActNameRejected         => "ACT_NAME_REJECTED",   Action, false;
    /// The write happened and cannot be taken back — it replaced a file and no
    /// copy of the earlier content was kept. Distinct from a refusal on
    /// purpose: nothing was prevented here, and the caller is being told the
    /// state of the world rather than why their request was declined.
    ActNotReversible        => "ACT_NOT_REVERSIBLE", Action, false;
    /// The edit would leave the file structurally invalid, and it parsed
    /// before. A regression, not a validity judgement: a file that was already
    /// broken can still be edited, or nothing could ever repair one.
    ActValidationFailed     => "ACT_VALIDATION_FAILED", Action, false;

    // POL_ — policy (never retryable)
    PolDenied               => "POL_DENIED",                 Policy, false;
    PolApprovalRequired     => "POL_APPROVAL_REQUIRED",      Policy, false;
    PolClassificationBlocked => "POL_CLASSIFICATION_BLOCKED", Policy, false;

    // CFG_ — configuration
    CfgInvalid              => "CFG_INVALID",                Config, false;
    CfgUnsupportedVersion   => "CFG_UNSUPPORTED_VERSION",    Config, false;

    // INT_ — internal invariant violation; always a bug here, never the user's
    IntInvariantViolated    => "INT_INVARIANT_VIOLATED",     Internal, false;
}

impl Code {
    /// Whether a failure isolates to one file, leaving the workspace running
    /// (FS-011). Storage and policy failures do not.
    pub const fn isolates_to_one_file(self) -> bool {
        matches!(self.class(), Class::Filesystem | Class::Parse)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Filesystem,
    Parse,
    Index,
    Storage,
    Model,
    /// A write the user asked for. Distinct from `Policy`: a policy refusal is
    /// about what is allowed, and an action refusal is about what is *true* —
    /// the file moved, the name is not writable, something already exists.
    Action,
    /// Reaching outward (Part 9). Distinct from `Filesystem`: a host that will
    /// not answer and a volume that will not mount are different problems.
    Network,
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
    fn every_code_is_unique_and_screaming_snake() {
        // Over `Code::ALL`, which is generated — so a code added tomorrow is
        // covered by this test today.
        let set: std::collections::HashSet<&str> = Code::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(set.len(), Code::ALL.len(), "duplicate error code string");
        for c in Code::ALL {
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
    fn a_codes_prefix_matches_its_class() {
        // The prefix is how a code is read in a log; a `MOD_` code in the
        // storage class would make that reading wrong.
        for c in Code::ALL {
            let prefix = c.as_str().split('_').next().unwrap();
            let expected = match c.class() {
                Class::Filesystem => "FS",
                Class::Parse => "PAR",
                Class::Index => "IDX",
                Class::Storage => "DB",
                Class::Model => "MOD",
                Class::Action => "ACT",
                Class::Network => "NET",
                Class::Policy => "POL",
                Class::Config => "CFG",
                Class::Internal => "INT",
            };
            assert_eq!(
                prefix,
                expected,
                "{} is in class {:?}",
                c.as_str(),
                c.class()
            );
        }
    }

    #[test]
    fn a_model_resource_refusal_is_retryable_but_a_model_fact_is_not() {
        // §142.3: resource refusals describe a moment and are overridable;
        // the rest are facts about this model on this machine.
        for c in [
            Code::ModInsufficientMemory,
            Code::ModThermalThrottled,
            Code::ModOnBattery,
            Code::ModQueueFull,
        ] {
            assert!(c.retryable(), "{c} describes a moment, not a fact");
        }
        for c in [
            Code::ModNotInstalled,
            Code::ModIntegrityFailed,
            Code::ModUnsupportedCapability,
            Code::ModCancelled,
            Code::ModDeadlineExpired,
        ] {
            assert!(!c.retryable(), "{c} will not become true by waiting");
        }
    }

    #[test]
    fn every_code_round_trips_through_its_wire_form() {
        for c in Code::ALL {
            assert_eq!(Code::from_wire(c.as_str()), Some(*c));
            let json = serde_json::to_string(c).unwrap();
            assert_eq!(json, format!("{:?}", c.as_str()));
            assert_eq!(serde_json::from_str::<Code>(&json).unwrap(), *c);
        }
        // A code from a newer build is a version mismatch, not a default.
        assert_eq!(Code::from_wire("MOD_TIME_TRAVEL"), None);
        assert!(serde_json::from_str::<Code>("\"MOD_TIME_TRAVEL\"").is_err());
    }

    #[test]
    fn messages_name_an_action_not_just_a_failure() {
        // SUP-001. A message that only restates the code is a defect.
        let e: Error = std::io::Error::from(std::io::ErrorKind::PermissionDenied).into();
        assert_eq!(e.code(), Code::FsPermissionDenied);
        assert!(e.message().len() > 30, "message must explain, not label");
    }
}
