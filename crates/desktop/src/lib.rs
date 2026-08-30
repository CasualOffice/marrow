//! Marrow desktop — the window's half of the product, as a library.
//!
//! Three frontends, one core ([GUI §1]). The binary beside this file is an
//! adapter: it opens the store, registers commands and shows a window.
//! Everything it would otherwise compute lives here, so the pipeline can be
//! run from a test without a window.
//!
//! [GUI §1]: ../../../docs/GUI.md

#![forbid(unsafe_code)]

pub mod ask;
pub mod commands;
pub mod models;
pub mod state;

pub use models::Hub;
pub use state::Core;
