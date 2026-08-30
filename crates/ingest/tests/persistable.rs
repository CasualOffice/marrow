//! Can the pipeline persist everything the parser can produce?
//!
//! Three bugs in this codebase have had the same shape: a value written into a
//! CHECK-constrained column that the constraint rejects, where the *common*
//! case satisfies it and an uncommon one does not.
//!
//! - `format!("{:?}", LowYield).to_uppercase()` → `LOWYIELD`, not `LOW_YIELD`.
//!   `Ok` and `Partial` are single words, so 35,000 real files wrote cleanly.
//! - `format!("{:?}", warnings)` into a `json_valid()` column. Only files that
//!   actually warn have warnings, so 156 of 35,201 failed.
//! - Both surfaced only as a number in an error counter, never as a failure.
//!
//! Finding these one at a time is luck. This file is the systematic version:
//! drive the real pipeline over fixtures chosen to produce every outcome the
//! parser can emit, and assert **zero** write failures.
//!
//! A new parser tier, outcome or warning that cannot be persisted fails here
//! rather than in a counter six months later.

use std::sync::Arc;

use marrow_core::Timestamp;
use marrow_ingest::{ingest_root_with_index, Cancel, IngestPolicy, Progress};
use marrow_scan::AuthorizedRoot;
use marrow_store::read::{NewRoot, NewWorkspace, StorageKind};
use marrow_store::Store;

/// Fixtures chosen so the router takes a different path through each one.
///
/// The point is coverage of *outcomes*, not of formats: an empty file and a
/// binary blob exercise the branches a well-formed Markdown file never reaches.
fn fixtures() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        // Ordinary success, several parsers.
        (
            "notes.md",
            b"# Title\n\nSome prose that is long enough to chunk.\n".to_vec(),
        ),
        (
            "main.rs",
            b"fn main() {\n    println!(\"hi\");\n}\n".to_vec(),
        ),
        ("conf.toml", b"[server]\nport = 8080\n".to_vec()),
        ("data.json", br#"{"a": 1, "b": [2, 3]}"#.to_vec()),
        ("rows.csv", b"name,qty\nwidget,3\ngadget,4\n".to_vec()),
        ("plain.txt", b"just some text\n".to_vec()),
        // Empty: LowYield, then the metadata-only terminal.
        ("empty.md", Vec::new()),
        ("empty.txt", Vec::new()),
        // Malformed: the corrupt path.
        ("broken.json", b"{ not valid json at all ".to_vec()),
        ("broken.toml", b"[[[unclosed\n".to_vec()),
        // JSONC — real tsconfig files do this, and it is what first produced a
        // degrade-to-text warning on the real corpus.
        (
            "tsconfig.json",
            b"{\n  // a comment\n  \"strict\": true\n}\n".to_vec(),
        ),
        // No parser at all: the T5 terminal.
        (
            "image.jpg",
            vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, 0x4a, 0x46],
        ),
        ("blob.bin", (0u8..=255).collect()),
        // Invalid UTF-8: the decoder's replacement path.
        ("mojibake.txt", vec![0xff, 0xfe, 0x00, 0x41, 0x00, 0x42]),
        // Multi-byte at a boundary — the char-boundary panic came from here.
        (
            "wide.md",
            format!("# H\n\n{}\n", "─".repeat(3000)).into_bytes(),
        ),
        // A single unbroken line past the chunk ceiling.
        ("long.txt", format!("{}\n", "x".repeat(20_000)).into_bytes()),
        // Nested structure, for the deepest parent chains.
        (
            "deep.md",
            b"# a\n## b\n### c\n#### d\n\nbody text here.\n".to_vec(),
        ),
    ]
}

struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    ws: marrow_core::WorkspaceId,
    root_id: marrow_core::RootId,
    root: AuthorizedRoot,
}

fn setup() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let corpus = dir.path().join("corpus");
    std::fs::create_dir_all(&corpus).unwrap();
    for (name, body) in fixtures() {
        std::fs::write(corpus.join(name), body).unwrap();
    }

    let store =
        Store::open_with_migrations(dir.path().join("marrow.sqlite"), marrow_index::MIGRATIONS)
            .unwrap();
    let now = Timestamp::now();
    let ws = store
        .upsert_workspace(NewWorkspace {
            workspace_id: marrow_core::WorkspaceId::new(),
            name: "fixtures".into(),
            at: now,
        })
        .unwrap();
    let root = AuthorizedRoot::open(&corpus).unwrap();
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
    store.flush().unwrap();

    Fixture {
        _dir: dir,
        store,
        ws,
        root_id,
        root,
    }
}

fn run(f: &Fixture) -> marrow_ingest::IngestOutcome {
    let index = marrow_index::Fts5Index::open(&f.store).unwrap();
    let progress = Arc::new(Progress::new());
    ingest_root_with_index(
        &f.store,
        f.ws,
        f.root_id,
        &f.root,
        &IngestPolicy::default(),
        &progress,
        &Cancel::new(),
        Some(&index),
    )
    .unwrap()
}

#[test]
fn every_artifact_the_parser_can_produce_is_persistable() {
    // The test that would have caught all three bugs. A CHECK rejection shows
    // up as `failed`, never as an error, so this is the only thing standing
    // between a constraint mismatch and a silent gap in the index.
    let f = setup();
    let out = run(&f);

    assert_eq!(
        out.failed, 0,
        "some artifact could not be persisted — this is almost always a value \
         that violates a CHECK constraint. Outcome: {out:?}"
    );
    assert_eq!(
        out.stored, out.discovered,
        "every discovered file must be stored: {out:?}"
    );
}

#[test]
fn a_parse_result_is_recorded_for_every_file_including_unparseable_ones() {
    // PAR-003: the parser's identity and version are how an upgrade schedules
    // reprocessing. A file with no parser still needs the row, or we re-attempt
    // it on every scan forever.
    let f = setup();
    let out = run(&f);

    let conn = f.store.reader().unwrap();
    let parses: i64 = conn
        .query_row("SELECT count(*) FROM parse_results", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        parses as u64, out.stored,
        "expected one parse result per stored file"
    );
}

#[test]
fn every_persisted_enum_satisfies_its_check_constraint() {
    // Reading them back proves the constraint accepted them, but assert the
    // exact vocabulary too: a typo that happens to be in the allowed set would
    // otherwise pass silently.
    let f = setup();
    run(&f);
    let conn = f.store.reader().unwrap();

    let allowed_outcomes = [
        "OK",
        "PARTIAL",
        "LOW_YIELD",
        "FAILED",
        "UNSUPPORTED",
        "SKIPPED_POLICY",
        "METADATA_ONLY",
    ];
    let allowed_tiers = ["T1", "T2", "T3", "T4", "T5"];
    let allowed_provenance = ["EXACT", "DEGRADED", "APPROXIMATE", "METADATA_ONLY"];

    for (column, allowed) in [
        ("outcome", allowed_outcomes.as_slice()),
        ("parser_tier", allowed_tiers.as_slice()),
        ("provenance_class", allowed_provenance.as_slice()),
    ] {
        let mut stmt = conn
            .prepare(&format!("SELECT DISTINCT {column} FROM parse_results"))
            .unwrap();
        let seen: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(!seen.is_empty(), "{column}: nothing was persisted at all");
        for v in &seen {
            assert!(
                allowed.contains(&v.as_str()),
                "{column}: persisted `{v}`, which is not in the schema's allowed set {allowed:?}"
            );
        }
    }
}

#[test]
fn persisted_warnings_are_valid_json() {
    // The column is `json_valid()`-constrained. Debug output is not JSON, and
    // only files that actually warn have warnings — so the failure hides.
    let f = setup();
    run(&f);
    let conn = f.store.reader().unwrap();

    let mut stmt = conn
        .prepare("SELECT warnings FROM parse_results WHERE warnings IS NOT NULL")
        .unwrap();
    let rows: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    assert!(
        !rows.is_empty(),
        "no fixture produced a warning, so this test proves nothing — add one \
         that does (an empty file or malformed JSON will)"
    );
    for w in rows {
        let parsed: serde_json::Value = serde_json::from_str(&w)
            .unwrap_or_else(|e| panic!("persisted warnings are not valid JSON: {w:?} ({e})"));
        assert!(
            parsed.is_array(),
            "warnings should be an array, got {parsed}"
        );
    }
}

#[test]
fn the_index_and_the_canonical_chunks_agree_after_a_full_run() {
    // D3's whole premise is that these cannot drift, because they commit in one
    // transaction. If they ever differ, that premise is broken.
    let f = setup();
    run(&f);
    let conn = f.store.reader().unwrap();

    let chunks: i64 = conn
        .query_row(
            "SELECT count(*) FROM chunks WHERE status='ACTIVE'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let docs: i64 = conn
        .query_row("SELECT count(*) FROM text_index_docs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        chunks, docs,
        "canonical chunks and index documents diverged"
    );
}

#[test]
fn re_running_over_the_same_fixtures_writes_nothing() {
    // Convergence. A pipeline that rewrites unchanged files looks like it works
    // and quietly makes every re-index a full re-index.
    let f = setup();
    run(&f);
    let second = run(&f);
    assert_eq!(second.stored, 0, "second run should be a no-op: {second:?}");
    assert_eq!(second.failed, 0);
}

#[test]
fn a_file_that_cannot_be_read_is_still_recorded_from_its_metadata() {
    // FS-011 / PAR-013, and a promise the CLI prints verbatim: after a failure
    // it says "these files are still findable by name". That was false — the
    // unreadable file was skipped entirely, so the report was reassuring the
    // user about something that had not happened.
    use std::os::unix::fs::PermissionsExt;

    let f = setup();
    let locked = f.root.path().join("unreadable.md");
    std::fs::write(&locked, "secret").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

    let out = run(&f);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();

    assert_eq!(
        out.stored, out.discovered,
        "an unreadable file must still be recorded from metadata: {out:?}"
    );
    assert!(out.failed > 0, "and the failure must still be reported");

    let conn = f.store.reader().unwrap();
    let present: i64 = conn
        .query_row(
            "SELECT count(*) FROM files WHERE current_path LIKE '%unreadable.md'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(present, 1, "the file must be findable by name");
}

#[test]
fn the_failure_headline_always_equals_the_sum_of_its_groups() {
    // A summary that says 2 over a list summing to 1 teaches people to distrust
    // the whole report. Two accounting paths caused exactly that.
    use std::os::unix::fs::PermissionsExt;

    let f = setup();
    for n in ["x1.md", "x2.md"] {
        let p = f.root.path().join(n);
        std::fs::write(&p, "body").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();
    }
    let out = run(&f);
    for n in ["x1.md", "x2.md"] {
        let p = f.root.path().join(n);
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    let summed: u64 = out.failures.values().map(|g| g.count).sum();
    assert_eq!(
        out.failed, summed,
        "headline {} disagrees with its groups {summed}: {out:?}",
        out.failed
    );
    assert!(out.failed >= 2, "both unreadable files should be reported");
}

/// **The interrupted run, which used to lose files permanently.**
///
/// `record_version` commits in its own writer batch; the parse result, the
/// chunks and the index write commit in a later one. A kill between the two —
/// which CLAUDE.md says happens constantly during development — leaves a
/// version row whose `content_hash` matches the disk and which has no chunks.
///
/// The old gate compared hashes only, found them equal, and skipped the file on
/// every subsequent run. The file was permanently unsearchable, nothing
/// reported it, and each interrupted run could add more.
///
/// Simulated by deleting what the second transaction wrote, which is exactly
/// the state a kill in that window leaves behind.
#[test]
fn a_run_interrupted_between_the_version_row_and_its_chunks_recovers_on_the_next_run() {
    let f = setup();
    let first = run(&f);
    assert!(first.chunks > 0, "the corpus must produce chunks at all");

    let conn = f.store.reader().unwrap();
    let (version_id, file_id, path): (String, String, String) = conn
        .query_row(
            "SELECT v.version_id, f.file_id, f.current_path
               FROM file_versions v
               JOIN files f ON f.file_id = v.file_id
              WHERE v.status='CURRENT'
                AND EXISTS (SELECT 1 FROM chunks c WHERE c.version_id = v.version_id)
              LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("a parsed file");
    drop(conn);

    // Undo precisely what the second transaction did, leaving the first's work
    // in place. This is the crash window, reproduced.
    let vid = version_id.clone();
    f.store
        .writer()
        .submit(move |c| {
            c.execute("DELETE FROM chunks WHERE version_id = ?1", [&vid])
                .and_then(|_| c.execute("DELETE FROM parse_results WHERE version_id = ?1", [&vid]))
                .map(|_| ())
                .map_err(|e| marrow_store::map_sqlite(e, "simulating an interrupted run"))
        })
        .unwrap();
    f.store.flush().unwrap();

    let conn = f.store.reader().unwrap();
    let orphaned: i64 = conn
        .query_row(
            "SELECT count(*) FROM chunks WHERE version_id = ?1",
            [&version_id],
            |r| r.get(0),
        )
        .unwrap();
    drop(conn);
    assert_eq!(orphaned, 0, "the crash window must actually be reproduced");

    // The file has not changed on disk, so a hash comparison alone says "done".
    let second = run(&f);
    assert!(
        second.chunks > 0,
        "the second run re-parsed nothing, so {path} is now permanently \
         unsearchable — this is the bug"
    );

    // Asserted against the *file*, not the version: recovering re-records the
    // version, so the chunks come back under a new version id. What matters is
    // that the file is searchable again, which is the thing that was lost.
    let conn = f.store.reader().unwrap();
    let recovered: i64 = conn
        .query_row(
            "SELECT count(*) FROM chunks c
               JOIN file_versions v ON v.version_id = c.version_id
              WHERE v.file_id = ?1 AND v.status = 'CURRENT'",
            [&file_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        recovered > 0,
        "{path} never got its chunks back, so it stays unsearchable forever"
    );
}

/// **A sweep that does not notice deletions is not a reconciliation.**
///
/// Nothing in the full run ever marked a file gone — only `apply_hints` did,
/// and only for paths a watcher happened to send. So a file deleted while
/// Marrow was closed stayed ACTIVE forever, and 43,686 files under `target/`,
/// `.git/` and `node_modules/` indexed by an earlier build stayed ACTIVE
/// permanently: the walker prunes those directories now, so it can never
/// revisit them to notice. They inflated every count and poisoned ranking —
/// `.git/config` outranked the real documentation for "admission control".
#[test]
fn a_file_deleted_while_marrow_was_closed_is_noticed_by_the_next_sweep() {
    let f = setup();
    let first = run(&f);
    assert!(first.discovered > 1, "the fixture must have several files");
    assert_eq!(first.removed, 0, "nothing was missing on the first run");

    let conn = f.store.reader().unwrap();
    let (path, file_id): (String, String) = conn
        .query_row(
            "SELECT current_path, file_id FROM files
              WHERE status='ACTIVE' AND current_path IS NOT NULL LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("an active file");
    drop(conn);

    // Deleted with nothing watching, which is the ordinary case.
    std::fs::remove_file(&path).expect("remove");

    let second = run(&f);
    assert_eq!(second.removed, 1, "the sweep did not notice the deletion");

    let conn = f.store.reader().unwrap();
    let status: String = conn
        .query_row(
            "SELECT status FROM files WHERE file_id = ?1",
            [&file_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        status, "DELETED",
        "{path} is still ACTIVE after it was removed"
    );
}

/// **The guard that makes the rest safe.**
///
/// A cancelled walk has seen an arbitrary prefix of the corpus. Concluding that
/// everything it did not reach is deleted would empty the index — the single
/// most destructive thing this code could do, and the reason the check happens
/// before the set is even built.
#[test]
fn a_cancelled_sweep_never_concludes_that_the_files_it_missed_are_gone() {
    let f = setup();
    run(&f);

    let before: i64 = f
        .store
        .reader()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM files WHERE status='ACTIVE'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(before > 0);

    // Cancelled before it starts: it reaches nothing at all, which is the
    // worst case for a rule that keys on what was not reached.
    let cancel = Cancel::new();
    cancel.cancel();
    let index = marrow_index::Fts5Index::open(&f.store).unwrap();
    let outcome = ingest_root_with_index(
        &f.store,
        f.ws,
        f.root_id,
        &f.root,
        &IngestPolicy::default(),
        &Arc::new(Progress::new()),
        &cancel,
        Some(&index),
    )
    .unwrap();
    assert!(outcome.cancelled);
    assert_eq!(outcome.removed, 0, "a cancelled sweep deleted something");

    let after: i64 = f
        .store
        .reader()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM files WHERE status='ACTIVE'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(after, before, "a cancelled sweep emptied the index");
}
