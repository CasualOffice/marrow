//! The circuit breaker (Part 8 §142.4).
//!
//! The explicit ask: *what if the model keeps failing?* The answer is not
//! "retry harder". Three failures in a row is information — the model is too
//! large, the weights are corrupt, or the machine changed. Retrying faster
//! converts a broken feature into a hot laptop.

use std::time::Duration;

use marrow_core::Timestamp;
use serde::{Deserialize, Serialize};

/// Failures needed before each escalation, and how long the cooldown lasts.
const LADDER: &[(u32, Duration)] = &[(3, Duration::from_secs(30)), (5, Duration::from_secs(300))];

/// Failures after which only the user can clear it.
const MANUAL: u32 = 8;

/// Whether this model may be attempted, and if not, why and for how long.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum BreakerState {
    Closed,
    /// Cooling down. `until` is when one more attempt is granted.
    Open {
        until: Timestamp,
    },
    /// Eight failures. Waiting will not help; the user is told what failed
    /// first and clears it deliberately.
    NeedsIntervention,
}

/// Per-model failure accounting.
///
/// Persisted with the registry entry (§138.1). **A breaker that resets on
/// relaunch does nothing for a model that fails at load** — which is the most
/// common way a model fails.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Breaker {
    pub consecutive_failures: u32,
    /// The first failure of the current run, kept verbatim. By failure eight
    /// the last error is usually a consequence; the first one is the cause.
    pub first_error: Option<String>,
    pub last_error: Option<String>,
    /// When the current cooldown ends. `None` when closed.
    pub open_until: Option<Timestamp>,
}

impl Breaker {
    pub fn record_failure(&mut self, now: Timestamp, error: impl Into<String>) {
        let e = error.into();
        self.consecutive_failures += 1;
        if self.first_error.is_none() {
            self.first_error = Some(e.clone());
        }
        self.last_error = Some(e);

        // Longest matching rung wins, so failure 6 gets the 5-minute cooldown
        // rather than the 30-second one.
        self.open_until = LADDER
            .iter()
            .rev()
            .find(|(n, _)| self.consecutive_failures >= *n)
            .map(|(_, d)| Timestamp::from_millis(now.as_millis() + d.as_millis() as i64));
    }

    /// A success resets everything. Only a success — see [`Breaker::state`].
    pub fn record_success(&mut self) {
        *self = Breaker::default();
    }

    pub fn state(&self, now: Timestamp) -> BreakerState {
        if self.consecutive_failures >= MANUAL {
            return BreakerState::NeedsIntervention;
        }
        match self.open_until {
            // Cooldown expiry grants one attempt; it does **not** reset the
            // count. Otherwise a model that fails every 31 seconds never
            // escalates past the first rung.
            Some(t) if now.as_millis() < t.as_millis() => BreakerState::Open { until: t },
            _ => BreakerState::Closed,
        }
    }

    pub fn is_open(&self, now: Timestamp) -> bool {
        !matches!(self.state(now), BreakerState::Closed)
    }

    /// The user clearing it by hand. Distinct from a success, because it is.
    pub fn reset_by_user(&mut self) {
        *self = Breaker::default();
    }

    /// What to show beside `Suspended` (SUP-002). Never silent.
    pub fn explain(&self, now: Timestamp) -> Option<String> {
        match self.state(now) {
            BreakerState::Closed => None,
            BreakerState::Open { until } => {
                let secs = (until.as_millis() - now.as_millis()).max(0) / 1000;
                Some(format!(
                    "Suspended after {} failures; retrying in {}s. First failure: {}",
                    self.consecutive_failures,
                    secs,
                    self.first_error.as_deref().unwrap_or("unknown")
                ))
            }
            BreakerState::NeedsIntervention => Some(format!(
                "Suspended after {} failures. It will not retry on its own. \
                 First failure: {}",
                self.consecutive_failures,
                self.first_error.as_deref().unwrap_or("unknown")
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: i64) -> Timestamp {
        Timestamp::from_millis(ms)
    }

    #[test]
    fn two_failures_do_not_trip_it() {
        let mut b = Breaker::default();
        b.record_failure(at(0), "oom");
        b.record_failure(at(1), "oom");
        assert!(!b.is_open(at(2)));
    }

    #[test]
    fn three_failures_open_it_for_thirty_seconds() {
        let mut b = Breaker::default();
        for i in 0..3 {
            b.record_failure(at(i), "oom");
        }
        assert!(b.is_open(at(10_000)));
        assert!(!b.is_open(at(40_000)), "30s cooldown must have elapsed");
    }

    #[test]
    fn the_ladder_escalates_rather_than_repeating_the_first_rung() {
        let mut b = Breaker::default();
        for i in 0..5 {
            b.record_failure(at(i), "oom");
        }
        // Still open a minute later: rung two is five minutes, not thirty
        // seconds.
        assert!(b.is_open(at(60_000)), "{:?}", b.state(at(60_000)));
    }

    #[test]
    fn eight_failures_stop_retrying_entirely() {
        let mut b = Breaker::default();
        for i in 0..8 {
            b.record_failure(at(i), "oom");
        }
        // Not "in an hour" — never, until a human says so.
        assert_eq!(b.state(at(86_400_000)), BreakerState::NeedsIntervention);
    }

    #[test]
    fn a_cooldown_expiry_grants_an_attempt_but_does_not_reset_the_count() {
        // The rule that makes escalation work. If expiry reset the count, a
        // model failing every 31 seconds would sit on rung one forever.
        let mut b = Breaker::default();
        for i in 0..3 {
            b.record_failure(at(i), "oom");
        }
        assert!(!b.is_open(at(40_000)));
        assert_eq!(b.consecutive_failures, 3, "count must survive the cooldown");
        b.record_failure(at(40_001), "oom");
        assert_eq!(b.consecutive_failures, 4);
    }

    #[test]
    fn only_a_success_resets_it() {
        let mut b = Breaker::default();
        for i in 0..4 {
            b.record_failure(at(i), "oom");
        }
        b.record_success();
        assert_eq!(b.consecutive_failures, 0);
        assert!(!b.is_open(at(5)));
        assert_eq!(b.first_error, None);
    }

    #[test]
    fn it_keeps_the_first_failure_not_only_the_last() {
        // By failure eight the last error is usually a consequence.
        let mut b = Breaker::default();
        b.record_failure(at(0), "weights sha mismatch");
        b.record_failure(at(1), "worker exited 137");
        b.record_failure(at(2), "worker exited 137");
        assert_eq!(b.first_error.as_deref(), Some("weights sha mismatch"));
        assert_eq!(b.last_error.as_deref(), Some("worker exited 137"));
        assert!(b.explain(at(3)).unwrap().contains("weights sha mismatch"));
    }

    #[test]
    fn a_suspended_model_always_has_a_reason_to_show() {
        // SUP-002: never silent.
        let mut b = Breaker::default();
        assert!(b.explain(at(0)).is_none(), "closed needs no explanation");
        for i in 0..3 {
            b.record_failure(at(i), "oom");
        }
        let msg = b.explain(at(1000)).expect("open must explain itself");
        assert!(msg.contains("retrying in"), "{msg}");
        assert!(msg.contains("First failure"), "{msg}");
    }

    #[test]
    fn breaker_state_survives_a_round_trip_to_disk() {
        // §138.1: it is persisted, or it is not a circuit breaker.
        let mut b = Breaker::default();
        for i in 0..3 {
            b.record_failure(at(i), "oom");
        }
        let json = serde_json::to_string(&b).unwrap();
        let back: Breaker = serde_json::from_str(&json).unwrap();
        assert_eq!(back, b);
        assert!(back.is_open(at(1000)), "a restart must not clear it");
    }
}
