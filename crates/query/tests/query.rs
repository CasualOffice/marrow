//! Read-path integration tests.
//!
//! Every fixture here is a **file-backed** store on a tempdir, never
//! `Store::open_in_memory`. An in-memory SQLite database uses shared-cache,
//! which locks at table granularity instead of using WAL's MVCC snapshots: a
//! long-lived reader — exactly what [`marrow_query::file_intelligence`] opens —
//! blocks the writer there and does not on disk. Testing against the in-memory
//! variant would therefore test a concurrency model the product does not ship.

use marrow_core::{
    ChunkId, Code, ContentHash, FileId, FileStatus, Origin, ProvenanceClass, RootId, SourceSpan,
    Timestamp, WorkspaceId,
};
use marrow_index::{Fts5Index, TextDoc, TextIndex};
use marrow_query::{
    explain, file_intelligence, search, FileRef, SearchFilters, SearchRequest,
    SELF_WRITTEN_MULTIPLIER,
};
use marrow_store::read::{NewChunk, NewFile, NewRoot, NewVersion, NewWorkspace};
use marrow_store::Store;
use tempfile::TempDir;

// ------------------------------------------------------------------- fixture

/// A store, an index, and a way to put a searchable file in both.
struct Corpus {
    // Declaration order is drop order: the index's reader and the store's
    // writer thread must both be gone before the tempdir is removed.
    index: Fts5Index,
    store: Store,
    _dir: TempDir,
}

/// One file to index. A struct rather than seven positional arguments, because
/// `add(ws, root, "a.md", "text", User, Exact, t)` is unreadable at the call
/// site and easy to transpose.
struct Doc<'a> {
    ws: WorkspaceId,
    root: RootId,
    path: &'a str,
    body: &'a str,
    origin: Origin,
    provenance: ProvenanceClass,
    modified: Timestamp,
}

impl<'a> Doc<'a> {
    fn new(ws: WorkspaceId, root: RootId, path: &'a str, body: &'a str) -> Self {
        Self {
            ws,
            root,
            path,
            body,
            origin: Origin::User,
            provenance: ProvenanceClass::Exact,
            modified: Timestamp::from_millis(1_700_000_000_000),
        }
    }

    fn origin(mut self, o: Origin) -> Self {
        self.origin = o;
        self
    }

    fn provenance(mut self, p: ProvenanceClass) -> Self {
        self.provenance = p;
        self
    }

    fn modified(mut self, ms: i64) -> Self {
        self.modified = Timestamp::from_millis(ms);
        self
    }
}

fn provenance_sql(p: ProvenanceClass) -> &'static str {
    match p {
        ProvenanceClass::Exact => "EXACT",
        ProvenanceClass::Degraded => "DEGRADED",
        ProvenanceClass::Approximate => "APPROXIMATE",
        ProvenanceClass::MetadataOnly => "METADATA_ONLY",
    }
}

impl Corpus {
    fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        let store = Store::open(dir.path().join("marrow.sqlite")).expect("open store");
        let index = Fts5Index::open(&store).expect("open index");
        Self {
            index,
            store,
            _dir: dir,
        }
    }

    /// A workspace with one consented root.
    fn workspace(&self, name: &str, root_path: &str) -> (WorkspaceId, RootId) {
        let ws = self
            .store
            .upsert_workspace(NewWorkspace::new(name))
            .expect("workspace");
        let root = self
            .store
            .upsert_root(NewRoot::new(ws, root_path))
            .expect("root");
        self.store.flush().expect("flush");
        (ws, root)
    }

    /// Record a file, one version, one chunk, and one index document — the
    /// same four writes the ingest pipeline makes.
    fn add(&self, d: Doc<'_>) -> (FileId, ChunkId) {
        let hash = ContentHash::of(d.body.as_bytes());
        let mut file = NewFile::new(d.ws, d.root, d.path);
        file.origin = d.origin;
        file.status = FileStatus::Active;
        let mut version = NewVersion::new(file.file_id, d.path, d.body.len() as i64, hash);
        version.mtime_ms = d.modified;
        version.observed_at = d.modified;
        version.mime = Some("text/plain".into());
        let version_id = version.version_id;

        let (file_id, _) = self
            .store
            .insert_file_with_version(file, version)
            .expect("insert file");

        let chunk_id = ChunkId::new();
        let rows = vec![NewChunk {
            chunk_id,
            version_id,
            chunk_kind: "TEXT".into(),
            text: d.body.to_string(),
            context_prefix: Some(format!("{} › body", d.path)),
            token_count: d.body.split_whitespace().count() as i64,
            text_hash: ContentHash::of(d.body.as_bytes()),
            chunker_version: "test-chunker/1".into(),
            provenance_class: provenance_sql(d.provenance).into(),
        }];
        self.store
            .writer()
            .submit(move |c| marrow_store::read::replace_chunks(c, version_id, &rows))
            .expect("chunks");
        self.store.flush().expect("flush");

        self.index
            .upsert(&[TextDoc {
                chunk_id,
                file_id,
                version_id,
                workspace_id: d.ws,
                path: d.path.to_string(),
                title: format!("{} › body", d.path),
                body: d.body.to_string(),
                span: SourceSpan::Lines { start: 1, end: 1 },
                provenance: d.provenance,
                origin: d.origin,
                modified: d.modified,
            }])
            .expect("index doc");
        self.store.flush().expect("flush");
        (file_id, chunk_id)
    }

    fn search(&self, req: &SearchRequest) -> marrow_core::Result<marrow_query::SearchResults> {
        search(&self.store, &self.index, req)
    }
}

// -------------------------------------------------------------------- search

#[test]
fn search_returns_workspace_relative_paths() {
    let c = Corpus::new();
    let (ws, root) = c.workspace("desktop", "/corpus/desktop");
    c.add(Doc::new(
        ws,
        root,
        "/corpus/desktop/src/auth/token.rs",
        "the refresh token rotates on every authentication",
    ));

    let results = c.search(&SearchRequest::new("refresh token")).unwrap();
    assert_eq!(results.hits.len(), 1);
    let hit = &results.hits[0];

    // The index only ever saw the absolute path; the workspace name and the
    // root to strip both come from canonical state.
    assert_eq!(hit.relative_path, "src/auth/token.rs");
    assert_eq!(hit.workspace, "desktop");
    assert_eq!(
        hit.hit.path, "/corpus/desktop/src/auth/token.rs",
        "the absolute path is kept: it is what actually opens the file"
    );
    assert_eq!(results.branches, vec!["lexical"]);
    assert_eq!(results.total, 1);
}

#[test]
fn self_written_content_is_flagged_and_downweighted() {
    // Invariant #13. Identical bodies, so BM25 cannot separate them and the
    // only thing that can reorder these two is §113.3's multiplier.
    let c = Corpus::new();
    let (ws, root) = c.workspace("desktop", "/corpus/desktop");
    let body = "quarterly revenue grew by eleven percent";
    c.add(Doc::new(ws, root, "/corpus/desktop/report.md", body));
    let (agent_file, _) =
        c.add(Doc::new(ws, root, "/corpus/desktop/summary.md", body).origin(Origin::SelfWritten));

    let results = c.search(&SearchRequest::new("quarterly revenue")).unwrap();
    assert_eq!(results.hits.len(), 2);

    let agent = results
        .hits
        .iter()
        .find(|h| h.hit.file_id == agent_file)
        .expect("agent-written result is still findable");
    let human = results
        .hits
        .iter()
        .find(|h| h.hit.file_id != agent_file)
        .expect("user result");

    // Findable: yes. Citable: no.
    assert!(!agent.can_support_a_claim);
    assert!(human.can_support_a_claim);
    assert!(
        agent.rank > human.rank,
        "self-written content must not outrank the file it summarised"
    );
    assert!(agent.fused_score < human.fused_score);

    let m = agent
        .multipliers
        .iter()
        .find(|m| m.factor == SELF_WRITTEN_MULTIPLIER)
        .expect("the SELF multiplier is recorded, not just applied");
    assert!(m.reason.contains("SELF"), "{}", m.reason);

    // And `--explain` carries the flag, so a caller assembling evidence never
    // has to infer it from a score.
    let e = explain(
        &SearchRequest::new("quarterly revenue"),
        &results.branches,
        &results.hits,
    );
    let agent_line = e
        .hits
        .iter()
        .find(|x| x.chunk_id == agent.hit.chunk_id)
        .unwrap();
    assert!(!agent_line.can_support_a_claim);
    assert!(e.caveats.iter().any(|c| c.contains("SELF")));
}

#[test]
fn degraded_provenance_ranks_below_exact() {
    // CONV-005. Same body again: the provenance multiplier is the only signal
    // that differs, which is exactly what is under test.
    let c = Corpus::new();
    let (ws, root) = c.workspace("desktop", "/corpus/desktop");
    let body = "the migration runbook lists every rollback step";
    let (exact, _) = c.add(Doc::new(ws, root, "/corpus/desktop/runbook.md", body));
    let (degraded, _) = c.add(
        Doc::new(ws, root, "/corpus/desktop/runbook-scan.pdf", body)
            .provenance(ProvenanceClass::Degraded),
    );

    let results = c.search(&SearchRequest::new("migration runbook")).unwrap();
    assert_eq!(results.hits.len(), 2);
    assert_eq!(results.hits[0].hit.file_id, exact);
    assert_eq!(results.hits[1].hit.file_id, degraded);

    let m = &results.hits[1].multipliers;
    assert_eq!(m.len(), 1);
    assert!((m[0].factor - 0.8).abs() < 1e-6, "{:?}", m[0]);
    // Degraded content is still perfectly citable — it is down-weighted, not
    // barred. Only `origin = SELF` is barred.
    assert!(results.hits[1].can_support_a_claim);
}

#[test]
fn filters_narrow_by_workspace_extension_and_date() {
    let c = Corpus::new();
    let (desktop, d_root) = c.workspace("desktop", "/corpus/desktop");
    let (archive, a_root) = c.workspace("archive", "/corpus/archive");

    const OLD: i64 = 1_600_000_000_000;
    const NEW: i64 = 1_700_000_000_000;

    c.add(
        Doc::new(
            desktop,
            d_root,
            "/corpus/desktop/notes.md",
            "budget planning notes",
        )
        .modified(NEW),
    );
    c.add(
        Doc::new(
            desktop,
            d_root,
            "/corpus/desktop/notes.txt",
            "budget planning notes",
        )
        .modified(NEW),
    );
    c.add(
        Doc::new(
            desktop,
            d_root,
            "/corpus/desktop/old.md",
            "budget planning notes",
        )
        .modified(OLD),
    );
    c.add(
        Doc::new(
            archive,
            a_root,
            "/corpus/archive/notes.md",
            "budget planning notes",
        )
        .modified(NEW),
    );

    let all = c.search(&SearchRequest::new("budget planning")).unwrap();
    assert_eq!(all.hits.len(), 4, "unfiltered baseline");

    // Workspace.
    let by_ws = c
        .search(
            &SearchRequest::new("budget planning").filters(SearchFilters {
                workspace: Some("desktop".into()),
                ..Default::default()
            }),
        )
        .unwrap();
    assert_eq!(by_ws.hits.len(), 3);
    assert!(by_ws.hits.iter().all(|h| h.workspace == "desktop"));

    // Extension, with and without the dot, in either case.
    for ext in ["md", ".md", ".MD"] {
        let by_ext = c
            .search(
                &SearchRequest::new("budget planning").filters(SearchFilters {
                    extension: Some(ext.into()),
                    ..Default::default()
                }),
            )
            .unwrap();
        assert_eq!(by_ext.hits.len(), 3, "extension {ext:?}");
        assert!(by_ext.hits.iter().all(|h| h.hit.path.ends_with(".md")));
    }

    // Date, inclusive on both bounds.
    let recent = c
        .search(
            &SearchRequest::new("budget planning").filters(SearchFilters {
                modified_after: Some(Timestamp::from_millis(NEW)),
                ..Default::default()
            }),
        )
        .unwrap();
    assert_eq!(recent.hits.len(), 3);

    let archived = c
        .search(
            &SearchRequest::new("budget planning").filters(SearchFilters {
                modified_before: Some(Timestamp::from_millis(OLD)),
                ..Default::default()
            }),
        )
        .unwrap();
    assert_eq!(archived.hits.len(), 1);
    assert!(archived.hits[0].relative_path.ends_with("old.md"));

    // All three together.
    let narrow = c
        .search(
            &SearchRequest::new("budget planning").filters(SearchFilters {
                workspace: Some("desktop".into()),
                extension: Some("md".into()),
                modified_after: Some(Timestamp::from_millis(NEW)),
                ..Default::default()
            }),
        )
        .unwrap();
    assert_eq!(narrow.hits.len(), 1);
    assert_eq!(narrow.hits[0].relative_path, "notes.md");
    assert_eq!(narrow.hits[0].workspace, "desktop");

    // And a filter that matches nothing is genuinely empty — not an error.
    let none = c
        .search(
            &SearchRequest::new("budget planning").filters(SearchFilters {
                extension: Some("xlsx".into()),
                ..Default::default()
            }),
        )
        .unwrap();
    assert!(none.hits.is_empty());
    assert_eq!(none.total, 0);
}

#[test]
fn an_unknown_workspace_name_is_a_clean_error_not_empty_results() {
    // The worst available outcome for a typo is zero results: it reads as
    // "nothing is indexed" and sends you to debug your corpus instead of your
    // command line.
    let c = Corpus::new();
    let (ws, root) = c.workspace("desktop", "/corpus/desktop");
    c.add(Doc::new(
        ws,
        root,
        "/corpus/desktop/notes.md",
        "budget planning notes",
    ));

    let err = c
        .search(&SearchRequest::new("budget").filters(SearchFilters {
            workspace: Some("dsektop".into()),
            ..Default::default()
        }))
        .expect_err("a name that resolves to nothing must be an error");

    assert_eq!(err.code(), Code::CfgInvalid);
    assert!(
        err.message().contains("dsektop"),
        "the message must quote what was typed: {}",
        err.message()
    );
    assert!(
        err.message().contains("desktop"),
        "and name the workspaces that do exist: {}",
        err.message()
    );
    assert!(!err.retryable(), "retrying a typo produces the same typo");

    // The correctly-spelled name still works, so the check is not just refusing
    // everything.
    let ok = c
        .search(&SearchRequest::new("budget").filters(SearchFilters {
            workspace: Some("desktop".into()),
            ..Default::default()
        }))
        .unwrap();
    assert_eq!(ok.hits.len(), 1);
}

#[test]
fn an_empty_query_is_a_clean_error() {
    let c = Corpus::new();
    let (ws, root) = c.workspace("desktop", "/corpus/desktop");
    c.add(Doc::new(
        ws,
        root,
        "/corpus/desktop/notes.md",
        "budget planning notes",
    ));

    // Nothing here tokenizes to a term, so there is no question to answer.
    // Returning zero results would claim the corpus was searched and came up
    // empty, which is a different and false statement.
    for text in ["", "   ", "\t\n", "!!!", "-- ?? --"] {
        let err = match c.search(&SearchRequest::new(text)) {
            Err(e) => e,
            Ok(r) => panic!("{text:?} must be refused, got {} hits", r.hits.len()),
        };
        assert_eq!(err.code(), Code::CfgInvalid, "{text:?}");
        assert!(
            err.message().len() > 30,
            "{text:?}: the message must name a cause and an action"
        );
        assert!(!err.retryable(), "{text:?}");
    }

    // A query with one usable term is fine, punctuation and all.
    let ok = c.search(&SearchRequest::new("!! budget !!")).unwrap();
    assert_eq!(ok.hits.len(), 1);
}

// ------------------------------------------------------------- intelligence

#[test]
fn file_intelligence_reports_path_history_after_a_rename() {
    // FS-006 and invariant #2: the rename moves the path, not the identity.
    let c = Corpus::new();
    let (ws, root) = c.workspace("desktop", "/corpus/desktop");
    let (file_id, _) = c.add(Doc::new(
        ws,
        root,
        "/corpus/desktop/draft.md",
        "the launch checklist for the beta",
    ));

    let renamed_at = Timestamp::from_millis(1_800_000_000_000);
    c.store
        .record_path_change(file_id, "/corpus/desktop/final.md".into(), renamed_at)
        .unwrap();
    c.store.flush().unwrap();

    let fi = file_intelligence(&c.store, FileRef::Id(file_id)).unwrap();

    assert_eq!(
        fi.identity.file_id, file_id,
        "the identity survives the rename"
    );
    assert_eq!(
        fi.identity.current_path.as_deref(),
        Some("/corpus/desktop/final.md")
    );
    assert_eq!(fi.location.relative_path.as_deref(), Some("final.md"));

    // Oldest first, with the old range closed and the new one open.
    let history = &fi.location.path_history;
    assert_eq!(history.len(), 2, "{history:?}");
    assert!(history[0].path.ends_with("draft.md"));
    assert_eq!(history[0].observed_to, Some(renamed_at));
    assert!(history[1].path.ends_with("final.md"));
    assert_eq!(
        history[1].observed_to, None,
        "the current path is still open"
    );

    // The chunk written before the rename is still attached to the file.
    assert_eq!(fi.chunks.count, 1);
    assert!(fi.index_state.parsed);
}

#[test]
fn file_intelligence_reports_unknown_as_unknown() {
    // FI-003. The sections M1 cannot answer are `None` with a stated reason,
    // never an empty list and never "".
    let c = Corpus::new();
    let (ws, root) = c.workspace("desktop", "/corpus/desktop");
    let (file_id, _) = c.add(Doc::new(
        ws,
        root,
        "/corpus/desktop/notes.md",
        "a note about nothing in particular",
    ));

    let fi = file_intelligence(&c.store, FileRef::Id(file_id)).unwrap();

    assert!(
        fi.embedded_metadata.is_none(),
        "no metadata extractor exists"
    );
    assert!(fi.structure.is_none(), "no IR outline is persisted");
    assert!(fi.entities.is_none(), "no knowledge graph in this build");
    assert!(fi.timeline.is_none(), "no unified event log in this build");

    // Nothing writes `parse_results` in M1, so the parser's identity is
    // unknown — and says so rather than reporting an empty string.
    assert!(fi.index_state.parse.is_none());

    // Every unanswered section names itself and why it is unanswered.
    let unanswered = fi.unanswered_sections();
    assert!(unanswered.len() >= 8, "{unanswered:?}");
    for s in &unanswered {
        assert!(!s.section.is_empty());
        assert!(
            s.reason.len() > 20,
            "{:?} must explain, not label",
            s.section
        );
    }
    for want in [
        "What this file says about itself",
        "Structure",
        "Entities & relations",
        "Timeline",
        "Tables",
        "Media derivatives",
        "Links",
        "Actions",
    ] {
        assert!(
            unanswered.iter().any(|s| s.section == want),
            "{want} must be accounted for"
        );
    }

    // An `Option` that this build *can* fill is filled, so `None` above means
    // "unknown" and not "we never populate anything".
    assert_eq!(fi.identity.mime.as_deref(), Some("text/plain"));
    assert!(fi.identity.content_hash.is_some());
    assert!(fi.identity.size_bytes.is_some());
    // And a language nobody detected is `None`, not "".
    assert!(fi.identity.language.is_none());
    assert!(fi.location.cloud_provider.is_none());
}

#[test]
fn file_intelligence_finds_duplicates_by_content_hash() {
    // FS-008. Invariant #3: a shared hash is dedup information, not shared
    // identity — these stay two files.
    let c = Corpus::new();
    let (ws, root) = c.workspace("desktop", "/corpus/desktop");
    let (other_ws, other_root) = c.workspace("archive", "/corpus/archive");
    let body = "identical bytes in two places";

    let (a, _) = c.add(Doc::new(ws, root, "/corpus/desktop/a.md", body));
    let (b, _) = c.add(Doc::new(other_ws, other_root, "/corpus/archive/b.md", body));
    let (unique, _) = c.add(Doc::new(
        ws,
        root,
        "/corpus/desktop/c.md",
        "different bytes",
    ));

    let fi = file_intelligence(&c.store, FileRef::Id(a)).unwrap();
    assert_eq!(
        fi.identity.duplicates.len(),
        1,
        "{:?}",
        fi.identity.duplicates
    );
    let dup = &fi.identity.duplicates[0];
    assert_eq!(dup.file_id, b);
    assert_eq!(dup.path.as_deref(), Some("/corpus/archive/b.md"));
    assert_eq!(dup.workspace, "archive", "duplicates cross workspaces");
    assert_ne!(a, b, "identical content is still two files");

    // Symmetric.
    let back = file_intelligence(&c.store, FileRef::Id(b)).unwrap();
    assert_eq!(back.identity.duplicates.len(), 1);
    assert_eq!(back.identity.duplicates[0].file_id, a);

    // And a file with no twin reports an empty list, which is a real answer —
    // unlike the `None` sections, which mean "not looked at".
    let alone = file_intelligence(&c.store, FileRef::Id(unique)).unwrap();
    assert!(alone.identity.duplicates.is_empty());
}

#[test]
fn file_intelligence_by_path_and_by_id_agree() {
    let c = Corpus::new();
    let (ws, root) = c.workspace("desktop", "/corpus/desktop");
    let (file_id, chunk_id) = c.add(Doc::new(
        ws,
        root,
        "/corpus/desktop/q2/report.md",
        "quarterly figures for the second quarter",
    ));
    let _ = chunk_id;

    let by_id = file_intelligence(&c.store, FileRef::Id(file_id)).unwrap();
    let by_path = file_intelligence(
        &c.store,
        FileRef::Path("/corpus/desktop/q2/report.md".into()),
    )
    .unwrap();

    // A path is a lookup, not an identity (invariant #2): resolving one must
    // land on the same file id and therefore the same panel.
    assert_eq!(by_id.identity.file_id, by_path.identity.file_id);
    assert_eq!(by_id.identity.content_hash, by_path.identity.content_hash);
    assert_eq!(by_id.identity.size_bytes, by_path.identity.size_bytes);
    assert_eq!(by_id.versions.count, by_path.versions.count);
    assert_eq!(
        by_id.versions.supersedes_chain(),
        by_path.versions.supersedes_chain()
    );
    assert_eq!(by_id.chunks.count, by_path.chunks.count);
    assert_eq!(by_id.location.workspace, by_path.location.workspace);
    assert_eq!(by_id.location.relative_path, by_path.location.relative_path);
    assert_eq!(
        by_id.index_state.chunk_count,
        by_path.index_state.chunk_count
    );

    // A path nobody indexed is a clean error, not an empty panel.
    let err = file_intelligence(&c.store, FileRef::Path("/corpus/desktop/nope.md".into()))
        .expect_err("an unknown path must be an error");
    assert_eq!(err.code(), Code::FsNotFound);
    assert!(err.message().len() > 30);
}

#[test]
fn file_intelligence_reports_the_index_state_it_can_actually_see() {
    let c = Corpus::new();
    let (ws, root) = c.workspace("desktop", "/corpus/desktop");
    let (file_id, _) = c.add(
        Doc::new(
            ws,
            root,
            "/corpus/desktop/scan.pdf",
            "text recovered from a scan",
        )
        .provenance(ProvenanceClass::Degraded),
    );

    let fi = file_intelligence(&c.store, FileRef::Id(file_id)).unwrap();
    assert!(fi.index_state.parsed);
    assert_eq!(fi.index_state.chunk_count, 1);
    assert_eq!(
        fi.index_state.provenance_class,
        Some(ProvenanceClass::Degraded),
        "the badge shows the worst class present"
    );
    assert_eq!(
        fi.index_state.chunker_versions,
        vec!["test-chunker/1".to_string()],
        "the one processor version M1 does record (invariant #4)"
    );
    assert!(fi.index_state.pending_jobs.is_empty());
    assert!(fi.index_state.errors.is_empty());

    // A summary, not a reader: counts and breadcrumbs, never chunk bodies.
    assert_eq!(fi.chunks.count, 1);
    assert_eq!(fi.chunks.by_kind.len(), 1);
    assert_eq!(fi.chunks.by_kind[0].kind, "TEXT");
    assert!(fi.chunks.total_tokens > 0);
    assert!(fi
        .chunks
        .sample_context
        .iter()
        .all(|s| !s.contains("recovered from a scan")));

    // Invariant #5 is answered before anyone opens the file.
    assert!(fi.location.safe_to_read);
    assert_eq!(fi.location.root_path, "/corpus/desktop");
}

#[test]
fn file_intelligence_is_assembled_while_the_writer_is_still_working() {
    // FI-005 assembles the panel inside one read transaction. On WAL that is a
    // snapshot, not a lock: a write landing mid-assembly must neither block nor
    // be half-visible. (This is the behaviour that in-memory shared-cache does
    // not reproduce, which is why these fixtures are file-backed.)
    let c = Corpus::new();
    let (ws, root) = c.workspace("desktop", "/corpus/desktop");
    let (file_id, _) = c.add(Doc::new(ws, root, "/corpus/desktop/a.md", "first file"));

    let before = file_intelligence(&c.store, FileRef::Id(file_id)).unwrap();
    c.add(Doc::new(ws, root, "/corpus/desktop/b.md", "second file"));
    let after = file_intelligence(&c.store, FileRef::Id(file_id)).unwrap();

    assert_eq!(before.identity.file_id, after.identity.file_id);
    assert_eq!(before.versions.count, after.versions.count);
    assert!(before.identity.duplicates.is_empty());
    assert!(after.identity.duplicates.is_empty());
}
