//! Marrow egress — the one tool that can tell someone else something.
//!
//! Implements [Part 9](../../../docs/Part_9_Egress.md). Every other tool in
//! Marrow reads the user's own disk; this one puts their content on the wire,
//! so the governing constraint is the opposite of convenience:
//!
//! > **A fetch is the user's content leaving their machine. This crate's job
//! > is to make that visible and expensive, not easy.**
//!
//! Three risks, and they are not the same risk (§152):
//!
//! ```text
//!   1. EGRESS   the request is content     — a query is the user's question,
//!                                            in somebody else's logs, forever
//!   2. INGRESS  the reply is untrusted     — and it reaches a model's context
//!   3. SSRF     the destination may be     — 127.0.0.1, 169.254.169.254, the
//!               inside the perimeter         user's own LAN, from inside it
//! ```
//!
//! The crate is arranged so each risk has one place it is handled:
//!
//! | Module | Owns |
//! |---|---|
//! | [`url`] | Parsing, and keeping the facts a policy needs (credentials, port, query) rather than normalising them away |
//! | [`addr`] | Which IP addresses may be connected to. Default deny |
//! | [`policy`] | Every decision, with **no I/O at all**, so every rule is testable without a network |
//! | [`fetch`] | Enforcing it: resolve, check the **resolved address**, one hop at a time, cap everything |
//! | [`html`] | Readable text out of markup, because markup is where instructions hide |
//!
//! # What it cannot do
//!
//! Structurally, not by discipline:
//!
//! - **It cannot write anything.** No store, no index, no filesystem writer in
//!   the dependency list, so fetched content cannot enter the index or land in
//!   a workspace root and be cited back as a local file (NET-040/041/042,
//!   invariant #10).
//! - **It cannot build a URL.** There is no `search(query)` entry point, so a
//!   question cannot be percent-encoded out of the device by a convenience
//!   (NET-048/049).
//! - **It cannot follow a redirect without re-deciding.** The HTTP client is
//!   configured with zero redirects; hops are taken by [`fetch::Client`], which
//!   re-runs the whole policy on each one (NET-013, NET-062).
//! - **It cannot carry an identity.** No cookie jar exists — the `cookies`
//!   feature is not enabled — and no header is settable by a caller (NET-029/030).
//!
//! # Using it
//!
//! ```no_run
//! use marrow_net::{Client, Consent, Decision, Turn};
//!
//! let client = Client::live();
//! let mut consent = Consent::new();   // one session
//! let mut turn = Turn::new();         // one user question, eight fetches
//!
//! let url = "https://example.com/";
//! match client.decide(url, &consent, &turn) {
//!     Decision::Allow => {}
//!     Decision::Confirm { url, why } => {
//!         // Show `client.preview(&url)` — the exact bytes that would leave —
//!         // and `why`. Only a human says yes.
//!         println!("{}", why.explain());
//!         consent.confirm_host("example.com");
//!     }
//!     Decision::Refuse(r) => {
//!         // Not overridable. There is no setting.
//!         eprintln!("[{}] {}", r.code(), r.message());
//!         return Ok(());
//!     }
//! }
//!
//! let fetched = client.fetch(url, &mut consent, &mut turn)?;
//! // Into the §114 envelope, and never anywhere else.
//! let label = fetched.label();
//! assert!(label.external);
//! assert_eq!(label.trust, "UNTRUSTED_CONTENT");
//! # Ok::<(), marrow_net::Refusal>(())
//! ```

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod addr;
pub mod fetch;
pub mod html;
pub mod policy;
pub mod url;

pub use addr::{classify, AddressClass};
pub use fetch::{
    Client, Egress, Fetched, Http, Https, Labelled, Resolve, Response, SystemDns, Visited,
};
pub use html::{extract, Extracted};
pub use policy::{
    decode_query, ConfirmReason, Consent, Decision, Policy, Refusal, Turn, ACCEPT, MAX_BODY_BYTES,
    MAX_ELAPSED, MAX_FETCHES_PER_TURN, MAX_REDIRECTS, USER_AGENT,
};
pub use url::{Url, UrlError};
