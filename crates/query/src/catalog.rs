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
    pub unindexed: i64,
}

impl WorkspaceStats {
    /// Whether anything about this workspace should be shown as degraded.
    ///
    /// Defined here rather than in each surface, so the sidebar and MCP cannot
    /// disagree about what "healthy" means.
    pub fn is_degraded(&self) -> bool {
        self.files == 0 || self.unindexed > 0 || self.cloud_only > 0
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
                    (SELECT COALESCE(sum(v2.size_bytes),0) FROM file_versions v2
                       JOIN files f3 ON f3.file_id=v2.file_id
                      WHERE f3.workspace_id=w.workspace_id AND v2.status='CURRENT'),
                    (SELECT count(*) FROM files f4
                      WHERE f4.workspace_id=w.workspace_id AND f4.tier_state!='RESIDENT'),
                    (SELECT count(*) FROM files f5
                       JOIN file_versions v5
                         ON v5.file_id=f5.file_id AND v5.status='CURRENT'
                      WHERE f5.workspace_id=w.workspace_id AND f5.status='ACTIVE'
                        AND NOT EXISTS (SELECT 1 FROM chunks c5
                                         WHERE c5.version_id=v5.version_id))
               FROM workspaces w
          LEFT JOIN workspace_roots r ON r.workspace_id=w.workspace_id
              WHERE w.status='ACTIVE' ORDER BY w.name",
        )
        .map_err(|e| map_sqlite(e, "listing workspaces"))?;
    stmt.query_map([], |r| {
        Ok(WorkspaceStats {
            name: r.get(0)?,
            path: r.get(1)?,
            files: r.get(2)?,
            chunks: r.get(3)?,
            content_bytes: r.get(4)?,
            cloud_only: r.get(5)?,
            unindexed: r.get(6)?,
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
                    (SELECT COALESCE(sum(size_bytes),0) FROM file_versions
                      WHERE status='CURRENT'),
                    (SELECT count(*) FROM files WHERE tier_state != 'RESIDENT'),
                    (SELECT count(*) FROM workspaces WHERE status='ACTIVE')",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .map_err(|e| map_sqlite(e, "reading index health"))?;
    Ok(IndexStats {
        files,
        chunks: marrow_store::read::chunk_count(conn)? as i64,
        content_bytes,
        cloud_only,
        workspaces,
        schema_version: marrow_store::migrate::current_version(conn)?,
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
        assert!(w.is_degraded(), "a wholly unindexed workspace is degraded");
    }

    #[test]
    fn the_schema_version_comes_from_the_database_not_from_a_constant() {
        // A surface that reported `marrow_core::SCHEMA_VERSION` would be wrong
        // in two directions at once: it is the highest migration *that crate*
        // defines, and the chain is numbered across crates, so a live database
        // is at the composed maximum — 4, not 3.
        let (_d, s) = fixture();
        let conn = s.reader().unwrap();
        let reported = index_stats(&conn).unwrap().schema_version;
        assert_eq!(reported, marrow_index::vector::VECTOR_INDEX_VERSION);
        assert!(
            reported > marrow_core::SCHEMA_VERSION,
            "the composed chain goes further than the store's own list"
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
