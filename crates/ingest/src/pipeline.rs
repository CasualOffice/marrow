//! The staged ingest pipeline.

use std::collections::{BTreeMap, HashSet};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Arc;
use std::thread;

use marrow_core::{
    ContentHash, FileId, FileStatus, Origin, Result, RootId, TierState, Timestamp, VersionId,
    WorkspaceId,
};
use marrow_scan::{walk, AuthorizedRoot, ScanEntry, ScanEvent, WalkPolicy};
use marrow_store::read::{NewFile, NewVersion};
use marrow_store::{Pending, ReadConn, Store};
use tracing::{debug, warn};

use crate::content::{documents_for, read_for_parsing, ContentInput};
use crate::progress::{Cancel, Progress, Stage};

/// How much of the corpus a run is allowed to look at.
#[derive(Clone, Debug)]
pub struct IngestPolicy {
    pub walk: WalkPolicy,
    /// Hash workers. Hashing is CPU-bound and was measured at 417 MB/s on one
    /// thread (M0), so this is the only stage worth widening.
    pub hash_workers: usize,
    /// Files above this size are recorded from metadata alone and never read
    /// (FS-015). M0 found nothing over 500 MB in the real corpus, so this is a
    /// guard against a pathological file, not a routine path.
    pub max_hash_bytes: u64,
    /// Parse and index file contents. Off leaves a metadata-only index, which
    /// is still a useful one — `search --literal` and every path/metadata
    /// query work without it.
    pub extract_content: bool,
    /// Ceiling for the parse stage. Lower than the hash ceiling because parsing
    /// holds the whole file plus its IR in memory, where hashing streams.
    pub max_parse_bytes: u64,
    pub chunking: marrow_parse::ChunkPolicy,
}

impl Default for IngestPolicy {
    fn default() -> Self {
        Self {
            walk: WalkPolicy::default(),
            hash_workers: std::thread::available_parallelism()
                .map(|n| (n.get().saturating_sub(2)).max(1))
                .unwrap_or(2),
            max_hash_bytes: 512 * 1024 * 1024,
            extract_content: true,
            max_parse_bytes: 16 * 1024 * 1024,
            chunking: marrow_parse::ChunkPolicy::default(),
        }
    }
}

/// What a run did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IngestOutcome {
    pub discovered: u64,
    pub stored: u64,
    pub unchanged: u64,
    pub skipped_placeholder: u64,
    pub failed: u64,
    pub parsed: u64,
    pub chunks: u64,
    /// Files that were in the index and that this walk did not reach — deleted,
    /// excluded by policy, or sitting under a directory the walker now prunes.
    /// Only ever set by a walk that finished; a cancelled run has seen an
    /// arbitrary prefix and must not conclude anything from what it missed.
    pub removed: u64,
    pub cancelled: bool,
    /// Why things failed, and how often — grouped by error code.
    ///
    /// A bare count is not actionable. "156 failed" tells you something is
    /// wrong; "156 × PAR_LOW_YIELD" tells you which files and what to do,
    /// and it is the difference between a number you ignore and a bug you fix.
    pub failures: BTreeMap<&'static str, FailureGroup>,
}

/// One error code's worth of failures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailureGroup {
    pub count: u64,
    /// The message from the first occurrence. They share a code, so they share
    /// a cause and an action.
    pub message: String,
    /// A few example paths, capped — enough to go and look, not a second log.
    pub examples: Vec<String>,
}

impl IngestOutcome {
    fn note_failure(&mut self, e: &marrow_core::Error, path: &str) {
        // `failed` is recomputed from the groups at the end of a run, so this
        // increment is only for callers that read it mid-run.
        self.failed += 1;
        let g = self
            .failures
            .entry(e.code().as_str())
            .or_insert_with(|| FailureGroup {
                count: 0,
                message: e.message().to_string(),
                examples: Vec::new(),
            });
        g.count += 1;
        if g.examples.len() < 3 {
            g.examples.push(path.to_string());
        }
    }

    /// Fold another run's failures in, for a caller indexing several roots.
    pub fn merge_failures_from(&mut self, other: &IngestOutcome) {
        for (code, g) in &other.failures {
            let e = self.failures.entry(code).or_insert_with(|| FailureGroup {
                count: 0,
                message: g.message.clone(),
                examples: Vec::new(),
            });
            e.count += g.count;
            for p in &g.examples {
                if e.examples.len() < 3 {
                    e.examples.push(p.clone());
                }
            }
        }
    }
}

/// One unit in flight between the hash stage and the writer.
#[derive(Debug)]
struct Hashed {
    path: String,
    /// Why the bytes could not be read, if they could not.
    ///
    /// Carried rather than counted in the worker: two accounting paths produced
    /// a summary whose headline said 2 and whose detail summed to 1, which is
    /// worse than either number alone.
    hash_error: Option<marrow_core::Error>,
    fs_identity: String,
    size: u64,
    mtime: Timestamp,
    tier: TierState,
    hash: Option<ContentHash>,
    mime: Option<String>,
}

/// Walk one root, hash what is readable, and record it.
///
/// Blocks until the walk is exhausted or cancelled. Errors on individual files
/// are counted and logged, never fatal — a workspace keeps indexing around a
/// file it cannot read (FS-011).
pub fn ingest_root(
    store: &Store,
    workspace_id: WorkspaceId,
    root_id: RootId,
    root: &AuthorizedRoot,
    policy: &IngestPolicy,
    progress: &Arc<Progress>,
    cancel: &Cancel,
) -> Result<IngestOutcome> {
    ingest_root_with_index(
        store,
        workspace_id,
        root_id,
        root,
        policy,
        progress,
        cancel,
        None,
    )
}

/// As [`ingest_root`], also writing chunks to a lexical index.
///
/// The index is optional because a metadata-only index is still useful — path
/// and metadata queries and `search --literal` all work without it — and
/// because it keeps the scan path testable without one.
#[allow(clippy::too_many_arguments)]
pub fn ingest_root_with_index(
    store: &Store,
    workspace_id: WorkspaceId,
    root_id: RootId,
    root: &AuthorizedRoot,
    policy: &IngestPolicy,
    progress: &Arc<Progress>,
    cancel: &Cancel,
    index: Option<&dyn marrow_index::TextIndex>,
) -> Result<IngestOutcome> {
    // **Marrow never walks its own data directory**, and this is applied here
    // rather than at the four places an `IngestPolicy` is built, because a
    // rule four callers have to remember is a rule three of them will
    // eventually forget — which is how the same class of bug reached MCP's
    // ranking, the stopword list and `file_detail` this week.
    //
    // Reading `marrow.sqlite` and its WAL writes to the database, which grows
    // what there is to index; the contention that produces breaks the reader
    // the ingest depends on, and indexing stops with a repeating "could not
    // read the record of what this system wrote" that never recovers.
    // `skip_hidden` covers the default `~/.local/share/marrow`; it does not
    // cover a `MARROW_DATA_DIR` pointed somewhere that is not hidden.
    // **Only when it lies inside this root.** A data directory beside or above
    // the corpus is never walked anyway, and excluding an ancestor would
    // exclude the corpus with it — the whole walk, silently, for every caller
    // whose database happens to sit next to the folder it indexes.
    let data_dir = store
        .path()
        .and_then(|p| p.parent())
        .and_then(|p| std::fs::canonicalize(p).ok())
        .filter(|d| d.starts_with(root.path()) && d != root.path());
    let policy = &match data_dir {
        Some(dir) => {
            let mut p = policy.clone();
            p.walk = p.walk.clone().without(&dir);
            p
        }
        None => policy.clone(),
    };

    // Bounded so the walk cannot outrun hashing and buffer the corpus.
    let (tx_scan, rx_scan) = sync_channel::<ScanEntry>(1024);
    let (tx_hash, rx_hash) = sync_channel::<Hashed>(256);

    let walk_errors: WalkErrors = Arc::new(std::sync::Mutex::new(Vec::new()));
    let walk_handle = spawn_walk(root, &policy.walk, tx_scan, progress, cancel, &walk_errors);
    let hash_handles = spawn_hashers(
        policy.hash_workers,
        policy.max_hash_bytes,
        rx_scan,
        tx_hash,
        progress,
        cancel,
    );

    // The writer stage runs on this thread: the store's own actor is the real
    // serialization point, so an extra thread here would only add a hop.
    //
    // ONE reader for the whole run. Opening a connection per file means 34,000
    // connection setups with their pragma work — measured at minutes, not
    // milliseconds.
    let conn = store.reader()?;

    // Loaded once for the run. The set is one row per file the write tools ever
    // produced, so it is small; a query per file would put a round trip in the
    // hot loop for a check that is almost always negative.
    let self_written = marrow_store::read::self_written_hashes(&conn)?;

    // Writes are SENT, not submitted. `Store`'s convenience helpers call
    // `submit`, which is `send().wait()` — so every file would block until its
    // batch committed, up to `max_batch_interval` (100 ms). Across a real
    // corpus that is the difference between three seconds and an hour.
    //
    // Handles are drained periodically so a write failure still surfaces
    // instead of being silently dropped.
    let mut outcome = IngestOutcome::default();
    // Every file this walk laid eyes on. Reconciliation is defined by what it
    // did *not* see, so this must include unchanged files as well as written
    // ones — on any second run the unchanged are the overwhelming majority.
    let mut seen: std::collections::HashSet<FileId> = std::collections::HashSet::new();
    let mut inflight: Vec<Pending<()>> = Vec::with_capacity(DRAIN_EVERY);
    let router = marrow_parse::ParserRouter::with_default_parsers();

    // Whether the parser chain has changed since this root was last *fully*
    // swept. Asked once, because it is a question about this build rather than
    // about any file. See `ParserRouter::fingerprint` for why re-routing on
    // every sweep would never converge.
    let fingerprint = router.fingerprint();
    let reroute = marrow_store::read::routing_fingerprint(&conn, root_id)
        .is_none_or(|seen| seen != fingerprint);
    if reroute {
        tracing::info!(
            root = %root_id,
            fingerprint = %fingerprint,
            "the parser chain has changed since this root was last swept; re-routing once"
        );
    }

    for h in rx_hash {
        if cancel.is_cancelled() {
            outcome.cancelled = true;
            break;
        }
        // A file we could stat but not read is still recorded from its
        // metadata (FS-011, PAR-013), so it stays findable by name — which is
        // what the failure report promises. Skipping it here made that promise
        // false for exactly the files it was written about.
        if let Some(e) = &h.hash_error {
            outcome.note_failure(e, &h.path);
        }
        match record(
            store,
            &conn,
            workspace_id,
            root_id,
            &h,
            &self_written,
            &router,
            reroute,
            &mut inflight,
        ) {
            Ok((file_id, Some(ids))) => {
                seen.insert(file_id);
                progress.bump(Stage::Stored);
                outcome.stored += 1;
                if policy.extract_content && h.hash_error.is_none() {
                    match extract(
                        store,
                        index,
                        &router,
                        policy,
                        workspace_id,
                        &h,
                        &ids,
                        &self_written,
                        &mut inflight,
                    ) {
                        Ok(n) if n > 0 => {
                            progress.bump(Stage::Parsed);
                            outcome.parsed += 1;
                            outcome.chunks += n as u64;
                        }
                        Ok(_) => {}
                        Err(e) => {
                            // A parse failure isolates to one file (FS-011);
                            // the file stays discoverable by metadata.
                            debug!(path = %h.path, error = %e, "content extraction failed");
                            progress.bump(Stage::Failed);
                            outcome.note_failure(&e, &h.path);
                        }
                    }
                }
            }
            Ok((file_id, None)) => {
                seen.insert(file_id);
                progress.bump(Stage::Unchanged);
                outcome.unchanged += 1;
            }
            Err(e) => {
                // A storage failure is not per-file recoverable the way a parse
                // failure is, but one row failing should not abandon the run.
                warn!(path = %h.path, error = %e, "failed to record file");
                progress.bump(Stage::Failed);
                outcome.note_failure(&e, &h.path);
            }
        }
        if inflight.len() >= DRAIN_EVERY {
            drain(&mut inflight, progress, &mut outcome);
        }
    }
    drain(&mut inflight, progress, &mut outcome);

    // Drain the workers before reading final counters, or the numbers race.
    for h in hash_handles {
        let _ = h.join();
    }
    let _ = walk_handle.join();

    // Folded in after the join, so they are counted before the guard below
    // reads `failures`. A walk error is the strongest possible reason not to
    // conclude that unseen files are deleted: an unopenable directory removes
    // its whole subtree from `seen`, and every file under it would otherwise be
    // marked gone while sitting perfectly happily on the disk.
    //
    // Kept as its own fact as well. Once folded, a directory that could not be
    // opened is indistinguishable from a spreadsheet that would not parse, and
    // those answer different questions: the first means the corpus was never
    // enumerated, the second means one file in it is broken.
    let walk_failed = walk_errors.lock().map(|e| !e.is_empty()).unwrap_or(true);
    if let Ok(errs) = walk_errors.lock() {
        for (e, path) in errs.iter() {
            outcome.note_failure(e, path);
        }
    }

    store.flush()?;

    if let Some(ix) = index {
        // Flush the writer first: index docs have a foreign key to `chunks`,
        // so the canonical rows must be committed before their documents are.
        store.flush()?;
        let _ = ix;
    }

    outcome.discovered = progress.get(Stage::Discovered);
    outcome.skipped_placeholder = progress.get(Stage::SkippedPlaceholder);
    // The headline is defined as the sum of the groups, so the number and the
    // detail below it cannot disagree. A summary that says 2 over a list that
    // sums to 1 teaches people to distrust the whole report.
    outcome.failed = outcome.failures.values().map(|g| g.count).sum();
    outcome.cancelled |= cancel.is_cancelled();

    // **A sweep that does not notice deletions is not a reconciliation.**
    //
    // Nothing in the full run ever marked a file gone: only `apply_hints` did,
    // and only for paths a watcher happened to send it. So a file deleted while
    // Marrow was closed stayed ACTIVE forever, and — worse — 43,686 files under
    // `target/`, `.git/` and `node_modules/` indexed by an earlier build stayed
    // ACTIVE permanently, because the walker now prunes those directories and
    // therefore can never revisit them to notice. They inflated every count and
    // poisoned ranking: `.git/config` outranked the actual documentation for
    // "admission control", on the strength of matching branch names.
    //
    // The walk defines the scope. A file under this root that the walk did not
    // reach is not in the index any more, whether it was deleted, excluded by
    // policy, or sits in a directory that is now pruned. Soft delete, so the
    // forget path stays the only thing that removes rows.
    //
    // **Only ever after a complete walk.** A cancelled or failed run has seen
    // an arbitrary prefix of the corpus, and marking everything it missed as
    // deleted would empty the index. This is the one guard that makes the rest
    // safe, which is why it is checked before the set is even built.
    if !outcome.cancelled && outcome.failures.is_empty() {
        outcome.removed = mark_unseen_deleted(store, root_id, &seen)?;

        store.flush()?;
    } else if outcome.cancelled {
        debug!("the sweep stopped early, so absent files were not reconciled");
    }

    // **The routing fingerprint has a weaker condition than the delete, and it
    // has to.** The delete asks "did this walk establish what is gone", which
    // any failure invalidates. This asks "was every file under this root
    // offered to the current parser chain", and a file that was offered and
    // failed to parse was still offered — re-routing it next sweep would fail
    // in exactly the same way.
    //
    // Recorded under the *walk* succeeding, not under a clean run. Three
    // spreadsheets that trip a UNIQUE constraint kept `failures` non-empty for
    // ever, so the fingerprint was never written and every sweep re-routed all
    // 34,000 files — the non-convergence this whole mechanism exists to avoid,
    // reintroduced by borrowing the delete's guard without asking what it
    // guarded against. The first test missed it because its fixture had no
    // file that fails.
    if !outcome.cancelled && !walk_failed {
        let fp = fingerprint.clone();
        store
            .writer()
            .submit(move |c| marrow_store::read::set_routing_fingerprint(c, root_id, &fp))?;
        store.flush()?;
    }
    Ok(outcome)
}

/// Mark every ACTIVE file under `root_id` that this walk did not reach.
///
/// The ids go into a temporary table rather than an `IN (...)` list: on this
/// corpus the set is 79,000 ULIDs, which is past SQLite's parameter limit and
/// would be a megabyte of SQL text besides.
fn mark_unseen_deleted(
    store: &Store,
    root_id: RootId,
    seen: &std::collections::HashSet<FileId>,
) -> Result<u64> {
    let ids: Vec<String> = seen.iter().map(|f| f.to_string()).collect();
    let now = Timestamp::now().as_millis();
    store.writer().submit(move |c| {
        c.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS seen_files (file_id TEXT PRIMARY KEY);
             DELETE FROM seen_files;",
        )
        .map_err(|e| marrow_store::map_sqlite(e, "preparing the reconciliation set"))?;
        {
            let mut stmt = c
                .prepare("INSERT OR IGNORE INTO seen_files(file_id) VALUES (?1)")
                .map_err(|e| marrow_store::map_sqlite(e, "recording a seen file"))?;
            for id in &ids {
                stmt.execute([id])
                    .map_err(|e| marrow_store::map_sqlite(e, "recording a seen file"))?;
            }
        }
        let n = c
            .execute(
                "UPDATE files SET status='DELETED', current_path=NULL, updated_at=?2
                  WHERE root_id=?1 AND status='ACTIVE'
                    AND file_id NOT IN (SELECT file_id FROM seen_files)",
                marrow_store::rusqlite::params![root_id.to_string(), now],
            )
            .map_err(|e| marrow_store::map_sqlite(e, "reconciling absent files"))?;
        Ok(n as u64)
    })
}

/// How many un-awaited writes to allow before checking their results.
///
/// Large enough that the 100 ms batch interval is amortised across many files,
/// small enough that a systematic write failure is noticed early rather than at
/// the end of a long run.
const DRAIN_EVERY: usize = 1000;

fn drain(inflight: &mut Vec<Pending<()>>, progress: &Progress, outcome: &mut IngestOutcome) {
    for p in inflight.drain(..) {
        if let Err(e) = p.wait() {
            warn!(error = %e, "write failed");
            progress.bump(Stage::Failed);
            // No path here — the write is detached from the file by this point.
            // The code and message are what make it actionable anyway.
            outcome.note_failure(&e, "");
            outcome.stored = outcome.stored.saturating_sub(1);
        }
    }
}

/// Re-examine a set of hinted paths.
///
/// **Watchers are hints; reconciliation is truth.** A hint says a path is worth looking at, not what happened
/// to it. Every path is re-stated and re-fingerprinted here; the watcher's
/// opinion of create-vs-modify-vs-delete is never believed.
///
/// A path that has vanished is marked deleted. A path outside the root is
/// dropped — a watcher can report one, and acting on it would index a file the
/// workspace grant never authorised. A path the walk policy excludes is dropped
/// too, for the reason spelled out at the check below: a hint is a prompt to
/// look, never a reason to index something a sweep would prune.
#[allow(clippy::too_many_arguments)]
pub fn apply_hints(
    store: &Store,
    workspace_id: WorkspaceId,
    root_id: RootId,
    root: &AuthorizedRoot,
    policy: &IngestPolicy,
    paths: &std::collections::BTreeSet<std::path::PathBuf>,
    progress: &Arc<Progress>,
    cancel: &Cancel,
    index: Option<&dyn marrow_index::TextIndex>,
) -> Result<IngestOutcome> {
    let mut outcome = IngestOutcome::default();
    let conn = store.reader()?;

    // Same reason as the full run: one row per file the write tools produced,
    // and a query per file would be a round trip for a check that is almost
    // always negative.
    let self_written = marrow_store::read::self_written_hashes(&conn)?;
    let mut inflight: Vec<Pending<()>> = Vec::new();
    let router = marrow_parse::ParserRouter::with_default_parsers();

    // Whether the parser chain has changed since this root was last *fully*
    // swept. Asked once, because it is a question about this build rather than
    // about any file. See `ParserRouter::fingerprint` for why re-routing on
    // every sweep would never converge.
    let fingerprint = router.fingerprint();
    let reroute = marrow_store::read::routing_fingerprint(&conn, root_id)
        .is_none_or(|seen| seen != fingerprint);
    if reroute {
        tracing::info!(
            root = %root_id,
            fingerprint = %fingerprint,
            "the parser chain has changed since this root was last swept; re-routing once"
        );
    }

    for path in paths {
        if cancel.is_cancelled() {
            outcome.cancelled = true;
            break;
        }
        // Containment is re-proved per path: a hint is untrusted input.
        if !root.contains(path) {
            debug!(path = %path.display(), "hint outside the root; ignored");
            continue;
        }

        let facts = match marrow_scan::probe(path) {
            Ok(f) => f,
            Err(_) => {
                // Gone since the hint fired. Mark it deleted rather than
                // leaving a row pointing at nothing.
                let p = path.to_string_lossy().into_owned();
                if let Ok(Some(file)) = marrow_store::read::find_file_by_path(&conn, root_id, &p) {
                    let id = file.file_id;
                    inflight.push(store.writer().send(move |c| {
                        c.execute(
                            "UPDATE files SET status='DELETED', current_path=NULL, \
                             updated_at=?2 WHERE file_id=?1",
                            marrow_store::rusqlite::params![
                                id.to_string(),
                                Timestamp::now().as_millis()
                            ],
                        )
                        .map(|_| ())
                        .map_err(|e| marrow_store::map_sqlite(e, "marking a file deleted"))
                    })?);
                    outcome.stored += 1;
                }
                continue;
            }
        };
        if facts.is_dir {
            continue;
        }
        /*
         * The walk policy applies to a hint too.
         *
         * A hint used to be an exemption from it: the sweep prunes
         * `node_modules`, `.git` and `target` by descending past them, and this
         * loop never descends anything, so a file created in one of them was
         * indexed the moment a watcher noticed it and pruned again by the next
         * full sweep. Churn, and a ranking briefly poisoned by build output —
         * `.git/config` outranking real documentation for "admission control".
         *
         * The verdict comes from `WalkPolicy` rather than from a second copy of
         * the rules here, because a second copy is what produced the gap.
         *
         * Placed *after* the vanished-path branch on purpose. A row that an
         * earlier build wrote under an excluded directory still has to be
         * retirable when its file disappears; refusing to look at the path at
         * all would strand it ACTIVE forever.
         */
        if policy.walk.excludes(root, path, facts.is_dir) {
            debug!(path = %path.display(), "hint excluded by the walk policy; ignored");
            continue;
        }
        outcome.discovered += 1;
        progress.bump(Stage::Discovered);

        let entry = marrow_scan::ScanEntry {
            path: path.clone(),
            depth: 0,
            facts,
        };
        let Some(h) = hash_one(&entry, policy.max_hash_bytes, progress) else {
            continue;
        };

        // A file we could stat but not read is still recorded from its
        // metadata (FS-011, PAR-013), so it stays findable by name — which is
        // what the failure report promises. Skipping it here made that promise
        // false for exactly the files it was written about.
        if let Some(e) = &h.hash_error {
            outcome.note_failure(e, &h.path);
        }
        match record(
            store,
            &conn,
            workspace_id,
            root_id,
            &h,
            &self_written,
            &router,
            reroute,
            &mut inflight,
        ) {
            Ok((_, Some(ids))) => {
                outcome.stored += 1;
                progress.bump(Stage::Stored);
                // `hash_error.is_none()` mirrors the sweep. A file we could stat
                // and not read is recorded from its metadata and must not then
                // be handed to a parser that will fail on the same `open` — the
                // failure is already counted above, and counting it twice makes
                // the hint path report more failures than there are files.
                if policy.extract_content && h.hash_error.is_none() {
                    match extract(
                        store,
                        index,
                        &router,
                        policy,
                        workspace_id,
                        &h,
                        &ids,
                        &self_written,
                        &mut inflight,
                    ) {
                        Ok(n) => {
                            outcome.chunks += n as u64;
                            if n > 0 {
                                outcome.parsed += 1;
                            }
                        }
                        // Failing open is right — the file is recorded and stays
                        // findable by name (FS-011) — but this was `if let Ok`,
                        // which failed open *silently*. A watcher-driven
                        // re-parse that broke on every save produced an outcome
                        // reporting zero failures, so the desktop's own edit
                        // loop was the one path where a parser regression left
                        // no trace anywhere. The sweep in `ingest_root` has
                        // always counted this; the hint path is the same event.
                        Err(e) => {
                            debug!(path = %h.path, error = %e, "content extraction failed");
                            progress.bump(Stage::Failed);
                            outcome.note_failure(&e, &h.path);
                        }
                    }
                }
            }
            Ok((_, None)) => outcome.unchanged += 1,
            Err(e) => {
                warn!(path = %h.path, error = %e, "failed to record file");
                outcome.note_failure(&e, &h.path);
            }
        }
    }

    drain(&mut inflight, progress, &mut outcome);
    store.flush()?;
    Ok(outcome)
}

/// Errors the walk itself raised — an unopenable directory, a metadata error, a
/// refused symlink escape.
///
/// **These are not the same as a file that failed to hash**, and conflating them
/// is what made the bulk delete unsafe. A file that could not be read is still a
/// file the walk *saw*; a directory that could not be opened removes everything
/// beneath it from the walk's knowledge, which is precisely the state in which
/// concluding "these files are gone" is wrong.
type WalkErrors = Arc<std::sync::Mutex<Vec<(marrow_core::Error, String)>>>;

fn spawn_walk(
    root: &AuthorizedRoot,
    policy: &WalkPolicy,
    tx: SyncSender<ScanEntry>,
    progress: &Arc<Progress>,
    cancel: &Cancel,
    errors: &WalkErrors,
) -> thread::JoinHandle<()> {
    let errors = Arc::clone(errors);
    let root = root.clone();
    let policy = policy.clone();
    let progress = Arc::clone(progress);
    let cancel = cancel.clone();
    thread::spawn(move || {
        for ev in walk(&root, &policy) {
            if cancel.is_cancelled() {
                break;
            }
            match ev {
                ScanEvent::Entry(e) => {
                    if e.facts.is_dir {
                        continue;
                    }
                    progress.bump(Stage::Discovered);
                    // A closed receiver means the consumer stopped; so do we.
                    if tx.send(e).is_err() {
                        break;
                    }
                }
                ScanEvent::Failed(err) => {
                    // Recorded, not just logged. `progress` is a live counter
                    // for the UI and is never read back into the outcome, so a
                    // bump alone left the run reporting zero failures — which
                    // is exactly the condition the reconciliation guard checks
                    // before deciding that everything it did not see is gone.
                    debug!(error = %err, "walk entry failed");
                    progress.bump(Stage::Failed);
                    if let Ok(mut v) = errors.lock() {
                        v.push((err, String::new()));
                    }
                }
            }
        }
    })
}

fn spawn_hashers(
    workers: usize,
    max_bytes: u64,
    rx: Receiver<ScanEntry>,
    tx: SyncSender<Hashed>,
    progress: &Arc<Progress>,
    cancel: &Cancel,
) -> Vec<thread::JoinHandle<()>> {
    // One receiver shared by N workers: the cheapest correct work queue, and
    // the channel is already the backpressure mechanism.
    let rx = Arc::new(std::sync::Mutex::new(rx));
    (0..workers.max(1))
        .map(|_| {
            let rx = Arc::clone(&rx);
            let tx = tx.clone();
            let progress = Arc::clone(progress);
            let cancel = cancel.clone();
            thread::spawn(move || loop {
                if cancel.is_cancelled() {
                    return;
                }
                let entry = {
                    let guard = match rx.lock() {
                        Ok(g) => g,
                        Err(_) => return,
                    };
                    match guard.recv() {
                        Ok(e) => e,
                        Err(_) => return,
                    }
                };
                if let Some(h) = hash_one(&entry, max_bytes, &progress) {
                    if tx.send(h).is_err() {
                        return;
                    }
                }
            })
        })
        .collect()
}

fn hash_one(entry: &ScanEntry, max_bytes: u64, progress: &Progress) -> Option<Hashed> {
    let f = &entry.facts;
    let path = entry.path.to_string_lossy().into_owned();

    // **Never hydrate a placeholder.** One is recorded from metadata and never
    // opened; opening it is what triggers the download.
    let hash = if !f.tier.safe_to_read() {
        progress.bump(Stage::SkippedPlaceholder);
        None
    } else if f.size > max_bytes {
        debug!(%path, size = f.size, "over hash budget; metadata only");
        None
    } else {
        match marrow_scan::hash_file_with_tier(&entry.path, f.tier) {
            Ok(h) => {
                progress.bump(Stage::Hashed);
                Some(h)
            }
            Err(e) => {
                debug!(%path, error = %e, "hash failed");
                return Some(Hashed {
                    path,
                    hash_error: Some(e),
                    fs_identity: format!("{}:{}", f.identity.dev, f.identity.ino),
                    size: f.size,
                    mtime: f.mtime,
                    tier: f.tier,
                    hash: None,
                    mime: f.mime_hint.map(|m| m.as_str().to_string()),
                });
            }
        }
    };

    Some(Hashed {
        path,
        hash_error: None,
        fs_identity: format!("{}:{}", f.identity.dev, f.identity.ino),
        size: f.size,
        mtime: f.mtime,
        tier: f.tier,
        hash,
        mime: f.mime_hint.map(|m| m.as_str().to_string()),
    })
}

/// Whether this system wrote these bytes — the `origin = SELF` rule.
///
/// Keyed on content, not path: a copy of agent output is still agent output,
/// and a file the user edits stops matching and becomes theirs again — which
/// is the right reading, because they changed it.
fn origin_of(h: &Hashed, self_written: &HashSet<ContentHash>) -> Origin {
    match h.hash {
        Some(hash) if self_written.contains(&hash) => Origin::SelfWritten,
        // A file with no hash was not read, so nothing is known about its
        // authorship. `User` is the default the schema already applies, and it
        // is the only honest answer.
        _ => Origin::User,
    }
}

/// Record one observation. Returns `true` if anything was written.
///
/// **Path is never identity**: a file is found by filesystem
/// identity first, so a rename keeps its `FileId` and its derived data. Only
/// when identity is unavailable or unknown do we fall back to path.
#[allow(clippy::too_many_arguments)] // Each is a distinct input; a struct would
                                     // move the list rather than shorten it.
fn record(
    store: &Store,
    conn: &ReadConn,
    workspace_id: WorkspaceId,
    root_id: RootId,
    h: &Hashed,
    self_written: &HashSet<ContentHash>,
    router: &marrow_parse::ParserRouter,
    // The parser chain has changed since this root was last fully swept, so
    // every file is re-routed once. See `ParserRouter::fingerprint`.
    reroute: bool,
    inflight: &mut Vec<Pending<()>>,
) -> Result<(FileId, Option<RecordedIds>)> {
    let now = Timestamp::now();

    // Path first, identity second — and identity only counts as a RENAME when
    // the previously recorded path is gone.
    //
    // Trusting identity first merges hardlinks: two paths to one inode collapse
    // to one row, then fight over `current_path` on every scan, so the index
    // reports them changed forever and never converges. Found on the real
    // corpus — macOS Photos libraries hardlink their Spotlight journals.
    //
    // A hardlink is a distinct path to the same bytes, not the same file. The
    // content hash still dedupes the *content* (FS-008); this is about identity.
    let existing = match marrow_store::read::find_file_by_path(conn, root_id, &h.path)? {
        Some(f) => Some(f),
        None => {
            match marrow_store::read::find_file_by_fs_identity(conn, root_id, &h.fs_identity)? {
                // `exists()` is a stat, not a read — safe on a placeholder.
                Some(f)
                    if f.current_path
                        .as_deref()
                        .is_none_or(|p| !std::path::Path::new(p).exists()) =>
                {
                    Some(f)
                }
                _ => None,
            }
        }
    };

    // **A file the walk found again is not deleted.** Reconciliation marks a
    // file DELETED when a walk does not reach it, which is correct when the file
    // is gone and wrong when the *walk* was — a directory it could not open, a
    // volume that was not mounted, a file moved out and back. Without this the
    // row stayed DELETED for ever while every counter reported it healthy: the
    // hash matches, so the run says "unchanged", and search filters it out.
    if let Some(f) = existing.as_ref() {
        if f.status == marrow_core::FileStatus::Deleted {
            let id = f.file_id;
            inflight.push(
                store
                    .writer()
                    .send(move |c| marrow_store::read::restore_file(c, id, now).map(|_| ()))?,
            );
            debug!(path = %h.path, "a file that was marked deleted is back");
        }
    }

    let Some(file) = existing else {
        // New file.
        let file_id = FileId::new();
        let f = NewFile {
            file_id,
            workspace_id,
            root_id,
            current_path: Some(h.path.clone()),
            fs_identity: Some(h.fs_identity.clone()),
            tier_state: h.tier,
            // **The `origin = SELF` rule.** `files.origin` defaults to `'USER'` and a scan
            // cannot tell agent output from something the user typed, so
            // without this lookup everything the write tools produced comes
            // back as the user's own work and becomes citable — and the system
            // quotes itself as independent corroboration.
            origin: origin_of(h, self_written),
            origin_txn_id: None,
            external_source_url: None,
            status: FileStatus::Active,
            at: now,
        };
        let v = new_version(file_id, h, now);
        let ids = RecordedIds {
            file_id,
            version_id: v.version_id,
        };
        inflight.push(
            store.writer().send(move |c| {
                marrow_store::read::insert_file_with_version(c, &f, &v).map(|_| ())
            })?,
        );
        return Ok((ids.file_id, Some(ids)));
    };

    // Known file. Two things can have changed: where it is, and what it says.
    let mut wrote = false;

    if file.current_path.as_deref() != Some(h.path.as_str()) {
        let (id, path) = (file.file_id, h.path.clone());
        inflight.push(store.writer().send(move |c| {
            marrow_store::read::record_path_change(c, id, &path, now).map(|_| ())
        })?);
        wrote = true;
    }

    let current = marrow_store::read::current_version(conn, file.file_id)?;
    // **The content hash decides. An mtime that advanced on its own does not,
    // and that is a considered position rather than an oversight.**
    //
    // Onyx gates the same decision on *either* an advancing `doc_updated_at`
    // *or* a changed content hash, and its comment names the case it was
    // written for: "a timestamp advance is authoritative … (e.g. GDrive
    // in-place image replacement)" — a document whose hash-relevant content is
    // byte-identical while the document genuinely changed. That is a real
    // counterexample to *their* hash. It is not one to this hash.
    //
    // Onyx hashes the text it extracted. Replace an image inside a Google Doc
    // and the extracted text is unchanged, so their hash cannot see an edit
    // that happened, and the timestamp is the only signal left. Marrow's
    // `content_hash` is blake3 over the whole file — every byte, streamed by
    // `marrow_scan::hash_file_with_tier` *before* any parser runs. Every
    // parser downstream reads a subset of what the hash already read, so there
    // is no edit this gate can miss that an mtime would have caught. The
    // dominance runs the other way too: a sync client that rewrites a file and
    // restores its mtime is invisible to a timestamp gate and caught here, and
    // on a cloud-synced root that is the case that actually costs a stale
    // index. `a_rewrite_that_restores_the_mtime_is_still_a_change` pins it.
    //
    // Adding "or the mtime advanced" would therefore add no detection and one
    // failure mode. `touch`, a sync client's metadata pass, a restore from
    // backup and a `cp -p` all move mtime without moving a byte, and each
    // would mint a version and re-parse — on an iCloud or Dropbox root, the
    // whole root at once, and again on the next pass, because the pass that
    // re-examines them is itself the thing that keeps moving the timestamps.
    // A corpus that never converges is the failure that idempotent, resumable jobs exist to
    // prevent, which is why idempotency is keyed to content and not to a clock.
    //
    // What that gives up is real and small: `file_versions.mtime_ms` is the
    // mtime of the bytes the row holds, not of the last `utimes` call, so a
    // search result's `modified` and the `--modified-after` filter answer
    // "when did this content last change" rather than "when was this inode
    // last touched". On a sync-managed root that is the more truthful of the
    // two answers, and it is the one a citation needs.
    let changed = match (&current, &h.hash) {
        // No hash on either side (placeholder or over budget): fall back to the
        // cheap signals. This is the only place we trust size+mtime, and only
        // because we are forbidden from reading the bytes.
        (Some(c), None) => c.size_bytes != h.size as i64 || c.mtime_ms != h.mtime,
        (Some(c), Some(new)) => c.content_hash != *new,
        (None, _) => true,
    };

    // **An unchanged file whose content stage never finished is not done.**
    // `record_version` commits in its own writer batch and the chunks commit in
    // a later one, so a kill between them leaves a version row that matches the
    // disk and has nothing behind it. Comparing hashes alone then skips that
    // file on every future run — permanently unsearchable, silently, and the
    // damage accumulates because each interrupted run can add more.
    //
    // Hard rule 7 asks for idempotent and resumable, and resumable means the
    // next run has to be able to *tell* that the last one stopped half way.
    // The parse result is what says so: it shares a transaction with the chunks
    // and the index write, so it exists only if all of them do.
    let unfinished = match &current {
        Some(c) if !changed => !marrow_store::read::content_stage_finished(conn, c.version_id)?,
        _ => false,
    };

    // **A parser fix has to reach the files already indexed.** PAR-003 calls the
    // parser's version "the mechanism by which an upgrade schedules
    // reprocessing", and it was written with every parse result and never read
    // back — so improving a parser changed nothing for the existing corpus,
    // because the bytes had not moved and the gate compared only content
    // hashes. The improvement applied to files indexed after it and to nothing
    // else, silently.
    // **And a parser that did not exist has to reach them too.** `stale_parser`
    // asks whether the parser that produced a result has changed. It cannot
    // fire for a file the chain fell *through* to the metadata fallback: the
    // row says `metadata`, the metadata parser has not moved, so nothing is
    // stale. Every file indexed before a parser shipped therefore kept its
    // metadata-only result for ever — on a real corpus, 26 spreadsheets, 25
    // Word documents, 11 images and 18 OpenDocument files with no content and
    // no tables, and `read_table` truthfully answering "this file has no tables
    // in it" about a spreadsheet full of them.
    //
    // `reroute` is decided once per sweep from the chain's fingerprint rather
    // than per file, because the question is about the build and not about the
    // file, and asking it per file would mean re-routing on every sweep for
    // ever: a `.xlsx` that is really a zip is claimed by name, refused on
    // content, recorded as metadata, and would be retried endlessly.
    let stale = match &current {
        Some(c) if !changed && !unfinished => {
            reroute
                || stale_parser(conn, router, c.version_id)?
                || stale_chunker(conn, c.version_id)?
        }
        _ => false,
    };

    // **Two different questions, and collapsing them was a bug.** `changed`
    // means the bytes moved and the file needs a *new version*. `unfinished`
    // and `stale` mean the same bytes need their content stage run *again* —
    // a kill left it half done, or the parser has improved since.
    //
    // Both used to set `changed`, so either minted a new version row. Measured
    // on a real `kill -9` at nine seconds into a twenty-eight second scan:
    // resuming produced 20,452 files with two versions each, identical content
    // hashes, identical sizes, differing only in timestamp — and re-chunked
    // every one. That is the "no duplicate work" half of hard rule 7 failing
    // while the "resumable" half worked, which is why it stayed invisible: the
    // index was correct, only larger and slower each time it was interrupted.
    //
    // A version is a version of the *file*. Our not having finished reading it
    // is not a fact about the file.
    let needs_content = changed || unfinished || stale;

    let mut ids = None;
    if changed {
        // Authorship follows the bytes — the `origin = SELF` rule. Decided once at
        // discovery and never revisited, a file the user edited would stay
        // marked as the system's own and be silently excluded from their own
        // answers — and one the system rewrote would stay citable.
        let origin = origin_of(h, self_written);
        if origin != file.origin {
            let id = file.file_id;
            inflight.push(
                store
                    .writer()
                    .send(move |c| marrow_store::read::set_file_origin(c, id, origin, now))?,
            );
        }

        let v = new_version(file.file_id, h, now);
        ids = Some(RecordedIds {
            file_id: file.file_id,
            version_id: v.version_id,
        });
        inflight.push(
            store
                .writer()
                .send(move |c| marrow_store::read::record_version(c, &v).map(|_| ()))?,
        );
        wrote = true;
    } else if needs_content {
        // Same bytes, work to redo. `replace_chunks` deletes before it inserts
        // and `record_parse` is keyed on the version, so pointing the content
        // stage at the row that already exists is idempotent — which is what
        // makes re-running it safe rather than additive.
        if let Some(c) = &current {
            ids = Some(RecordedIds {
                file_id: file.file_id,
                version_id: c.version_id,
            });
        }
    }

    // A path change on its own is a write, but not a reason to re-parse: the
    // bytes did not move. Only a new version yields ids for the content stage.
    //
    // The file id comes back either way, because "this walk saw this file" is a
    // different fact from "this walk changed it", and reconciliation needs the
    // first one to know what it did *not* see.
    let _ = wrote;
    Ok((file.file_id, ids))
}

/// Whether the parser that produced this version's result has moved on.
///
/// Asked of the recorded `parser_id`, so a build that no longer carries that
/// parser leaves the file alone rather than reprocessing it with something
/// else — losing a parser is a different event from improving one, and only the
/// second is a reason to re-read a file that has not changed.
fn stale_parser(
    conn: &ReadConn,
    router: &marrow_parse::ParserRouter,
    version_id: VersionId,
) -> Result<bool> {
    let mut stmt = conn
        .prepare_cached("SELECT parser_id, parser_version FROM parse_results WHERE version_id = ?1")
        .map_err(|e| marrow_store::map_sqlite(e, "reading the parser that produced a version"))?;
    let rows = stmt
        .query_map([version_id.to_string()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(|e| marrow_store::map_sqlite(e, "reading the parser that produced a version"))?;

    for row in rows {
        let (id, was) =
            row.map_err(|e| marrow_store::map_sqlite(e, "reading a recorded parser version"))?;
        if let Some(now) = router.version_of(&id) {
            if now != was {
                debug!(parser = %id, was = %was, now = %now, "the parser moved on; reprocessing");
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Whether a different chunker wrote this version's chunks.
///
/// `CHUNKER_VERSION` was persisted from the start and documented as the thing
/// that "can schedule re-chunking", and then nothing ever read it back: the
/// staleness check looked only at parser versions, so changing how chunks are
/// cut left every already-indexed file cut the old way. Silent, and exactly the
/// stale-index failure the reconciler exists to prevent -- a search returns a
/// chunk whose text no longer matches what the current code would produce from
/// the same bytes.
///
/// A version this run has no chunks for is not stale. That is a file whose
/// content stage never finished, which `unfinished` already handles, and
/// reporting it here would re-run it twice.
fn stale_chunker(conn: &ReadConn, version_id: VersionId) -> Result<bool> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT 1 FROM chunks WHERE version_id = ?1 AND chunker_version <> ?2 LIMIT 1",
        )
        .map_err(|e| marrow_store::map_sqlite(e, "reading the chunker that cut a version"))?;
    let stale = stmt
        .exists([
            version_id.to_string().as_str(),
            marrow_parse::CHUNKER_VERSION,
        ])
        .map_err(|e| marrow_store::map_sqlite(e, "reading the chunker that cut a version"))?;
    if stale {
        debug!(version = %version_id, now = %marrow_parse::CHUNKER_VERSION, "the chunker moved on; re-chunking");
    }
    Ok(stale)
}

/// What `record` produced, so the content stage knows what to attach chunks to.
#[derive(Clone, Copy, Debug)]
struct RecordedIds {
    file_id: FileId,
    version_id: VersionId,
}

/// Parse a file and write its chunks to the store and the lexical index.
///
/// Returns the number of chunks produced. Zero is normal — a photo has no
/// parser, and photos are 3,478 of this corpus's 41,110 files.
#[allow(clippy::too_many_arguments)]
fn extract(
    store: &Store,
    index: Option<&dyn marrow_index::TextIndex>,
    router: &marrow_parse::ParserRouter,
    policy: &IngestPolicy,
    workspace_id: WorkspaceId,
    h: &Hashed,
    ids: &RecordedIds,
    self_written: &HashSet<ContentHash>,
    inflight: &mut Vec<Pending<()>>,
) -> Result<usize> {
    // **Never hydrate a placeholder** guards the open itself, not a caller's discipline.
    let Some(bytes) = read_for_parsing(&h.path, h.tier, policy.max_parse_bytes)? else {
        return Ok(0);
    };

    let file_name = std::path::Path::new(&h.path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| h.path.clone());

    let input = ContentInput {
        file_id: ids.file_id,
        version_id: ids.version_id,
        workspace_id,
        path: h.path.clone(),
        file_name,
        size: h.size,
        tier: h.tier,
        modified: h.mtime,
        // The chunk carries the same origin as the file, so a citation cannot
        // be built from agent output even if the file row is read separately.
        origin: origin_of(h, self_written),
    };

    let extracted = documents_for(router, &policy.chunking, &input, &bytes)?;
    let docs = extracted.docs;
    let parse = extracted.parse;
    let tables = extracted.tables;
    let version_id = ids.version_id;

    // PAR-003: recorded even when nothing was chunked, so a parser upgrade can
    // find every file it has already seen.
    if docs.is_empty() {
        inflight.push(store.writer().send(move |c| {
            marrow_store::read::record_parse(c, &parse)?;
            // Still replaced, not skipped: a file that *had* a table and no
            // longer does must lose it, or a query cites a grid that is gone.
            marrow_store::read::replace_tables(c, version_id, &tables)
        })?);
        return Ok(0);
    }

    // Chunks first: the index's documents have a foreign key to them, which is
    // D3's consistency property expressed in DDL rather than in prose.
    let rows: Vec<marrow_store::read::NewChunk> = docs
        .iter()
        .zip(extracted.kinds.iter())
        .map(|(d, kind)| marrow_store::read::NewChunk {
            chunk_id: d.chunk_id,
            version_id: d.version_id,
            // The chunker's own classification. It used to be hard-coded
            // `TEXT`, which made `TABLE_BAND` and `TABLE_SCHEMA` (TBL-011)
            // indistinguishable from prose the moment they existed.
            chunk_kind: (*kind).to_string(),
            text: d.body.clone(),
            context_prefix: (!d.title.is_empty()).then(|| d.title.clone()),
            token_count: d.body.len().div_ceil(4) as i64,
            text_hash: marrow_core::ContentHash::of(d.body.as_bytes()),
            chunker_version: marrow_parse::CHUNKER_VERSION.into(),
            provenance_class: format!("{:?}", d.provenance).to_uppercase(),
            // The same span the index document gets, kept canonically too —
            // otherwise a rebuild reads it back as `Whole` and the citation is
            // gone while the text survives.
            source_span: serde_json::to_string(&d.span).ok(),
        })
        .collect();

    let n = rows.len();
    if let Some(ix) = index {
        // One closure, so one transaction: the canonical chunks and their index
        // documents commit together or not at all. That is the D3 property, and
        // it also means the index write already sees the chunks it references —
        // no need to wait for a commit between them.
        //
        // `send`, not `submit`. Submitting blocks until the batch commits (up
        // to 100 ms), which across 34k files is the difference between seconds
        // and an hour. I made exactly this mistake once already in the metadata
        // path; the convenience API is a trap at scale.
        let docs_for_write = docs.clone();
        inflight.push(store.writer().send(move |c| {
            marrow_store::read::record_parse(c, &parse)?;
            marrow_store::read::replace_tables(c, version_id, &tables)?;
            marrow_store::read::replace_chunks(c, version_id, &rows)?;
            marrow_index::fts5::upsert_docs(c, &docs_for_write)
        })?);
        let _ = ix;
    } else {
        inflight.push(store.writer().send(move |c| {
            marrow_store::read::record_parse(c, &parse)?;
            marrow_store::read::replace_tables(c, version_id, &tables)?;
            marrow_store::read::replace_chunks(c, version_id, &rows)
        })?);
    }
    Ok(n)
}

fn new_version(file_id: FileId, h: &Hashed, now: Timestamp) -> NewVersion {
    NewVersion {
        version_id: VersionId::new(),
        file_id,
        path_at_observation: h.path.clone(),
        size_bytes: h.size as i64,
        mtime_ms: h.mtime,
        // A placeholder has no readable bytes, so it gets the hash of nothing
        // rather than a fabricated one. `tier_state` on the file row is what
        // says why.
        content_hash: h.hash.unwrap_or_else(|| ContentHash::of(&[])),
        mime: h.mime.clone(),
        language: None,
        observed_at: now,
    }
}
