//! Marrow scan — filesystem discovery.
//!
//! This crate answers "what is on disk, and what can safely be read". It
//! **produces values and nothing else**: no parsing, no database writes, no
//! caching. Persistence is `marrow-store`'s job; this layer must stay callable
//! from a test with a `tempfile` and no other setup.
//!
//! ```text
//! path::AuthorizedRoot   consent + containment  (symlink escape, NFC)
//!        │
//!        ├── walk::walk  →  Iterator<ScanEvent>  (FS-011, WS-005, D47)
//!        │        │
//!        │        └── probe::FileFacts  one lstat, no content   (FS-014)
//!        │                 └── tier::tier_from_metadata (never-hydrate)
//!        │
//!        └── hash::hash_file  →  ContentHash    refuses non-Resident
//! ```
//!
//! Three rules hold across every module here:
//!
//! - **A placeholder is never opened.** The tier is decided from metadata the
//!   walk already had, and [`hash`] is the only code that opens a file — behind
//!   a tier check it cannot be reached around. The never-hydrate rule.
//! - **Containment is proved by comparing canonical path *components*.** Never
//!   a string prefix, and re-proved at operation time. The symlink-escape rule.
//! - **Paths are NFC-normalised before comparison**, so the NFD form macOS
//!   stores and the NFC form everything else produces are one identity, not two.
//!   The Unicode NFC/NFD rule. None of these keys are file identities — that
//!   is `marrow_core::FileId`, per the path-is-never-identity rule.
//!
//! **Platform: macOS only** (D5). Platform-specific code is behind
//! `#[cfg(target_os = "macos")]` with a fail-closed fallback: on any other
//! target [`tier::tier_from_metadata`] returns
//! [`marrow_core::TierState::Unavailable`], so a port that forgets to implement
//! TIER-002/TIER-004 indexes nothing rather than downloading a cloud drive.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod hash;
pub mod path;
pub mod probe;
pub mod sweep_lock;
pub mod tier;
pub mod walk;
pub mod watch;

pub use hash::{hash_file, hash_file_with_tier, HASH_BUFFER_BYTES};
pub use path::{path_key, AuthorizedRoot, PathKey, SafePath};
pub use probe::{mime_hint, probe, FileFacts, FsIdentity, MimeHint};
pub use sweep_lock::{sweeps_dir, SweepLock};
pub use tier::{ensure_safe_to_read, tier_of};
pub use walk::{walk, ScanEntry, ScanEvent, WalkPolicy, DEFAULT_NOISE_DIRS};
pub use watch::{reconcile_interval, Health, Hints, Watcher};
