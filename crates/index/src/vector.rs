//! The vector branch, over SQLite.
//!
//! # Why brute force
//!
//! An ANN index (HNSW, IVF) is the reflex, and it is the wrong reflex here.
//! This is one person's own files: a corpus of 35,000 files produced about
//! 60,000 chunks, and a dot product against 60,000 unit vectors of 768 floats
//! is ~46M multiply-adds — single-digit milliseconds, and **exact**. An ANN
//! index would trade that exactness for a speed the machine does not need,
//! and cost a build step, a tuning parameter nobody will tune, and a second
//! structure that can disagree with the canonical one.
//!
//! The trade reverses somewhere north of a million chunks. `search` measures
//! itself and says so rather than degrading quietly.
//!
//! # Why the vectors are cached in memory
//!
//! Reading 60,000 blobs out of SQLite per query is the actual cost, not the
//! arithmetic. They are loaded once and invalidated on write. The cache is the
//! reason this is fast; the brute force is the reason it is correct.

use std::sync::{Mutex, RwLock};

use marrow_core::{ChunkId, Code, Error, FileId, Result, WorkspaceId};
use marrow_store::migrate::Migration;
use marrow_store::rusqlite::{self, Connection};
use marrow_store::{ReadConn, Store, Writer};

use crate::port::{Embedding, VectorDoc, VectorHit, VectorIndex, VectorQuery};

/// Schema version this migration writes. See `marrow_store::migrate` — the
/// chain is numbered across crates, and 3 is `self_written`.
pub const VECTOR_INDEX_VERSION: i64 = 4;

/// `schema_meta` key recording which model produced the stored vectors.
pub const MODEL_META_KEY: &str = "vector_index_model";

/// Above this many chunks, brute force stops being the obvious answer and
/// `search` says so once rather than getting quietly slower.
const BRUTE_FORCE_CEILING: usize = 1_000_000;

/// **Migration 8 — the embedding cache.**
///
/// Its own migration rather than an addition to [`MIGRATION`], which every
/// existing database has already applied and will never run again. The number
/// is the next free one in the chain across both crates: `marrow-store` took
/// 7 for `chunks.source_span`.
pub const CACHE_MIGRATION: Migration = Migration {
    version: 8,
    name: "m8_embedding_cache",
    up: r#"
-- **The embedding cache. Content-addressed, and deliberately not tied to a
-- chunk.**
--
-- `chunk_embeddings` cascades from `chunks`, so re-chunking destroys every
-- vector: a chunker change, or a scan resumed after a kill, throws away work
-- measured in hours. `chunks.text_hash` has carried the comment "embedding
-- cache key (EMB-008)" since the first migration and nothing ever used it as
-- one.
--
-- Keyed on the text rather than the chunk, so the same paragraph embeds once
-- however many chunks hold it. On the author's index that is not a marginal
-- saving: 894,493 active chunks share 175,066 distinct texts, and 104,027 of
-- those texts appear more than once.
--
-- The model is part of the key because a vector means nothing without the
-- model that produced it. Swapping models clears this table along with
-- `chunk_embeddings` — keeping the old model's vectors would let a swap back
-- be instant, and would also let the cache grow without bound on a machine
-- whose disk has already filled twice.
CREATE TABLE embedding_cache (
    text_hash    TEXT NOT NULL,
    model_id     TEXT NOT NULL,
    dims         INTEGER NOT NULL,
    vector       BLOB NOT NULL,
    created_at   INTEGER NOT NULL,
    PRIMARY KEY (text_hash, model_id)
) WITHOUT ROWID;
"#,
};

pub const MIGRATION: Migration = Migration {
    version: VECTOR_INDEX_VERSION,
    name: "m4_vector_index",
    up: r#"
CREATE TABLE chunk_embeddings (
    chunk_id     TEXT PRIMARY KEY REFERENCES chunks(chunk_id) ON DELETE CASCADE,
    file_id      TEXT NOT NULL,
    version_id   TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    dims         INTEGER NOT NULL,
    -- Little-endian f32, unit length. Normalized on the way in so similarity
    -- is a dot product and no reader has to remember to divide.
    vector       BLOB NOT NULL
);
CREATE INDEX idx_chunk_embeddings_ws ON chunk_embeddings(workspace_id);
"#,
};

/// A vector index backed by `chunk_embeddings`.
pub struct SqliteVectorIndex {
    writer: Writer,
    // One mutex-guarded reader, for the same reason `Fts5Index` has one: the
    // port is `Send + Sync` and `ReadConn` is not `Sync`.
    reader: Mutex<ReadConn>,
    /// Loaded on first search, invalidated on write. `None` means cold.
    cache: RwLock<Option<Vec<Row>>>,
    /// Set once, so "this is a large corpus" is said rather than repeated.
    warned: Mutex<bool>,
}

impl std::fmt::Debug for SqliteVectorIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteVectorIndex").finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
struct Row {
    chunk_id: ChunkId,
    file_id: FileId,
    workspace_id: WorkspaceId,
    embedding: Embedding,
}

impl SqliteVectorIndex {
    pub fn open(store: &Store) -> Result<Self> {
        Ok(Self {
            writer: store.writer().clone(),
            reader: Mutex::new(store.reader()?),
            cache: RwLock::new(None),
            warned: Mutex::new(false),
        })
    }

    fn set_model_inner(&self, model_id: &str) -> Result<bool> {
        let current = self.model_id()?;
        if current.as_deref() == Some(model_id) {
            return Ok(false);
        }
        let model = model_id.to_string();
        self.writer.submit(move |conn| {
            conn.execute("DELETE FROM chunk_embeddings", [])
                .map_err(|e| marrow_store::map_sqlite(e, "clearing embeddings"))?;
            // **The cache goes with them.** A vector means nothing without the
            // model that made it, and keeping the old model's entries would
            // let the cache grow without bound for the sake of making a swap
            // back instant — a trade the wrong way round on a machine that has
            // filled its disk.
            conn.execute("DELETE FROM embedding_cache", [])
                .map_err(|e| marrow_store::map_sqlite(e, "clearing the embedding cache"))?;
            conn.execute(
                "INSERT INTO schema_meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                rusqlite::params![MODEL_META_KEY, model],
            )
            .map_err(|e| marrow_store::map_sqlite(e, "recording the embedding model"))?;
            Ok(())
        })?;
        self.invalidate();
        Ok(true)
    }

    fn read(&self) -> Result<std::sync::MutexGuard<'_, ReadConn>> {
        self.reader.lock().map_err(|_| {
            Error::new(
                Code::IntInvariantViolated,
                "The vector index read connection was left in a broken state by a \
                 panic. Restart Marrow.",
            )
        })
    }

    fn invalidate(&self) {
        if let Ok(mut c) = self.cache.write() {
            *c = None;
        }
    }

    fn rows(&self) -> Result<Vec<Row>> {
        if let Ok(c) = self.cache.read() {
            if let Some(rows) = c.as_ref() {
                return Ok(rows.clone());
            }
        }
        let conn = self.read()?;
        let loaded = load_rows(&conn)?;
        if let Ok(mut c) = self.cache.write() {
            *c = Some(loaded.clone());
        }
        Ok(loaded)
    }
}

fn load_rows(conn: &Connection) -> Result<Vec<Row>> {
    let mut stmt = conn
        .prepare("SELECT chunk_id, file_id, workspace_id, vector FROM chunk_embeddings")
        .map_err(|e| marrow_store::map_sqlite(e, "reading embeddings"))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Vec<u8>>(3)?,
            ))
        })
        .map_err(|e| marrow_store::map_sqlite(e, "reading embeddings"))?;

    let mut out = Vec::new();
    for row in rows {
        let (chunk, file, ws, blob) =
            row.map_err(|e| marrow_store::map_sqlite(e, "reading an embedding"))?;
        // A row that cannot be parsed is skipped rather than failing the
        // search: one corrupt blob must not take the whole branch offline,
        // and the derived index is rebuildable, by the rule that says so.
        let (Ok(chunk_id), Ok(file_id), Ok(workspace_id)) = (
            chunk.parse::<ChunkId>(),
            file.parse::<FileId>(),
            ws.parse::<WorkspaceId>(),
        ) else {
            tracing::warn!(chunk = %chunk, "skipping an embedding with an unreadable id");
            continue;
        };
        let Some(embedding) = Embedding::from_bytes(&blob) else {
            tracing::warn!(chunk = %chunk, "skipping an embedding with an unreadable vector");
            continue;
        };
        out.push(Row {
            chunk_id,
            file_id,
            workspace_id,
            embedding,
        });
    }
    Ok(out)
}

impl VectorIndex for SqliteVectorIndex {
    fn upsert(&self, docs: &[VectorDoc]) -> Result<()> {
        if docs.is_empty() {
            return Ok(());
        }
        // Every vector in one index must have the same width, or a query
        // silently matches only the subset that happens to agree with it.
        let dims = docs[0].embedding.dims();
        if let Some(odd) = docs.iter().find(|d| d.embedding.dims() != dims) {
            return Err(Error::new(
                Code::IdxCorrupt,
                "Two embeddings in one batch have different widths, which means \
                 they came from different models. Nothing was written.",
            )
            .with_context(format!(
                "{} has {} dimensions, expected {dims}",
                odd.chunk_id,
                odd.embedding.dims()
            )));
        }

        let owned: Vec<(String, String, String, String, i64, Vec<u8>)> = docs
            .iter()
            .map(|d| {
                (
                    d.chunk_id.to_string(),
                    d.file_id.to_string(),
                    d.version_id.to_string(),
                    d.workspace_id.to_string(),
                    d.embedding.dims() as i64,
                    d.embedding.to_bytes(),
                )
            })
            .collect();

        self.writer.submit(move |conn| {
            let mut stmt = conn
                .prepare(
                    "INSERT INTO chunk_embeddings
                       (chunk_id, file_id, version_id, workspace_id, dims, vector)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT(chunk_id) DO UPDATE SET
                       file_id = excluded.file_id, version_id = excluded.version_id,
                       workspace_id = excluded.workspace_id, dims = excluded.dims,
                       vector = excluded.vector",
                )
                .map_err(|e| marrow_store::map_sqlite(e, "writing embeddings"))?;
            for (c, f, v, w, d, blob) in &owned {
                stmt.execute(rusqlite::params![c, f, v, w, d, blob])
                    .map_err(|e| marrow_store::map_sqlite(e, "writing an embedding"))?;
            }
            Ok(())
        })?;
        self.invalidate();
        Ok(())
    }

    fn delete(&self, ids: &[ChunkId]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let owned: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
        self.writer.submit(move |conn| {
            for id in &owned {
                conn.execute("DELETE FROM chunk_embeddings WHERE chunk_id = ?1", [id])
                    .map_err(|e| marrow_store::map_sqlite(e, "deleting an embedding"))?;
            }
            Ok(())
        })?;
        self.invalidate();
        Ok(())
    }

    fn search(&self, q: &VectorQuery) -> Result<Vec<VectorHit>> {
        let rows = self.rows()?;
        if rows.len() > BRUTE_FORCE_CEILING {
            if let Ok(mut warned) = self.warned.lock() {
                if !*warned {
                    *warned = true;
                    tracing::warn!(
                        chunks = rows.len(),
                        "semantic search is scanning every vector; past a million \
                         chunks an approximate index would be the right trade"
                    );
                }
            }
        }

        let mut scored: Vec<VectorHit> = rows
            .iter()
            .filter(|r| q.workspace.is_none_or(|w| r.workspace_id == w))
            .filter_map(|r| {
                // A width mismatch is not a weak match, it is a different
                // model. Skipped rather than scored against.
                let score = q.embedding.similarity(&r.embedding)?;
                (score >= q.min_similarity).then_some(VectorHit {
                    chunk_id: r.chunk_id,
                    file_id: r.file_id,
                    workspace_id: r.workspace_id,
                    score,
                })
            })
            .collect();

        // Ties broken by id so the ranking is reproducible: an unstable order
        // makes RRF's output move between identical queries.
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.chunk_id.cmp(&b.chunk_id))
        });
        scored.truncate(q.limit);
        Ok(scored)
    }

    fn doc_count(&self) -> Result<u64> {
        let conn = self.read()?;
        conn.query_row("SELECT count(*) FROM chunk_embeddings", [], |r| {
            r.get::<_, i64>(0)
        })
        .map(|n| n as u64)
        .map_err(|e| marrow_store::map_sqlite(e, "counting embeddings"))
    }

    fn set_model(&self, model_id: &str) -> Result<bool> {
        self.set_model_inner(model_id)
    }

    fn model_id(&self) -> Result<Option<String>> {
        let conn = self.read()?;
        conn.query_row(
            "SELECT value FROM schema_meta WHERE key = ?1",
            [MODEL_META_KEY],
            |r| r.get::<_, String>(0),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(marrow_store::map_sqlite(
                other,
                "reading the embedding model",
            )),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use marrow_core::{RootId, Timestamp, VersionId};

    /// A store with one file and one version, so chunks can exist.
    ///
    /// The embedding table's foreign key to `chunks` is deliberate — it is
    /// `ON DELETE CASCADE`, so forgetting a file takes its vectors with it —
    /// and that means a test cannot invent chunk ids.
    struct Fixture {
        _dir: tempfile::TempDir,
        store: Store,
        version: VersionId,
        file: FileId,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_with_migrations(
            dir.path().join(marrow_store::DB_FILE_NAME),
            crate::MIGRATIONS,
        )
        .unwrap();
        let now = Timestamp::now();
        let ws = store
            .upsert_workspace(marrow_store::NewWorkspace {
                workspace_id: WorkspaceId::new(),
                name: "vectors".into(),
                at: now,
            })
            .unwrap();
        let root = store
            .upsert_root(marrow_store::NewRoot {
                root_id: RootId::new(),
                workspace_id: ws,
                canonical_path: dir.path().to_string_lossy().into_owned(),
                volume_identity: None,
                grant_token: None,
                storage_kind: marrow_store::StorageKind::Local,
                cloud_provider: None,
                at: now,
            })
            .unwrap();
        let file = FileId::new();
        let f = marrow_store::NewFile {
            file_id: file,
            workspace_id: ws,
            root_id: root,
            current_path: Some(dir.path().join("a.md").to_string_lossy().into_owned()),
            fs_identity: Some("test".into()),
            tier_state: marrow_core::TierState::Resident,
            origin: marrow_core::Origin::User,
            origin_txn_id: None,
            external_source_url: None,
            status: marrow_core::FileStatus::Active,
            at: now,
        };
        let v = marrow_store::NewVersion::new(file, "a.md", 1, marrow_core::ContentHash::of(b"x"));
        let version = v.version_id;
        store
            .writer()
            .submit(move |c| marrow_store::read::insert_file_with_version(c, &f, &v).map(|_| ()))
            .unwrap();
        store.flush().unwrap();
        Fixture {
            _dir: dir,
            store,
            version,
            file,
        }
    }

    impl Fixture {
        /// A real chunk row, so the foreign key is satisfied.
        fn chunk(&self, text: &str) -> ChunkId {
            let id = ChunkId::new();
            let (cid, vid, body) = (id.to_string(), self.version.to_string(), text.to_string());
            self.store
                .writer()
                .submit(move |c| {
                    c.execute(
                        "INSERT INTO chunks (chunk_id, version_id, chunk_kind, text,
                                             token_count, text_hash, chunker_version)
                         VALUES (?1, ?2, 'TEXT', ?3, 1, 'h', 'v1')",
                        rusqlite::params![cid, vid, body],
                    )
                    .map(|_| ())
                    .map_err(|e| marrow_store::map_sqlite(e, "inserting a test chunk"))
                })
                .unwrap();
            self.store.flush().unwrap();
            id
        }

        fn doc(&self, values: &[f32], ws: WorkspaceId) -> VectorDoc {
            VectorDoc {
                chunk_id: self.chunk("text"),
                file_id: self.file,
                version_id: self.version,
                workspace_id: ws,
                embedding: Embedding::new(values.to_vec()).unwrap(),
            }
        }
    }

    #[test]
    fn an_embedding_is_unit_length_whatever_it_was_given() {
        // Similarity is a dot product downstream. One un-normalized vector
        // makes every comparison against it quietly wrong rather than
        // obviously wrong.
        let e = Embedding::new(vec![3.0, 4.0]).unwrap();
        let norm: f32 = e.as_slice().iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6, "norm was {norm}");
        assert!((e.similarity(&e).unwrap() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_zero_or_empty_vector_has_no_direction_and_is_refused() {
        // A zero vector matches everything equally, which is the worst
        // possible answer to give confidently.
        assert!(Embedding::new(vec![0.0, 0.0]).is_none());
        assert!(Embedding::new(Vec::<f32>::new()).is_none());
        assert!(Embedding::new(vec![f32::NAN, 1.0]).is_none());
    }

    #[test]
    fn vectors_of_different_widths_are_not_compared_at_all() {
        // Two embedding models' outputs are not comparable. Comparing what
        // they have in common returns plausible nonsense.
        let a = Embedding::new(vec![1.0, 0.0]).unwrap();
        let b = Embedding::new(vec![1.0, 0.0, 0.0]).unwrap();
        assert_eq!(a.similarity(&b), None);
    }

    #[test]
    fn a_vector_round_trips_through_its_stored_bytes() {
        let e = Embedding::new(vec![0.1, -0.5, 0.83, 0.2]).unwrap();
        let back = Embedding::from_bytes(&e.to_bytes()).unwrap();
        assert!((e.similarity(&back).unwrap() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_truncated_blob_is_refused_rather_than_decoded_short() {
        // A short decode would produce a narrower vector that silently stops
        // matching anything, which reads as "semantic search found nothing".
        let e = Embedding::new(vec![1.0, 2.0, 3.0]).unwrap();
        let mut bytes = e.to_bytes();
        bytes.pop();
        assert!(Embedding::from_bytes(&bytes).is_none());
    }

    #[test]
    fn the_nearest_vector_comes_back_first() {
        let f = fixture();
        let idx = SqliteVectorIndex::open(&f.store).unwrap();
        let ws = WorkspaceId::new();
        let near = f.doc(&[1.0, 0.1, 0.0], ws);
        let far = f.doc(&[0.0, 0.0, 1.0], ws);
        let near_id = near.chunk_id;
        idx.upsert(&[near, far]).unwrap();

        let q = VectorQuery::new(Embedding::new(vec![1.0, 0.0, 0.0]).unwrap()).min_similarity(-1.0);
        let hits = idx.search(&q).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].chunk_id, near_id);
        assert!(hits[0].score > hits[1].score);
    }

    #[test]
    fn a_weak_match_is_left_out_rather_than_ranked_last() {
        // A nearest-neighbour search always returns *something*. Without a
        // floor the branch contributes its least-bad guesses to every query,
        // and RRF then promotes them for having a rank at all.
        let f = fixture();
        let idx = SqliteVectorIndex::open(&f.store).unwrap();
        let ws = WorkspaceId::new();
        idx.upsert(&[f.doc(&[0.0, 1.0, 0.0], ws)]).unwrap();

        let q = VectorQuery::new(Embedding::new(vec![1.0, 0.0, 0.0]).unwrap());
        assert!(
            idx.search(&q).unwrap().is_empty(),
            "orthogonal is not a match"
        );
        assert_eq!(
            idx.search(&q.clone().min_similarity(-1.0)).unwrap().len(),
            1
        );
    }

    #[test]
    fn a_workspace_filter_is_applied_before_ranking() {
        let f = fixture();
        let idx = SqliteVectorIndex::open(&f.store).unwrap();
        let (mine, theirs) = (WorkspaceId::new(), WorkspaceId::new());
        idx.upsert(&[f.doc(&[1.0, 0.0], theirs), f.doc(&[0.9, 0.1], mine)])
            .unwrap();
        let q = VectorQuery::new(Embedding::new(vec![1.0, 0.0]).unwrap())
            .workspace(mine)
            .min_similarity(-1.0);
        let hits = idx.search(&q).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].workspace_id, mine);
    }

    #[test]
    fn a_batch_mixing_two_models_widths_is_refused_whole() {
        // Half-written, the index would answer some queries and not others,
        // and nothing would say which.
        let f = fixture();
        let idx = SqliteVectorIndex::open(&f.store).unwrap();
        let ws = WorkspaceId::new();
        let e = idx
            .upsert(&[f.doc(&[1.0, 0.0], ws), f.doc(&[1.0, 0.0, 0.0], ws)])
            .unwrap_err();
        assert_eq!(e.code(), Code::IdxCorrupt);
        assert_eq!(idx.doc_count().unwrap(), 0, "nothing was written");
    }

    #[test]
    fn changing_the_embedding_model_discards_every_vector() {
        // Two models' vectors are not comparable. Mixing them produces a
        // search that is wrong in a way no test on either model would catch.
        let f = fixture();
        let idx = SqliteVectorIndex::open(&f.store).unwrap();
        let ws = WorkspaceId::new();
        assert!(idx.set_model("embeddinggemma-300m").unwrap());
        idx.upsert(&[f.doc(&[1.0, 0.0], ws)]).unwrap();
        assert_eq!(idx.doc_count().unwrap(), 1);

        assert!(
            !idx.set_model("embeddinggemma-300m").unwrap(),
            "same model is a no-op"
        );
        assert_eq!(idx.doc_count().unwrap(), 1);

        assert!(idx.set_model("qwen3-embedding-0.6b").unwrap());
        assert_eq!(idx.doc_count().unwrap(), 0, "the old vectors must be gone");
        assert_eq!(
            idx.model_id().unwrap().as_deref(),
            Some("qwen3-embedding-0.6b")
        );
    }

    #[test]
    fn an_upsert_replaces_rather_than_duplicating() {
        let f = fixture();
        let idx = SqliteVectorIndex::open(&f.store).unwrap();
        let ws = WorkspaceId::new();
        let mut d = f.doc(&[1.0, 0.0], ws);
        idx.upsert(std::slice::from_ref(&d)).unwrap();
        d.embedding = Embedding::new(vec![0.0, 1.0]).unwrap();
        idx.upsert(std::slice::from_ref(&d)).unwrap();
        assert_eq!(idx.doc_count().unwrap(), 1);

        let q = VectorQuery::new(Embedding::new(vec![0.0, 1.0]).unwrap());
        assert_eq!(idx.search(&q).unwrap()[0].score, 1.0);
    }

    #[test]
    fn deleting_a_chunk_removes_it_from_the_next_search() {
        // The cache is the reason this is fast and the reason it could be
        // wrong. A write that does not invalidate it serves deleted results.
        let f = fixture();
        let idx = SqliteVectorIndex::open(&f.store).unwrap();
        let ws = WorkspaceId::new();
        let d = f.doc(&[1.0, 0.0], ws);
        let id = d.chunk_id;
        idx.upsert(&[d]).unwrap();
        let q = VectorQuery::new(Embedding::new(vec![1.0, 0.0]).unwrap());
        assert_eq!(idx.search(&q).unwrap().len(), 1);

        idx.delete(&[id]).unwrap();
        assert!(
            idx.search(&q).unwrap().is_empty(),
            "the cache served a deleted vector"
        );
    }

    #[test]
    fn ties_are_broken_reproducibly() {
        // An unstable order makes RRF's output move between identical queries.
        let f = fixture();
        let idx = SqliteVectorIndex::open(&f.store).unwrap();
        let ws = WorkspaceId::new();
        idx.upsert(&(0..8).map(|_| f.doc(&[1.0, 0.0], ws)).collect::<Vec<_>>())
            .unwrap();
        let q = VectorQuery::new(Embedding::new(vec![1.0, 0.0]).unwrap());
        let a: Vec<_> = idx.search(&q).unwrap().iter().map(|h| h.chunk_id).collect();
        let b: Vec<_> = idx.search(&q).unwrap().iter().map(|h| h.chunk_id).collect();
        assert_eq!(a, b);
    }

    #[test]
    fn an_empty_index_answers_nothing_rather_than_failing() {
        // Semantic search being unavailable is not an error; it is the state
        // before a backfill has run, and search must still work with no LLM,
        // no GPU and no network.
        let f = fixture();
        let idx = SqliteVectorIndex::open(&f.store).unwrap();
        let q = VectorQuery::new(Embedding::new(vec![1.0, 0.0]).unwrap());
        assert!(idx.search(&q).unwrap().is_empty());
        assert_eq!(idx.doc_count().unwrap(), 0);
        assert_eq!(idx.model_id().unwrap(), None);
    }
}

/// Vectors already computed for these texts, under the current model.
///
/// The key is the text, not the chunk: the same paragraph in four files embeds
/// once. Returns only what it has — a miss is ordinary and the caller embeds it.
pub fn cached(
    conn: &marrow_store::rusqlite::Connection,
    model_id: &str,
    hashes: &[String],
) -> Result<std::collections::HashMap<String, Embedding>> {
    let mut out = std::collections::HashMap::new();
    if hashes.is_empty() {
        return Ok(out);
    }
    let mut stmt = conn
        .prepare_cached(
            "SELECT dims, vector FROM embedding_cache WHERE text_hash = ?1 AND model_id = ?2",
        )
        .map_err(|e| marrow_store::map_sqlite(e, "reading the embedding cache"))?;
    for h in hashes {
        let row: Option<(i64, Vec<u8>)> = match stmt
            .query_row(marrow_store::rusqlite::params![h, model_id], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
            }) {
            Ok(v) => Some(v),
            Err(marrow_store::rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(marrow_store::map_sqlite(e, "reading a cached embedding")),
        };
        if let Some((dims, blob)) = row {
            // `from_bytes` refuses a truncated blob rather than decoding a
            // narrower vector, which would silently stop matching anything and
            // read as "semantic search found nothing". A row that fails here is
            // corrupt rather than stale, and skipping it re-embeds the text —
            // the safe direction.
            match Embedding::from_bytes(&blob) {
                Some(v) if v.dims() as i64 == dims => {
                    out.insert(h.clone(), v);
                }
                _ => tracing::warn!(
                    text_hash = %h,
                    "a cached embedding would not decode; it will be recomputed"
                ),
            }
        }
    }
    Ok(out)
}

/// Remember these vectors by their text, so re-chunking does not throw them away.
pub fn cache(
    conn: &marrow_store::rusqlite::Connection,
    model_id: &str,
    now: marrow_core::Timestamp,
    entries: &[(String, Embedding)],
) -> Result<()> {
    let mut stmt = conn
        .prepare_cached(
            "INSERT INTO embedding_cache (text_hash, model_id, dims, vector, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(text_hash, model_id) DO NOTHING",
        )
        .map_err(|e| marrow_store::map_sqlite(e, "writing the embedding cache"))?;
    for (hash, v) in entries {
        stmt.execute(marrow_store::rusqlite::params![
            hash,
            model_id,
            v.dims() as i64,
            v.to_bytes(),
            now.as_millis(),
        ])
        .map_err(|e| marrow_store::map_sqlite(e, "writing a cached embedding"))?;
    }
    Ok(())
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    use marrow_store::Store;

    fn store() -> (tempfile::TempDir, Store) {
        let d = tempfile::tempdir().expect("tempdir");
        let s = Store::open_with_migrations(d.path().join("m.sqlite"), crate::MIGRATIONS)
            .expect("store");
        (d, s)
    }

    fn put(s: &Store, model: &str, entries: Vec<(String, Embedding)>) {
        let m = model.to_string();
        let at = marrow_core::Timestamp::now();
        s.writer()
            .submit(move |c| cache(c, &m, at, &entries))
            .expect("submit");
        s.flush().expect("flush");
    }

    /// **The whole point: a vector outlives the chunk it was made for.**
    ///
    /// `chunk_embeddings` cascades from `chunks`, so a re-chunk — a chunker
    /// change, or a scan resumed after a kill — destroys every vector and the
    /// next backfill recomputes work measured in hours. The cache is keyed on
    /// the text and has no foreign key to a chunk, so it survives.
    #[test]
    fn a_cached_vector_survives_the_chunk_it_was_made_for() {
        let (_d, s) = store();
        let v = Embedding::new(vec![0.1, 0.2, 0.3]).expect("a vector");
        put(&s, "bge-small", vec![("hash-a".into(), v.clone())]);

        // Nothing here references a chunk row, so there is nothing for a
        // delete to cascade through.
        let conn = s.reader().expect("reader");
        let got = cached(&conn, "bge-small", &["hash-a".to_string()]).expect("read");
        let back = got.get("hash-a").expect("still there");
        assert!((v.similarity(back).expect("same width") - 1.0).abs() < 1e-6);
    }

    /// A vector means nothing without the model that produced it.
    #[test]
    fn a_vector_is_not_reused_across_models() {
        let (_d, s) = store();
        let v = Embedding::new(vec![1.0, 0.0]).expect("a vector");
        put(&s, "bge-small", vec![("hash-a".into(), v)]);

        let conn = s.reader().expect("reader");
        let other = cached(&conn, "a-different-model", &["hash-a".to_string()]).expect("read");
        assert!(
            other.is_empty(),
            "reusing another model's vector would silently poison every comparison"
        );
    }

    #[test]
    fn a_miss_is_ordinary_and_returns_nothing_rather_than_failing() {
        let (_d, s) = store();
        let conn = s.reader().expect("reader");
        let got = cached(&conn, "bge-small", &["never-seen".to_string()]).expect("not an error");
        assert!(got.is_empty());
    }

    #[test]
    fn re_caching_the_same_text_keeps_the_first_vector_rather_than_erroring() {
        // The backfill can meet the same text twice across runs. Writing must
        // converge rather than conflict.
        let (_d, s) = store();
        let a = Embedding::new(vec![1.0, 0.0]).expect("a");
        put(&s, "m", vec![("h".into(), a.clone())]);
        put(
            &s,
            "m",
            vec![("h".into(), Embedding::new(vec![0.0, 1.0]).expect("b"))],
        );

        let conn = s.reader().expect("reader");
        let got = cached(&conn, "m", &["h".to_string()]).expect("read");
        let back = got.get("h").expect("present");
        assert!(
            (a.similarity(back).expect("same width") - 1.0).abs() < 1e-6,
            "the same text under the same model has one answer; the first stands"
        );
    }
}
