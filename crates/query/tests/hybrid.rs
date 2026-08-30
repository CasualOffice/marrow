//! The semantic branch, fused with the lexical one.
//!
//! Embeddings here are hand-written unit vectors rather than a model's output.
//! That is on purpose: what needs testing is the *fusion* — that a semantic-only
//! hit is rendered and citable, that a failure drops the branch rather than the
//! search, and that the result says which branches actually ran. None of that
//! is a property of the model, and running one would make these tests slow,
//! non-deterministic and unable to construct the cases that matter.

use marrow_core::{
    ChunkId, ContentHash, FileId, FileStatus, Origin, RootId, TierState, Timestamp, VersionId,
    WorkspaceId,
};
use marrow_index::{Embedding, Fts5Index, SqliteVectorIndex, VectorDoc, VectorIndex, VectorQuery};
use marrow_query::search::{search, search_hybrid, SearchRequest, LEXICAL, SEMANTIC};
use marrow_store::{NewFile, NewRoot, NewVersion, NewWorkspace, StorageKind, Store};

struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    text: Fts5Index,
    vectors: SqliteVectorIndex,
    workspace: WorkspaceId,
    version: VersionId,
    file: FileId,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_with_migrations(
        dir.path().join(marrow_store::DB_FILE_NAME),
        marrow_index::MIGRATIONS,
    )
    .unwrap();
    let now = Timestamp::now();
    let workspace = store
        .upsert_workspace(NewWorkspace {
            workspace_id: WorkspaceId::new(),
            name: "notes".into(),
            at: now,
        })
        .unwrap();
    let root = store
        .upsert_root(NewRoot {
            root_id: RootId::new(),
            workspace_id: workspace,
            canonical_path: dir.path().to_string_lossy().into_owned(),
            volume_identity: None,
            grant_token: None,
            storage_kind: StorageKind::Local,
            cloud_provider: None,
            at: now,
        })
        .unwrap();
    let file = FileId::new();
    let f = NewFile {
        file_id: file,
        workspace_id: workspace,
        root_id: root,
        current_path: Some(dir.path().join("lease.md").to_string_lossy().into_owned()),
        fs_identity: Some("id".into()),
        tier_state: TierState::Resident,
        origin: Origin::User,
        origin_txn_id: None,
        external_source_url: None,
        status: FileStatus::Active,
        at: now,
    };
    let v = NewVersion::new(file, "lease.md", 1, ContentHash::of(b"x"));
    let version = v.version_id;
    store
        .writer()
        .submit(move |c| marrow_store::read::insert_file_with_version(c, &f, &v).map(|_| ()))
        .unwrap();
    store.flush().unwrap();

    let text = Fts5Index::open(&store).unwrap();
    let vectors = SqliteVectorIndex::open(&store).unwrap();
    Fixture {
        _dir: dir,
        store,
        text,
        vectors,
        workspace,
        version,
        file,
    }
}

impl Fixture {
    /// A chunk in the canonical store and in the lexical index.
    fn chunk(&self, body: &str) -> ChunkId {
        let id = self.chunk_row(body);
        use marrow_index::TextIndex;
        self.text
            .upsert(&[marrow_index::TextDoc {
                chunk_id: id,
                file_id: self.file,
                version_id: self.version,
                workspace_id: self.workspace,
                path: "lease.md".into(),
                title: String::new(),
                body: body.to_string(),
                span: marrow_core::SourceSpan::Lines { start: 1, end: 1 },
                provenance: marrow_core::ProvenanceClass::Exact,
                origin: Origin::User,
                modified: Timestamp::now(),
            }])
            .unwrap();
        id
    }

    /// A chunk the lexical index has never seen, so it can only be reached
    /// through the semantic branch and its hydration step.
    fn chunk_row(&self, body: &str) -> ChunkId {
        let id = ChunkId::new();
        let (cid, vid, text) = (id.to_string(), self.version.to_string(), body.to_string());
        self.store
            .writer()
            .submit(move |c| {
                c.execute(
                    "INSERT INTO chunks (chunk_id, version_id, chunk_kind, text,
                                         token_count, text_hash, chunker_version)
                     VALUES (?1, ?2, 'TEXT', ?3, 1, 'h', 'v1')",
                    marrow_store::rusqlite::params![cid, vid, text],
                )
                .map(|_| ())
                .map_err(|e| marrow_store::map_sqlite(e, "test chunk"))
            })
            .unwrap();
        self.store.flush().unwrap();
        id
    }

    fn embed(&self, id: ChunkId, values: &[f32]) {
        self.vectors
            .upsert(&[VectorDoc {
                chunk_id: id,
                file_id: self.file,
                version_id: self.version,
                workspace_id: self.workspace,
                embedding: Embedding::new(values.to_vec()).unwrap(),
            }])
            .unwrap();
    }
}

fn query(values: &[f32]) -> Embedding {
    Embedding::new(values.to_vec()).unwrap()
}

#[test]
fn a_chunk_only_the_semantic_branch_found_is_still_rendered_and_citable() {
    // The whole reason for the branch. If a semantic-only hit cannot be
    // rendered, the branch can only ever re-rank what lexical already found,
    // which is not semantic search.
    let f = fixture();
    let lexical = f.chunk("the agreement renews on 31 December");
    let semantic = f.chunk("the tenancy rolls over at the end of the year");
    f.embed(lexical, &[1.0, 0.0]);
    f.embed(semantic, &[0.99, 0.14]);

    let req = SearchRequest::new("renews").limit(10);
    let out = search_hybrid(
        &f.store,
        &f.text,
        Some((&f.vectors, &query(&[1.0, 0.0]))),
        &req,
    )
    .unwrap();

    let ids: Vec<ChunkId> = out.hits.iter().map(|h| h.hit.chunk_id).collect();
    assert!(
        ids.contains(&semantic),
        "the semantic-only chunk did not survive fusion"
    );
    let hit = out
        .hits
        .iter()
        .find(|h| h.hit.chunk_id == semantic)
        .unwrap();
    assert!(
        hit.hit.snippet.text.contains("rolls over"),
        "a semantic hit must carry its own text: {:?}",
        hit.hit.snippet.text
    );
    assert!(hit.can_support_a_claim);
}

#[test]
fn a_semantic_hit_claims_no_highlight_it_did_not_earn() {
    // There is no matched term to point at. Inventing markers would put a
    // highlight on a word the user never searched for.
    let f = fixture();
    let only = f.chunk("the tenancy rolls over at the end of the year");
    f.embed(only, &[1.0, 0.0]);

    let out = search_hybrid(
        &f.store,
        &f.text,
        Some((&f.vectors, &query(&[1.0, 0.0]))),
        &SearchRequest::new("renews").limit(10),
    )
    .unwrap();
    let hit = out.hits.iter().find(|h| h.hit.chunk_id == only).unwrap();
    assert!(hit.hit.snippet.matches.is_empty());
}

#[test]
fn agreeing_branches_rank_a_chunk_above_one_only_half_of_them_found() {
    // What fusion is for: a result both branches like beats one either found
    // alone. Weights are a parameter, but this ordering is the point of RRF.
    let f = fixture();
    let both = f.chunk("the agreement renews on 31 December");
    let lexical_only = f.chunk("renews the parking permit annually");
    let semantic_only = f.chunk("the tenancy rolls over each year");
    f.embed(both, &[1.0, 0.0]);
    f.embed(semantic_only, &[0.98, 0.2]);
    f.embed(lexical_only, &[0.0, 1.0]);

    let out = search_hybrid(
        &f.store,
        &f.text,
        Some((&f.vectors, &query(&[1.0, 0.0]))),
        &SearchRequest::new("renews").limit(10),
    )
    .unwrap();
    assert_eq!(
        out.hits[0].hit.chunk_id, both,
        "the chunk both branches found must lead"
    );
    assert_eq!(out.hits[0].branch_ranks.len(), 2);
}

#[test]
fn the_result_says_which_branches_actually_ran() {
    // A result claiming a semantic branch that did not run is worse than one
    // that ran without it: the user cannot tell why an answer is thin.
    let f = fixture();
    f.chunk("the agreement renews on 31 December");

    let lexical_only = search(&f.store, &f.text, &SearchRequest::new("renews")).unwrap();
    assert_eq!(lexical_only.branches, vec![LEXICAL]);

    let hybrid = search_hybrid(
        &f.store,
        &f.text,
        Some((&f.vectors, &query(&[1.0, 0.0]))),
        &SearchRequest::new("renews"),
    )
    .unwrap();
    assert_eq!(hybrid.branches, vec![LEXICAL, SEMANTIC]);
}

#[test]
fn search_still_works_with_no_embeddings_at_all() {
    // Hard rule 10: search works with no LLM, no GPU and no network. An empty
    // vector index is the normal state before a backfill has run.
    let f = fixture();
    f.chunk("the agreement renews on 31 December");
    let out = search_hybrid(
        &f.store,
        &f.text,
        Some((&f.vectors, &query(&[1.0, 0.0]))),
        &SearchRequest::new("renews"),
    )
    .unwrap();
    assert_eq!(out.hits.len(), 1);
}

#[test]
fn a_vector_for_a_chunk_the_store_no_longer_has_is_dropped_not_rendered() {
    // The derived index is rebuildable and may lag. A result nobody can open
    // is not a result.
    let f = fixture();
    // Not in the lexical index, so the only route to it is hydration.
    let doomed = f.chunk_row("something that will be deleted");
    f.embed(doomed, &[1.0, 0.0]);
    f.store
        .writer()
        .submit(move |c| {
            c.execute(
                "UPDATE chunks SET status = 'TOMBSTONED' WHERE chunk_id = ?1",
                [doomed.to_string()],
            )
            .map(|_| ())
            .map_err(|e| marrow_store::map_sqlite(e, "tombstoning"))
        })
        .unwrap();
    f.store.flush().unwrap();

    let out = search_hybrid(
        &f.store,
        &f.text,
        Some((&f.vectors, &query(&[1.0, 0.0]))),
        &SearchRequest::new("zzzznothingmatches").limit(10),
    )
    .unwrap();
    assert!(
        out.hits.iter().all(|h| h.hit.chunk_id != doomed),
        "a tombstoned chunk was rendered from the vector index"
    );
}

#[test]
fn a_workspace_filter_reaches_the_semantic_branch_too() {
    // A filter honoured by one branch and not the other leaks results from a
    // workspace the user excluded, which is a policy failure rather than a
    // ranking one.
    let f = fixture();
    let mine = f.chunk("the agreement renews");
    f.embed(mine, &[1.0, 0.0]);

    let out = search_hybrid(
        &f.store,
        &f.text,
        Some((&f.vectors, &query(&[1.0, 0.0]))),
        &SearchRequest::new("renews")
            .filters(marrow_query::search::SearchFilters {
                workspace: Some("notes".into()),
                ..Default::default()
            })
            .limit(10),
    )
    .unwrap();
    assert_eq!(out.hits.len(), 1);

    // And a vector query built for another workspace returns nothing.
    let elsewhere = VectorQuery::new(query(&[1.0, 0.0])).workspace(WorkspaceId::new());
    assert!(f.vectors.search(&elsewhere).unwrap().is_empty());
}
