//! Marrow store — the canonical SQLite state.
//!
//! Everything Marrow knows that cannot be re-derived from the user's files
//! lives here. Derived indexes (Tantivy, vectors) are rebuildable and live
//! elsewhere; this database is the one that must not be lost, which is why
//! every migration is preceded by a backup (hard rule 8).
//!
//! ```text
//!            ┌──────────────┐  submit(op)   ┌────────────────────────┐
//!  callers ──┤   Writer     ├──────────────►│ writer thread          │
//!            │  (cloneable) │               │  · the only write conn │
//!            └──────────────┘               │  · batches 500 / 100ms │
//!            ┌──────────────┐               └────────────────────────┘
//!  callers ──┤  ReadConn    ├─────────────────────► WAL readers, unlimited
//!            └──────────────┘                       (query_only)
//! ```
//!
//! Scope is the M1 subset of the schema (ROADMAP "Schema staging"): workspaces,
//! roots, files, paths, versions, parse results, IR nodes, chunks and jobs. No
//! graph, no actions, no media.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod migrate;
pub mod read;
pub mod schema;
pub mod writer;

use std::path::{Path, PathBuf};

use marrow_core::{Code, Error, FileId, JobId, Result, RootId, Timestamp, VersionId, WorkspaceId};
use rusqlite::{Connection, OpenFlags};

pub use read::{
    Enqueued, FileRow, JobStatus, LeasedJob, NewFile, NewJob, NewRoot, NewVersion, NewWorkspace,
    PathRow, ReadConn, StorageKind, VersionRow,
};
pub use writer::{Pending, Writer, WriterConfig};

/// Re-exported so callers can write their own ops against the writer without
/// pinning a second, possibly different, rusqlite version.
pub use rusqlite;

/// Default database file name inside the index directory.
pub const DB_FILE_NAME: &str = "marrow.sqlite";

/// Where a store's database lives.
#[derive(Clone, Debug)]
pub(crate) enum Location {
    File(PathBuf),
    /// A private in-memory database, named so that reader connections can find
    /// it. Used by tests and by `--dry-run` style callers.
    Memory(String),
}

impl Location {
    pub(crate) fn memory() -> Self {
        Location::Memory(format!(
            "file:marrow-{}?mode=memory&cache=shared",
            ulid::Ulid::new()
        ))
    }

    pub(crate) fn file_path(&self) -> Option<&Path> {
        match self {
            Location::File(p) => Some(p.as_path()),
            Location::Memory(_) => None,
        }
    }

    pub(crate) fn open(&self) -> Result<Connection> {
        match self {
            Location::File(p) => {
                if let Some(dir) = p.parent() {
                    if !dir.as_os_str().is_empty() {
                        std::fs::create_dir_all(dir)
                            .map_err(|e| Error::from(e).with_context(dir.display().to_string()))?;
                    }
                }
                Connection::open(p).map_err(|e| {
                    map_sqlite(
                        e,
                        "Could not open the index database. Check that the index directory is \
                         writable, or point Marrow at a different one.",
                    )
                    .with_context(p.display().to_string())
                })
            }
            Location::Memory(uri) => Connection::open_with_flags(
                uri,
                OpenFlags::SQLITE_OPEN_READ_WRITE
                    | OpenFlags::SQLITE_OPEN_CREATE
                    | OpenFlags::SQLITE_OPEN_URI
                    | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(|e| map_sqlite(e, "Could not open the in-memory index database.")),
        }
    }
}

/// The canonical store: one writer, many readers.
#[derive(Debug)]
pub struct Store {
    loc: Location,
    schema_version: i64,
    /// `None` only after [`Store::close`] or [`Store::abort`] has taken it.
    actor: Option<writer::WriterActor>,
    handle: Writer,
}

impl Store {
    /// Open (creating if needed) the database at `path`, migrating it to this
    /// build's schema version.
    ///
    /// Fails rather than opening if the database is newer than this build
    /// understands, or if a migration failed (§107).
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with(
            Location::File(path.as_ref().to_path_buf()),
            WriterConfig::default(),
        )
    }

    /// As [`Store::open`], with non-default writer batching.
    pub fn open_with_config(path: impl AsRef<Path>, cfg: WriterConfig) -> Result<Self> {
        Self::open_with(Location::File(path.as_ref().to_path_buf()), cfg)
    }

    /// Open with adapter migrations appended to the base chain.
    ///
    /// Adapters that add tables to the canonical database — `marrow-index`'s
    /// FTS5 tables are the first — cannot register themselves here: they depend
    /// on this crate, so this crate referencing them back would be a cycle.
    ///
    /// So the **composition root** assembles the chain. The binary is the only
    /// place that knows which adapters are in play, which is exactly where that
    /// knowledge belongs; `store` stays unaware of `index`, and `index` stays a
    /// swappable implementation of a port.
    ///
    /// Ordering is the caller's responsibility: versions must be contiguous and
    /// ascending across the whole composed chain.
    pub fn open_with_migrations(
        path: impl AsRef<Path>,
        extra: &[migrate::Migration],
    ) -> Result<Self> {
        let mut chain: Vec<migrate::Migration> = migrate::MIGRATIONS.to_vec();
        chain.extend_from_slice(extra);
        Self::open_with_chain(
            Location::File(path.as_ref().to_path_buf()),
            WriterConfig::default(),
            &chain,
        )
    }

    /// An in-memory store. Readers still work; nothing survives the process.
    pub fn open_in_memory() -> Result<Self> {
        Self::open_with(Location::memory(), WriterConfig::default())
    }

    /// An in-memory store with non-default writer batching.
    pub fn open_in_memory_with_config(cfg: WriterConfig) -> Result<Self> {
        Self::open_with(Location::memory(), cfg)
    }

    fn open_with(loc: Location, cfg: WriterConfig) -> Result<Self> {
        Self::open_with_chain(loc, cfg, migrate::MIGRATIONS)
    }

    fn open_with_chain(
        loc: Location,
        cfg: WriterConfig,
        chain: &[migrate::Migration],
    ) -> Result<Self> {
        let (conn, schema_version) = migrate::open_migrated_with(&loc, chain)?;
        let actor = writer::WriterActor::spawn(conn, cfg);
        let handle = actor.handle().clone();
        tracing::info!(
            schema_version,
            location = ?loc.file_path().map(|p| p.display().to_string()),
            "store open"
        );
        Ok(Self {
            loc,
            schema_version,
            actor: Some(actor),
            handle,
        })
    }

    /// The schema version currently in the database.
    pub fn schema_version(&self) -> i64 {
        self.schema_version
    }

    /// The database file, if this store is not in memory.
    pub fn path(&self) -> Option<&Path> {
        self.loc.file_path()
    }

    /// The write handle. Cloneable; every clone feeds the same single writer.
    pub fn writer(&self) -> &Writer {
        &self.handle
    }

    /// A fresh read-only connection. Open one per worker; there is no limit.
    pub fn reader(&self) -> Result<ReadConn> {
        ReadConn::open(&self.loc)
    }

    /// Commit anything pending. Needed before reading your own writes through a
    /// reader connection.
    pub fn flush(&self) -> Result<()> {
        self.handle.flush()
    }

    /// Flush pending writes and stop the writer thread.
    ///
    /// Dropping a `Store` does the same thing; this exists so the caller can
    /// see the error if the final commit fails.
    pub fn close(mut self) -> Result<()> {
        match self.actor.take() {
            Some(mut a) => a.shutdown(),
            None => Ok(()),
        }
    }

    /// Stop the writer **without** committing the open batch.
    ///
    /// This is what process death looks like from SQLite's side: the
    /// transaction is rolled back and uncommitted work is gone. It exists so
    /// the crash-safety invariant can be tested rather than asserted.
    pub fn abort(mut self) {
        if let Some(mut a) = self.actor.take() {
            a.abort();
        }
    }

    // ---------------------------------------------------------- write helpers
    //
    // Thin wrappers so callers do not have to spell out a closure for the
    // handful of writes M1 actually performs. Anything else goes through
    // `store.writer().submit(|conn| ...)`.

    pub fn upsert_workspace(&self, ws: NewWorkspace) -> Result<WorkspaceId> {
        self.handle.submit(move |c| read::upsert_workspace(c, &ws))
    }

    pub fn upsert_root(&self, root: NewRoot) -> Result<RootId> {
        self.handle.submit(move |c| read::upsert_root(c, &root))
    }

    pub fn insert_file(&self, f: NewFile) -> Result<FileId> {
        self.handle.submit(move |c| read::insert_file(c, &f))
    }

    pub fn record_version(&self, v: NewVersion) -> Result<VersionId> {
        self.handle.submit(move |c| read::record_version(c, &v))
    }

    pub fn insert_file_with_version(
        &self,
        f: NewFile,
        v: NewVersion,
    ) -> Result<(FileId, VersionId)> {
        self.handle
            .submit(move |c| read::insert_file_with_version(c, &f, &v))
    }

    pub fn record_path_change(
        &self,
        file_id: FileId,
        new_path: String,
        at: Timestamp,
    ) -> Result<()> {
        self.handle
            .submit(move |c| read::record_path_change(c, file_id, &new_path, at))
    }

    pub fn enqueue_job(&self, job: NewJob) -> Result<Enqueued> {
        self.handle.submit(move |c| read::enqueue_job(c, &job))
    }

    pub fn lease_job(
        &self,
        owner: impl Into<String>,
        lease_for: std::time::Duration,
        now: Timestamp,
    ) -> Result<Option<LeasedJob>> {
        let owner = owner.into();
        self.handle
            .submit(move |c| read::lease_job(c, &owner, lease_for, now))
    }

    pub fn start_job(&self, job_id: JobId, now: Timestamp) -> Result<()> {
        self.handle.submit(move |c| read::start_job(c, job_id, now))
    }

    pub fn complete_job(&self, job_id: JobId, now: Timestamp) -> Result<()> {
        self.handle
            .submit(move |c| read::complete_job(c, job_id, now))
    }

    pub fn fail_job(
        &self,
        job_id: JobId,
        code: Code,
        detail: Option<String>,
        now: Timestamp,
    ) -> Result<JobStatus> {
        self.handle
            .submit(move |c| read::fail_job(c, job_id, code, detail.as_deref(), now))
    }

    pub fn release_expired_leases(&self, now: Timestamp) -> Result<usize> {
        self.handle
            .submit(move |c| read::release_expired_leases(c, now))
    }
}

/// Map a rusqlite failure onto the §108 taxonomy.
///
/// `message` is the cause-and-action string the user sees (SUP-001); the
/// SQLite text goes into `context`, which is diagnostic and never displayed as
/// the primary message.
pub fn map_sqlite(e: rusqlite::Error, message: &str) -> Error {
    use rusqlite::ErrorCode as C;
    let code = match &e {
        rusqlite::Error::SqliteFailure(f, _) => match f.code {
            C::DatabaseBusy | C::DatabaseLocked => Code::DbBusy,
            C::DatabaseCorrupt | C::NotADatabase => Code::DbCorrupt,
            C::DiskFull => Code::DbDiskFull,
            // A constraint the schema declares and this build violated: our
            // bug, not the user's. §106.12's invariants surface here.
            C::ConstraintViolation => Code::IntInvariantViolated,
            C::CannotOpen => Code::FsPermissionDenied,
            // Writing through a `query_only` reader lands here — the one thing
            // Part 2 §50 says must never happen.
            C::ReadOnly => Code::IntInvariantViolated,
            // §108 has no DB_UNKNOWN. Anything else is SQL this build issued
            // being wrong, which is an internal defect.
            _ => Code::IntInvariantViolated,
        },
        _ => Code::IntInvariantViolated,
    };
    Error::new(code, message)
        .with_context(e.to_string())
        .with_source(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_store_can_be_shared_across_worker_threads() {
        // The scanner holds one `Store` and hands `Writer` clones to workers,
        // so both must be Sync. A `ReadConn` wraps a raw SQLite connection and
        // is Send only — one per worker, never shared.
        fn send_sync<T: Send + Sync>() {}
        fn send<T: Send>() {}
        send_sync::<Store>();
        send_sync::<Writer>();
        send::<ReadConn>();
    }

    #[test]
    fn sqlite_constraint_failures_map_to_an_invariant_violation() {
        // A CHECK or unique index firing means this build wrote something the
        // schema forbids, which is a defect here and not the user's problem.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (x TEXT CHECK (x IN ('A')))")
            .unwrap();
        let e = conn.execute("INSERT INTO t VALUES ('B')", []).unwrap_err();
        let mapped = map_sqlite(e, "test");
        assert_eq!(mapped.code(), Code::IntInvariantViolated);
        assert!(mapped.context().is_some(), "SQLite detail is kept for logs");
    }
}
