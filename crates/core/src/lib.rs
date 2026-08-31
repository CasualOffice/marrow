//! Marrow core — domain types shared by every other crate.
//!
//! This crate holds the vocabulary and nothing else: no I/O, no SQL, no
//! filesystem. If something here needs a `std::fs` call, it belongs elsewhere.
//!
//! Three types in here encode non-negotiable invariants (Part 7 §126). They are
//! designed so the invariant is awkward to violate rather than merely
//! documented:
//!
//! - [`model::SourceSpan`] — provenance to an exact location (#1)
//! - [`model::TierState`] — cloud placeholders are never read (#5)
//! - [`model::Origin`] — self-written content cannot be cited (#13)

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod a1;
pub mod error;
pub mod id;
pub mod model;

pub use error::{Class, Code, Error, Result};
pub use id::{
    ChunkId, DeviceId, FileId, JobId, NodeId, ParseId, PathId, RequestId, RootId, VersionId,
    WorkspaceId,
};
pub use model::{
    ContentHash, FileStatus, Origin, ProvenanceClass, SourceSpan, TierState, Timestamp,
    VersionStatus,
};

/// The highest migration **this crate's chain** defines.
///
/// Not the version an opened database is at. The chain is numbered across
/// crates — `marrow-index` owns 2 and 4, `marrow-store` owns 1, 3, 5 and 6 — so a
/// live database opened by the app is at the maximum of the *composed* chain,
/// which is never lower than this and is only equal to it while the store
/// happens to hold the highest number. Read it from `schema_meta` when you want
/// to know what a database actually is; this is only what `marrow-store` alone
/// would apply.
pub const SCHEMA_VERSION: i64 = 6;

/// Chunk bodies larger than this go to the content-addressed cache rather than
/// inline in SQLite (Part 2 §50).
///
/// M0 F7: 70.6% of the real corpus is under 64 KB, so in practice almost
/// everything stays inline and the cache path is rarely exercised.
pub const INLINE_BLOB_LIMIT: usize = 64 * 1024;
