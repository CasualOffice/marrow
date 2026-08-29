//! Application state: an open store and index, shared by every command.

use marrow_core::Result;
use marrow_index::{Fts5Index, TextIndex, TextQuery};
use marrow_store::Store;

use crate::commands::{to_hit, FileDetail, IndexHealth, SearchHit, SearchResponse, WorkspaceRow};

/// Everything the commands need. Opened once at startup.
pub struct Core {
    store: Store,
    index: Fts5Index,
}

impl Core {
    pub fn open(path: std::path::PathBuf) -> Result<Self> {
        // The composition root assembles the migration chain: `index` depends
        // on `store`, so store cannot reference it back without a cycle.
        let store = Store::open_with_migrations(path, &[marrow_index::fts5::MIGRATION])?;
        let index = Fts5Index::open(&store)?;
        Ok(Self { store, index })
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<SearchResponse> {
        let started = std::time::Instant::now();
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok(SearchResponse {
                query: query.to_string(),
                total: 0,
                elapsed_ms: 0,
                hits: Vec::new(),
                branches: vec!["lexical".into()],
            });
        }

        let q = TextQuery::new(trimmed).limit(limit.clamp(1, 200));
        let raw = self.index.search(&q)?;
        let roots = self.roots()?;
        let hits: Vec<SearchHit> = raw
            .iter()
            .enumerate()
            .map(|(i, h)| to_hit(i + 1, h, &roots))
            .collect();

        Ok(SearchResponse {
            query: trimmed.to_string(),
            total: hits.len(),
            elapsed_ms: started.elapsed().as_millis() as u64,
            hits,
            branches: vec!["lexical".into()],
        })
    }

    pub fn workspaces(&self) -> Result<Vec<WorkspaceRow>> {
        let conn = self.store.reader()?;
        let mut stmt = conn
            .prepare(
                "SELECT w.name, COALESCE(r.canonical_path,''),
                        (SELECT count(*) FROM files f
                          WHERE f.workspace_id = w.workspace_id AND f.status='ACTIVE')
                   FROM workspaces w
              LEFT JOIN workspace_roots r ON r.workspace_id = w.workspace_id
                  WHERE w.status='ACTIVE' ORDER BY w.name",
            )
            .map_err(|e| marrow_store::map_sqlite(e, "listing workspaces"))?;
        stmt.query_map([], |r| {
            Ok(WorkspaceRow {
                name: r.get(0)?,
                path: r.get(1)?,
                files: r.get(2)?,
            })
        })
        .and_then(|it| it.collect())
        .map_err(|e| marrow_store::map_sqlite(e, "listing workspaces"))
    }

    pub fn health(&self) -> Result<IndexHealth> {
        let conn = self.store.reader()?;
        let (files, bytes, cloud_only): (i64, i64, i64) = conn
            .query_row(
                "SELECT (SELECT count(*) FROM files WHERE status='ACTIVE'),
                        (SELECT COALESCE(sum(size_bytes),0) FROM file_versions WHERE status='CURRENT'),
                        (SELECT count(*) FROM files WHERE tier_state != 'RESIDENT')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(|e| marrow_store::map_sqlite(e, "reading index health"))?;
        Ok(IndexHealth {
            files,
            chunks: marrow_store::read::chunk_count(&conn)?,
            content_bytes: bytes,
            cloud_only,
            schema_version: self.store.schema_version(),
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
    pub fn read_region(&self, path: &str, around: Option<u32>) -> Result<Vec<String>> {
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
        Ok(body
            .lines()
            .enumerate()
            .filter(|(i, _)| {
                let n = *i as u32 + 1;
                n >= from && n <= to
            })
            .map(|(_, l)| l.to_string())
            .take(MAX_LINES)
            .collect())
    }

    fn roots(&self) -> Result<Vec<String>> {
        let conn = self.store.reader()?;
        let mut stmt = conn
            .prepare("SELECT canonical_path FROM workspace_roots")
            .map_err(|e| marrow_store::map_sqlite(e, "reading roots"))?;
        stmt.query_map([], |r| r.get(0))
            .and_then(|it| it.collect())
            .map_err(|e| marrow_store::map_sqlite(e, "reading roots"))
    }
}
