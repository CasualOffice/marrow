//! Read connections and the M1 query set.
//!
//! Every function here takes a plain `&Connection` on purpose. `Transaction`
//! and `Savepoint` both deref to it, so the *same* function body runs inside
//! the writer actor's batch (for the ones that write) and on any reader
//! connection (for the ones that only read). There is no second implementation
//! to drift.
//!
//! Readers are unlimited — that is what WAL buys — and are opened `query_only`,
//! so "never open a second write connection" (Part 2 §50) is enforced by SQLite
//! rather than by discipline.
//!
//! Two conversions happen at this boundary and nowhere else:
//! IDs are `TEXT` ULID, timestamps are `INTEGER` epoch millis (§106.1).

use std::str::FromStr;
use std::time::Duration;

use marrow_core::{
    ChunkId, Code, ContentHash, Error, FileId, FileStatus, JobId, Origin, Result, RootId,
    TierState, Timestamp, VersionId, VersionStatus, WorkspaceId,
};
use rusqlite::{params, types::Type, Connection, OptionalExtension, Row};

use crate::Location;

// ---------------------------------------------------------------- read handle

/// A read-only connection. Open as many as you like.
#[derive(Debug)]
pub struct ReadConn {
    conn: Connection,
}

impl ReadConn {
    pub(crate) fn open(loc: &Location) -> Result<Self> {
        let conn = loc.open()?;
        crate::schema::apply_reader_pragmas(&conn)?;
        Ok(Self { conn })
    }

    /// The underlying connection, for queries this module does not cover yet.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }
}

impl std::ops::Deref for ReadConn {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self.conn
    }
}

// ------------------------------------------------------------- SQL enum codecs
//
// §106.1: enums are TEXT with a CHECK, "readable in a debugger". The domain
// types spell them in snake_case for JSON, the schema in SCREAMING_CASE, so the
// mapping is explicit in both directions rather than a `to_uppercase()` that
// would quietly produce 'SELF_WRITTEN' and trip a CHECK at 3am.

pub fn tier_state_sql(t: TierState) -> &'static str {
    match t {
        TierState::Resident => "RESIDENT",
        TierState::Placeholder => "PLACEHOLDER",
        TierState::Hydrating => "HYDRATING",
        TierState::Unavailable => "UNAVAILABLE",
    }
}

fn tier_state_of(s: &str) -> Option<TierState> {
    Some(match s {
        "RESIDENT" => TierState::Resident,
        "PLACEHOLDER" => TierState::Placeholder,
        "HYDRATING" => TierState::Hydrating,
        "UNAVAILABLE" => TierState::Unavailable,
        _ => return None,
    })
}

pub fn origin_sql(o: Origin) -> &'static str {
    match o {
        Origin::User => "USER",
        Origin::SelfWritten => "SELF",
    }
}

fn origin_of(s: &str) -> Option<Origin> {
    Some(match s {
        "USER" => Origin::User,
        "SELF" => Origin::SelfWritten,
        _ => return None,
    })
}

pub fn file_status_sql(s: FileStatus) -> &'static str {
    match s {
        FileStatus::Active => "ACTIVE",
        FileStatus::Deleted => "DELETED",
        FileStatus::Excluded => "EXCLUDED",
        FileStatus::Error => "ERROR",
        FileStatus::Forgotten => "FORGOTTEN",
    }
}

fn file_status_of(s: &str) -> Option<FileStatus> {
    Some(match s {
        "ACTIVE" => FileStatus::Active,
        "DELETED" => FileStatus::Deleted,
        "EXCLUDED" => FileStatus::Excluded,
        "ERROR" => FileStatus::Error,
        "FORGOTTEN" => FileStatus::Forgotten,
        _ => return None,
    })
}

pub fn version_status_sql(s: VersionStatus) -> &'static str {
    match s {
        VersionStatus::Current => "CURRENT",
        VersionStatus::Historical => "HISTORICAL",
        VersionStatus::Tombstoned => "TOMBSTONED",
    }
}

fn version_status_of(s: &str) -> Option<VersionStatus> {
    Some(match s {
        "CURRENT" => VersionStatus::Current,
        "HISTORICAL" => VersionStatus::Historical,
        "TOMBSTONED" => VersionStatus::Tombstoned,
        _ => return None,
    })
}

/// `workspace_roots.storage_kind` (§106.3). Not in `marrow_core` — it is
/// storage vocabulary, and core is not allowed to grow for this crate's
/// convenience.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageKind {
    Local,
    Removable,
    Network,
    TieredCloud,
    RedirectedProfile,
}

impl StorageKind {
    pub fn as_sql(self) -> &'static str {
        match self {
            StorageKind::Local => "LOCAL",
            StorageKind::Removable => "REMOVABLE",
            StorageKind::Network => "NETWORK",
            StorageKind::TieredCloud => "TIERED_CLOUD",
            StorageKind::RedirectedProfile => "REDIRECTED_PROFILE",
        }
    }
}

/// `jobs.status` (§106.10, state machine in §111.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobStatus {
    Pending,
    Leased,
    Running,
    Done,
    /// Reserved by §106.10. M1 never writes it: a job that has exhausted its
    /// attempts goes straight to `Dead` so index health has one bucket to show,
    /// not two that mean nearly the same thing.
    Failed,
    Dead,
    Cancelled,
}

impl JobStatus {
    pub fn as_sql(self) -> &'static str {
        match self {
            JobStatus::Pending => "PENDING",
            JobStatus::Leased => "LEASED",
            JobStatus::Running => "RUNNING",
            JobStatus::Done => "DONE",
            JobStatus::Failed => "FAILED",
            JobStatus::Dead => "DEAD",
            JobStatus::Cancelled => "CANCELLED",
        }
    }

    fn of(s: &str) -> Option<JobStatus> {
        Some(match s {
            "PENDING" => JobStatus::Pending,
            "LEASED" => JobStatus::Leased,
            "RUNNING" => JobStatus::Running,
            "DONE" => JobStatus::Done,
            "FAILED" => JobStatus::Failed,
            "DEAD" => JobStatus::Dead,
            "CANCELLED" => JobStatus::Cancelled,
            _ => return None,
        })
    }
}

// ------------------------------------------------------------- row decode help

fn decode_err(column: &str, value: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{column}: unexpected value {value:?}"),
        )),
    )
}

fn id_of<T: FromStr<Err = ulid::DecodeError>>(s: &str, column: &str) -> rusqlite::Result<T> {
    s.parse::<T>().map_err(|_| decode_err(column, s))
}

fn ts(row: &Row<'_>, idx: usize) -> rusqlite::Result<Timestamp> {
    Ok(Timestamp::from_millis(row.get::<_, i64>(idx)?))
}

fn ts_opt(row: &Row<'_>, idx: usize) -> rusqlite::Result<Option<Timestamp>> {
    Ok(row.get::<_, Option<i64>>(idx)?.map(Timestamp::from_millis))
}

/// Wrap a rusqlite failure as a store error with a cause-and-action message.
fn q<T>(r: rusqlite::Result<T>, what: &str) -> Result<T> {
    r.map_err(|e| crate::map_sqlite(e, what))
}

// ------------------------------------------------------- workspaces and roots

/// A workspace to create or update. Identity is the **name**.
#[derive(Clone, Debug)]
pub struct NewWorkspace {
    pub workspace_id: WorkspaceId,
    pub name: String,
    pub at: Timestamp,
}

impl NewWorkspace {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            workspace_id: WorkspaceId::new(),
            name: name.into(),
            at: Timestamp::now(),
        }
    }
}

/// A consented root to create or update. Identity is `(workspace, path)`.
#[derive(Clone, Debug)]
pub struct NewRoot {
    pub root_id: RootId,
    pub workspace_id: WorkspaceId,
    /// Already canonicalized by the caller — invariant #5 is a scan-side
    /// concern, but storing a non-canonical path here would defeat it.
    pub canonical_path: String,
    pub volume_identity: Option<String>,
    pub grant_token: Option<String>,
    pub storage_kind: StorageKind,
    pub cloud_provider: Option<String>,
    pub at: Timestamp,
}

impl NewRoot {
    pub fn new(workspace_id: WorkspaceId, canonical_path: impl Into<String>) -> Self {
        Self {
            root_id: RootId::new(),
            workspace_id,
            canonical_path: canonical_path.into(),
            volume_identity: None,
            grant_token: None,
            storage_kind: StorageKind::Local,
            cloud_provider: None,
            at: Timestamp::now(),
        }
    }
}

/// Create the workspace, or update the existing one with this name.
///
/// Returns the id that is now in the database — the *existing* one on conflict,
/// because a workspace id is referenced by every file under it and must never
/// change out from under them.
pub fn upsert_workspace(conn: &Connection, ws: &NewWorkspace) -> Result<WorkspaceId> {
    q(
        conn.execute(
            "INSERT INTO workspaces (workspace_id, name, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT(name) DO UPDATE SET updated_at = excluded.updated_at",
            params![ws.workspace_id.to_string(), ws.name, ws.at.as_millis()],
        ),
        "Could not record the workspace in the index database.",
    )?;
    let existing: String = q(
        conn.query_row(
            "SELECT workspace_id FROM workspaces WHERE name = ?1",
            params![ws.name],
            |r| r.get(0),
        ),
        "Could not read back the workspace that was just written.",
    )?;
    parse_id(&existing, "workspaces.workspace_id")
}

/// Create the root, or update the existing one at this path.
/// Record that a root was just reconciled, and what its watcher can see.
///
/// **Both columns existed and nothing wrote either of them.** `watcher_health`
/// defaulted to `LIVE`, so every surface reported a live watcher whether or not
/// one had ever run, and `last_reconciled_at` stayed NULL, so nothing could
/// tell a fresh index from a nine-hour-old one. A stale index that presents
/// itself as current is worse than no index: a search over it answers
/// confidently about a disk it has not looked at.
pub fn mark_reconciled(
    conn: &Connection,
    root_id: RootId,
    health: WatcherHealth,
    at: Timestamp,
) -> Result<()> {
    conn.execute(
        "UPDATE workspace_roots SET watcher_health = ?2, last_reconciled_at = ?3
          WHERE root_id = ?1",
        rusqlite::params![root_id.to_string(), health.as_sql(), at.as_millis()],
    )
    .map_err(|e| crate::map_sqlite(e, "recording that a root was reconciled"))?;
    Ok(())
}

/// Record what the watcher can see, **without** claiming a reconciliation.
///
/// These were one call, and conflating them recreated the bug they were written
/// to fix. `last_reconciled_at` is the sole input to `may_be_stale`, so writing
/// it whenever a watcher reported its health meant: opening the app stamped
/// "just now" before the first sweep had walked anything; a sweep the user
/// cancelled eight seconds into a 41,000-file walk recorded a full
/// reconciliation; and a watcher degrading from `LIVE` to `POLL_ONLY` refreshed
/// the timestamp, so the index looked *fresher* at the moment its coverage got
/// worse.
///
/// Health is what the watcher can see. Freshness is what the walk established.
/// Only a completed walk may write the second.
pub fn mark_watcher_health(
    conn: &Connection,
    root_id: RootId,
    health: WatcherHealth,
) -> Result<()> {
    conn.execute(
        "UPDATE workspace_roots SET watcher_health = ?2 WHERE root_id = ?1",
        rusqlite::params![root_id.to_string(), health.as_sql()],
    )
    .map_err(|e| crate::map_sqlite(e, "recording watcher health"))?;
    Ok(())
}

/// What a watcher can currently see, as the schema spells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatcherHealth {
    Live,
    Degraded,
    PollOnly,
    /// No watcher is running. **The honest default**, and the one the schema
    /// should have had: a column that says `LIVE` because nobody set it is a
    /// lie that every reader downstream repeats.
    Unavailable,
}

impl WatcherHealth {
    pub fn as_sql(self) -> &'static str {
        match self {
            WatcherHealth::Live => "LIVE",
            WatcherHealth::Degraded => "DEGRADED",
            WatcherHealth::PollOnly => "POLL_ONLY",
            WatcherHealth::Unavailable => "UNAVAILABLE",
        }
    }
}

pub fn upsert_root(conn: &Connection, root: &NewRoot) -> Result<RootId> {
    q(
        conn.execute(
            "INSERT INTO workspace_roots
               (root_id, workspace_id, canonical_path, volume_identity, grant_token,
                storage_kind, cloud_provider, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(workspace_id, canonical_path) DO UPDATE SET
                volume_identity = excluded.volume_identity,
                grant_token     = excluded.grant_token,
                storage_kind    = excluded.storage_kind,
                cloud_provider  = excluded.cloud_provider",
            params![
                root.root_id.to_string(),
                root.workspace_id.to_string(),
                root.canonical_path,
                root.volume_identity,
                root.grant_token,
                root.storage_kind.as_sql(),
                root.cloud_provider,
                root.at.as_millis(),
            ],
        ),
        "Could not record the workspace root in the index database.",
    )?;
    let existing: String = q(
        conn.query_row(
            "SELECT root_id FROM workspace_roots WHERE workspace_id = ?1 AND canonical_path = ?2",
            params![root.workspace_id.to_string(), root.canonical_path],
            |r| r.get(0),
        ),
        "Could not read back the workspace root that was just written.",
    )?;
    parse_id(&existing, "workspace_roots.root_id")
}

// ------------------------------------------------------------------- files

/// A file's row in `files`. Identity is [`FileId`] — **never the path**.
#[derive(Clone, Debug)]
pub struct FileRow {
    pub file_id: FileId,
    pub workspace_id: WorkspaceId,
    pub root_id: RootId,
    pub current_path: Option<String>,
    pub fs_identity: Option<String>,
    pub current_version_id: Option<VersionId>,
    pub tier_state: TierState,
    pub origin: Origin,
    pub origin_txn_id: Option<String>,
    pub external_source_url: Option<String>,
    pub status: FileStatus,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

const FILE_COLUMNS: &str = "file_id, workspace_id, root_id, current_path, fs_identity,
     current_version_id, tier_state, origin, origin_txn_id, external_source_url,
     status, created_at, updated_at";

fn file_row(row: &Row<'_>) -> rusqlite::Result<FileRow> {
    let tier: String = row.get(6)?;
    let origin: String = row.get(7)?;
    let status: String = row.get(10)?;
    let version: Option<String> = row.get(5)?;
    Ok(FileRow {
        file_id: id_of(&row.get::<_, String>(0)?, "files.file_id")?,
        workspace_id: id_of(&row.get::<_, String>(1)?, "files.workspace_id")?,
        root_id: id_of(&row.get::<_, String>(2)?, "files.root_id")?,
        current_path: row.get(3)?,
        fs_identity: row.get(4)?,
        current_version_id: match version {
            Some(v) => Some(id_of(&v, "files.current_version_id")?),
            None => None,
        },
        tier_state: tier_state_of(&tier).ok_or_else(|| decode_err("files.tier_state", &tier))?,
        origin: origin_of(&origin).ok_or_else(|| decode_err("files.origin", &origin))?,
        origin_txn_id: row.get(8)?,
        external_source_url: row.get(9)?,
        status: file_status_of(&status).ok_or_else(|| decode_err("files.status", &status))?,
        created_at: ts(row, 11)?,
        updated_at: ts(row, 12)?,
    })
}

/// A file to insert. The caller mints the [`FileId`] so it can key derived work
/// on it before the write lands.
#[derive(Clone, Debug)]
pub struct NewFile {
    pub file_id: FileId,
    pub workspace_id: WorkspaceId,
    pub root_id: RootId,
    pub current_path: Option<String>,
    pub fs_identity: Option<String>,
    pub tier_state: TierState,
    pub origin: Origin,
    pub origin_txn_id: Option<String>,
    pub external_source_url: Option<String>,
    pub status: FileStatus,
    pub at: Timestamp,
}

impl NewFile {
    pub fn new(workspace_id: WorkspaceId, root_id: RootId, path: impl Into<String>) -> Self {
        Self {
            file_id: FileId::new(),
            workspace_id,
            root_id,
            current_path: Some(path.into()),
            fs_identity: None,
            tier_state: TierState::Resident,
            origin: Origin::User,
            origin_txn_id: None,
            external_source_url: None,
            status: FileStatus::Active,
            at: Timestamp::now(),
        }
    }
}

/// Look a file up by the path it currently occupies inside a root.
///
/// This is a *lookup*, not an identity: the answer is a [`FileId`], and every
/// caller must carry that forward rather than the path it started from
/// (invariant #2, FS-005).
pub fn find_file_by_path(
    conn: &Connection,
    root_id: RootId,
    path: &str,
) -> Result<Option<FileRow>> {
    q(
        conn.query_row(
            &format!("SELECT {FILE_COLUMNS} FROM files WHERE root_id = ?1 AND current_path = ?2"),
            params![root_id.to_string(), path],
            file_row,
        )
        .optional(),
        "Could not look up a file by path in the index database.",
    )
}

/// Look a file up by filesystem identity (inode+device, or the Windows file
/// id). This is what survives a rename the watcher did not see.
pub fn find_file_by_fs_identity(
    conn: &Connection,
    root_id: RootId,
    fs_identity: &str,
) -> Result<Option<FileRow>> {
    q(
        conn.query_row(
            &format!("SELECT {FILE_COLUMNS} FROM files WHERE root_id = ?1 AND fs_identity = ?2"),
            params![root_id.to_string(), fs_identity],
            file_row,
        )
        .optional(),
        "Could not look up a file by filesystem identity in the index database.",
    )
}

pub fn find_file_by_id(conn: &Connection, file_id: FileId) -> Result<Option<FileRow>> {
    q(
        conn.query_row(
            &format!("SELECT {FILE_COLUMNS} FROM files WHERE file_id = ?1"),
            params![file_id.to_string()],
            file_row,
        )
        .optional(),
        "Could not look up a file in the index database.",
    )
}

/// Insert a file and open its first path-history range.
pub fn insert_file(conn: &Connection, f: &NewFile) -> Result<FileId> {
    q(
        conn.execute(
            "INSERT INTO files
               (file_id, workspace_id, root_id, current_path, fs_identity, tier_state,
                origin, origin_txn_id, external_source_url, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
            params![
                f.file_id.to_string(),
                f.workspace_id.to_string(),
                f.root_id.to_string(),
                f.current_path,
                f.fs_identity,
                tier_state_sql(f.tier_state),
                origin_sql(f.origin),
                f.origin_txn_id,
                f.external_source_url,
                file_status_sql(f.status),
                f.at.as_millis(),
            ],
        ),
        "Could not record a file in the index database.",
    )?;
    // Path history starts at birth, so a rename never has to invent a first
    // observation (FS-006).
    if let Some(path) = &f.current_path {
        insert_path_row(conn, f.file_id, path, f.at)?;
    }
    tracing::trace!(file_id = %f.file_id, "file inserted");
    Ok(f.file_id)
}

// ------------------------------------------------------------------ versions

#[derive(Clone, Debug)]
pub struct VersionRow {
    pub version_id: VersionId,
    pub file_id: FileId,
    pub path_at_observation: String,
    pub size_bytes: i64,
    pub mtime_ms: Timestamp,
    pub content_hash: ContentHash,
    pub mime: Option<String>,
    pub language: Option<String>,
    pub observed_at: Timestamp,
    pub supersedes: Option<VersionId>,
    pub status: VersionStatus,
}

const VERSION_COLUMNS: &str = "version_id, file_id, path_at_observation, size_bytes, mtime_ms,
     content_hash, mime, language, observed_at, supersedes, status";

fn version_row(row: &Row<'_>) -> rusqlite::Result<VersionRow> {
    let hash: String = row.get(5)?;
    let supersedes: Option<String> = row.get(9)?;
    let status: String = row.get(10)?;
    Ok(VersionRow {
        version_id: id_of(&row.get::<_, String>(0)?, "file_versions.version_id")?,
        file_id: id_of(&row.get::<_, String>(1)?, "file_versions.file_id")?,
        path_at_observation: row.get(2)?,
        size_bytes: row.get(3)?,
        mtime_ms: ts(row, 4)?,
        content_hash: ContentHash::from_hex(&hash)
            .ok_or_else(|| decode_err("file_versions.content_hash", &hash))?,
        mime: row.get(6)?,
        language: row.get(7)?,
        observed_at: ts(row, 8)?,
        supersedes: match supersedes {
            Some(s) => Some(id_of(&s, "file_versions.supersedes")?),
            None => None,
        },
        status: version_status_of(&status)
            .ok_or_else(|| decode_err("file_versions.status", &status))?,
    })
}

/// A newly observed state of a file's bytes.
#[derive(Clone, Debug)]
pub struct NewVersion {
    pub version_id: VersionId,
    pub file_id: FileId,
    pub path_at_observation: String,
    pub size_bytes: i64,
    pub mtime_ms: Timestamp,
    pub content_hash: ContentHash,
    pub mime: Option<String>,
    pub language: Option<String>,
    pub observed_at: Timestamp,
}

impl NewVersion {
    pub fn new(
        file_id: FileId,
        path: impl Into<String>,
        size_bytes: i64,
        content_hash: ContentHash,
    ) -> Self {
        let now = Timestamp::now();
        Self {
            version_id: VersionId::new(),
            file_id,
            path_at_observation: path.into(),
            size_bytes,
            mtime_ms: now,
            content_hash,
            mime: None,
            language: None,
            observed_at: now,
        }
    }
}

/// Insert a version as the file's CURRENT one, demoting the previous CURRENT.
///
/// Order matters: the old row is demoted *before* the new one is inserted,
/// because `idx_versions_current` is a unique index and would otherwise reject
/// the insert. Both statements are in the caller's transaction, so a reader
/// never sees zero CURRENT versions either.
pub fn record_version(conn: &Connection, v: &NewVersion) -> Result<VersionId> {
    let previous: Option<String> = q(
        conn.query_row(
            "SELECT version_id FROM file_versions WHERE file_id = ?1 AND status = 'CURRENT'",
            params![v.file_id.to_string()],
            |r| r.get(0),
        )
        .optional(),
        "Could not read the current version of a file from the index database.",
    )?;

    if let Some(prev) = &previous {
        q(
            conn.execute(
                "UPDATE file_versions SET status = 'HISTORICAL' WHERE version_id = ?1",
                params![prev],
            ),
            "Could not supersede the previous version of a file.",
        )?;
    }

    q(
        conn.execute(
            "INSERT INTO file_versions
               (version_id, file_id, path_at_observation, size_bytes, mtime_ms, content_hash,
                mime, language, observed_at, supersedes, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'CURRENT')",
            params![
                v.version_id.to_string(),
                v.file_id.to_string(),
                v.path_at_observation,
                v.size_bytes,
                v.mtime_ms.as_millis(),
                v.content_hash.to_hex(),
                v.mime,
                v.language,
                v.observed_at.as_millis(),
                previous,
            ],
        ),
        "Could not record a new version of a file in the index database.",
    )?;

    q(
        conn.execute(
            "UPDATE files SET current_version_id = ?1, updated_at = ?2 WHERE file_id = ?3",
            params![
                v.version_id.to_string(),
                v.observed_at.as_millis(),
                v.file_id.to_string()
            ],
        ),
        "Could not point a file at its new current version.",
    )?;
    tracing::trace!(file_id = %v.file_id, version_id = %v.version_id, "version recorded");
    Ok(v.version_id)
}

/// Insert a file and its first version in one go.
pub fn insert_file_with_version(
    conn: &Connection,
    f: &NewFile,
    v: &NewVersion,
) -> Result<(FileId, VersionId)> {
    if f.file_id != v.file_id {
        return Err(Error::invariant(
            "A file and the version being inserted with it must carry the same file_id.",
        ));
    }
    let file_id = insert_file(conn, f)?;
    let version_id = record_version(conn, v)?;
    Ok((file_id, version_id))
}

/// Whether the content stage ever finished for this version.
///
/// **The version row is not the done-marker, and treating it as one loses
/// files.** `record_version` commits in its own writer batch; the parse result
/// and the chunks commit in a later one. A kill between the two — which happens
/// constantly during development, and is exactly what hard rule 7 is about —
/// leaves a version row whose `content_hash` matches the disk and which has no
/// chunks at all. The next run compares hashes, finds them equal, and skips the
/// file forever: permanently unsearchable, with nothing to notice it.
///
/// The parse result is the honest marker, because `record_parse`,
/// `replace_chunks` and the index write share one closure and therefore one
/// transaction. Either all three are there or none of them are.
pub fn content_stage_finished(conn: &Connection, version_id: VersionId) -> Result<bool> {
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM parse_results WHERE version_id = ?1",
            [version_id.to_string()],
            |r| r.get(0),
        )
        .map_err(|e| crate::map_sqlite(e, "checking whether a version was parsed"))?;
    Ok(n > 0)
}

pub fn current_version(conn: &Connection, file_id: FileId) -> Result<Option<VersionRow>> {
    q(
        conn.query_row(
            &format!(
                "SELECT {VERSION_COLUMNS} FROM file_versions
                 WHERE file_id = ?1 AND status = 'CURRENT'"
            ),
            params![file_id.to_string()],
            version_row,
        )
        .optional(),
        "Could not read the current version of a file.",
    )
}

/// Every version of a file, newest observation first.
pub fn versions_for(conn: &Connection, file_id: FileId) -> Result<Vec<VersionRow>> {
    let mut stmt = q(
        conn.prepare(&format!(
            "SELECT {VERSION_COLUMNS} FROM file_versions WHERE file_id = ?1
             ORDER BY observed_at DESC, version_id DESC"
        )),
        "Could not read a file's version history.",
    )?;
    let rows = q(
        stmt.query_map(params![file_id.to_string()], version_row),
        "Could not read a file's version history.",
    )?;
    let mut out = Vec::new();
    for r in rows {
        out.push(q(r, "Could not decode a file version row.")?);
    }
    Ok(out)
}

// --------------------------------------------------------------- path history

/// One observed path range for a file. `observed_to` is NULL while current.
#[derive(Clone, Debug)]
pub struct PathRow {
    pub path_id: String,
    pub file_id: FileId,
    pub path: String,
    pub observed_from: Timestamp,
    pub observed_to: Option<Timestamp>,
}

fn insert_path_row(conn: &Connection, file_id: FileId, path: &str, at: Timestamp) -> Result<()> {
    q(
        conn.execute(
            "INSERT INTO file_paths (path_id, file_id, path, observed_from)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                ulid::Ulid::new().to_string(),
                file_id.to_string(),
                path,
                at.as_millis()
            ],
        ),
        "Could not record a file's path in the index database.",
    )?;
    Ok(())
}

/// Record that a file now lives at a new path.
///
/// **Invariant #2: the path is not the identity.** `file_id` is untouched, the
/// open range in `file_paths` is closed at `at`, and a new one is opened. Every
/// version, chunk and job keyed on this file stays valid across the move.
/// Bring a soft-deleted file back, because the walk found it again.
///
/// **Nothing set `status` back to `ACTIVE`, so every delete was permanent.**
/// Reconciliation marks a file DELETED when a walk does not reach it, which is
/// right when the file is gone and wrong when the walk was. Move a file out of
/// a watched folder and back, or let one sweep fail to open a directory, and
/// the row stayed DELETED for ever: the next walk finds it by filesystem
/// identity, restores `current_path`, sees the content hash unchanged, and
/// reports it as *unchanged* — while search, `read_file` and `file_info` all
/// filter on `status = 'ACTIVE'` and refuse it. No error, no warning, and no
/// counter that moves.
///
/// A soft delete has to be reversible by the same mechanism that made it, or it
/// is not a soft delete.
pub fn restore_file(conn: &Connection, file_id: FileId, at: Timestamp) -> Result<bool> {
    let n = conn
        .execute(
            "UPDATE files SET status = 'ACTIVE', updated_at = ?2
              WHERE file_id = ?1 AND status = 'DELETED'",
            rusqlite::params![file_id.to_string(), at.as_millis()],
        )
        .map_err(|e| crate::map_sqlite(e, "restoring a file the walk found again"))?;
    Ok(n > 0)
}

pub fn record_path_change(
    conn: &Connection,
    file_id: FileId,
    new_path: &str,
    at: Timestamp,
) -> Result<()> {
    let exists: i64 = q(
        conn.query_row(
            "SELECT count(*) FROM files WHERE file_id = ?1",
            params![file_id.to_string()],
            |r| r.get(0),
        ),
        "Could not check that the moved file is known to the index.",
    )?;
    if exists == 0 {
        return Err(Error::invariant(
            "A path change was recorded for a file that is not in the index. The scan and the \
             database have diverged; re-run the scan for this root.",
        )
        .with_context(format!("file_id = {file_id}")));
    }

    q(
        conn.execute(
            "UPDATE file_paths SET observed_to = ?2 WHERE file_id = ?1 AND observed_to IS NULL",
            params![file_id.to_string(), at.as_millis()],
        ),
        "Could not close a file's previous path range.",
    )?;
    insert_path_row(conn, file_id, new_path, at)?;
    q(
        conn.execute(
            "UPDATE files SET current_path = ?2, updated_at = ?3 WHERE file_id = ?1",
            params![file_id.to_string(), new_path, at.as_millis()],
        ),
        "Could not update a file's current path.",
    )?;
    tracing::debug!(file_id = %file_id, new_path, "path change recorded");
    Ok(())
}

/// A file's path history, oldest first.
pub fn path_history(conn: &Connection, file_id: FileId) -> Result<Vec<PathRow>> {
    let mut stmt = q(
        conn.prepare(
            "SELECT path_id, file_id, path, observed_from, observed_to FROM file_paths
             WHERE file_id = ?1 ORDER BY observed_from ASC, path_id ASC",
        ),
        "Could not read a file's path history.",
    )?;
    let rows = q(
        stmt.query_map(params![file_id.to_string()], |row| {
            Ok(PathRow {
                path_id: row.get(0)?,
                file_id: id_of(&row.get::<_, String>(1)?, "file_paths.file_id")?,
                path: row.get(2)?,
                observed_from: ts(row, 3)?,
                observed_to: ts_opt(row, 4)?,
            })
        }),
        "Could not read a file's path history.",
    )?;
    let mut out = Vec::new();
    for r in rows {
        out.push(q(r, "Could not decode a path-history row.")?);
    }
    Ok(out)
}

// -------------------------------------------------------------------- jobs

/// A unit of durable background work (§106.10, §111).
#[derive(Clone, Debug)]
pub struct NewJob {
    pub job_id: JobId,
    pub workspace_id: Option<WorkspaceId>,
    pub job_type: String,
    pub target_id: Option<String>,
    pub target_version: Option<String>,
    /// §20.2. Two enqueues with the same key are one job, forever.
    pub idempotency_key: String,
    pub priority: i64,
    pub max_attempts: i64,
    pub not_before: Timestamp,
    /// JSON, or NULL. A CHECK constraint rejects anything else.
    pub payload: Option<String>,
    pub at: Timestamp,
}

impl NewJob {
    /// The idempotency key is `(target, processor, processor_version)` in the
    /// caller's terms — hard rule 7. It is required, not derived, because only
    /// the caller knows what "the same work" means for its job type.
    pub fn new(job_type: impl Into<String>, idempotency_key: impl Into<String>) -> Self {
        Self {
            job_id: JobId::new(),
            workspace_id: None,
            job_type: job_type.into(),
            target_id: None,
            target_version: None,
            idempotency_key: idempotency_key.into(),
            priority: 3,
            max_attempts: 3,
            not_before: Timestamp::EPOCH,
            payload: None,
            at: Timestamp::now(),
        }
    }
}

/// The outcome of an enqueue. `created == false` means the key was already
/// queued and nothing changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Enqueued {
    pub job_id: JobId,
    pub created: bool,
}

/// A job handed to a worker under a time-bounded lease.
#[derive(Clone, Debug)]
pub struct LeasedJob {
    pub job_id: JobId,
    pub workspace_id: Option<WorkspaceId>,
    pub job_type: String,
    pub target_id: Option<String>,
    pub target_version: Option<String>,
    pub payload: Option<String>,
    pub attempt: i64,
    pub max_attempts: i64,
    pub priority: i64,
    pub lease_expires_at: Timestamp,
}

/// Enqueue a job. Re-enqueueing an existing `idempotency_key` is a no-op
/// (§111) and returns the id of the job that is already queued.
pub fn enqueue_job(conn: &Connection, j: &NewJob) -> Result<Enqueued> {
    let inserted = q(
        conn.execute(
            "INSERT INTO jobs
               (job_id, workspace_id, job_type, target_id, target_version, idempotency_key,
                priority, max_attempts, not_before, payload, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)
             ON CONFLICT(idempotency_key) DO NOTHING",
            params![
                j.job_id.to_string(),
                j.workspace_id.map(|w| w.to_string()),
                j.job_type,
                j.target_id,
                j.target_version,
                j.idempotency_key,
                j.priority,
                j.max_attempts,
                j.not_before.as_millis(),
                j.payload,
                j.at.as_millis(),
            ],
        ),
        "Could not enqueue a background job.",
    )?;
    if inserted == 1 {
        tracing::trace!(job_id = %j.job_id, job_type = %j.job_type, "job enqueued");
        return Ok(Enqueued {
            job_id: j.job_id,
            created: true,
        });
    }
    let existing: String = q(
        conn.query_row(
            "SELECT job_id FROM jobs WHERE idempotency_key = ?1",
            params![j.idempotency_key],
            |r| r.get(0),
        ),
        "Could not read back an already-queued job.",
    )?;
    Ok(Enqueued {
        job_id: parse_id(&existing, "jobs.job_id")?,
        created: false,
    })
}

/// Exponential backoff with jitter, in milliseconds (§111.1).
///
/// Jitter comes from the wall clock's sub-millisecond noise rather than a `rand`
/// dependency: it only has to stop a thundering herd, not resist an adversary.
pub fn backoff_ms(attempt: i64) -> i64 {
    const BASE: i64 = 1_000;
    const CAP: i64 = 5 * 60 * 1_000;
    let exp = attempt.clamp(1, 20) as u32 - 1;
    let base = BASE
        .saturating_mul(1_i64.checked_shl(exp).unwrap_or(i64::MAX))
        .min(CAP);
    let noise = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::from(d.subsec_nanos() % 1_000))
        .unwrap_or(0);
    // Up to +25%.
    base + (base / 4) * noise / 1_000
}

/// Return every job whose lease has expired to `PENDING` (crash recovery,
/// NFR-003). Jobs that have also exhausted their attempts go to `DEAD` instead,
/// so a job that kills the process cannot loop forever.
pub fn release_expired_leases(conn: &Connection, now: Timestamp) -> Result<usize> {
    let dead = q(
        conn.execute(
            "UPDATE jobs SET status = 'DEAD', lease_owner = NULL, lease_expires_at = NULL,
                             last_error_code = COALESCE(last_error_code, 'DB_WRITER_GONE'),
                             updated_at = ?1
             WHERE status IN ('LEASED','RUNNING') AND lease_expires_at <= ?1
               AND attempt >= max_attempts",
            params![now.as_millis()],
        ),
        "Could not retire jobs whose lease expired.",
    )?;
    let revived = q(
        conn.execute(
            "UPDATE jobs SET status = 'PENDING', lease_owner = NULL, lease_expires_at = NULL,
                             updated_at = ?1
             WHERE status IN ('LEASED','RUNNING') AND lease_expires_at <= ?1",
            params![now.as_millis()],
        ),
        "Could not return jobs whose lease expired to the queue.",
    )?;
    if dead + revived > 0 {
        tracing::info!(revived, dead, "expired job leases reclaimed");
    }
    Ok(dead + revived)
}

/// Take the next runnable job under a lease, oldest-first within priority.
///
/// Expired leases are reclaimed first, so a crashed worker's job is picked up
/// by whoever asks next rather than needing a separate sweeper.
pub fn lease_job(
    conn: &Connection,
    owner: &str,
    lease_for: Duration,
    now: Timestamp,
) -> Result<Option<LeasedJob>> {
    release_expired_leases(conn, now)?;

    let candidate: Option<(String, i64, i64, i64)> = q(
        conn.query_row(
            "SELECT job_id, attempt, max_attempts, priority FROM jobs
             WHERE status = 'PENDING' AND not_before <= ?1 AND attempt < max_attempts
             ORDER BY priority ASC, created_at ASC, job_id ASC
             LIMIT 1",
            params![now.as_millis()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional(),
        "Could not read the job queue.",
    )?;
    let Some((job_id, attempt, max_attempts, priority)) = candidate else {
        return Ok(None);
    };

    let expires = Timestamp::from_millis(now.as_millis() + lease_for.as_millis() as i64);
    let taken = q(
        conn.execute(
            "UPDATE jobs SET status = 'LEASED', lease_owner = ?2, lease_expires_at = ?3,
                             attempt = attempt + 1, updated_at = ?4
             WHERE job_id = ?1 AND status = 'PENDING'",
            params![job_id, owner, expires.as_millis(), now.as_millis()],
        ),
        "Could not take a lease on a job.",
    )?;
    if taken == 0 {
        // Someone else took it between the read and the write. Only possible if
        // a second writer exists, which is the bug this store is designed to
        // make impossible — so say so rather than looping.
        return Err(Error::invariant(
            "A job changed state during leasing, which means a second writer is active. \
             All writes must go through the single writer actor.",
        )
        .with_context(format!("job_id = {job_id}")));
    }

    let row: (
        Option<String>,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = q(
        conn.query_row(
            "SELECT workspace_id, job_type, target_id, target_version, payload
             FROM jobs WHERE job_id = ?1",
            params![job_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        ),
        "Could not read a leased job.",
    )?;

    tracing::debug!(job_id = %job_id, owner, attempt = attempt + 1, "job leased");
    Ok(Some(LeasedJob {
        job_id: parse_id(&job_id, "jobs.job_id")?,
        workspace_id: match row.0 {
            Some(w) => Some(parse_id(&w, "jobs.workspace_id")?),
            None => None,
        },
        job_type: row.1,
        target_id: row.2,
        target_version: row.3,
        payload: row.4,
        attempt: attempt + 1,
        max_attempts,
        priority,
        lease_expires_at: expires,
    }))
}

/// Move a leased job to `RUNNING` (§111.1).
pub fn start_job(conn: &Connection, job_id: JobId, now: Timestamp) -> Result<()> {
    q(
        conn.execute(
            "UPDATE jobs SET status = 'RUNNING', updated_at = ?2
             WHERE job_id = ?1 AND status = 'LEASED'",
            params![job_id.to_string(), now.as_millis()],
        ),
        "Could not mark a job as running.",
    )?;
    Ok(())
}

/// Finish a job successfully.
pub fn complete_job(conn: &Connection, job_id: JobId, now: Timestamp) -> Result<()> {
    let n = q(
        conn.execute(
            "UPDATE jobs SET status = 'DONE', lease_owner = NULL, lease_expires_at = NULL,
                             last_error_code = NULL, last_error_detail = NULL, updated_at = ?2
             WHERE job_id = ?1 AND status IN ('LEASED','RUNNING')",
            params![job_id.to_string(), now.as_millis()],
        ),
        "Could not mark a job as done.",
    )?;
    if n == 0 {
        return Err(Error::invariant(
            "A job was completed that was not leased. Its lease may have expired mid-run; the \
             work will be redone.",
        )
        .with_context(format!("job_id = {job_id}")));
    }
    tracing::trace!(job_id = %job_id, "job done");
    Ok(())
}

/// Fail a job: back off and requeue, or retire it to `DEAD` once its attempts
/// are spent. `DEAD` is visible in index health and never retried silently
/// (§111.1).
pub fn fail_job(
    conn: &Connection,
    job_id: JobId,
    code: Code,
    detail: Option<&str>,
    now: Timestamp,
) -> Result<JobStatus> {
    let (attempt, max_attempts): (i64, i64) = q(
        conn.query_row(
            "SELECT attempt, max_attempts FROM jobs WHERE job_id = ?1",
            params![job_id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ),
        "Could not read the job that failed.",
    )?;

    // A non-retryable code is spent immediately: retrying a POL_* denial or a
    // PAR_UNSUPPORTED is just the same answer, more slowly.
    let spent = attempt >= max_attempts || !code.retryable();
    let next = if spent {
        JobStatus::Dead
    } else {
        JobStatus::Pending
    };
    let not_before = if spent {
        now.as_millis()
    } else {
        now.as_millis() + backoff_ms(attempt)
    };

    q(
        conn.execute(
            "UPDATE jobs SET status = ?2, lease_owner = NULL, lease_expires_at = NULL,
                             not_before = ?3, last_error_code = ?4, last_error_detail = ?5,
                             updated_at = ?6
             WHERE job_id = ?1",
            params![
                job_id.to_string(),
                next.as_sql(),
                not_before,
                code.as_str(),
                detail,
                now.as_millis(),
            ],
        ),
        "Could not record a job failure.",
    )?;
    if next == JobStatus::Dead {
        tracing::warn!(job_id = %job_id, code = code.as_str(), attempt, "job is dead");
    } else {
        tracing::debug!(job_id = %job_id, code = code.as_str(), attempt, "job requeued");
    }
    Ok(next)
}

pub fn job_status(conn: &Connection, job_id: JobId) -> Result<Option<JobStatus>> {
    let raw: Option<String> = q(
        conn.query_row(
            "SELECT status FROM jobs WHERE job_id = ?1",
            params![job_id.to_string()],
            |r| r.get(0),
        )
        .optional(),
        "Could not read a job's status.",
    )?;
    match raw {
        None => Ok(None),
        Some(s) => JobStatus::of(&s).map(Some).ok_or_else(|| {
            Error::new(
                Code::DbCorrupt,
                "The job queue holds a status this build does not recognise. Delete the index \
                 directory to rebuild it from your files.",
            )
            .with_context(format!("jobs.status = {s:?}"))
        }),
    }
}

/// How many jobs are in each state. For `marrow status`.
pub fn job_counts(conn: &Connection) -> Result<Vec<(JobStatus, i64)>> {
    let mut stmt = q(
        conn.prepare("SELECT status, count(*) FROM jobs GROUP BY status ORDER BY status"),
        "Could not summarise the job queue.",
    )?;
    let rows = q(
        stmt.query_map([], |r| {
            let s: String = r.get(0)?;
            let n: i64 = r.get(1)?;
            Ok((
                JobStatus::of(&s).ok_or_else(|| decode_err("jobs.status", &s))?,
                n,
            ))
        }),
        "Could not summarise the job queue.",
    )?;
    let mut out = Vec::new();
    for r in rows {
        out.push(q(r, "Could not decode a job-queue summary row.")?);
    }
    Ok(out)
}

// ------------------------------------------------------------------ id parsing

fn parse_id<T: FromStr<Err = ulid::DecodeError>>(s: &str, column: &str) -> Result<T> {
    s.parse::<T>().map_err(|e| {
        Error::new(
            Code::DbCorrupt,
            "The index database holds an identifier that is not a ULID. Delete the index \
             directory to rebuild it from your files.",
        )
        .with_context(format!("{column} = {s:?}"))
        .with_source(e)
    })
}

// ── chunks ────────────────────────────────────────────────────────────────

/// A chunk to persist. Mirrors `marrow_parse::Chunk` without depending on it —
/// `store` is an adapter and must not know the parser exists (LLD §1).
#[derive(Clone, Debug)]
pub struct NewChunk {
    pub chunk_id: ChunkId,
    pub version_id: VersionId,
    pub chunk_kind: String,
    pub text: String,
    pub context_prefix: Option<String>,
    pub token_count: i64,
    pub text_hash: ContentHash,
    pub chunker_version: String,
    pub provenance_class: String,
}

/// Replace a version's chunks.
///
/// Deleting first is what makes re-parsing idempotent: a file that shrinks must
/// not leave its old tail behind as searchable chunks that no longer exist in
/// the source. `ON DELETE CASCADE` from the lexical index's doc table carries
/// the removal through, so the two cannot drift.
pub fn replace_chunks(conn: &Connection, version_id: VersionId, chunks: &[NewChunk]) -> Result<()> {
    conn.execute(
        "DELETE FROM chunks WHERE version_id = ?1",
        [version_id.to_string()],
    )
    .map_err(|e| crate::map_sqlite(e, "clearing previous chunks"))?;

    if chunks.is_empty() {
        return Ok(());
    }
    let mut stmt = conn
        .prepare_cached(
            "INSERT INTO chunks
                (chunk_id, version_id, chunk_kind, text, context_prefix,
                 token_count, text_hash, chunker_version, provenance_class)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        )
        .map_err(|e| crate::map_sqlite(e, "preparing chunk insert"))?;
    for c in chunks {
        stmt.execute(params![
            c.chunk_id.to_string(),
            c.version_id.to_string(),
            c.chunk_kind,
            c.text,
            c.context_prefix,
            c.token_count,
            c.text_hash.to_hex(),
            c.chunker_version,
            c.provenance_class,
        ])
        .map_err(|e| crate::map_sqlite(e, "inserting a chunk"))?;
    }
    Ok(())
}

/// A parse attempt to persist.
///
/// PAR-003: the parser's identity and version are what let an upgrade schedule
/// reprocessing without a manual reindex. Without this row, a parser fix is
/// invisible to every file already indexed.
#[derive(Clone, Debug)]
pub struct NewParse {
    pub version_id: VersionId,
    pub parser_id: String,
    pub parser_version: String,
    pub parser_tier: String,
    pub provenance_class: String,
    pub outcome: String,
    pub char_yield: Option<i64>,
    pub page_count: Option<i64>,
    pub warnings: Option<String>,
    pub parsed_at: Timestamp,
}

/// Record a parse attempt, replacing any earlier one for the same
/// (version, parser, parser_version).
///
/// That triple is the idempotency key from §20.2: re-running the same parser
/// over the same bytes converges rather than accumulating rows.
pub fn record_parse(conn: &Connection, p: &NewParse) -> Result<()> {
    conn.execute(
        "INSERT INTO parse_results
            (parse_id, version_id, parser_id, parser_version, parser_tier,
             provenance_class, outcome, char_yield, page_count, warnings, parsed_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
         ON CONFLICT(version_id, parser_id, parser_version) DO UPDATE SET
             parser_tier      = excluded.parser_tier,
             provenance_class = excluded.provenance_class,
             outcome          = excluded.outcome,
             char_yield       = excluded.char_yield,
             page_count       = excluded.page_count,
             warnings         = excluded.warnings,
             parsed_at        = excluded.parsed_at",
        params![
            ulid::Ulid::new().to_string(),
            p.version_id.to_string(),
            p.parser_id,
            p.parser_version,
            p.parser_tier,
            p.provenance_class,
            p.outcome,
            p.char_yield,
            p.page_count,
            p.warnings,
            p.parsed_at.as_millis(),
        ],
    )
    .map(|_| ())
    .map_err(|e| crate::map_sqlite(e, "recording a parse result"))
}

// ------------------------------------------------------------ tables (§106.6)
//
// Plain fields, no `marrow-parse` types. The parse crate turns bytes into an
// IR and this crate turns rows into SQL; a dependency between them would make
// the store care what a `ColumnType` is, and the enum-to-`TEXT` conversion
// belongs at one boundary rather than two. `NewParse` above works the same way.

/// One table to persist, with its cells.
#[derive(Clone, Debug)]
pub struct NewTable {
    /// ULID.
    pub table_id: String,
    pub version_id: VersionId,
    /// Ordinal of the IR node this came from. `ir_nodes` is not written yet, so
    /// this is what identifies the node — see the note on `node_id` in the DDL.
    pub node_ordinal: Option<i64>,
    /// `SourceSpan` as JSON, the same shape `ir_nodes.source_span` holds.
    pub source_span: String,
    pub n_rows: i64,
    pub n_cols: i64,
    pub header_rows: i64,
    pub header_cols: i64,
    pub header_row_idx: Option<i64>,
    /// TBL-003. Recorded even when no header was accepted.
    pub header_confidence: f64,
    pub column_names: Option<String>,
    pub column_types: Option<String>,
    pub merged_regions: Option<String>,
    pub caption: Option<String>,
    pub extraction_method: String,
    pub provenance_class: String,
    pub reconstruction: String,
    pub cells: Vec<NewCell>,
}

/// One cell. **TBL-002**: `cell_span` is not optional, here or in the schema.
#[derive(Clone, Debug)]
pub struct NewCell {
    pub row_idx: i64,
    pub col_idx: i64,
    pub rowspan: i64,
    pub colspan: i64,
    /// TBL-005. Always the text as written.
    pub raw_text: String,
    pub typed_value: Option<String>,
    pub value_type: Option<String>,
    /// **TBL-007 / PAR-007.** The formula the cell's value was computed from,
    /// as written. `None` for a literal and for every format without formulas.
    pub formula: Option<String>,
    pub cell_span: String,
    pub confidence: f64,
}

/// Replace a version's tables.
///
/// Delete-then-insert for the same reason as [`replace_chunks`]: re-parsing a
/// file that lost a table must not leave the table behind as something a query
/// can still cite. `ON DELETE CASCADE` takes the cells with it.
pub fn replace_tables(conn: &Connection, version_id: VersionId, tables: &[NewTable]) -> Result<()> {
    conn.execute(
        "DELETE FROM table_ir WHERE version_id = ?1",
        [version_id.to_string()],
    )
    .map_err(|e| crate::map_sqlite(e, "clearing previous tables"))?;

    if tables.is_empty() {
        return Ok(());
    }
    let mut table_stmt = conn
        .prepare_cached(
            "INSERT INTO table_ir
                (table_id, version_id, node_ordinal, source_span, n_rows, n_cols,
                 header_rows, header_cols, header_row_idx, header_confidence,
                 column_names, column_types, merged_regions, caption,
                 extraction_method, provenance_class, reconstruction)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
        )
        .map_err(|e| crate::map_sqlite(e, "preparing table insert"))?;
    let mut cell_stmt = conn
        .prepare_cached(
            "INSERT INTO table_cells
                (cell_id, table_id, row_idx, col_idx, rowspan, colspan,
                 raw_text, typed_value, value_type, formula, cell_span, confidence)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        )
        .map_err(|e| crate::map_sqlite(e, "preparing table cell insert"))?;

    for t in tables {
        table_stmt
            .execute(params![
                t.table_id,
                t.version_id.to_string(),
                t.node_ordinal,
                t.source_span,
                t.n_rows,
                t.n_cols,
                t.header_rows,
                t.header_cols,
                t.header_row_idx,
                t.header_confidence,
                t.column_names,
                t.column_types,
                t.merged_regions,
                t.caption,
                t.extraction_method,
                t.provenance_class,
                t.reconstruction,
            ])
            .map_err(|e| crate::map_sqlite(e, "inserting a table"))?;
        for c in &t.cells {
            cell_stmt
                .execute(params![
                    ulid::Ulid::new().to_string(),
                    t.table_id,
                    c.row_idx,
                    c.col_idx,
                    c.rowspan,
                    c.colspan,
                    c.raw_text,
                    c.typed_value,
                    c.value_type,
                    c.formula,
                    c.cell_span,
                    c.confidence,
                ])
                .map_err(|e| crate::map_sqlite(e, "inserting a table cell"))?;
        }
    }
    Ok(())
}

/// A stored table, without its cells.
#[derive(Clone, Debug, PartialEq)]
pub struct TableRow {
    pub table_id: String,
    pub version_id: VersionId,
    pub source_span: String,
    pub n_rows: i64,
    pub n_cols: i64,
    pub header_rows: i64,
    pub header_row_idx: Option<i64>,
    pub header_confidence: f64,
    pub column_names: Option<String>,
    pub column_types: Option<String>,
    pub caption: Option<String>,
    pub extraction_method: String,
    pub provenance_class: String,
    pub reconstruction: String,
}

/// Every live table of one file version, in document order.
pub fn tables_for(conn: &Connection, version_id: VersionId) -> Result<Vec<TableRow>> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT table_id, version_id, source_span, n_rows, n_cols, header_rows,
                    header_row_idx, header_confidence, column_names, column_types,
                    caption, extraction_method, provenance_class, reconstruction
             FROM table_ir
             WHERE version_id = ?1 AND status = 'ACTIVE'
             ORDER BY node_ordinal, table_id",
        )
        .map_err(|e| crate::map_sqlite(e, "preparing the table query"))?;
    let rows = stmt
        .query_map([version_id.to_string()], |r| {
            Ok(TableRow {
                table_id: r.get(0)?,
                version_id: id_of(&r.get::<_, String>(1)?, "table_ir.version_id")?,
                source_span: r.get(2)?,
                n_rows: r.get(3)?,
                n_cols: r.get(4)?,
                header_rows: r.get(5)?,
                header_row_idx: r.get(6)?,
                header_confidence: r.get(7)?,
                column_names: r.get(8)?,
                column_types: r.get(9)?,
                caption: r.get(10)?,
                extraction_method: r.get(11)?,
                provenance_class: r.get(12)?,
                reconstruction: r.get(13)?,
            })
        })
        .map_err(|e| crate::map_sqlite(e, "reading tables"))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| crate::map_sqlite(e, "reading tables"))
}

/// A stored cell.
#[derive(Clone, Debug, PartialEq)]
pub struct CellRow {
    pub row_idx: i64,
    pub col_idx: i64,
    pub rowspan: i64,
    pub colspan: i64,
    pub raw_text: String,
    pub typed_value: Option<String>,
    pub value_type: Option<String>,
    pub formula: Option<String>,
    pub cell_span: String,
    pub confidence: f64,
}

/// One table's cells, row-major.
pub fn cells_for(conn: &Connection, table_id: &str) -> Result<Vec<CellRow>> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT row_idx, col_idx, rowspan, colspan, raw_text, typed_value,
                    value_type, formula, cell_span, confidence
             FROM table_cells WHERE table_id = ?1 ORDER BY row_idx, col_idx",
        )
        .map_err(|e| crate::map_sqlite(e, "preparing the cell query"))?;
    let rows = stmt
        .query_map([table_id], |r| {
            Ok(CellRow {
                row_idx: r.get(0)?,
                col_idx: r.get(1)?,
                rowspan: r.get(2)?,
                colspan: r.get(3)?,
                raw_text: r.get(4)?,
                typed_value: r.get(5)?,
                value_type: r.get(6)?,
                formula: r.get(7)?,
                cell_span: r.get(8)?,
                confidence: r.get(9)?,
            })
        })
        .map_err(|e| crate::map_sqlite(e, "reading table cells"))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| crate::map_sqlite(e, "reading table cells"))
}

/// How many active chunks exist, for `marrow status`.
pub fn chunk_count(conn: &Connection) -> Result<i64> {
    conn.query_row(
        "SELECT count(*) FROM chunks WHERE status = 'ACTIVE'",
        [],
        |r| r.get(0),
    )
    .map_err(|e| crate::map_sqlite(e, "counting chunks"))
}

/// Re-classify a file's authorship.
///
/// Called when its content changed: authorship follows the bytes, so a file the
/// user edits stops being the system's and a file the system rewrites becomes
/// its own again. Without this, `origin` is decided once at discovery and never
/// revisited, which makes an edited file permanently uncitable — the user's own
/// work, silently excluded from their own answers.
pub fn set_file_origin(
    conn: &Connection,
    file_id: FileId,
    origin: Origin,
    at: Timestamp,
) -> Result<()> {
    q(
        conn.execute(
            "UPDATE files SET origin = ?2, updated_at = ?3 WHERE file_id = ?1",
            params![file_id.to_string(), origin_sql(origin), at.as_millis()],
        ),
        "Could not update who wrote that file.",
    )?;
    Ok(())
}

// ------------------------------------------------------------- self-written

/// Record that this system wrote these bytes (invariant #9).
///
/// Idempotent on the hash: writing the same content twice is one fact, not
/// two, and re-running a failed action must not double-count.
pub fn record_self_written(
    conn: &Connection,
    content_hash: ContentHash,
    written_path: &str,
    txn_id: &str,
    tool: &str,
    at: Timestamp,
) -> Result<()> {
    q(
        conn.execute(
            "INSERT INTO self_written (content_hash, written_path, txn_id, tool, written_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(content_hash) DO UPDATE SET
                 written_path = excluded.written_path,
                 txn_id       = excluded.txn_id,
                 tool         = excluded.tool,
                 written_at   = excluded.written_at",
            params![
                content_hash.to_hex(),
                written_path,
                txn_id,
                tool,
                at.as_millis()
            ],
        ),
        "Could not record that this system wrote that file. It would then be \
         treated as your own work and cited back; the write was not recorded.",
    )?;
    Ok(())
}

/// Every hash this system wrote.
///
/// Loaded once per ingest run rather than queried per file: the set is small
/// (one row per file the tools ever produced) and a query per file would put a
/// round trip in the hot loop for a check that is almost always negative.
pub fn self_written_hashes(conn: &Connection) -> Result<std::collections::HashSet<ContentHash>> {
    let mut stmt = q(
        conn.prepare("SELECT content_hash FROM self_written"),
        "Could not read the record of what this system wrote.",
    )?;
    let rows = q(
        stmt.query_map([], |r| r.get::<_, String>(0)),
        "Could not read the record of what this system wrote.",
    )?;
    let mut out = std::collections::HashSet::new();
    for row in rows {
        let hex = q(row, "Could not read a self-written record.")?;
        if let Some(h) = ContentHash::from_hex(&hex) {
            out.insert(h);
        }
    }
    Ok(out)
}

/// Forget one write. Used by the forget path; the file itself is not touched.
pub fn forget_self_written(conn: &Connection, content_hash: ContentHash) -> Result<bool> {
    let n = q(
        conn.execute(
            "DELETE FROM self_written WHERE content_hash = ?1",
            [content_hash.to_hex()],
        ),
        "Could not forget that write.",
    )?;
    Ok(n > 0)
}
