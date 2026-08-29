//! Cancellation and progress reporting.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// A cooperative cancellation flag.
///
/// Every stage checks this at its loop boundary, so `Ctrl-C` is honoured within
/// one unit of work rather than at the end of the run ([UX §10] requires 500 ms).
/// Passed explicitly rather than living in a global, so a function's signature
/// says whether it can be interrupted.
///
/// [UX §10]: ../../../docs/UX.md
#[derive(Clone, Debug, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation. Idempotent, callable from any thread.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Which stage a counter belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Discovered,
    Hashed,
    Stored,
    /// Cloud placeholders seen and deliberately not read (TIER-005). Surfaced
    /// because a silent zero here is indistinguishable from "no cloud files",
    /// which is exactly the failure TIER-008 exists to prevent.
    SkippedPlaceholder,
    Unchanged,
    Failed,
}

/// Live counters for a run.
///
/// Cheap enough to update per file (relaxed atomics), so the CLI and the
/// desktop app can both poll it without the pipeline knowing either exists.
#[derive(Debug, Default)]
pub struct Progress {
    discovered: AtomicU64,
    hashed: AtomicU64,
    stored: AtomicU64,
    skipped_placeholder: AtomicU64,
    unchanged: AtomicU64,
    failed: AtomicU64,
}

impl Progress {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn bump(&self, stage: Stage) {
        let c = match stage {
            Stage::Discovered => &self.discovered,
            Stage::Hashed => &self.hashed,
            Stage::Stored => &self.stored,
            Stage::SkippedPlaceholder => &self.skipped_placeholder,
            Stage::Unchanged => &self.unchanged,
            Stage::Failed => &self.failed,
        };
        c.fetch_add(1, Ordering::Relaxed);
    }

    pub fn get(&self, stage: Stage) -> u64 {
        match stage {
            Stage::Discovered => &self.discovered,
            Stage::Hashed => &self.hashed,
            Stage::Stored => &self.stored,
            Stage::SkippedPlaceholder => &self.skipped_placeholder,
            Stage::Unchanged => &self.unchanged,
            Stage::Failed => &self.failed,
        }
        .load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_is_visible_across_threads() {
        let c = Cancel::new();
        assert!(!c.is_cancelled());
        let c2 = c.clone();
        std::thread::spawn(move || c2.cancel()).join().unwrap();
        assert!(c.is_cancelled());
    }

    #[test]
    fn cancel_is_idempotent() {
        let c = Cancel::new();
        c.cancel();
        c.cancel();
        assert!(c.is_cancelled());
    }

    #[test]
    fn counters_are_independent() {
        let p = Progress::new();
        p.bump(Stage::Discovered);
        p.bump(Stage::Discovered);
        p.bump(Stage::Failed);
        assert_eq!(p.get(Stage::Discovered), 2);
        assert_eq!(p.get(Stage::Failed), 1);
        assert_eq!(p.get(Stage::Hashed), 0);
    }
}
