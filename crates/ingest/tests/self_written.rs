//! Invariant #9, end to end: what this system writes cannot be cited back.
//!
//! The half of it that lives in `marrow-tools` — marking a write `origin =
//! SELF` — was already true. This is the other half, and it is the one that
//! matters: `files.origin` defaults to `'USER'`, a scan cannot tell agent
//! output from a document the user typed, and without a record of what was
//! written the next reconciliation quietly reclassifies the system's own words
//! as evidence.

use std::sync::Arc;

use marrow_core::{ContentHash, Origin, RootId, Timestamp, WorkspaceId};
use marrow_ingest::{ingest_root_with_index, Cancel, IngestPolicy, Progress};
use marrow_scan::AuthorizedRoot;
use marrow_store::{NewRoot, NewWorkspace, StorageKind, Store};

struct Fixture {
    _dir: tempfile::TempDir,
    corpus: tempfile::TempDir,
    store: Store,
    ws: WorkspaceId,
    root_id: RootId,
    root: AuthorizedRoot,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let corpus = tempfile::tempdir().unwrap();
    let store = Store::open_with_migrations(
        dir.path().join(marrow_store::DB_FILE_NAME),
        &[marrow_index::fts5::MIGRATION],
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
    let root = AuthorizedRoot::open(corpus.path()).unwrap();
    let root_id = store
        .upsert_root(NewRoot {
            root_id: RootId::new(),
            workspace_id: ws,
            canonical_path: root.path().to_string_lossy().into_owned(),
            volume_identity: None,
            grant_token: None,
            storage_kind: StorageKind::Local,
            cloud_provider: None,
            at: now,
        })
        .unwrap();
    store.flush().unwrap();
    Fixture {
        _dir: dir,
        corpus,
        store,
        ws,
        root_id,
        root,
    }
}

fn run(f: &Fixture) {
    let index = marrow_index::Fts5Index::open(&f.store).unwrap();
    ingest_root_with_index(
        &f.store,
        f.ws,
        f.root_id,
        &f.root,
        &IngestPolicy::default(),
        &Arc::new(Progress::new()),
        &Cancel::new(),
        Some(&index),
    )
    .unwrap();
    f.store.flush().unwrap();
}

fn origin_of(f: &Fixture, name: &str) -> Origin {
    let conn = f.store.reader().unwrap();
    // The *canonical* path: `TempDir::path()` is `/var/...` and the root
    // resolves to `/private/var/...` on macOS. Querying by the uncanonicalised
    // spelling finds nothing and reads as "the file was not indexed".
    let path = f.root.path().join(name).to_string_lossy().into_owned();
    let raw: String = conn
        .query_row(
            "SELECT origin FROM files WHERE current_path = ?1",
            [path],
            |r| r.get(0),
        )
        .unwrap_or_else(|e| panic!("{name} is not in the index: {e}"));
    match raw.as_str() {
        "SELF" => Origin::SelfWritten,
        _ => Origin::User,
    }
}

fn remember(f: &Fixture, bytes: &[u8], name: &str) {
    let hash = ContentHash::of(bytes);
    let path = f.corpus.path().join(name).to_string_lossy().into_owned();
    f.store
        .writer()
        .submit(move |conn| {
            marrow_store::read::record_self_written(
                conn,
                hash,
                &path,
                "txn-1",
                "create_file",
                Timestamp::now(),
            )
        })
        .unwrap();
}

const AGENT: &[u8] = b"# Summary\n\nThe lease renews in 2031, as I concluded earlier.\n";
const HUMAN: &[u8] = b"# Lease\n\nThe agreement renews on 31 December 2031.\n";

#[test]
fn a_file_this_system_wrote_is_indexed_as_self_written_and_cannot_be_cited() {
    // The whole point. Without the record, this file comes back as the user's
    // own work and the system cites itself as independent corroboration.
    let f = fixture();
    std::fs::write(f.corpus.path().join("summary.md"), AGENT).unwrap();
    std::fs::write(f.corpus.path().join("lease.md"), HUMAN).unwrap();
    remember(&f, AGENT, "summary.md");

    run(&f);

    assert_eq!(origin_of(&f, "summary.md"), Origin::SelfWritten);
    assert!(!origin_of(&f, "summary.md").can_support_a_claim());
    assert_eq!(origin_of(&f, "lease.md"), Origin::User);
    assert!(origin_of(&f, "lease.md").can_support_a_claim());
}

#[test]
fn the_chunks_carry_the_same_origin_as_the_file() {
    // A citation is built from a chunk. If the file row says SELF and the
    // chunk says USER, the check is in the wrong place and the quote gets out.
    let f = fixture();
    std::fs::write(f.corpus.path().join("summary.md"), AGENT).unwrap();
    remember(&f, AGENT, "summary.md");
    run(&f);

    // The lexical index carries the origin, because that is where a citation
    // is built from.
    let conn = f.store.reader().unwrap();
    let (total, selfish): (i64, i64) = conn
        .query_row(
            "SELECT count(*), sum(origin = 'SELF') FROM text_index_docs",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(total > 0, "nothing was chunked, so nothing was proved");
    assert_eq!(
        selfish, total,
        "every chunk of a self-written file must say so"
    );
}

#[test]
fn the_record_survives_a_reindex() {
    // A record that only holds until the next scan is not a record. This is
    // the failure the whole table exists to prevent.
    let f = fixture();
    std::fs::write(f.corpus.path().join("summary.md"), AGENT).unwrap();
    remember(&f, AGENT, "summary.md");
    run(&f);
    assert_eq!(origin_of(&f, "summary.md"), Origin::SelfWritten);

    // Touch it so the scanner re-examines it, and run again.
    filetime::set_file_mtime(
        f.corpus.path().join("summary.md"),
        filetime::FileTime::from_unix_time(1_700_000_000, 0),
    )
    .ok();
    run(&f);
    assert_eq!(origin_of(&f, "summary.md"), Origin::SelfWritten);
}

#[test]
fn a_file_the_user_edits_becomes_theirs_again() {
    // The rule is keyed on content, and this is the consequence: they changed
    // it, so they wrote it. Keeping it marked SELF would quietly make the
    // user's own edits uncitable.
    let f = fixture();
    let path = f.corpus.path().join("summary.md");
    std::fs::write(&path, AGENT).unwrap();
    remember(&f, AGENT, "summary.md");
    run(&f);
    assert_eq!(origin_of(&f, "summary.md"), Origin::SelfWritten);

    let mut edited = AGENT.to_vec();
    edited.extend_from_slice(b"\nAnd I have checked this against the original.\n");
    std::fs::write(&path, &edited).unwrap();
    run(&f);
    assert_eq!(
        origin_of(&f, "summary.md"),
        Origin::User,
        "an edited file is the editor's work"
    );
}

#[test]
fn a_copy_of_agent_output_is_still_agent_output() {
    // Path is never identity (invariant #2), and authorship follows the bytes.
    // A rule keyed on path would let a rename launder the origin.
    let f = fixture();
    std::fs::write(f.corpus.path().join("summary.md"), AGENT).unwrap();
    std::fs::write(f.corpus.path().join("copy.md"), AGENT).unwrap();
    remember(&f, AGENT, "summary.md");
    run(&f);

    assert_eq!(origin_of(&f, "copy.md"), Origin::SelfWritten);
}

#[test]
fn forgetting_a_write_returns_the_file_to_the_user() {
    // The forget path has to be able to undo this, or a false positive is
    // permanent.
    let f = fixture();
    std::fs::write(f.corpus.path().join("summary.md"), AGENT).unwrap();
    remember(&f, AGENT, "summary.md");
    run(&f);
    assert_eq!(origin_of(&f, "summary.md"), Origin::SelfWritten);

    let hash = ContentHash::of(AGENT);
    let forgotten = f
        .store
        .writer()
        .submit(move |conn| marrow_store::read::forget_self_written(conn, hash))
        .unwrap();
    assert!(forgotten);

    // The file row still says SELF until it is re-examined; the record is what
    // decides, and it is gone.
    let conn = f.store.reader().unwrap();
    let n: i64 = conn
        .query_row("SELECT count(*) FROM self_written", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
}
