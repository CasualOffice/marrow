//! Marrow hardware — what this machine can run, and whether it should right now.
//!
//! Two jobs with different costs and different consumers ([Part 8 §137]):
//!
//! | | [`Probe`] | [`Sampler`] |
//! |---|---|---|
//! | When | Once, and on hardware change | Every few seconds while a model is loaded |
//! | Answers | Which models are *offerable* | Whether one may run *right now* |
//! | Budget | < 2 s | **< 1 ms** — a sampler that costs measurable CPU is a bug in the thing measuring CPU |
//!
//! The split matters because a recommendation made at launch is wrong by the
//! time it is acted on: the user opened a browser, a build started, the laptop
//! came off charge. Admission decisions read the sampler, never the probe.
//!
//! [Part 8 §137]: ../../../docs/Part_8_Model_Runtime.md

// Not `forbid`: `sample` reads the kernel's own counters through libc, because
// the alternatives are a subprocess per tick (too slow for a 1 ms budget) or a
// crate that shells out for us. The unsafe is confined to that one module and
// every call there is a documented read with no lifetime to get wrong.
#![deny(unsafe_code)]
#![warn(missing_debug_implementations)]

mod probe;
mod profile;
mod sample;
mod sizing;

pub use probe::{Accelerator, Machine, Probe, Tier};
pub use profile::{choose, default_profile, offerable, Choice, Profile, Workload};
pub use sample::{Conditions, Sample, Sampler, Thermal};
pub use sizing::{
    assess, prefer, Fit, KvPrecision, ModelShape, Quantization, Requirement, RuntimeKind, Verdict,
};
