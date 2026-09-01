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
//! # Optional stages fail open. Required ones fail loudly.
//!
//! **Hard rule 10 is the whole of it: search works with no LLM, no GPU and no
//! network.** A stage that can take the entire query down when it breaks is a
//! violation of that rule waiting for its first bad row, so the split below is
//! a convention and not one branch's judgement call.
//!
//! A stage is **optional** when its failure makes the answer *thinner*: fewer
//! candidates, a plainer snippet, an explanation with less in it. Those catch
//! their error, log it at `warn` with enough context to find the cause, and
//! carry on with what the required stages already produced. A caller must be
//! able to tell that it happened — [`SearchResults::branches`] names the
//! branches that actually ran, and [`explain::explain`] states in words when only one
//! did — because a thin answer presented as a complete one is its own defect.
//!
//! A stage is **required** when its failure would make the answer *wrong*.
//! Those propagate. Degrading them would substitute a plausible answer for a
//! true one, which is worse than an error, and the caller loses the one signal
//! that would have told them not to trust what they are reading.
//!
//! | Stage | Failure |
//! |---|---|
//! | Lexical retrieval ([`search::search_hybrid`]) | **Propagates.** It is the answer, not an enrichment. There is nothing to degrade to |
//! | Workspace resolution, filter resolution | **Propagates.** An unresolvable `--workspace` returns zero hits, and zero hits for a typo reads as "nothing is indexed" |
//! | Semantic branch ([`marrow_index::VectorIndex::search`]) | Degrades. The branch drops out; lexical answers alone |
//! | Hydrating semantic-only candidates | Degrades. Those candidates are lost; every lexical hit survives |
//! | A fused candidate with no hit behind it | Degrades. Dropped with a warning — a result nobody can open or cite is not a result |
//! | `--explain` assembly ([`explain::explain`]) | Cannot fail: pure, and a projection of a decision already made |
//! | [`file_intelligence`] | **Propagates.** The panel *is* the answer. A section that silently vanished would let a reader conclude a file has no duplicates, or that its parse succeeded |
//! | [`catalog::index_stats`], [`catalog::workspace_stats`] | **Propagates.** A health number that degrades to zero is a wrong number, and these are read precisely to decide whether to trust the index. The one absence handled in-band is the vector table, because a database composed without it is a legitimate state rather than a failure |
//!
//! Two rules from [Part 6 §113.3] are enforced here because they are
//! correctness, not ranking:
//!
//! - `origin = SelfWritten` is down-weighted **and flagged**, so a caller can
//!   bar it from evidence (the `origin = SELF` rule). See [`search::Hit::can_support_a_claim`].
//! - Anything short of `ProvenanceClass::Exact` is down-weighted (CONV-005).
//!
//! # Ranking changes are measured, not argued
//!
//! The tests next door are correctness tests, and correctness is not quality:
//! nothing in them can tell you whether changing [`search::RRF_K`], the
//! [`search::SEMANTIC_WEIGHT`]/[`search::LEXICAL_WEIGHT`] ratio or
//! `marrow_index::FieldWeights` made results better or worse. `tests/eval.rs`
//! can. It scores a committed fixture corpus (`eval/`) against a golden query
//! set and fails on a drop beyond a stated tolerance — [Part 6 §113.4]'s
//! tuning protocol, with something to actually run.
//!
//! **Touch any of those numbers and run it.** It needs no model, no GPU and no
//! network, and it takes about five seconds:
//!
//! ```text
//! cargo test -p marrow-query --test eval
//! MARROW_EVAL_SHOW=q06 cargo test -p marrow-query --test eval -- --nocapture
//! ```
//!
//! [Part 6 §113.3]: ../../../docs/Part_6_Engineering_Reference.md
//! [Part 6 §113.4]: ../../../docs/Part_6_Engineering_Reference.md

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod catalog;
pub mod explain;
/// Everything the index knows about one file, read once and rendered three
/// ways. Two surfaces carried the same query and reached different verdicts.
pub mod files;
pub mod intelligence;
/// Whether a recorded file is still on the disk, and what that means for
/// citing it. Shared so two surfaces cannot answer it differently.
pub mod presence;
pub mod search;
/// Arithmetic over a spreadsheet range, computed here rather than by a model.
pub mod table;

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
