//! The parser chain of responsibility (LLD §2.2).
//!
//! Part 3 §63's T1 → T2 → T3 → T5 model *is* a chain: try native, fall back on
//! unsupported or failed, degrade provenance as you go. The router owns the
//! chain; parsers know nothing about each other, which is what makes "add a
//! format" a one-crate change (LLD §10).
//!
//! # The chain always terminates in success
//!
//! There is no path out of [`ParserRouter::parse`] that reports "no parser" as
//! an error. A file nothing understands falls through to a metadata-only
//! artifact and stays discoverable (PAR-013). "No parser" is a fact about the
//! file, not a failure of the system, and the difference shows up directly in
//! the index-health view: a corpus with 3,478 photos in it would otherwise
//! report itself as 37% broken.
//!
//! # The panic boundary
//!
//! LLD §5 puts parsers in a subprocess so a panic or segfault kills one file
//! rather than the daemon (NFR-001). That subprocess does not exist yet, and
//! building it before there is anything to run inside it would be building
//! ahead of the milestone. `catch_unwind` is the in-process half of the same
//! contract: it catches the Rust panics, records `PAR_WORKER_CRASH`, and moves
//! to the next tier. It cannot catch a segfault in a Tree-sitter grammar's C
//! code — that is precisely what the subprocess is for, and this is the seam it
//! will slot into when it arrives.

use std::panic::{catch_unwind, AssertUnwindSafe};

use marrow_core::{Code, Error, Result};
use tracing::{debug, warn};

use crate::budget::{BudgetGuard, Budgets};
use crate::ir::{ParseWarning, ParsedArtifact};
use crate::parser::{ContentParser, FileProbe, ParseInput};

/// Ordered chain of parsers plus the budgets they run under.
#[derive(Debug)]
pub struct ParserRouter {
    /// Sorted by tier, stable within a tier so registration order breaks ties.
    parsers: Vec<Box<dyn ContentParser>>,
    budgets: Budgets,
}

impl ParserRouter {
    /// An empty router. Terminates in metadata-only for everything.
    pub fn empty() -> Self {
        Self {
            parsers: Vec::new(),
            budgets: Budgets::default(),
        }
    }

    /// Every parser this crate ships, in the order M0 §6 justifies.
    ///
    /// Order matters within T1, where `handles()` overlaps: the plain-text
    /// parser claims anything text-shaped, so it is registered last and acts as
    /// the T1 catch-all. Registering it first would mean a `.rs` file is
    /// paragraphs and never symbols.
    pub fn with_default_parsers() -> Self {
        let mut r = Self::empty();
        // 1. Code — ~1,300 files, the largest real content class.
        r.register(Box::new(crate::code::CodeParser::new()));
        // 2. Markdown — 289 files, and the format the docs themselves are in.
        r.register(Box::new(crate::markdown::MarkdownParser));
        // 3. Structured config — ~165 TOML/JSON/YAML files.
        r.register(Box::new(crate::structured::StructuredParser));
        // 4. CSV — ~90 files.
        r.register(Box::new(crate::csv::CsvParser));
        // 5. PDF — before the text fallback, which would otherwise decode a
        //    PDF's compressed streams into replacement characters and call it
        //    content. On a non-macOS build this refuses and the file stays
        //    findable by name.
        r.register(Box::new(crate::pdf::PdfParser));
        // 6. Plain text — 449 files, and the T1 fallback for everything else
        //    that decodes.
        r.register(Box::new(crate::text::TextParser));
        r
    }

    pub fn with_budgets(mut self, budgets: Budgets) -> Self {
        self.budgets = budgets;
        self
    }

    pub fn budgets(&self) -> &Budgets {
        &self.budgets
    }

    /// Add a parser. Kept sorted by tier; ties keep registration order.
    pub fn register(&mut self, parser: Box<dyn ContentParser>) {
        self.parsers.push(parser);
        self.parsers.sort_by_key(|p| p.tier());
    }

    /// Parser ids in chain order. For diagnostics and tests.
    pub fn parser_ids(&self) -> Vec<&'static str> {
        self.parsers.iter().map(|p| p.id()).collect()
    }

    /// Run the chain. **Never** returns an error for "nothing handled it".
    ///
    /// It does still return an error for a storage or policy failure raised by
    /// a parser, because those are not about this file and swallowing them
    /// would keep a broken workspace running silently.
    pub fn parse(&self, bytes: &[u8], probe: &FileProbe) -> Result<ParsedArtifact> {
        let mut warnings: Vec<ParseWarning> = Vec::new();

        // Invariant #5, re-checked here rather than trusted from upstream. If
        // these bytes exist at all for a non-Resident file, something already
        // went wrong; parsing them would compound it.
        if !probe.tier.safe_to_read() {
            debug!(file = %probe.file_name, tier = ?probe.tier, "not resident; metadata only");
            warnings.push(ParseWarning::new(
                Code::FsPlaceholderSkipped,
                "This file is not on local disk, so only its metadata is indexed. Download it \
                 in your sync client to have its contents parsed.",
            ));
            return Ok(ParsedArtifact::metadata_only(warnings));
        }

        // PAR-010. Checked against the bytes we were actually handed, not the
        // size the probe claims: `lstat` reports the logical size, which for a
        // partially hydrated file is not what is in memory.
        let size = bytes.len() as u64;
        let guard = BudgetGuard::new(self.budgets);
        if let Err(e) = guard.check_file_size(size.max(probe.size)) {
            warn!(file = %probe.file_name, %e, "over parse budget; metadata only");
            warnings.push(ParseWarning::from_error(&e));
            return Ok(ParsedArtifact::metadata_only(warnings));
        }

        for parser in &self.parsers {
            let attempt = self.attempt(parser.as_ref(), bytes, probe);
            match attempt {
                Attempt::Skipped => continue,
                Attempt::Crashed => {
                    // NFR-001: one file, not the process.
                    warn!(parser = parser.id(), file = %probe.file_name, "parser panicked");
                    warnings.push(ParseWarning::new(
                        Code::ParWorkerCrash,
                        format!(
                            "The `{}` parser crashed on this file, so a lower-fidelity parser \
                             was used instead. The file is still indexed.",
                            parser.id()
                        ),
                    ));
                    continue;
                }
                Attempt::Unsupported => {
                    // Silent by design: "this parser does not do this format"
                    // is the chain working, not a problem to report.
                    debug!(parser = parser.id(), file = %probe.file_name, "unsupported; next tier");
                    continue;
                }
                Attempt::Degraded(e) => {
                    warn!(parser = parser.id(), file = %probe.file_name, %e, "parse failed, degrading");
                    warnings.push(ParseWarning::from_error(&e));
                    continue;
                }
                Attempt::Fatal(e) => {
                    // Storage or policy. Not about this file; stop.
                    return Err(e);
                }
                Attempt::Parsed(mut artifact) => {
                    artifact.validate()?;
                    // Carry forward why the higher-fidelity parsers did not
                    // win, so index health can explain a degraded result.
                    if !warnings.is_empty() {
                        let mut all = warnings;
                        all.append(&mut artifact.warnings);
                        artifact.warnings = all;
                    }
                    debug!(
                        parser = artifact.parser_id,
                        nodes = artifact.nodes.len(),
                        outcome = artifact.outcome.as_str(),
                        "parsed"
                    );
                    return Ok(*artifact);
                }
            }
        }

        debug!(file = %probe.file_name, "no parser claimed the file; metadata only");
        Ok(ParsedArtifact::metadata_only(warnings))
    }

    /// One parser attempt, with the panic boundary around it.
    ///
    /// `handles` is inside the boundary as well: it is supposed to be a cheap
    /// probe test, but a parser that panics there would otherwise take the
    /// process down before the chain had a chance to route around it.
    fn attempt(&self, parser: &dyn ContentParser, bytes: &[u8], probe: &FileProbe) -> Attempt {
        let input = ParseInput {
            bytes,
            probe,
            // A fresh guard per attempt: the second parser gets the whole
            // wall-clock budget, not what the first one left behind.
            budget: BudgetGuard::new(self.budgets),
        };

        // `AssertUnwindSafe`: parsers take `&self` and hold no interior
        // mutability, so there is no half-updated state for a caught panic to
        // expose. The bytes and probe are borrowed immutably and unchanged.
        let result = catch_unwind(AssertUnwindSafe(|| {
            if !parser.handles(probe) {
                return None;
            }
            Some(parser.parse(input))
        }));

        match result {
            Err(_panic) => Attempt::Crashed,
            Ok(None) => Attempt::Skipped,
            Ok(Some(Ok(a))) => Attempt::Parsed(Box::new(a)),
            Ok(Some(Err(e))) if e.code() == Code::ParUnsupported => Attempt::Unsupported,
            Ok(Some(Err(e))) if e.code().isolates_to_one_file() => Attempt::Degraded(e),
            Ok(Some(Err(e))) => Attempt::Fatal(e),
        }
    }
}

impl Default for ParserRouter {
    fn default() -> Self {
        Self::with_default_parsers()
    }
}

/// Outcome of one link in the chain. Named so the routing table in
/// [`ParserRouter::parse`] reads as policy rather than as error plumbing.
enum Attempt {
    /// `handles()` said no.
    Skipped,
    Parsed(Box<ParsedArtifact>),
    /// `ParUnsupported`: the guess from the file name was wrong.
    Unsupported,
    /// Failed in a way that isolates to this file. Try the next tier.
    Degraded(Error),
    /// Storage or policy. Propagate.
    Fatal(Error),
    /// Panicked. `PAR_WORKER_CRASH`.
    Crashed,
}

/// Suppress the default panic message for the duration of a call.
///
/// The router deliberately does not touch the panic hook itself — a parser
/// crashing in production should leave a backtrace in the log. Tests that
/// *expect* a crash use this so the output stays readable.
#[doc(hidden)]
pub fn without_panic_output<T>(f: impl FnOnce() -> T) -> T {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let out = f();
    std::panic::set_hook(previous);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IrKind, ParseOutcome, ParserTier};
    use marrow_core::{ProvenanceClass, TierState};

    #[derive(Debug)]
    struct Never(ParserTier, &'static str);

    impl ContentParser for Never {
        fn id(&self) -> &'static str {
            self.1
        }
        fn version(&self) -> &'static str {
            "1"
        }
        fn tier(&self) -> ParserTier {
            self.0
        }
        fn handles(&self, _: &FileProbe) -> bool {
            true
        }
        fn parse(&self, _: ParseInput<'_>) -> Result<ParsedArtifact> {
            Err(Error::new(Code::ParUnsupported, "not this one"))
        }
    }

    #[derive(Debug)]
    struct Fatal;

    impl ContentParser for Fatal {
        fn id(&self) -> &'static str {
            "fatal"
        }
        fn version(&self) -> &'static str {
            "1"
        }
        fn tier(&self) -> ParserTier {
            ParserTier::T1
        }
        fn handles(&self, _: &FileProbe) -> bool {
            true
        }
        fn parse(&self, _: ParseInput<'_>) -> Result<ParsedArtifact> {
            Err(Error::new(
                Code::DbDiskFull,
                "no space; free some and retry",
            ))
        }
    }

    #[test]
    fn parsers_run_in_tier_order_with_registration_breaking_ties() {
        let mut r = ParserRouter::empty();
        r.register(Box::new(Never(ParserTier::T3, "t3")));
        r.register(Box::new(Never(ParserTier::T1, "t1a")));
        r.register(Box::new(Never(ParserTier::T1, "t1b")));
        assert_eq!(r.parser_ids(), vec!["t1a", "t1b", "t3"]);
    }

    #[test]
    fn a_storage_error_stops_the_chain() {
        let mut r = ParserRouter::empty();
        r.register(Box::new(Fatal));
        let probe = FileProbe::new("x.txt", 1);
        let e = r.parse(b"x", &probe).unwrap_err();
        assert_eq!(e.code(), Code::DbDiskFull);
    }

    #[test]
    fn a_placeholder_is_never_content_parsed() {
        let r = ParserRouter::with_default_parsers();
        let probe = FileProbe::new("notes.md", 100).with_tier(TierState::Placeholder);
        let a = r.parse(b"# real content", &probe).unwrap();
        assert_eq!(a.outcome, ParseOutcome::MetadataOnly);
        assert_eq!(a.provenance, ProvenanceClass::MetadataOnly);
        assert_eq!(a.nodes[0].kind, IrKind::Metadata);
        assert_eq!(a.warnings[0].code, Code::FsPlaceholderSkipped.as_str());
    }

    #[test]
    fn unsupported_falls_through_without_a_warning() {
        let mut r = ParserRouter::empty();
        r.register(Box::new(Never(ParserTier::T1, "a")));
        r.register(Box::new(Never(ParserTier::T2, "b")));
        let a = r.parse(b"x", &FileProbe::new("x.q", 1)).unwrap();
        assert_eq!(a.outcome, ParseOutcome::MetadataOnly);
        assert!(a.warnings.is_empty(), "the chain working is not a warning");
    }
}
