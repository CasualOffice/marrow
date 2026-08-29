//! May this run *now*? (Part 8 §142.3)
//!
//! Evaluated per request against the **current** sample, never the startup
//! probe. A recommendation made at launch is wrong by the time it is acted on:
//! the user opened a browser, a build started, the laptop came off charge.
//!
//! Two rules shape every branch here:
//!
//! 1. **A refusal names the number that caused it.** "Needs 4.4 GB, 1.2 GB
//!    available" — never "insufficient resources".
//! 2. **Resource refusals are overridable; policy refusals are not.**
//!    Conflating the two teaches people to ignore both.

use marrow_core::{Code, Timestamp};
use marrow_hw::{Conditions, Machine, ModelShape, Requirement};
use serde::Serialize;

use crate::breaker::BreakerState;
use crate::registry::Entry;
use crate::request::{Priority, Request};

/// Below this, a request is deferred rather than run — it would compete with
/// whatever is already saturating the machine rather than finishing sooner.
const LOAD_CEILING: f32 = 0.85;

/// Battery level under which local inference is refused without an override.
const BATTERY_FLOOR: f32 = 0.20;

/// How stale a sample may be before admission refuses to decide on it.
const SAMPLE_TOLERANCE_MS: i64 = 15_000;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "decision")]
pub enum Decision {
    Admit,
    /// The machine is busy but not short. Queue it rather than compete.
    Defer {
        reason: String,
    },
    Refuse {
        code: Code,
        reason: String,
        /// Whether the user may override. Resources yes; policy no.
        overridable: bool,
    },
}

impl Decision {
    pub fn admitted(&self) -> bool {
        matches!(self, Decision::Admit)
    }

    fn refuse(code: Code, reason: String) -> Self {
        Decision::Refuse {
            code,
            reason,
            overridable: true,
        }
    }

    fn policy(code: Code, reason: String) -> Self {
        Decision::Refuse {
            code,
            reason,
            overridable: false,
        }
    }
}

/// What the caller has said may be ignored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Overrides {
    pub ignore_memory: bool,
    pub ignore_battery: bool,
    pub ignore_thermal: bool,
}

/// Policy that admission must honour and the user cannot override.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Policy {
    /// The workspace's classification forbids this provider (MOD-004).
    pub provider_forbidden: bool,
}

/// Everything admission reads. Grouped so the signature does not grow a
/// parameter every time a new condition matters.
#[derive(Clone, Copy, Debug)]
pub struct Context<'a> {
    pub machine: &'a Machine,
    pub conditions: &'a Conditions,
    pub now: Timestamp,
    pub overrides: Overrides,
    pub policy: Policy,
}

/// Decide.
///
/// Order is deliberate: **policy first**, because a policy refusal must not be
/// pre-empted by a resource refusal the user could override their way past.
pub fn admit(entry: &Entry, request: &Request, shape: &ModelShape, cx: Context<'_>) -> Decision {
    // 1. Policy. Not overridable, and evaluated before anything the user could
    //    make go away by closing a browser.
    if cx.policy.provider_forbidden {
        return Decision::policy(
            Code::PolClassificationBlocked,
            "This workspace's classification does not allow that provider. \
             Choose a local model, or change the workspace's classification."
                .into(),
        );
    }

    // 2. Is the model even here.
    if !entry.installed {
        return Decision::Refuse {
            code: Code::ModNotInstalled,
            reason: format!("{} is not installed.", entry.display_name),
            overridable: false,
        };
    }

    // 3. Can the model do what was asked (GEN-013).
    if request.reasoning.is_on() && !entry.capabilities.reasoning {
        return Decision::Refuse {
            code: Code::ModUnsupportedCapability,
            reason: entry
                .reasoning_unavailable_because()
                .unwrap_or_else(|| "This model answers directly.".into()),
            overridable: false,
        };
    }

    // 4. The breaker. Before the resource checks, because a suspended model
    //    would fail anyway and the cooldown is the more useful thing to say.
    match entry.breaker.state(cx.now) {
        BreakerState::Closed => {}
        _ => {
            return Decision::Refuse {
                code: Code::ModSuspended,
                reason: entry
                    .breaker
                    .explain(cx.now)
                    .unwrap_or_else(|| "Suspended.".into()),
                // Waiting is what clears it, not a flag.
                overridable: false,
            };
        }
    }

    // 5. Has the request already outlived its asker (SUP-006).
    if request.expired(cx.now) {
        return Decision::Refuse {
            code: Code::ModDeadlineExpired,
            reason: "The request expired before it could run.".into(),
            overridable: false,
        };
    }

    // 6. Is the sampler alive. Deciding on a frozen reading is worse than
    //    admitting we do not know (HW-015).
    let sample_age = cx.now.as_millis() - cx.conditions.latest.taken_at_ms as i64;
    if cx.conditions.stale || sample_age > SAMPLE_TOLERANCE_MS {
        return Decision::refuse(
            Code::ModInsufficientMemory,
            "Cannot tell how much memory is free — the hardware sampler has \
             stopped reporting. Restart Marrow, or override to run anyway."
                .into(),
        );
    }

    // 7. Memory, against the trough of the window rather than the last peak.
    let req = Requirement::estimate(cx.machine, shape);
    let need = req.total();
    let have = cx.conditions.min_available_bytes;
    if need > have && !cx.overrides.ignore_memory {
        return Decision::refuse(
            Code::ModInsufficientMemory,
            format!(
                "Needs {}, {} available. Closing other applications would make room.",
                gb(need),
                gb(have)
            ),
        );
    }

    // 8. Thermal. `Unknown` does not refuse — that would disable local models
    //    on every platform that does not report it.
    if cx.conditions.latest.thermal.blocks_work() && !cx.overrides.ignore_thermal {
        return Decision::refuse(
            Code::ModThermalThrottled,
            format!(
                "The machine is thermally throttled ({:?}). Running now would be \
                 slow and would make it worse.",
                cx.conditions.latest.thermal
            ),
        );
    }

    // 9. Battery.
    if let Some(level) = cx.conditions.latest.battery_level {
        if cx.conditions.latest.on_battery && level < BATTERY_FLOOR && !cx.overrides.ignore_battery
        {
            return Decision::refuse(
                Code::ModOnBattery,
                format!(
                    "On battery at {:.0}%. Local inference would drain it quickly.",
                    level * 100.0
                ),
            );
        }
    }

    // 10. Load. Not a refusal — the memory is there, the CPU is busy. Queue it
    //     rather than compete, unless someone is watching a skeleton.
    if cx.conditions.sustained_load > LOAD_CEILING && request.priority != Priority::Interactive {
        return Decision::Defer {
            reason: format!(
                "The machine is busy (load {:.2}). Queued rather than competing.",
                cx.conditions.sustained_load
            ),
        };
    }

    Decision::Admit
}

fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue;
    use marrow_hw::{KvPrecision, Sample, Thermal};
    use std::time::Duration;

    fn machine() -> Machine {
        Machine {
            total_memory_bytes: 17_179_869_184,
            cpu_cores: 10,
            unified_memory: true,
            ..Machine::unknown()
        }
    }

    fn installed() -> Entry {
        let mut e = catalogue::builtin().into_iter().next().unwrap();
        e.installed = true;
        e
    }

    fn conditions(available: u64) -> Conditions {
        Conditions {
            latest: Sample {
                available_memory_bytes: available,
                cpu_load: 0.3,
                thermal: Thermal::Unknown,
                on_battery: false,
                battery_level: None,
                taken_at_ms: 1_000,
            },
            min_available_bytes: available,
            sustained_load: 0.3,
            stale: false,
        }
    }

    fn cx<'a>(m: &'a Machine, c: &'a Conditions) -> Context<'a> {
        Context {
            machine: m,
            conditions: c,
            now: Timestamp::from_millis(2_000),
            overrides: Overrides::default(),
            policy: Policy::default(),
        }
    }

    fn req() -> Request {
        Request::new(
            "qwen3.5-4b-mlx-q4",
            Priority::Interactive,
            Duration::from_secs(60),
        )
    }

    #[test]
    fn a_fitting_model_on_an_idle_machine_is_admitted() {
        let (m, c, e) = (machine(), conditions(9_000_000_000), installed());
        let d = admit(&e, &req(), &e.shape(8192, KvPrecision::F16), cx(&m, &c));
        assert_eq!(d, Decision::Admit, "{d:?}");
    }

    #[test]
    fn a_memory_refusal_names_both_numbers() {
        // The rule the whole module exists for. "Insufficient resources" tells
        // the user nothing they can act on.
        let (m, c, e) = (machine(), conditions(1_000_000_000), installed());
        let d = admit(&e, &req(), &e.shape(8192, KvPrecision::F16), cx(&m, &c));
        let Decision::Refuse {
            code,
            reason,
            overridable,
        } = d
        else {
            panic!("expected refusal, got {d:?}");
        };
        assert_eq!(code, Code::ModInsufficientMemory);
        assert!(reason.contains("Needs"), "{reason}");
        assert!(reason.contains("available"), "{reason}");
        assert!(reason.matches("GB").count() >= 2, "both numbers: {reason}");
        assert!(overridable, "a resource refusal must be overridable");
    }

    #[test]
    fn a_policy_refusal_is_not_overridable_and_wins_over_a_resource_one() {
        // If a resource refusal came first, the user could override their way
        // past a classification boundary by closing a browser.
        let (m, c, e) = (machine(), conditions(1), installed());
        let mut context = cx(&m, &c);
        context.policy.provider_forbidden = true;
        context.overrides.ignore_memory = true;
        let d = admit(&e, &req(), &e.shape(8192, KvPrecision::F16), context);
        let Decision::Refuse {
            code, overridable, ..
        } = d
        else {
            panic!("expected refusal, got {d:?}");
        };
        assert_eq!(code, Code::PolClassificationBlocked);
        assert!(!overridable, "policy is not a resource");
    }

    #[test]
    fn the_memory_override_works_but_the_policy_one_does_not_exist() {
        let (m, c, e) = (machine(), conditions(1_000_000_000), installed());
        let mut context = cx(&m, &c);
        context.overrides.ignore_memory = true;
        assert!(admit(&e, &req(), &e.shape(8192, KvPrecision::F16), context).admitted());
    }

    #[test]
    fn admission_reads_the_trough_not_the_latest_sample() {
        // A model admitted on a peak OOMs in the trough. `min_available_bytes`
        // is the number that must decide.
        let m = machine();
        let mut c = conditions(9_000_000_000);
        c.min_available_bytes = 1_000_000_000;
        let e = installed();
        let d = admit(&e, &req(), &e.shape(8192, KvPrecision::F16), cx(&m, &c));
        assert!(!d.admitted(), "the trough must decide: {d:?}");
    }

    #[test]
    fn a_suspended_model_is_refused_with_its_cooldown() {
        // SUP-002: never silent.
        let (m, c) = (machine(), conditions(9_000_000_000));
        let mut e = installed();
        for i in 0..3 {
            e.breaker
                .record_failure(Timestamp::from_millis(i), "worker exited 137");
        }
        let d = admit(&e, &req(), &e.shape(8192, KvPrecision::F16), cx(&m, &c));
        let Decision::Refuse { code, reason, .. } = d else {
            panic!("expected refusal")
        };
        assert_eq!(code, Code::ModSuspended);
        assert!(reason.contains("retrying in"), "{reason}");
        assert!(
            reason.contains("worker exited 137"),
            "must name the first failure: {reason}"
        );
    }

    #[test]
    fn thorough_on_a_model_that_cannot_think_is_refused_rather_than_ignored() {
        // GEN-013. Silently dropping the flag makes Thorough a lie.
        let (m, c) = (machine(), conditions(9_000_000_000));
        let mut e = installed();
        e.capabilities.reasoning = false;
        let r = req().with_reasoning(crate::request::Reasoning::THOROUGH);
        let d = admit(&e, &r, &e.shape(8192, KvPrecision::F16), cx(&m, &c));
        let Decision::Refuse { code, reason, .. } = d else {
            panic!("expected refusal, got {d:?}")
        };
        assert_eq!(code, Code::ModUnsupportedCapability);
        assert!(reason.contains(&e.display_name), "{reason}");
    }

    #[test]
    fn a_busy_machine_defers_background_work_but_not_an_interactive_request() {
        // SUP-005 in the admission layer: a user waiting always wins.
        let m = machine();
        let mut c = conditions(9_000_000_000);
        c.sustained_load = 1.4;
        let e = installed();
        let shape = e.shape(8192, KvPrecision::F16);

        let bg = Request::new("m", Priority::Enrichment, Duration::from_secs(60));
        assert!(matches!(
            admit(&e, &bg, &shape, cx(&m, &c)),
            Decision::Defer { .. }
        ));

        let interactive = req();
        assert!(admit(&e, &interactive, &shape, cx(&m, &c)).admitted());
    }

    #[test]
    fn a_frozen_sampler_refuses_rather_than_deciding_on_a_stale_reading() {
        // HW-015. Trusting a stopped sampler is worse than having none.
        let (m, e) = (machine(), installed());
        let mut c = conditions(9_000_000_000);
        c.stale = true;
        let d = admit(&e, &req(), &e.shape(8192, KvPrecision::F16), cx(&m, &c));
        let Decision::Refuse {
            reason,
            overridable,
            ..
        } = d
        else {
            panic!("expected refusal")
        };
        assert!(reason.contains("sampler"), "{reason}");
        assert!(overridable, "the user may still choose to run it");
    }

    #[test]
    fn thermal_unknown_does_not_block_but_serious_does() {
        let (m, e) = (machine(), installed());
        let shape = e.shape(8192, KvPrecision::F16);

        let c = conditions(9_000_000_000);
        assert_eq!(c.latest.thermal, Thermal::Unknown);
        assert!(
            admit(&e, &req(), &shape, cx(&m, &c)).admitted(),
            "Unknown must not refuse"
        );

        let mut hot = conditions(9_000_000_000);
        hot.latest.thermal = Thermal::Serious;
        let d = admit(&e, &req(), &shape, cx(&m, &hot));
        assert!(
            matches!(
                d,
                Decision::Refuse {
                    code: Code::ModThermalThrottled,
                    ..
                }
            ),
            "{d:?}"
        );
    }

    #[test]
    fn a_low_battery_refuses_and_offers_the_override() {
        let (m, e) = (machine(), installed());
        let mut c = conditions(9_000_000_000);
        c.latest.on_battery = true;
        c.latest.battery_level = Some(0.10);
        let d = admit(&e, &req(), &e.shape(8192, KvPrecision::F16), cx(&m, &c));
        let Decision::Refuse {
            code,
            reason,
            overridable,
        } = d
        else {
            panic!("expected refusal")
        };
        assert_eq!(code, Code::ModOnBattery);
        assert!(reason.contains("10%"), "must name the level: {reason}");
        assert!(overridable);
    }

    #[test]
    fn an_expired_request_is_dropped_rather_than_run() {
        // SUP-006. Running it burns memory for an answer nobody will read.
        let (m, c, e) = (machine(), conditions(9_000_000_000), installed());
        let r = Request::new("m", Priority::Interactive, Duration::from_millis(0));
        let mut context = cx(&m, &c);
        context.now = Timestamp::from_millis(r.deadline.as_millis() + 1);
        // Keep the sample fresh relative to the moved clock.
        let mut c2 = c;
        c2.latest.taken_at_ms = context.now.as_millis() as u64;
        context.conditions = &c2;
        let d = admit(&e, &r, &e.shape(8192, KvPrecision::F16), context);
        assert!(
            matches!(
                d,
                Decision::Refuse {
                    code: Code::ModDeadlineExpired,
                    ..
                }
            ),
            "{d:?}"
        );
    }

    #[test]
    fn an_uninstalled_model_is_refused_before_any_resource_check() {
        // Otherwise "insufficient memory" is reported for a model that is not
        // even on disk.
        let (m, c) = (machine(), conditions(1));
        let e = catalogue::builtin().into_iter().next().unwrap(); // not installed
        let d = admit(&e, &req(), &e.shape(8192, KvPrecision::F16), cx(&m, &c));
        assert!(
            matches!(
                d,
                Decision::Refuse {
                    code: Code::ModNotInstalled,
                    ..
                }
            ),
            "{d:?}"
        );
    }

    #[test]
    fn every_refusal_carries_a_code_and_a_sentence() {
        // No branch may return a bare code. A code without a sentence is
        // "insufficient resources" with extra steps.
        let m = machine();
        let e = installed();
        let shape = e.shape(8192, KvPrecision::F16);
        let cases: Vec<Decision> = vec![
            admit(&e, &req(), &shape, cx(&m, &conditions(1))),
            {
                let mut c = conditions(9_000_000_000);
                c.stale = true;
                admit(&e, &req(), &shape, cx(&m, &c))
            },
            {
                let mut c = conditions(9_000_000_000);
                c.latest.thermal = Thermal::Critical;
                admit(&e, &req(), &shape, cx(&m, &c))
            },
        ];
        for d in cases {
            let Decision::Refuse { reason, .. } = &d else {
                panic!("expected refusal: {d:?}")
            };
            assert!(reason.len() > 25, "not a sentence: {reason}");
            assert!(reason.ends_with('.'), "not a sentence: {reason}");
        }
    }
}
