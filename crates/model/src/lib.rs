//! Marrow model runtime — which model, may it run now, and who is waiting.
//!
//! This crate owns the decisions; it owns no inference. Workers do that (Part 8
//! §139.2), and they are a separate process precisely so an OOM kills the
//! worker and never the index.
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

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod admission;
pub mod breaker;
pub mod catalogue;
pub mod kv;
pub mod queue;
pub mod registry;
pub mod request;
pub mod scratch;
pub mod supervisor;

pub use admission::{admit, Decision, Overrides, Policy};
pub use breaker::{Breaker, BreakerState};
pub use kv::{PrefixCache, PrefixKey, Scope};
pub use queue::{Cancel, Depth, Queue};
pub use registry::{Capabilities, Entry, Format, Licence, Registry, Source};
pub use request::{Priority, Reasoning, Request};
pub use scratch::{ModelWorkspace, Scratch};
pub use supervisor::{Command, Event, LoadStage, ModelState, Supervisor};
