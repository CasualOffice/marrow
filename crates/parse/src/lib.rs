//! Marrow parse — bytes in, intermediate representation out.
//!
//! This crate answers "what is *in* this file, and exactly where". It reads no
//! files, opens no sockets and touches no database: a parser is handed bytes
//! and a [`FileProbe`], and returns a [`ParsedArtifact`] or an explanation.
//! Everything here is a pure function of its input, which is why every test in
//! the crate is a string literal rather than a tempdir.
//!
//! ```text
//! router::ParserRouter          chain of responsibility, sorted by tier
//!        │
//!        ├── code::CodeParser         T1  Tree-sitter, ~1,300 files
//!        ├── markdown::MarkdownParser T1  headings, sections, links
//!        ├── html::HtmlParser         T1  headings, prose, tables
//!        ├── structured::Structured…  T1  TOML / JSON / YAML key paths
//!        ├── csv::CsvParser           T1  table / row / cell
//!        ├── text::TextParser         T1  the catch-all, byte + line spans
//!        ├── pdf::PdfParser           T2  PDFKit, page + bbox
//!        ├── xlsx::XlsxParser         T2  calamine, sheet + cell ref
//!        ├── docx::DocxParser         T2  OOXML, XML path
//!        ├── image::ImageParser       T4  Vision OCR, page 1 + bbox
//!        │
//!        └── (nothing matched)   →   ParsedArtifact::metadata_only()   T5
//! ```
//!
//! # The invariant this crate exists to enforce
//!
//! **Every IR node carries a [`SourceSpan`](marrow_core::SourceSpan). Not an
//! `Option`.** There is no constructor, no `Default` and no builder that can
//! produce a node without one. Provenance to an exact location is the entire
//! reason this project exists rather than `ripgrep | llm`; it is nearly free to
//! record while the parser still knows where it is, and nearly impossible to
//! add afterwards. See [`ir`].
//!
//! Two consequences worth naming:
//!
//! - [`SourceSpan::Whole`](marrow_core::SourceSpan::Whole) is legal on exactly
//!   one node kind, [`IrKind::Metadata`], and
//!   [`ParsedArtifact::validate`] rejects it anywhere else. "Somewhere in this
//!   file" is not a citation.
//! - Byte spans index the **decoded** text. For UTF-8 — effectively the whole
//!   real corpus — that is the file's own bytes; for a legacy encoding it is
//!   not, and [`decode::Decoded::offsets_match_source`] says which you have.
//!
//! # PAR-014: two kinds of payload
//!
//! Structure the parser derived is [`Trust::DeterministicRuntime`]. Text lifted
//! out of the file is [`Trust::UntrustedContent`], always, including a symbol's
//! source, a link's target and a front-matter block that says `role: system`.
//! The prompt envelope (invariant #12) filters on this, so it is enforced by
//! construction rather than by convention: `IrNode`'s text-bearing constructors
//! do not take a trust argument.
//!
//! # No `async`
//!
//! LLD §4: async lives at the adapter edge and nowhere below it. Parsing is
//! CPU-bound work over bytes already in memory. There is no `async fn` in this
//! crate and there should never be one.
//!
//! # What this crate deliberately does not parse
//!
//! PPTX, ODS/ODT, audio and video. They route to the metadata-only terminal,
//! which is a correct answer rather than a gap: PAR-013 makes a file with no
//! parser discoverable.
//!
//! Four entries left this list. PDF was D4 — fourteen files in the whole home
//! directory, deferred indefinitely — and [`pdf`] reads it now. Images were
//! M0 F6 — "EXIF is the whole image story" — and that turned out to be true of
//! photographs and false of everything else: a screenshot of an error, a
//! photographed whiteboard and a diagram exported to PNG are documents stored
//! as pixels, and [`image`] reads them through the OS's own recogniser (D13).
//! XLSX and DOCX left with the Table IR: §99 makes a number in a table the
//! thing a large share of factual questions turn on, and a workbook that is
//! only findable by its filename answers none of them.

// Not `forbid`: `pdf` calls PDFKit and `image` calls Vision, both of which are
// Objective-C and therefore unsafe by definition. The alternative in each case
// is vendoring a library to do what the OS already does — a multi-megabyte
// Chromium PDF renderer, a bundled OCR engine — and losing the per-character
// bounds and per-line boxes that make an exact citation possible. The unsafe is
// confined to those two modules, each carries a module-level `#[allow]`, and
// every call is a documented message send with no lifetime to get wrong.
#![deny(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod a1;
pub mod budget;
pub mod chunk;
pub mod code;
pub mod csv;
pub mod decode;
pub mod docx;
pub mod html;
pub mod image;
pub mod ir;
pub mod markdown;
pub mod ooxml;
pub mod parser;
pub mod pdf;
pub mod router;
pub mod structured;
pub mod table;
pub mod text;
pub mod xlsx;

pub use budget::{BudgetGuard, Budgets};
pub use chunk::{chunk, Chunk, ChunkKind, ChunkPolicy, CHUNKER_VERSION};
pub use code::{CodeParser, Lang};
pub use csv::CsvParser;
pub use decode::Decoded;
pub use docx::DocxParser;
pub use html::HtmlParser;
pub use image::ImageParser;
pub use ir::{
    ArtifactBuilder, IrKind, IrNode, LineIndex, NodeAttrs, ParseOutcome, ParseWarning,
    ParsedArtifact, ParserTier, SymbolKind, Trust,
};
pub use markdown::MarkdownParser;
pub use parser::{ContentParser, FileProbe, ParseInput};
pub use router::ParserRouter;
pub use structured::StructuredParser;
pub use table::{
    tables_in, CellValue, ColumnType, Header, NamedRange, Reconstruction, TableCell, TableIr,
};
pub use text::TextParser;
pub use xlsx::XlsxParser;

/// Parse one file's bytes with the default chain and the default budgets.
///
/// The one-line entry point. Never returns an error for "no parser understood
/// it" — see [`ParserRouter::parse`].
pub fn parse(bytes: &[u8], probe: &FileProbe) -> marrow_core::Result<ParsedArtifact> {
    ParserRouter::with_default_parsers().parse(bytes, probe)
}
