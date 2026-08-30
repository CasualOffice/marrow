//! The store's non-negotiable invariants, one named test each (§106.12,
//! Part 7 §126). An invariant without a test is a comment.

use std::time::Duration;

use marrow_core::{Code, ContentHash, FileId, Timestamp};
use marrow_store::{
    migrate, read, JobStatus, NewFile, NewJob, NewRoot, NewVersion, NewWorkspace, Store,
    WriterConfig, DB_FILE_NAME,
};

struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    workspace: marrow_core::WorkspaceId,
    root: marrow_core::RootId,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path().join(DB_FILE_NAME)).expect("open store");
    let workspace = store
        .upsert_workspace(NewWorkspace::new("desktop"))
        .expect("workspace");
    let root = store
        .upsert_root(NewRoot::new(workspace, "/Users/test/Desktop"))
        .expect("root");
    Fixture {
        _dir: dir,
        store,
        workspace,
        root,
    }
}

impl Fixture {
    /// Insert a file with one version and return its stable id.
    fn file_at(&self, path: &str, body: &[u8]) -> FileId {
        let mut f = NewFile::new(self.workspace, self.root, path);
        f.fs_identity = Some(format!("dev1:{}", path.len()));
        let v = NewVersion::new(f.file_id, path, body.len() as i64, ContentHash::of(body));
        self.store
            .insert_file_with_version(f, v)
            .expect("insert file")
            .0
    }
}

// ------------------------------------------------------------------ versions

#[test]
fn exactly_one_current_version_per_file() {
    let fx = fixture();
    let file_id = fx.file_at("/Users/test/Desktop/a.md", b"one");

    // Superseding twice must leave exactly one CURRENT and a full history.
    for body in [b"two".as_slice(), b"three".as_slice()] {
        let v = NewVersion::new(
            file_id,
            "/Users/test/Desktop/a.md",
            body.len() as i64,
            ContentHash::of(body),
        );
        fx.store.record_version(v).expect("record version");
    }

    let r = fx.store.reader().unwrap();
    let current: i64 = r
        .query_row(
            "SELECT count(*) FROM file_versions WHERE file_id = ?1 AND status = 'CURRENT'",
            [file_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(current, 1, "exactly one CURRENT version per file");

    let all = read::versions_for(&r, file_id).unwrap();
    assert_eq!(
        all.len(),
        3,
        "superseded versions are kept, not overwritten"
    );
    let newest = read::current_version(&r, file_id).unwrap().unwrap();
    assert_eq!(newest.content_hash, ContentHash::of(b"three"));
    assert!(
        newest.supersedes.is_some(),
        "a superseding version records what it replaced"
    );

    // And the index — not just the code path above — is what enforces it.
    let dup = fx.store.writer().submit(move |c| {
        c.execute(
            "INSERT INTO file_versions
               (version_id, file_id, path_at_observation, size_bytes, mtime_ms,
                content_hash, observed_at, status)
             VALUES ('01J0000000000000000000000A', ?1, '/x', 0, 0, 'ff', 0, 'CURRENT')",
            [file_id.to_string()],
        )
        .map_err(|e| marrow_store::map_sqlite(e, "second CURRENT"))
    });
    let err = dup.expect_err("idx_versions_current must reject a second CURRENT row");
    assert_eq!(err.code(), Code::IntInvariantViolated);
}

// ---------------------------------------------------------------------- paths

#[test]
fn path_is_never_identity() {
    let fx = fixture();
    let before = "/Users/test/Desktop/notes.md";
    let after = "/Users/test/Desktop/archive/notes.md";
    let file_id = fx.file_at(before, b"hello");

    let moved_at = Timestamp::from_millis(1_800_000_000_000);
    fx.store
        .record_path_change(file_id, after.to_string(), moved_at)
        .expect("record path change");

    let r = fx.store.reader().unwrap();

    // The identity survived.
    let found = read::find_file_by_path(&r, fx.root, after)
        .unwrap()
        .expect("file is findable at its new path");
    assert_eq!(found.file_id, file_id, "file_id survives a move");
    assert!(
        read::find_file_by_path(&r, fx.root, before)
            .unwrap()
            .is_none(),
        "the old path no longer resolves"
    );

    // Filesystem identity still finds it, which is what a missed rename needs.
    let ident = found.fs_identity.clone().expect("fs_identity was recorded");
    let by_ident = read::find_file_by_fs_identity(&r, fx.root, &ident)
        .unwrap()
        .expect("file is findable by fs_identity");
    assert_eq!(by_ident.file_id, file_id);

    // History is complete and the ranges do not overlap.
    let history = read::path_history(&r, file_id).unwrap();
    assert_eq!(history.len(), 2, "both paths are kept");
    assert_eq!(history[0].path, before);
    assert_eq!(history[0].observed_to, Some(moved_at), "old range closed");
    assert_eq!(history[1].path, after);
    assert_eq!(history[1].observed_from, moved_at);
    assert_eq!(history[1].observed_to, None, "current range stays open");

    // Derived rows keyed on the file are untouched by the move.
    let versions = read::versions_for(&r, file_id).unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(
        versions[0].path_at_observation, before,
        "a version records where it was observed, not where the file is now"
    );
}

// ----------------------------------------------------------------------- jobs

#[test]
fn job_idempotency_key_makes_reenqueue_a_noop() {
    let fx = fixture();
    let mut job = NewJob::new("PARSE_FILE", "parse:v1:file-42");
    job.workspace_id = Some(fx.workspace);
    job.payload = Some(r#"{"path":"/Users/test/Desktop/a.md"}"#.to_string());

    let first = fx.store.enqueue_job(job.clone()).unwrap();
    assert!(first.created);

    // Same key, different job_id: must not create a second job.
    let mut again = job.clone();
    again.job_id = marrow_core::JobId::new();
    again.priority = 0;
    let second = fx.store.enqueue_job(again).unwrap();
    assert!(!second.created, "re-enqueue is a no-op");
    assert_eq!(second.job_id, first.job_id, "the original job is returned");

    let r = fx.store.reader().unwrap();
    let n: i64 = r
        .query_row("SELECT count(*) FROM jobs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(n, 1, "one key, one job");
    let priority: i64 = r
        .query_row("SELECT priority FROM jobs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(priority, 3, "a no-op enqueue changes nothing about the job");
}

#[test]
fn expired_job_leases_return_the_job_to_pending() {
    let fx = fixture();
    let enq = fx
        .store
        .enqueue_job(NewJob::new("HASH_FILE", "hash:v1:file-1"))
        .unwrap();

    let t0 = Timestamp::from_millis(1_800_000_000_000);
    let leased = fx
        .store
        .lease_job("worker-a", Duration::from_secs(30), t0)
        .unwrap()
        .expect("a pending job is leasable");
    assert_eq!(leased.job_id, enq.job_id);
    assert_eq!(leased.attempt, 1);

    // While the lease holds, nobody else gets it.
    assert!(
        fx.store
            .lease_job("worker-b", Duration::from_secs(30), t0)
            .unwrap()
            .is_none(),
        "a leased job is not handed out twice"
    );

    // The worker dies. After the lease expires the job is PENDING again and
    // leasable by someone else — no sweeper process required.
    let t1 = Timestamp::from_millis(t0.as_millis() + 31_000);
    let reclaimed = fx.store.release_expired_leases(t1).unwrap();
    assert_eq!(reclaimed, 1);
    assert_eq!(
        fx.store
            .writer()
            .submit(move |c| read::job_status(c, enq.job_id))
            .unwrap(),
        Some(JobStatus::Pending),
        "an expired lease returns the job to PENDING"
    );

    let retaken = fx
        .store
        .lease_job("worker-b", Duration::from_secs(30), t1)
        .unwrap()
        .expect("the reclaimed job is leasable again");
    assert_eq!(retaken.job_id, enq.job_id);
    assert_eq!(retaken.attempt, 2, "the crashed attempt still counts");
}

#[test]
fn a_failed_job_backs_off_then_dies_visibly() {
    let fx = fixture();
    let mut job = NewJob::new("PARSE_FILE", "parse:v1:file-9");
    job.max_attempts = 2;
    let enq = fx.store.enqueue_job(job).unwrap();

    let t0 = Timestamp::from_millis(1_800_000_000_000);
    let leased = fx
        .store
        .lease_job("w", Duration::from_secs(30), t0)
        .unwrap()
        .unwrap();
    let next = fx
        .store
        .fail_job(leased.job_id, Code::FsLocked, Some("locked".into()), t0)
        .unwrap();
    assert_eq!(next, JobStatus::Pending, "a retryable failure requeues");

    let r = fx.store.reader().unwrap();
    let not_before: i64 = r
        .query_row(
            "SELECT not_before FROM jobs WHERE job_id = ?1",
            [enq.job_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        not_before > t0.as_millis(),
        "backoff is written to not_before"
    );
    assert!(
        fx.store
            .lease_job("w", Duration::from_secs(30), t0)
            .unwrap()
            .is_none(),
        "a backed-off job is not runnable yet"
    );

    let t1 = Timestamp::from_millis(not_before);
    let leased = fx
        .store
        .lease_job("w", Duration::from_secs(30), t1)
        .unwrap()
        .expect("runnable once the backoff elapses");
    assert_eq!(leased.attempt, 2);
    let next = fx
        .store
        .fail_job(leased.job_id, Code::FsLocked, None, t1)
        .unwrap();
    assert_eq!(next, JobStatus::Dead, "attempts spent, so it is DEAD");

    let (status, code): (String, String) = r
        .query_row(
            "SELECT status, last_error_code FROM jobs WHERE job_id = ?1",
            [enq.job_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "DEAD");
    assert_eq!(
        code, "FS_LOCKED",
        "a DEAD job keeps its error code for health"
    );
}

#[test]
fn a_non_retryable_failure_does_not_burn_attempts_on_retries() {
    let fx = fixture();
    fx.store
        .enqueue_job(NewJob::new("PARSE_FILE", "parse:v1:denied"))
        .unwrap();
    let t0 = Timestamp::now();
    let leased = fx
        .store
        .lease_job("w", Duration::from_secs(30), t0)
        .unwrap()
        .unwrap();
    // POL_DENIED is never retryable (§108). Retrying it would just be the same
    // answer, later.
    let next = fx
        .store
        .fail_job(leased.job_id, Code::PolDenied, None, t0)
        .unwrap();
    assert_eq!(next, JobStatus::Dead);
}

#[test]
fn completing_a_job_that_is_not_leased_is_an_invariant_violation() {
    let fx = fixture();
    let enq = fx
        .store
        .enqueue_job(NewJob::new("HASH_FILE", "hash:v1:x"))
        .unwrap();
    let err = fx
        .store
        .complete_job(enq.job_id, Timestamp::now())
        .expect_err("a PENDING job cannot be completed");
    assert_eq!(err.code(), Code::IntInvariantViolated);
}

// ------------------------------------------------------------------ migration

#[test]
fn backup_exists_before_a_migration_runs() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(DB_FILE_NAME);

    let store = Store::open(&db).unwrap();
    assert_eq!(store.schema_version(), marrow_core::SCHEMA_VERSION);

    let backups = migrate::backups_for(&db).unwrap();
    assert_eq!(backups.len(), 1, "one migration ran, so one backup exists");

    // It is a real database, and it is the *pre*-migration state: the backup of
    // a v0 database has none of the schema the migration then created.
    let backup = marrow_store::rusqlite::Connection::open(&backups[0]).unwrap();
    let tables: i64 = backup
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        tables, 0,
        "the backup was taken before the migration, not after"
    );

    // The live database, by contrast, has the whole schema.
    let live: i64 = store
        .reader()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(live as usize, marrow_store::schema::all_tables().len());
}

// --------------------------------------------------------------------- crash

#[test]
fn crash_mid_transaction_leaves_no_partial_state() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(DB_FILE_NAME);

    // Committed, durable prefix.
    let (workspace, root) = {
        let store = Store::open(&db).unwrap();
        let workspace = store
            .upsert_workspace(NewWorkspace::new("desktop"))
            .unwrap();
        let root = store
            .upsert_root(NewRoot::new(workspace, "/Users/test/Desktop"))
            .unwrap();
        store.close().unwrap();
        (workspace, root)
    };

    let file_id = {
        // A batch interval long enough that nothing commits on its own: the
        // only way out of this batch is a flush, a clean shutdown, or death.
        let store = Store::open_with_config(
            &db,
            WriterConfig {
                max_batch_rows: 100_000,
                max_batch_interval: Duration::from_secs(600),
                ..WriterConfig::default()
            },
        )
        .unwrap();

        // Queue a file and its version without waiting for the commit, then die
        // with the transaction open.
        let f = NewFile::new(workspace, root, "/Users/test/Desktop/half.md");
        let file_id = f.file_id;
        let v = NewVersion::new(
            file_id,
            "/Users/test/Desktop/half.md",
            5,
            ContentHash::of(b"half!"),
        );
        let pending = store
            .writer()
            .send(move |c| read::insert_file_with_version(c, &f, &v))
            .unwrap();
        store.abort();
        assert!(
            pending.wait().is_err(),
            "a caller whose batch died is told so, not left hanging"
        );
        file_id
    };

    // Reopen: the committed prefix is intact, the killed batch left nothing.
    let store = Store::open(&db).unwrap();
    let r = store.reader().unwrap();

    let ws_rows: i64 = r
        .query_row("SELECT count(*) FROM workspaces", [], |row| row.get(0))
        .unwrap();
    assert_eq!(ws_rows, 1, "work committed before the crash survives");
    assert!(
        read::find_file_by_id(&r, file_id).unwrap().is_none(),
        "the file from the killed batch is absent"
    );
    for table in ["files", "file_paths", "file_versions"] {
        let n: i64 = r
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(n, 0, "{table} has no partial row from the killed batch");
    }
    // And the store is still usable: no half-open transaction was left behind.
    let f = NewFile::new(workspace, root, "/Users/test/Desktop/whole.md");
    store
        .insert_file(f)
        .expect("store still writable after a crash");
}

// --------------------------------------------------------------- storage form

#[test]
fn timestamps_are_stored_as_integer_epoch_millis() {
    let fx = fixture();
    let at = Timestamp::from_millis(1_800_000_123_456);
    let mut f = NewFile::new(fx.workspace, fx.root, "/Users/test/Desktop/t.md");
    f.at = at;
    let mut v = NewVersion::new(
        f.file_id,
        "/Users/test/Desktop/t.md",
        1,
        ContentHash::of(b"t"),
    );
    v.observed_at = at;
    v.mtime_ms = Timestamp::from_millis(1_700_000_000_000);
    let file_id = fx.store.insert_file_with_version(f, v).unwrap().0;

    let r = fx.store.reader().unwrap();
    for (table, column, key) in [
        ("files", "created_at", "file_id"),
        ("files", "updated_at", "file_id"),
        ("file_paths", "observed_from", "file_id"),
        ("file_versions", "observed_at", "file_id"),
        ("file_versions", "mtime_ms", "file_id"),
    ] {
        let kind: String = r
            .query_row(
                &format!("SELECT typeof({column}) FROM {table} WHERE {key} = ?1"),
                [file_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(kind, "integer", "{table}.{column} must be INTEGER millis");
    }

    let stored: i64 = r
        .query_row(
            "SELECT created_at FROM files WHERE file_id = ?1",
            [file_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        stored,
        at.as_millis(),
        "millis are stored verbatim, not seconds"
    );

    // And they round-trip back as Timestamps.
    let version = read::current_version(&r, file_id).unwrap().unwrap();
    assert_eq!(version.observed_at, at);
    assert_eq!(version.mtime_ms.as_millis(), 1_700_000_000_000);
}

#[test]
fn ids_are_stored_as_text_ulid() {
    let fx = fixture();
    let file_id = fx.file_at("/Users/test/Desktop/id.md", b"id");
    let r = fx.store.reader().unwrap();

    for (table, column) in [
        ("files", "file_id"),
        ("files", "workspace_id"),
        ("files", "root_id"),
        ("files", "current_version_id"),
        ("file_paths", "path_id"),
        ("file_versions", "version_id"),
        ("workspaces", "workspace_id"),
        ("workspace_roots", "root_id"),
    ] {
        let (kind, value): (String, String) = r
            .query_row(
                &format!("SELECT typeof({column}), {column} FROM {table} LIMIT 1"),
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(kind, "text", "{table}.{column} must be TEXT");
        assert_eq!(value.len(), 26, "{table}.{column} must be a 26-char ULID");
        assert!(
            value.parse::<marrow_core::FileId>().is_ok(),
            "{table}.{column} must decode as a ULID: {value}"
        );
    }

    // Round-trips back into the typed id, not a bare string.
    let row = read::find_file_by_id(&r, file_id).unwrap().unwrap();
    assert_eq!(row.file_id, file_id);
    assert_eq!(row.workspace_id, fx.workspace);
    assert_eq!(row.root_id, fx.root);
}

// ------------------------------------------------------------------ plumbing

#[test]
fn upserting_a_workspace_and_root_twice_keeps_the_original_ids() {
    let fx = fixture();
    let again = fx
        .store
        .upsert_workspace(NewWorkspace::new("desktop"))
        .unwrap();
    assert_eq!(
        again, fx.workspace,
        "a workspace id is stable across upserts"
    );

    let mut root = NewRoot::new(fx.workspace, "/Users/test/Desktop");
    root.storage_kind = marrow_store::StorageKind::TieredCloud;
    root.cloud_provider = Some("icloud".into());
    let root_again = fx.store.upsert_root(root).unwrap();
    assert_eq!(root_again, fx.root, "a root id is stable across upserts");

    let r = fx.store.reader().unwrap();
    let (kind, provider): (String, String) = r
        .query_row(
            "SELECT storage_kind, cloud_provider FROM workspace_roots WHERE root_id = ?1",
            [fx.root.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(kind, "TIERED_CLOUD", "the upsert updated the row it found");
    assert_eq!(provider, "icloud");

    let n: i64 = r
        .query_row("SELECT count(*) FROM workspace_roots", [], |row| row.get(0))
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn readers_cannot_write() {
    let fx = fixture();
    let r = fx.store.reader().unwrap();
    let err = r
        .execute("DELETE FROM workspaces", [])
        .expect_err("a reader connection must be query_only");
    assert!(err.to_string().contains("readonly") || err.to_string().contains("read-only"));
}

#[test]
fn many_readers_can_read_while_the_writer_writes() {
    let fx = fixture();
    let readers: Vec<_> = (0..8).map(|_| fx.store.reader().unwrap()).collect();
    for i in 0..20 {
        fx.file_at(&format!("/Users/test/Desktop/f{i}.md"), b"x");
    }
    for r in &readers {
        let n: i64 = r
            .query_row("SELECT count(*) FROM files", [], |row| row.get(0))
            .unwrap();
        assert_eq!(n, 20, "WAL readers see committed writes without blocking");
    }
}

#[test]
fn an_in_memory_store_works_the_same_way() {
    let store = Store::open_in_memory().unwrap();
    let ws = store
        .upsert_workspace(NewWorkspace::new("scratch"))
        .unwrap();
    let root = store.upsert_root(NewRoot::new(ws, "/tmp/scratch")).unwrap();
    let f = NewFile::new(ws, root, "/tmp/scratch/a.txt");
    let id = f.file_id;
    store.insert_file(f).unwrap();
    let r = store.reader().unwrap();
    assert_eq!(read::find_file_by_id(&r, id).unwrap().unwrap().file_id, id);
}

// --------------------------------------------------------------------- tables

/// Two tables' worth of rows for one version, shaped the way the parse crate
/// produces them.
fn sample_tables(version: marrow_core::VersionId) -> Vec<read::NewTable> {
    let cell = |row: i64, col: i64, text: &str, ty: &str, typed: Option<&str>| read::NewCell {
        row_idx: row,
        col_idx: col,
        rowspan: 1,
        colspan: 1,
        raw_text: text.to_owned(),
        typed_value: typed.map(str::to_owned),
        value_type: Some(ty.to_owned()),
        formula: None,
        cell_span: format!(r#"{{"bytes":{{"start":{row},"end":{}}}}}"#, row + 1),
        confidence: 1.0,
    };
    vec![read::NewTable {
        table_id: ulid::Ulid::new().to_string(),
        version_id: version,
        node_ordinal: Some(0),
        source_span: r#"{"bytes":{"start":0,"end":40}}"#.to_owned(),
        n_rows: 2,
        n_cols: 2,
        header_rows: 1,
        header_cols: 0,
        header_row_idx: Some(0),
        header_confidence: 0.85,
        column_names: Some(r#"["part","qty"]"#.to_owned()),
        column_types: Some(r#"["string","integer"]"#.to_owned()),
        merged_regions: Some("[]".to_owned()),
        caption: None,
        extraction_method: "native_delimited".to_owned(),
        provenance_class: "EXACT".to_owned(),
        reconstruction: "EXACT".to_owned(),
        cells: vec![
            cell(0, 0, "part", "string", None),
            cell(0, 1, "qty", "string", None),
            cell(1, 0, "bolt", "string", None),
            cell(1, 1, "12", "integer", Some("12")),
        ],
    }]
}

fn version_of(fx: &Fixture, path: &str) -> marrow_core::VersionId {
    let file_id = fx.file_at(path, b"body");
    fx.store.flush().unwrap();
    let r = fx.store.reader().unwrap();
    read::current_version(&r, file_id)
        .unwrap()
        .unwrap()
        .version_id
}

#[test]
fn every_persisted_cell_carries_a_source_span() {
    // TBL-002 and hard rule 1: the column is NOT NULL, so a cell without a
    // location is refused by the database rather than by a code review.
    let fx = fixture();
    let version = version_of(&fx, "/Users/test/Desktop/parts.csv");
    let tables = sample_tables(version);
    let table_id = tables[0].table_id.clone();
    fx.store
        .writer()
        .submit(move |c| read::replace_tables(c, version, &tables))
        .unwrap();
    fx.store.flush().unwrap();

    let r = fx.store.reader().unwrap();
    let cells = read::cells_for(&r, &table_id).unwrap();
    assert_eq!(cells.len(), 4);
    for c in &cells {
        assert!(c.cell_span.contains("bytes"), "{c:?}");
    }

    // And the schema will not take one without.
    let err = fx
        .store
        .writer()
        .submit(move |c| {
            c.execute(
                "INSERT INTO table_cells
                    (cell_id, table_id, row_idx, col_idx, raw_text, cell_span)
                 VALUES ('x', ?1, 9, 9, 'orphan', NULL)",
                [&table_id],
            )
            .map_err(|e| marrow_store::map_sqlite(e, "test"))
        })
        .unwrap_err();
    assert_eq!(err.code(), Code::IntInvariantViolated);
}

#[test]
fn a_tables_raw_text_and_header_confidence_survive_the_round_trip() {
    // TBL-003 and TBL-005: the confidence is stored, and the raw text is stored
    // next to the typed value rather than replaced by it.
    let fx = fixture();
    let version = version_of(&fx, "/Users/test/Desktop/parts.csv");
    let tables = sample_tables(version);
    fx.store
        .writer()
        .submit(move |c| read::replace_tables(c, version, &tables))
        .unwrap();
    fx.store.flush().unwrap();

    let r = fx.store.reader().unwrap();
    let rows = read::tables_for(&r, version).unwrap();
    assert_eq!(rows.len(), 1);
    assert!((rows[0].header_confidence - 0.85).abs() < 1e-9);
    assert_eq!(rows[0].header_row_idx, Some(0));
    assert_eq!(
        rows[0].column_types.as_deref(),
        Some(r#"["string","integer"]"#)
    );

    let qty = read::cells_for(&r, &rows[0].table_id)
        .unwrap()
        .into_iter()
        .find(|c| c.row_idx == 1 && c.col_idx == 1)
        .unwrap();
    assert_eq!(qty.raw_text, "12", "the raw text is never replaced");
    assert_eq!(qty.typed_value.as_deref(), Some("12"));
}

#[test]
fn re_parsing_replaces_a_versions_tables_rather_than_accumulating_them() {
    let fx = fixture();
    let version = version_of(&fx, "/Users/test/Desktop/parts.csv");
    for _ in 0..3 {
        let tables = sample_tables(version);
        fx.store
            .writer()
            .submit(move |c| read::replace_tables(c, version, &tables))
            .unwrap();
    }
    fx.store.flush().unwrap();
    let r = fx.store.reader().unwrap();
    assert_eq!(read::tables_for(&r, version).unwrap().len(), 1);
    let cells: i64 = r
        .conn()
        .query_row("SELECT count(*) FROM table_cells", [], |row| row.get(0))
        .unwrap();
    assert_eq!(cells, 4, "cascade took the old cells with the old table");
}

#[test]
fn deleting_a_file_version_takes_its_tables_with_it() {
    // Derived state, so it must not outlive what it was derived from — an
    // orphaned table row is a citation into a file that is gone.
    let fx = fixture();
    let version = version_of(&fx, "/Users/test/Desktop/parts.csv");
    let tables = sample_tables(version);
    fx.store
        .writer()
        .submit(move |c| read::replace_tables(c, version, &tables))
        .unwrap();
    fx.store
        .writer()
        .submit(move |c| {
            c.execute(
                "DELETE FROM file_versions WHERE version_id = ?1",
                [version.to_string()],
            )
            .map_err(|e| marrow_store::map_sqlite(e, "test"))
        })
        .unwrap();
    fx.store.flush().unwrap();
    let r = fx.store.reader().unwrap();
    let n: i64 = r
        .conn()
        .query_row("SELECT count(*) FROM table_cells", [], |row| row.get(0))
        .unwrap();
    assert_eq!(n, 0);
}

#[test]
fn a_reconstruction_grade_outside_the_taxonomy_is_refused() {
    let fx = fixture();
    let version = version_of(&fx, "/Users/test/Desktop/parts.csv");
    let mut tables = sample_tables(version);
    tables[0].reconstruction = "PROBABLY_FINE".to_owned();
    let err = fx
        .store
        .writer()
        .submit(move |c| read::replace_tables(c, version, &tables))
        .unwrap_err();
    assert_eq!(err.code(), Code::IntInvariantViolated);
}

/// **A count of searchable chunks must agree with what searching returns.**
///
/// `chunks.status` has a `SUPERSEDED` value that nothing has ever written, so a
/// chunk stays ACTIVE after its version is superseded or its file is deleted.
/// Counting on that column alone over-reported by 4.6x on the author's real
/// index — 274,519 counted against 59,197 a search could reach — and sent
/// `marrow embed` to a two-hour job of which four fifths would have embedded
/// text nothing can ever retrieve, while `marrow status` suggested running it.
#[test]
fn the_chunk_count_excludes_what_search_can_no_longer_return() {
    let fx = fixture();
    let file_id = fx.file_at("/Users/test/Desktop/notes.md", b"the renewal clause");

    let version: String = fx
        .store
        .reader()
        .expect("reader")
        .query_row(
            "SELECT version_id FROM file_versions WHERE file_id = ?1",
            [file_id.to_string()],
            |r| r.get(0),
        )
        .expect("a version");
    let vid: marrow_core::VersionId = version.parse().expect("version id");

    let chunk = marrow_store::read::NewChunk {
        chunk_id: marrow_core::ChunkId::new(),
        version_id: vid,
        chunk_kind: "TEXT".into(),
        text: "the renewal clause".into(),
        context_prefix: None,
        token_count: 3,
        text_hash: ContentHash::of(b"the renewal clause"),
        chunker_version: "test".into(),
        provenance_class: "EXACT".into(),
    };
    fx.store
        .writer()
        .submit(move |c| marrow_store::read::replace_chunks(c, vid, std::slice::from_ref(&chunk)))
        .expect("write chunk");
    fx.store.flush().expect("flush");

    let count =
        || marrow_store::read::chunk_count(&fx.store.reader().expect("reader")).expect("count");
    assert_eq!(count(), 1, "a live chunk is searchable");

    // Supersede the version. Nothing touches `chunks.status` — that is exactly
    // the point, and why counting on it alone was wrong.
    let v = vid;
    fx.store
        .writer()
        .submit(move |c| {
            c.execute(
                "UPDATE file_versions SET status='HISTORICAL' WHERE version_id = ?1",
                [v.to_string()],
            )
            .map(|_| ())
            .map_err(|e| marrow_store::map_sqlite(e, "superseding"))
        })
        .expect("supersede");
    fx.store.flush().expect("flush");
    assert_eq!(
        count(),
        0,
        "a chunk of a superseded version was counted as searchable"
    );

    // The other half of the same predicate: a deleted file.
    fx.store
        .writer()
        .submit(move |c| {
            c.execute(
                "UPDATE file_versions SET status='CURRENT' WHERE version_id = ?1",
                [v.to_string()],
            )
            .map(|_| ())
            .map_err(|e| marrow_store::map_sqlite(e, "restoring"))
        })
        .expect("restore");
    fx.store.flush().expect("flush");
    assert_eq!(count(), 1);

    fx.store
        .writer()
        .submit(move |c| {
            c.execute(
                "UPDATE files SET status='DELETED' WHERE file_id = ?1",
                [file_id.to_string()],
            )
            .map(|_| ())
            .map_err(|e| marrow_store::map_sqlite(e, "deleting"))
        })
        .expect("delete");
    fx.store.flush().expect("flush");
    assert_eq!(
        count(),
        0,
        "a chunk of a deleted file was counted as searchable"
    );
}
