//! One sweep per root at a time, across processes.
//!
//! `marrow watch` and the desktop app both reconcile, and both may be running:
//! the app watches while it is open, and a terminal running `marrow watch` has
//! no idea it exists. Nothing coordinated them, so two processes would walk the
//! same 35,000 files, hash them, and hand identical work to the one writer —
//! which serialises it and does it twice.
//!
//! It never corrupted anything, because the store's writer actor is the real
//! serialisation point and the ingest is idempotent. It is simply waste, and
//! waste nobody could see: neither process said it was duplicating the other,
//! and the second sweep's "unchanged" counts looked exactly like a healthy run.
//!
//! **Advisory, and deliberately so.** This is a hint between cooperating
//! processes, not a correctness barrier — the guarantees that matter live in the
//! writer and in the idempotent job keys, and a lock that could deadlock a sweep
//! would be a worse bug than the duplication it prevents. So every failure mode
//! resolves towards *doing the work*: an unreadable lock, an unwritable
//! directory, a stale holder, a clock that moved backwards — all of them mean
//! "go ahead", never "refuse".
//!
//! **A file rather than a row.** The lock has to be visible to a process that
//! may not have the database open yet, must survive that process being killed
//! (which is how it usually ends), and must be inspectable by a person asking
//! why their sweep was skipped. `net-allow.txt` set the precedent for using the
//! filesystem where a person is the other reader.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use marrow_core::RootId;

/// How long a holder's stamp stays believable.
///
/// A sweep of the author's real corpus takes about a minute, and a holder
/// refreshes on every batch, so a stamp older than this means the process is
/// gone rather than slow. Set too low, two processes sweep together and the
/// lock buys nothing; set too high, a crash leaves a root unswept for as long
/// as it takes to expire. Five minutes is comfortably longer than any sweep
/// observed and short enough that a crash costs one reconciliation interval.
pub const STALE_AFTER: Duration = Duration::from_secs(300);

/// A claim on one root's sweep. Releasing is `Drop`.
#[derive(Debug)]
pub struct SweepLock {
    path: PathBuf,
}

impl SweepLock {
    /// Claim `root_id`, or return `None` because someone else holds it.
    ///
    /// `None` means "another process is already doing this" and the caller
    /// should skip its sweep and say so — not retry, and not wait. Waiting
    /// would turn a watcher's loop into a queue behind a sweep it does not need
    /// the results of.
    pub fn acquire(dir: &Path, root_id: RootId) -> Option<Self> {
        let path = lock_path(dir, root_id);

        if let Some(held) = holder(&path) {
            if held < STALE_AFTER {
                return None;
            }
            // The stamp is old. Either the holder died — which is the ordinary
            // way a sweep ends — or it is wedged, and in both cases the root
            // needs sweeping more than it needs protecting.
            tracing::debug!(
                lock = %path.display(),
                age_s = held.as_secs(),
                "taking over a stale sweep lock"
            );
        }

        // Any failure to write is a reason to proceed unlocked, never a reason
        // to skip. A read-only data directory must not stop reconciliation.
        match write_stamp(&path) {
            Ok(()) => Some(Self { path }),
            Err(e) => {
                tracing::debug!(error = %e, "could not take a sweep lock; sweeping anyway");
                Some(Self {
                    path: PathBuf::new(),
                })
            }
        }
    }

    /// Re-stamp, so a long sweep does not look abandoned half way through.
    pub fn refresh(&self) {
        if self.path.as_os_str().is_empty() {
            return;
        }
        let _ = write_stamp(&self.path);
    }
}

impl Drop for SweepLock {
    fn drop(&mut self) {
        if !self.path.as_os_str().is_empty() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn lock_path(dir: &Path, root_id: RootId) -> PathBuf {
    dir.join("sweeps").join(format!("{root_id}.lock"))
}

/// How long ago the current holder stamped it, if anyone holds it.
///
/// An unreadable or unparseable file is treated as unheld: a corrupt lock must
/// not be able to stop reconciliation for ever, which is the failure mode a
/// lock file is most likely to have.
fn holder(path: &Path) -> Option<Duration> {
    let text = fs::read_to_string(path).ok()?;
    let stamped_ms: u64 = text.split_whitespace().next()?.parse().ok()?;
    let now_ms = now_ms()?;
    // A stamp from the future means the clock moved, not that a sweep is
    // running. Treated as expired, because the alternative is a lock nothing
    // can clear until the clock catches up.
    now_ms
        .checked_sub(stamped_ms)
        .map(Duration::from_millis)
        .or(Some(STALE_AFTER))
}

fn write_stamp(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let ms = now_ms().unwrap_or(0);
    // The pid is for the person reading the file, not for the logic — checking
    // whether a pid is alive is racy and tells you nothing about whether *that*
    // process is the one that wrote this.
    let mut f = fs::File::create(path)?;
    writeln!(f, "{ms} pid={}", std::process::id())
}

fn now_ms() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64)
}

/// Where a person should look to see what is holding a sweep.
pub fn sweeps_dir(dir: &Path) -> PathBuf {
    dir.join("sweeps")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> RootId {
        RootId::new()
    }

    #[test]
    fn a_second_process_is_told_the_root_is_already_being_swept() {
        let dir = tempfile::tempdir().expect("dir");
        let r = root();
        let first = SweepLock::acquire(dir.path(), r).expect("first claim");
        assert!(
            SweepLock::acquire(dir.path(), r).is_none(),
            "two processes would have swept the same root at once"
        );
        drop(first);
        assert!(
            SweepLock::acquire(dir.path(), r).is_some(),
            "the lock outlived its holder"
        );
    }

    #[test]
    fn two_roots_do_not_block_each_other() {
        // The unit is a root, not the installation: a busy folder must not stop
        // a quiet one being reconciled.
        let dir = tempfile::tempdir().expect("dir");
        let a = SweepLock::acquire(dir.path(), root()).expect("a");
        let b = SweepLock::acquire(dir.path(), root());
        assert!(b.is_some(), "one root's sweep blocked another's");
        drop(a);
    }

    #[test]
    fn a_stale_stamp_is_taken_over_rather_than_waited_on() {
        // How a sweep usually ends: the process is killed and never removes its
        // file. A lock that only a graceful exit can clear would stop
        // reconciliation permanently after the first crash.
        let dir = tempfile::tempdir().expect("dir");
        let r = root();
        let path = lock_path(dir.path(), r);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let long_ago = now_ms().expect("clock") - (STALE_AFTER.as_millis() as u64) - 1_000;
        fs::write(&path, format!("{long_ago} pid=1\n")).expect("write");

        assert!(
            SweepLock::acquire(dir.path(), r).is_some(),
            "a dead holder's lock was treated as live"
        );
    }

    #[test]
    fn a_corrupt_or_future_stamp_never_blocks_reconciliation() {
        // Both resolve towards doing the work. A lock file is exactly the thing
        // that gets truncated by a crash or written by a machine whose clock
        // then moved, and neither may be able to stop a sweep for ever.
        let dir = tempfile::tempdir().expect("dir");
        for stamp in ["", "not-a-number", "99999999999999 pid=1"] {
            let r = root();
            let path = lock_path(dir.path(), r);
            fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            fs::write(&path, stamp).expect("write");
            assert!(
                SweepLock::acquire(dir.path(), r).is_some(),
                "a {stamp:?} stamp blocked a sweep"
            );
        }
    }

    #[test]
    fn a_refreshed_lock_stays_held_through_a_long_sweep() {
        let dir = tempfile::tempdir().expect("dir");
        let r = root();
        let held = SweepLock::acquire(dir.path(), r).expect("claim");
        held.refresh();
        assert!(
            SweepLock::acquire(dir.path(), r).is_none(),
            "refreshing released the lock"
        );
    }
}
