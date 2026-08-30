//! Search: branch retrieval, fusion, post-fusion multipliers, hydration.
//!
//! The shape follows [Part 6 §113.1] with the branches M1 actually has:
//!
//! ```text
//! request
//!   → validate (an empty query is refused, not silently answered)
//!   → resolve the workspace *name* to an id (a typo is an error, not 0 hits)
//!   → per-branch candidates       (M1: lexical only, depth 100)
//!   → RRF fusion                  (§113.2, k = 60)
//!   → post-fusion multipliers     (§113.3, the two that are correctness)
//!   → hydrate                     (workspace name + workspace-relative path)
//! ```
//!
//! **There is no fusion framework here.** With one branch, RRF is a monotone
//! transform of the branch's own order and changes nothing; it is written out
//! anyway because [`rrf`] is where the vector branch lands at M4, and a
//! `Vec<Branch>` plus one function is a far cheaper seam than retrofitting
//! fusion into a single-branch pipeline. Anything more — a registry, a trait, a
//! plugin — would be building for a milestone that has not been scoped.
//!
//! [Part 6 §113.1]: ../../../docs/Part_6_Engineering_Reference.md

use std::collections::HashMap;
use std::time::Instant;

use marrow_core::{ChunkId, Code, Error, Origin, ProvenanceClass, Result, Timestamp, WorkspaceId};
use marrow_index::{Embedding, Filters, MatchMode, TextHit, TextIndex, TextQuery, VectorIndex};
use marrow_store::rusqlite::{params, Connection};
use marrow_store::{map_sqlite, ReadConn, Store};
use serde::Serialize;

// ------------------------------------------------------------------ constants

/// How many results a caller gets when it does not say.
///
/// UX §4 renders a screenful; more than this is a pager, which is a different
/// product.
pub const DEFAULT_LIMIT: usize = 20;

/// Candidates pulled from each branch before fusion (§113.1's default).
///
/// Always at least this many even when `limit` is small, because §113.3's
/// multipliers can only reorder results that were retrieved: asking the index
/// for exactly `limit` rows would let a down-weighted hit keep a slot a better
/// one should have taken.
pub const CANDIDATE_DEPTH: usize = 100;

/// RRF's rank-smoothing constant (§113.2).
pub const RRF_K: f32 = 60.0;

/// The lexical branch's name, as it appears in [`SearchResults::branches`] and
/// in `search --explain`.
pub const LEXICAL: &str = "lexical";

/// The semantic branch's name.
pub const SEMANTIC: &str = "semantic";

/// The lexical branch's fusion weight (§113.2 table).
pub const LEXICAL_WEIGHT: f32 = 1.0;

/// The semantic branch's weight.
///
/// Deliberately below lexical, and this is the second branch §113.4 was
/// waiting for. The reason is not that embeddings are worse — it is that this
/// product's promise is a citation to an exact span, and a lexical hit is one
/// the user can *see* is a hit. A semantic result they cannot trace to a word
/// on the page reads as a guess, so it earns its place by agreeing with the
/// lexical branch more often than by outvoting it.
///
/// A parameter, not a truth. §113.4 wants these in config and versioned; they
/// are here until there is a measurement to set them from.
pub const SEMANTIC_WEIGHT: f32 = 0.8;

/// `origin = SELF` multiplier (§113.3).
///
/// The multiplier is the *soft* half of invariant #13 and is not what enforces
/// it. The hard half is [`Hit::can_support_a_claim`], which is a flag a caller
/// checks, not a number it compares.
pub const SELF_WRITTEN_MULTIPLIER: f32 = 0.5;

// -------------------------------------------------------------------- request

/// Filters in the caller's vocabulary.
///
/// The difference from [`marrow_index::Filters`] is `workspace`: callers know a
/// workspace by the **name** they typed (`marrow search --workspace desktop`),
/// and the index knows only a [`WorkspaceId`]. Resolving that is this crate's
/// job, and doing it here is what makes a typo an error instead of zero
/// results.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchFilters {
    /// Workspace **name**, resolved to an id by [`search`].
    pub workspace: Option<String>,
    /// SQLite `GLOB` syntax (`*`, `?`, `[...]`). Bound as a parameter, never
    /// interpolated.
    pub path_glob: Option<String>,
    /// With or without the leading dot; case is ignored.
    pub extension: Option<String>,
    /// Inclusive bounds on the source file's mtime.
    pub modified_after: Option<Timestamp>,
    pub modified_before: Option<Timestamp>,
}

impl SearchFilters {
    pub fn is_empty(&self) -> bool {
        *self == SearchFilters::default()
    }
}

/// One search.
#[derive(Clone, Debug)]
pub struct SearchRequest {
    /// Raw user input. Untrusted; it reaches the index as bound tokens only.
    pub text: String,
    pub limit: usize,
    pub mode: MatchMode,
    pub filters: SearchFilters,
}

impl SearchRequest {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            limit: DEFAULT_LIMIT,
            mode: MatchMode::default(),
            filters: SearchFilters::default(),
        }
    }

    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    pub fn mode(mut self, mode: MatchMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn filters(mut self, filters: SearchFilters) -> Self {
        self.filters = filters;
        self
    }
}

/// The stable wire name of a match mode. `MatchMode` is not `Serialize` (it is
/// a port type and the port owes serde nothing), and every renderer needs a
/// label, so the mapping lives here once.
pub fn mode_label(mode: MatchMode) -> &'static str {
    match mode {
        MatchMode::Terms => "terms",
        MatchMode::Phrase => "phrase",
        MatchMode::Prefix => "prefix",
        MatchMode::Any => "any",
    }
}

// ------------------------------------------------------------------- branches

/// One retrieval branch's ranked candidates (§113.2).
///
/// `ranked` is best-first; a chunk's rank is its 0-based position + 1. The
/// branch's own score is deliberately absent: RRF fuses by **rank**, so branch
/// scores never need normalizing and must never be compared across branches.
#[derive(Clone, Debug)]
pub struct Branch {
    pub name: &'static str,
    pub weight: f32,
    pub ranked: Vec<ChunkId>,
}

/// A chunk's 1-based rank inside one branch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct BranchRank {
    pub branch: &'static str,
    pub rank: usize,
}

/// A fused candidate: RRF score plus the ranks it was computed from.
#[derive(Clone, Debug)]
pub struct FusedCandidate {
    pub chunk_id: ChunkId,
    /// `Σ_b w_b / (k + rank_b)`.
    pub score: f32,
    /// Only the branches that actually returned this chunk.
    pub branch_ranks: Vec<BranchRank>,
}

/// Reciprocal Rank Fusion (§113.2): `score(d) = Σ_b w_b / (k + rank_b(d))`.
///
/// Best-first, with `chunk_id` as the tie-break so the same inputs always
/// produce the same order — a search that reorders equal-scoring results
/// between runs looks broken even when it is not.
pub fn rrf(branches: &[Branch], k: f32) -> Vec<FusedCandidate> {
    let mut acc: HashMap<ChunkId, FusedCandidate> = HashMap::new();
    for b in branches {
        for (i, id) in b.ranked.iter().enumerate() {
            let rank = i + 1;
            let entry = acc.entry(*id).or_insert_with(|| FusedCandidate {
                chunk_id: *id,
                score: 0.0,
                branch_ranks: Vec::new(),
            });
            entry.score += b.weight / (k + rank as f32);
            entry.branch_ranks.push(BranchRank {
                branch: b.name,
                rank,
            });
        }
    }
    let mut out: Vec<FusedCandidate> = acc.into_values().collect();
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.chunk_id.cmp(&b.chunk_id))
    });
    out
}

// --------------------------------------------------------------- multipliers

/// One post-fusion multiplier that was applied, and why (§113.3, RET-004).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct AppliedMultiplier {
    pub reason: &'static str,
    pub factor: f32,
}

/// The §113.3 multipliers M1 applies, in the order they are applied.
///
/// Only two of the seven rows in that table are here, and on purpose: these two
/// are **correctness**, not ranking. A `SELF`-written file that outranks the
/// document it summarised is a citation loop (invariant #13); an `APPROXIMATE`
/// hit that outranks an exact one is silent precision loss (CONV-005). The rest
/// of the table — pins, exact-filename boosts, staleness, recency decay — is
/// tuning, and §113.4 says tuning waits for measurement.
///
/// An empty result means nothing was down-weighted, which is the common case.
pub fn multipliers_for(hit: &TextHit) -> Vec<AppliedMultiplier> {
    let mut out = Vec::new();
    if hit.origin == Origin::SelfWritten {
        out.push(AppliedMultiplier {
            reason: "origin = SELF: written by Marrow, findable but never evidence (#13)",
            factor: SELF_WRITTEN_MULTIPLIER,
        });
    }
    if hit.provenance != ProvenanceClass::Exact {
        out.push(AppliedMultiplier {
            reason: match hit.provenance {
                ProvenanceClass::Degraded => {
                    "provenance = DEGRADED: converted through a lossy path"
                }
                ProvenanceClass::Approximate => "provenance = APPROXIMATE: reconstructed, not read",
                ProvenanceClass::MetadataOnly => {
                    "provenance = METADATA_ONLY: no content was parsed"
                }
                // Unreachable: guarded by the `!=` above. Spelled out rather
                // than `unreachable!()` because a panic in the hot path to
                // avoid one `match` arm is a bad trade.
                ProvenanceClass::Exact => "provenance = EXACT",
            },
            // The factors live in `marrow_core` next to the enum, so ranking
            // and rendering cannot disagree about what DEGRADED costs.
            factor: hit.provenance.rank_multiplier(),
        });
    }
    out
}

// -------------------------------------------------------------------- results

/// One result: the branch's hit, plus what a renderer needs and the index
/// cannot know.
///
/// The index sees an absolute path and a `WorkspaceId`; it has never heard of
/// the workspace's *name* and has no root to make the path relative to. Both
/// come from canonical state, which is why hydration happens here.
///
/// Derefs to the underlying [`TextHit`], so `hit.path`, `hit.snippet` and
/// `hit.span` all work directly. `fused_score` is named apart from
/// [`TextHit::score`] deliberately: the latter is a **branch-local** BM25
/// score and comparing it against anything outside its own branch is wrong.
#[derive(Clone, Debug)]
pub struct Hit {
    /// The lexical branch's hit, unmodified.
    pub hit: TextHit,
    /// The name of the workspace this file belongs to.
    pub workspace: String,
    /// The path with its workspace root stripped — `src/auth/token.rs`, not
    /// `/Users/…/proj/src/auth/token.rs`. Falls back to the absolute path when
    /// no root matches.
    pub relative_path: String,
    /// **Invariant #13.** `false` for `origin = SelfWritten`: the content is
    /// findable and rendered, and may never support a claim. A caller that
    /// assembles evidence filters on this flag, never on the score.
    pub can_support_a_claim: bool,
    /// Final rank, 1-based.
    pub rank: usize,
    /// RRF score before §113.3's multipliers.
    pub base_score: f32,
    /// `base_score` times every applied multiplier. This is the sort key.
    pub fused_score: f32,
    /// The branches that returned this chunk, with its rank in each.
    pub branch_ranks: Vec<BranchRank>,
    /// The multipliers applied to reach `fused_score`. Empty means none.
    pub multipliers: Vec<AppliedMultiplier>,
}

impl std::ops::Deref for Hit {
    type Target = TextHit;
    fn deref(&self) -> &TextHit {
        &self.hit
    }
}

/// What one search produced.
#[derive(Clone, Debug)]
pub struct SearchResults {
    /// Best first, at most `request.limit` long.
    pub hits: Vec<Hit>,
    /// Candidates that survived fusion, **before** the limit was applied. Not a
    /// corpus-wide match count: branch retrieval is capped at
    /// [`CANDIDATE_DEPTH`], so this saturates rather than growing without bound.
    pub total: usize,
    pub elapsed_ms: u128,
    /// The branches that ran. One in M1.
    pub branches: Vec<&'static str>,
}

// ------------------------------------------------------------------ workspaces

/// A workspace and the roots consented into it.
#[derive(Clone, Debug)]
pub struct WorkspaceInfo {
    pub workspace_id: WorkspaceId,
    pub name: String,
    /// Canonical root paths, **longest first** so that nested roots strip
    /// correctly.
    pub roots: Vec<String>,
}

/// Every workspace, with its roots. Ordered by name.
pub fn workspaces(conn: &ReadConn) -> Result<Vec<WorkspaceInfo>> {
    load_workspaces(conn)
}

fn load_workspaces(conn: &Connection) -> Result<Vec<WorkspaceInfo>> {
    const SQL: &str = "SELECT w.workspace_id, w.name, r.canonical_path
                         FROM workspaces w
                         LEFT JOIN workspace_roots r ON r.workspace_id = w.workspace_id
                        ORDER BY w.name, r.canonical_path";
    let mut stmt = conn
        .prepare(SQL)
        .map_err(|e| map_sqlite(e, "Could not read the list of workspaces."))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(|e| map_sqlite(e, "Could not read the list of workspaces."))?;

    let mut out: Vec<WorkspaceInfo> = Vec::new();
    for r in rows {
        let (id, name, root) = r.map_err(|e| map_sqlite(e, "Could not decode a workspace row."))?;
        let workspace_id = parse_workspace_id(&id)?;
        match out.last_mut() {
            Some(w) if w.workspace_id == workspace_id => {
                if let Some(p) = root {
                    w.roots.push(p);
                }
            }
            _ => out.push(WorkspaceInfo {
                workspace_id,
                name,
                roots: root.into_iter().collect(),
            }),
        }
    }
    // Longest first: a nested root would otherwise be stripped by its parent
    // and leave a misleading prefix on the relative path.
    for w in &mut out {
        w.roots.sort_by_key(|p| std::cmp::Reverse(p.len()));
    }
    Ok(out)
}

fn parse_workspace_id(s: &str) -> Result<WorkspaceId> {
    s.parse::<WorkspaceId>().map_err(|e| {
        Error::new(
            Code::DbCorrupt,
            "The index database holds a workspace identifier that is not a ULID. Delete the \
             index directory to rebuild it from your files.",
        )
        .with_context(format!("workspaces.workspace_id = {s:?}"))
        .with_source(e)
    })
}

/// The path with its longest matching root stripped.
///
/// An absolute path eats the terminal width the snippet needs and buries the
/// part that distinguishes one result from another (UX §4). A path under no
/// known root is returned unchanged rather than mangled.
pub fn relative_path(path: &str, roots: &[String]) -> String {
    roots
        .iter()
        .filter(|r| path.starts_with(r.as_str()))
        .max_by_key(|r| r.len())
        .and_then(|r| path.strip_prefix(r.as_str()))
        .map(|s| s.trim_start_matches(['/', '\\']).to_string())
        .unwrap_or_else(|| path.to_string())
}

// --------------------------------------------------------------------- search

/// Run one search.
///
/// A fresh reader is opened per call: this crate owns no state (LLD §1), and a
/// `query_only` WAL reader costs a connection open plus four pragmas — real,
/// but far below the 50 ms first-result budget (LLD §8) and cheaper than the
/// lifetime question a cached handle would raise.
/// Fetch chunks the semantic branch found and the lexical branch did not.
///
/// A `TextHit` is what everything downstream renders and cites from, so a
/// semantic-only candidate has to become one. The snippet is the chunk's own
/// opening rather than a match window: there is no matched term to centre on,
/// and inventing highlight markers would claim a match that did not happen.
fn hydrate_chunks(conn: &marrow_store::ReadConn, ids: &[ChunkId]) -> Result<Vec<TextHit>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    const PREVIEW: usize = 320;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let row = conn
            .query_row(
                "SELECT c.text, COALESCE(c.context_prefix,''), c.provenance_class,
                        v.path_at_observation, v.mtime_ms, f.file_id, f.workspace_id, f.origin,
                        c.version_id
                   FROM chunks c
                   JOIN file_versions v ON v.version_id = c.version_id
                   JOIN files f ON f.file_id = v.file_id
                  WHERE c.chunk_id = ?1 AND c.status = 'ACTIVE'",
                [id.to_string()],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, String>(7)?,
                        r.get::<_, String>(8)?,
                    ))
                },
            )
            .map(Some)
            .or_else(|e| match e {
                marrow_store::rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(marrow_store::map_sqlite(
                    other,
                    "hydrating a semantic result",
                )),
            })?;

        // A vector for a chunk the canonical store no longer has. The derived
        // index is rebuildable, so this is a stale row rather than a crisis —
        // but it must not become a result nobody can open.
        let Some((text, title, provenance, path, mtime, file, ws, origin, version)) = row else {
            tracing::warn!(chunk = %id, "the vector index has a chunk the store does not");
            continue;
        };
        let (Ok(file_id), Ok(workspace_id), Ok(version_id)) = (
            file.parse(),
            ws.parse(),
            version.parse::<marrow_core::VersionId>(),
        ) else {
            continue;
        };

        let preview: String = text.chars().take(PREVIEW).collect();
        out.push(TextHit {
            chunk_id: *id,
            file_id,
            version_id,
            workspace_id,
            path,
            title,
            // Branch-local and never compared across branches (§113.2). The
            // fusion uses rank; this is here because the type has the field.
            score: 0.0,
            span: marrow_core::SourceSpan::Whole,
            snippet: marrow_index::Snippet {
                text: preview,
                // No matched term to point at. Claiming one would put a
                // highlight on a word the user never searched for.
                matches: Vec::new(),
            },
            provenance: match provenance.as_str() {
                "DEGRADED" => marrow_core::ProvenanceClass::Degraded,
                "APPROXIMATE" => marrow_core::ProvenanceClass::Approximate,
                "METADATA_ONLY" => marrow_core::ProvenanceClass::MetadataOnly,
                _ => marrow_core::ProvenanceClass::Exact,
            },
            origin: if origin == "SELF" {
                marrow_core::Origin::SelfWritten
            } else {
                marrow_core::Origin::User
            },
            modified: marrow_core::Timestamp::from_millis(mtime),
        });
    }
    Ok(out)
}

pub fn search(store: &Store, index: &dyn TextIndex, req: &SearchRequest) -> Result<SearchResults> {
    search_hybrid(store, index, None, req)
}

/// Search with the semantic branch as well, when there is one.
///
/// Two branches rather than a "semantic mode": a question is answered better by
/// both than by either, and a mode switch makes the user responsible for
/// guessing which kind of query they have typed. RRF fuses them by rank, so
/// neither branch's scores need normalizing against the other's — which is
/// what makes adding a branch cheap.
///
/// `vectors` is `None` when no embedding model is installed, when the backfill
/// has not run, or when this query could not be embedded. In every one of those
/// cases lexical search answers alone (hard rule 10), and the result says which
/// branches ran rather than pretending both did.
pub fn search_hybrid(
    store: &Store,
    index: &dyn TextIndex,
    vectors: Option<(&dyn VectorIndex, &Embedding)>,
    req: &SearchRequest,
) -> Result<SearchResults> {
    let started = Instant::now();
    validate(req)?;

    let reader = store.reader()?;
    let spaces = load_workspaces(&reader)?;
    let filters = resolve_filters(&req.filters, &spaces)?;
    let workspace_filter = filters.workspace;

    // Retrieve deeper than the caller asked for, so the multipliers below have
    // something to reorder. See CANDIDATE_DEPTH.
    let q = TextQuery::new(req.text.clone())
        .mode(req.mode)
        .with_filters(filters)
        .limit(req.limit.max(CANDIDATE_DEPTH));
    let text_hits = index.search(&q)?;

    let mut branches = vec![Branch {
        name: LEXICAL,
        weight: LEXICAL_WEIGHT,
        ranked: text_hits.iter().map(|h| h.chunk_id).collect(),
    }];

    // The semantic branch returns chunk ids the lexical branch may never have
    // seen, so its hits are hydrated separately below. A vector failure is not
    // a search failure: the branch drops out with a line in the log, because
    // returning nothing would be worse than returning the lexical half.
    let mut vector_hits: Vec<marrow_index::VectorHit> = Vec::new();
    if let Some((store_of_vectors, embedding)) = vectors {
        let vq =
            marrow_index::VectorQuery::new(embedding.clone()).limit(req.limit.max(CANDIDATE_DEPTH));
        let vq = match workspace_filter {
            Some(w) => vq.workspace(w),
            None => vq,
        };
        match store_of_vectors.search(&vq) {
            Ok(hits) => {
                branches.push(Branch {
                    name: SEMANTIC,
                    weight: SEMANTIC_WEIGHT,
                    ranked: hits.iter().map(|h| h.chunk_id).collect(),
                });
                vector_hits = hits;
            }
            Err(e) => tracing::warn!(error = %e, "the semantic branch failed; answering lexically"),
        }
    }

    let fused = rrf(&branches, RRF_K);
    let total = fused.len();

    // Semantic-only candidates need their text fetched: the lexical branch
    // never saw them, so there is no `TextHit` to render from.
    let semantic_only: Vec<ChunkId> = vector_hits
        .iter()
        .map(|h| h.chunk_id)
        .filter(|id| !text_hits.iter().any(|t| t.chunk_id == *id))
        .collect();
    let hydrated = hydrate_chunks(&reader, &semantic_only)?;

    let mut by_chunk: HashMap<ChunkId, &TextHit> =
        text_hits.iter().map(|h| (h.chunk_id, h)).collect();
    by_chunk.extend(hydrated.iter().map(|h| (h.chunk_id, h)));
    let by_workspace: HashMap<WorkspaceId, &WorkspaceInfo> =
        spaces.iter().map(|w| (w.workspace_id, w)).collect();

    let mut hits: Vec<Hit> = Vec::with_capacity(total);
    for candidate in fused {
        // A chunk that fused but has no hit behind it cannot happen with one
        // branch, and at M4 it means a branch returned an id the hydration
        // step could not resolve. Dropping it is right either way: a result we
        // cannot render or cite is not a result.
        let Some(hit) = by_chunk.get(&candidate.chunk_id) else {
            // A chunk the vector index knows and the canonical store does not.
            // Dropping it is right: a result that cannot be rendered or cited
            // is not a result, and the derived index is rebuildable.
            tracing::warn!(chunk = %candidate.chunk_id, "fused candidate with no hit; dropped");
            continue;
        };
        let multipliers = multipliers_for(hit);
        let fused_score = multipliers
            .iter()
            .fold(candidate.score, |acc, m| acc * m.factor);
        let ws = by_workspace.get(&hit.workspace_id);
        hits.push(Hit {
            workspace: ws.map(|w| w.name.clone()).unwrap_or_default(),
            relative_path: match ws {
                Some(w) => relative_path(&hit.path, &w.roots),
                None => hit.path.clone(),
            },
            can_support_a_claim: hit.origin.can_support_a_claim(),
            rank: 0,
            base_score: candidate.score,
            fused_score,
            branch_ranks: candidate.branch_ranks,
            multipliers,
            hit: (*hit).clone(),
        });
    }

    // Re-sort: the multipliers are exactly the thing that can change the order
    // RRF produced, which is the point of applying them.
    hits.sort_by(|a, b| {
        b.fused_score
            .partial_cmp(&a.fused_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.hit.chunk_id.cmp(&b.hit.chunk_id))
    });
    hits.truncate(req.limit);
    for (i, h) in hits.iter_mut().enumerate() {
        h.rank = i + 1;
    }

    let elapsed_ms = started.elapsed().as_millis();
    tracing::debug!(hits = hits.len(), total, elapsed_ms, "search");
    Ok(SearchResults {
        hits,
        total,
        elapsed_ms,
        branches: branches.iter().map(|b| b.name).collect(),
    })
}

/// Refuse a query that has nothing to match on.
///
/// The lexical adapter refuses the same input for the same reason, but the
/// check is here too so the error is identical whether or not an index is
/// wired up, and so a caller that never reaches the index still gets told.
fn validate(req: &SearchRequest) -> Result<()> {
    // `unicode61` treats every non-alphanumeric character as a separator, so
    // this is exactly "would tokenize to nothing".
    if !req.text.chars().any(char::is_alphanumeric) {
        // §108 has no query-input class. CFG_INVALID is the closest and says
        // the right thing — what this build was handed is not usable input —
        // and it is what `marrow-index` already returns for the same case.
        return Err(Error::new(
            Code::CfgInvalid,
            // Deliberately names no flag and no tool. This is a library, and
            // three surfaces show this message — a CLI flag, an MCP tool and a
            // desktop control. Each names its own; a message that names one of
            // them is wrong on the other two, which is a suggestion that leads
            // nowhere.
            "A search needs at least one letter or digit. Type a word to search for, or run \
             an exact scan to match punctuation.",
        )
        .with_context(format!(
            "query had {} characters, 0 searchable terms",
            req.text.chars().count()
        )));
    }
    Ok(())
}

/// Map caller-facing filters onto the index's, resolving the workspace name.
///
/// A name that matches nothing is an **error**. Passing an unresolvable filter
/// through would return zero hits, and zero hits for a typo is the worst
/// outcome available: it reads as "nothing is indexed" and sends the user to
/// debug their corpus instead of their command line.
fn resolve_filters(f: &SearchFilters, spaces: &[WorkspaceInfo]) -> Result<Filters> {
    let workspace = match &f.workspace {
        None => None,
        Some(name) => Some(resolve_workspace_name(name, spaces)?),
    };
    Ok(Filters {
        workspace,
        path_glob: f.path_glob.clone(),
        extensions: f
            .extension
            .iter()
            .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
            .filter(|e| !e.is_empty())
            .collect(),
        modified_after: f.modified_after,
        modified_before: f.modified_before,
    })
}

fn resolve_workspace_name(name: &str, spaces: &[WorkspaceInfo]) -> Result<WorkspaceId> {
    if let Some(w) = spaces.iter().find(|w| w.name == name) {
        return Ok(w.workspace_id);
    }
    // Case-insensitive second pass: workspace names are typed by hand and the
    // schema's unique index is case-sensitive, so `Desktop` vs `desktop` is a
    // near-certain typo rather than two workspaces.
    let lower = name.to_lowercase();
    let mut ci = spaces
        .iter()
        .filter(|w| w.name.to_lowercase() == lower)
        .collect::<Vec<_>>();
    if ci.len() == 1 {
        if let Some(w) = ci.pop() {
            return Ok(w.workspace_id);
        }
    }

    let known: Vec<&str> = spaces.iter().map(|w| w.name.as_str()).collect();
    let known = if known.is_empty() {
        "there are no workspaces yet; add one with `marrow workspace add <path>`".to_string()
    } else {
        format!("known workspaces: {}", known.join(", "))
    };
    // CFG_INVALID for the same reason as `validate`: a name that does not
    // resolve is unusable input, not a missing file.
    Err(Error::new(
        Code::CfgInvalid,
        format!("There is no workspace named {name:?} — {known}."),
    )
    .with_context(format!("filter workspace = {name:?}")))
}

// ------------------------------------------------------------------- lookups

/// Resolve a workspace name to its id, or say why it cannot be resolved.
///
/// Exposed because every caller that accepts a `--workspace` flag needs the
/// same error, and duplicating it would let the two drift.
pub fn workspace_id_for(store: &Store, name: &str) -> Result<WorkspaceId> {
    let reader = store.reader()?;
    let spaces = load_workspaces(&reader)?;
    resolve_workspace_name(name, &spaces)
}

/// The workspace a file belongs to, and the roots to make its path relative to.
pub(crate) fn workspace_of(conn: &Connection, workspace_id: WorkspaceId) -> Result<WorkspaceInfo> {
    let name: String = conn
        .query_row(
            "SELECT name FROM workspaces WHERE workspace_id = ?1",
            params![workspace_id.to_string()],
            |r| r.get(0),
        )
        .map_err(|e| map_sqlite(e, "Could not read the workspace a file belongs to."))?;
    let mut stmt = conn
        .prepare(
            "SELECT canonical_path FROM workspace_roots WHERE workspace_id = ?1
              ORDER BY canonical_path",
        )
        .map_err(|e| map_sqlite(e, "Could not read a workspace's roots."))?;
    let rows = stmt
        .query_map(params![workspace_id.to_string()], |r| r.get::<_, String>(0))
        .map_err(|e| map_sqlite(e, "Could not read a workspace's roots."))?;
    let mut roots = Vec::new();
    for r in rows {
        roots.push(r.map_err(|e| map_sqlite(e, "Could not decode a workspace root row."))?);
    }
    roots.sort_by_key(|p| std::cmp::Reverse(p.len()));
    Ok(WorkspaceInfo {
        workspace_id,
        name,
        roots,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use marrow_core::{FileId, SourceSpan, VersionId};
    use marrow_index::Snippet;

    fn hit(origin: Origin, provenance: ProvenanceClass) -> TextHit {
        TextHit {
            chunk_id: ChunkId::new(),
            file_id: FileId::new(),
            version_id: VersionId::new(),
            workspace_id: WorkspaceId::new(),
            path: "/root/a.md".into(),
            title: String::new(),
            score: 1.0,
            span: SourceSpan::Whole,
            snippet: Snippet::default(),
            provenance,
            origin,
            modified: Timestamp::EPOCH,
        }
    }

    #[test]
    fn rrf_is_rank_based_and_deterministic() {
        let a = ChunkId::new();
        let b = ChunkId::new();
        let branches = vec![Branch {
            name: LEXICAL,
            weight: 1.0,
            ranked: vec![a, b],
        }];
        let first = rrf(&branches, RRF_K);
        assert_eq!(first[0].chunk_id, a);
        assert!(first[0].score > first[1].score);
        assert_eq!(first[0].score, 1.0 / (RRF_K + 1.0));
        // Same input, same order — every time.
        let second = rrf(&branches, RRF_K);
        assert_eq!(
            first.iter().map(|c| c.chunk_id).collect::<Vec<_>>(),
            second.iter().map(|c| c.chunk_id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_chunk_in_two_branches_scores_higher_than_one_in_either() {
        // The M4 shape, exercised now so the seam is not theoretical.
        let both = ChunkId::new();
        let only = ChunkId::new();
        let branches = vec![
            Branch {
                name: LEXICAL,
                weight: 1.0,
                ranked: vec![only, both],
            },
            Branch {
                name: "vector",
                weight: 1.0,
                ranked: vec![both],
            },
        ];
        let fused = rrf(&branches, RRF_K);
        assert_eq!(fused[0].chunk_id, both);
        assert_eq!(fused[0].branch_ranks.len(), 2);
    }

    #[test]
    fn exact_user_content_gets_no_multipliers() {
        assert!(multipliers_for(&hit(Origin::User, ProvenanceClass::Exact)).is_empty());
    }

    #[test]
    fn self_written_and_degraded_multipliers_compound() {
        let m = multipliers_for(&hit(Origin::SelfWritten, ProvenanceClass::Degraded));
        assert_eq!(m.len(), 2);
        let product: f32 = m.iter().map(|x| x.factor).product();
        assert!((product - 0.5 * 0.8).abs() < 1e-6, "got {product}");
    }

    #[test]
    fn every_multiplier_says_why() {
        // RET-004: `--explain` renders these verbatim. A bare number is not an
        // explanation.
        for m in multipliers_for(&hit(Origin::SelfWritten, ProvenanceClass::Approximate)) {
            assert!(m.reason.len() > 20, "{:?} does not explain itself", m);
            assert!(m.factor < 1.0);
        }
    }

    #[test]
    fn paths_render_relative_to_the_longest_matching_root() {
        let roots = vec!["/u/x/proj/sub".to_string(), "/u/x/proj".to_string()];
        assert_eq!(relative_path("/u/x/proj/sub/a.rs", &roots), "a.rs");
        assert_eq!(relative_path("/u/x/proj/b.rs", &roots), "b.rs");
        assert_eq!(relative_path("/elsewhere/c.rs", &roots), "/elsewhere/c.rs");
    }

    #[test]
    fn an_empty_or_punctuation_only_query_is_refused() {
        for text in ["", "   ", "!!!", "-- ??"] {
            let err = validate(&SearchRequest::new(text)).unwrap_err();
            assert_eq!(err.code(), Code::CfgInvalid);
            assert!(err.message().len() > 30, "must name an action");
        }
        assert!(validate(&SearchRequest::new("auth")).is_ok());
    }

    #[test]
    fn an_unknown_workspace_name_names_the_ones_that_exist() {
        let spaces = vec![WorkspaceInfo {
            workspace_id: WorkspaceId::new(),
            name: "desktop".into(),
            roots: vec![],
        }];
        let err = resolve_workspace_name("dsektop", &spaces).unwrap_err();
        assert_eq!(err.code(), Code::CfgInvalid);
        assert!(err.message().contains("desktop"), "{}", err.message());
    }

    #[test]
    fn workspace_names_resolve_case_insensitively_when_unambiguous() {
        let id = WorkspaceId::new();
        let spaces = vec![WorkspaceInfo {
            workspace_id: id,
            name: "Desktop".into(),
            roots: vec![],
        }];
        assert_eq!(resolve_workspace_name("desktop", &spaces).unwrap(), id);
    }

    #[test]
    fn extensions_are_normalized_before_they_reach_the_index() {
        let f = SearchFilters {
            extension: Some(".MD".into()),
            ..Default::default()
        };
        let mapped = resolve_filters(&f, &[]).unwrap();
        assert_eq!(mapped.extensions, vec!["md".to_string()]);
    }
}
