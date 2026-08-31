//! Marrow tools — the creation operations, and the one guarded path they all
//! write through.
//!
//! ```text
//!   create_file ─┐
//!   create_diagram ├─→ Workspace::write ─→ name rules → containment →
//!   create_page ─┘        protected dirs → collision → temp file →
//!                         re-verify → stale check → rename → Origin::SELF
//! ```
//!
//! Three things decide the shape of this crate.
//!
//! **One write path.** Three tools, one `rename`. A second write, however
//! small, is a second place for every rule in [`guard`] to be missing — and
//! nothing in the [adversarial corpus](corpus) would cover it.
//!
//! **The checks are re-run at operation time.** Canonicalising a path proves
//! something about the filesystem as it was. Between that proof and the write
//! there is a `git checkout`, a sync client, a second terminal. The
//! symlink-escape rule says *at operation time*; [`guard`] asks the containment and staleness
//! questions again with nothing between them and the `rename`.
//!
//! **Everything written is [`marrow_core::Origin::SelfWritten`].** Not a
//! parameter, not a default — there is no constructor for [`Written`] that says
//! anything else. The `origin = SELF` rule: a summary the agent wrote,
//! re-indexed and cited
//! back, is the system corroborating itself.
//!
//! # Caller's remaining obligation
//!
//! [`Written::origin`] is returned, not persisted. Whatever indexes the file
//! afterwards must record `files.origin = 'SELF'`; until it does, the next scan
//! will class the file as the user's own work and the citation rule has a hole
//! in it that this crate cannot close from here.
//!
//! # Platform
//!
//! macOS, like `marrow-scan` (D5). The placeholder check that guards the
//! stale-read is `#[cfg(target_os = "macos")]` there and fails closed
//! elsewhere, which on another platform would refuse every replacement.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod corpus;
pub mod create;
pub mod guard;
mod name;

pub use create::{create_diagram, create_file, create_page, CreateDiagram, CreateFile, CreatePage};
pub use guard::{Expect, Workspace, Written};
