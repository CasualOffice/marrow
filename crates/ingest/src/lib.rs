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
//! # Optional stages fail open, and say so in the outcome
//!
//! The same convention `marrow-query` states at its crate root, in the shape
//! ingest needs: **one file must never end a run.** Recording a file is
//! required and its failure is counted against that file; parsing it is
//! optional, because a file with no content is still discoverable by name and
//! date (T5, PAR-013), which is what FS-011 promises.
//!
//! Failing open here means *counted*, not *ignored*. Every degraded stage lands
//! in [`IngestOutcome::failures`] with its error code, so `marrow index` can
//! report "34,102 files, 11 could not be parsed" — a swallowed error is
//! indistinguishable from a file that had nothing in it, and the two call for
//! completely different actions.
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
