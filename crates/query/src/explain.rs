//! `search --explain` (RET-004, [Part 6 §113.4]).
//!
//! Per-branch ranks and the multipliers applied, for every result. Nothing is
//! recomputed here: [`crate::search::search`] already recorded what it did on
//! each [`Hit`], so this is a projection of a decision that was actually made
//! rather than a second, plausible-looking derivation of it. An explanation
//! that can disagree with the ranking is worse than no explanation.
//!
//! **With one branch it says so.** When only the lexical branch runs — which is
//! the state of any machine with no embedding model, and hard rule 10 says that
//! machine must still search — RRF is a monotone transform of BM25 order and
//! contributes nothing to the outcome. Presenting a fusion score as though it
//! were doing work would be theatre; [`Explanation::caveats`] states the
//! limitation instead, and states it only when it is true: the branch list is
//! derived from the hits, so a hybrid search is described as one.
//!
//! [Part 6 §113.4]: ../../../docs/Part_6_Engineering_Reference.md

use marrow_core::{ChunkId, Origin, ProvenanceClass};
use serde::Serialize;

use crate::search::{
    mode_label, AppliedMultiplier, BranchRank, Hit, SearchRequest, LEXICAL, LEXICAL_WEIGHT, RRF_K,
    SEMANTIC, SEMANTIC_WEIGHT,
};

/// Why the results came out in the order they did.
#[derive(Clone, Debug, Serialize)]
pub struct Explanation {
    pub query: String,
    /// `terms`, `phrase` or `prefix`.
    pub mode: &'static str,
    /// The branches that ran, in fusion order.
    pub branches: Vec<BranchExplanation>,
    /// RRF's rank-smoothing constant (§113.2).
    pub rrf_k: f32,
    /// One entry per returned hit, in final rank order.
    pub hits: Vec<HitExplanation>,
    /// What this explanation cannot tell you. Stated, never omitted.
    pub caveats: Vec<&'static str>,
}

/// One retrieval branch.
#[derive(Clone, Debug, Serialize)]
pub struct BranchExplanation {
    pub name: &'static str,
    /// `w_b` in `Σ w_b / (k + rank_b)`.
    pub weight: f32,
    /// How many of the returned hits this branch contributed.
    pub contributed: usize,
    /// The §113.2 "runs when" column, in plain words.
    pub ran_because: &'static str,
}

/// Why one hit landed where it did.
#[derive(Clone, Debug, Serialize)]
pub struct HitExplanation {
    /// Final rank, 1-based.
    pub rank: usize,
    pub chunk_id: ChunkId,
    /// Workspace-relative, the same string the renderer shows.
    pub path: String,
    /// The hit's 1-based rank inside each branch that returned it. A branch
    /// that did not return this chunk is absent rather than listed with a
    /// zero — "not retrieved" and "retrieved last" are different facts.
    pub branch_ranks: Vec<BranchRank>,
    /// RRF score before the multipliers.
    pub base_score: f32,
    /// §113.3 multipliers, in the order applied. Empty means none.
    pub multipliers: Vec<AppliedMultiplier>,
    /// `base_score` × every factor above. The sort key.
    pub final_score: f32,
    /// **Invariant #13.** `false` bars this content from supporting a claim,
    /// no matter how it scored.
    pub can_support_a_claim: bool,
}

/// Assemble the explanation for a finished search.
///
/// Pure: no store, no index, no second query. It takes the request for the
/// query text and mode, and the hits for everything else.
pub fn explain(req: &SearchRequest, hits: &[Hit]) -> Explanation {
    let contributed = hits
        .iter()
        .filter(|h| h.branch_ranks.iter().any(|b| b.branch == LEXICAL))
        .count();

    // Stated from what this search actually did, not from what the milestone was
    // when the code was written. The original text said "M1 runs one branch …
    // when the vector branch lands"; the vector branch has landed, so a caveat
    // asserting otherwise is exactly the kind of stale claim an explanation
    // exists to prevent.
    // **Derived from the hits, not asserted.** This list was hard-coded to
    // lexical alone, so once the semantic branch shipped the explanation
    // reported one branch for a search that had run two — an explanation that
    // is wrong about what happened is worse than none, because it is the thing
    // people reach for when they already suspect something.
    let semantic_hits = hits
        .iter()
        .filter(|h| h.branch_ranks.iter().any(|b| b.branch == SEMANTIC))
        .count();

    let mut branches = vec![BranchExplanation {
        name: LEXICAL,
        weight: LEXICAL_WEIGHT,
        contributed,
        ran_because: "always: lexical retrieval needs no model, no GPU and no network \
                      (invariant #15)",
    }];
    if semantic_hits > 0 {
        branches.push(BranchExplanation {
            name: SEMANTIC,
            weight: SEMANTIC_WEIGHT,
            contributed: semantic_hits,
            ran_because: "an embedding model was loaded and the chunk had a vector",
        });
    }

    let mut caveats = Vec::new();
    if branches.len() < 2 {
        caveats.push(
            "Only one branch ran, so fusion did not change the order BM25 produced. \
             The per-branch ranks become informative when a second branch runs — \
             the semantic one needs an embedding model and a finished backfill.",
        );
    }
    if hits.iter().any(|h| !h.can_support_a_claim) {
        caveats.push(
            "Some results are agent-written (origin = SELF). They are findable and \
             down-weighted, and they may never be cited as evidence (invariant #13).",
        );
    }
    if hits
        .iter()
        .any(|h| h.hit.provenance != ProvenanceClass::Exact)
    {
        caveats.push(
            "Some results came through a lossy conversion, so their citations point at a \
             coarser location than a byte range (CONV-003).",
        );
    }

    Explanation {
        query: req.text.clone(),
        mode: mode_label(req.mode),
        branches,
        rrf_k: RRF_K,
        hits: hits.iter().map(hit_explanation).collect(),
        caveats,
    }
}

fn hit_explanation(h: &Hit) -> HitExplanation {
    HitExplanation {
        rank: h.rank,
        chunk_id: h.hit.chunk_id,
        path: h.relative_path.clone(),
        branch_ranks: h.branch_ranks.clone(),
        base_score: h.base_score,
        multipliers: h.multipliers.clone(),
        final_score: h.fused_score,
        can_support_a_claim: h.can_support_a_claim,
    }
}

/// A one-line summary of a hit's scoring, for the terminal.
///
/// `#2  0.0161 × 0.5 (origin = SELF …) = 0.0081`. The multiplication is spelled
/// out because a lone final score is not an explanation of anything.
pub fn summarize(h: &HitExplanation) -> String {
    let mut s = format!("#{}  {:.4}", h.rank, h.base_score);
    for m in &h.multipliers {
        s.push_str(&format!(" × {:.2} ({})", m.factor, m.reason));
    }
    s.push_str(&format!(" = {:.4}", h.final_score));
    if !h.can_support_a_claim {
        s.push_str("  [not evidence]");
    }
    s
}

/// Whether an explanation describes content that may be cited.
///
/// The convenience form of invariant #13 for a caller assembling evidence: it
/// filters on this, never on the score.
pub fn citable(h: &HitExplanation) -> bool {
    h.can_support_a_claim
}

/// Whether an origin may be cited. Restated here so a renderer that only has an
/// [`Origin`] does not have to re-derive the rule.
pub fn origin_is_citable(origin: Origin) -> bool {
    origin.can_support_a_claim()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{multipliers_for, SearchFilters};
    use marrow_core::{FileId, SourceSpan, Timestamp, VersionId, WorkspaceId};
    use marrow_index::{Snippet, TextHit};

    fn hit(rank: usize, origin: Origin, provenance: ProvenanceClass) -> Hit {
        let text = TextHit {
            chunk_id: ChunkId::new(),
            file_id: FileId::new(),
            version_id: VersionId::new(),
            workspace_id: WorkspaceId::new(),
            path: "/root/notes.md".into(),
            title: String::new(),
            score: 1.0,
            span: SourceSpan::Whole,
            snippet: Snippet::default(),
            provenance,
            origin,
            modified: Timestamp::EPOCH,
        };
        let multipliers = multipliers_for(&text);
        let base_score = 1.0 / (RRF_K + rank as f32);
        Hit {
            workspace: "desktop".into(),
            relative_path: "notes.md".into(),
            can_support_a_claim: origin.can_support_a_claim(),
            rank,
            base_score,
            fused_score: multipliers.iter().fold(base_score, |a, m| a * m.factor),
            branch_ranks: vec![BranchRank {
                branch: LEXICAL,
                rank,
            }],
            multipliers,
            hit: text,
        }
    }

    #[test]
    fn one_branch_says_it_is_one_branch() {
        let req = SearchRequest::new("auth").filters(SearchFilters::default());
        let e = explain(&req, &[hit(1, Origin::User, ProvenanceClass::Exact)]);
        assert_eq!(e.branches.len(), 1);
        assert_eq!(e.branches[0].name, LEXICAL);
        assert_eq!(e.branches[0].contributed, 1);
        assert!(
            e.caveats.iter().any(|c| c.contains("one branch")),
            "the single-branch limitation must be stated, not implied"
        );
    }

    #[test]
    fn the_explanation_reproduces_the_ranking_it_describes() {
        // RET-004 is worthless if the numbers shown are recomputed and can
        // disagree with the order the user is looking at.
        let hits = vec![
            hit(1, Origin::SelfWritten, ProvenanceClass::Exact),
            hit(2, Origin::User, ProvenanceClass::Exact),
        ];
        let e = explain(&SearchRequest::new("auth"), &hits);
        for (h, x) in hits.iter().zip(&e.hits) {
            assert_eq!(x.final_score, h.fused_score);
            assert_eq!(x.base_score, h.base_score);
            assert_eq!(x.rank, h.rank);
        }
    }

    #[test]
    fn self_written_hits_are_called_out_and_marked_uncitable() {
        let hits = vec![hit(1, Origin::SelfWritten, ProvenanceClass::Exact)];
        let e = explain(&SearchRequest::new("auth"), &hits);
        assert!(!e.hits[0].can_support_a_claim);
        assert!(!citable(&e.hits[0]));
        assert!(e.caveats.iter().any(|c| c.contains("SELF")));
        assert!(summarize(&e.hits[0]).contains("not evidence"));
    }

    #[test]
    fn the_summary_spells_out_the_multiplication() {
        let hits = vec![hit(1, Origin::SelfWritten, ProvenanceClass::Degraded)];
        let e = explain(&SearchRequest::new("auth"), &hits);
        let line = summarize(&e.hits[0]);
        assert!(line.contains("× 0.50"), "{line}");
        assert!(line.contains("× 0.80"), "{line}");
    }

    #[test]
    fn an_exact_user_result_has_nothing_to_apologise_for() {
        let hits = vec![hit(1, Origin::User, ProvenanceClass::Exact)];
        let e = explain(&SearchRequest::new("auth"), &hits);
        assert!(e.hits[0].multipliers.is_empty());
        assert_eq!(e.caveats.len(), 1, "only the one-branch caveat");
        assert!(origin_is_citable(Origin::User));
    }
}
