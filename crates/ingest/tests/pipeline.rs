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
}

#[test]
fn a_rename_keeps_the_file_id_and_records_path_history() {
    // **Invariant #2, the load-bearing one.** If a rename minted a new FileId,
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
    assert_eq!(out.stored, 1, "still recorded");
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
fn files_are_recorded_as_user_origin_not_self() {
    // **Invariant #13.** Content the agent wrote is barred from supporting a
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
