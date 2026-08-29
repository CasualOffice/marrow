//! One bounded queue per model (Part 8 §143).
//!
//! Strict priority, not weighted: a user waiting always wins. Weighted fair
//! queuing would let a batch of enrichment jobs delay an interactive question
//! by a bounded-but-visible amount, and "visible" is the whole problem.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use marrow_core::{Code, Error, RequestId, Timestamp};
use serde::Serialize;

use crate::request::{Priority, Request};

/// Shared with whoever asked, so cancellation reaches a queued request *and* an
/// in-flight one (SUP-007).
#[derive(Clone, Debug, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Clone, Debug)]
struct Entry {
    request: Request,
    cancel: Cancel,
    /// Monotonic within the queue, so equal priorities stay FIFO.
    seq: u64,
}

/// Why a request left the queue without running.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Dropped {
    Expired,
    Cancelled,
}

/// What the UI shows so "it is slow" is answerable (SUP-008).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Depth {
    pub total: usize,
    pub interactive: usize,
    pub background: usize,
    pub enrichment: usize,
    pub capacity: usize,
}

#[derive(Debug)]
pub struct Queue {
    items: VecDeque<Entry>,
    capacity: usize,
    next_seq: u64,
    /// Counted rather than inferred, so the two reasons are distinguishable in
    /// a bug report.
    pub dropped_expired: u64,
    pub dropped_cancelled: u64,
}

impl Queue {
    pub fn new(capacity: usize) -> Self {
        Self {
            items: VecDeque::with_capacity(capacity.min(1024)),
            capacity,
            next_seq: 0,
            dropped_expired: 0,
            dropped_cancelled: 0,
        }
    }

    /// Enqueue, or say why not.
    ///
    /// SUP-004: **a full queue rejects rather than growing.** A queue that
    /// grows converts backpressure into a memory leak and an answer nobody is
    /// still waiting for.
    pub fn push(&mut self, request: Request, cancel: Cancel) -> Result<(), Error> {
        if self.items.len() >= self.capacity {
            return Err(Error::new(
                Code::ModQueueFull,
                format!(
                    "{} requests are already waiting for this model. \
                     Wait for one to finish, or cancel one.",
                    self.items.len()
                ),
            ));
        }
        self.items.push_back(Entry {
            request,
            cancel,
            seq: self.next_seq,
        });
        self.next_seq += 1;
        Ok(())
    }

    /// The next request to run: highest priority, then oldest.
    ///
    /// Drops expired and cancelled entries on the way past rather than in a
    /// sweep — a request whose asker has gone must never reach a worker.
    pub fn pop(&mut self, now: Timestamp) -> Option<Request> {
        loop {
            let best = self
                .items
                .iter()
                .enumerate()
                .max_by_key(|(_, e)| (e.request.priority, std::cmp::Reverse(e.seq)))
                .map(|(i, _)| i)?;
            let e = self.items.remove(best)?;

            if e.cancel.is_cancelled() {
                self.dropped_cancelled += 1;
                continue;
            }
            // SUP-006. Checked here rather than at push, because the deadline
            // usually passes *while* waiting.
            if e.request.expired(now) {
                self.dropped_expired += 1;
                continue;
            }
            return Some(e.request);
        }
    }

    /// Cancel one queued request by id. Returns whether it was found.
    pub fn cancel(&mut self, id: RequestId) -> bool {
        match self.items.iter().position(|e| e.request.id == id) {
            Some(i) => {
                self.items[i].cancel.cancel();
                true
            }
            None => false,
        }
    }

    /// Cancel everything. Used when a model is evicted with work outstanding
    /// (SUP-009) — explicitly, rather than dropping requests silently.
    pub fn cancel_all(&mut self) -> usize {
        for e in &self.items {
            e.cancel.cancel();
        }
        self.items.len()
    }

    /// Remove expired entries without running anything, so `depth()` does not
    /// report a queue of requests nobody is waiting for.
    pub fn evict_expired(&mut self, now: Timestamp) -> usize {
        let before = self.items.len();
        self.items
            .retain(|e| !e.request.expired(now) && !e.cancel.is_cancelled());
        let removed = before - self.items.len();
        self.dropped_expired += removed as u64;
        removed
    }

    pub fn depth(&self) -> Depth {
        let count = |p: Priority| {
            self.items
                .iter()
                .filter(|e| e.request.priority == p)
                .count()
        };
        Depth {
            total: self.items.len(),
            interactive: count(Priority::Interactive),
            background: count(Priority::Background),
            enrichment: count(Priority::Enrichment),
            capacity: self.capacity,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn r(p: Priority) -> Request {
        Request::new("m", p, Duration::from_secs(60))
    }

    fn now() -> Timestamp {
        Timestamp::now()
    }

    #[test]
    fn an_interactive_request_jumps_a_queue_of_enrichment() {
        // SUP-005, strict. A user waiting always wins.
        let mut q = Queue::new(16);
        for _ in 0..5 {
            q.push(r(Priority::Enrichment), Cancel::new()).unwrap();
        }
        let urgent = r(Priority::Interactive);
        let id = urgent.id;
        q.push(urgent, Cancel::new()).unwrap();
        assert_eq!(q.pop(now()).unwrap().id, id, "interactive must come first");
    }

    #[test]
    fn equal_priorities_stay_in_order() {
        // Otherwise "queued" means "randomly ordered", and a queue of five
        // enrichment jobs finishes in an order nobody can predict or debug.
        let mut q = Queue::new(16);
        let ids: Vec<_> = (0..4)
            .map(|_| {
                let req = r(Priority::Background);
                let id = req.id;
                q.push(req, Cancel::new()).unwrap();
                id
            })
            .collect();
        for expected in ids {
            assert_eq!(q.pop(now()).unwrap().id, expected);
        }
    }

    #[test]
    fn a_full_queue_rejects_with_a_message_rather_than_growing() {
        // SUP-004. Growing converts backpressure into a memory leak.
        let mut q = Queue::new(2);
        q.push(r(Priority::Background), Cancel::new()).unwrap();
        q.push(r(Priority::Background), Cancel::new()).unwrap();
        let e = q.push(r(Priority::Background), Cancel::new()).unwrap_err();
        assert_eq!(e.code(), Code::ModQueueFull);
        assert!(e.message().contains("waiting"), "{}", e.message());
        assert!(
            e.message().contains("cancel"),
            "must name a remedy: {}",
            e.message()
        );
        assert_eq!(q.len(), 2, "the queue must not have grown");
    }

    #[test]
    fn an_expired_request_is_never_handed_to_a_worker() {
        // SUP-006. Running it burns memory for an answer nobody will read.
        let mut q = Queue::new(8);
        q.push(
            Request::new("m", Priority::Interactive, Duration::from_millis(0)),
            Cancel::new(),
        )
        .unwrap();
        let live = r(Priority::Enrichment);
        let live_id = live.id;
        q.push(live, Cancel::new()).unwrap();

        let later = Timestamp::from_millis(Timestamp::now().as_millis() + 10);
        // The interactive one outranks the enrichment one but is expired, so
        // the enrichment one runs — and the drop is counted, not silent.
        assert_eq!(q.pop(later).unwrap().id, live_id);
        assert_eq!(q.dropped_expired, 1);
    }

    #[test]
    fn cancellation_reaches_a_request_that_is_still_queued() {
        // SUP-007. A cancel that only works once the request is running is a
        // cancel that does not work when it matters.
        let mut q = Queue::new(8);
        let doomed = r(Priority::Interactive);
        let id = doomed.id;
        let cancel = Cancel::new();
        q.push(doomed, cancel.clone()).unwrap();
        let survivor = r(Priority::Enrichment);
        let survivor_id = survivor.id;
        q.push(survivor, Cancel::new()).unwrap();

        cancel.cancel();
        assert_eq!(q.pop(now()).unwrap().id, survivor_id);
        assert_eq!(q.dropped_cancelled, 1);
        assert_ne!(id, survivor_id);
    }

    #[test]
    fn cancel_by_id_finds_a_queued_request() {
        let mut q = Queue::new(8);
        let req = r(Priority::Background);
        let id = req.id;
        q.push(req, Cancel::new()).unwrap();
        assert!(q.cancel(id));
        assert!(!q.cancel(RequestId::new()), "an unknown id is not a cancel");
        assert!(q.pop(now()).is_none(), "the only entry was cancelled");
    }

    #[test]
    fn eviction_cancels_outstanding_work_explicitly_rather_than_dropping_it() {
        // SUP-009. Silently discarding a queue leaves callers waiting forever.
        let mut q = Queue::new(8);
        let cancels: Vec<_> = (0..3)
            .map(|_| {
                let c = Cancel::new();
                q.push(r(Priority::Background), c.clone()).unwrap();
                c
            })
            .collect();
        assert_eq!(q.cancel_all(), 3);
        assert!(
            cancels.iter().all(|c| c.is_cancelled()),
            "every caller must be told"
        );
    }

    #[test]
    fn depth_breaks_down_by_priority_so_slowness_is_explainable() {
        // SUP-008. "There are 40 enrichment jobs ahead of you" is an answer;
        // "queued" is not.
        let mut q = Queue::new(10);
        q.push(r(Priority::Interactive), Cancel::new()).unwrap();
        for _ in 0..3 {
            q.push(r(Priority::Enrichment), Cancel::new()).unwrap();
        }
        let d = q.depth();
        assert_eq!(d.total, 4);
        assert_eq!(d.interactive, 1);
        assert_eq!(d.enrichment, 3);
        assert_eq!(d.capacity, 10);
    }

    #[test]
    fn evicting_expired_entries_keeps_depth_honest() {
        // A depth that counts requests nobody is waiting for is a wait-time
        // estimate that is wrong in the direction that looks bad.
        let mut q = Queue::new(8);
        for _ in 0..3 {
            q.push(
                Request::new("m", Priority::Background, Duration::from_millis(0)),
                Cancel::new(),
            )
            .unwrap();
        }
        let later = Timestamp::from_millis(Timestamp::now().as_millis() + 10);
        assert_eq!(q.evict_expired(later), 3);
        assert_eq!(q.depth().total, 0);
    }

    #[test]
    fn a_queue_of_only_dead_requests_pops_nothing_rather_than_looping() {
        let mut q = Queue::new(8);
        for _ in 0..4 {
            let c = Cancel::new();
            q.push(r(Priority::Background), c.clone()).unwrap();
            c.cancel();
        }
        assert!(q.pop(now()).is_none());
        assert!(q.is_empty());
    }
}
