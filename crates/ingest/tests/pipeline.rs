//! Integration tests for the ingest pipeline, against a real filesystem and a
//! real database. No mocks — M0 proved the interesting behaviour is in the
//! actual syscalls (LLD §9).

use std::sync::Arc;

use marrow_core::{FileStatus, Origin, Timestamp};
use marrow_ingest::{ingest_root, Cancel, IngestPolicy, Progress, Stage};
use marrow_scan::AuthorizedRoot;
use marrow_store::read::{NewRoot, NewWorkspace, StorageKind};
use marrow_store::Store;

struct Fixture {
    _dir: tempfile::TempDir,
    root_path: std::path::PathBuf,
    store: Store,
    ws: marrow_core::WorkspaceId,
    root_id: marrow_core::RootId,
    root: AuthorizedRoot,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root_path = dir.path().join("corpus");
    std::fs::create_dir_all(&root_path).unwrap();

    // File-backed, not `open_in_memory`. An in-memory store uses SQLite
    // shared-cache mode, which locks at table granularity instead of using
    // WAL's MVCC — so a long-lived reader blocks the writer there and not on
    // disk. Testing against memory would have passed code that deadlocks in
    // production, and failed code that works. Test what ships.
    let store = Store::open(dir.path().join("marrow.sqlite")).unwrap();
    let now = Timestamp::now();
    let ws = store
        .upsert_workspace(NewWorkspace {
            workspace_id: marrow_core::WorkspaceId::new(),
            name: "test".into(),
            at: now,
        })
        .unwrap();

    let root = AuthorizedRoot::open(&root_path).unwrap();
    // macOS tempdirs live under /var, which is a symlink to /private/var, so
    // `TempDir::path()` is never canonical. The walk yields canonical paths, so
    // a fixture that builds expectations from the raw tempdir path compares two
    // different strings for the same file. Use the canonicalized root instead.
    let root_path = root.path().to_path_buf();
    let root_id = store
        .upsert_root(NewRoot {
            root_id: marrow_core::RootId::new(),
            workspace_id: ws,
            canonical_path: root.path().to_string_lossy().into_owned(),
            volume_identity: None,
            grant_token: None,
            storage_kind: StorageKind::Local,
            cloud_provider: None,
            at: now,
        })
        .unwrap();

    Fixture {
        _dir: dir,
        root_path,
        store,
        ws,
        root_id,
        root,
    }
}

impl Fixture {
    fn write(&self, rel: &str, body: &str) -> std::path::PathBuf {
        let p = self.root_path.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, body).unwrap();
        p
    }

    fn run(&self) -> (marrow_ingest::IngestOutcome, Arc<Progress>) {
        self.run_with(&IngestPolicy::default(), &Cancel::new())
    }

    fn run_with(
        &self,
        policy: &IngestPolicy,
        cancel: &Cancel,
    ) -> (marrow_ingest::IngestOutcome, Arc<Progress>) {
        let progress = Arc::new(Progress::new());
        let out = ingest_root(
            &self.store,
            self.ws,
            self.root_id,
            &self.root,
            policy,
            &progress,
            cancel,
        )
        .unwrap();
        (out, progress)
    }

    /// The watcher path: hand the pipeline a set of paths rather than a tree.
    fn hint(&self, paths: &[std::path::PathBuf]) -> marrow_ingest::IngestOutcome {
        marrow_ingest::apply_hints(
            &self.store,
            self.ws,
            self.root_id,
            &self.root,
            &IngestPolicy::default(),
            &paths.iter().cloned().collect(),
            &Arc::new(Progress::new()),
            &Cancel::new(),
            None,
        )
        .unwrap()
    }

    fn is_indexed(&self, p: &std::path::Path) -> bool {
        let conn = self.store.reader().unwrap();
        marrow_store::read::find_file_by_path(&conn, self.root_id, &p.to_string_lossy())
            .unwrap()
            .is_some()
    }
}

#[test]
fn a_rename_keeps_the_file_id_and_records_path_history() {
    // **Path is never identity — the load-bearing one.** If a rename minted a new FileId,
    // every derived artifact — chunks, vectors, evidence — would be orphaned
    // on every `mv`.
    let f = fixture();
    let before = f.write("notes.md", "# hello\n");
    let (out, _) = f.run();
    assert_eq!(out.stored, 1);

    let conn = f.store.reader().unwrap();
    let path_str = before.to_string_lossy().into_owned();
    let original = marrow_store::read::find_file_by_path(&conn, f.root_id, &path_str)
        .unwrap()
        .unwrap();
    drop(conn);

    let after = f.root_path.join("renamed.md");
    std::fs::rename(&before, &after).unwrap();
    f.run();

    let conn = f.store.reader().unwrap();
    let moved = marrow_store::read::find_file_by_path(&conn, f.root_id, &after.to_string_lossy())
        .unwrap()
        .expect("file should be found at its new path");

    assert_eq!(
        original.file_id, moved.file_id,
        "a rename must not mint a new FileId"
    );

    let history = marrow_store::read::path_history(&conn, moved.file_id).unwrap();
    assert!(
        history.len() >= 2,
        "the old path must be retained as history, got {history:?}"
    );
}

#[test]
fn an_unchanged_file_produces_no_second_version() {
    // Idempotency: re-running over a static corpus must converge, not grow.
    let f = fixture();
    f.write("a.txt", "unchanged");
    let (first, _) = f.run();
    assert_eq!(first.stored, 1);

    let (second, _) = f.run();
    assert_eq!(
        second.stored, 0,
        "nothing changed, nothing should be written"
    );
    assert_eq!(second.unchanged, 1);

    let conn = f.store.reader().unwrap();
    let file = marrow_store::read::find_file_by_path(
        &conn,
        f.root_id,
        &f.root_path.join("a.txt").to_string_lossy(),
    )
    .unwrap()
    .unwrap();
    let versions = marrow_store::read::versions_for(&conn, file.file_id).unwrap();
    assert_eq!(versions.len(), 1, "one observation, one version");
}

#[test]
fn changed_content_produces_a_new_current_version() {
    let f = fixture();
    let p = f.write("a.txt", "one");
    f.run();
    std::fs::write(&p, "two").unwrap();
    let (out, _) = f.run();
    assert_eq!(out.stored, 1);

    let conn = f.store.reader().unwrap();
    let file = marrow_store::read::find_file_by_path(&conn, f.root_id, &p.to_string_lossy())
        .unwrap()
        .unwrap();
    let versions = marrow_store::read::versions_for(&conn, file.file_id).unwrap();
    assert_eq!(versions.len(), 2);
    // The one-CURRENT-version invariant is the store's, but it is the pipeline
    // that would violate it, so assert it from here too.
    let current = versions
        .iter()
        .filter(|v| v.status == marrow_core::VersionStatus::Current)
        .count();
    assert_eq!(current, 1);
}

#[test]
fn a_touch_that_changes_no_bytes_is_not_a_change() {
    // Onyx re-indexes when *either* the timestamp advanced or the content hash
    // moved, for a document whose extracted text is identical while the
    // document changed. Marrow hashes the whole file before any parser runs, so
    // that case cannot arise here — and adopting the timestamp half would make
    // `touch`, a sync client's metadata pass, `cp -p` and a restore from backup
    // each mint a version and re-parse the file. On a cloud-synced root that is
    // the whole root, on every sweep, for ever. See the gate in `record`.
    let f = fixture();
    let p = f.write("a.txt", "unchanged");
    let (first, _) = f.run();
    assert_eq!(first.stored, 1);

    // Ten minutes into the future: far enough that no filesystem timestamp
    // granularity can hide it, which is what makes this an assertion about the
    // gate rather than about the clock.
    let handle = std::fs::File::options().write(true).open(&p).unwrap();
    let touched = std::time::SystemTime::now() + std::time::Duration::from_secs(600);
    handle.set_modified(touched).unwrap();
    drop(handle);

    let (second, _) = f.run();
    assert_eq!(
        second.stored, 0,
        "an mtime advance with identical bytes must not be a change"
    );
    assert_eq!(second.unchanged, 1);

    let conn = f.store.reader().unwrap();
    let file = marrow_store::read::find_file_by_path(&conn, f.root_id, &p.to_string_lossy())
        .unwrap()
        .unwrap();
    let versions = marrow_store::read::versions_for(&conn, file.file_id).unwrap();
    assert_eq!(versions.len(), 1, "a touch is not an edit");
}

#[test]
fn a_rewrite_that_restores_the_mtime_is_still_a_change() {
    // The other half of the argument, and the reason the hash gate is strictly
    // stronger than a timestamp gate rather than merely different. A sync
    // client that rewrites a file and puts its mtime back — every one of them
    // does, restoring from its own server copy — is invisible to
    // `doc_updated_at` and caught here. Missing this is a stale index, which is
    // the failure the author has said is worse than no index at all.
    let f = fixture();
    let p = f.write("a.txt", "one");
    f.run();

    let before = std::fs::metadata(&p).unwrap().modified().unwrap();
    std::fs::write(&p, "two").unwrap();
    let handle = std::fs::File::options().write(true).open(&p).unwrap();
    handle.set_modified(before).unwrap();
    drop(handle);
    assert_eq!(
        std::fs::metadata(&p).unwrap().modified().unwrap(),
        before,
        "the fixture must actually have restored the mtime, or this proves nothing"
    );

    let (out, _) = f.run();
    assert_eq!(out.stored, 1, "different bytes are a change at any mtime");

    let conn = f.store.reader().unwrap();
    let file = marrow_store::read::find_file_by_path(&conn, f.root_id, &p.to_string_lossy())
        .unwrap()
        .unwrap();
    let versions = marrow_store::read::versions_for(&conn, file.file_id).unwrap();
    assert_eq!(versions.len(), 2);
}

#[test]
fn a_run_records_every_discovered_file() {
    let f = fixture();
    for i in 0..50 {
        f.write(&format!("dir{}/f{i}.txt", i % 5), &format!("body {i}"));
    }
    let (out, progress) = f.run();
    assert_eq!(out.discovered, 50);
    assert_eq!(out.stored, 50);
    assert_eq!(progress.get(Stage::Hashed), 50);
    assert_eq!(progress.get(Stage::SkippedPlaceholder), 0);
}

#[test]
fn noise_directories_never_reach_the_store() {
    let f = fixture();
    f.write("src/real.rs", "fn main() {}");
    f.write("node_modules/pkg/index.js", "module.exports = {}");
    f.write("target/debug/build.rs", "// generated");

    let (out, _) = f.run();
    assert_eq!(
        out.discovered, 1,
        "only src/real.rs should be discovered, got {out:?}"
    );
}

/// **A hint is a prompt to look, not an exemption from the walk policy.**
///
/// The sweep prunes noise directories by never descending into them.
/// `apply_hints` descends nothing — it is handed finished paths — so it used to
/// index anything a watcher noticed, including files created inside
/// `node_modules`, `.git` and `target`. The next full sweep pruned them again,
/// which churns the index and poisons ranking for as long as the window lasts:
/// `.git/config` outranked the real documentation for "admission control".
///
/// The pairing with `noise_directories_never_reach_the_store` is the point. The
/// two entry points have to reach the same verdict, and only one of them was
/// ever asserted.
#[test]
fn a_hinted_file_under_an_excluded_directory_is_never_indexed() {
    let f = fixture();
    let real = f.write("src/real.rs", "fn main() {}");
    let excluded = [
        f.write("node_modules/pkg/index.js", "module.exports = {}"),
        f.write(".git/config", "[core]\n"),
        f.write("target/debug/build.rs", "// generated"),
        // Not a noise directory — the other half of the same policy, which the
        // hint path was also skipping.
        f.write(".env", "SECRET=1\n"),
    ];

    let mut hinted = vec![real.clone()];
    hinted.extend(excluded.iter().cloned());
    let out = f.hint(&hinted);

    assert_eq!(
        out.discovered, 1,
        "only src/real.rs should survive the policy, got {out:?}"
    );
    assert_eq!(out.stored, 1, "{out:?}");
    assert!(f.is_indexed(&real), "the one legitimate hint was dropped");
    for p in &excluded {
        assert!(
            !f.is_indexed(p),
            "{} was indexed by a hint; the next sweep would prune it again",
            p.display()
        );
    }
}

/// A row an earlier build wrote under a directory that is excluded *now* still
/// has to be retirable.
///
/// The exclusion check sits after the vanished-path branch for exactly this
/// reason. Refusing to look at an excluded path at all would strand every such
/// row ACTIVE forever, because the sweep can no longer reach the directory to
/// notice the file is gone — the failure `persistable.rs` records for the
/// 43,686 files an earlier build left behind.
#[test]
fn a_hint_still_retires_a_row_under_a_directory_that_is_excluded_now() {
    let f = fixture();
    let stale = f.write("vendor/lib.rs", "// indexed before vendor was pruned");

    // Put the row there the way the earlier build would have: a hint under a
    // policy that did not exclude `vendor`.
    let lenient = IngestPolicy {
        walk: marrow_scan::WalkPolicy::default().include_dir("vendor"),
        ..Default::default()
    };
    marrow_ingest::apply_hints(
        &f.store,
        f.ws,
        f.root_id,
        &f.root,
        &lenient,
        &[stale.clone()].into_iter().collect(),
        &Arc::new(Progress::new()),
        &Cancel::new(),
        None,
    )
    .unwrap();
    let conn = f.store.reader().unwrap();
    let file_id = marrow_store::read::find_file_by_path(&conn, f.root_id, &stale.to_string_lossy())
        .unwrap()
        .expect("the fixture never got its stale row")
        .file_id;
    drop(conn);

    std::fs::remove_file(&stale).unwrap();
    f.hint(std::slice::from_ref(&stale));

    // By id, not by path: retiring a row clears `current_path`, which is what
    // makes the soft delete a soft delete rather than a second row waiting to
    // be minted at the same path.
    let conn = f.store.reader().unwrap();
    let status: String = conn
        .query_row(
            "SELECT status FROM files WHERE file_id = ?1",
            [file_id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        status, "DELETED",
        "an excluded path that vanished must still retire its row"
    );
}

#[test]
fn a_file_over_the_hash_budget_is_recorded_but_not_read() {
    // FS-015: a huge file stays discoverable from metadata rather than being
    // dropped or blocking the run.
    let f = fixture();
    f.write("big.bin", "0123456789");
    let policy = IngestPolicy {
        max_hash_bytes: 4,
        ..Default::default()
    };

    let (out, progress) = f.run_with(&policy, &Cancel::new());
    assert_eq!(out.stored, 1, "still recorded; outcome was {out:?}");
    assert_eq!(progress.get(Stage::Hashed), 0, "but never hashed");
}

#[test]
fn cancellation_stops_the_run_and_leaves_the_store_consistent() {
    let f = fixture();
    for i in 0..500 {
        f.write(&format!("f{i}.txt"), &format!("body {i}"));
    }
    let cancel = Cancel::new();
    cancel.cancel(); // pre-cancelled: the run must not proceed

    let (out, _) = f.run_with(&IngestPolicy::default(), &cancel);
    assert!(out.cancelled);
    assert!(
        out.stored < 500,
        "a cancelled run must not have stored everything"
    );

    // Whatever did land must be well-formed: every file has exactly one
    // current version.
    let conn = f.store.reader().unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM files f
             WHERE (SELECT count(*) FROM file_versions v
                    WHERE v.file_id = f.file_id AND v.status = 'CURRENT') != 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        n, 0,
        "cancelling must not leave a file without a current version"
    );
}

#[test]
fn an_unreadable_file_is_counted_not_fatal() {
    // FS-011: the workspace keeps indexing around a file it cannot read.
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    f.write("readable.txt", "fine");
    let bad = f.write("locked.txt", "secret");
    std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o000)).unwrap();

    let (out, _) = f.run();

    // restore so the tempdir can clean up
    std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o644)).unwrap();

    assert_eq!(out.discovered, 2, "both are discovered");
    assert!(out.stored >= 1, "the readable one is still stored");
}

#[test]
fn a_hinted_file_that_cannot_be_read_is_recorded_and_counted_exactly_once() {
    // The hint path fails open on the content stage, which is right — the file
    // stays findable by name (FS-011) — but it used to do so *silently*, with
    // `if let Ok(n) = extract(..)`, and it ran the parser over a file the hash
    // stage had already failed to open. So a watcher-driven re-parse reported
    // either nothing at all or two failures for one file, and the desktop's own
    // edit loop was the one path where a parser regression left no trace.
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    let bad = f.write("locked.txt", "secret");
    std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o000)).unwrap();

    let out = f.hint(std::slice::from_ref(&bad));

    // restore so the tempdir can clean up
    std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o644)).unwrap();

    assert!(
        f.is_indexed(&bad),
        "a file that could not be read is still recorded from its metadata"
    );
    assert_eq!(
        out.failed, 1,
        "one unreadable file is one failure, reported: {:?}",
        out.failures
    );
}

#[test]
fn files_are_recorded_as_user_origin_not_self() {
    // **The `origin = SELF` rule.** Content the agent wrote is barred from supporting a
    // claim; content the user wrote is not. Ingest must never mark discovered
    // files as SELF.
    let f = fixture();
    f.write("a.txt", "user content");
    f.run();

    let conn = f.store.reader().unwrap();
    let file = marrow_store::read::find_file_by_path(
        &conn,
        f.root_id,
        &f.root_path.join("a.txt").to_string_lossy(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(file.origin, Origin::User);
    assert_eq!(file.status, FileStatus::Active);
}

#[test]
fn every_discovered_file_is_actually_stored() {
    // The counter that matters. `discovered` coming from the walk and `stored`
    // coming from the writer are produced by different threads through
    // different channels, so a mismatch is the first sign the pipeline is
    // dropping work — which is precisely how the shared-cache reader/writer
    // deadlock showed up: 50 hashed, 4 stored.
    let f = fixture();
    for i in 0..200 {
        f.write(&format!("d{}/f{i}.txt", i % 7), &format!("body {i}"));
    }
    let (out, _) = f.run();
    assert_eq!(out.discovered, 200);
    assert_eq!(
        out.stored, out.discovered,
        "every discovered file must be stored; {out:?}"
    );
    assert_eq!(out.failed, 0, "no write should fail: {out:?}");
}

#[test]
fn hardlinks_are_distinct_files_and_the_index_converges() {
    // Two paths to one inode are two files, not one. Treating an inode match
    // as identity unconditionally makes them fight over `current_path`, so
    // every scan reports a change and the index never settles.
    //
    // Found on the real corpus: macOS Photos libraries hardlink their Spotlight
    // journals, which made two files show as "new" on every single run.
    let f = fixture();
    let a = f.write("a.txt", "shared bytes");
    let b = f.root_path.join("b.txt");
    std::fs::hard_link(&a, &b).unwrap();

    let (first, _) = f.run();
    assert_eq!(first.discovered, 2);
    assert_eq!(first.stored, 2, "both paths are files: {first:?}");

    // The property that actually matters: a second run changes nothing.
    let (second, _) = f.run();
    assert_eq!(
        second.stored, 0,
        "re-running over an unchanged corpus must be a no-op, got {second:?}"
    );
    assert_eq!(second.unchanged, 2);
}

#[test]
fn a_rename_is_still_detected_when_the_old_path_is_gone() {
    // The other half of the hardlink fix: identity must still resolve a rename.
    let f = fixture();
    let before = f.write("before.txt", "same bytes");
    f.run();

    let conn = f.store.reader().unwrap();
    let original =
        marrow_store::read::find_file_by_path(&conn, f.root_id, &before.to_string_lossy())
            .unwrap()
            .unwrap();
    drop(conn);

    let after = f.root_path.join("after.txt");
    std::fs::rename(&before, &after).unwrap();
    f.run();

    let conn = f.store.reader().unwrap();
    let moved = marrow_store::read::find_file_by_path(&conn, f.root_id, &after.to_string_lossy())
        .unwrap()
        .expect("renamed file must be findable at its new path");
    assert_eq!(original.file_id, moved.file_id, "a rename keeps its FileId");
}

#[test]
fn a_csv_is_ingested_as_a_table_with_its_cells_and_their_spans() {
    // The Table IR reaches the database, not just the parser. `table_ir` and
    // `table_cells` exist so a numeric question can be answered by evaluating
    // over cells and citing them (§99.3); a schema nothing writes cannot do
    // that, which is what `ir_nodes` has been demonstrating since M1.
    let f = fixture();
    let body = "part,qty,price\nbolt,12,0.40\nnut,144,0.02\n";
    f.write("parts.csv", body);
    let (out, _) = f.run();
    assert_eq!(out.stored, 1);

    let conn = f.store.reader().unwrap();
    let version = marrow_store::read::current_version(
        &conn,
        marrow_store::read::find_file_by_path(
            &conn,
            f.root_id,
            &f.root_path.join("parts.csv").to_string_lossy(),
        )
        .unwrap()
        .unwrap()
        .file_id,
    )
    .unwrap()
    .unwrap()
    .version_id;

    let tables = marrow_store::read::tables_for(&conn, version).unwrap();
    assert_eq!(tables.len(), 1, "one table in one CSV");
    let t = &tables[0];
    assert_eq!((t.n_rows, t.n_cols), (3, 3));
    assert_eq!(t.header_row_idx, Some(0));
    assert!(t.header_confidence >= 0.9, "{t:?}");
    assert_eq!(t.extraction_method, "native_delimited");
    assert_eq!(t.reconstruction, "EXACT");
    assert_eq!(t.column_names.as_deref(), Some(r#"["part","qty","price"]"#));
    assert_eq!(
        t.column_types.as_deref(),
        Some(r#"["string","integer","decimal"]"#)
    );

    let cells = marrow_store::read::cells_for(&conn, &t.table_id).unwrap();
    assert_eq!(cells.len(), 9);
    let qty = cells
        .iter()
        .find(|c| c.row_idx == 1 && c.col_idx == 1)
        .unwrap();
    assert_eq!(qty.raw_text, "12");
    assert_eq!(qty.typed_value.as_deref(), Some("12"));
    assert_eq!(qty.value_type.as_deref(), Some("integer"));
    // TBL-002: the stored span resolves back into the file's own bytes.
    let span: marrow_core::SourceSpan = serde_json::from_str(&qty.cell_span).unwrap();
    let marrow_core::SourceSpan::Bytes { start, end } = span else {
        panic!("a CSV cell is a byte range, not {span:?}");
    };
    assert_eq!(&body[start as usize..end as usize], "12");

    // TBL-011: the schema chunk is persisted as its own kind, not as prose.
    let kinds: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT DISTINCT chunk_kind FROM chunks ORDER BY chunk_kind")
            .unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
        rows.map(|r| r.unwrap()).collect()
    };
    assert_eq!(kinds, vec!["TABLE_BAND", "TABLE_SCHEMA"], "{kinds:?}");
}

#[test]
fn a_file_that_stops_being_a_table_has_no_table_at_its_current_version() {
    // The old version keeps its rows, exactly as its chunks do — history is not
    // a lie, it is history. What must not happen is the *current* version still
    // answering with a grid the file no longer contains.
    let f = fixture();
    f.write("parts.csv", "part,qty\nbolt,12\nnut,144\n");
    f.run();

    f.write("parts.csv", "just a line of prose\n");
    f.run();

    let conn = f.store.reader().unwrap();
    let file_id = marrow_store::read::find_file_by_path(
        &conn,
        f.root_id,
        &f.root_path.join("parts.csv").to_string_lossy(),
    )
    .unwrap()
    .unwrap()
    .file_id;
    let version = marrow_store::read::current_version(&conn, file_id)
        .unwrap()
        .unwrap()
        .version_id;
    assert!(
        marrow_store::read::tables_for(&conn, version)
            .unwrap()
            .is_empty(),
        "the current version must not still offer a table"
    );
}
