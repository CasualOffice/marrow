//! Marrow ingest — the write path.
//!
//! Orchestration, the mirror of `marrow-query`'s read path: it joins `scan`
//! (what is on disk) to `store` (what we know) and owns nothing itself.
//!
//! # Shape
//!
//! A staged pipeline with bounded channels ([LLD §2.6]). The stages have very
//! different costs — M0 measured the walk at 97k files/s and hashing at 4.2k —
//! so the bounded channels let the slowest stage set the pace while memory
//! stays flat. Nothing ever buffers the corpus.
//!
//! ```text
//! walk ─▶ [1024] ─▶ probe+tier ─▶ [512] ─▶ hash ─▶ [256] ─▶ writer
//!  1 thread            (inline)            N threads        1 actor
//! ```
//!
//! Probe and tier are folded into the walk because `marrow-scan` already
//! produces `FileFacts` from the same `lstat` the walker performs. Making them
//! a separate stage would mean a second stat per file for no gain.
//!
//! # What it does not do
//!
//! No parsing, no chunking, no indexing. Those stages attach behind the hash
//! stage once `marrow-parse` and `marrow-index` land; the channel between them
//! is the seam.
//!
//! [LLD §2.6]: ../../../docs/LLD.md

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

mod content;
mod pipeline;
mod progress;

pub use content::{documents_for, read_for_parsing, ContentInput, Extracted};
pub use pipeline::{apply_hints, ingest_root, ingest_root_with_index, IngestOutcome, IngestPolicy};
pub use progress::{Cancel, Progress, Stage};
