//! Filesystem watching.
//!
//! # Invariant #6: watchers are hints, reconciliation is truth
//!
//! Every platform's change notification is lossy. FSEvents coalesces and can
//! drop under load, `ReadDirectoryChangesW` overflows its buffer, inotify runs
//! out of watches. A watcher that is believed produces an index that is quietly
//! wrong, which is worse than one that is loudly stale.
//!
//! So nothing here reports what *changed*. It reports what is worth *looking
//! at*, and the caller re-stats and re-fingerprints before believing anything
//! ([Part 1 §8.4]). A missed event costs latency until the next reconciliation;
//! a trusted-but-wrong event costs correctness.
//!
//! # Health is never silent
//!
//! WATCH-009: dropping to poll-only without saying so is the failure mode that
//! matters. A user who knows the watcher is degraded waits; one who does not
//! believes a stale answer.
//!
//! [Part 1 §8.4]: ../../../docs/Part_1_Master_Specification.md

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use marrow_core::{Code, Error, Result};
use notify::{Event, EventKind, RecursiveMode, Watcher as _};
use tracing::{debug, warn};

use crate::path::AuthorizedRoot;

/// How long to gather events before emitting a batch.
///
/// An editor's atomic save is a burst — write to a temp file, rename over the
/// original, sometimes touch permissions. Emitting per event would re-hash the
/// same file three times; waiting a moment collapses it to one.
pub const DEBOUNCE: Duration = Duration::from_millis(300);

/// Upper bound on a batch, so a bulk operation (a `git checkout`, an unzip)
/// does not grow the set without limit before anything is processed.
pub const MAX_BATCH: usize = 4096;

/// What a watcher can tell you about itself (WATCH-008).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Health {
    /// Events are arriving and nothing has been lost.
    Live,
    /// Watching, but something was missed — reconciliation must cover the gap.
    Degraded(&'static str),
    /// Not watching at all. Only the reconciliation sweep will see changes.
    PollOnly(&'static str),
}

impl Health {
    /// Whether the caller must shorten its reconciliation interval (WATCH-004).
    pub fn needs_frequent_reconciliation(&self) -> bool {
        !matches!(self, Health::Live)
    }

    /// The short label the sidebar and `marrow status` render.
    pub fn label(&self) -> &'static str {
        match self {
            Health::Live => "live",
            Health::Degraded(_) => "degraded",
            Health::PollOnly(_) => "poll-only",
        }
    }

    /// Why, in a sentence a human can act on. `None` when healthy.
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            Health::Live => None,
            Health::Degraded(r) | Health::PollOnly(r) => Some(r),
        }
    }
}

/// A batch of paths worth re-examining, plus the watcher's state.
///
/// Deliberately not `Created`/`Modified`/`Deleted`: the platform's opinion of
/// which happened is exactly the part that cannot be trusted. The caller stats
/// the path and decides.
#[derive(Clone, Debug, Default)]
pub struct Hints {
    /// Paths to re-stat and re-fingerprint.
    pub touched: BTreeSet<PathBuf>,
    /// Set when events were lost and only a full sweep can restore truth.
    pub rescan_required: bool,
}

impl Hints {
    pub fn is_empty(&self) -> bool {
        self.touched.is_empty() && !self.rescan_required
    }
}

/// Watches one root and emits coalesced hints.
pub struct Watcher {
    _inner: notify::RecommendedWatcher,
    rx: Receiver<notify::Result<Event>>,
    health: Health,
    root: PathBuf,
}

impl Watcher {
    /// Begin watching. Falls back to poll-only rather than failing outright:
    /// an index that updates every five minutes beats one that will not start.
    pub fn open(root: &AuthorizedRoot) -> Result<Self> {
        let (tx, rx) = channel();
        let mut inner = notify::recommended_watcher(move |res| {
            // A closed receiver means the caller stopped; dropping is correct.
            let _ = tx.send(res);
        })
        .map_err(|e| {
            Error::new(
                Code::FsLocked,
                "Could not start a filesystem watcher. Marrow will fall back to \
                 periodic scanning, so changes may take a few minutes to appear.",
            )
            .with_source(e)
        })?;

        let health = match inner.watch(root.path(), RecursiveMode::Recursive) {
            Ok(()) => Health::Live,
            Err(e) => {
                warn!(root = %root.path().display(), error = %e, "watch failed");
                Health::PollOnly(
                    "The operating system refused to watch this folder. Changes are \
                     picked up by periodic scanning instead.",
                )
            }
        };

        Ok(Self {
            _inner: inner,
            rx,
            health,
            root: root.path().to_path_buf(),
        })
    }

    pub fn health(&self) -> &Health {
        &self.health
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Wait for activity, then gather everything that arrives within
    /// [`DEBOUNCE`] of it.
    ///
    /// Returns `None` when the watcher has shut down. An empty-but-`Some`
    /// result means the wait timed out with nothing to report, which is the
    /// normal idle case.
    pub fn next_batch(&mut self, timeout: Duration) -> Option<Hints> {
        let first = match self.rx.recv_timeout(timeout) {
            Ok(ev) => ev,
            Err(RecvTimeoutError::Timeout) => return Some(Hints::default()),
            Err(RecvTimeoutError::Disconnected) => return None,
        };

        let mut hints = Hints::default();
        self.absorb(first, &mut hints);

        // Keep collecting while events keep arriving, so a burst becomes one
        // batch rather than one batch per event.
        let deadline = Instant::now() + DEBOUNCE;
        while hints.touched.len() < MAX_BATCH {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match self.rx.recv_timeout(left) {
                Ok(ev) => self.absorb(ev, &mut hints),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }

        if hints.touched.len() >= MAX_BATCH {
            // Past this point the batch is no cheaper than a sweep, and holding
            // more paths in memory buys nothing.
            debug!(root = %self.root.display(), "batch cap reached; requesting a sweep");
            hints.rescan_required = true;
        }
        Some(hints)
    }

    fn absorb(&mut self, ev: notify::Result<Event>, hints: &mut Hints) {
        match ev {
            Ok(e) => {
                // A rescan is the only honest response to a queue overflow: we
                // know something changed and not what.
                if matches!(e.kind, EventKind::Other) && e.paths.is_empty() {
                    hints.rescan_required = true;
                    self.degrade(
                        "The system dropped change notifications, so a full \
                                  rescan is scheduled to catch what was missed.",
                    );
                    return;
                }
                for p in e.paths {
                    hints.touched.insert(p);
                }
            }
            Err(e) => {
                warn!(error = %e, "watch error");
                hints.rescan_required = true;
                self.degrade(
                    "The filesystem watcher reported an error, so a full rescan is \
                     scheduled to catch what was missed.",
                );
            }
        }
    }

    /// Record that something was missed. Never silently (WATCH-009).
    fn degrade(&mut self, reason: &'static str) {
        if self.health == Health::Live {
            warn!(root = %self.root.display(), reason, "watcher degraded");
            self.health = Health::Degraded(reason);
        }
    }
}

impl std::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watcher")
            .field("root", &self.root)
            .field("health", &self.health)
            .finish_non_exhaustive()
    }
}

/// How often to sweep, given the watcher's state (WATCH-010).
///
/// A healthy watcher makes reconciliation a backstop; a degraded one makes it
/// the primary mechanism, so the interval collapses.
pub fn reconcile_interval(health: &Health) -> Duration {
    match health {
        Health::Live => Duration::from_secs(6 * 3600),
        Health::Degraded(_) => Duration::from_secs(15 * 60),
        Health::PollOnly(_) => Duration::from_secs(5 * 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> (tempfile::TempDir, AuthorizedRoot) {
        let td = tempfile::tempdir().unwrap();
        let r = AuthorizedRoot::open(td.path()).unwrap();
        (td, r)
    }

    #[test]
    fn a_new_watcher_starts_live() {
        let (_td, r) = root();
        let w = Watcher::open(&r).unwrap();
        assert_eq!(*w.health(), Health::Live);
        assert!(!w.health().needs_frequent_reconciliation());
        assert_eq!(w.health().reason(), None);
    }

    #[test]
    fn a_write_produces_a_hint() {
        let (td, r) = root();
        let mut w = Watcher::open(&r).unwrap();
        std::fs::write(td.path().join("a.txt"), "hello").unwrap();

        // Generous: FSEvents batches on its own schedule, and this test is
        // about the hint arriving at all, not about latency.
        let hints = w
            .next_batch(Duration::from_secs(5))
            .expect("watcher should still be open");
        assert!(
            hints.touched.iter().any(|p| p.ends_with("a.txt")) || hints.rescan_required,
            "expected a hint for a.txt, got {hints:?}"
        );
    }

    #[test]
    fn a_burst_collapses_into_one_batch() {
        // An editor's atomic save is several events for one logical change.
        // Emitting per event would re-hash the same file repeatedly.
        let (td, r) = root();
        let mut w = Watcher::open(&r).unwrap();
        for i in 0..20 {
            std::fs::write(td.path().join(format!("f{i}.txt")), "x").unwrap();
        }
        let hints = w.next_batch(Duration::from_secs(5)).unwrap();
        assert!(
            hints.touched.len() > 1 || hints.rescan_required,
            "a burst should coalesce, got {} hints",
            hints.touched.len()
        );
    }

    #[test]
    fn an_idle_watcher_returns_an_empty_batch_not_a_shutdown() {
        let (_td, r) = root();
        let mut w = Watcher::open(&r).unwrap();
        let hints = w.next_batch(Duration::from_millis(50)).unwrap();
        assert!(hints.is_empty());
    }

    #[test]
    fn degrading_is_recorded_and_sticks() {
        let (_td, r) = root();
        let mut w = Watcher::open(&r).unwrap();
        w.degrade("test reason");
        assert_eq!(w.health().label(), "degraded");
        assert_eq!(w.health().reason(), Some("test reason"));
        assert!(w.health().needs_frequent_reconciliation());
    }

    #[test]
    fn a_watch_error_demands_a_rescan_rather_than_being_swallowed() {
        // Invariant #6: a lost event is a correctness problem, and the only
        // honest response is to look again.
        let (_td, r) = root();
        let mut w = Watcher::open(&r).unwrap();
        let mut hints = Hints::default();
        w.absorb(
            Err(notify::Error::generic("simulated overflow")),
            &mut hints,
        );
        assert!(hints.rescan_required);
        assert_eq!(w.health().label(), "degraded");
    }

    #[test]
    fn the_sweep_interval_shortens_as_health_drops() {
        // WATCH-010: a degraded watcher makes reconciliation the primary
        // mechanism, not a backstop.
        let live = reconcile_interval(&Health::Live);
        let degraded = reconcile_interval(&Health::Degraded("x"));
        let poll = reconcile_interval(&Health::PollOnly("x"));
        assert!(degraded < live);
        assert!(poll < degraded);
    }

    #[test]
    fn every_unhealthy_state_can_explain_itself() {
        // WATCH-009: never silently degraded.
        for h in [Health::Degraded("a"), Health::PollOnly("b")] {
            assert!(h.reason().is_some(), "{h:?} must explain itself");
            assert!(h.needs_frequent_reconciliation());
        }
    }
}
