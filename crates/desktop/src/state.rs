//! Application state: an open store and index, shared by every command.

use marrow_core::Result;
use marrow_index::{Fts5Index, TextIndex, TextQuery};
use marrow_store::Store;

use crate::commands::{
    to_hit, FileDetail, FileRow, IndexHealth, Region, SearchHit, SearchResponse, WorkspaceRow,
};

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
        let q = TextQuery::new(trimmed)
            .mode(marrow_index::MatchMode::Prefix)
            .limit(capped);
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

    pub fn workspaces(&self) -> Result<Vec<WorkspaceRow>> {
        let conn = self.store.reader()?;
        // One query per workspace rather than a global row: the sidebar has to
        // show WHICH workspace is degraded, and a single total cannot.
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
            .map_err(|e| marrow_store::map_sqlite(e, "listing workspaces"))?;
        stmt.query_map([], |r| {
            Ok(WorkspaceRow {
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
        let conn = self.store.reader()?;
        let mut stmt = conn
            .prepare("SELECT canonical_path FROM workspace_roots")
            .map_err(|e| marrow_store::map_sqlite(e, "reading roots"))?;
        stmt.query_map([], |r| r.get(0))
            .and_then(|it| it.collect())
            .map_err(|e| marrow_store::map_sqlite(e, "reading roots"))
    }
}
