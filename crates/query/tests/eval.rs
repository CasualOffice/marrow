//! Retrieval **quality** eval, and the regression gate over it.
//!
//! `hybrid.rs` and `query.rs` next door are correctness tests: this hit is
//! citable, that branch ran, a tombstoned chunk is not returned. None of them
//! can tell you whether a change to [`RRF_K`], to the branch weights, or to the
//! per-field BM25 weights made results *better or worse* — and until something
//! can, [Part 6 §113.4]'s tuning protocol ("any weight change runs the golden
//! query set in CI; a Recall@10 regression blocks merge") has nothing to run.
//!
//! [`RRF_K`]: marrow_query::RRF_K
//! [Part 6 §113.4]: ../../../docs/Part_6_Engineering_Reference.md
//!
//! # The pieces, and where they live
//!
//! | | |
//! |---|---|
//! | `eval/corpus/` | 24 hand-written text files, one per retrieval case worth arguing about |
//! | `eval/corpus.toml` | the facts about a file that its bytes cannot carry: `origin`, provenance, mtime |
//! | `eval/judgements.toml` | the golden query set: `(query, file, grade)` plus the pass/fail checks, with the reasoning for each |
//! | `eval/concepts.toml` | the stub embedder's vocabulary — see below |
//! | `eval/baseline.toml` | the committed numbers this gate compares against |
//!
//! Everything a person would want to argue with is **data**. Adding a query,
//! re-grading a document or extending the corpus needs no Rust at all.
//!
//! # Running with no model, no GPU and no network
//!
//! Hard rule 10 is not negotiable and it is also what makes this runnable in
//! CI, so the eval ships a deterministic stub embedder ([`Concepts`]) whose
//! vocabulary is `eval/concepts.toml`. It is a fixture, not a model. What the
//! `hybrid` numbers measure is therefore the **fusion pipeline** — RRF, the
//! branch weights, the multipliers, hydration of semantic-only candidates —
//! given an embedder whose behaviour is known exactly. They are not a claim
//! about any real embedding model, and swapping one in would move every one of
//! them.
//!
//! The harness reports **both** configurations, always:
//!
//! - `lexical` — [`search`], the branch that must work on a machine with
//!   nothing installed.
//! - `hybrid` — [`search_hybrid`], both branches fused.
//!
//! Neither is skipped and neither is silent. A query whose text hits no concept
//! has no embedding, so its `hybrid` run is lexical-only; the report names
//! those queries rather than letting them quietly dilute the hybrid figures.
//!
//! # Why these metrics
//!
//! 16 queries and 24 documents is a small set, and the metric has to be one
//! that a set this size can actually support:
//!
//! - **Recall@10** — named by §113.4 as the thing that blocks merge. With one
//!   or two relevant documents per query it is a blunt, honest number.
//! - **Precision@5** — five is what a screenful of results is (UX §4), so this
//!   is the number a person feels.
//! - **MRR** — over grade-2 documents only. "How far down is the document that
//!   actually answers me."
//! - **NDCG@10** over three grades — defensible here precisely *because* the
//!   grades are coarse. Distinguishing "answers it" from "related" from
//!   "irrelevant" is a call a person can make and defend on 24 files; a
//!   five-point scale, or anything needing thousands of judgements to
//!   stabilise, is not.
//!
//! Nothing here is a leaderboard. The output that matters is the delta against
//! `eval/baseline.toml`.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use marrow_core::{
    ChunkId, ContentHash, FileId, FileStatus, Origin, ProvenanceClass, RootId, SourceSpan,
    Timestamp, VersionId, WorkspaceId,
};
use marrow_index::{
    Embedding, Fts5Index, MatchMode, SqliteVectorIndex, TextDoc, TextIndex, VectorDoc, VectorIndex,
};
use marrow_query::search::search_hybrid;
use marrow_query::{search, SearchRequest};
use marrow_store::read::{NewChunk, NewFile, NewRoot, NewVersion, NewWorkspace};
use marrow_store::{StorageKind, Store};
use serde::Deserialize;

// ----------------------------------------------------------------- the gate

/// Depth every metric is measured at. §113.4 names Recall@10; the rest follow
/// it so one ranked list serves all four.
const K: usize = 10;

/// The screenful (UX §4).
const K_PRECISION: usize = 5;

/// How far an **aggregate** metric may fall below the committed baseline.
///
/// The harness is deterministic — fixed corpus, fixed identifiers, fixed
/// judgements, no model — so this is not absorbing measurement noise. There is
/// none. It absorbs *insignificance*: with 16 queries the smallest change one
/// query can make to Precision@5 is 1/5/16 = 0.0125 and to Recall@10 is at
/// least 1/2/16 = 0.031, so 0.01 is below the granularity of "one query got
/// worse" on both — any query that genuinely regressed trips the gate. What it
/// tolerates is a sub-position wobble in MRR or NDCG somewhere in the tail
/// (rank 9 to rank 10 moves MRR by 0.0007), which is not a change a person
/// experiences.
///
/// Tightening it to zero was tempting and is wrong: it would fire on a
/// tie-break reshuffle that changed nothing anybody can see, and a gate that
/// cries wolf gets re-blessed without reading.
const AGGREGATE_TOLERANCE: f64 = 0.01;

/// How far a **single query's** NDCG@10 may fall.
///
/// The aggregate gate alone is not enough: one query dropping the full 0.10
/// below moves the 16-query mean by 0.006, under [`AGGREGATE_TOLERANCE`]. A
/// change that helps one query and hurts another nets out to nothing on the
/// mean and would sail through. This is the gate that catches it.
///
/// 0.10 is where it is because of what NDCG costs per position at the top of a
/// list: for a query with one relevant document, rank 1 → 2 costs 0.37 and
/// rank 2 → 3 costs 0.13, while rank 5 → 6 costs 0.03. So this fires on any
/// movement inside the part of the list a person actually reads, and tolerates
/// shuffling below it.
const PER_QUERY_NDCG_TOLERANCE: f64 = 0.10;

/// Set `MARROW_EVAL_BLESS=1` to rewrite `eval/baseline.toml` from this run.
const BLESS_ENV: &str = "MARROW_EVAL_BLESS";

/// Set `MARROW_EVAL_SHOW=q06` (or `*`) to print the ranked list behind a
/// number.
///
/// A metric that dropped and cannot be explained is a number, not a finding.
/// This is the whole debugger the harness needs: which files came back, in what
/// order, with what grade.
const SHOW_ENV: &str = "MARROW_EVAL_SHOW";

// ------------------------------------------------------------- the fixtures

fn eval_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../eval")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn parse_toml<T: for<'de> Deserialize<'de>>(path: &Path) -> T {
    toml::from_str(&read(path)).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
}

#[derive(Debug, Deserialize)]
struct Manifest {
    #[serde(default)]
    file: Vec<ManifestEntry>,
}

#[derive(Debug, Deserialize)]
struct ManifestEntry {
    path: String,
    origin: Option<String>,
    provenance: Option<String>,
    modified: Option<i64>,
}

/// Every file not named in `corpus.toml` gets these. Documented there too.
const DEFAULT_MTIME_MS: i64 = 1_717_200_000_000;

#[derive(Debug, Deserialize)]
struct Judgements {
    query: Vec<JudgedQuery>,
}

#[derive(Debug, Deserialize)]
struct JudgedQuery {
    id: String,
    text: String,
    mode: String,
    #[serde(default)]
    #[allow(dead_code)] // Read by people, not by code. That is its whole job.
    why: String,
    #[serde(default)]
    relevant: Vec<Graded>,
    #[serde(default)]
    never_citable: Vec<NeverCitable>,
    #[serde(default)]
    ranked_below: Vec<RankedBelow>,
}

#[derive(Debug, Deserialize)]
struct Graded {
    file: String,
    grade: u32,
}

#[derive(Debug, Deserialize)]
struct NeverCitable {
    file: String,
    /// Set when this build is known not to satisfy the rule. See
    /// [`CheckOutcome::known`] — the judgement stays as written and the test
    /// asserts the failure *persists*, so fixing it is what breaks the build.
    known_failure: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RankedBelow {
    file: String,
    below: String,
    known_failure: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ConceptFile {
    concepts: BTreeMap<String, Vec<String>>,
}

// --------------------------------------------------------- the stub embedder

/// A bag-of-concepts embedder. See `eval/concepts.toml` for what it is and,
/// more importantly, what it is not.
#[derive(Debug)]
struct Concepts {
    dims: usize,
    /// word → the dimensions it contributes to.
    words: HashMap<String, Vec<usize>>,
}

impl Concepts {
    fn load(path: &Path) -> Self {
        let raw: ConceptFile = parse_toml(path);
        let mut words: HashMap<String, Vec<usize>> = HashMap::new();
        for (dim, (_name, terms)) in raw.concepts.iter().enumerate() {
            for t in terms {
                words.entry(t.to_lowercase()).or_default().push(dim);
            }
        }
        Self {
            dims: raw.concepts.len(),
            words,
        }
    }

    /// `None` when the text hits no concept at all — there is no direction to
    /// keep, and [`Embedding::new`] refuses a zero vector for the same reason.
    fn embed(&self, text: &str) -> Option<Embedding> {
        let mut v = vec![0.0f32; self.dims];
        for token in tokens(text) {
            if let Some(dims) = self.words.get(&token) {
                for d in dims {
                    v[*d] += 1.0;
                }
            }
        }
        Embedding::new(v)
    }
}

fn tokens(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
}

// ---------------------------------------------------------- deterministic ids

/// A ULID built from a counter rather than from the clock and a PRNG.
///
/// Not cosmetic. Fused candidates with equal scores are broken by `chunk_id`,
/// so random identifiers make the tail of a ranked list reorder between runs —
/// and an eval whose numbers move when nothing changed cannot gate anything.
/// Decimal digits are all valid Crockford base32, so a zero-padded counter is
/// a well-formed ULID.
fn det_id<T: FromStr>(n: u64) -> T
where
    T::Err: std::fmt::Debug,
{
    format!("{n:026}").parse().expect("counter is a valid ULID")
}

// -------------------------------------------------------------- the chunker

/// One chunk of a fixture file.
struct Block {
    heading: String,
    text: String,
    first_line: u32,
    last_line: u32,
}

/// Split a fixture on blank lines, carrying the nearest heading as the
/// structural context prefix (CHK-002's `context_prefix`, in miniature).
///
/// Deliberately not Marrow's chunker: this crate is the read path and must not
/// grow one. What the eval needs is a split that a person can predict by
/// looking at the file, so that "which chunk was that" is never a question.
fn blocks(body: &str) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    let mut heading = String::new();
    let mut current: Vec<&str> = Vec::new();
    let mut start = 0u32;

    let flush = |out: &mut Vec<Block>, cur: &mut Vec<&str>, heading: &str, start: u32, end: u32| {
        if cur.is_empty() {
            return;
        }
        out.push(Block {
            heading: heading.to_string(),
            text: cur.join("\n"),
            first_line: start,
            last_line: end,
        });
        cur.clear();
    };

    for (i, line) in body.lines().enumerate() {
        let n = i as u32 + 1;
        if line.trim().is_empty() {
            flush(&mut out, &mut current, &heading, start, n.saturating_sub(1));
            continue;
        }
        if let Some(h) = line.trim().strip_prefix('#') {
            flush(&mut out, &mut current, &heading, start, n.saturating_sub(1));
            heading = h.trim_start_matches('#').trim().to_string();
            continue;
        }
        if current.is_empty() {
            start = n;
        }
        current.push(line);
    }
    let end = body.lines().count() as u32;
    flush(&mut out, &mut current, &heading, start, end);
    out
}

// ------------------------------------------------------------- the fixture db

struct Corpus {
    // Drop order: the index's reader and the store's writer thread must both be
    // gone before the tempdir is.
    index: Fts5Index,
    vectors: SqliteVectorIndex,
    store: Store,
    concepts: Concepts,
    /// Corpus-relative path, by the id the search returns.
    path_of: HashMap<FileId, String>,
    _dir: tempfile::TempDir,
}

impl Corpus {
    /// Index every file under `eval/corpus/`, applying `eval/corpus.toml`.
    fn build() -> Self {
        let root = eval_dir().join("corpus");
        let manifest: Manifest = parse_toml(&eval_dir().join("corpus.toml"));
        let overrides: HashMap<&str, &ManifestEntry> =
            manifest.file.iter().map(|f| (f.path.as_str(), f)).collect();
        let concepts = Concepts::load(&eval_dir().join("concepts.toml"));

        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open_with_migrations(
            dir.path().join(marrow_store::DB_FILE_NAME),
            marrow_index::MIGRATIONS,
        )
        .expect("open store");
        let index = Fts5Index::open(&store).expect("open lexical index");
        let vectors = SqliteVectorIndex::open(&store).expect("open vector index");

        let at = Timestamp::from_millis(DEFAULT_MTIME_MS);
        let workspace: WorkspaceId = det_id(1);
        store
            .upsert_workspace(NewWorkspace {
                workspace_id: workspace,
                name: "eval".into(),
                at,
            })
            .expect("workspace");
        let root_id: RootId = det_id(1);
        let root_path = root.to_string_lossy().into_owned();
        store
            .upsert_root(NewRoot {
                root_id,
                workspace_id: workspace,
                canonical_path: root_path.clone(),
                volume_identity: None,
                grant_token: None,
                storage_kind: StorageKind::Local,
                cloud_provider: None,
                at,
            })
            .expect("root");
        store.flush().expect("flush");

        let mut path_of = HashMap::new();
        let mut docs: Vec<TextDoc> = Vec::new();
        let mut vecs: Vec<VectorDoc> = Vec::new();
        let mut counter = 1u64;
        for rel in corpus_files(&root) {
            let abs = root.join(&rel);
            let body = read(&abs);
            let entry = overrides.get(rel.as_str());
            let origin = match entry.and_then(|e| e.origin.as_deref()) {
                Some("SELF") => Origin::SelfWritten,
                _ => Origin::User,
            };
            let provenance = match entry.and_then(|e| e.provenance.as_deref()) {
                Some("DEGRADED") => ProvenanceClass::Degraded,
                Some("APPROXIMATE") => ProvenanceClass::Approximate,
                Some("METADATA_ONLY") => ProvenanceClass::MetadataOnly,
                _ => ProvenanceClass::Exact,
            };
            let modified =
                Timestamp::from_millis(entry.and_then(|e| e.modified).unwrap_or(DEFAULT_MTIME_MS));

            counter += 1;
            let file_id: FileId = det_id(counter);
            let version_id: VersionId = det_id(counter);
            let abs_path = abs.to_string_lossy().into_owned();

            let file = NewFile {
                file_id,
                workspace_id: workspace,
                root_id,
                current_path: Some(abs_path.clone()),
                fs_identity: Some(rel.clone()),
                tier_state: marrow_core::TierState::Resident,
                origin,
                origin_txn_id: None,
                external_source_url: None,
                status: FileStatus::Active,
                at: modified,
            };
            let version = NewVersion {
                version_id,
                file_id,
                path_at_observation: abs_path.clone(),
                size_bytes: body.len() as i64,
                mtime_ms: modified,
                content_hash: ContentHash::of(body.as_bytes()),
                mime: Some("text/plain".into()),
                language: None,
                observed_at: modified,
            };
            store
                .insert_file_with_version(file, version)
                .expect("insert file");
            path_of.insert(file_id, rel.clone());

            let mut chunk_rows = Vec::new();
            for block in blocks(&body) {
                counter += 1;
                let chunk_id: ChunkId = det_id(counter);
                let title = if block.heading.is_empty() {
                    rel.clone()
                } else {
                    format!("{rel} › {}", block.heading)
                };
                // A table band keeps its header with it, which is why the
                // fixture repeats the header row in the totals block.
                let kind = if block.text.starts_with('|') {
                    "TABLE_BAND"
                } else {
                    "TEXT"
                };
                chunk_rows.push(NewChunk {
                    chunk_id,
                    version_id,
                    chunk_kind: kind.into(),
                    text: block.text.clone(),
                    context_prefix: Some(title.clone()),
                    token_count: block.text.split_whitespace().count() as i64,
                    text_hash: ContentHash::of(block.text.as_bytes()),
                    chunker_version: "eval-blocks/1".into(),
                    provenance_class: provenance_sql(provenance).into(),
                });
                docs.push(TextDoc {
                    chunk_id,
                    file_id,
                    version_id,
                    workspace_id: workspace,
                    path: abs_path.clone(),
                    title: title.clone(),
                    body: block.text.clone(),
                    span: SourceSpan::Lines {
                        start: block.first_line,
                        end: block.last_line,
                    },
                    provenance,
                    origin,
                    modified,
                });
                // The heading is part of what a chunk is about, so the stub
                // embedder sees it — the same reason the real chunker carries
                // a `context_prefix`.
                if let Some(embedding) = concepts.embed(&format!("{title} {}", block.text)) {
                    vecs.push(VectorDoc {
                        chunk_id,
                        file_id,
                        version_id,
                        workspace_id: workspace,
                        embedding,
                    });
                }
            }
            store
                .writer()
                .submit(move |c| marrow_store::read::replace_chunks(c, version_id, &chunk_rows))
                .expect("chunks");
        }
        // One flush for the whole corpus. The lexical index's doc table has a
        // foreign key onto `chunks`, so every chunk has to be committed before
        // any document is indexed — but that is one barrier, not twenty-four.
        store.flush().expect("flush");
        index.upsert(&docs).expect("index");
        vectors.upsert(&vecs).expect("vectors");

        Self {
            index,
            vectors,
            store,
            concepts,
            path_of,
            _dir: dir,
        }
    }

    /// The ranked list of **files**, best first, deduplicated to the best rank
    /// each file reached.
    ///
    /// Judgements are per file, not per chunk: on a corpus this size, "did the
    /// right document come back" is a question a person can answer and defend,
    /// and "was it the third paragraph or the fourth" is not.
    fn ranked_files(&self, q: &JudgedQuery, semantic: bool) -> (Vec<RankedFile>, bool) {
        let req = SearchRequest::new(q.text.clone())
            .mode(mode_of(&q.mode))
            .limit(K * 4); // deeper than K: several chunks may share a file
        let embedding = semantic.then(|| self.concepts.embed(&q.text)).flatten();
        let results = match &embedding {
            Some(e) => search_hybrid(
                &self.store,
                &self.index,
                Some((&self.vectors as &dyn VectorIndex, e)),
                &req,
            ),
            None => search(&self.store, &self.index, &req),
        }
        .unwrap_or_else(|e| panic!("{} ({}): {e}", q.id, q.text));

        let mut seen = Vec::new();
        for hit in &results.hits {
            let path = self
                .path_of
                .get(&hit.hit.file_id)
                .cloned()
                .unwrap_or_else(|| hit.hit.path.clone());
            if seen.iter().any(|r: &RankedFile| r.path == path) {
                continue;
            }
            seen.push(RankedFile {
                path,
                citable: hit.can_support_a_claim,
            });
        }
        seen.truncate(K);
        (seen, embedding.is_some())
    }
}

struct RankedFile {
    path: String,
    citable: bool,
}

fn provenance_sql(p: ProvenanceClass) -> &'static str {
    match p {
        ProvenanceClass::Exact => "EXACT",
        ProvenanceClass::Degraded => "DEGRADED",
        ProvenanceClass::Approximate => "APPROXIMATE",
        ProvenanceClass::MetadataOnly => "METADATA_ONLY",
    }
}

fn mode_of(s: &str) -> MatchMode {
    match s {
        "any" => MatchMode::Any,
        "phrase" => MatchMode::Phrase,
        "prefix" => MatchMode::Prefix,
        "terms" => MatchMode::Terms,
        other => panic!("unknown match mode {other:?} in judgements.toml"),
    }
}

/// Every file under `eval/corpus/`, as sorted corpus-relative paths.
fn corpus_files(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
            .map(|e| e.expect("dir entry").path())
            .collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                walk(&p, root, out);
            } else if p.file_name().is_some_and(|n| n != ".DS_Store") {
                out.push(
                    p.strip_prefix(root)
                        .expect("under root")
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out
}

// ----------------------------------------------------------------- metrics

#[derive(Clone, Copy, Debug, Default)]
struct QueryScore {
    precision_at_5: f64,
    recall_at_10: f64,
    reciprocal_rank: f64,
    ndcg_at_10: f64,
}

fn score(q: &JudgedQuery, ranked: &[RankedFile]) -> QueryScore {
    let grade = |path: &str| -> u32 {
        q.relevant
            .iter()
            .find(|g| g.file == path)
            .map(|g| g.grade)
            .unwrap_or(0)
    };

    let hits_at_5 = ranked
        .iter()
        .take(K_PRECISION)
        .filter(|r| grade(&r.path) > 0)
        .count();

    let judged_relevant = q.relevant.iter().filter(|g| g.grade > 0).count();
    let found = q
        .relevant
        .iter()
        .filter(|g| g.grade > 0 && ranked.iter().any(|r| r.path == g.file))
        .count();

    let reciprocal_rank = ranked
        .iter()
        .position(|r| grade(&r.path) >= 2)
        .map(|i| 1.0 / (i as f64 + 1.0))
        .unwrap_or(0.0);

    // Gain 2^g - 1: the distance from "related" to "answers it" is bigger than
    // the distance from "irrelevant" to "related", which is what the grades
    // mean.
    let gain = |g: u32| 2f64.powi(g as i32) - 1.0;
    let dcg: f64 = ranked
        .iter()
        .enumerate()
        .map(|(i, r)| gain(grade(&r.path)) / ((i as f64) + 2.0).log2())
        .sum();
    let mut ideal: Vec<u32> = q.relevant.iter().map(|g| g.grade).collect();
    ideal.sort_unstable_by(|a, b| b.cmp(a));
    let idcg: f64 = ideal
        .iter()
        .take(K)
        .enumerate()
        .map(|(i, g)| gain(*g) / ((i as f64) + 2.0).log2())
        .sum();

    QueryScore {
        precision_at_5: hits_at_5 as f64 / K_PRECISION as f64,
        recall_at_10: if judged_relevant == 0 {
            0.0
        } else {
            found as f64 / judged_relevant as f64
        },
        reciprocal_rank,
        ndcg_at_10: if idcg > 0.0 { dcg / idcg } else { 0.0 },
    }
}

/// One evaluation of one pass/fail rule, in one configuration.
///
/// Three states matter and only one of them is "failed", which is why this is a
/// struct and not a `Vec<String>` of complaints:
///
/// - **not exercised** — the rule's files never came back, so the rule proved
///   nothing. A judgement that can never fire is dead weight and the test says
///   so; this is the "born green" failure mode, and it is the one an eval is
///   most likely to acquire silently as the corpus grows.
/// - **violated** — the ranking broke the rule.
/// - **held** — the ranking satisfied it.
struct CheckOutcome {
    /// Stable identity of the rule, so the same rule can be tracked across
    /// configurations.
    key: String,
    detail: String,
    exercised: bool,
    violated: bool,
    /// Set from `known_failure` in `judgements.toml`: this build is known not
    /// to satisfy the rule, and why.
    known: Option<String>,
}

/// The pass/fail checks that are correctness rather than quality, so they are
/// never traded against a metric.
fn hard_checks(q: &JudgedQuery, ranked: &[RankedFile]) -> Vec<CheckOutcome> {
    let mut out = Vec::new();
    for rule in &q.never_citable {
        let at = ranked.iter().position(|r| r.path == rule.file);
        out.push(CheckOutcome {
            key: format!("{}/citable/{}", q.id, rule.file),
            detail: match at {
                Some(i) => format!(
                    "{}: {} returned at {} and must never be citable (the `origin = SELF` rule)",
                    q.id,
                    rule.file,
                    i + 1
                ),
                None => format!(
                    "{}: {} must never be citable (the `origin = SELF` rule)",
                    q.id, rule.file
                ),
            },
            exercised: at.is_some(),
            violated: at.is_some_and(|i| ranked[i].citable),
            known: rule.known_failure.clone(),
        });
    }
    for rule in &q.ranked_below {
        let at = |p: &str| ranked.iter().position(|r| r.path == p);
        let (lower, upper) = (at(&rule.file), at(&rule.below));
        let violated = matches!((lower, upper), (Some(l), Some(u)) if l < u);
        out.push(CheckOutcome {
            key: format!("{}/below/{}", q.id, rule.file),
            detail: match (lower, upper) {
                (Some(l), Some(u)) => format!(
                    "{}: {} ranked {} against {} at {}",
                    q.id,
                    rule.file,
                    l + 1,
                    rule.below,
                    u + 1
                ),
                _ => format!("{}: {} must rank below {}", q.id, rule.file, rule.below),
            },
            exercised: lower.is_some() && upper.is_some(),
            violated,
            known: rule.known_failure.clone(),
        });
    }
    out
}

/// One rule's verdict across every configuration that ran it.
#[derive(Default)]
struct RuleVerdict {
    detail: String,
    /// How informative `detail` is: 0 the rule never fired, 1 it was exercised,
    /// 2 it was violated. The most informative wins, so the line a reader sees
    /// carries the actual ranks rather than the restatement of the rule.
    detail_rank: u8,
    exercised: bool,
    violated_in: Vec<&'static str>,
    known: Option<String>,
}

fn verdicts(runs: &[Run]) -> BTreeMap<String, RuleVerdict> {
    let mut map: BTreeMap<String, RuleVerdict> = BTreeMap::new();
    for r in runs {
        for c in &r.checks {
            let v = map.entry(c.key.clone()).or_default();
            v.exercised |= c.exercised;
            v.known = c.known.clone().or_else(|| v.known.take());
            if c.violated {
                v.violated_in.push(r.label);
            }
            let rank = match (c.violated, c.exercised) {
                (true, _) => 2,
                (false, true) => 1,
                (false, false) => 0,
            };
            if rank >= v.detail_rank || v.detail.is_empty() {
                v.detail = c.detail.clone();
                v.detail_rank = rank;
            }
        }
    }
    map
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
struct Aggregate {
    p_at_5: f64,
    recall_at_10: f64,
    mrr: f64,
    ndcg_at_10: f64,
    #[serde(default)]
    per_query_ndcg: BTreeMap<String, f64>,
}

#[derive(Debug, Deserialize)]
struct Baseline {
    lexical: Aggregate,
    hybrid: Aggregate,
}

fn aggregate(per_query: &[(String, QueryScore)]) -> Aggregate {
    let n = per_query.len().max(1) as f64;
    let mean = |f: fn(&QueryScore) -> f64| per_query.iter().map(|(_, s)| f(s)).sum::<f64>() / n;
    Aggregate {
        p_at_5: mean(|s| s.precision_at_5),
        recall_at_10: mean(|s| s.recall_at_10),
        mrr: mean(|s| s.reciprocal_rank),
        ndcg_at_10: mean(|s| s.ndcg_at_10),
        per_query_ndcg: per_query
            .iter()
            .map(|(id, s)| (id.clone(), round(s.ndcg_at_10)))
            .collect(),
    }
}

/// Four decimals. More would be recording float noise as ground truth; fewer
/// would hide a real one-position move at the bottom of the list.
fn round(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

// -------------------------------------------------------------------- report

struct Run {
    label: &'static str,
    per_query: Vec<(String, QueryScore)>,
    aggregate: Aggregate,
    checks: Vec<CheckOutcome>,
    /// Queries whose text hit no concept, so the semantic branch could not run.
    no_embedding: Vec<String>,
    /// Ranked lists dumped because [`SHOW_ENV`] asked for them.
    dumps: Vec<String>,
}

fn run(corpus: &Corpus, queries: &[JudgedQuery], label: &'static str, semantic: bool) -> Run {
    let show = std::env::var(SHOW_ENV).unwrap_or_default();
    let mut per_query = Vec::new();
    let mut checks = Vec::new();
    let mut no_embedding = Vec::new();
    let mut dumps = Vec::new();
    for q in queries {
        let (ranked, embedded) = corpus.ranked_files(q, semantic);
        if semantic && !embedded {
            no_embedding.push(q.id.clone());
        }
        if show == "*" || show == q.id {
            let mut d = format!("\n{label} {} {:?}\n", q.id, q.text);
            for (i, r) in ranked.iter().enumerate() {
                let grade = q
                    .relevant
                    .iter()
                    .find(|g| g.file == r.path)
                    .map(|g| g.grade)
                    .unwrap_or(0);
                let _ = writeln!(
                    d,
                    "  {:>2}. grade {grade}  {}{}",
                    i + 1,
                    r.path,
                    if r.citable { "" } else { "   [not citable]" }
                );
            }
            dumps.push(d);
        }
        checks.extend(hard_checks(q, &ranked));
        per_query.push((q.id.clone(), score(q, &ranked)));
    }
    let aggregate = aggregate(&per_query);
    Run {
        label,
        per_query,
        aggregate,
        checks,
        no_embedding,
        dumps,
    }
}

/// The best Precision@5 this judgement set allows.
///
/// Most queries here have one relevant document, so P@5 cannot exceed 1/5 on
/// them however good the ranker is. Printing the raw 0.27 next to nothing
/// invites reading it as "73% wrong"; printing the ceiling beside it makes it
/// the regression signal it actually is.
fn precision_ceiling(queries: &[JudgedQuery]) -> f64 {
    let n = queries.len().max(1) as f64;
    queries
        .iter()
        .map(|q| {
            let relevant = q.relevant.iter().filter(|g| g.grade > 0).count();
            relevant.min(K_PRECISION) as f64 / K_PRECISION as f64
        })
        .sum::<f64>()
        / n
}

fn report(runs: &[Run], queries: &[JudgedQuery], files: usize, elapsed_ms: u128) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "\nretrieval eval — {} queries, {files} files, {elapsed_ms} ms\n",
        runs[0].per_query.len(),
    );
    let _ = writeln!(
        s,
        "{:<6} {:>10} {:>10}   {:>10} {:>10}",
        "query", "ndcg@10", "ndcg@10", "rr", "rr"
    );
    let _ = writeln!(
        s,
        "{:<6} {:>10} {:>10}   {:>10} {:>10}",
        "", runs[0].label, runs[1].label, runs[0].label, runs[1].label
    );
    for (i, (id, a)) in runs[0].per_query.iter().enumerate() {
        let b = &runs[1].per_query[i].1;
        let _ = writeln!(
            s,
            "{id:<6} {:>10.3} {:>10.3}   {:>10.3} {:>10.3}",
            a.ndcg_at_10, b.ndcg_at_10, a.reciprocal_rank, b.reciprocal_rank
        );
    }
    let ceiling = precision_ceiling(queries);
    for r in runs {
        let _ = writeln!(
            s,
            "\n{:<8} P@5 {:.4} (of {ceiling:.4} attainable)   Recall@10 {:.4}   MRR {:.4}   \
             NDCG@10 {:.4}",
            r.label,
            r.aggregate.p_at_5,
            r.aggregate.recall_at_10,
            r.aggregate.mrr,
            r.aggregate.ndcg_at_10
        );
        if !r.no_embedding.is_empty() {
            let _ = writeln!(
                s,
                "         no embedding, answered lexically: {}",
                r.no_embedding.join(", ")
            );
        }
    }

    for r in runs {
        for d in &r.dumps {
            s.push_str(d);
        }
    }

    let _ = writeln!(s, "\npass/fail rules");
    for (_key, v) in verdicts(runs) {
        let state = match (v.exercised, v.violated_in.is_empty(), &v.known) {
            (false, _, _) => "NOT EXERCISED".to_string(),
            (true, true, None) => "held".to_string(),
            (true, true, Some(_)) => "held, but marked known_failure".to_string(),
            (true, false, None) => format!("VIOLATED in {}", v.violated_in.join(", ")),
            (true, false, Some(why)) => format!(
                "known failure in {} — {}",
                v.violated_in.join(", "),
                why.trim().replace('\n', " ")
            ),
        };
        let _ = writeln!(s, "  {:<14} {}", "", format_args!("{} [{state}]", v.detail));
    }
    s
}

fn baseline_toml(runs: &[Run]) -> String {
    let mut s = String::new();
    s.push_str(
        "# Committed retrieval baseline. Regenerate with:\n\
         #\n\
         #     MARROW_EVAL_BLESS=1 cargo test -p marrow-query --test eval\n\
         #\n\
         # A re-bless is a claim that the numbers below are the new truth. Say why\n\
         # in the commit message, or it is a regression filed as an improvement.\n\
         #\n\
         # `lexical` is `search` — the branch that answers with no model, no GPU and\n\
         # no network. `hybrid` is `search_hybrid` fused with the stub embedder in\n\
         # `eval/concepts.toml`, so it measures the fusion pipeline and not any real\n\
         # embedding model.\n",
    );
    for r in runs {
        let a = &r.aggregate;
        let _ = write!(
            s,
            "\n[{}]\np_at_5 = {:.4}\nrecall_at_10 = {:.4}\nmrr = {:.4}\nndcg_at_10 = {:.4}\n\n[{}.per_query_ndcg]\n",
            r.label, a.p_at_5, a.recall_at_10, a.mrr, a.ndcg_at_10, r.label
        );
        for (id, v) in &a.per_query_ndcg {
            let _ = writeln!(s, "{id} = {v:.4}");
        }
    }
    s
}

fn compare(run: &Run, base: &Aggregate) -> Vec<String> {
    let mut out = Vec::new();
    let mut check = |name: &str, now: f64, was: f64| {
        if now < was - AGGREGATE_TOLERANCE {
            out.push(format!(
                "{} {name}: {now:.4}, baseline {was:.4} (drop {:.4} > tolerance {AGGREGATE_TOLERANCE})",
                run.label,
                was - now
            ));
        }
    };
    check("P@5", run.aggregate.p_at_5, base.p_at_5);
    check("Recall@10", run.aggregate.recall_at_10, base.recall_at_10);
    check("MRR", run.aggregate.mrr, base.mrr);
    check("NDCG@10", run.aggregate.ndcg_at_10, base.ndcg_at_10);

    for (id, now) in &run.aggregate.per_query_ndcg {
        let Some(was) = base.per_query_ndcg.get(id) else {
            continue; // A new query has no baseline yet; blessing gives it one.
        };
        if *now < was - PER_QUERY_NDCG_TOLERANCE {
            out.push(format!(
                "{} {id} NDCG@10: {now:.4}, baseline {was:.4} (drop {:.4} > tolerance {PER_QUERY_NDCG_TOLERANCE})",
                run.label,
                was - now
            ));
        }
    }
    out
}

// ---------------------------------------------------------------- the tests

#[test]
fn every_judgement_names_a_file_that_exists() {
    // A typo in a path silently becomes "this document was never retrieved",
    // which reads as a ranking failure and is not one.
    let judgements: Judgements = parse_toml(&eval_dir().join("judgements.toml"));
    let root = eval_dir().join("corpus");
    let known = corpus_files(&root);
    let manifest: Manifest = parse_toml(&eval_dir().join("corpus.toml"));

    let mut missing = Vec::new();
    let mut check = |p: &String| {
        if !known.contains(p) {
            missing.push(p.clone());
        }
    };
    for q in &judgements.query {
        for g in &q.relevant {
            check(&g.file);
        }
        for p in &q.never_citable {
            check(&p.file);
        }
        for r in &q.ranked_below {
            check(&r.file);
            check(&r.below);
        }
    }
    for f in &manifest.file {
        check(&f.path);
    }
    assert!(missing.is_empty(), "no such fixture file: {missing:?}");

    let mut ids: Vec<&str> = judgements.query.iter().map(|q| q.id.as_str()).collect();
    ids.sort_unstable();
    let n = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), n, "duplicate query id in judgements.toml");
}

#[test]
fn retrieval_quality_has_not_regressed() {
    let started = std::time::Instant::now();
    let judgements: Judgements = parse_toml(&eval_dir().join("judgements.toml"));
    let corpus = Corpus::build();

    let runs = vec![
        run(&corpus, &judgements.query, "lexical", false),
        run(&corpus, &judgements.query, "hybrid", true),
    ];
    let elapsed = started.elapsed().as_millis();
    let text = report(&runs, &judgements.query, corpus.path_of.len(), elapsed);
    println!("{text}");

    if std::env::var(BLESS_ENV).is_ok() {
        let path = eval_dir().join("baseline.toml");
        std::fs::write(&path, baseline_toml(&runs)).expect("write baseline");
        println!("blessed {}", path.display());
        return;
    }

    // Correctness first, and never traded against a metric. A better average
    // does not buy anybody out of the `origin = SELF` rule.
    let mut problems: Vec<String> = Vec::new();
    for (_key, v) in verdicts(&runs) {
        match (v.exercised, v.violated_in.is_empty(), &v.known) {
            // A rule whose files never came back proved nothing. Left alone it
            // is an assertion that can only ever pass, which is the exact way
            // an eval rots into decoration.
            (false, _, _) => problems.push(format!("{} — NEVER EXERCISED", v.detail)),
            // The rule broke and nobody had written down that it would.
            (true, false, None) => problems.push(format!(
                "{} — violated in {}",
                v.detail,
                v.violated_in.join(", ")
            )),
            // The rule holds everywhere but is still marked as a known
            // failure. Somebody fixed retrieval; the marker is now a lie and
            // has to go, or the next real regression hides behind it.
            (true, true, Some(why)) => problems.push(format!(
                "{} — now HOLDS, so delete its known_failure from judgements.toml \
                 (it said: {})",
                v.detail,
                why.trim().replace('\n', " ")
            )),
            _ => {}
        }
    }
    assert!(
        problems.is_empty(),
        "{text}\npass/fail rules:\n  {}",
        problems.join("\n  ")
    );

    let base: Baseline = parse_toml(&eval_dir().join("baseline.toml"));
    let mut regressions = compare(&runs[0], &base.lexical);
    regressions.extend(compare(&runs[1], &base.hybrid));
    assert!(
        regressions.is_empty(),
        "{text}\nretrieval quality regressed against eval/baseline.toml:\n{}\n\n\
         If this change is an improvement the baseline does not know about yet, \
         re-bless it with {BLESS_ENV}=1 and say why in the commit message.",
        regressions.join("\n")
    );
}
