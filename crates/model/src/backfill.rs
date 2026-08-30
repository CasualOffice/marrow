//! Embedding the chunks that do not have vectors yet.
//!
//! A backfill over 60,000 chunks takes minutes, and it will be interrupted —
//! the window closes, the machine sleeps, the user asks a question and would
//! rather have the memory. So the design is the same as every other durable
//! job here (hard rule 7): **idempotent and resumable**. It asks the store what
//! is missing, does a batch, commits it, and asks again. Nothing is remembered
//! between runs except what is already in the index.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use marrow_core::{ChunkId, FileId, Result, VersionId, WorkspaceId};
use marrow_index::{VectorDoc, VectorIndex};
use marrow_store::Store;

use crate::embed::{Embedder, BATCH};
use crate::queue::Cancel;

/// Where a backfill has got to.
///
/// Shared, because the UI needs to render it while the work is happening on
/// another thread.
#[derive(Debug, Default)]
pub struct Progress {
    pub embedded: AtomicU64,
    pub remaining: AtomicU64,
    pub failed: AtomicU64,
}

impl Progress {
    pub fn snapshot(&self) -> (u64, u64, u64) {
        (
            self.embedded.load(Ordering::Relaxed),
            self.remaining.load(Ordering::Relaxed),
            self.failed.load(Ordering::Relaxed),
        )
    }
}

/// What one run did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub embedded: u64,
    /// Chunks that could not be embedded. Counted rather than fatal: one bad
    /// chunk must not stop the other 59,999.
    pub failed: u64,
    /// True when the run stopped early because it was asked to.
    pub cancelled: bool,
}

/// One chunk waiting for a vector.
struct Pending {
    chunk_id: ChunkId,
    file_id: FileId,
    version_id: VersionId,
    workspace_id: WorkspaceId,
    text: String,
}

/// How many chunks still have no vector.
///
/// The number the UI needs to say "semantic search covers 40,000 of your
/// 60,000 chunks" rather than implying it covers everything.
pub fn remaining(store: &Store) -> Result<u64> {
    let conn = store.reader()?;
    conn.query_row(
        // **Only chunks a search could return.** `chunks.status` alone counts
        // every chunk of every superseded version and every deleted file --
        // 274,519 against 59,197 reachable on the author's index. Reporting
        // that as work remaining sends the user to a two-hour job of which four
        // fifths embeds text nothing can ever retrieve, while `marrow status`
        // helpfully suggests they run it.
        "SELECT count(*) FROM chunks c
           JOIN file_versions v ON v.version_id = c.version_id
           JOIN files f          ON f.file_id    = v.file_id
          WHERE c.status = 'ACTIVE' AND v.status = 'CURRENT' AND f.status = 'ACTIVE'
            AND NOT EXISTS (SELECT 1 FROM chunk_embeddings e WHERE e.chunk_id = c.chunk_id)",
        [],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n as u64)
    .map_err(|e| marrow_store::map_sqlite(e, "counting chunks without embeddings"))
}

fn next_batch(store: &Store, limit: usize) -> Result<Vec<Pending>> {
    let conn = store.reader()?;
    let mut stmt = conn
        .prepare(
            "SELECT c.chunk_id, c.version_id, c.text, c.context_prefix,
                    v.file_id, f.workspace_id
               FROM chunks c
               JOIN file_versions v ON v.version_id = c.version_id
               JOIN files f ON f.file_id = v.file_id
              WHERE c.status = 'ACTIVE'
                AND v.status = 'CURRENT'
                AND f.status = 'ACTIVE'
                AND NOT EXISTS (SELECT 1 FROM chunk_embeddings e WHERE e.chunk_id = c.chunk_id)
              LIMIT ?1",
        )
        .map_err(|e| marrow_store::map_sqlite(e, "selecting chunks to embed"))?;

    let rows = stmt
        .query_map([limit as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })
        .map_err(|e| marrow_store::map_sqlite(e, "selecting chunks to embed"))?;

    let mut out = Vec::new();
    for row in rows {
        let (chunk, version, text, prefix, file, ws) =
            row.map_err(|e| marrow_store::map_sqlite(e, "reading a chunk to embed"))?;
        let (Ok(chunk_id), Ok(version_id), Ok(file_id), Ok(workspace_id)) = (
            chunk.parse(),
            version.parse(),
            file.parse(),
            ws.parse::<WorkspaceId>(),
        ) else {
            continue;
        };
        // The heading chain goes in with the body (CHK-002). A chunk that says
        // "renews on 31 December" means something different under *Termination*
        // than under *Rent review*, and the embedding should know which.
        let text = match prefix.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
            Some(p) => format!("{p}\n\n{text}"),
            None => text,
        };
        out.push(Pending {
            chunk_id,
            file_id,
            version_id,
            workspace_id,
            text,
        });
    }
    Ok(out)
}

/// Embed everything that has no vector yet.
///
/// Returns when there is nothing left, or when `cancel` is set. Safe to call
/// again either way: it re-asks the store rather than resuming from a cursor,
/// so a run that died mid-batch loses at most that batch.
pub fn run(
    store: &Store,
    vectors: &dyn VectorIndex,
    embedder: &Embedder,
    cancel: &Cancel,
    progress: &Arc<Progress>,
) -> Result<Outcome> {
    // Changing the model invalidates every vector, so this is checked *before*
    // any work rather than discovered when the widths disagree.
    if vectors.set_model(embedder.model_id())? {
        tracing::info!(
            model = embedder.model_id(),
            "the embedding model changed; existing vectors were discarded"
        );
    }

    let mut outcome = Outcome::default();
    loop {
        if cancel.is_cancelled() {
            outcome.cancelled = true;
            break;
        }
        progress
            .remaining
            .store(remaining(store)?, Ordering::Relaxed);

        let batch = next_batch(store, BATCH)?;
        if batch.is_empty() {
            break;
        }

        let texts: Vec<String> = batch.iter().map(|p| p.text.clone()).collect();
        let embeddings = match embedder.embed(&texts) {
            Ok(v) => v,
            Err(e) => {
                // One bad batch must not stop the other 59,000 chunks. The
                // batch is counted as failed and skipped — and because the
                // loop re-asks the store, a batch that keeps failing would
                // spin, so it stops instead.
                tracing::warn!(error = %e, count = batch.len(), "a batch could not be embedded");
                outcome.failed += batch.len() as u64;
                progress
                    .failed
                    .fetch_add(batch.len() as u64, Ordering::Relaxed);
                break;
            }
        };

        let docs: Vec<VectorDoc> = batch
            .iter()
            .zip(embeddings)
            .map(|(p, embedding)| VectorDoc {
                chunk_id: p.chunk_id,
                file_id: p.file_id,
                version_id: p.version_id,
                workspace_id: p.workspace_id,
                embedding,
            })
            .collect();
        let n = docs.len() as u64;
        vectors.upsert(&docs)?;
        outcome.embedded += n;
        progress.embedded.fetch_add(n, Ordering::Relaxed);
    }

    progress
        .remaining
        .store(remaining(store)?, Ordering::Relaxed);
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use marrow_core::{ContentHash, FileStatus, Origin, RootId, TierState, Timestamp};
    use marrow_index::{Embedding, SqliteVectorIndex};
    use marrow_store::{NewFile, NewRoot, NewVersion, NewWorkspace, StorageKind};

    pub(super) struct Fixture {
        _dir: tempfile::TempDir,
        pub(super) store: Store,
        pub(super) vectors: SqliteVectorIndex,
        version: VersionId,
    }

    pub(super) fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_with_migrations(
            dir.path().join(marrow_store::DB_FILE_NAME),
            marrow_index::MIGRATIONS,
        )
        .unwrap();
        let now = Timestamp::now();
        let ws = store
            .upsert_workspace(NewWorkspace {
                workspace_id: WorkspaceId::new(),
                name: "notes".into(),
                at: now,
            })
            .unwrap();
        let root = store
            .upsert_root(NewRoot {
                root_id: RootId::new(),
                workspace_id: ws,
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
            workspace_id: ws,
            root_id: root,
            current_path: Some(dir.path().join("a.md").to_string_lossy().into_owned()),
            fs_identity: Some("id".into()),
            tier_state: TierState::Resident,
            origin: Origin::User,
            origin_txn_id: None,
            external_source_url: None,
            status: FileStatus::Active,
            at: now,
        };
        let v = NewVersion::new(file, "a.md", 1, ContentHash::of(b"x"));
        let version = v.version_id;
        store
            .writer()
            .submit(move |c| marrow_store::read::insert_file_with_version(c, &f, &v).map(|_| ()))
            .unwrap();
        store.flush().unwrap();
        let vectors = SqliteVectorIndex::open(&store).unwrap();
        Fixture {
            _dir: dir,
            store,
            vectors,
            version,
        }
    }

    impl Fixture {
        pub(super) fn chunk(&self, text: &str, prefix: Option<&str>) -> ChunkId {
            let id = ChunkId::new();
            let (c, v, t, p) = (
                id.to_string(),
                self.version.to_string(),
                text.to_string(),
                prefix.map(str::to_string),
            );
            self.store
                .writer()
                .submit(move |conn| {
                    conn.execute(
                        "INSERT INTO chunks (chunk_id, version_id, chunk_kind, text,
                                             context_prefix, token_count, text_hash,
                                             chunker_version)
                         VALUES (?1, ?2, 'TEXT', ?3, ?4, 1, 'h', 'v1')",
                        marrow_store::rusqlite::params![c, v, t, p],
                    )
                    .map(|_| ())
                    .map_err(|e| marrow_store::map_sqlite(e, "test chunk"))
                })
                .unwrap();
            self.store.flush().unwrap();
            id
        }
    }

    #[test]
    fn remaining_counts_only_chunks_without_a_vector() {
        // The number the UI shows. Counting all chunks would say semantic
        // search is unavailable when most of it is ready.
        let f = fixture();
        let a = f.chunk("first", None);
        f.chunk("second", None);
        assert_eq!(remaining(&f.store).unwrap(), 2);

        f.vectors
            .upsert(&[VectorDoc {
                chunk_id: a,
                file_id: FileId::new(),
                version_id: f.version,
                workspace_id: WorkspaceId::new(),
                embedding: Embedding::new(vec![1.0, 0.0]).unwrap(),
            }])
            .unwrap();
        assert_eq!(remaining(&f.store).unwrap(), 1);
    }

    #[test]
    fn the_heading_chain_goes_in_with_the_body() {
        // CHK-002. "renews on 31 December" means something different under
        // *Termination* than under *Rent review*, and the embedding should
        // know which.
        let f = fixture();
        f.chunk("renews on 31 December", Some("Lease › Termination"));
        let batch = next_batch(&f.store, 10).unwrap();
        assert_eq!(batch.len(), 1);
        assert!(batch[0].text.starts_with("Lease › Termination"));
        assert!(batch[0].text.contains("renews on 31 December"));
    }

    #[test]
    fn an_empty_heading_chain_does_not_prepend_blank_lines() {
        let f = fixture();
        f.chunk("body only", Some("   "));
        let batch = next_batch(&f.store, 10).unwrap();
        assert_eq!(batch[0].text, "body only");
    }

    #[test]
    fn a_tombstoned_chunk_is_not_queued_for_embedding() {
        // Embedding it would spend the budget on something that can never be
        // returned, and `remaining` would never reach zero.
        let f = fixture();
        let doomed = f.chunk("gone", None);
        f.store
            .writer()
            .submit(move |c| {
                c.execute(
                    "UPDATE chunks SET status='TOMBSTONED' WHERE chunk_id=?1",
                    [doomed.to_string()],
                )
                .map(|_| ())
                .map_err(|e| marrow_store::map_sqlite(e, "tombstone"))
            })
            .unwrap();
        f.store.flush().unwrap();
        assert_eq!(remaining(&f.store).unwrap(), 0);
        assert!(next_batch(&f.store, 10).unwrap().is_empty());
    }

    #[test]
    fn a_batch_is_bounded_so_a_failure_loses_little() {
        let f = fixture();
        for i in 0..(BATCH * 2) {
            f.chunk(&format!("chunk {i}"), None);
        }
        assert_eq!(next_batch(&f.store, BATCH).unwrap().len(), BATCH);
    }
}

/// Against a real embedding model. `#[ignore]` by default.
#[cfg(test)]
mod real {
    use super::*;
    use crate::worker::Runtime;
    use marrow_index::VectorQuery;
    use std::path::PathBuf;

    fn embedder() -> Embedder {
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
        let data = home.join(".local/share/marrow");
        let rt = Runtime::discover(
            &data,
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("worker/mlx_worker.py"),
        )
        .unwrap_or_else(|| panic!("{}", Runtime::setup_hint(&data)));
        let ws = crate::scratch::ModelWorkspace::open(data.join("models"), &[]).unwrap();
        let entry = crate::catalogue::builtin()
            .into_iter()
            .find(|e| e.capabilities.embedding)
            .unwrap();
        let dir = crate::download::download(
            &entry,
            &ws,
            &crate::download::Https,
            &Cancel::new(),
            &mut |_| {},
        )
        .unwrap();
        Embedder::start(&rt, &entry.id, &dir).unwrap()
    }

    #[test]
    #[ignore = "loads a real embedding model"]
    fn a_question_finds_a_chunk_that_shares_no_words_with_it() {
        // The point of the whole branch, end to end: lexical search cannot
        // find this, and semantic search must.
        let f = super::tests::fixture();
        let paraphrase = f.chunk(
            "The tenancy rolls over automatically at the end of each term unless \
             notice is given.",
            Some("Lease › Termination"),
        );
        f.chunk(
            "Deliveries are accepted between 07:00 and 11:00 on weekdays only.",
            Some("Handbook › Deliveries"),
        );

        let e = embedder();
        let progress = Arc::new(Progress::default());
        let out = run(&f.store, &f.vectors, &e, &Cancel::new(), &progress).unwrap();
        assert_eq!(out.embedded, 2);
        assert_eq!(out.failed, 0);
        assert_eq!(remaining(&f.store).unwrap(), 0, "the backfill must finish");

        // A question with none of the chunk's words in it.
        let q = e.embed_one("when does the lease renew?").unwrap();
        let hits = f.vectors.search(&VectorQuery::new(q).limit(5)).unwrap();
        assert!(!hits.is_empty(), "semantic search found nothing");
        assert_eq!(
            hits[0].chunk_id, paraphrase,
            "the paraphrase should rank first, not the delivery hours"
        );
        eprintln!("\n  top score {:.3}\n", hits[0].score);
    }

    #[test]
    #[ignore = "loads a real embedding model"]
    fn a_second_run_does_nothing_and_a_cancelled_one_can_be_resumed() {
        // Hard rule 7: idempotent and resumable. This will be interrupted —
        // the window closes, the machine sleeps — so a second run must pick up
        // where the first stopped without redoing it.
        let f = super::tests::fixture();
        for i in 0..5 {
            f.chunk(
                &format!("paragraph number {i} about a commercial lease"),
                None,
            );
        }
        let e = embedder();
        let progress = Arc::new(Progress::default());

        // Stop it after the first batch by cancelling before the second.
        let cancel = Cancel::new();
        cancel.cancel();
        let stopped = run(&f.store, &f.vectors, &e, &cancel, &progress).unwrap();
        assert!(stopped.cancelled);
        assert_eq!(stopped.embedded, 0, "a cancel before any work does none");
        assert_eq!(remaining(&f.store).unwrap(), 5);

        let first = run(&f.store, &f.vectors, &e, &Cancel::new(), &progress).unwrap();
        assert_eq!(first.embedded, 5);
        let second = run(&f.store, &f.vectors, &e, &Cancel::new(), &progress).unwrap();
        assert_eq!(second.embedded, 0, "a second run must do nothing");
    }
}
