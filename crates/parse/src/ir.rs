//! The parser intermediate representation (PAR-001).
//!
//! # Invariant #1 lives here
//!
//! [`IrNode::span`] is a [`SourceSpan`], **not** an `Option<SourceSpan>`. There
//! is no constructor, no `Default`, and no builder that can produce a node
//! without one: a struct literal must name the field, so "I forgot the span"
//! is a compile error rather than a code-review finding.
//!
//! # PAR-014 lives here too
//!
//! `text` and `trust` are **private**. A node is built through exactly two
//! doors:
//!
//! - [`IrNode::structural`] — structure the parser derived (a table, a row, the
//!   metadata-only marker). Never carries file text, always
//!   [`Trust::DeterministicRuntime`].
//! - [`IrNode::content`] / [`IrNode::verbatim`] / [`IrNode::content_in`] — text
//!   lifted out of the file. Always [`Trust::UntrustedContent`].
//!
//! That is deliberately narrower than the sketch in the task, which had `text`
//! and `trust` as public fields. Public fields make "text marked as trusted"
//! representable, and the whole injection defence downstream (invariant #12)
//! is a filter on `trust`. Read access is unchanged: [`IrNode::text`] and
//! [`IrNode::trust`].

use std::ops::Range;

use marrow_core::{Code, Error, ProvenanceClass, Result, SourceSpan};
use serde::{Deserialize, Serialize};

/// Parser tier (Part 3 §63). Ordered: the router tries lower tiers first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ParserTier {
    /// Native, full provenance: byte range, line, AST node.
    T1,
    /// Native, structural provenance: page/bbox, XML path, cell ref.
    /// Nothing implements T2 at M1 — PDF is D4, deferred indefinitely (M0 F3).
    T2,
    /// Converter fallback. Degraded provenance. Not built (needs the sidecar).
    T3,
    /// Media understanding. Approximate provenance. Not built.
    T4,
    /// Metadata only. The terminal tier; the router synthesises it.
    T5,
}

impl ParserTier {
    /// Stable wire form, matching the `parse_results.parser_tier` CHECK
    /// constraint in Part 6 §106.5.
    pub const fn as_str(self) -> &'static str {
        match self {
            ParserTier::T1 => "T1",
            ParserTier::T2 => "T2",
            ParserTier::T3 => "T3",
            ParserTier::T4 => "T4",
            ParserTier::T5 => "T5",
        }
    }

    /// The best provenance this tier can honestly claim (Part 3 §63).
    ///
    /// A parser may report something *worse* than this — a lossy decode
    /// downgrades T1 to `Degraded` — but never something better.
    pub const fn best_provenance(self) -> ProvenanceClass {
        match self {
            ParserTier::T1 | ParserTier::T2 => ProvenanceClass::Exact,
            ParserTier::T3 => ProvenanceClass::Degraded,
            ParserTier::T4 => ProvenanceClass::Approximate,
            ParserTier::T5 => ProvenanceClass::MetadataOnly,
        }
    }
}

/// How a parse ended.
///
/// Part 6 §106.5's `parse_results.outcome` CHECK also lists `FAILED`,
/// `UNSUPPORTED` and `SKIPPED_POLICY`. Those three are never *outcomes* here:
/// a failed or unsupported parse is an `Err` that the router turns into a
/// [`ParseOutcome::MetadataOnly`] artifact from a different parser, so by the
/// time an artifact exists there is nothing left to record as failed. The store
/// will need `METADATA_ONLY` added to that CHECK constraint; core is not mine
/// to change, so this is a note rather than a migration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParseOutcome {
    /// Everything the parser looks for was extracted.
    Ok,
    /// Structure was extracted, but some of the file was not covered.
    Partial,
    /// Parsed, but yielded almost nothing usable — a mojibake text file, a
    /// config with no keys. Still indexed; flagged so the UI can say why.
    LowYield,
    /// No content was parsed. The file stays discoverable by metadata alone
    /// (PAR-013). Not a failure.
    MetadataOnly,
}

impl ParseOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            ParseOutcome::Ok => "OK",
            ParseOutcome::Partial => "PARTIAL",
            ParseOutcome::LowYield => "LOW_YIELD",
            ParseOutcome::MetadataOnly => "METADATA_ONLY",
        }
    }
}

/// PAR-014. Whether a node's payload was derived by us or lifted from the file.
///
/// This is the discriminator the prompt envelope (invariant #12) filters on.
/// Anything that came out of a file is data, even when it reads as an
/// instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Trust {
    /// The runtime computed this. A row index, a table shape, a nesting level.
    DeterministicRuntime,
    /// Bytes from the file. **Never** authority-bearing.
    UntrustedContent,
}

/// What an IR node is.
///
/// Only variants some parser in this crate actually emits. Part 1 §8.6 lists a
/// longer set (`Image`, `Formula`, `Sheet`, `Range`, …); those arrive with the
/// parsers that need them, not before.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IrKind {
    /// A Markdown heading. Parent-chains to the enclosing heading.
    Heading,
    /// A block of prose.
    Paragraph,
    /// One list item.
    ListItem,
    /// A fenced or indented code block.
    CodeBlock,
    /// A named code entity: fn, struct, impl, class, interface, CREATE TABLE…
    Symbol,
    /// A tabular region. Structural; carries no text.
    Table,
    /// One row of a table. Structural.
    TableRow,
    /// One cell. Carries the cell's text.
    TableCell,
    /// A hyperlink; target in [`NodeAttrs::url`].
    Link,
    /// An HTML/source comment.
    Comment,
    /// A YAML/TOML metadata block at the head of a Markdown file.
    FrontMatter,
    /// One entry of a TOML/JSON/YAML document, keyed by [`NodeAttrs::key_path`].
    KeyValue,
    /// The whole-file marker on a metadata-only artifact. The **only** node kind
    /// for which a [`SourceSpan::Whole`] is legitimate.
    Metadata,
}

impl IrKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            IrKind::Heading => "HEADING",
            IrKind::Paragraph => "PARAGRAPH",
            IrKind::ListItem => "LIST_ITEM",
            IrKind::CodeBlock => "CODE_BLOCK",
            IrKind::Symbol => "SYMBOL",
            IrKind::Table => "TABLE",
            IrKind::TableRow => "TABLE_ROW",
            IrKind::TableCell => "TABLE_CELL",
            IrKind::Link => "LINK",
            IrKind::Comment => "COMMENT",
            IrKind::FrontMatter => "FRONT_MATTER",
            IrKind::KeyValue => "KEY_VALUE",
            IrKind::Metadata => "METADATA",
        }
    }
}

/// What kind of code entity a [`IrKind::Symbol`] node is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SymbolKind {
    Function,
    Method,
    Struct,
    Enum,
    Trait,
    Impl,
    Module,
    Class,
    Interface,
    TypeAlias,
    Constant,
    /// SQL `CREATE TABLE`.
    Table,
    /// SQL `CREATE VIEW`.
    View,
    /// SQL `CREATE INDEX`.
    Index,
    /// SQL routine: function, procedure, trigger.
    Routine,
}

/// A small, closed set of node attributes.
///
/// Deliberately **not** a `serde_json::Value` bag. A JSON blob is where schema
/// goes to die: nothing type-checks it, nothing migrates it, and every consumer
/// invents its own key spelling. Adding a field here is one line and a compile
/// error at every construction site that ought to set it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeAttrs {
    /// Heading level, 1–6.
    pub level: Option<u8>,
    /// Language label: a code fence's info string, or the source language of a
    /// code file. Derived by us for code files, lifted from the file for fences.
    pub language: Option<String>,
    /// What sort of code entity this is.
    pub symbol_kind: Option<SymbolKind>,
    /// Declared name of a symbol. Lifted from the file, so untrusted.
    pub name: Option<String>,
    /// Dotted key path into a structured document, e.g. `package.metadata.docs`.
    pub key_path: Option<String>,
    /// Link target. Lifted from the file, so untrusted — never auto-followed.
    pub url: Option<String>,
    /// Zero-based row index within the enclosing table.
    pub row: Option<u32>,
    /// Zero-based column index within the enclosing row.
    pub col: Option<u32>,
    /// How many rows this cell covers. `None` means one — TBL-004 asks for
    /// merged regions to be *preserved*, so the formats that have them say so
    /// and the ones that do not stay silent rather than claiming `1`.
    pub rowspan: Option<u32>,
    /// How many columns this cell covers.
    pub colspan: Option<u32>,
    /// Column header for this cell, when the table has one.
    pub column_name: Option<String>,
    /// 1-based inclusive line range. Companion to a `Bytes` span, per the note
    /// on [`SourceSpan::Lines`] — a node carries one span, and the byte range is
    /// the one that has to be exact, so lines ride along here.
    pub line_start: Option<u32>,
    /// 1-based inclusive end line.
    pub line_end: Option<u32>,
    /// How sure the producer is that this node's text is what the source says,
    /// in `[0, 1]`. Only meaningful where reading the text was a guess — OCR —
    /// and `None` everywhere a parser simply read bytes, which is most places.
    ///
    /// Distinct from [`ParsedArtifact::provenance`], which classifies the whole
    /// artifact: one blurry line in an otherwise crisp screenshot is a fact
    /// about that line, and averaging it away would hide exactly the node a
    /// reader most needs warning about.
    pub confidence: Option<f32>,
}

impl NodeAttrs {
    /// Record the 1-based inclusive line range covering `range`.
    pub fn with_lines(mut self, lines: &LineIndex, range: &Range<usize>) -> Self {
        self.line_start = Some(lines.line_of(range.start));
        self.line_end = Some(lines.line_of(range.end.saturating_sub(1).max(range.start)));
        self
    }
}

/// One node of the IR. See the module docs for the two invariants it enforces.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IrNode {
    pub kind: IrKind,
    /// **Invariant #1.** Mandatory, by construction.
    pub span: SourceSpan,
    /// Index into [`ParsedArtifact::nodes`]. An arena, not pointers: the IR is
    /// serialised, diffed and written to SQLite as rows, and `Rc`/`Box` trees
    /// survive none of that.
    pub parent: Option<usize>,
    /// Document order within the artifact. Assigned by [`ArtifactBuilder`].
    pub ordinal: u32,
    pub attrs: NodeAttrs,
    text: Option<String>,
    trust: Trust,
    verbatim: bool,
}

impl IrNode {
    /// Structure the parser derived. Carries no file text (PAR-014).
    pub fn structural(kind: IrKind, span: SourceSpan) -> Self {
        Self {
            kind,
            span,
            parent: None,
            ordinal: 0,
            attrs: NodeAttrs::default(),
            text: None,
            trust: Trust::DeterministicRuntime,
            verbatim: false,
        }
    }

    /// Text lifted out of the file. Always untrusted (PAR-014).
    ///
    /// Prefer [`IrNode::content_in`] when the source string is to hand: it also
    /// records whether the text is the span's bytes verbatim.
    pub fn content(kind: IrKind, span: SourceSpan, text: impl Into<String>) -> Self {
        Self {
            kind,
            span,
            parent: None,
            ordinal: 0,
            attrs: NodeAttrs::default(),
            text: Some(text.into()),
            trust: Trust::UntrustedContent,
            verbatim: false,
        }
    }

    /// Text lifted out of the file at `range`, with `text` as its extracted
    /// form. Sets a `Bytes` span and marks the node verbatim when `text` is
    /// exactly `source[range]`.
    ///
    /// Errors if `range` is out of bounds or lands mid-codepoint. That would be
    /// a provenance lie, so it is an invariant violation rather than a clamp.
    pub fn content_in(
        kind: IrKind,
        source: &str,
        range: Range<usize>,
        text: impl Into<String>,
    ) -> Result<Self> {
        let slice = slice_or_invariant(source, &range)?;
        let text: String = text.into();
        let verbatim = slice == text;
        Ok(Self {
            kind,
            span: bytes_span(&range),
            parent: None,
            ordinal: 0,
            attrs: NodeAttrs::default(),
            text: Some(text),
            trust: Trust::UntrustedContent,
            verbatim,
        })
    }

    /// Text lifted out of the file, exactly `source[range]`.
    pub fn verbatim(kind: IrKind, source: &str, range: Range<usize>) -> Result<Self> {
        let slice = slice_or_invariant(source, &range)?;
        Ok(Self {
            kind,
            span: bytes_span(&range),
            parent: None,
            ordinal: 0,
            attrs: NodeAttrs::default(),
            text: Some(slice.to_owned()),
            trust: Trust::UntrustedContent,
            verbatim: true,
        })
    }

    pub fn with_attrs(mut self, attrs: NodeAttrs) -> Self {
        self.attrs = attrs;
        self
    }

    /// The node's text, if it has any. Always [`Trust::UntrustedContent`].
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }

    /// PAR-014 classification.
    pub fn trust(&self) -> Trust {
        self.trust
    }

    /// Whether [`IrNode::text`] is byte-for-byte the span's slice.
    ///
    /// False for nodes whose text was normalised on the way out: a Markdown
    /// heading's span includes its `##` marker, a quoted CSV field's span
    /// includes its quotes and doubled escapes. The span still covers the text
    /// either way; this says whether equality is expected.
    pub fn is_verbatim(&self) -> bool {
        self.verbatim
    }

    /// The byte range, when the span is a byte range.
    pub fn byte_range(&self) -> Option<Range<usize>> {
        match &self.span {
            SourceSpan::Bytes { start, end } => Some(*start as usize..*end as usize),
            _ => None,
        }
    }
}

fn bytes_span(range: &Range<usize>) -> SourceSpan {
    SourceSpan::Bytes {
        start: range.start as u64,
        end: range.end as u64,
    }
}

fn slice_or_invariant<'a>(source: &'a str, range: &Range<usize>) -> Result<&'a str> {
    source.get(range.clone()).ok_or_else(|| {
        Error::invariant(
            "A parser produced a byte span that is out of bounds or not on a character \
             boundary. Provenance must resolve to real bytes; this is a bug in the parser, \
             not in the file.",
        )
        .with_context(format!(
            "range {}..{} of {} bytes",
            range.start,
            range.end,
            source.len()
        ))
    })
}

/// A non-fatal problem noticed during a parse.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParseWarning {
    /// Stable code from the core taxonomy.
    pub code: String,
    /// Cause and action, per SUP-001.
    pub message: String,
    /// Where, when the problem has a location.
    pub span: Option<SourceSpan>,
}

impl ParseWarning {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code: code.as_str().to_owned(),
            message: message.into(),
            span: None,
        }
    }

    pub fn at(mut self, span: SourceSpan) -> Self {
        self.span = Some(span);
        self
    }

    /// Record a failed parser attempt so the reason a file degraded survives
    /// into the index-health view instead of only into a log line.
    pub fn from_error(e: &Error) -> Self {
        Self {
            code: e.code().as_str().to_owned(),
            message: e.message().to_owned(),
            span: None,
        }
    }
}

/// The output of one parse attempt against one file version.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParsedArtifact {
    /// Stable parser identity. Persisted with the result (PAR-003) so an
    /// upgrade can schedule reprocessing without a manual reindex.
    pub parser_id: &'static str,
    pub parser_version: &'static str,
    pub tier: ParserTier,
    pub provenance: ProvenanceClass,
    pub outcome: ParseOutcome,
    /// Arena. `parent` indexes into this vector.
    pub nodes: Vec<IrNode>,
    pub warnings: Vec<ParseWarning>,
}

impl ParsedArtifact {
    /// The terminal artifact (PAR-013): a file with no parser is still a file.
    ///
    /// This is not a failure and must never be reported as one. `Whole` is the
    /// honest span for it — there is no location to point at, because nothing
    /// was read.
    pub fn metadata_only(warnings: Vec<ParseWarning>) -> Self {
        Self {
            parser_id: "metadata",
            parser_version: "1",
            tier: ParserTier::T5,
            provenance: ProvenanceClass::MetadataOnly,
            outcome: ParseOutcome::MetadataOnly,
            nodes: vec![IrNode::structural(IrKind::Metadata, SourceSpan::Whole)],
            warnings,
        }
    }

    /// Structural checks the router runs on every artifact before returning it.
    ///
    /// A third-party parser cannot be trusted to have got these right, and the
    /// cost of checking is a walk over a few hundred nodes.
    pub fn validate(&self) -> Result<()> {
        for (i, n) in self.nodes.iter().enumerate() {
            if let Some(p) = n.parent {
                if p >= self.nodes.len() {
                    return Err(Error::invariant(
                        "IR node parent index points outside the arena. The parser built a \
                         malformed tree; fix the parser rather than tolerating the node.",
                    )
                    .with_context(format!("node {i} -> parent {p}")));
                }
                if p >= i {
                    return Err(Error::invariant(
                        "IR node parent must appear before the child in document order, so \
                         the arena can be written to SQLite without a second pass.",
                    )
                    .with_context(format!("node {i} -> parent {p}")));
                }
            }
            // PAR-014, belt and braces. `IrNode`'s constructors already make
            // this unrepresentable; the check catches a future `Deserialize`
            // of an artifact produced by an older or hostile build.
            if n.text.is_some() && n.trust == Trust::DeterministicRuntime {
                return Err(Error::invariant(
                    "An IR node carries file text but is labelled DETERMINISTIC_RUNTIME. \
                     File text is always untrusted (PAR-014); relabel it at the source.",
                )
                .with_context(format!("node {i} kind {}", n.kind.as_str())));
            }
            if !n.span.is_precise() && n.kind != IrKind::Metadata {
                return Err(Error::invariant(
                    "An IR node has a whole-file span. Invariant #1 requires provenance to \
                     an exact location; only the metadata-only marker may be whole-file.",
                )
                .with_context(format!("node {i} kind {}", n.kind.as_str())));
            }
        }
        Ok(())
    }

    /// Total bytes of extracted text. Feeds `parse_results.char_yield`.
    pub fn text_yield(&self) -> usize {
        self.nodes
            .iter()
            .filter_map(|n| n.text())
            .map(str::len)
            .sum()
    }
}

/// Accumulates nodes for one parse, enforcing budgets and arena discipline.
///
/// Parsers never push into `Vec<IrNode>` directly: ordinals, depth accounting
/// and the node budget all live here, so getting them wrong is not an option a
/// parser has.
#[derive(Debug)]
pub struct ArtifactBuilder {
    parser_id: &'static str,
    parser_version: &'static str,
    tier: ParserTier,
    provenance: ProvenanceClass,
    outcome: ParseOutcome,
    nodes: Vec<IrNode>,
    /// Depth of each node, parallel to `nodes`. Avoids walking the parent chain
    /// on every push.
    depth: Vec<u16>,
    warnings: Vec<ParseWarning>,
    budget: crate::budget::BudgetGuard,
}

impl ArtifactBuilder {
    pub fn new(
        parser_id: &'static str,
        parser_version: &'static str,
        tier: ParserTier,
        budget: crate::budget::BudgetGuard,
    ) -> Self {
        Self {
            parser_id,
            parser_version,
            tier,
            provenance: tier.best_provenance(),
            outcome: ParseOutcome::Ok,
            nodes: Vec::new(),
            depth: Vec::new(),
            warnings: Vec::new(),
            budget,
        }
    }

    /// Append a node under `parent`. Returns its arena index.
    pub fn push(&mut self, parent: Option<usize>, mut node: IrNode) -> Result<usize> {
        self.budget.check_time()?;
        self.budget.check_node(self.nodes.len())?;

        let depth = match parent {
            Some(p) => {
                let d = self
                    .depth
                    .get(p)
                    .copied()
                    .ok_or_else(|| Error::invariant("parent index is not in the arena yet"))?;
                d.saturating_add(1)
            }
            None => 0,
        };
        self.budget.check_depth(depth)?;

        node.parent = parent;
        node.ordinal = self.nodes.len() as u32;
        self.nodes.push(node);
        self.depth.push(depth);
        Ok(self.nodes.len() - 1)
    }

    /// Widen a already-pushed node's byte span to end at `to`.
    ///
    /// For a container whose extent is only known at its closing tag — an HTML
    /// `<table>` is not `<table>`, it is everything up to `</table>`. Widening
    /// only ever moves the end forward, and only on a `Bytes` span, so it cannot
    /// turn a precise span into a wrong one.
    pub fn widen_span(&mut self, idx: usize, to: u64) {
        if let Some(SourceSpan::Bytes { end, .. }) = self.nodes.get_mut(idx).map(|n| &mut n.span) {
            *end = (*end).max(to);
        }
    }

    pub fn warn(&mut self, w: ParseWarning) {
        // Bounded: a pathological file must not turn into a million warnings.
        if self.warnings.len() < crate::budget::MAX_WARNINGS {
            self.warnings.push(w);
        }
    }

    /// Report something worse than the tier's best. Never something better.
    pub fn degrade_provenance(&mut self, to: ProvenanceClass) {
        if to > self.provenance {
            self.provenance = to;
        }
    }

    pub fn set_outcome(&mut self, outcome: ParseOutcome) {
        self.outcome = outcome;
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn budget(&self) -> &crate::budget::BudgetGuard {
        &self.budget
    }

    pub fn finish(self) -> ParsedArtifact {
        ParsedArtifact {
            parser_id: self.parser_id,
            parser_version: self.parser_version,
            tier: self.tier,
            provenance: self.provenance,
            outcome: self.outcome,
            nodes: self.nodes,
            warnings: self.warnings,
        }
    }
}

/// Byte offset → 1-based line number, by binary search over line starts.
#[derive(Clone, Debug)]
pub struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    pub fn new(source: &str) -> Self {
        let mut starts = vec![0usize];
        starts.extend(
            source
                .bytes()
                .enumerate()
                .filter(|(_, b)| *b == b'\n')
                .map(|(i, _)| i + 1),
        );
        Self { starts }
    }

    /// 1-based line containing `offset`.
    pub fn line_of(&self, offset: usize) -> u32 {
        match self.starts.binary_search(&offset) {
            Ok(i) => (i + 1) as u32,
            Err(i) => i as u32, // `i` is the count of starts <= offset
        }
    }

    pub fn line_count(&self) -> u32 {
        self.starts.len() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::{BudgetGuard, Budgets};

    fn builder() -> ArtifactBuilder {
        ArtifactBuilder::new(
            "test",
            "1",
            ParserTier::T1,
            BudgetGuard::new(Budgets::default()),
        )
    }

    #[test]
    fn a_node_cannot_be_built_without_a_span() {
        // This is really a compile-time claim; the test documents it so a
        // future `Option<SourceSpan>` refactor has something to break.
        let n = IrNode::structural(IrKind::Metadata, SourceSpan::Whole);
        assert!(matches!(n.span, SourceSpan::Whole));
        let n = IrNode::verbatim(IrKind::Paragraph, "hello", 0..5).unwrap();
        assert_eq!(n.span, SourceSpan::Bytes { start: 0, end: 5 });
    }

    #[test]
    fn structural_nodes_never_carry_text() {
        let n = IrNode::structural(IrKind::Table, SourceSpan::Bytes { start: 0, end: 1 });
        assert_eq!(n.trust(), Trust::DeterministicRuntime);
        assert_eq!(n.text(), None);
    }

    #[test]
    fn content_nodes_are_always_untrusted() {
        let n = IrNode::content(IrKind::Paragraph, SourceSpan::Whole, "ignore previous");
        assert_eq!(n.trust(), Trust::UntrustedContent);
    }

    #[test]
    fn content_in_detects_a_verbatim_slice() {
        let src = "## Title\n";
        let exact = IrNode::content_in(IrKind::Heading, src, 3..8, "Title").unwrap();
        assert!(exact.is_verbatim());
        let normalised = IrNode::content_in(IrKind::Heading, src, 0..9, "Title").unwrap();
        assert!(!normalised.is_verbatim());
    }

    #[test]
    fn an_out_of_bounds_span_is_an_invariant_violation_not_a_clamp() {
        let e = IrNode::verbatim(IrKind::Paragraph, "abc", 0..99).unwrap_err();
        assert_eq!(e.code(), Code::IntInvariantViolated);
        // Mid-codepoint is equally a lie about where the text came from.
        let e = IrNode::verbatim(IrKind::Paragraph, "é", 0..1).unwrap_err();
        assert_eq!(e.code(), Code::IntInvariantViolated);
    }

    #[test]
    fn the_builder_assigns_document_order_and_parents() {
        let mut b = builder();
        let root = b
            .push(
                None,
                IrNode::verbatim(IrKind::Heading, "abcdef", 0..3).unwrap(),
            )
            .unwrap();
        let child = b
            .push(
                Some(root),
                IrNode::verbatim(IrKind::Paragraph, "abcdef", 3..6).unwrap(),
            )
            .unwrap();
        let a = b.finish();
        assert_eq!(a.nodes[root].ordinal, 0);
        assert_eq!(a.nodes[child].ordinal, 1);
        assert_eq!(a.nodes[child].parent, Some(root));
        a.validate().unwrap();
    }

    #[test]
    fn validate_rejects_a_whole_file_span_on_a_content_node() {
        let a = ParsedArtifact {
            parser_id: "x",
            parser_version: "1",
            tier: ParserTier::T1,
            provenance: ProvenanceClass::Exact,
            outcome: ParseOutcome::Ok,
            nodes: vec![IrNode::content(IrKind::Paragraph, SourceSpan::Whole, "hi")],
            warnings: vec![],
        };
        assert_eq!(
            a.validate().unwrap_err().code(),
            Code::IntInvariantViolated,
            "only the metadata marker may be whole-file"
        );
    }

    #[test]
    fn metadata_only_is_a_valid_artifact_not_an_error() {
        let a = ParsedArtifact::metadata_only(vec![]);
        a.validate().unwrap();
        assert_eq!(a.outcome, ParseOutcome::MetadataOnly);
        assert_eq!(a.provenance, ProvenanceClass::MetadataOnly);
        assert_eq!(a.nodes.len(), 1);
    }

    #[test]
    fn provenance_only_ever_degrades() {
        let b = builder();
        assert_eq!(b.finish().provenance, ProvenanceClass::Exact);
        let mut b = builder();
        b.degrade_provenance(ProvenanceClass::Degraded);
        b.degrade_provenance(ProvenanceClass::Exact); // must not upgrade back
        assert_eq!(b.finish().provenance, ProvenanceClass::Degraded);
    }

    #[test]
    fn line_index_is_one_based_and_handles_the_last_line() {
        let src = "a\nbb\n\nccc";
        let li = LineIndex::new(src);
        assert_eq!(li.line_of(0), 1);
        assert_eq!(li.line_of(2), 2);
        assert_eq!(li.line_of(5), 3);
        assert_eq!(li.line_of(6), 4);
        assert_eq!(li.line_of(src.len() - 1), 4);
    }
}
