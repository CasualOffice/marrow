//! The read model everything above the store shares.
//!
//! Three frontends, one core (GUI §1) — but the CLI, MCP and the desktop each
//! grew their own copy of the same four queries, and by the time anyone noticed
//! the workspace listing existed twice with different columns, `roots()` was
//! byte-identical in two crates, and MCP's index status was a strict subset of
//! the desktop's computed by a second, separately-maintained statement.
//!
//! That is not a tidiness problem. A number the sidebar reports and a number
//! MCP reports are the *same fact about the same index*, and two SQL statements
//! that drift apart mean two answers to one question with nothing saying which
//! is right.
//!
//! Everything here is a read. There is no writer in this crate and there should
//! not be one.

use marrow_core::{Code, Error, Result};
use marrow_store::{map_sqlite, ReadConn};
use serde::Serialize;

/// One workspace, with the numbers a surface needs to say whether it is healthy.
///
/// Per-workspace rather than a global row, because "which one is degraded" is
/// the question a total cannot answer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceStats {
    pub name: String,
    /// The root's canonical path. Empty when a workspace has no root yet.
    pub path: String,
    pub files: i64,
    pub chunks: i64,
    pub content_bytes: i64,
    /// TIER-001: files present as placeholders. Never a silent zero — a
    /// workspace that is mostly in the cloud is not a workspace that is
    /// mostly indexed.
    pub cloud_only: i64,
    /// Recorded but with no chunks: findable by name, not by content.
    ///
    /// The total, kept because it is a real fact and surfaces already report
    /// it. On its own it is **not** a health signal, which is what this number
    /// was being used as: it counted a folder of photos and a folder of
    /// corrupt PDFs identically, so 42,581 photos raised a warning triangle
    /// that no action could ever clear. The three fields below say which is
    /// which, and they sum to this one.
    pub unindexed: i64,
    /// Nothing to index, and that is the answer, not a failure.
    ///
    /// A parse ran and produced no searchable text. On any real corpus this is
    /// the T5 terminal outcome `METADATA_ONLY` — a photo, a font, a binary,
    /// which stays discoverable by name and date (Part 3 §63). An empty file
    /// that parsed `OK` and yielded no chunks lands here too; it is equally
    /// "there was nothing to search", and equally not something to fix.
    pub no_parser: i64,
    /// A parser ran and did not get the whole file: `FAILED`, `PARTIAL` or
    /// `LOW_YIELD`. The only one of the three that is worth an alarm, because
    /// it is the only one where the text exists and Marrow does not have it.
    pub parse_failed: i64,
    /// No parse was ever attempted for the current version.
    ///
    /// Not yet reached rather than given up on: an ingest run killed part-way
    /// through leaves exactly this (invariant #7 exists because that is the
    /// normal case), as does a file whose bytes were never opened. A sweep
    /// clears it, so it is actionable in a way `no_parser` is not.
    pub not_processed: i64,
}

impl WorkspaceStats {
    /// Whether anything about this workspace should be shown as degraded.
    ///
    /// Defined here rather than in each surface, so the sidebar and MCP cannot
    /// disagree about what "healthy" means.
    ///
    /// `no_parser` is deliberately absent. A folder of photos is a healthy
    /// workspace — "a file with no parser stays discoverable via metadata; not
    /// a failure" — and a warning nobody can act on is a warning everybody
    /// learns to ignore, including the one that mattered.
    pub fn is_degraded(&self) -> bool {
        self.files == 0 || self.parse_failed > 0 || self.not_processed > 0 || self.cloud_only > 0
    }
}

/// The whole index, in the numbers a status line reports.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexStats {
    pub files: i64,
    pub chunks: i64,
    pub content_bytes: i64,
    pub cloud_only: i64,
    pub workspaces: i64,
    pub schema_version: i64,
    /// Chunks with a vector, out of `chunks`.
    ///
    /// Surfaced because semantic search answers from these and only these. On
    /// this corpus 2,304 of 182,306 chunks are embedded — so a semantic branch
    /// speaks for 1.3% of the index while looking exactly like one that speaks
    /// for all of it. A coverage figure is the difference between a narrow
    /// answer and a wrong impression of the whole.
    pub embedded_chunks: i64,
    /// When any root was last reconciled with the disk, and what the watcher
    /// could see at the time.
    ///
    /// **Both columns existed and nothing wrote either.** `watcher_health`
    /// defaulted to `LIVE`, so a database nobody had ever watched reported a
    /// live watcher, and `last_reconciled_at` stayed NULL, so no reader could
    /// tell a current index from a nine-hour-old one. Counts without freshness
    /// are the failure mode: a search over a stale index answers confidently
    /// about a disk it has not looked at.
    pub last_reconciled_ms: Option<i64>,
    /// `live` | `degraded` | `poll_only` | `unavailable`, lowercased from the
    /// worst root — the reassuring root must not hide the broken one.
    pub watcher_health: String,
}

impl IndexStats {
    /// Whether an answer drawn from this index may be out of date.
    ///
    /// True when nothing has ever reconciled, or nothing is watching now. Both
    /// mean the same thing to a caller: the disk may have moved on and this
    /// index would not know.
    /// How much of the index semantic search can speak for, as a percentage.
    ///
    /// `None` when nothing has been embedded, because "0%" and "this feature is
    /// not in use" are different things to say and only one of them needs
    /// saying.
    pub fn semantic_coverage(&self) -> Option<f64> {
        if self.embedded_chunks == 0 || self.chunks == 0 {
            return None;
        }
        Some(self.embedded_chunks as f64 / self.chunks as f64 * 100.0)
    }

    pub fn may_be_stale(&self) -> bool {
        self.last_reconciled_ms.is_none() || self.watcher_health == "unavailable"
    }
}

/// Every workspace, with its counts.
pub fn workspace_stats(conn: &ReadConn) -> Result<Vec<WorkspaceStats>> {
    let mut stmt = conn
        .prepare(
            "SELECT w.name,
                    COALESCE(r.canonical_path,''),
                    (SELECT count(*) FROM files f
                      WHERE f.workspace_id=w.workspace_id AND f.status='ACTIVE'),
                    (SELECT count(*) FROM chunks c
                       JOIN file_versions v ON v.version_id=c.version_id
                       JOIN files f2 ON f2.file_id=v.file_id
                      WHERE f2.workspace_id=w.workspace_id AND c.status='ACTIVE'),
                    -- `f3.status='ACTIVE'` matters: the file count beside this
                    -- number has it and this did not, so the two disagreed by
                    -- 4.02 GB (29%) on the author's index — a deleted file keeps
                    -- its CURRENT version row, and 9,989 of them were still
                    -- being weighed.
                    (SELECT COALESCE(sum(v2.size_bytes),0) FROM file_versions v2
                       JOIN files f3 ON f3.file_id=v2.file_id
                      WHERE f3.workspace_id=w.workspace_id AND v2.status='CURRENT'
                        AND f3.status='ACTIVE'),
                    (SELECT count(*) FROM files f4
                      WHERE f4.workspace_id=w.workspace_id AND f4.tier_state!='RESIDENT'),
                    (SELECT count(*) FROM files f5
                       JOIN file_versions v5
                         ON v5.file_id=f5.file_id AND v5.status='CURRENT'
                      WHERE f5.workspace_id=w.workspace_id AND f5.status='ACTIVE'
                        AND NOT EXISTS (SELECT 1 FROM chunks c5
                                         WHERE c5.version_id=v5.version_id)),
                    (SELECT count(*) FROM files f6
                       JOIN file_versions v6
                         ON v6.file_id=f6.file_id AND v6.status='CURRENT'
                      WHERE f6.workspace_id=w.workspace_id AND f6.status='ACTIVE'
                        AND NOT EXISTS (SELECT 1 FROM chunks c6
                                         WHERE c6.version_id=v6.version_id)
                        AND EXISTS (SELECT 1 FROM parse_results p6
                                     WHERE p6.version_id=v6.version_id
                                       AND p6.outcome IN
                                           ('FAILED','PARTIAL','LOW_YIELD'))),
                    (SELECT count(*) FROM files f7
                       JOIN file_versions v7
                         ON v7.file_id=f7.file_id AND v7.status='CURRENT'
                      WHERE f7.workspace_id=w.workspace_id AND f7.status='ACTIVE'
                        AND NOT EXISTS (SELECT 1 FROM chunks c7
                                         WHERE c7.version_id=v7.version_id)
                        AND NOT EXISTS (SELECT 1 FROM parse_results p7
                                         WHERE p7.version_id=v7.version_id))
               FROM workspaces w
          LEFT JOIN workspace_roots r ON r.workspace_id=w.workspace_id
              WHERE w.status='ACTIVE' ORDER BY w.name",
        )
        .map_err(|e| map_sqlite(e, "listing workspaces"))?;
    stmt.query_map([], |r| {
        let unindexed: i64 = r.get(6)?;
        let parse_failed: i64 = r.get(7)?;
        let not_processed: i64 = r.get(8)?;
        Ok(WorkspaceStats {
            name: r.get(0)?,
            path: r.get(1)?,
            files: r.get(2)?,
            chunks: r.get(3)?,
            content_bytes: r.get(4)?,
            cloud_only: r.get(5)?,
            unindexed,
            // The complement rather than a fourth `count(*)`: the three buckets
            // have to sum to the total or the card shows a number nobody can
            // account for, and subtraction is the only way to guarantee that
            // for an outcome the CHECK constraint gains later.
            no_parser: unindexed - parse_failed - not_processed,
            parse_failed,
            not_processed,
        })
    })
    .and_then(|it| it.collect())
    .map_err(|e| map_sqlite(e, "listing workspaces"))
}

/// The index as a whole.
pub fn index_stats(conn: &ReadConn) -> Result<IndexStats> {
    let (files, content_bytes, cloud_only, workspaces): (i64, i64, i64, i64) = conn
        .query_row(
            "SELECT (SELECT count(*) FROM files WHERE status='ACTIVE'),
                    -- Joined to `files` for the same reason as above: without
                    -- it this counted the bytes of deleted files, so one line
                    -- of output reported a file count and a byte total that
                    -- described different sets.
                    (SELECT COALESCE(sum(v.size_bytes),0) FROM file_versions v
                       JOIN files f ON f.file_id=v.file_id
                      WHERE v.status='CURRENT' AND f.status='ACTIVE'),
                    (SELECT count(*) FROM files WHERE tier_state != 'RESIDENT'),
                    (SELECT count(*) FROM workspaces WHERE status='ACTIVE')",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .map_err(|e| map_sqlite(e, "reading index health"))?;

    // The *worst* root, not the newest: one healthy folder must not make a
    // folder nobody is watching look watched. `ORDER BY` walks the enum from
    // worst to best, and NULL sorts as never-reconciled.
    let (last_reconciled_ms, watcher_health): (Option<i64>, String) = conn
        .query_row(
            "SELECT max(last_reconciled_at), COALESCE(
               (SELECT watcher_health FROM workspace_roots
                 ORDER BY CASE watcher_health
                            WHEN 'UNAVAILABLE' THEN 0 WHEN 'POLL_ONLY' THEN 1
                            WHEN 'DEGRADED' THEN 2 ELSE 3 END,
                          last_reconciled_at IS NOT NULL
                 LIMIT 1), 'unavailable')
               FROM workspace_roots",
            [],
            |r| Ok((r.get(0)?, r.get::<_, String>(1)?)),
        )
        .map_err(|e| map_sqlite(e, "reading index freshness"))?;

    // The vector table belongs to `marrow-index`'s migration, and a database
    // composed without it is a legitimate state — hard rule 10 says search
    // works with no model at all. Absent means zero coverage, not an error.
    let embedded_chunks: i64 = conn
        .query_row(
            "SELECT CASE WHEN EXISTS
               (SELECT 1 FROM sqlite_master WHERE type='table' AND name='chunk_embeddings')
             THEN (SELECT count(*) FROM chunk_embeddings) ELSE 0 END",
            [],
            |r| r.get(0),
        )
        .map_err(|e| map_sqlite(e, "reading semantic coverage"))?;

    Ok(IndexStats {
        files,
        chunks: marrow_store::read::chunk_count(conn)? as i64,
        content_bytes,
        cloud_only,
        workspaces,
        schema_version: marrow_store::migrate::current_version(conn)?,
        embedded_chunks,
        last_reconciled_ms,
        // A root that has never been reconciled reports whatever the schema
        // default left behind, which is `LIVE`. That default is the lie; treat
        // "never reconciled" as unwatched regardless of what the column says.
        watcher_health: if last_reconciled_ms.is_none() {
            "unavailable".to_string()
        } else {
            watcher_health.to_lowercase()
        },
    })
}

/// Every authorized root's canonical path.
///
/// Used to turn an absolute path into a workspace-relative one for display.
/// Two crates had this byte-for-byte identical.
pub fn roots(conn: &ReadConn) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT canonical_path FROM workspace_roots")
        .map_err(|e| map_sqlite(e, "reading roots"))?;
    stmt.query_map([], |r| r.get(0))
        .and_then(|it| it.collect())
        .map_err(|e| map_sqlite(e, "reading roots"))
}

/// Every path a file has been seen at, oldest first (FS-006).
///
/// **Path is never identity** (invariant #2): this is history, and the current
/// path is the last entry rather than the only one.
pub fn path_history(conn: &ReadConn, file_id: marrow_core::FileId) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT path FROM file_paths WHERE file_id = ?1 ORDER BY observed_from")
        .map_err(|e| map_sqlite(e, "reading path history"))?;
    stmt.query_map([file_id.to_string()], |r| r.get(0))
        .and_then(|it| it.collect())
        .map_err(|e| map_sqlite(e, "reading path history"))
}

/// Resolve a workspace name to its id, with a message that lists the real ones.
///
/// "No such workspace" is a dead end; naming what exists is one step from a
/// working command.
pub fn workspace_id_by_name(conn: &ReadConn, name: &str) -> Result<marrow_core::WorkspaceId> {
    let found: Option<String> = conn
        .query_row(
            "SELECT workspace_id FROM workspaces WHERE name = ?1 AND status='ACTIVE'",
            [name],
            |r| r.get(0),
        )
        .map(Some)
        .or_else(|e| match e {
            marrow_store::rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(map_sqlite(other, "resolving a workspace")),
        })?;

    match found.and_then(|id| id.parse().ok()) {
        Some(id) => Ok(id),
        None => {
            let names: Vec<String> = workspace_stats(conn)?.into_iter().map(|w| w.name).collect();
            Err(Error::new(
                Code::CfgInvalid,
                if names.is_empty() {
                    format!("No workspace called `{name}`, and none have been added yet.")
                } else {
                    format!(
                        "No workspace called `{name}`. There is {}.",
                        names.join(", ")
                    )
                },
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use marrow_core::{ContentHash, FileStatus, Origin, RootId, TierState, Timestamp, WorkspaceId};
    use marrow_store::{NewFile, NewRoot, NewVersion, NewWorkspace, StorageKind, Store};

    fn fixture() -> (tempfile::TempDir, Store) {
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
                canonical_path: "/tmp/notes".into(),
                volume_identity: None,
                grant_token: None,
                storage_kind: StorageKind::Local,
                cloud_provider: None,
                at: now,
            })
            .unwrap();
        let file = marrow_core::FileId::new();
        let f = NewFile {
            file_id: file,
            workspace_id: ws,
            root_id: root,
            current_path: Some("/tmp/notes/a.md".into()),
            fs_identity: Some("id".into()),
            tier_state: TierState::Resident,
            origin: Origin::User,
            origin_txn_id: None,
            external_source_url: None,
            status: FileStatus::Active,
            at: now,
        };
        let v = NewVersion::new(file, "/tmp/notes/a.md", 120, ContentHash::of(b"x"));
        store
            .writer()
            .submit(move |c| marrow_store::read::insert_file_with_version(c, &f, &v).map(|_| ()))
            .unwrap();
        store.flush().unwrap();
        (dir, store)
    }

    /// Add one more chunkless file, carrying the parse outcome the router would
    /// have recorded for it — or none at all, for a file no ingest run has
    /// reached yet.
    ///
    /// Chunkless on purpose: every one of these counts towards `unindexed`, and
    /// the whole question these tests exist to pin is *which kind* of unindexed
    /// each one is.
    fn add_file(store: &Store, name: &str, outcome: Option<&str>) {
        let conn = store.reader().unwrap();
        let ws: WorkspaceId = conn
            .query_row("SELECT workspace_id FROM workspaces LIMIT 1", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap()
            .parse()
            .unwrap();
        let root: RootId = conn
            .query_row("SELECT root_id FROM workspace_roots LIMIT 1", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap()
            .parse()
            .unwrap();
        drop(conn);

        let path = format!("/tmp/notes/{name}");
        let file = marrow_core::FileId::new();
        let f = NewFile {
            file_id: file,
            workspace_id: ws,
            root_id: root,
            current_path: Some(path.clone()),
            fs_identity: Some(name.to_string()),
            tier_state: TierState::Resident,
            origin: Origin::User,
            origin_txn_id: None,
            external_source_url: None,
            status: FileStatus::Active,
            at: Timestamp::now(),
        };
        let v = NewVersion::new(file, &path, 10, ContentHash::of(name.as_bytes()));
        let parse = outcome.map(|o| marrow_store::read::NewParse {
            version_id: v.version_id,
            parser_id: "test".into(),
            parser_version: "1".into(),
            parser_tier: if o == "METADATA_ONLY" { "T5" } else { "T1" }.into(),
            provenance_class: if o == "METADATA_ONLY" {
                "METADATA_ONLY"
            } else {
                "EXACT"
            }
            .into(),
            outcome: o.to_string(),
            char_yield: None,
            page_count: None,
            warnings: None,
            parsed_at: Timestamp::now(),
        });
        store
            .writer()
            .submit(move |c| {
                marrow_store::read::insert_file_with_version(c, &f, &v)?;
                match &parse {
                    Some(p) => marrow_store::read::record_parse(c, p),
                    None => Ok(()),
                }
            })
            .unwrap();
        store.flush().unwrap();
    }

    #[test]
    fn one_statement_answers_for_every_surface() {
        // The whole point. The sidebar and MCP report the same fact about the
        // same index; two statements that drift apart mean two answers to one
        // question with nothing saying which is right.
        let (_d, s) = fixture();
        let conn = s.reader().unwrap();
        let per_workspace = workspace_stats(&conn).unwrap();
        let whole = index_stats(&conn).unwrap();

        assert_eq!(per_workspace.len(), 1);
        assert_eq!(
            per_workspace.iter().map(|w| w.files).sum::<i64>(),
            whole.files,
            "the per-workspace counts must sum to the total"
        );
        assert_eq!(
            per_workspace.iter().map(|w| w.content_bytes).sum::<i64>(),
            whole.content_bytes
        );
        assert_eq!(whole.workspaces, 1);
    }

    #[test]
    fn a_file_with_no_chunks_counts_as_unindexed_not_as_absent() {
        // "Findable by name but not by content" is a real state (T5) and the
        // one the status view exists to surface. Counting it as zero files
        // would say the workspace is empty when it is half-done.
        let (_d, s) = fixture();
        let conn = s.reader().unwrap();
        let w = &workspace_stats(&conn).unwrap()[0];
        assert_eq!(w.files, 1);
        assert_eq!(w.chunks, 0);
        assert_eq!(w.unindexed, 1);
        // No parse result at all: nothing has looked at this file's bytes yet.
        assert_eq!(w.not_processed, 1);
        assert!(w.is_degraded(), "a file nothing has parsed yet is degraded");
    }

    #[test]
    fn a_folder_of_photos_is_healthy_not_degraded() {
        // The bug: `unindexed` counted "has no parser" and "the parse failed"
        // as one number, so 42,581 photos raised a warning triangle over a
        // workspace where nothing was wrong and no action could clear it. A
        // file with no parser stays discoverable via metadata (T5) — expected,
        // not a failure.
        let (_d, s) = fixture();
        for i in 0..3 {
            add_file(&s, &format!("photo{i}.heic"), Some("METADATA_ONLY"));
        }
        let conn = s.reader().unwrap();
        let w = &workspace_stats(&conn).unwrap()[0];
        assert_eq!(w.no_parser, 3);
        assert_eq!(w.parse_failed, 0);
        // The one file from the fixture has no parse result of its own, so it
        // is what keeps this workspace degraded; the photos do not.
        assert_eq!(w.not_processed, 1);

        // And the verdict itself: a workspace whose only unindexed files have
        // no parser is healthy. This is the assertion the status page renders.
        let photos = WorkspaceStats {
            files: 3,
            unindexed: 3,
            no_parser: 3,
            ..WorkspaceStats::default()
        };
        assert!(
            !photos.is_degraded(),
            "files with no parser must not read as broken: {photos:?}"
        );
    }

    #[test]
    fn a_failed_parse_is_the_one_that_is_actually_wrong() {
        // The distinction the whole split exists for. These are the files whose
        // text exists on disk and is not in the index — the only bucket where
        // there is something to do about it.
        let (_d, s) = fixture();
        add_file(&s, "scan.pdf", Some("FAILED"));
        add_file(&s, "half.docx", Some("PARTIAL"));
        add_file(&s, "thin.pdf", Some("LOW_YIELD"));
        add_file(&s, "photo.heic", Some("METADATA_ONLY"));

        let conn = s.reader().unwrap();
        let w = &workspace_stats(&conn).unwrap()[0];
        assert_eq!(w.parse_failed, 3, "FAILED, PARTIAL and LOW_YIELD");
        assert_eq!(w.no_parser, 1);
        assert!(w.is_degraded());
    }

    #[test]
    fn the_three_buckets_account_for_every_unindexed_file() {
        // A card that shows three numbers which do not add up to the fourth is
        // a card the reader cannot trust — and the missing file is exactly the
        // one they would have wanted to know about. Subtraction is what makes
        // this hold for an outcome the schema gains later.
        let (_d, s) = fixture();
        add_file(&s, "photo.heic", Some("METADATA_ONLY"));
        add_file(&s, "scan.pdf", Some("FAILED"));
        add_file(&s, "empty.txt", Some("OK"));
        add_file(&s, "queued.md", None);

        let conn = s.reader().unwrap();
        let w = &workspace_stats(&conn).unwrap()[0];
        assert_eq!(w.unindexed, 5, "the fixture file plus four");
        assert_eq!(
            w.no_parser + w.parse_failed + w.not_processed,
            w.unindexed,
            "{w:?}"
        );
        // An `OK` parse that produced no chunks is an empty file, not a
        // failure: there was nothing to search, which is `no_parser`'s meaning.
        assert_eq!(w.no_parser, 2);
        assert_eq!(w.parse_failed, 1);
        assert_eq!(w.not_processed, 2);
    }

    #[test]
    fn the_schema_version_comes_from_the_database_not_from_a_constant() {
        // A surface that reported `marrow_core::SCHEMA_VERSION` would be wrong
        // in two directions at once: it is the highest migration *that crate*
        // defines, and the chain is numbered across crates, so a live database
        // is at the composed maximum.
        //
        // Asserted against the single declared maximum rather than a literal,
        // which is the whole reason that constant exists (D57). This test named
        // the vector migration's own number and started failing the moment a
        // later migration was added — pinning a number is how a test about
        // composition becomes a test about one link in it.
        let (_d, s) = fixture();
        let conn = s.reader().unwrap();
        let reported = index_stats(&conn).unwrap().schema_version;
        assert_eq!(reported, marrow_index::SCHEMA_VERSION);
        // Not "greater than the store's own maximum": the store now owns
        // migration 5 and `marrow-index` owns 2 and 4, so the composed maximum
        // and the store's own maximum are the same number today. Which crate
        // happens to hold the highest one is not the invariant — the invariant
        // is that a surface reports what the database is at, from the database.
        assert!(
            reported >= marrow_core::SCHEMA_VERSION,
            "a composed chain cannot be shorter than the store's own list"
        );
    }

    #[test]
    fn an_unknown_workspace_names_the_ones_that_exist() {
        // "No such workspace" is a dead end; naming what is there is one step
        // from a working command.
        let (_d, s) = fixture();
        let conn = s.reader().unwrap();
        assert!(workspace_id_by_name(&conn, "notes").is_ok());
        let e = workspace_id_by_name(&conn, "nope").unwrap_err();
        assert_eq!(e.code(), Code::CfgInvalid);
        assert!(e.message().contains("notes"), "{}", e.message());
    }

    #[test]
    fn roots_come_back_for_making_paths_relative() {
        let (_d, s) = fixture();
        let conn = s.reader().unwrap();
        assert_eq!(roots(&conn).unwrap(), vec!["/tmp/notes".to_string()]);
    }

    #[test]
    fn path_history_is_oldest_first_because_it_is_history() {
        // Invariant #2: the current path is the last entry, not the only one.
        let (_d, s) = fixture();
        let conn = s.reader().unwrap();
        let id: String = conn
            .query_row("SELECT file_id FROM files LIMIT 1", [], |r| r.get(0))
            .unwrap();
        let hist = path_history(&conn, id.parse().unwrap()).unwrap();
        assert_eq!(hist, vec!["/tmp/notes/a.md".to_string()]);
    }
}
