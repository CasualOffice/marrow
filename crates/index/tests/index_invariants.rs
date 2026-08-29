//! The named tests that must never be allowed to rot (Part 6 §116.3).
//!
//! Each one holds up a property that is either the reason for a decision or the
//! reason a decision is safe. If one of these has to be weakened to make a
//! change pass, the change is wrong.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use marrow_core::{
    ChunkId, Code, ContentHash, FileId, NodeId, Origin, ProvenanceClass, Result, SourceSpan,
    TierState, Timestamp, VersionId, WorkspaceId,
};
use marrow_index::fts5::{self, Fts5Index, StoreChunkSource};
use marrow_index::literal::{literal_search, LiteralQuery, LiteralTarget, StopReason};
use marrow_index::{MatchMode, TextDoc, TextIndex, TextQuery};
use marrow_store::rusqlite::{params, Connection};
use marrow_store::{
    map_sqlite, read, NewFile, NewRoot, NewVersion, NewWorkspace, Store, WriterConfig,
};

// ------------------------------------------------------------------ fixture

/// A store on disk with one workspace and one root, plus helpers for the
/// canonical rows an index document has to derive from.
struct Fixture {
    dir: tempfile::TempDir,
    store: Option<Store>,
    workspace: WorkspaceId,
    root: marrow_core::RootId,
}

impl Fixture {
    fn new() -> Self {
        Self::with_config(WriterConfig::default())
    }

    /// A fixture whose writer never commits on its own. Every write has to be
    /// flushed explicitly, which is what makes "roll this batch back" a thing
    /// the test controls rather than a thing it races.
    fn with_long_batches() -> Self {
        Self::with_config(WriterConfig {
            max_batch_rows: 1_000_000,
            max_batch_interval: Duration::from_secs(300),
            ..WriterConfig::default()
        })
    }

    fn with_config(cfg: WriterConfig) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open_with_config(dir.path().join("marrow.sqlite"), cfg).expect("open");
        let mut f = Self {
            dir,
            store: Some(store),
            workspace: WorkspaceId::new(),
            root: marrow_core::RootId::new(),
        };
        let root_path = f.dir.path().display().to_string();
        let (ws, root) = f.commit(move |c| {
            let ws = read::upsert_workspace(c, &NewWorkspace::new("test"))?;
            let root = read::upsert_root(c, &NewRoot::new(ws, root_path))?;
            Ok((ws, root))
        });
        f.workspace = ws;
        f.root = root;
        f
    }

    fn store(&self) -> &Store {
        self.store.as_ref().expect("store is open")
    }

    fn db(&self) -> PathBuf {
        self.dir.path().join("marrow.sqlite")
    }

    /// Run `f` in a writer batch and force that batch to commit.
    fn commit<T, F>(&self, f: F) -> T
    where
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let pending = self.store().writer().send(f).expect("send");
        self.store().flush().expect("flush");
        pending.wait().expect("commit")
    }

    fn read<T>(&self, f: impl FnOnce(&Connection) -> T) -> T {
        let reader = self.store().reader().expect("reader");
        f(&reader)
    }

    /// Kill the writer without committing its open batch: what process death
    /// looks like from SQLite's side.
    fn abort_writer(&mut self) {
        if let Some(s) = self.store.take() {
            s.abort();
        }
    }

    fn reopen(&mut self) {
        if let Some(s) = self.store.take() {
            s.close().expect("close");
        }
        self.store = Some(Store::open(self.db()).expect("reopen"));
    }

    fn index(&self) -> Fts5Index {
        Fts5Index::open(self.store()).expect("open index")
    }

    /// A file with one version, both ACTIVE/CURRENT.
    fn add_file(&self, path: &str) -> (FileId, VersionId) {
        let full = self.dir.path().join(path).display().to_string();
        let f = NewFile::new(self.workspace, self.root, full.clone());
        let v = NewVersion::new(f.file_id, full, 1, ContentHash::of(path.as_bytes()));
        let (fi, vi) = (f.file_id, v.version_id);
        self.commit(move |c| read::insert_file_with_version(c, &f, &v));
        (fi, vi)
    }

    /// The document for a chunk, without writing anything.
    fn doc(
        &self,
        file: FileId,
        version: VersionId,
        path: &str,
        title: &str,
        body: &str,
    ) -> TextDoc {
        TextDoc {
            chunk_id: ChunkId::new(),
            file_id: file,
            version_id: version,
            workspace_id: self.workspace,
            path: self.dir.path().join(path).display().to_string(),
            title: title.to_string(),
            body: body.to_string(),
            span: SourceSpan::Bytes {
                start: 0,
                end: body.len() as u64,
            },
            provenance: ProvenanceClass::Exact,
            origin: Origin::User,
            modified: Timestamp::from_millis(1_700_000_000_000),
        }
    }

    /// Write the canonical chunk (and its IR node) for `doc`. Does **not**
    /// index it — that is the caller's choice, which is what lets a test
    /// deliberately leave the index stale.
    fn add_chunk(&self, doc: &TextDoc) {
        let d = doc.clone();
        self.commit(move |c| insert_chunk_row(c, &d));
    }

    /// Canonical chunk plus its index document, in one transaction (D3).
    fn add_indexed_chunk(&self, doc: &TextDoc) {
        let d = doc.clone();
        self.commit(move |c| {
            insert_chunk_row(c, &d)?;
            fts5::upsert_docs(c, std::slice::from_ref(&d))
        });
    }

    fn doc_rows(&self) -> i64 {
        self.read(|c| {
            c.query_row("SELECT count(*) FROM text_index_docs", [], |r| r.get(0))
                .expect("count docs")
        })
    }

    fn fts_rows(&self) -> i64 {
        self.read(|c| {
            c.query_row("SELECT count(*) FROM text_index", [], |r| r.get(0))
                .expect("count fts rows")
        })
    }

    fn chunk_rows(&self) -> i64 {
        self.read(|c| {
            c.query_row("SELECT count(*) FROM chunks", [], |r| r.get(0))
                .expect("count chunks")
        })
    }
}

/// The canonical rows behind one index document: an IR node carrying the
/// `source_span` (invariant #1) and the chunk that points at it.
fn insert_chunk_row(conn: &Connection, doc: &TextDoc) -> Result<()> {
    let node = NodeId::new();
    conn.execute(
        "INSERT INTO ir_nodes (node_id, version_id, kind, ordinal, source_span, trust)
         VALUES (?1, ?2, 'paragraph', 0, ?3, 'UNTRUSTED_CONTENT')",
        params![
            node.to_string(),
            doc.version_id.to_string(),
            serde_json::to_string(&doc.span).expect("span json"),
        ],
    )
    .map_err(|e| map_sqlite(e, "test: insert ir node"))?;
    conn.execute(
        "INSERT INTO chunks (chunk_id, version_id, root_node_id, chunk_kind, text,
                             context_prefix, token_count, text_hash, chunker_version,
                             provenance_class, extraction_method, status)
         VALUES (?1, ?2, ?3, 'TEXT', ?4, ?5, ?6, ?7, 'test-1', 'EXACT', 'NATIVE', 'ACTIVE')",
        params![
            doc.chunk_id.to_string(),
            doc.version_id.to_string(),
            node.to_string(),
            doc.body,
            doc.title,
            doc.body.split_whitespace().count() as i64,
            ContentHash::of(doc.body.as_bytes()).to_hex(),
        ],
    )
    .map_err(|e| map_sqlite(e, "test: insert chunk"))?;
    Ok(())
}

// ----------------------------------------------------------- the D3 property

/// **The load-bearing D3 property.**
///
/// FTS5 was chosen over Tantivy because the index lives in the same database as
/// the canonical row, so the two commit together and there is no window where
/// they disagree. That is only true if the ingest path writes them in one
/// transaction — so: roll a transaction back with both in it and neither may
/// survive. If this test is ever "fixed" by relaxing it, D3's entire argument
/// has been thrown away and Tantivy should be reconsidered on its merits.
#[test]
fn index_and_canonical_write_share_one_transaction() {
    let mut f = Fixture::with_long_batches();
    let index = f.index();
    let (file, version) = f.add_file("a.txt");
    let doc = f.doc(file, version, "a.txt", "", "the refresh token rotates");
    let chunk_id = doc.chunk_id;

    // Positive control: committed together, both are there.
    let survivor = f.doc(file, version, "a.txt", "", "committed together");
    f.add_indexed_chunk(&survivor);
    assert_eq!(f.chunk_rows(), 1);
    assert_eq!(f.doc_rows(), 1);

    // Now the same pair of writes, in one batch, killed before it commits.
    // `ran` proves the batch really did both inserts before it was killed — a
    // rollback test that never ran the writes would pass for the wrong reason.
    let ran = std::sync::Arc::new(AtomicBool::new(false));
    let ran_in_writer = ran.clone();
    let d = doc.clone();
    let pending = f
        .store()
        .writer()
        .send(move |c| {
            insert_chunk_row(c, &d)?;
            fts5::upsert_docs(c, std::slice::from_ref(&d))?;
            // Both rows are visible inside the transaction, right now.
            let n: i64 = c
                .query_row(
                    "SELECT count(*) FROM chunks c JOIN text_index_docs d
                       ON d.chunk_id = c.chunk_id WHERE c.chunk_id = ?1",
                    [d.chunk_id.to_string()],
                    |r| r.get(0),
                )
                .map_err(|e| map_sqlite(e, "test: read inside the transaction"))?;
            assert_eq!(n, 1, "canonical and derived row must both exist pre-commit");
            ran_in_writer.store(true, Ordering::SeqCst);
            Ok(())
        })
        .expect("send");
    f.abort_writer();
    assert!(
        ran.load(Ordering::SeqCst),
        "the batch must have executed both writes before it was killed"
    );
    assert!(
        pending.wait().is_err(),
        "an aborted batch reports failure rather than silently succeeding"
    );
    f.reopen();

    let canonical: i64 = f.read(|c| {
        c.query_row(
            "SELECT count(*) FROM chunks WHERE chunk_id = ?1",
            [chunk_id.to_string()],
            |r| r.get(0),
        )
        .expect("count")
    });
    let derived: i64 = f.read(|c| {
        c.query_row(
            "SELECT count(*) FROM text_index_docs WHERE chunk_id = ?1",
            [chunk_id.to_string()],
            |r| r.get(0),
        )
        .expect("count")
    });
    assert_eq!(canonical, 0, "the canonical chunk must not survive");
    assert_eq!(derived, 0, "the index document must not survive either");
    assert_eq!(
        f.chunk_rows(),
        f.doc_rows(),
        "canonical and derived must agree after a rollback, not merely both be small"
    );
    assert_eq!(f.fts_rows(), 1, "only the committed pair is left");

    // The index still answers for what did commit.
    let hits = index
        .search(&TextQuery::new("committed"))
        .expect("search after rollback");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].chunk_id, survivor.chunk_id);
}

/// Invariant #11's other half: derived state is disposable because it can
/// always be re-derived. Blow the index away entirely and rebuild from
/// canonical rows; the answers must be the same ones.
#[test]
fn derived_index_is_rebuildable_from_canonical() {
    let f = Fixture::new();
    let index = f.index();
    let (file, version) = f.add_file("auth.rs");
    let docs = vec![
        f.doc(
            file,
            version,
            "auth.rs",
            "fn refresh_token › impl TokenService",
            "the refresh token rotates on each use",
        ),
        f.doc(
            file,
            version,
            "auth.rs",
            "fn decode › impl TokenService",
            "decode the claims before trusting them",
        ),
        f.doc(file, version, "auth.rs", "", "unrelated prose about cats"),
    ];
    for d in &docs {
        f.add_indexed_chunk(d);
    }

    let queries = ["refresh token", "claims", "cats", "token"];
    let before: Vec<Vec<(ChunkId, String)>> = queries
        .iter()
        .map(|q| {
            index
                .search(&TextQuery::new(*q))
                .expect("search")
                .into_iter()
                .map(|h| (h.chunk_id, h.snippet.text))
                .collect()
        })
        .collect();
    assert!(before.iter().all(|r| !r.is_empty()), "fixture must match");
    assert_eq!(index.doc_count().expect("count"), 3);

    // Delete the whole FTS table's contents, and the doc table with it.
    f.commit(fts5::clear);
    assert_eq!(f.fts_rows(), 0, "the index really is gone");
    assert_eq!(index.doc_count().expect("count"), 0);
    assert!(index
        .search(&TextQuery::new("refresh token"))
        .expect("search")
        .is_empty());
    assert_eq!(f.chunk_rows(), 3, "canonical state is untouched");

    // Rebuild from canonical state alone.
    f.commit(|c| {
        let src = StoreChunkSource::new(c);
        fts5::rebuild(c, &src)
    });

    assert_eq!(index.doc_count().expect("count"), 3);
    for (q, want) in queries.iter().zip(&before) {
        let got: Vec<(ChunkId, String)> = index
            .search(&TextQuery::new(*q))
            .expect("search")
            .into_iter()
            .map(|h| (h.chunk_id, h.snippet.text))
            .collect();
        assert_eq!(&got, want, "rebuilt index answers {q:?} differently");
    }

    // And the spans came back through the IR node, not as a shrug.
    let hit = &index.search(&TextQuery::new("claims")).expect("search")[0];
    assert!(
        hit.span.is_precise(),
        "a rebuilt hit must keep its provenance, got {:?}",
        hit.span
    );
}

/// Query text is content, and content is untrusted (invariant #12). Nothing a
/// user can type may become FTS5 syntax, SQL, or a panic.
#[test]
fn query_syntax_cannot_be_injected() {
    let f = Fixture::new();
    let index = f.index();
    let (file, version) = f.add_file("a.txt");
    for body in ["alpha beta gamma", "a b c d", "OR NOT NEAR"] {
        let d = f.doc(file, version, "a.txt", "", body);
        f.add_indexed_chunk(&d);
    }

    let hostile = [
        r#"" OR "a" NEAR/5 "b"#,
        "*",
        "NOT",
        "a OR b",
        "a AND NOT b",
        r#""unbalanced"#,
        r#"""""""#,
        "NEAR(alpha beta, 5)",
        "{path title} : (x)",
        "body : alpha",
        "^alpha",
        "alpha*",
        "-alpha",
        r#"x" ) ; DROP TABLE files; --"#,
        r#"'); DELETE FROM chunks; --"#,
        "\u{0}\u{1}\u{2}",
        "\\",
        "((((((((((",
        // 10 KB of one repeated word: one clipped term, must simply work.
        &"averylongtoken".repeat(750),
        // 10 KB of distinct words: over the term limit, must be a clean error.
        &(0..2000).map(|i| format!("w{i} ")).collect::<String>(),
    ];

    for q in hostile {
        for mode in [MatchMode::Terms, MatchMode::Phrase, MatchMode::Prefix] {
            let query = TextQuery::new(q).mode(mode);
            match index.search(&query) {
                Ok(_) => {}
                Err(e) => assert_eq!(
                    e.code(),
                    Code::CfgInvalid,
                    "{q:?} in {mode:?} produced {e} — a rejection must be about the input, \
                     never an engine or SQL failure"
                ),
            }
        }
    }

    // Nothing got dropped, deleted or corrupted along the way.
    assert_eq!(f.chunk_rows(), 3);
    assert_eq!(f.doc_rows(), 3);
    let files: i64 = f.read(|c| {
        c.query_row("SELECT count(*) FROM files", [], |r| r.get(0))
            .expect("files table still exists")
    });
    assert_eq!(files, 1);
}

/// BM25 must prefer the tight, exact document over the one that merely contains
/// the words somewhere. If this stops holding, the lexical branch is returning
/// candidates in an order that fusion (§113.2) will faithfully preserve.
#[test]
fn bm25_ranks_exact_above_partial() {
    let f = Fixture::new();
    let index = f.index();
    let (file, version) = f.add_file("a.txt");

    let exact = f.doc(file, version, "exact.md", "", "refresh token");
    let filler: String = (0..400).map(|i| format!("word{i} ")).collect();
    let partial = f.doc(
        file,
        version,
        "partial.md",
        "",
        &format!("refresh {filler} token"),
    );
    f.add_indexed_chunk(&partial);
    f.add_indexed_chunk(&exact);

    let hits = index
        .search(&TextQuery::new("refresh token"))
        .expect("search");
    assert_eq!(hits.len(), 2, "both documents contain both terms");
    assert_eq!(
        hits[0].chunk_id, exact.chunk_id,
        "the exact, dense document must rank first"
    );
    assert!(
        hits[0].score > hits[1].score,
        "scores must order the same way as the results: {} vs {}",
        hits[0].score,
        hits[1].score
    );

    // And a phrase query excludes the partial one entirely.
    let phrase = index
        .search(&TextQuery::new("refresh token").phrase())
        .expect("search");
    assert_eq!(phrase.len(), 1);
    assert_eq!(phrase[0].chunk_id, exact.chunk_id);
}

/// Derived data must not outlive what it derives from — by either route: an
/// explicit `delete`, or a canonical delete cascading underneath us.
#[test]
fn deleted_chunks_leave_no_orphan_docs() {
    let f = Fixture::new();
    let index = f.index();
    let (file, version) = f.add_file("a.txt");
    let a = f.doc(file, version, "a.txt", "", "alpha needle");
    let b = f.doc(file, version, "a.txt", "", "beta needle");
    let c = f.doc(file, version, "a.txt", "", "gamma needle");
    for d in [&a, &b, &c] {
        f.add_indexed_chunk(d);
    }
    assert_eq!(f.fts_rows(), 3);

    // Route 1: the port's own delete.
    index.delete(&[a.chunk_id]).expect("delete");
    assert_eq!(f.doc_rows(), 2);
    assert_eq!(f.fts_rows(), 2, "the FTS5 row must go with the doc row");
    let ids: Vec<ChunkId> = index
        .search(&TextQuery::new("needle"))
        .expect("search")
        .into_iter()
        .map(|h| h.chunk_id)
        .collect();
    assert!(!ids.contains(&a.chunk_id));

    // Route 2: the canonical chunk is deleted directly. No application code
    // runs; the FK cascade and the trigger have to do it.
    let gone = b.chunk_id;
    f.commit(move |conn| {
        conn.execute("DELETE FROM chunks WHERE chunk_id = ?1", [gone.to_string()])
            .map_err(|e| map_sqlite(e, "test: delete chunk"))?;
        Ok(())
    });
    assert_eq!(f.doc_rows(), 1, "FK cascade must reach the index doc");
    assert_eq!(f.fts_rows(), 1, "the trigger must reach the FTS5 row");

    // Route 3: the whole file version goes, cascading through chunks.
    let v = version;
    f.commit(move |conn| {
        conn.execute(
            "DELETE FROM file_versions WHERE version_id = ?1",
            [v.to_string()],
        )
        .map_err(|e| map_sqlite(e, "test: delete version"))?;
        Ok(())
    });
    assert_eq!(f.chunk_rows(), 0);
    assert_eq!(f.doc_rows(), 0);
    assert_eq!(f.fts_rows(), 0, "no orphan documents anywhere");
    assert!(index
        .search(&TextQuery::new("needle"))
        .expect("search")
        .is_empty());
}

/// The snippet's match offsets have to land on the actual match — a highlight
/// that is off by a word is worse than no highlight, because it is believed.
#[test]
fn snippet_offsets_land_on_the_match() {
    let f = Fixture::new();
    let index = f.index();
    let (file, version) = f.add_file("a.txt");
    let d = f.doc(
        file,
        version,
        "a.txt",
        "",
        "alpha beta refresh gamma delta epsilon",
    );
    f.add_indexed_chunk(&d);

    let hits = index.search(&TextQuery::new("refresh")).expect("search");
    assert_eq!(hits.len(), 1);
    let s = &hits[0].snippet;
    assert!(!s.matches.is_empty(), "a hit must carry match offsets");
    for m in &s.matches {
        assert!(m.end <= s.text.len(), "offsets must be inside the snippet");
        assert!(s.text.is_char_boundary(m.start) && s.text.is_char_boundary(m.end));
    }
    assert_eq!(
        s.matched_text(),
        vec!["refresh"],
        "the offsets must select the matched word, not its neighbours"
    );
    assert!(
        !s.text.contains('\u{1}') && !s.text.contains('\u{2}'),
        "no marker residue may reach a renderer"
    );

    // Two terms, two ranges, in order and non-overlapping.
    let hits = index
        .search(&TextQuery::new("alpha epsilon"))
        .expect("search");
    let s = &hits[0].snippet;
    assert_eq!(s.matched_text(), vec!["alpha", "epsilon"]);
    assert!(s.matches.windows(2).all(|w| w[0].end <= w[1].start));

    // Non-ASCII: offsets are byte offsets and must still be char boundaries.
    let (file2, version2) = f.add_file("b.txt");
    let d2 = f.doc(
        file2,
        version2,
        "b.txt",
        "",
        "un café très chaud aujourd'hui",
    );
    f.add_indexed_chunk(&d2);
    let hits = index.search(&TextQuery::new("chaud")).expect("search");
    let s = &hits[0].snippet;
    assert_eq!(s.matched_text(), vec!["chaud"]);
    assert_eq!(s.highlighted("[", "]"), "un café très [chaud] aujourd'hui");
}

/// CAP-005: literal search is available *independently of index freshness*.
/// The index here is deliberately empty; the scan still finds the string.
#[test]
fn literal_search_works_with_a_stale_index() {
    let f = Fixture::new();
    let index = f.index();
    let (file, version) = f.add_file("stale.rs");

    let body = "const FOO_BAR: &str = \"sentinel\";\n";
    let path = f.dir.path().join("stale.rs");
    std::fs::write(&path, body).expect("write file");
    // Canonical rows exist; the index was never told about them.
    let doc = f.doc(file, version, "stale.rs", "", body);
    f.add_chunk(&doc);

    assert_eq!(index.doc_count().expect("count"), 0, "index is stale");
    assert!(
        index
            .search(&TextQuery::new("sentinel"))
            .expect("search")
            .is_empty(),
        "the index cannot find it, which is the premise of this test"
    );

    let out = literal_search(
        &[LiteralTarget::new(file, path, TierState::Resident)],
        &LiteralQuery::new("sentinel"),
        &AtomicBool::new(false),
    )
    .expect("literal scan");
    assert_eq!(
        out.hits.len(),
        1,
        "the literal scan does not consult the index"
    );
    assert_eq!(out.hits[0].line, 1);
    assert!(out.stopped.is_complete());

    // And it finds what the tokenizer deliberately splits: `FOO_BAR` is
    // `foo` + `bar` in FTS5, exact here.
    let out = literal_search(
        &[LiteralTarget::new(
            file,
            f.dir.path().join("stale.rs"),
            TierState::Resident,
        )],
        &LiteralQuery::new("FOO_BAR"),
        &AtomicBool::new(false),
    )
    .expect("literal scan");
    assert_eq!(out.hits[0].snippet.matched_text(), vec!["FOO_BAR"]);
}

/// **Invariant #5.** Reading a cloud placeholder silently downloads it. The
/// literal scan refuses, counts the refusal, and never opens the file.
#[test]
fn literal_search_refuses_non_resident_files() {
    let f = Fixture::new();
    let dir = f.dir.path();
    let resident = dir.join("resident.txt");
    std::fs::write(&resident, "needle in the resident file\n").expect("write");

    let mut targets = vec![LiteralTarget::new(
        FileId::new(),
        resident,
        TierState::Resident,
    )];
    // Real files on disk with the needle in them, marked as anything but
    // Resident. If the scan reads them, it finds them — so a clean result is
    // proof it did not.
    for (i, tier) in [
        TierState::Placeholder,
        TierState::Hydrating,
        TierState::Unavailable,
    ]
    .into_iter()
    .enumerate()
    {
        let p = dir.join(format!("cloud{i}.txt"));
        std::fs::write(&p, "needle in a file that must not be read\n").expect("write");
        targets.push(LiteralTarget::new(FileId::new(), p, tier));
    }

    let out = literal_search(
        &targets,
        &LiteralQuery::new("needle"),
        &AtomicBool::new(false),
    )
    .expect("literal scan");

    assert_eq!(out.hits.len(), 1, "only the resident file may be read");
    assert!(out.hits[0].path.ends_with("resident.txt"));
    assert_eq!(out.files_scanned, 1);
    assert_eq!(
        out.files_skipped_not_resident, 3,
        "every non-Resident tier must be refused, not just Placeholder"
    );
    assert!(
        out.has_gaps(),
        "a scan that skipped files must say so — that is UX §4's zero-results diagnosis"
    );
}

/// A cancel token must stop the scan within a bounded time, and the result must
/// say it was cancelled rather than pass a partial answer off as complete.
#[test]
fn literal_search_honours_cancellation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut targets = Vec::new();
    for i in 0..4000 {
        let p = dir.path().join(format!("f{i}.txt"));
        std::fs::write(&p, format!("line one\nneedle {i}\nline three\n")).expect("write");
        targets.push(LiteralTarget::new(FileId::new(), p, TierState::Resident));
    }
    let q = LiteralQuery::new("needle")
        .max_total_matches(usize::MAX)
        .time_budget(Duration::from_secs(600));

    // Already cancelled: nothing is read at all.
    let cancel = AtomicBool::new(true);
    let started = Instant::now();
    let out = literal_search(&targets, &q, &cancel).expect("scan");
    assert_eq!(out.stopped, StopReason::Cancelled);
    assert_eq!(out.files_scanned, 0);
    assert!(out.hits.is_empty());
    assert!(started.elapsed() < Duration::from_secs(1));

    // Cancelled while running: it must notice and stop early.
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let started = Instant::now();
    let worker = std::thread::spawn(move || literal_search(&targets, &q, &flag));
    cancel.store(true, Ordering::SeqCst);
    let out = worker.join().expect("join").expect("scan");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "cancellation must be bounded, took {:?}",
        started.elapsed()
    );
    assert_eq!(out.stopped, StopReason::Cancelled);
    assert!(
        out.files_scanned < 4000,
        "a cancelled scan must stop early, not merely report a flag"
    );
    assert!(!out.stopped.is_complete());
}

/// A query with nothing to search for is the user's mistake, and gets a message
/// that says what to do — never an FTS5 syntax error and never a panic.
#[test]
fn empty_and_whitespace_queries_are_clean_errors() {
    let f = Fixture::new();
    let index = f.index();
    let (file, version) = f.add_file("a.txt");
    let d = f.doc(file, version, "a.txt", "", "something to find");
    f.add_indexed_chunk(&d);

    for q in [
        "",
        " ",
        "   \t\n  ",
        "\u{a0}",
        "***",
        "!!!",
        "...",
        "-",
        "\"\"",
    ] {
        for mode in [MatchMode::Terms, MatchMode::Phrase, MatchMode::Prefix] {
            let err = index
                .search(&TextQuery::new(q).mode(mode))
                .expect_err(&format!("{q:?} in {mode:?} must be refused"));
            assert_eq!(err.code(), Code::CfgInvalid, "{q:?}: {err}");
            assert!(
                err.message().len() > 30,
                "SUP-001: {q:?} got a label, not a cause and an action: {err}"
            );
            assert!(
                !err.retryable(),
                "retrying the same empty query cannot help"
            );
        }
    }

    // The index is untouched and still works.
    assert_eq!(index.doc_count().expect("count"), 1);
    assert_eq!(
        index.search(&TextQuery::new("find")).expect("search").len(),
        1
    );
}

// ------------------------------------------------------------------ latency

/// Deterministic xorshift. A benchmark whose corpus changes between runs is a
/// benchmark whose numbers cannot be compared between runs.
fn rng(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

/// Search latency on a synthetic corpus the size M0 measured (~34,459 files).
///
/// The vocabulary is Zipf-shaped rather than uniform, because latency here is a
/// function of **document frequency**, not corpus size: BM25 scores every
/// posting it reads. A uniform vocabulary makes every term a worst case and
/// reports a number that no real query would ever produce. The planted terms
/// below have known document frequencies so the curve is readable instead of a
/// single unattributable figure.
///
/// Ignored by default: it builds a 34k-document index. Run it with
/// `cargo test -p marrow-index --release -- --ignored --nocapture`.
#[test]
#[ignore = "benchmark: builds a 34k-document index"]
fn search_latency_on_a_synthetic_corpus() {
    const DOCS: usize = 34_459;
    const VOCAB: usize = 4_000;
    const WORDS_PER_DOC: usize = 120;
    let f = Fixture::new();
    let index = f.index();
    let (file, version) = f.add_file("corpus.txt");

    let built = Instant::now();
    let mut seed = 0x5eed_1234_5678_9abcu64;
    let mut batch = Vec::with_capacity(500);
    for i in 0..DOCS {
        let mut body = String::with_capacity(WORDS_PER_DOC * 8);
        for _ in 0..WORDS_PER_DOC {
            // Zipf-ish: square the uniform draw so low ranks dominate.
            let u = (rng(&mut seed) % 10_000) as f64 / 10_000.0;
            let rank = (u * u * VOCAB as f64) as usize;
            body.push_str(&format!("w{rank} "));
        }
        // Planted terms with known document frequency.
        body.push_str("everywhereterm ");
        if i % 10 == 0 {
            body.push_str("tenpercentterm ");
        }
        if i % 1000 == 0 {
            body.push_str("rareterm ");
        }
        if i == 0 {
            body.push_str("uniqueterm ");
        }
        let d = f.doc(
            file,
            version,
            &format!("dir{}/file{i}.rs", i % 97),
            &format!("fn f{i} › impl Mod{}", i % 31),
            &body,
        );
        batch.push(d);
        if batch.len() == 500 {
            let docs = std::mem::take(&mut batch);
            f.commit(move |c| {
                for d in &docs {
                    insert_chunk_row(c, d)?;
                }
                fts5::upsert_docs(c, &docs)
            });
        }
    }
    if !batch.is_empty() {
        let docs = std::mem::take(&mut batch);
        f.commit(move |c| {
            for d in &docs {
                insert_chunk_row(c, d)?;
            }
            fts5::upsert_docs(c, &docs)
        });
    }
    let build = built.elapsed();
    assert_eq!(index.doc_count().expect("count"), DOCS as u64);

    let db_bytes = std::fs::metadata(f.db()).map(|m| m.len()).unwrap_or(0);
    println!(
        "\n{DOCS} docs ({WORDS_PER_DOC} words each) built in {build:?} ({:.0} docs/s), \
         database {:.1} MB",
        DOCS as f64 / build.as_secs_f64(),
        db_bytes as f64 / 1e6
    );

    let cases: Vec<(&str, TextQuery)> = vec![
        ("df=1 (unique)", TextQuery::new("uniqueterm")),
        ("df=35 (0.1%)", TextQuery::new("rareterm")),
        ("df=3446 (10%)", TextQuery::new("tenpercentterm")),
        ("df=34459 (100%)", TextQuery::new("everywhereterm")),
        (
            "two terms, both rare",
            TextQuery::new("rareterm uniqueterm"),
        ),
        ("two terms, one common", TextQuery::new("tenpercentterm w3")),
        ("phrase", TextQuery::new("rareterm everywhereterm").phrase()),
        ("prefix (as-you-type)", TextQuery::new("rarete").prefix()),
        (
            "title field only",
            TextQuery::new("Mod7").in_fields([marrow_index::TextField::Title]),
        ),
        (
            "df=10% + extension filter",
            TextQuery::new("tenpercentterm").with_filters(marrow_index::Filters {
                extensions: vec!["rs".into()],
                ..Default::default()
            }),
        ),
        (
            "df=10% + path glob",
            TextQuery::new("tenpercentterm").with_filters(marrow_index::Filters {
                path_glob: Some("*/dir7/*".into()),
                ..Default::default()
            }),
        ),
    ];
    for (name, q) in &cases {
        // Warm, then take the median of 40 runs: the p50 is what §116's "under
        // 50 ms to first result" budget is about, not a lucky best case.
        for _ in 0..3 {
            index.search(q).expect("search");
        }
        let mut times = Vec::new();
        let mut hits = 0;
        for _ in 0..40 {
            let t = Instant::now();
            hits = index.search(q).expect("search").len();
            times.push(t.elapsed());
        }
        times.sort();
        println!(
            "  {name:<24} p50 {:>9.3?}   p95 {:>9.3?}   max {:>9.3?}   ({hits} hits)",
            times[times.len() / 2],
            times[times.len() * 95 / 100],
            times[times.len() - 1],
        );
    }

    let t = Instant::now();
    f.commit(|c| {
        let src = StoreChunkSource::new(c);
        fts5::rebuild(c, &src)
    });
    println!("  {:<24} {:?}\n", "full rebuild", t.elapsed());
}

// ------------------------------------------------------- supporting behaviour
//
// Not on the named-invariant list, but each one holds up something the named
// tests take for granted.

/// The migration installs once, records its version where the store's runner
/// records versions, and re-opening is a no-op rather than a second attempt.
#[test]
fn the_migration_is_idempotent_and_records_its_version() {
    let f = Fixture::new();
    let _first = f.index();
    let version: String = f.read(|c| {
        c.query_row(
            "SELECT value FROM schema_meta WHERE key = ?1",
            [fts5::VERSION_META_KEY],
            |r| r.get(0),
        )
        .expect("version recorded")
    });
    assert_eq!(version, fts5::TEXT_INDEX_VERSION.to_string());
    assert_eq!(
        fts5::MIGRATION.version,
        marrow_store::migrate::target_version() + 1,
        "the text index migration must be the next number in the store's chain"
    );

    // Opening again, and calling the migration directly again, both no-op.
    let second = f.index();
    f.commit(fts5::ensure_installed);
    assert_eq!(second.doc_count().expect("count"), 0);
    let tables: i64 = f.read(|c| {
        c.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name IN ('text_index', 'text_index_docs')",
            [],
            |r| r.get(0),
        )
        .expect("count tables")
    });
    assert_eq!(tables, 2);
}

/// Every filter narrows on document metadata, not on the FTS expression.
#[test]
fn filters_narrow_on_document_metadata() {
    let f = Fixture::new();
    let index = f.index();
    let (file, version) = f.add_file("a.txt");

    let mut rs = f.doc(file, version, "src/token.rs", "", "needle in rust");
    rs.modified = Timestamp::from_millis(1_000);
    let mut md = f.doc(file, version, "docs/token.md", "", "needle in markdown");
    md.modified = Timestamp::from_millis(5_000);
    let mut other = f.doc(file, version, "other/token.md", "", "needle elsewhere");
    other.modified = Timestamp::from_millis(9_000);
    for d in [&rs, &md, &other] {
        f.add_indexed_chunk(d);
    }

    let ids = |q: &TextQuery| -> Vec<ChunkId> {
        let mut v: Vec<ChunkId> = index
            .search(q)
            .expect("search")
            .into_iter()
            .map(|h| h.chunk_id)
            .collect();
        v.sort();
        v
    };

    let by_ext = ids(
        &TextQuery::new("needle").with_filters(marrow_index::Filters {
            extensions: vec![".RS".into()], // dot and case must not matter
            ..Default::default()
        }),
    );
    assert_eq!(by_ext, vec![rs.chunk_id]);

    let by_glob = ids(
        &TextQuery::new("needle").with_filters(marrow_index::Filters {
            path_glob: Some("*/docs/*".into()),
            ..Default::default()
        }),
    );
    assert_eq!(by_glob, vec![md.chunk_id]);

    let by_time = ids(
        &TextQuery::new("needle").with_filters(marrow_index::Filters {
            modified_after: Some(Timestamp::from_millis(2_000)),
            modified_before: Some(Timestamp::from_millis(6_000)),
            ..Default::default()
        }),
    );
    assert_eq!(by_time, vec![md.chunk_id]);

    let by_ws = ids(
        &TextQuery::new("needle").with_filters(marrow_index::Filters {
            workspace: Some(WorkspaceId::new()),
            ..Default::default()
        }),
    );
    assert!(by_ws.is_empty(), "a foreign workspace matches nothing");

    let mut all = vec![rs.chunk_id, md.chunk_id, other.chunk_id];
    all.sort();
    assert_eq!(
        ids(&TextQuery::new("needle")),
        all,
        "unfiltered sees them all"
    );

    // A glob full of metacharacters is data, not syntax.
    let hostile = ids(
        &TextQuery::new("needle").with_filters(marrow_index::Filters {
            path_glob: Some("*[!]' OR 1=1 --*".into()),
            ..Default::default()
        }),
    );
    assert!(hostile.is_empty());
    assert_eq!(
        index.doc_count().expect("count"),
        3,
        "and nothing was harmed"
    );
}

/// Field scoping keeps a term from matching in a field the caller excluded, and
/// the weights that scored it are a query parameter (§113.4).
#[test]
fn field_scoping_and_weights_are_query_parameters() {
    use marrow_index::{FieldWeights, TextField};
    let f = Fixture::new();
    let index = f.index();
    let (file, version) = f.add_file("a.txt");

    let in_title = f.doc(
        file,
        version,
        "x/plain.txt",
        "sentinel heading",
        "ordinary prose",
    );
    let in_body = f.doc(
        file,
        version,
        "y/plain.txt",
        "heading",
        "prose mentioning sentinel",
    );
    let in_path = f.doc(file, version, "z/sentinel.txt", "heading", "ordinary prose");
    for d in [&in_title, &in_body, &in_path] {
        f.add_indexed_chunk(d);
    }

    let scoped = |fields: Vec<TextField>| -> Vec<ChunkId> {
        index
            .search(&TextQuery::new("sentinel").in_fields(fields))
            .expect("search")
            .into_iter()
            .map(|h| h.chunk_id)
            .collect()
    };
    assert_eq!(scoped(vec![TextField::Title]), vec![in_title.chunk_id]);
    assert_eq!(scoped(vec![TextField::Body]), vec![in_body.chunk_id]);
    assert_eq!(scoped(vec![TextField::Path]), vec![in_path.chunk_id]);
    assert_eq!(scoped(vec![TextField::Title, TextField::Path]).len(), 2);

    // Weights, not code, decide which field wins.
    let ranked = |w: FieldWeights| -> ChunkId {
        index
            .search(&TextQuery::new("sentinel").with_weights(w))
            .expect("search")[0]
            .chunk_id
    };
    assert_eq!(
        ranked(FieldWeights {
            path: 1.0,
            title: 50.0,
            body: 1.0
        }),
        in_title.chunk_id
    );
    assert_eq!(
        ranked(FieldWeights {
            path: 50.0,
            title: 1.0,
            body: 1.0
        }),
        in_path.chunk_id
    );
}

/// Prefix mode is the as-you-type path: the last term matches as a prefix, and
/// the earlier ones do not.
#[test]
fn prefix_mode_matches_only_the_last_term_as_a_prefix() {
    let f = Fixture::new();
    let index = f.index();
    let (file, version) = f.add_file("a.txt");
    let d = f.doc(file, version, "a.txt", "", "refreshing the tokenizer");
    f.add_indexed_chunk(&d);

    let hit = |q: TextQuery| index.search(&q).expect("search").len();
    assert_eq!(hit(TextQuery::new("refre").prefix()), 1);
    assert_eq!(hit(TextQuery::new("refreshing tokeni").prefix()), 1);
    assert_eq!(
        hit(TextQuery::new("refre tokenizer").prefix()),
        0,
        "only the last term is a prefix; `refre` must be an exact term"
    );
    assert_eq!(hit(TextQuery::new("refre").mode(MatchMode::Terms)), 0);
}

/// Re-indexing a chunk replaces its document rather than adding a second one —
/// including after a rename, where the path changes and the chunk id does not.
#[test]
fn upsert_replaces_and_survives_a_rename() {
    let f = Fixture::new();
    let index = f.index();
    let (file, version) = f.add_file("a.txt");
    let mut d = f.doc(file, version, "old/name.txt", "", "the body text");
    f.add_indexed_chunk(&d);
    assert_eq!(f.doc_rows(), 1);

    // Same chunk, new path and new body: one row, updated.
    d.path = f.dir.path().join("new/name.md").display().to_string();
    d.body = "a completely different body".to_string();
    index.upsert(std::slice::from_ref(&d)).expect("upsert");
    assert_eq!(f.doc_rows(), 1, "invariant #2: the chunk id is the key");
    assert_eq!(f.fts_rows(), 1, "no orphan FTS5 row left behind");

    assert!(index
        .search(&TextQuery::new("body text"))
        .expect("search")
        .is_empty());
    let hits = index.search(&TextQuery::new("different")).expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].chunk_id, d.chunk_id);
    assert!(hits[0].path.ends_with("new/name.md"));

    // The extension moved with the path, so the filter follows it.
    let by_ext = index
        .search(
            &TextQuery::new("different").with_filters(marrow_index::Filters {
                extensions: vec!["md".into()],
                ..Default::default()
            }),
        )
        .expect("search");
    assert_eq!(by_ext.len(), 1);
}

/// A hit carries the facts the renderer and the evidence layer need: exact
/// provenance, and the origin that bars self-written content from supporting a
/// claim (invariant #13).
#[test]
fn hits_carry_provenance_and_origin_unchanged() {
    let f = Fixture::new();
    let index = f.index();
    let (file, version) = f.add_file("a.txt");

    let mut agent = f.doc(
        file,
        version,
        "notes/summary.md",
        "§ Summary",
        "the agent wrote this",
    );
    agent.origin = Origin::SelfWritten;
    agent.provenance = ProvenanceClass::Degraded;
    agent.span = SourceSpan::Lines { start: 4, end: 9 };
    f.add_indexed_chunk(&agent);

    let hits = index
        .search(&TextQuery::new("agent wrote"))
        .expect("search");
    assert_eq!(
        hits.len(),
        1,
        "SELF content is searchable — that is the point"
    );
    let h = &hits[0];
    assert_eq!(h.origin, Origin::SelfWritten);
    assert!(
        !h.origin.can_support_a_claim(),
        "invariant #13: findable, never citable"
    );
    assert_eq!(h.provenance, ProvenanceClass::Degraded);
    assert_eq!(h.span, SourceSpan::Lines { start: 4, end: 9 });
    assert_eq!(h.title, "§ Summary", "the breadcrumb reaches the renderer");
    assert_eq!(h.file_id, file);
    assert_eq!(h.version_id, version);
    assert_eq!(h.workspace_id, f.workspace);
    assert_eq!(h.modified, agent.modified);
}

/// The port is a port: usable behind `dyn`, and shareable across threads.
#[test]
fn the_port_works_as_a_trait_object_across_threads() {
    let f = Fixture::new();
    let index: std::sync::Arc<dyn TextIndex> = std::sync::Arc::new(f.index());
    let (file, version) = f.add_file("a.txt");
    let d = f.doc(file, version, "a.txt", "", "shared needle");
    f.add_indexed_chunk(&d);
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let index = index.clone();
            std::thread::spawn(move || {
                index
                    .search(&TextQuery::new("needle"))
                    .expect("search")
                    .len()
            })
        })
        .collect();
    for h in handles {
        assert_eq!(h.join().expect("join"), 1);
    }
    assert_eq!(index.doc_count().expect("count"), 1);
}

/// Diacritics fold both ways (`remove_diacritics 2`), and `_` is a separator so
/// either half of an identifier finds it.
#[test]
fn the_tokenizer_folds_diacritics_and_splits_identifiers() {
    let f = Fixture::new();
    let index = f.index();
    let (file, version) = f.add_file("a.txt");
    let d = f.doc(
        file,
        version,
        "a.txt",
        "",
        "résumé of the refresh_token rotation, Việt Nam edition",
    );
    f.add_indexed_chunk(&d);

    let n = |q: &str| index.search(&TextQuery::new(q)).expect("search").len();
    assert_eq!(n("resume"), 1, "unaccented query finds accented text");
    assert_eq!(n("résumé"), 1);
    assert_eq!(n("refresh"), 1, "`_` is a separator");
    assert_eq!(n("token"), 1);
    assert_eq!(
        n("refresh_token"),
        1,
        "and the whole identifier still works"
    );
    assert_eq!(
        n("Viet Nam"),
        1,
        "remove_diacritics 2 folds beyond U+0800, which version 1 does not"
    );
}

/// An empty index and an index that was never created both answer without
/// blowing up — and the missing one says what to do about it.
#[test]
fn a_missing_index_asks_for_a_rebuild_rather_than_failing_obscurely() {
    let f = Fixture::new();
    let index = f.index();
    assert_eq!(index.doc_count().expect("count"), 0);
    assert!(index
        .search(&TextQuery::new("anything"))
        .expect("search")
        .is_empty());
    assert!(
        index.delete(&[ChunkId::new()]).is_ok(),
        "unknown ids are not an error"
    );
    assert!(index.upsert(&[]).is_ok());

    // Now the tables are gone, as they would be after a botched restore.
    f.commit(|c| {
        c.execute_batch("DROP TABLE text_index; DROP TABLE text_index_docs;")
            .map_err(|e| map_sqlite(e, "test: drop index tables"))
    });
    let err = index.search(&TextQuery::new("anything")).unwrap_err();
    assert_eq!(err.code(), Code::IdxRebuildRequired);
    assert!(err.message().contains("rebuil"), "{err}");
    let err = index.doc_count().unwrap_err();
    assert_eq!(err.code(), Code::IdxRebuildRequired);
}
