//! Everything the index knows about one file, read once.
//!
//! **Three surfaces asked this question and two of them wrote the query.** MCP's
//! `file_info` and the desktop's `file_detail` carried the same twelve-line
//! SELECT, character for character, and the CLI had no answer at all. The
//! queries never disagreed; what diverged was what they *concluded* — MCP
//! checked whether the file was still on the disk and the desktop did not, so
//! the app offered to cite files that were gone.
//!
//! One read, one rule ([`crate::presence`]), three renderings.

use marrow_core::{Code, Error, Origin, Result};
use marrow_store::rusqlite::Connection;

use crate::presence::{self, Presence};

/// One file, as the index has it plus what the disk says now.
#[derive(Clone, Debug)]
pub struct Detail {
    pub path: String,
    pub file_id: String,
    pub workspace: String,
    /// `None` when no version row exists yet — recorded, never read.
    pub size_bytes: Option<i64>,
    pub content_hash: Option<String>,
    pub mime: Option<String>,
    pub modified_ms: Option<i64>,
    pub versions: i64,
    pub chunks: i64,
    pub origin: Origin,
    /// **Path is never identity.** How a rename stays the same file, and the
    /// difference between a file that moved and one that was destroyed.
    pub previous_paths: Vec<String>,
    /// What is true now, rather than at the last sweep.
    pub presence: Presence,
}

/// Read one file by its current path.
///
/// Refuses a path the index does not have. A file it *does* have but the disk
/// no longer does is **reported, not refused** — a refusal is indistinguishable
/// from "no such path", which is a different fact and the wrong next move, and
/// it would throw away the id, the hash and the path history that say whether
/// the content was renamed or destroyed.
pub fn detail(conn: &Connection, path: &str) -> Result<Detail> {
    let row = conn
        .query_row(
            "SELECT f.file_id, f.tier_state, f.origin, w.name,
                    v.size_bytes, v.content_hash, v.mime, v.mtime_ms,
                    (SELECT count(*) FROM file_versions x WHERE x.file_id = f.file_id),
                    (SELECT count(*) FROM chunks c WHERE c.version_id = v.version_id)
               FROM files f
               JOIN workspaces w ON w.workspace_id = f.workspace_id
          LEFT JOIN file_versions v
                 ON v.file_id = f.file_id AND v.status = 'CURRENT'
              WHERE f.current_path = ?1 AND f.status = 'ACTIVE' LIMIT 1",
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
        .ok();

    let Some((file_id, tier, origin, workspace, size, hash, mime, mtime, versions, chunks)) = row
    else {
        return Err(Error::new(
            Code::FsNotFound,
            "That file is not indexed. Add its folder as a workspace, then run an index."
                .to_string(),
        ));
    };

    let origin = if origin == "SELF" {
        Origin::SelfWritten
    } else {
        Origin::User
    };

    let mut stmt = conn
        .prepare("SELECT path FROM file_paths WHERE file_id = ?1 ORDER BY observed_from")
        .map_err(|e| marrow_store::map_sqlite(e, "reading path history"))?;
    let history: Vec<String> = stmt
        .query_map([&file_id], |r| r.get(0))
        .and_then(|it| it.collect())
        .map_err(|e| marrow_store::map_sqlite(e, "reading path history"))?;

    Ok(Detail {
        path: path.to_string(),
        file_id,
        workspace,
        size_bytes: size,
        content_hash: hash,
        mime,
        modified_ms: mtime,
        versions,
        chunks,
        origin,
        // The current path is not its own history.
        previous_paths: history.into_iter().filter(|p| p != path).collect(),
        presence: presence::check(path, &tier, origin, chunks),
    })
}
