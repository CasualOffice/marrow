//! The supervisor (Part 8 §142).
//!
//! One long-lived thread that owns the live sampler, the state of every model,
//! the queues, admission and the circuit breakers. **It owns no inference.**
//! Workers do that; the supervisor decides whether they may.
//!
//! ```text
//!                  ┌──────────────────────────┐
//!    requests ───▶ │        Supervisor        │ ──▶ worker process
//!                  │  sampler · queue · state │ ◀── health, results
//!                  └───────────┬──────────────┘
//!                              │ events
//!                              ▼
//!                     UI · CLI · MCP
//! ```
//!
//! SUP-003: **a state change emits an event; nothing polls to find out.** The
//! UI is a subscriber, not a poller, because a poller either lags or burns the
//! battery this whole subsystem exists to protect.

use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use marrow_core::Timestamp;
use marrow_hw::{Conditions, KvPrecision, Machine, Requirement, Sampler};
use serde::Serialize;

use crate::admission::{self, Context, Decision, Overrides, Policy};
use crate::breaker::BreakerState;
use crate::kv::PrefixCache;
use crate::queue::{Cancel, Depth, Queue};
use crate::registry::{Entry, Registry};
use crate::request::Request;

/// Default queue depth per model.
const QUEUE_CAPACITY: usize = 32;

/// How often the supervisor wakes when it has nothing to do.
const IDLE_TICK: Duration = Duration::from_millis(500);

/// Default idle timeout before a loaded model is released (LLM-048).
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(180);

/// Bounds on the idle timeout the user may set.
pub const IDLE_TIMEOUT_RANGE: (Duration, Duration) =
    (Duration::from_secs(120), Duration::from_secs(300));

/// Where a model is in its lifecycle (§142.2, LLM-053).
///
/// ```text
/// Absent ──download──▶ Installed ──load──▶ Ready ──request──▶ Busy
///                           ▲                 │                │
///                           │            idle timeout          │
///                           └─────evict───────┘                │
///                                                              │
///    Suspended ◀──breaker trips──── Failing ◀──error───────────┘
///        │
///        └──cooldown elapsed, conditions improved──▶ Ready
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum ModelState {
    Absent,
    Installed,
    Loading { stage: LoadStage },
    Ready,
    Busy,
    Unloading,
    Suspended { reason: String },
}

/// SKEL-006: model load shows the stage, not a spinner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoadStage {
    Downloading,
    Verifying,
    Loading,
}

/// Everything the UI, CLI and MCP subscribe to.
///
/// SUP-001: every transition carries its reason. "The model stopped working" is
/// not a diagnosis.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "event")]
pub enum Event {
    StateChanged {
        model_id: String,
        from: ModelState,
        to: ModelState,
        reason: String,
    },
    Admitted {
        model_id: String,
        request_id: String,
    },
    Refused {
        model_id: String,
        request_id: String,
        code: String,
        reason: String,
        overridable: bool,
    },
    Deferred {
        model_id: String,
        request_id: String,
        reason: String,
        depth: Depth,
    },
    /// Emitted whenever the queue or the memory picture moves materially, so
    /// "it is slow" is answerable (SUP-008) without polling.
    Pressure {
        available_bytes: u64,
        sustained_load: f32,
        resident_bytes: u64,
    },
}

/// What the supervisor is asked to do.
#[derive(Debug)]
pub enum Command {
    Submit {
        request: Request,
        cancel: Cancel,
        overrides: Overrides,
        policy: Policy,
    },
    Cancel(marrow_core::RequestId),
    /// The user clearing a breaker by hand (§142.4).
    ResetBreaker(String),
    /// Release a model now rather than waiting out the idle timer.
    Unload {
        model_id: String,
        reason: String,
    },
    Shutdown,
}

/// Per-model runtime state the supervisor keeps.
#[derive(Debug)]
struct Slot {
    state: ModelState,
    queue: Queue,
    cache: PrefixCache,
    /// `None` while unloaded.
    resident_bytes: Option<u64>,
    last_used: Timestamp,
}

/// The supervisor's own state. Separated from the thread so every decision is
/// testable without spawning anything.
#[derive(Debug)]
pub struct Supervisor {
    machine: Machine,
    registry: Registry,
    slots: BTreeMap<String, Slot>,
    idle_timeout: Duration,
    events: Vec<Event>,
}

impl Supervisor {
    pub fn new(machine: Machine, registry: Registry) -> Self {
        let slots = registry
            .iter()
            .map(|e| {
                let cache_budget =
                    Requirement::estimate(&machine, &e.shape(8192, KvPrecision::F16))
                        .cache_budget();
                (
                    e.id.clone(),
                    Slot {
                        state: if e.installed {
                            ModelState::Installed
                        } else {
                            ModelState::Absent
                        },
                        queue: Queue::new(QUEUE_CAPACITY),
                        cache: PrefixCache::new(cache_budget),
                        resident_bytes: None,
                        last_used: Timestamp::EPOCH,
                    },
                )
            })
            .collect();
        Self {
            machine,
            registry,
            slots,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            events: Vec::new(),
        }
    }

    /// Clamped to [`IDLE_TIMEOUT_RANGE`]: shorter thrashes the load path,
    /// longer holds the budget for a session that has ended.
    pub fn with_idle_timeout(mut self, d: Duration) -> Self {
        self.idle_timeout = d.clamp(IDLE_TIMEOUT_RANGE.0, IDLE_TIMEOUT_RANGE.1);
        self
    }

    pub fn idle_timeout(&self) -> Duration {
        self.idle_timeout
    }

    pub fn state_of(&self, model_id: &str) -> Option<&ModelState> {
        self.slots.get(model_id).map(|s| &s.state)
    }

    pub fn depth_of(&self, model_id: &str) -> Option<Depth> {
        self.slots.get(model_id).map(|s| s.queue.depth())
    }

    /// Total memory held by loaded models and their caches. This is the number
    /// the UI puts beside the lifecycle state (LLM-053).
    pub fn resident_bytes(&self) -> u64 {
        self.slots
            .values()
            .map(|s| s.resident_bytes.unwrap_or(0) + s.cache.used_bytes())
            .sum()
    }

    /// Drain events for a subscriber.
    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    fn transition(&mut self, model_id: &str, to: ModelState, reason: impl Into<String>) {
        let Some(slot) = self.slots.get_mut(model_id) else {
            return;
        };
        let from = std::mem::replace(&mut slot.state, to.clone());
        if from == to {
            return;
        }
        let reason = reason.into();
        // SUP-001: logged with the reason, at the same moment it is emitted, so
        // the log and the UI can never disagree about why.
        tracing::info!(model = model_id, ?from, ?to, %reason, "model state");
        self.events.push(Event::StateChanged {
            model_id: model_id.to_string(),
            from,
            to,
            reason,
        });
    }

    /// Offer a request. Returns the decision and queues it when admitted or
    /// deferred.
    pub fn submit(
        &mut self,
        request: Request,
        cancel: Cancel,
        conditions: &Conditions,
        now: Timestamp,
        overrides: Overrides,
        policy: Policy,
    ) -> Decision {
        let Some(entry) = self.registry.get(&request.model_id).cloned() else {
            return Decision::Refuse {
                code: marrow_core::Code::ModNotInstalled,
                reason: format!("No model called {}.", request.model_id),
                overridable: false,
            };
        };
        let shape = entry.shape(entry.context_limit, KvPrecision::F16);
        let decision = admission::admit(
            &entry,
            &request,
            &shape,
            Context {
                machine: &self.machine,
                conditions,
                now,
                overrides,
                policy,
            },
        );

        let request_id = request.id.to_string();
        match &decision {
            Decision::Admit | Decision::Defer { .. } => {
                let Some(slot) = self.slots.get_mut(&entry.id) else {
                    return decision;
                };
                if let Err(e) = slot.queue.push(request, cancel) {
                    return Decision::Refuse {
                        code: e.code(),
                        reason: e.message().to_string(),
                        overridable: true,
                    };
                }
                let depth = slot.queue.depth();
                slot.last_used = now;
                match &decision {
                    Decision::Defer { reason } => self.events.push(Event::Deferred {
                        model_id: entry.id.clone(),
                        request_id,
                        reason: reason.clone(),
                        depth,
                    }),
                    _ => self.events.push(Event::Admitted {
                        model_id: entry.id.clone(),
                        request_id,
                    }),
                }
            }
            Decision::Refuse {
                code,
                reason,
                overridable,
            } => {
                self.events.push(Event::Refused {
                    model_id: entry.id.clone(),
                    request_id,
                    code: code.as_str().to_string(),
                    reason: reason.clone(),
                    overridable: *overridable,
                });
            }
        }
        decision
    }

    /// Record that a model failed, running the breaker ladder.
    pub fn record_failure(&mut self, model_id: &str, now: Timestamp, error: impl Into<String>) {
        let Some(entry) = self.registry.get_mut(model_id) else {
            return;
        };
        entry.breaker.record_failure(now, error);
        let state = entry.breaker.state(now);
        let explanation = entry.breaker.explain(now);
        if !matches!(state, BreakerState::Closed) {
            let reason = explanation.unwrap_or_else(|| "Suspended.".into());
            self.transition(
                model_id,
                ModelState::Suspended {
                    reason: reason.clone(),
                },
                reason,
            );
        }
    }

    pub fn record_success(&mut self, model_id: &str, now: Timestamp) {
        if let Some(entry) = self.registry.get_mut(model_id) {
            entry.breaker.record_success();
        }
        if let Some(slot) = self.slots.get_mut(model_id) {
            slot.last_used = now;
        }
        if matches!(self.state_of(model_id), Some(ModelState::Suspended { .. })) {
            self.transition(model_id, ModelState::Ready, "A request succeeded.");
        }
    }

    /// Called by the user (§142.4). Distinct from a success, and it says what
    /// failed first.
    pub fn reset_breaker(&mut self, model_id: &str) -> Option<String> {
        let entry = self.registry.get_mut(model_id)?;
        let first = entry.breaker.first_error.clone();
        entry.breaker.reset_by_user();
        self.transition(model_id, ModelState::Installed, "Reset by the user.");
        first
    }

    /// Mark a model loaded and resident.
    pub fn mark_loaded(&mut self, model_id: &str, bytes: u64, now: Timestamp) {
        if let Some(slot) = self.slots.get_mut(model_id) {
            slot.resident_bytes = Some(bytes);
            slot.last_used = now;
        }
        self.transition(model_id, ModelState::Ready, "Loaded.");
    }

    /// Release a model. Refuses while work is outstanding (SUP-009): eviction
    /// waits for the queue to drain or cancels it *explicitly*, never
    /// mid-request.
    pub fn unload(&mut self, model_id: &str, reason: &str, force: bool) -> bool {
        let Some(slot) = self.slots.get_mut(model_id) else {
            return false;
        };
        if slot.state == ModelState::Busy && !force {
            return false;
        }
        if !slot.queue.is_empty() {
            if !force {
                return false;
            }
            // Told, not dropped. A silently discarded queue leaves callers
            // waiting forever.
            slot.queue.cancel_all();
        }
        // LLM-049: weights, cache and buffers. A model unloaded while its cache
        // stays resident has not been unloaded.
        slot.cache.clear();
        slot.resident_bytes = None;
        self.transition(model_id, ModelState::Installed, reason);
        true
    }

    /// Periodic housekeeping: idle eviction, pressure eviction, expiry sweep.
    ///
    /// LLM-051: pressure is checked **before** the idle timer. Waiting out a
    /// three-minute timer while the machine swaps is the wrong order of events.
    pub fn tick(&mut self, conditions: &Conditions, now: Timestamp) {
        let ids: Vec<String> = self.slots.keys().cloned().collect();

        for id in &ids {
            if let Some(slot) = self.slots.get_mut(id) {
                slot.queue.evict_expired(now);
            }
        }

        let under_pressure = conditions.min_available_bytes < pressure_floor(&self.machine);
        for id in &ids {
            let Some(slot) = self.slots.get(id) else {
                continue;
            };
            if slot.resident_bytes.is_none() {
                continue;
            }
            let idle_ms = now.as_millis() - slot.last_used.as_millis();
            if under_pressure {
                self.unload(
                    id,
                    &format!(
                        "Released early: only {:.1} GB free.",
                        conditions.min_available_bytes as f64 / 1e9
                    ),
                    false,
                );
            } else if idle_ms >= self.idle_timeout.as_millis() as i64 {
                self.unload(
                    id,
                    &format!("Idle for {} minutes.", idle_ms / 60_000),
                    false,
                );
            }
        }

        self.events.push(Event::Pressure {
            available_bytes: conditions.min_available_bytes,
            sustained_load: conditions.sustained_load,
            resident_bytes: self.resident_bytes(),
        });
    }

    /// Take the next request to run for a model.
    pub fn next_request(&mut self, model_id: &str, now: Timestamp) -> Option<Request> {
        self.slots.get_mut(model_id)?.queue.pop(now)
    }

    pub fn cancel(&mut self, id: marrow_core::RequestId) -> bool {
        self.slots.values_mut().any(|s| s.queue.cancel(id))
    }

    pub fn entry(&self, model_id: &str) -> Option<&Entry> {
        self.registry.get(model_id)
    }

    pub fn cache_mut(&mut self, model_id: &str) -> Option<&mut PrefixCache> {
        self.slots.get_mut(model_id).map(|s| &mut s.cache)
    }
}

/// Below this much free memory, loaded models are released without waiting for
/// the idle timer.
fn pressure_floor(machine: &Machine) -> u64 {
    if machine.unified_memory {
        2_000_000_000
    } else {
        (machine.total_memory_bytes as f64 * 0.10) as u64
    }
}

/// Runs a [`Supervisor`] on its own thread.
///
/// The sampler ticks here rather than on its own thread: two threads to read
/// one number would be two threads to keep alive, stop and test.
pub fn run(
    mut supervisor: Supervisor,
    sampler: Sampler,
    commands: Receiver<Command>,
    events: Sender<Event>,
) {
    let tolerance = sampler.interval() * 4;
    let mut last_sample = std::time::Instant::now();
    let mut stopping = false;

    while !stopping {
        if last_sample.elapsed() >= sampler.interval() {
            sampler.tick();
            last_sample = std::time::Instant::now();
            let conditions = sampler.conditions(tolerance);
            supervisor.tick(&conditions, Timestamp::now());
        }

        // Wait no longer than the next sample is due. A flat 500 ms wait would
        // make a 2 s sampler tick every 2.5 s under load and every 2.0 s idle,
        // which is exactly the kind of drift that makes a memory reading
        // arrive after the memory is gone.
        let wait = sampler
            .interval()
            .saturating_sub(last_sample.elapsed())
            .min(IDLE_TICK)
            .max(Duration::from_millis(1));

        match commands.recv_timeout(wait) {
            Ok(Command::Shutdown) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                stopping = true;
            }
            Ok(Command::Submit {
                request,
                cancel,
                overrides,
                policy,
            }) => {
                let conditions = sampler.conditions(tolerance);
                supervisor.submit(
                    request,
                    cancel,
                    &conditions,
                    Timestamp::now(),
                    overrides,
                    policy,
                );
            }
            Ok(Command::Cancel(id)) => {
                supervisor.cancel(id);
            }
            Ok(Command::ResetBreaker(id)) => {
                supervisor.reset_breaker(&id);
            }
            Ok(Command::Unload { model_id, reason }) => {
                supervisor.unload(&model_id, &reason, false);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }

        // Drained on every pass **including the last**. Shutting down with
        // events still buffered loses the final state transitions, which are
        // the ones a crash report needs most.
        for e in supervisor.take_events() {
            // A disconnected subscriber is not a reason to stop supervising.
            if events.send(e).is_err() {
                break;
            }
        }
    }
    tracing::info!("supervisor stopped");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue;
    use crate::request::Priority;
    use marrow_hw::{Sample, Thermal};

    const MODEL: &str = "qwen3.5-4b-mlx-q4";

    fn machine() -> Machine {
        Machine {
            total_memory_bytes: 17_179_869_184,
            cpu_cores: 10,
            unified_memory: true,
            ..Machine::unknown()
        }
    }

    fn registry(installed: bool) -> Registry {
        let mut r = Registry::new();
        for mut e in catalogue::builtin() {
            e.installed = installed;
            r.insert(e);
        }
        r
    }

    fn sup() -> Supervisor {
        Supervisor::new(machine(), registry(true))
    }

    fn conditions(available: u64) -> Conditions {
        Conditions {
            latest: Sample {
                available_memory_bytes: available,
                cpu_load: 0.2,
                thermal: Thermal::Unknown,
                on_battery: false,
                battery_level: None,
                taken_at_ms: 0,
            },
            min_available_bytes: available,
            sustained_load: 0.2,
            stale: false,
        }
    }

    fn at(ms: i64) -> Timestamp {
        Timestamp::from_millis(ms)
    }

    fn req() -> Request {
        Request::new(MODEL, Priority::Interactive, Duration::from_secs(600))
    }

    #[test]
    fn a_state_change_emits_an_event_with_a_reason() {
        // SUP-001 and SUP-003 together: nothing polls, and nothing says "the
        // model stopped working".
        let mut s = sup();
        s.mark_loaded(MODEL, 3_000_000_000, at(0));
        let events = s.take_events();
        let Some(Event::StateChanged {
            from, to, reason, ..
        }) = events.first()
        else {
            panic!("expected a state change, got {events:?}");
        };
        assert_eq!(*from, ModelState::Installed);
        assert_eq!(*to, ModelState::Ready);
        assert!(!reason.is_empty());
    }

    #[test]
    fn nothing_is_resident_before_the_first_request() {
        // LLM-047. An app that costs 3 GB before being asked anything is an
        // app people quit.
        let s = sup();
        assert_eq!(s.resident_bytes(), 0);
        assert_eq!(s.state_of(MODEL), Some(&ModelState::Installed));
    }

    #[test]
    fn an_uninstalled_model_starts_absent_not_installed() {
        let s = Supervisor::new(machine(), registry(false));
        assert_eq!(s.state_of(MODEL), Some(&ModelState::Absent));
    }

    #[test]
    fn a_model_idle_past_the_timeout_is_released() {
        let mut s = sup();
        s.mark_loaded(MODEL, 3_000_000_000, at(0));
        assert_eq!(s.resident_bytes(), 3_000_000_000);
        s.tick(
            &conditions(9_000_000_000),
            at(DEFAULT_IDLE_TIMEOUT.as_millis() as i64 + 1),
        );
        assert_eq!(s.resident_bytes(), 0, "the idle timer must release it");
        assert_eq!(s.state_of(MODEL), Some(&ModelState::Installed));
    }

    #[test]
    fn memory_pressure_releases_before_the_idle_timer_does() {
        // LLM-051. Waiting out a three-minute timer while the machine swaps is
        // the wrong order of events.
        let mut s = sup();
        s.mark_loaded(MODEL, 3_000_000_000, at(0));
        s.take_events(); // the "Loaded." transition; we are asserting on the next one
        s.tick(&conditions(500_000_000), at(1_000)); // one second later
        assert_eq!(s.resident_bytes(), 0);
        let reason = s
            .take_events()
            .into_iter()
            .find_map(|e| match e {
                Event::StateChanged { reason, .. } => Some(reason),
                _ => None,
            })
            .expect("must explain itself");
        assert!(reason.contains("free"), "must name the number: {reason}");
    }

    #[test]
    fn unloading_releases_the_cache_as_well_as_the_weights() {
        // LLM-049.
        let mut s = sup();
        s.mark_loaded(MODEL, 3_000_000_000, at(0));
        let cache = s.cache_mut(MODEL).unwrap();
        cache.insert(
            crate::kv::Scope {
                workspace: None,
                classification: 0,
            },
            crate::kv::PrefixKey::of(&[1, 2, 3]),
            3,
            50_000_000,
        );
        assert!(s.resident_bytes() > 3_000_000_000);
        assert!(s.unload(MODEL, "test", false));
        assert_eq!(s.resident_bytes(), 0, "the cache must go too");
    }

    #[test]
    fn eviction_never_interrupts_outstanding_work() {
        // SUP-009. It waits for the queue to drain, or cancels it explicitly.
        let mut s = sup();
        s.mark_loaded(MODEL, 3_000_000_000, at(0));
        s.submit(
            req(),
            Cancel::new(),
            &conditions(9_000_000_000),
            at(0),
            Overrides::default(),
            Policy::default(),
        );
        assert!(
            !s.unload(MODEL, "idle", false),
            "must refuse while work is queued"
        );
        assert_eq!(s.resident_bytes(), 3_000_000_000);
        assert!(s.unload(MODEL, "shutting down", true), "force must work");
    }

    #[test]
    fn a_forced_unload_tells_the_waiting_callers_rather_than_dropping_them() {
        let mut s = sup();
        s.mark_loaded(MODEL, 3_000_000_000, at(0));
        let cancel = Cancel::new();
        s.submit(
            req(),
            cancel.clone(),
            &conditions(9_000_000_000),
            at(0),
            Overrides::default(),
            Policy::default(),
        );
        s.unload(MODEL, "shutting down", true);
        assert!(
            cancel.is_cancelled(),
            "a silently discarded queue leaves callers waiting forever"
        );
    }

    #[test]
    fn three_failures_suspend_the_model_and_the_state_says_why() {
        // SUP-002: `Suspended` is visible with its reason, never silent.
        let mut s = sup();
        for i in 0..3 {
            s.record_failure(MODEL, at(i), "worker exited 137");
        }
        let Some(ModelState::Suspended { reason }) = s.state_of(MODEL) else {
            panic!("expected Suspended, got {:?}", s.state_of(MODEL))
        };
        assert!(reason.contains("worker exited 137"), "{reason}");
        assert!(reason.contains("retrying in"), "{reason}");
    }

    #[test]
    fn a_suspended_model_refuses_new_requests_with_its_cooldown() {
        let mut s = sup();
        for i in 0..3 {
            s.record_failure(MODEL, at(i), "oom");
        }
        let d = s.submit(
            req(),
            Cancel::new(),
            &conditions(9_000_000_000),
            at(10),
            Overrides::default(),
            Policy::default(),
        );
        let Decision::Refuse { code, .. } = d else {
            panic!("expected refusal, got {d:?}")
        };
        assert_eq!(code, marrow_core::Code::ModSuspended);
    }

    #[test]
    fn one_bad_model_does_not_suspend_a_good_one() {
        // §142.4: failures are counted per model, not globally.
        let mut s = sup();
        for i in 0..8 {
            s.record_failure(MODEL, at(i), "oom");
        }
        assert!(matches!(
            s.state_of(MODEL),
            Some(ModelState::Suspended { .. })
        ));
        assert_eq!(
            s.state_of("granite-4-3b-mlx-q4"),
            Some(&ModelState::Installed)
        );
    }

    #[test]
    fn the_user_can_reset_a_breaker_and_is_told_what_failed_first() {
        let mut s = sup();
        s.record_failure(MODEL, at(0), "weights sha mismatch");
        for i in 1..4 {
            s.record_failure(MODEL, at(i), "worker exited 137");
        }
        let first = s
            .reset_breaker(MODEL)
            .expect("must report the first failure");
        assert_eq!(first, "weights sha mismatch");
        assert_eq!(s.state_of(MODEL), Some(&ModelState::Installed));
    }

    #[test]
    fn a_refusal_reaches_subscribers_as_an_event_not_only_as_a_return_value() {
        // The CLI gets the return value; the UI gets the event. Both must see
        // the same sentence.
        let mut s = sup();
        let d = s.submit(
            req(),
            Cancel::new(),
            &conditions(1_000_000),
            at(0),
            Overrides::default(),
            Policy::default(),
        );
        let Decision::Refuse { reason, .. } = &d else {
            panic!("expected refusal")
        };
        let event_reason = s
            .take_events()
            .into_iter()
            .find_map(|e| match e {
                Event::Refused { reason, .. } => Some(reason),
                _ => None,
            })
            .expect("must emit a Refused event");
        assert_eq!(&event_reason, reason);
    }

    #[test]
    fn a_deferred_request_is_queued_and_reports_the_depth() {
        // SUP-008: "there are N ahead of you" is an answer; "queued" is not.
        let mut s = sup();
        let mut busy = conditions(9_000_000_000);
        busy.sustained_load = 1.5;
        let r = Request::new(MODEL, Priority::Enrichment, Duration::from_secs(600));
        let d = s.submit(
            r,
            Cancel::new(),
            &busy,
            at(0),
            Overrides::default(),
            Policy::default(),
        );
        assert!(matches!(d, Decision::Defer { .. }), "{d:?}");
        assert_eq!(
            s.depth_of(MODEL).unwrap().total,
            1,
            "deferred means queued, not dropped"
        );
        assert!(s
            .take_events()
            .iter()
            .any(|e| matches!(e, Event::Deferred { depth, .. } if depth.total == 1)));
    }

    #[test]
    fn a_full_queue_refuses_rather_than_growing() {
        let mut s = sup();
        let c = conditions(9_000_000_000);
        for _ in 0..QUEUE_CAPACITY {
            let d = s.submit(
                req(),
                Cancel::new(),
                &c,
                at(0),
                Overrides::default(),
                Policy::default(),
            );
            assert!(d.admitted(), "{d:?}");
        }
        let d = s.submit(
            req(),
            Cancel::new(),
            &c,
            at(0),
            Overrides::default(),
            Policy::default(),
        );
        let Decision::Refuse { code, .. } = d else {
            panic!("expected refusal, got {d:?}")
        };
        assert_eq!(code, marrow_core::Code::ModQueueFull);
        assert_eq!(s.depth_of(MODEL).unwrap().total, QUEUE_CAPACITY);
    }

    #[test]
    fn an_unknown_model_is_named_in_the_refusal() {
        let mut s = sup();
        let r = Request::new(
            "no-such-model",
            Priority::Interactive,
            Duration::from_secs(60),
        );
        let d = s.submit(
            r,
            Cancel::new(),
            &conditions(9_000_000_000),
            at(0),
            Overrides::default(),
            Policy::default(),
        );
        let Decision::Refuse { reason, .. } = d else {
            panic!("expected refusal")
        };
        assert!(reason.contains("no-such-model"), "{reason}");
    }

    #[test]
    fn the_idle_timeout_is_clamped_to_the_documented_range() {
        // LLM-048: shorter thrashes the load path, longer holds the budget for
        // a session that has ended.
        assert_eq!(
            sup()
                .with_idle_timeout(Duration::from_secs(1))
                .idle_timeout(),
            IDLE_TIMEOUT_RANGE.0
        );
        assert_eq!(
            sup()
                .with_idle_timeout(Duration::from_secs(3600))
                .idle_timeout(),
            IDLE_TIMEOUT_RANGE.1
        );
        assert_eq!(
            sup()
                .with_idle_timeout(Duration::from_secs(240))
                .idle_timeout(),
            Duration::from_secs(240)
        );
    }

    #[test]
    fn a_success_clears_a_suspension() {
        let mut s = sup();
        for i in 0..3 {
            s.record_failure(MODEL, at(i), "oom");
        }
        s.record_success(MODEL, at(100));
        assert_eq!(s.state_of(MODEL), Some(&ModelState::Ready));
        let d = s.submit(
            req(),
            Cancel::new(),
            &conditions(9_000_000_000),
            at(101),
            Overrides::default(),
            Policy::default(),
        );
        assert!(d.admitted(), "{d:?}");
    }

    #[test]
    fn the_thread_starts_and_stops_cleanly() {
        // The loop must not need a request to notice a shutdown.
        let (ctx, crx) = std::sync::mpsc::channel();
        let (etx, erx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            run(sup(), Sampler::new(10, Duration::from_millis(20)), crx, etx)
        });
        // Give it long enough to tick at least once.
        std::thread::sleep(Duration::from_millis(80));
        ctx.send(Command::Shutdown).unwrap();
        handle.join().expect("supervisor thread must not panic");
        assert!(
            erx.try_iter().any(|e| matches!(e, Event::Pressure { .. })),
            "the sampler must have ticked and reported"
        );
    }

    #[test]
    fn dropping_the_command_sender_stops_the_thread() {
        // Otherwise a crashed caller leaves a thread sampling forever.
        let (ctx, crx) = std::sync::mpsc::channel::<Command>();
        let (etx, _erx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            run(sup(), Sampler::new(10, Duration::from_millis(20)), crx, etx)
        });
        drop(ctx);
        handle.join().expect("supervisor thread must not panic");
    }
}
