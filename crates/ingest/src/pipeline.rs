//! The staged ingest pipeline.

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
}

impl Default for IngestPolicy {
    fn default() -> Self {
        Self {
            walk: WalkPolicy::default(),
            hash_workers: std::thread::available_parallelism()
                .map(|n| (n.get().saturating_sub(2)).max(1))
                .unwrap_or(2),
            max_hash_bytes: 512 * 1024 * 1024,
        }
    }
}

/// What a run did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IngestOutcome {
    pub discovered: u64,
    pub stored: u64,
    pub unchanged: u64,
    pub skipped_placeholder: u64,
    pub failed: u64,
    pub cancelled: bool,
}

/// One unit in flight between the hash stage and the writer.
#[derive(Debug)]
struct Hashed {
    path: String,
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
    // Bounded so the walk cannot outrun hashing and buffer the corpus.
    let (tx_scan, rx_scan) = sync_channel::<ScanEntry>(1024);
    let (tx_hash, rx_hash) = sync_channel::<Hashed>(256);

    let walk_handle = spawn_walk(root, &policy.walk, tx_scan, progress, cancel);
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

    // Writes are SENT, not submitted. `Store`'s convenience helpers call
    // `submit`, which is `send().wait()` — so every file would block until its
    // batch committed, up to `max_batch_interval` (100 ms). Across a real
    // corpus that is the difference between three seconds and an hour.
    //
    // Handles are drained periodically so a write failure still surfaces
    // instead of being silently dropped.
    let mut outcome = IngestOutcome::default();
    let mut inflight: Vec<Pending<()>> = Vec::with_capacity(DRAIN_EVERY);

    for h in rx_hash {
        if cancel.is_cancelled() {
            outcome.cancelled = true;
            break;
        }
        match record(store, &conn, workspace_id, root_id, &h, &mut inflight) {
            Ok(true) => {
                progress.bump(Stage::Stored);
                outcome.stored += 1;
            }
            Ok(false) => {
                progress.bump(Stage::Unchanged);
                outcome.unchanged += 1;
            }
            Err(e) => {
                // A storage failure is not per-file recoverable the way a parse
                // failure is, but one row failing should not abandon the run.
                warn!(path = %h.path, error = %e, "failed to record file");
                progress.bump(Stage::Failed);
                outcome.failed += 1;
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

    store.flush()?;

    outcome.discovered = progress.get(Stage::Discovered);
    outcome.skipped_placeholder = progress.get(Stage::SkippedPlaceholder);
    outcome.failed = progress.get(Stage::Failed);
    outcome.cancelled |= cancel.is_cancelled();
    Ok(outcome)
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
            outcome.failed += 1;
            outcome.stored = outcome.stored.saturating_sub(1);
        }
    }
}

fn spawn_walk(
    root: &AuthorizedRoot,
    policy: &WalkPolicy,
    tx: SyncSender<ScanEntry>,
    progress: &Arc<Progress>,
    cancel: &Cancel,
) -> thread::JoinHandle<()> {
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
                    debug!(error = %err, "walk entry failed");
                    progress.bump(Stage::Failed);
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

    // **Invariant #5.** A placeholder is recorded from metadata and never
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
                progress.bump(Stage::Failed);
                None
            }
        }
    };

    Some(Hashed {
        path,
        fs_identity: format!("{}:{}", f.identity.dev, f.identity.ino),
        size: f.size,
        mtime: f.mtime,
        tier: f.tier,
        hash,
        mime: f.mime_hint.map(|m| m.as_str().to_string()),
    })
}

/// Record one observation. Returns `true` if anything was written.
///
/// **Path is never identity** (invariant #2): a file is found by filesystem
/// identity first, so a rename keeps its `FileId` and its derived data. Only
/// when identity is unavailable or unknown do we fall back to path.
fn record(
    store: &Store,
    conn: &ReadConn,
    workspace_id: WorkspaceId,
    root_id: RootId,
    h: &Hashed,
    inflight: &mut Vec<Pending<()>>,
) -> Result<bool> {
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
            origin: Origin::User,
            origin_txn_id: None,
            external_source_url: None,
            status: FileStatus::Active,
            at: now,
        };
        let v = new_version(file_id, h, now);
        inflight.push(
            store.writer().send(move |c| {
                marrow_store::read::insert_file_with_version(c, &f, &v).map(|_| ())
            })?,
        );
        return Ok(true);
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
    let changed = match (&current, &h.hash) {
        // No hash on either side (placeholder or over budget): fall back to the
        // cheap signals. This is the only place we trust size+mtime, and only
        // because we are forbidden from reading the bytes.
        (Some(c), None) => c.size_bytes != h.size as i64 || c.mtime_ms != h.mtime,
        (Some(c), Some(new)) => c.content_hash != *new,
        (None, _) => true,
    };

    if changed {
        let v = new_version(file.file_id, h, now);
        inflight.push(
            store
                .writer()
                .send(move |c| marrow_store::read::record_version(c, &v).map(|_| ()))?,
        );
        wrote = true;
    }

    Ok(wrote)
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
