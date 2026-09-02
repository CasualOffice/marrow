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

// Not `forbid`: `worker` reads a child process's resident size through
// `proc_pid_rusage`, because macOS has no portable way to ask that question
// safely. The unsafe is confined to one function and every call is a
// documented read with no lifetime to get wrong.
#![deny(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod admission;
pub mod backfill;
pub mod breaker;
pub mod catalogue;
pub mod detect;
pub mod download;
pub mod embed;
pub mod envelope;
pub mod injection;
pub mod kv;
pub mod openai;
pub mod provider;
pub mod queue;
pub mod registry;
pub mod request;
pub mod runtime;
pub mod scratch;
pub mod secrets;
pub mod supervisor;
pub mod worker;

pub use admission::{admit, Classification, Decision, Overrides, Policy};
pub use breaker::{Breaker, BreakerState};
pub use detect::{Detected, Scan};
pub use download::{download, Https, Progress, Stage};
pub use embed::Embedder;
pub use envelope::{Envelope, Evidence, Fact, Role, Session, Trust, Turn};
pub use kv::{PrefixCache, PrefixKey, Scope};
pub use openai::{Endpoint, OpenAiProvider};
pub use provider::{
    Boundary, Completion, Finish, GenerationProvider, Notice, StopReason, StreamEvent, Usage,
};
pub use queue::{Cancel, Depth, Queue};
pub use registry::{Artifact, Capabilities, Entry, Format, Licence, Registry, Source};
pub use request::{Priority, Reasoning, Request};
pub use runtime::{install as install_runtime, Archive, Install};
pub use scratch::{ModelWorkspace, Scratch};
pub use secrets::{Keyring, MemorySecrets, Secret, SecretStore};
pub use supervisor::{Command, Event, LoadStage, ModelState, Supervisor};
pub use worker::{MlxProvider, Runtime, Worker};
