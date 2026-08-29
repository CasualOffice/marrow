//! What the user gets by default, and what they can change it to.
//!
//! Two separate ideas that are easy to conflate:
//!
//! - **[`Profile`]** — a preference, chosen by the user, about memory and
//!   battery versus answer quality.
//! - **[`Workload`]** — what is being asked for. Ingest-time classification and
//!   an interactive answer want different-sized models *at the same profile*.
//!
//! The second one is the reason this module exists. A single global "AI
//! performance" dial that picks one model for everything is the design that
//! makes a 4B run 35,000 times during ingest.

use serde::{Deserialize, Serialize};

use crate::probe::{Machine, Tier};

/// The user-facing choice.
///
/// Labelled by what it *costs*, not by "performance" — the honest axis is
/// memory and battery against answer quality, and a bigger local model is not
/// uniformly better at the things Marrow asks for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    /// Lowest memory and battery. ~2B locally.
    Efficient,
    /// Recommended. ~4B locally.
    #[default]
    Balanced,
    /// More memory for the generative model. 8B and up, where it fits.
    LargerLocal,
    /// A frontier model over the network, with everything §140 requires.
    Cloud,
}

impl Profile {
    pub fn label(self) -> &'static str {
        match self {
            Profile::Efficient => "Efficient",
            Profile::Balanced => "Balanced",
            Profile::LargerLocal => "Larger local model",
            Profile::Cloud => "Cloud",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Profile::Efficient => "Lowest memory and battery use. About 2B, local.",
            Profile::Balanced => "About 4B, local. Recommended.",
            Profile::LargerLocal => "8B and above where it fits. More memory, slower to answer.",
            Profile::Cloud => "A frontier model over the network. Content leaves this device.",
        }
    }
}

/// What the model is being asked to do.
///
/// The tier split is by workload, not by a global dial. The reasoning:
///
/// - **Routing and extraction run thousands of times** (once per file during
///   ingest, once per question during ask) and never need to *generate* — a
///   classification and a JSON object are the whole output. A 0.5–2B model does
///   that well and costs 500–900 MB.
/// - **Answering runs once per question** and is the entire visible quality of
///   the product. That is where the 4B goes.
///
/// The trap in the obvious version of this design: routing an *ask* through a
/// tiny model saves nothing when the 4B is about to load anyway to write the
/// answer. You pay both footprints and both load paths to avoid one short
/// prefill. So the tiny model is used for the ask router **only when no
/// generation follows** — "find the PDF about X" resolves to a search and never
/// needs the 4B at all. That case is common enough to be worth it and
/// conditional enough not to be architecture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Workload {
    /// Intent classification, query rewriting, NER, file classification, tool
    /// routing. High volume, structured output, no prose.
    Routing,
    /// Writing an answer a person will read.
    Generation,
    /// Embedding for search. Always resident; it is the product.
    Embedding,
}

/// The parameter class chosen for a (profile, workload) pair, in billions.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Choice {
    pub params_b: f64,
    /// Whether this model stays loaded between requests.
    pub resident: bool,
    pub why: &'static str,
}

/// The default profile for a machine.
///
/// | Machine | Default |
/// |---|---|
/// | 8 GB | Efficient — 2B |
/// | 16 GB | Balanced — 4B |
/// | 24 GB | Balanced — 4B |
/// | 32 GB+ | Balanced — 4B, larger available |
///
/// A 32 GB machine still defaults to 4B. Quality per gigabyte flattens above
/// 4B for routing, extraction and grounded answering — the three things this
/// product actually does — while latency and battery draw do not. The default
/// should be the setting that stays out of the way; the larger model is one
/// click away for the person who wants it.
pub fn default_profile(machine: &Machine) -> Profile {
    match machine.tier {
        Tier::Minimal | Tier::Low => Profile::Efficient,
        Tier::Mid | Tier::High | Tier::Max => Profile::Balanced,
    }
}

/// Which model class to use.
pub fn choose(profile: Profile, workload: Workload) -> Choice {
    match (profile, workload) {
        (_, Workload::Embedding) => Choice {
            params_b: 0.1,
            resident: true,
            why: "Search is the product; the embedder does not go cold because generation did.",
        },
        // Routing is the same job at every profile. Spending a 4B on intent
        // classification buys nothing a 1.5B does not already get right.
        (Profile::Efficient, Workload::Routing) => Choice {
            params_b: 0.5,
            resident: true,
            why: "Classification and structured extraction only. Loaded during ingest.",
        },
        (_, Workload::Routing) => Choice {
            params_b: 1.5,
            resident: true,
            why: "Classification, query rewrite and NER. Runs once per file, so it must be cheap.",
        },
        (Profile::Efficient, Workload::Generation) => Choice {
            params_b: 2.0,
            resident: false,
            why: "Smallest model that writes a usable grounded answer.",
        },
        (Profile::Balanced, Workload::Generation) => Choice {
            params_b: 4.0,
            resident: false,
            why: "The quality knee for grounded answering. Loaded on demand, unloaded when idle.",
        },
        (Profile::LargerLocal, Workload::Generation) => Choice {
            params_b: 8.0,
            resident: false,
            why: "More capable on multi-step reasoning; noticeably slower to first token.",
        },
        (Profile::Cloud, Workload::Generation) => Choice {
            params_b: 0.0,
            resident: false,
            why: "Runs on the provider's hardware. Nothing is loaded here.",
        },
    }
}

/// Whether a profile is offerable on this machine, and why not if it is not.
pub fn offerable(machine: &Machine, profile: Profile) -> Result<(), String> {
    let needed = choose(profile, Workload::Generation).params_b;
    if needed == 0.0 {
        return Ok(()); // Cloud needs no local memory.
    }
    let shape = crate::sizing::ModelShape::new(needed, crate::sizing::Quantization::Q4, 8192);
    let req = crate::sizing::Requirement::estimate(machine, &shape);
    if req.total() <= machine.total_memory_bytes {
        Ok(())
    } else {
        Err(format!(
            "{} needs about {:.1} GB and this machine has {:.1} GB.",
            profile.label(),
            req.total() as f64 / 1e9,
            machine.total_memory_bytes as f64 / 1e9
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mac(gb: f64) -> Machine {
        let bytes = (gb * 1_073_741_824.0) as u64;
        let mut m = Machine {
            total_memory_bytes: bytes,
            cpu_cores: 8,
            unified_memory: true,
            ..Machine::unknown()
        };
        m.tier = crate::probe::Tier::for_test(bytes, true);
        m
    }

    #[test]
    fn an_eight_gigabyte_mac_defaults_to_efficient_and_sixteen_to_balanced() {
        assert_eq!(default_profile(&mac(8.0)), Profile::Efficient);
        assert_eq!(default_profile(&mac(16.0)), Profile::Balanced);
        assert_eq!(default_profile(&mac(24.0)), Profile::Balanced);
    }

    #[test]
    fn a_thirty_two_gigabyte_machine_still_defaults_to_four_b() {
        // Deliberate. Quality per gigabyte flattens above 4B for routing,
        // extraction and grounded answering; latency and battery do not. The
        // larger model is offered, not imposed.
        assert_eq!(default_profile(&mac(32.0)), Profile::Balanced);
        assert_eq!(
            choose(Profile::Balanced, Workload::Generation).params_b,
            4.0
        );
        assert!(
            offerable(&mac(32.0), Profile::LargerLocal).is_ok(),
            "8B must be available"
        );
    }

    #[test]
    fn a_smaller_machine_is_told_why_a_profile_is_not_available() {
        // Never a greyed-out row with no explanation (LLM-016).
        let why = offerable(&mac(8.0), Profile::LargerLocal).unwrap_err();
        assert!(why.contains("GB"), "{why}");
        assert!(why.contains("Larger local"), "{why}");
    }

    #[test]
    fn routing_does_not_scale_with_the_profile_past_the_efficient_step() {
        // Spending a 4B on intent classification buys nothing. The dial moves
        // the *generator*, not the router.
        let balanced = choose(Profile::Balanced, Workload::Routing).params_b;
        let larger = choose(Profile::LargerLocal, Workload::Routing).params_b;
        assert_eq!(balanced, larger, "the dial must not inflate the router");
        assert!(balanced < choose(Profile::Balanced, Workload::Generation).params_b);
    }

    #[test]
    fn the_router_and_the_embedder_are_resident_and_the_generator_is_not() {
        // The whole lifecycle: what runs thousands of times stays; what runs
        // once per question loads on demand and unloads when idle (LLM-047/49).
        assert!(choose(Profile::Balanced, Workload::Routing).resident);
        assert!(choose(Profile::Balanced, Workload::Embedding).resident);
        assert!(!choose(Profile::Balanced, Workload::Generation).resident);
    }

    #[test]
    fn cloud_loads_nothing_locally() {
        let c = choose(Profile::Cloud, Workload::Generation);
        assert_eq!(c.params_b, 0.0);
        assert!(!c.resident);
        // And it is offerable on any machine, because it needs no local memory.
        assert!(offerable(&mac(4.0), Profile::Cloud).is_ok());
    }

    #[test]
    fn the_resident_floor_is_small_enough_to_leave_loaded() {
        // Router + embedder are always in memory. If that pair is not under
        // ~1.2 GB the whole tiered design costs more than it saves.
        let router = choose(Profile::Balanced, Workload::Routing).params_b;
        let embed = choose(Profile::Balanced, Workload::Embedding).params_b;
        let m = mac(16.0);
        let bytes = |p: f64| {
            crate::sizing::Requirement::estimate(
                &m,
                &crate::sizing::ModelShape::new(p, crate::sizing::Quantization::Q4, 4096),
            )
            .weights_bytes
        };
        let floor = bytes(router) + bytes(embed);
        assert!(
            floor < 1_200_000_000,
            "resident floor {floor} exceeds the budget the tiering is meant to protect"
        );
    }

    #[test]
    fn every_profile_explains_itself_in_the_ui() {
        for p in [
            Profile::Efficient,
            Profile::Balanced,
            Profile::LargerLocal,
            Profile::Cloud,
        ] {
            assert!(!p.label().is_empty());
            assert!(p.detail().len() > 20, "{:?}: {}", p, p.detail());
        }
        // Cloud must say what it costs in privacy, not only in quality.
        assert!(Profile::Cloud.detail().contains("leaves this device"));
    }

    #[test]
    fn balanced_is_the_default_when_nothing_is_known() {
        // But the *machine* default falls back to Efficient on an unknown
        // machine, because `Machine::unknown()` is Minimal (HW-010).
        assert_eq!(Profile::default(), Profile::Balanced);
        assert_eq!(default_profile(&Machine::unknown()), Profile::Efficient);
    }
}
