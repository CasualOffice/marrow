//! Marrow MCP — the index, exposed to whatever agent front-end you already use.
//!
//! An adapter, in the [LLD §1] sense: it speaks a wire protocol and calls
//! `marrow-query`. It holds no state beyond an open store, and it computes
//! nothing the CLI cannot also reach — the moment it does, the boundary is in
//! the wrong place ([UX §1]).
//!
//! [LLD §1]: ../../../docs/LLD.md
//! [UX §1]: ../../../docs/UX.md

#![forbid(unsafe_code)]

pub mod protocol;
pub mod server;
pub mod tools;

pub use protocol::{PROTOCOL_VERSION, SERVER_NAME, SERVER_VERSION};
pub use server::{serve, Server};
