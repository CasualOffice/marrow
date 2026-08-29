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
//! invariant #11's other half, and `derived_index_is_rebuildable_from_canonical`
//! is the test.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod fts5;
pub mod literal;
pub mod port;

pub use fts5::{Fts5Index, StoreChunkSource};
pub use literal::{
    literal_search, CaseMode, LiteralHit, LiteralOutcome, LiteralQuery, LiteralTarget, PatternKind,
    StopReason,
};
pub use port::{
    extension_of, ChunkSource, FieldWeights, Filters, MatchMode, MatchRange, Snippet,
    SnippetOptions, TextDoc, TextField, TextHit, TextIndex, TextQuery,
};
