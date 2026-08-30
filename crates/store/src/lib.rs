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

pub mod conversations;
pub mod migrate;
pub mod read;
pub mod schema;
pub mod writer;

use std::path::{Path, PathBuf};

use marrow_core::{Code, Error, FileId, JobId, Result, RootId, Timestamp, VersionId, WorkspaceId};
use rusqlite::{Connection, OpenFlags};

pub use conversations::{ConversationRow, NewTurn, TurnMode, TurnRow};
pub use read::{
    CellRow, Enqueued, FileRow, JobStatus, LeasedJob, NewCell, NewFile, NewJob, NewRoot, NewTable,
    NewVersion, NewWorkspace, PathRow, ReadConn, StorageKind, TableRow, VersionRow,
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
    /// The composed chain is **sorted and checked here**, not by the caller.
    ///
    /// It used to be the caller's job, and that was fine while this crate's own
    /// list was `[1]` and every extra came after it. It stopped being fine the
    /// moment the store added a migration *after* one another crate had
    /// claimed: `[1, 3] + [2]` composes to `[1, 3, 2]`, which applies 3, then
    /// skips 2 as already-applied, and leaves a database missing tables it
    /// reports as present. Sorting is one line; remembering to sort at four
    /// call sites is a bug waiting for the fifth.
    pub fn open_with_migrations(
        path: impl AsRef<Path>,
        extra: &[migrate::Migration],
    ) -> Result<Self> {
        let chain = compose(extra)?;
        Self::open_with_chain(
            Location::File(path.as_ref().to_path_buf()),
            WriterConfig::default(),
            &chain,
        )
    }

    /// An in-memory store with extra migrations, for tests.
    pub fn open_in_memory_with_migrations(extra: &[migrate::Migration]) -> Result<Self> {
        let chain = compose(extra)?;
        Self::open_with_chain(Location::memory(), WriterConfig::default(), &chain)
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

    /// Record a reconciliation. See [`read::mark_reconciled`].
    pub fn mark_reconciled(
        &self,
        root_id: marrow_core::RootId,
        health: read::WatcherHealth,
        at: marrow_core::Timestamp,
    ) -> Result<()> {
        self.handle
            .submit(move |c| read::mark_reconciled(c, root_id, health, at))
    }

    /// Record watcher health without claiming a reconciliation.
    /// See [`read::mark_watcher_health`].
    pub fn mark_watcher_health(
        &self,
        root_id: marrow_core::RootId,
        health: read::WatcherHealth,
    ) -> Result<()> {
        self.handle
            .submit(move |c| read::mark_watcher_health(c, root_id, health))
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

    // ------------------------------------------------------- conversations
    //
    // Reads go through `reader()` like every other read; only the three writes
    // need a helper, because they must go through the one writer connection.

    /// Record a completed exchange, starting the conversation if `into` is
    /// `None`. Returns the conversation it landed in.
    pub fn append_turn(
        &self,
        into: Option<String>,
        turn: conversations::NewTurn,
    ) -> Result<String> {
        self.handle
            .submit(move |c| conversations::append_turn(c, into.as_deref(), &turn))
    }

    pub fn rename_conversation(&self, id: String, title: String, at: Timestamp) -> Result<()> {
        self.handle
            .submit(move |c| conversations::rename_conversation(c, &id, &title, at))
    }

    /// Soft delete — `status` moves to `DELETED` and every row stays.
    pub fn delete_conversation(&self, id: String) -> Result<()> {
        self.handle
            .submit(move |c| conversations::delete_conversation(c, &id))
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
mod compose_tests {
    use super::*;

    const CLASH: migrate::Migration = migrate::Migration {
        version: 1,
        name: "clash",
        up: "SELECT 1;",
    };
    const GAP: migrate::Migration = migrate::Migration {
        version: 9,
        name: "gap",
        up: "SELECT 1;",
    };

    /// Stand-ins for the two versions `marrow-index` owns, so this crate can
    /// test composition without depending on it.
    const INDEX_TWO: migrate::Migration = migrate::Migration {
        version: 2,
        name: "index_two",
        up: "SELECT 1;",
    };
    const INDEX_FOUR: migrate::Migration = migrate::Migration {
        version: 4,
        name: "index_four",
        up: "SELECT 1;",
    };

    #[test]
    fn a_later_store_migration_does_not_reorder_an_earlier_extension() {
        // `[1, 3] + [2]` composes to `[1, 3, 2]` unsorted, which applies 3,
        // skips 2 as already-applied, and leaves a database missing tables it
        // reports as present. This is the case that broke.
        let chain = compose(&[INDEX_TWO, INDEX_FOUR]).unwrap();
        let versions: Vec<i64> = chain.iter().map(|m| m.version).collect();
        // Contiguous from 1, however many this crate has since added: the
        // property is the ordering, not the length.
        assert_eq!(
            versions,
            (1..=versions.len() as i64).collect::<Vec<_>>(),
            "{versions:?}"
        );
        assert!(versions.len() >= 4, "both extensions are in: {versions:?}");
        assert_eq!(chain[1].name, "index_two", "the extension keeps its place");
    }

    #[test]
    fn two_migrations_claiming_one_version_is_refused_not_resolved() {
        // Whichever one loses would silently never run, and which one loses
        // would depend on argument order.
        let e = compose(&[INDEX_TWO, INDEX_FOUR, CLASH]).unwrap_err();
        assert_eq!(e.code(), marrow_core::Code::DbMigrationFailed);
        assert!(
            e.message().contains("silently never run"),
            "{}",
            e.message()
        );
    }

    #[test]
    fn a_gap_in_the_chain_is_refused() {
        let e = compose(&[INDEX_TWO, INDEX_FOUR, GAP]).unwrap_err();
        assert_eq!(e.code(), marrow_core::Code::DbMigrationFailed);
        assert!(e.message().contains("gap"), "{}", e.message());
    }

    #[test]
    fn composing_without_the_index_migrations_says_which_number_is_missing() {
        // This crate owns 1 and 3; `marrow-index` owns 2 and 4. Composing with
        // nothing added is not "the store on its own", it is a chain with a
        // hole in it, and the message has to say so rather than let the caller
        // discover it as a missing table.
        let e = compose(&[]).unwrap_err();
        assert_eq!(e.code(), marrow_core::Code::DbMigrationFailed);
        assert!(e.message().contains("marrow-index"), "{}", e.message());
        assert!(
            e.context()
                .unwrap_or_default()
                .contains("is version 1 and the next is 3"),
            "the context must name the gap: {:?}",
            e.context()
        );
    }
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

/// This crate's migrations plus the caller's, sorted and checked.
///
/// A duplicate version is refused rather than resolved: two crates claiming the
/// same number means one of them will silently never run, and which one depends
/// on argument order.
fn compose(extra: &[migrate::Migration]) -> Result<Vec<migrate::Migration>> {
    let mut chain: Vec<migrate::Migration> = migrate::MIGRATIONS.to_vec();
    chain.extend_from_slice(extra);
    chain.sort_by_key(|m| m.version);
    for pair in chain.windows(2) {
        if pair[0].version == pair[1].version {
            return Err(marrow_core::Error::new(
                marrow_core::Code::DbMigrationFailed,
                "Two schema migrations claim the same version, so one of them would \
                 silently never run. This is a build error, not something a user \
                 can fix.",
            )
            .with_context(format!(
                "version {} is claimed by both `{}` and `{}`",
                pair[0].version, pair[0].name, pair[1].name
            )));
        }
        if pair[1].version != pair[0].version + 1 {
            // Almost always a missing crate rather than a numbering mistake:
            // this crate owns versions 1 and 3, and `marrow-index` owns 2 and
            // 4, so composing without the index migrations leaves exactly this
            // hole. Saying which number is missing turns "the database will
            // not open" into "you did not pass the index migrations".
            return Err(marrow_core::Error::new(
                marrow_core::Code::DbMigrationFailed,
                "The schema migration chain has a gap in it. A crate that owns one \
                 of the missing versions was probably not passed in — the index \
                 migrations live in `marrow-index`.",
            )
            .with_context(format!(
                "`{}` is version {} and the next is {}",
                pair[0].name, pair[0].version, pair[1].version
            )));
        }
    }
    Ok(chain)
}
