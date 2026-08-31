//! Marrow index — the lexical retrieval branch.
//!
//! Two things live here, and they exist for opposite reasons.
//!
//! [`fts5`] is the **index**: fast, ranked, structured, and only as fresh as
//! the last write. [`literal`] is the **escape hatch** (CAP-005): slower, exact,
//! and correct even when the index is empty, mid-rebuild or missing. A search
//! product with only the first is one that quietly lies after a crash; a search
//! product with only the second is `grep`.
//!
//! ```text
//!   marrow search "auth refresh"      marrow search --literal FOO_BAR
//!            │                                   │
//!            ▼                                   ▼
//!     TextIndex::search                   literal::literal_search
//!            │                                   │
//!      FTS5 in marrow.sqlite              the files themselves
//!      (same transaction as               (index never consulted;
//!       the canonical row — D3)            non-Resident files refused)
//! ```
//!
//! **Fusion is not this crate's job.** Part 6 §113 combines this branch with
//! the vector, path, symbol and recency branches; that belongs to `query`.
//! What this crate owes it is one branch's ranked candidates, each carrying
//! enough provenance to be rendered and cited.
//!
//! Everything here is derived state. Deleting the FTS5 tables and running
//! [`TextIndex::rebuild_from`] must reproduce the same answers — that is
//! the other half of "derived indexes are rebuildable; corrections are not",
//! and `derived_index_is_rebuildable_from_canonical` is the test.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod fts5;
pub mod literal;
pub mod port;
pub mod vector;

/// Every migration this crate contributes to the shared database.
///
/// **Composition roots must pass this, never a hand-written subset.** The
/// chain is numbered across crates (`marrow-store` owns 1, 3, 5 and 6, this crate
/// owns 2 and 4), and `Store::compose` rejects a chain that is unsorted, has a
/// clash, or has a gap — but a chain that merely *stops early* is
/// well-formed. `[1, 2, 3]` is a perfectly valid chain; it is also a build
/// that declares v3 and refuses to open the v4 database another root wrote.
///
/// That is not hypothetical. The CLI passed `fts5::MIGRATION` alone while the
/// desktop passed both, so every `marrow search`, `marrow status` and
/// `marrow mcp` against a real index failed with
/// `CFG_UNSUPPORTED_VERSION`. One list is the fix: adding a migration here
/// reaches every binary at once.
pub const MIGRATIONS: &[marrow_store::migrate::Migration] = &[fts5::MIGRATION, vector::MIGRATION];

/// The version a database is at once [`MIGRATIONS`] has been applied over
/// `marrow-store`'s chain.
///
/// Distinct from [`marrow_core::SCHEMA_VERSION`], which is only what
/// `marrow-store` alone would apply. The two agree whenever the store holds the
/// highest number in the chain, which it does at 7; they part again the next
/// time this crate takes one.
pub const SCHEMA_VERSION: i64 = 7;

pub use fts5::{Fts5Index, StoreChunkSource};
pub use literal::{
    literal_search, CaseMode, LiteralHit, LiteralOutcome, LiteralQuery, LiteralTarget, PatternKind,
    StopReason,
};
pub use port::{
    extension_of, ChunkSource, Embedding, FieldWeights, Filters, MatchMode, MatchRange, Snippet,
    SnippetOptions, TextDoc, TextField, TextHit, TextIndex, TextQuery, VectorDoc, VectorHit,
    VectorIndex, VectorQuery,
};
pub use vector::SqliteVectorIndex;
