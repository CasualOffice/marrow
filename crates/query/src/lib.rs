//! Marrow query — the read path.
//!
//! The mirror of `marrow-ingest`. Ingest joins `scan`, `parse`, `store` and
//! `index` on the way in; this crate joins `store` and `index` on the way out.
//! It is **orchestration only**: it owns no state, opens no long-lived handle,
//! caches nothing and holds no engine type. Everything it knows it asks for.
//!
//! ```text
//!   marrow search "auth refresh"            marrow file q2-report.xlsx
//!            │                                        │
//!            ▼                                        ▼
//!     query::search                          query::file_intelligence
//!       ├─ resolve workspace name ─▶ Store       one read transaction over
//!       ├─ branch: lexical ────────▶ TextIndex   files / file_paths /
//!       ├─ fuse (RRF, §113.2)                    file_versions / parse_results
//!       ├─ multipliers (§113.3)                  / chunks / jobs
//!       └─ hydrate paths ─────────▶ Store
//! ```
//!
//! Three things this crate deliberately is not:
//!
//! - **Not a fusion framework.** M1 has one branch. [`search::rrf`] takes a
//!   `Vec<Branch>` so the vector branch slots in at M4 without a rewrite, and
//!   that is the whole of the extensibility budget (LLD §3).
//! - **Not a cache.** FI-005: the file-intelligence panel is a read model
//!   assembled on demand from canonical state. At M0's 9.4k files that is
//!   single-digit milliseconds; a cache would be pure liability.
//! - **Not engine-aware.** It depends on the [`marrow_index::TextIndex`]
//!   *trait*, never on `Fts5Index`. That port exists so this crate does not
//!   care what is behind it.
//!
//! Two rules from [Part 6 §113.3] are enforced here because they are
//! correctness, not ranking:
//!
//! - `origin = SelfWritten` is down-weighted **and flagged**, so a caller can
//!   bar it from evidence (invariant #13). See [`search::Hit::can_support_a_claim`].
//! - Anything short of `ProvenanceClass::Exact` is down-weighted (CONV-005).
//!
//! [Part 6 §113.3]: ../../../docs/Part_6_Engineering_Reference.md

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod explain;
pub mod intelligence;
pub mod search;

pub use explain::{
    citable, explain, origin_is_citable, summarize, BranchExplanation, Explanation, HitExplanation,
};
pub use intelligence::{
    file_intelligence, ChunkSummary, Duplicate, EntityMention, FileIntelligence, FileLocation,
    FileRef, Identity, IndexError, IndexState, KindCount, MetadataField, OutlineEntry, ParseState,
    PathEvent, PendingJob, TimelineEvent, UnansweredSection, VersionSummary, Versions,
};
pub use search::{
    mode_label, multipliers_for, relative_path, rrf, search, workspace_id_for, workspaces,
    AppliedMultiplier, Branch, BranchRank, FusedCandidate, Hit, SearchFilters, SearchRequest,
    SearchResults, WorkspaceInfo, CANDIDATE_DEPTH, DEFAULT_LIMIT, LEXICAL, LEXICAL_WEIGHT, RRF_K,
    SELF_WRITTEN_MULTIPLIER,
};
