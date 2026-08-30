//! Application state: an open store and index, shared by every command.

use marrow_core::Result;
use marrow_index::{Fts5Index, TextIndex, TextQuery, VectorIndex};
use marrow_store::Store;

use crate::commands::{
    to_hit, ConversationDetail, ConversationSummary, FileDetail, FileRow, IndexHealth, NewTurn,
    Region, SavedTurn, SearchHit, SearchResponse, StoredTurn, WorkspaceRow,
};

/// Everything the commands need. Opened once at startup.
pub struct Core {
    store: Store,
    index: Fts5Index,
    vectors: marrow_index::SqliteVectorIndex,
    /// The per-user directory the database sits in.
    ///
    /// Held because two things Marrow *owns* live beside the database and have
    /// to be found from anywhere a command runs: the preferences file, and the
    /// scratch workspace dropped files are copied into. Derived from the
    /// database's own parent rather than passed in, so there is one answer to
    /// "where is Marrow's data" and it cannot disagree with where the database
    /// actually is — which is what a second constructor parameter would allow.
    data_dir: std::path::PathBuf,
}

/// Words too common to help, and common enough to hurt.
///
/// BM25 already gives them almost no weight, so this is not about scoring —
/// it is about the term cap: a long question full of "the" and "of" can hit
/// the index's limit and be refused outright, and the words it drops for that
/// are the ones that mattered.
const STOPWORDS: &[&str] = &[
    "a", "an", "the", "and", "or", "but", "if", "of", "in", "on", "at", "to", "for", "with", "is",
    "are", "was", "were", "be", "been", "am", "do", "does", "did", "have", "has", "had", "what",
    "when", "where", "who", "whom", "which", "why", "how", "that", "this", "these", "those", "it",
    "its", "as", "by", "from", "my", "our", "me", "i", "you", "your", "can", "could", "would",
    "should", "will", "shall", "may", "might", "about", "please", "tell",
    // What is left of a contraction after the apostrophe splits it. "What's"
    // becomes "what" and "s", and the "s" retrieves nothing but noise.
    "s", "t", "d", "m", "ll", "re", "ve",
];

/// A question, reduced to the words worth retrieving on.
///
/// Not a router — that comes later and will rewrite the query properly
/// (ASK-001). This is the floor beneath it, and the floor has to work on its
/// own, because ASK-004 says a broken router degrades to the product that
/// already worked.
///
/// It lives here rather than in the caller because [`Core::retrieve`] is
/// disjunctive: unreduced, "when does **the** lease renew?" matches every
/// document that contains "the", which is all of them. A caller that forgot to
/// reduce would get a result set that looks full and means nothing, and only
/// one caller remembering is not a rule.
pub fn retrieval_terms(question: &str) -> String {
    let kept: Vec<&str> = question
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        // `--` splits into a token of hyphens under this rule; a term with no
        // letter or digit in it is punctuation, and the index refuses those.
        .filter(|w| w.chars().any(|c| c.is_alphanumeric()))
        .filter(|w| !STOPWORDS.contains(&w.to_ascii_lowercase().as_str()))
        .collect();
    // A question made entirely of stopwords is still a question. Searching for
    // nothing would refuse; searching for the original at least tries.
    if kept.is_empty() {
        question.to_string()
    } else {
        kept.join(" ")
    }
}

/// A chunk on its way into an evidence block.
///
/// Deliberately not a [`SearchHit`]: a result row wants two lines and a
/// highlight, and an evidence block wants as much of the document as the
/// budget allows. Sharing one type between them is how the first end-to-end
/// run produced evidence blocks containing a path and no text.
#[derive(Clone, Debug)]
pub struct RetrievedChunk {
    pub path: String,
    pub relative_path: String,
    pub location: String,
    pub line: Option<u32>,
    pub text: String,
    pub provenance: marrow_core::ProvenanceClass,
    /// Invariant #9: `SelfWritten` cannot support a claim.
    pub origin: marrow_core::Origin,
}

/// A scope as the window sends it, reduced to the fragment worth matching on.
///
/// The window sends a path relative to the workspace root — `services/STT` —
/// and the index stores absolute paths, so the fragment is matched as a
/// substring rather than as a prefix. Leading and trailing slashes are trimmed
/// because a user who types `/services/STT/` means the same subtree, and an
/// empty scope is no scope at all rather than a filter nothing can satisfy.
pub(crate) fn scope_fragment(scope: Option<&str>) -> Option<String> {
    let trimmed = scope?.trim().trim_matches('/').trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Longest matching root wins, so a nested workspace does not display as a
/// path relative to its parent.
fn relative_to(path: &str, roots: &[String]) -> String {
    roots
        .iter()
        .filter(|r| path.starts_with(r.as_str()))
        .max_by_key(|r| r.len())
        .and_then(|r| path.strip_prefix(r.as_str()))
        .map(|s| s.trim_start_matches('/').to_string())
        .unwrap_or_else(|| path.to_string())
}

/// One folder a question can be narrowed to.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub path: String,
    pub files: i64,
}

impl Core {
    pub fn open(path: std::path::PathBuf) -> Result<Self> {
        // A database at the filesystem root has no parent; that cannot happen
        // in practice and an empty path here would only ever produce a clear
        // failure at the moment something tried to use it.
        let data_dir = path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default();
        // The composition root assembles the migration chain: `index` depends
        // on `store`, so store cannot reference it back without a cycle.
        let store = Store::open_with_migrations(path, marrow_index::MIGRATIONS)?;
        let index = Fts5Index::open(&store)?;
        let vectors = marrow_index::SqliteVectorIndex::open(&store)?;
        Ok(Self {
            store,
            index,
            vectors,
            data_dir,
        })
    }

    /// Where Marrow keeps its own files — the database, the preferences, the
    /// scratch workspace. Never a folder the user granted.
    pub fn data_dir(&self) -> &std::path::Path {
        &self.data_dir
    }

    /// The vector index, for the backfill and for `search_hybrid`.
    pub fn vectors(&self) -> &marrow_index::SqliteVectorIndex {
        &self.vectors
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// The lexical index, so the watcher can keep it in step with the store.
    pub fn index(&self) -> &Fts5Index {
        &self.index
    }

    /// Retrieve for a **question** rather than for a search box.
    ///
    /// A different mode, and the difference is not a nicety: the search field
    /// is conjunctive and prefix-matched because that is what as-you-type
    /// wants, and a question run through it matches nothing. "When does the
    /// lease renew?" needs a document containing every one of those words;
    /// the lease says "renews".
    ///
    /// This is the fallback ASK-004 promises when the router cannot run. The
    /// router will do better — it rewrites the query — but this must work
    /// without it, because a broken router has to degrade to the product that
    /// already worked rather than to an error.
    /// Re-order hits to match a fused ranking, fetching any the lexical branch
    /// did not return.
    ///
    /// A semantic-only candidate has no `TextHit` behind it, and a `TextHit` is
    /// what everything downstream renders and cites from. Without this the
    /// vector branch could only re-rank what lexical already found, which is
    /// not semantic search.
    fn hydrate_in_order(
        &self,
        order: &[marrow_core::ChunkId],
        lexical: &[marrow_index::TextHit],
    ) -> Result<Vec<marrow_index::TextHit>> {
        let conn = self.store.reader()?;
        let mut out = Vec::with_capacity(order.len());
        for id in order {
            if let Some(h) = lexical.iter().find(|h| h.chunk_id == *id) {
                out.push(h.clone());
                continue;
            }
            match hydrate_chunk(&conn, *id)? {
                Some(h) => out.push(h),
                // A vector for a chunk the canonical store no longer has. The
                // derived index is rebuildable, so this is a stale row — but it
                // must not become a result nobody can open.
                None => tracing::warn!(chunk = %id, "vector hit with no chunk; dropped"),
            }
        }
        Ok(out)
    }

    /// `scope` restricts the answer to one subtree — `services/STT` — because a
    /// workspace is routinely one folder holding many unrelated projects, and
    /// a question about one of them has no business being answered from the
    /// others. Asking "what is STT?" over `~/Desktop/melp` returned MFA
    /// settings and a code of conduct alongside the service that was asked
    /// about, with nothing saying the sources came from different projects.
    pub fn retrieve(
        &self,
        question: &str,
        limit: usize,
        embedding: Option<&marrow_index::Embedding>,
        scope: Option<&str>,
    ) -> Result<Vec<RetrievedChunk>> {
        // Reduced here, not by the caller: the lexical branch is disjunctive,
        // so an unreduced question matches every document containing "the".
        let reduced = retrieval_terms(question);
        let trimmed = reduced.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }
        let want = limit.clamp(1, 50);
        let scope = scope_fragment(scope);
        let mut q = TextQuery::new(trimmed)
            .mode(marrow_index::MatchMode::Any)
            .with_snippet(Self::evidence_snippet())
            .limit(want);
        if let Some(fragment) = &scope {
            // The filter goes INTO the query, not onto its results. Applied
            // afterwards the index would take its `limit` first and the filter
            // would then discard most of what came back — so a scope could
            // report nothing while matching documents sat just past the cut.
            q = q.with_filters(marrow_index::Filters {
                // GLOB, so the substring has to be wrapped rather than passed
                // raw. It is a bound parameter, so nothing in it becomes SQL.
                path_glob: Some(format!("*{fragment}*")),
                ..Default::default()
            });
        }
        let roots = self.roots()?;

        // The semantic branch when there is one, lexical alone when there is
        // not. `None` is the ordinary state before a backfill has run, and it
        // must not turn into an error (hard rule 10).
        let lexical = self.index.search(&q)?;
        let mut hits = lexical.clone();
        if let Some(e) = embedding {
            // The vector index has no path filter, so a scoped question can
            // only narrow this branch after the fact — which is the very thing
            // the lexical filter goes into the query to avoid. Over-fetching is
            // what buys the filtering something to keep: without it the
            // unscoped nearest neighbours fill the limit and the scope is left
            // with whichever of them happened to fall inside it.
            let depth = if scope.is_some() {
                want.saturating_mul(8).clamp(1, 200)
            } else {
                want
            };
            match self
                .vectors
                .search(&marrow_index::VectorQuery::new(e.clone()).limit(depth))
            {
                Ok(semantic) => {
                    // Fused by rank, so neither branch's scores need
                    // normalizing against the other's (§113.2).
                    let branches = [
                        marrow_query::search::Branch {
                            name: marrow_query::search::LEXICAL,
                            weight: marrow_query::search::LEXICAL_WEIGHT,
                            ranked: lexical.iter().map(|h| h.chunk_id).collect(),
                        },
                        marrow_query::search::Branch {
                            name: marrow_query::search::SEMANTIC,
                            weight: marrow_query::search::SEMANTIC_WEIGHT,
                            ranked: semantic.iter().map(|h| h.chunk_id).collect(),
                        },
                    ];
                    let order: Vec<marrow_core::ChunkId> =
                        marrow_query::search::rrf(&branches, marrow_query::search::RRF_K)
                            .into_iter()
                            .map(|c| c.chunk_id)
                            .collect();
                    hits = self.hydrate_in_order(&order, &lexical)?;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "the semantic branch failed; answering lexically")
                }
            }
        }

        // Whatever the semantic branch contributed is unscoped, so it has to be
        // dropped here. Nothing the lexical branch found is lost to this: that
        // one was filtered in the query and kept its full budget.
        if let Some(fragment) = &scope {
            hits.retain(|h| h.path.contains(fragment.as_str()));
        }
        // Fusing two branches of `want` candidates each yields up to twice as
        // many, and every one of them reaches the model — which is how a
        // twelve-chunk budget produced an answer citing twenty-four sources.
        // The caller asked for `limit`; giving it more is not generosity, it is
        // the budget §114 exists to protect being quietly doubled.
        hits.truncate(want);

        Ok(hits
            .iter()
            .map(|h| {
                let relative = relative_to(&h.path, &roots);
                RetrievedChunk {
                    location: match &h.span {
                        marrow_core::SourceSpan::Lines { start, .. } => {
                            format!("{relative}:{start}")
                        }
                        _ => relative.clone(),
                    },
                    line: match &h.span {
                        marrow_core::SourceSpan::Lines { start, .. } => Some(*start),
                        _ => None,
                    },
                    relative_path: relative,
                    path: h.path.clone(),
                    // The whole window, markers stripped. Those are for
                    // highlighting in a result row; a model asked to reason
                    // about control characters is being asked the wrong thing.
                    text: h
                        .snippet
                        .text
                        .chars()
                        .filter(|c| *c != '\u{1}' && *c != '\u{2}')
                        .collect(),
                    provenance: h.provenance,
                    origin: h.origin,
                }
            })
            .collect())
    }

    /// The widest snippet FTS5 will produce.
    ///
    /// A result row wants two or three lines (UX §4); an **evidence block**
    /// wants enough of the document to answer from. The first time this ran
    /// end to end the model reported, correctly, that the evidence blocks
    /// contained a path and no text — the row-sized snippet had nothing in it.
    fn evidence_snippet() -> marrow_index::SnippetOptions {
        marrow_index::SnippetOptions {
            // FTS5 caps this at 64.
            tokens: 64,
            max_chars: 1_400,
            // The body, not whichever column matched best. A question about a
            // lease matches `lease.md` in the *path* column, and FTS5 would
            // hand back the filename — evidence with no text in it.
            column: Some(marrow_index::TextField::Body),
        }
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<SearchResponse> {
        self.search_with(query, limit, marrow_index::MatchMode::Prefix)
    }

    fn search_with(
        &self,
        query: &str,
        limit: usize,
        mode: marrow_index::MatchMode,
    ) -> Result<SearchResponse> {
        let started = std::time::Instant::now();
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok(SearchResponse {
                query: query.to_string(),
                total: 0,
                matched: 0,
                elapsed_ms: 0,
                hits: Vec::new(),
                branches: vec!["lexical".into()],
            });
        }

        let capped = limit.clamp(1, 200);

        // **Prefix mode, because this is an as-you-type field.**
        //
        // Whole-token matching means `enclav` matches nothing until the final
        // `e` is typed — every intermediate keystroke shows an empty result
        // list, which reads as "search is broken" rather than "keep typing".
        // Prefix makes the last term match as a prefix; GUI §5.2 calls this the
        // as-you-type mode and it measures at ~415 µs.
        let mut q = TextQuery::new(trimmed).mode(mode).limit(capped);
        if mode == marrow_index::MatchMode::Any {
            q = q.with_snippet(Self::evidence_snippet());
        }
        let raw = self.index.search(&q)?;

        // How many documents actually matched, so the footer does not report
        // the page size as the result count. Asking for one more than the page
        // is enough to distinguish "exactly a page" from "more than a page";
        // beyond that the number is a count, not a ranking, so a cheap
        // over-fetch is the honest trade.
        let matched = if raw.len() < capped {
            raw.len()
        } else {
            self.index
                .search(
                    &TextQuery::new(trimmed)
                        .mode(marrow_index::MatchMode::Prefix)
                        .limit(capped * 10),
                )
                .map(|r| r.len())
                .unwrap_or(raw.len())
        };
        let roots = self.roots()?;
        let hits: Vec<SearchHit> = raw
            .iter()
            .enumerate()
            .map(|(i, h)| to_hit(i + 1, h, &roots))
            .collect();

        Ok(SearchResponse {
            query: trimmed.to_string(),
            total: hits.len(),
            matched,
            elapsed_ms: started.elapsed().as_millis() as u64,
            hits,
            branches: vec!["lexical".into()],
        })
    }

    /// Register a folder as an authorized root.
    ///
    /// Canonicalizes first, because a root that is not canonical defeats every
    /// containment check that depends on it (invariant #5). Refuses a folder
    /// that is already granted, or that overlaps one — nesting two roots means
    /// every file underneath is stored twice under two identities, and path is
    /// never identity (invariant #2).
    pub fn grant(&self, path: &std::path::Path) -> Result<marrow_core::RootId> {
        self.grant_named(path, None)
    }

    /// [`Core::grant`], with the workspace's display name chosen by the caller.
    ///
    /// The name is otherwise the folder's own final component, which is right
    /// for a folder the user picked and wrong for the one Marrow owns: the
    /// scratch root is a directory called `dropped` inside the application's
    /// data directory, and a sidebar row reading "dropped" says nothing about
    /// what is in it. Only the *name* differs — it is granted, canonicalized,
    /// overlap-checked, watched and indexed exactly like any other root, which
    /// is the whole point of making it a real one.
    pub fn grant_named(
        &self,
        path: &std::path::Path,
        name: Option<&str>,
    ) -> Result<marrow_core::RootId> {
        let root = marrow_scan::AuthorizedRoot::open(path)?;
        let canonical = root.path().to_path_buf();

        for existing in marrow_query::catalog::roots(&self.store.reader()?)? {
            let existing = std::path::Path::new(&existing);
            if existing == canonical {
                return Err(marrow_core::Error::new(
                    marrow_core::Code::ActAlreadyExists,
                    format!("{} is already indexed.", canonical.display()),
                ));
            }
            if canonical.starts_with(existing) || existing.starts_with(&canonical) {
                return Err(marrow_core::Error::new(
                    marrow_core::Code::CfgInvalid,
                    format!(
                        "{} overlaps {}, which is already indexed. Indexing one inside \
                         the other would store every file underneath twice, under two \
                         identities. Pick a folder outside it.",
                        canonical.display(),
                        existing.display()
                    ),
                ));
            }
        }

        let name = name.map(str::to_string).unwrap_or_else(|| {
            canonical
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "workspace".to_string())
        });
        let now = marrow_core::Timestamp::now();
        let workspace_id = self.store.upsert_workspace(marrow_store::NewWorkspace {
            workspace_id: marrow_core::WorkspaceId::new(),
            name,
            at: now,
        })?;
        let root_id = self.store.upsert_root(marrow_store::NewRoot {
            root_id: marrow_core::RootId::new(),
            workspace_id,
            canonical_path: canonical.to_string_lossy().into_owned(),
            volume_identity: None,
            grant_token: None,
            storage_kind: marrow_store::StorageKind::Local,
            cloud_provider: None,
            at: now,
        })?;
        self.store.flush()?;
        Ok(root_id)
    }

    /// The projects a question can be scoped to, with how much each holds.
    ///
    /// Derived rather than configured: a "project" here is just a folder under
    /// the workspace root that holds indexed files, and the depth is chosen the
    /// same way [`crate::ask::projects_of`] chooses it when naming the projects
    /// an answer drew from. The two must agree — a scope the user picks from a
    /// list and a project named in an answer have to be the same kind of thing,
    /// or picking one would not narrow to the other.
    ///
    /// One segment when that already distinguishes the folders, two when it does
    /// not. Under a root whose only child is `services/`, one segment names the
    /// container and nothing else, which is no use as a filter.
    pub fn projects(&self) -> Result<Vec<Project>> {
        let conn = self.store.reader()?;
        let roots = marrow_query::catalog::roots(&conn)?;
        let mut stmt = conn
            .prepare(
                "SELECT current_path FROM files
                  WHERE status='ACTIVE' AND current_path IS NOT NULL",
            )
            .map_err(|e| marrow_store::map_sqlite(e, "listing files for projects"))?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| marrow_store::map_sqlite(e, "listing files for projects"))?;

        let mut folders: Vec<Vec<String>> = Vec::new();
        for row in rows {
            let path = row.map_err(|e| marrow_store::map_sqlite(e, "reading a file path"))?;
            let rel = relative_to(&path, &roots);
            let segments: Vec<String> = rel.split('/').map(str::to_string).collect();
            if segments.len() > 1 {
                folders.push(segments[..segments.len() - 1].to_vec());
            }
        }

        let at = |n: usize| -> std::collections::BTreeMap<String, i64> {
            let mut counts = std::collections::BTreeMap::new();
            for f in &folders {
                if f.len() < n {
                    continue;
                }
                *counts.entry(f[..n].join("/")).or_insert(0) += 1;
            }
            counts
        };

        let top = at(1);
        let chosen = if top.len() > 1 { top } else { at(2) };
        Ok(chosen
            .into_iter()
            .map(|(path, files)| Project { path, files })
            .collect())
    }

    pub fn workspaces(&self) -> Result<Vec<WorkspaceRow>> {
        // One statement, in `marrow-query`. This and MCP's listing were two
        // separately-maintained queries answering the same question about the
        // same index.
        let conn = self.store.reader()?;
        // Compared as a path, not as a name: the workspace's *name* is a label
        // and a user could grant a folder called "Dropped files" of their own.
        // What makes a workspace the scratch one is that it is the directory
        // Marrow created inside its own data directory, and that is a fact
        // about the path.
        //
        // Canonicalized, because the stored root is. `~/.local/share` is
        // routinely reached through a symlink and a temporary directory on
        // macOS always is (`/var` → `/private/var`), so comparing the
        // unresolved form matches nothing. `None` when it does not exist yet,
        // which is not equal to any stored path — the intended answer.
        let scratch = std::fs::canonicalize(crate::scratch::dir_in(&self.data_dir)).ok();
        Ok(marrow_query::catalog::workspace_stats(&conn)?
            .into_iter()
            .map(|w| WorkspaceRow {
                scratch: scratch.as_deref() == Some(std::path::Path::new(&w.path)),
                name: w.name,
                path: w.path,
                files: w.files,
                chunks: w.chunks,
                content_bytes: w.content_bytes,
                cloud_only: w.cloud_only,
                unindexed: w.unindexed,
                no_parser: w.no_parser,
                parse_failed: w.parse_failed,
                not_processed: w.not_processed,
            })
            .collect())
    }

    pub fn health(&self) -> Result<IndexHealth> {
        let conn = self.store.reader()?;
        let s = marrow_query::catalog::index_stats(&conn)?;
        Ok(IndexHealth {
            files: s.files,
            chunks: s.chunks,
            content_bytes: s.content_bytes,
            cloud_only: s.cloud_only,
            // From the database, not from a build constant: the chain is
            // numbered across crates, so `marrow_core::SCHEMA_VERSION` is the
            // store's own maximum and not what an open database is at.
            schema_version: s.schema_version,
            last_indexed_ms: s.last_reconciled_ms,
            may_be_stale: s.may_be_stale(),
            watcher: s.watcher_health.clone(),
        })
    }

    pub fn file_detail(&self, path: &str) -> Result<FileDetail> {
        let conn = self.store.reader()?;
        let row = conn
            .query_row(
                "SELECT f.file_id, f.tier_state, f.origin, w.name,
                        v.size_bytes, v.content_hash, v.mime, v.mtime_ms,
                        (SELECT count(*) FROM file_versions x WHERE x.file_id=f.file_id),
                        (SELECT count(*) FROM chunks c WHERE c.version_id=v.version_id)
                   FROM files f
                   JOIN workspaces w ON w.workspace_id=f.workspace_id
              LEFT JOIN file_versions v ON v.file_id=f.file_id AND v.status='CURRENT'
                  WHERE f.current_path=?1 AND f.status='ACTIVE' LIMIT 1",
                [path],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<i64>>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, Option<String>>(6)?,
                        r.get::<_, Option<i64>>(7)?,
                        r.get::<_, i64>(8)?,
                        r.get::<_, i64>(9)?,
                    ))
                },
            )
            .map_err(|_| {
                marrow_core::Error::new(
                    marrow_core::Code::FsNotFound,
                    "That file is not indexed. Add its folder as a workspace, then run an index.",
                )
            })?;

        let (file_id, tier, origin, workspace, size, hash, mime, mtime, versions, chunks) = row;
        let mut stmt = conn
            .prepare("SELECT path FROM file_paths WHERE file_id=?1 ORDER BY observed_from")
            .map_err(|e| marrow_store::map_sqlite(e, "reading path history"))?;
        let history: Vec<String> = stmt
            .query_map([&file_id], |r| r.get(0))
            .and_then(|it| it.collect())
            .map_err(|e| marrow_store::map_sqlite(e, "reading path history"))?;

        Ok(FileDetail {
            path: path.to_string(),
            file_id,
            workspace,
            size_bytes: size,
            content_hash: hash,
            mime,
            modified_ms: mtime,
            versions,
            chunks,
            tier_state: tier.to_lowercase(),
            citable: origin == "USER",
            previous_paths: history.into_iter().filter(|p| p != path).collect(),
            // M1 extracts neither. `None` renders as `—`; omitting the field
            // would make absence look like emptiness (FI-003).
            embedded_metadata: None,
            structure: None,
        })
    }

    /// Lines around a match, for the preview pane.
    ///
    /// Bounded on both sides: a 50 MB file renders its matched region, never
    /// the whole file (GUI §7).
    pub fn read_region(&self, path: &str, around: Option<u32>) -> Result<Region> {
        const CONTEXT: u32 = 40;
        const MAX_LINES: usize = 400;

        let conn = self.store.reader()?;
        let tier: String = conn
            .query_row(
                "SELECT tier_state FROM files WHERE current_path=?1 AND status='ACTIVE' LIMIT 1",
                [path],
                |r| r.get(0),
            )
            .map_err(|_| {
                marrow_core::Error::new(
                    marrow_core::Code::FsNotFound,
                    "That file is not indexed, so Marrow will not read it.",
                )
            })?;
        if tier != "RESIDENT" {
            // **Invariant #5.** Opening it is what triggers the download.
            return Err(marrow_core::Error::new(
                marrow_core::Code::FsPlaceholderSkipped,
                "That file is cloud-only. Its contents are not on this machine, and \
                 opening it would download them.",
            ));
        }

        let body = std::fs::read_to_string(path)?;
        let (from, to) = match around {
            Some(l) => (l.saturating_sub(CONTEXT).max(1), l + CONTEXT),
            None => (1, MAX_LINES as u32),
        };
        let selected: Vec<String> = body
            .lines()
            .enumerate()
            .filter(|(i, _)| {
                let n = *i as u32 + 1;
                n >= from && n <= to
            })
            .map(|(_, l)| l.to_string())
            .collect();

        let truncated = selected.len() > MAX_LINES;
        Ok(Region {
            first_line: from,
            lines: selected.into_iter().take(MAX_LINES).collect(),
            truncated,
        })
    }

    /// List indexed files, newest first.
    ///
    /// Browsing is not searching: the Files view was built on `search`, so with
    /// no query it showed an empty pane for an index holding 35,000 files.
    pub fn list_files(
        &self,
        workspace: Option<&str>,
        prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<FileRow>> {
        let conn = self.store.reader()?;
        let roots = self.roots()?;
        let limit = limit.clamp(1, 1000) as i64;

        let mut stmt = conn
            .prepare(
                "SELECT f.current_path, w.name, v.size_bytes, v.mtime_ms,
                        (SELECT count(*) FROM chunks c WHERE c.version_id = v.version_id)
                   FROM files f
                   JOIN workspaces w ON w.workspace_id = f.workspace_id
              LEFT JOIN file_versions v
                     ON v.file_id = f.file_id AND v.status = 'CURRENT'
                  WHERE f.status = 'ACTIVE'
                    AND f.current_path IS NOT NULL
                    AND (?1 IS NULL OR w.name = ?1)
                    AND (?2 IS NULL OR lower(f.current_path) LIKE '%' || lower(?2) || '%')
               ORDER BY COALESCE(v.mtime_ms, 0) DESC
                  LIMIT ?3",
            )
            .map_err(|e| marrow_store::map_sqlite(e, "listing files"))?;

        let rows = stmt
            .query_map(
                marrow_store::rusqlite::params![workspace, prefix, limit],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                        r.get::<_, Option<i64>>(3)?,
                        r.get::<_, i64>(4)?,
                    ))
                },
            )
            .and_then(|it| it.collect::<std::result::Result<Vec<_>, _>>())
            .map_err(|e| marrow_store::map_sqlite(e, "listing files"))?;

        Ok(rows
            .into_iter()
            .map(
                |(path, workspace, size_bytes, modified_ms, chunks)| FileRow {
                    relative_path: roots
                        .iter()
                        .filter(|r| path.starts_with(r.as_str()))
                        .max_by_key(|r| r.len())
                        .and_then(|r| path.strip_prefix(r.as_str()))
                        .map(|s| s.trim_start_matches('/').to_string())
                        .unwrap_or_else(|| path.clone()),
                    path,
                    workspace,
                    size_bytes,
                    modified_ms,
                    chunks,
                    metadata_only: chunks == 0,
                },
            )
            .collect())
    }

    /// Hand a file to the system, or reveal it in the file manager.
    ///
    /// Guarded by the index for the same reason `read_region` is: the workspace
    /// grant says which files Marrow may touch, and handing one to another
    /// application is still touching it.
    pub fn open_path(&self, path: &str, reveal: bool) -> Result<()> {
        let conn = self.store.reader()?;
        let known: i64 = conn
            .query_row(
                "SELECT count(*) FROM files WHERE current_path=?1 AND status='ACTIVE'",
                [path],
                |r| r.get(0),
            )
            .map_err(|e| marrow_store::map_sqlite(e, "looking up a file"))?;
        if known == 0 {
            return Err(marrow_core::Error::new(
                marrow_core::Code::FsNotFound,
                "That file is not indexed, so Marrow will not open it.",
            ));
        }

        // Structured argv, never a shell string (SEC-011): a filename
        // containing a quote or a semicolon is a filename, not a command.
        let mut cmd = std::process::Command::new("/usr/bin/open");
        if reveal {
            cmd.arg("-R");
        }
        cmd.arg(path);
        cmd.status()
            .map_err(|e| {
                marrow_core::Error::new(
                    marrow_core::Code::FsLocked,
                    "Could not open that file. The system reported an error.",
                )
                .with_source(e)
            })
            .map(|_| ())
    }

    fn roots(&self) -> Result<Vec<String>> {
        marrow_query::catalog::roots(&self.store.reader()?)
    }

    // ------------------------------------------------------- conversations
    //
    // The window used to hold the thread in component state, so there was one
    // conversation and it ended with the process. These four are what make the
    // sidebar's list worth its width.

    pub fn conversations(&self, limit: usize) -> Result<Vec<ConversationSummary>> {
        let conn = self.store.reader()?;
        Ok(
            marrow_store::conversations::list_conversations(&conn, limit)?
                .into_iter()
                .map(|c| ConversationSummary {
                    id: c.conversation_id,
                    title: c.title,
                    scope: c.scope,
                    created_ms: c.created_at.as_millis(),
                    updated_ms: c.updated_at.as_millis(),
                    turns: c.turns,
                })
                .collect(),
        )
    }

    pub fn conversation(&self, id: &str) -> Result<ConversationDetail> {
        let conn = self.store.reader()?;
        let (head, turns) = marrow_store::conversations::load_conversation(&conn, id)?;
        Ok(ConversationDetail {
            id: head.conversation_id,
            title: head.title,
            scope: head.scope,
            turns: turns
                .into_iter()
                .map(|t| {
                    // Stored JSON, parsed back into the shapes the window
                    // renders. A blob that will not parse costs the turn its
                    // citation list and nothing else: the answer is still what
                    // was said, and refusing to open the whole conversation
                    // over one unreadable column would lose far more.
                    let citations: Vec<crate::ask::Citation> = decode(&t.citations, "citations");
                    StoredTurn {
                        question: t.question,
                        answer: t.answer,
                        thorough: t.mode == marrow_store::TurnMode::Thorough,
                        model: t.model,
                        scope: t.scope,
                        projects: crate::ask::projects_of(&citations),
                        citations,
                        excluded: decode(&t.excluded, "excluded"),
                        usage: t.usage.as_deref().and_then(|u| decode_one(u, "usage")),
                        asked_ms: t.asked_at.as_millis(),
                    }
                })
                .collect(),
        })
    }

    /// Persist a finished exchange, starting the conversation if there is not
    /// one yet.
    ///
    /// The citations go in as the JSON that was shown, not as references to the
    /// chunks they came from: reopening a conversation must show what the
    /// answer actually cited, and a chunk can be superseded or its file deleted
    /// between asking and returning.
    pub fn save_turn(&self, into: Option<String>, turn: NewTurn) -> Result<SavedTurn> {
        let question = turn.question.clone();
        let id = self.store.append_turn(
            into,
            marrow_store::NewTurn {
                question: turn.question,
                answer: turn.answer,
                mode: if turn.thorough {
                    marrow_store::TurnMode::Thorough
                } else {
                    marrow_store::TurnMode::Fast
                },
                model: turn.model,
                scope: turn.scope,
                citations: encode(&turn.citations),
                excluded: encode(&turn.excluded),
                usage: turn.usage.as_ref().map(encode),
                at: marrow_core::Timestamp::now(),
            },
        )?;
        Ok(SavedTurn {
            id,
            // Derived from the first question, so the window can label the row
            // it just created without a second round trip for the list.
            title: marrow_store::conversations::title_from(&question),
        })
    }

    pub fn rename_conversation(&self, id: String, title: String) -> Result<()> {
        self.store
            .rename_conversation(id, title, marrow_core::Timestamp::now())
    }

    /// Soft delete. The rows stay; `status` moves to `DELETED`.
    pub fn delete_conversation(&self, id: String) -> Result<()> {
        self.store.delete_conversation(id)
    }
}

/// Serialize a value the store holds as an opaque JSON column.
///
/// Infallible in practice — these are plain structs of strings and numbers, and
/// `serde_json` only fails on a map with non-string keys or a non-finite float.
/// An empty array is the honest fallback: it says "nothing recorded", which is
/// exactly what a column that could not be written would mean.
fn encode<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "a conversation column could not be serialized");
        "[]".to_string()
    })
}

fn decode<T: serde::de::DeserializeOwned>(json: &str, what: &str) -> Vec<T> {
    serde_json::from_str(json).unwrap_or_else(|e| {
        tracing::warn!(error = %e, column = what, "stored conversation JSON did not parse");
        Vec::new()
    })
}

fn decode_one<T: serde::de::DeserializeOwned>(json: &str, what: &str) -> Option<T> {
    serde_json::from_str(json)
        .map_err(
            |e| tracing::warn!(error = %e, column = what, "stored conversation JSON did not parse"),
        )
        .ok()
}

/// One chunk as a `TextHit`, for a candidate only the semantic branch found.
///
/// The snippet is the chunk's own opening rather than a match window: there is
/// no matched term to centre on, and inventing highlight markers would claim a
/// match that did not happen.
fn hydrate_chunk(
    conn: &marrow_store::ReadConn,
    id: marrow_core::ChunkId,
) -> Result<Option<marrow_index::TextHit>> {
    const PREVIEW: usize = 1_400;
    let row = conn
        .query_row(
            "SELECT c.text, COALESCE(c.context_prefix,''), c.provenance_class, c.version_id,
                    v.path_at_observation, v.mtime_ms, v.file_id, f.workspace_id, f.origin
               FROM chunks c
               JOIN file_versions v ON v.version_id = c.version_id
               JOIN files f ON f.file_id = v.file_id
              WHERE c.chunk_id = ?1 AND c.status = 'ACTIVE' AND f.status = 'ACTIVE'",
            [id.to_string()],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, i64>(5)?,
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

    let Some((text, title, provenance, version, path, mtime, file, ws, origin)) = row else {
        return Ok(None);
    };
    let (Ok(version_id), Ok(file_id), Ok(workspace_id)) =
        (version.parse(), file.parse(), ws.parse())
    else {
        return Ok(None);
    };

    Ok(Some(marrow_index::TextHit {
        chunk_id: id,
        file_id,
        version_id,
        workspace_id,
        path,
        title,
        // Branch-local and never compared across branches (§113.2).
        score: 0.0,
        span: marrow_core::SourceSpan::Whole,
        snippet: marrow_index::Snippet {
            text: text.chars().take(PREVIEW).collect(),
            // No matched term to point at. Claiming one would highlight a word
            // the user never searched for.
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
    }))
}

#[cfg(test)]
mod retrieval_tests {
    use super::{retrieval_terms, scope_fragment};

    #[test]
    fn a_scope_is_a_subtree_however_the_caller_spelled_it() {
        // `/services/STT/` and `services/STT` are the same subtree, and an
        // empty scope is no scope rather than a filter nothing can satisfy.
        assert_eq!(
            scope_fragment(Some("/services/STT/")).as_deref(),
            Some("services/STT")
        );
        assert_eq!(scope_fragment(Some("  ")), None);
        assert_eq!(scope_fragment(Some("/")), None);
        assert_eq!(scope_fragment(None), None);
    }

    #[test]
    fn a_question_is_reduced_to_the_words_worth_retrieving_on() {
        assert_eq!(
            retrieval_terms("When does the lease renew and what is the rent?"),
            "lease renew rent"
        );
        assert_eq!(retrieval_terms("Where is my invoice?"), "invoice");
    }

    #[test]
    fn the_stopwords_are_dropped_because_the_query_is_disjunctive() {
        // The bug this exists for: `retrieve` ORs its terms, so "the" alone
        // matches every document in the index. Unreduced, a question returns a
        // result set that looks full and means nothing.
        let reduced = retrieval_terms("when does the lease renew?");
        for stop in ["when", "does", "the"] {
            assert!(
                !reduced.split_whitespace().any(|w| w == stop),
                "`{stop}` survived: {reduced}"
            );
        }
    }

    #[test]
    fn punctuation_and_case_do_not_survive_into_the_query() {
        // The index refuses a query of pure punctuation, so a question ending
        // in "?!" must not carry it through.
        assert_eq!(
            retrieval_terms("What's the rent -- exactly?!"),
            "rent exactly"
        );
    }

    #[test]
    fn a_question_made_only_of_stopwords_still_searches_for_something() {
        // Reducing it to nothing would turn a strange question into a refusal
        // rather than an empty result, and those read very differently.
        assert_eq!(retrieval_terms("what is it?"), "what is it?");
    }
}
