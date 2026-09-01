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

    // **And recovery reuses the version rather than minting one.**
    //
    // This assertion used to read "asserted against the *file*, not the
    // version: recovering re-records the version, so the chunks come back
    // under a new version id" — the test documenting the bug as intended
    // behaviour. It is not: the bytes did not move, and our not having
    // finished reading them is not a fact about the file.
    //
    // Measured on a real `kill -9` nine seconds into a twenty-eight second
    // scan of 34,807 files: resuming produced 20,452 files carrying two
    // versions each with identical content hashes, and re-chunked every one.
    // Hard rule 7 asks for idempotent *and* resumable; only the second half
    // was working, which is why it stayed invisible — the index was correct,
    // just larger and slower after every interruption.
    let versions: i64 = conn
        .query_row(
            "SELECT count(*) FROM file_versions WHERE file_id = ?1",
            [&file_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        versions, 1,
        "the bytes never changed, so recovering must not invent a second version"
    );
    let still_current: String = conn
        .query_row(
            "SELECT version_id FROM file_versions WHERE file_id = ?1 AND status = 'CURRENT'",
            [&file_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        still_current, version_id,
        "the content stage must re-run against the version that already exists"
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

/// **A parser fix must reach the files that are already indexed.**
///
/// PAR-003 calls the parser's version "the mechanism by which an upgrade
/// schedules reprocessing". It was written faithfully with every parse result
/// and never read back, so improving a parser changed nothing for the existing
/// corpus: the bytes had not moved, the gate compared only content hashes, and
/// the better output applied to files indexed afterwards and to nothing else.
///
/// Simulated by ageing the recorded version, which is what an upgrade looks
/// like from the next run's point of view.
#[test]
fn a_file_is_reparsed_when_the_parser_that_read_it_has_moved_on() {
    let f = setup();
    let first = run(&f);
    assert!(first.parsed > 0, "the fixture must parse something");

    // Unchanged bytes: nothing to do, which is the behaviour that made the bug
    // invisible.
    let second = run(&f);
    assert_eq!(
        second.parsed, 0,
        "an unchanged corpus must not be re-parsed"
    );

    // Now the parser claims a different version than the one on record.
    f.store
        .writer()
        .submit(|c| {
            c.execute("UPDATE parse_results SET parser_version = 'ancient'", [])
                .map(|_| ())
                .map_err(|e| marrow_store::map_sqlite(e, "ageing the recorded parser version"))
        })
        .unwrap();
    f.store.flush().unwrap();

    let third = run(&f);
    assert!(
        third.parsed > 0,
        "the parser moved on and nothing was re-read, so the fix never reaches \
         any file already in the index"
    );

    // And it settles: having caught up, the next run has nothing to do again.
    let fourth = run(&f);
    assert_eq!(
        fourth.parsed, 0,
        "reprocessing did not converge — every run would re-parse the corpus"
    );
}

/// **A chunker change must reach the files that are already chunked.**
///
/// The sibling of the test above, and the same bug one layer down.
/// `CHUNKER_VERSION` is documented as the thing "persisted so a change can
/// schedule re-chunking" and was written faithfully with every chunk — and the
/// staleness gate only ever compared *parser* versions, so it was never read
/// back. Changing how chunks are cut left every indexed file cut the old way,
/// and a search kept returning chunks whose text the current code would no
/// longer produce from the same bytes.
#[test]
fn a_file_is_rechunked_when_the_chunker_that_cut_it_has_moved_on() {
    let f = setup();
    let first = run(&f);
    assert!(first.parsed > 0, "the fixture must parse something");

    let second = run(&f);
    assert_eq!(
        second.parsed, 0,
        "an unchanged corpus must not be re-parsed"
    );

    // What an upgrade looks like from the next run's point of view.
    f.store
        .writer()
        .submit(|c| {
            c.execute("UPDATE chunks SET chunker_version = 'ancient'", [])
                .map(|_| ())
                .map_err(|e| marrow_store::map_sqlite(e, "ageing the recorded chunker version"))
        })
        .unwrap();
    f.store.flush().unwrap();

    let third = run(&f);
    assert!(
        third.parsed > 0,
        "the chunker moved on and nothing was re-cut, so every already-indexed \
         file keeps chunks the current code would not produce"
    );

    let fourth = run(&f);
    assert_eq!(
        fourth.parsed, 0,
        "re-chunking did not converge — every run would re-chunk the corpus"
    );
}

/// **A parser that did not exist yet must still reach the files already indexed.**
///
/// The sibling of the two above, and the one they could not cover.
/// `stale_parser` asks whether the parser that produced a result has changed.
/// It cannot fire for a file the chain fell *through* to the metadata fallback:
/// the row says `metadata`, the metadata parser has not moved, so nothing is
/// stale. Every file indexed before a parser shipped therefore kept its
/// metadata-only result for ever.
///
/// On the author's own corpus that was 26 spreadsheets, 25 Word documents, 11
/// images and 18 OpenDocument files with no content and no tables — and
/// `read_table` truthfully answering "this file has no tables in it" about a
/// spreadsheet full of them, which reads as a broken parser and is a routing
/// decision nobody ever revisited.
///
/// Simulated by ageing the recorded routing fingerprint, which is what a build
/// carrying a new parser looks like from the next sweep's point of view.
#[test]
fn a_file_is_rerouted_when_the_parser_chain_has_changed() {
    let f = setup();
    let first = run(&f);
    assert!(first.parsed > 0, "the fixture must parse something");

    let second = run(&f);
    assert_eq!(
        second.parsed, 0,
        "an unchanged corpus must not be re-parsed"
    );

    f.store
        .writer()
        .submit(|c| {
            c.execute(
                "UPDATE schema_meta SET value = 'ancient' WHERE key LIKE 'parser_routing:%'",
                [],
            )
            .map(|_| ())
            .map_err(|e| marrow_store::map_sqlite(e, "ageing the routing fingerprint"))
        })
        .unwrap();
    f.store.flush().unwrap();

    let third = run(&f);
    assert!(
        third.parsed > 0,
        "the parser chain changed and nothing was re-routed, so a file that fell \
         through to metadata keeps that result for ever"
    );

    // And it settles. Re-routing on every sweep is not the fix: a `.xlsx` that
    // is really a zip is claimed by name, refused on content, recorded as
    // metadata, and would be retried endlessly.
    let fourth = run(&f);
    assert_eq!(
        fourth.parsed, 0,
        "re-routing did not converge — every run would re-parse the corpus"
    );
}

/// **The routing fingerprint is recorded on a complete walk, not a clean one.**
///
/// The mistake underneath the fix above, which its own test could not see: the
/// fingerprint was written under the *delete's* guard,
/// `!cancelled && failures.is_empty()`, and a single unparseable file keeps
/// that false for ever.
///
/// Found on the real corpus, where three spreadsheets trip a UNIQUE constraint
/// on `table_cells`. The fingerprint was never written, so every sweep
/// re-routed all 34,000 files — exactly the non-convergence the fingerprint
/// exists to prevent, reintroduced by borrowing a guard without asking what it
/// guarded against.
///
/// The two guards answer different questions. The delete asks "did this walk
/// establish what is gone", which any failure invalidates. The fingerprint asks
/// "was every file offered to the current parser chain", and a file that was
/// offered and failed was still offered — offering it again next sweep fails
/// the same way.
///
/// What is checked here is the boundary this test can own: a cancelled sweep
/// has *not* offered the corpus and must not record, and a completed one must.
/// The failure half was verified against the real corpus, where the three
/// failing files no longer stop it settling.
#[test]
fn a_cancelled_sweep_does_not_record_that_it_re_routed_the_corpus() {
    let f = setup();

    let cancel = Cancel::new();
    cancel.cancel();
    let progress = std::sync::Arc::new(Progress::default());
    let cancelled = ingest_root_with_index(
        &f.store,
        f.ws,
        f.root_id,
        &f.root,
        &IngestPolicy::default(),
        &progress,
        &cancel,
        None,
    )
    .unwrap();
    assert!(cancelled.cancelled, "the fixture must actually cancel");
    assert_eq!(
        fingerprint_of(&f),
        None,
        "a cancelled sweep claimed it had re-routed a corpus it never walked"
    );

    // And a complete one does record, so the next sweep has nothing to do.
    run(&f);
    assert!(
        fingerprint_of(&f).is_some(),
        "a completed sweep did not record what it swept with"
    );
    assert_eq!(run(&f).parsed, 0, "re-routing did not converge");
}

/// The routing fingerprint stored for the fixture's root, if any.
fn fingerprint_of(f: &Fixture) -> Option<String> {
    let conn = f.store.reader().unwrap();
    marrow_store::read::routing_fingerprint(&conn, f.root_id)
}

/// **A walk that could not read a directory has not established what is gone.**
///
/// The guard on the bulk delete checks `outcome.failures.is_empty()`, and its
/// comment says in as many words that a *failed* run must not conclude
/// anything. But a walk error was only logged and bumped on a live progress
/// counter that is never read back into the outcome, so the run reported zero
/// failures and the delete went ahead — soft-deleting every file under a
/// directory that was merely unreadable, while they sat perfectly happily on
/// the disk. `removed: 10000, failed: 0` reads exactly like a healthy
/// reconciliation of a folder the user emptied.
#[test]
#[cfg(unix)]
fn a_walk_that_could_not_open_a_directory_does_not_conclude_anything_is_gone() {
    use std::os::unix::fs::PermissionsExt;

    let f = setup();
    let first = run(&f);
    assert_eq!(first.removed, 0);

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

    // A subdirectory the walk cannot open. Its contents vanish from what the
    // walk knows, which is exactly the state in which "everything I did not see
    // is deleted" is the wrong conclusion.
    let locked = f.root.path().join("locked");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::write(locked.join("kept.md"), "still here\n").unwrap();
    run(&f); // index it while it is readable
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

    let outcome = run(&f);
    // Restore before asserting, so a failure does not leave the tempdir
    // undeletable.
    let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755));

    assert!(
        outcome.failed > 0,
        "a directory that could not be opened was reported as zero failures"
    );
    assert_eq!(
        outcome.removed, 0,
        "an incomplete walk soft-deleted files it simply could not see"
    );

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
    assert_eq!(
        after,
        before + 1,
        "files were lost to an unreadable directory"
    );
}

/// **A soft delete has to be reversible by the mechanism that made it.**
///
/// Nothing ever set `status` back to `ACTIVE`. So a file moved out of a watched
/// folder and back stayed DELETED for ever: the next walk finds it by
/// filesystem identity, restores its path, sees the hash unchanged and reports
/// it as *unchanged*, while search and every read tool filter on ACTIVE and
/// refuse it. No error, no warning, no counter that moves.
#[test]
fn a_file_that_comes_back_is_searchable_again() {
    let f = setup();
    run(&f);

    let conn = f.store.reader().unwrap();
    let (file_id, path): (String, String) = conn
        .query_row(
            "SELECT file_id, current_path FROM files
              WHERE status='ACTIVE' AND current_path IS NOT NULL LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    drop(conn);

    let away = f.root.path().join("..").join("moved-away.tmp");
    std::fs::rename(&path, &away).unwrap();
    let gone = run(&f);
    assert_eq!(gone.removed, 1, "the sweep did not notice it leave");

    std::fs::rename(&away, &path).unwrap();
    run(&f);

    let status: String = f
        .store
        .reader()
        .unwrap()
        .query_row(
            "SELECT status FROM files WHERE file_id = ?1",
            [&file_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        status, "ACTIVE",
        "{path} came back and stayed invisible for ever"
    );
}
