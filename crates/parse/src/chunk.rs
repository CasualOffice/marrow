//! Chunking — IR to retrieval units ([Part 6 §112]).
//!
//! Lives here rather than in its own crate because it is a pure transformation
//! of [`ParsedArtifact`] with exactly one implementation. It is not a seam, so
//! it does not get a port ([LLD §6] — four seams is the budget).
//!
//! # Why walk the IR rather than the text
//!
//! The whole reason the parsers produce structure is so chunk boundaries can
//! fall where meaning does. A sliding window over raw text would split a
//! function in half and lose the heading a paragraph sits under; both are
//! things retrieval then cannot recover.
//!
//! # Context, not overlap
//!
//! Fixed-token chunkers duplicate neighbouring text so a match near a boundary
//! still has context. That is expensive and imprecise. Here a chunk carries its
//! **ancestor chain** as `context_prefix` instead (CHK-002) — `impl
//! TokenService › fn refresh_token`, or `Authentication › Refresh token
//! rotation`. Cheaper, exact, and it is also what the UI renders as the
//! breadcrumb.
//!
//! [Part 6 §112]: ../../../docs/Part_6_Engineering_Reference.md
//! [LLD §6]: ../../../docs/LLD.md

use marrow_core::{ContentHash, ProvenanceClass, SourceSpan};

use crate::ir::{IrKind, IrNode, ParsedArtifact};

/// Chunker identity, persisted so a change can schedule re-chunking (§20.2).
pub const CHUNKER_VERSION: &str = "1";

/// Sizing policy.
///
/// Token counts are approximated from bytes rather than tokenized: the real
/// tokenizer depends on the embedding model (CHK-008), which M1 does not have.
/// Storing the byte count and a documented ratio means the estimate can be
/// replaced without re-chunking.
#[derive(Clone, Copy, Debug)]
pub struct ChunkPolicy {
    /// Target size. Boundaries are structural, so this is a goal, not a rule.
    pub target_bytes: usize,
    /// Hard ceiling. A single node larger than this is split at a safe point.
    pub max_bytes: usize,
    /// Below this, a chunk is merged into its neighbour rather than emitted —
    /// a 12-byte chunk is noise in a ranked list.
    pub min_bytes: usize,
}

impl Default for ChunkPolicy {
    fn default() -> Self {
        // ~512 tokens at the ~4 bytes/token rule of thumb for English and code.
        Self {
            target_bytes: 2048,
            max_bytes: 4096,
            min_bytes: 256,
        }
    }
}

/// One retrieval unit.
#[derive(Clone, Debug, PartialEq)]
pub struct Chunk {
    /// Ancestor chain, `›`-joined. Empty at the document root.
    pub context_prefix: String,
    pub text: String,
    /// **Invariant #1.** Where in the source this came from.
    pub span: SourceSpan,
    /// Index of the IR node this chunk was rooted at.
    pub root_node: usize,
    pub kind: ChunkKind,
    pub provenance: ProvenanceClass,
    /// Content-addressed, so re-chunking an unchanged file reuses its
    /// embeddings (EMB-008) and unchanged chunks keep their IDs (CHK-003/007).
    pub text_hash: ContentHash,
    pub byte_len: usize,
}

impl Chunk {
    /// Approximate token count. See [`ChunkPolicy`] on why this is an estimate.
    pub fn approx_tokens(&self) -> usize {
        self.byte_len.div_ceil(4)
    }

    /// What the lexical index should treat as the title field.
    pub fn title(&self) -> &str {
        &self.context_prefix
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChunkKind {
    Text,
    Code,
    /// A band of table rows, with the headers and caption repeated (TBL-011).
    TableBand,
    /// The one chunk per table that describes its columns rather than its rows
    /// (TBL-011). What semantic search actually matches a question about shape
    /// against — no band of forty rows says "revenue by quarter" anywhere.
    TableSchema,
    Metadata,
}

impl ChunkKind {
    /// The `chunks.chunk_kind` value, matching the CHECK constraint in
    /// [Part 6 §106.7].
    pub const fn as_str(self) -> &'static str {
        match self {
            ChunkKind::Text => "TEXT",
            ChunkKind::Code => "CODE",
            ChunkKind::TableBand => "TABLE_BAND",
            ChunkKind::TableSchema => "TABLE_SCHEMA",
            ChunkKind::Metadata => "METADATA",
        }
    }

    const fn is_table(self) -> bool {
        matches!(self, ChunkKind::TableBand | ChunkKind::TableSchema)
    }
}

/// Split an artifact into chunks.
///
/// `source` is the decoded file text, used to slice spans for nodes that carry
/// no text of their own (a `Table` is structural; its cells hold the text).
pub fn chunk(artifact: &ParsedArtifact, policy: &ChunkPolicy) -> Vec<Chunk> {
    if artifact.nodes.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    let mut pending: Option<Pending> = None;

    // TBL-011. A table is chunked as a table — bands with the headers repeated,
    // plus a schema chunk — not as a run of loose cells. Claiming the nodes up
    // front is what stops the ordinary path from also emitting them: a chunk of
    // rows with no header text is unsearchable by the thing that names the
    // column, which is the whole point of CHK-002.
    let tables = crate::table::tables_in(artifact);
    let mut owner = vec![usize::MAX; artifact.nodes.len()];
    for (ti, t) in tables.iter().enumerate() {
        // First claim wins, so a nested table stays part of the outer one
        // rather than being emitted twice.
        for (i, owned) in crate::table::descendants_of(artifact, t.node)
            .into_iter()
            .enumerate()
        {
            if owned && owner[i] == usize::MAX {
                owner[i] = ti;
            }
        }
    }

    for (i, node) in artifact.nodes.iter().enumerate() {
        if let Some(t) = owner.get(i).and_then(|o| tables.get(*o)) {
            if t.node == i {
                flush(&mut pending, &mut out, artifact.provenance);
                out.extend(table_chunks(artifact, t, policy));
            }
            continue;
        }

        // Structural-only nodes contribute their text through their children.
        let Some(text) = node.text() else { continue };
        if text.trim().is_empty() {
            continue;
        }

        let prefix = context_prefix(artifact, i);
        let kind = chunk_kind(node.kind);

        // CHK-004: a symbol is never merged with its neighbours. Splitting a
        // function in half produces two chunks neither of which is the
        // function, so a symbol starts its own chunk and ends it.
        let atomic = matches!(node.kind, IrKind::Symbol | IrKind::CodeBlock);

        // The ceiling applies to every node, not only atomic ones. Gating this
        // on `atomic` let a single huge paragraph through as one chunk — the
        // ceiling has to be a ceiling or it is only a suggestion.
        if text.len() > policy.max_bytes {
            flush(&mut pending, &mut out, artifact.provenance);
            out.extend(split_oversized(
                node,
                i,
                &prefix,
                kind,
                text,
                policy,
                artifact.provenance,
            ));
            continue;
        }

        // A node that would overflow the current chunk, or that belongs under a
        // different heading, starts a new one.
        let starts_new = match &pending {
            None => true,
            Some(p) => {
                p.prefix != prefix
                    || p.kind != kind
                    || atomic
                    || p.text.len() + text.len() > policy.target_bytes
            }
        };

        if starts_new {
            flush(&mut pending, &mut out, artifact.provenance);
            pending = Some(Pending {
                prefix,
                kind,
                text: text.to_string(),
                span: node.span.clone(),
                root: i,
            });
        } else if let Some(p) = pending.as_mut() {
            p.text.push('\n');
            p.text.push_str(text);
            p.span = merge_spans(&p.span, &node.span);
        }

        // Emit as soon as the target is met so the tail stays bounded.
        if pending
            .as_ref()
            .is_some_and(|p| p.text.len() >= policy.target_bytes)
        {
            flush(&mut pending, &mut out, artifact.provenance);
        }
    }
    flush(&mut pending, &mut out, artifact.provenance);

    merge_runts(out, policy)
}

struct Pending {
    prefix: String,
    kind: ChunkKind,
    text: String,
    span: SourceSpan,
    root: usize,
}

fn flush(pending: &mut Option<Pending>, out: &mut Vec<Chunk>, provenance: ProvenanceClass) {
    if let Some(p) = pending.take() {
        out.push(finish(p, provenance));
    }
}

fn finish(p: Pending, provenance: ProvenanceClass) -> Chunk {
    Chunk {
        text_hash: ContentHash::of(p.text.as_bytes()),
        byte_len: p.text.len(),
        context_prefix: p.prefix,
        text: p.text,
        span: p.span,
        root_node: p.root,
        kind: p.kind,
        provenance,
    }
}

/// The ancestor chain of headings and symbols above `idx` (CHK-002).
///
/// This is the breadcrumb the UI renders and the title field the index weights,
/// so it is built once here rather than recomputed at query time.
fn context_prefix(artifact: &ParsedArtifact, idx: usize) -> String {
    let mut parts = Vec::new();
    let mut cur = artifact.nodes[idx].parent;
    // Bounded: `parent` always points backwards (validated by the router), so
    // this terminates, but cap it anyway rather than trusting that at runtime.
    let mut hops = 0;
    while let Some(p) = cur {
        hops += 1;
        if hops > 64 {
            break;
        }
        let node = &artifact.nodes[p];
        if matches!(node.kind, IrKind::Heading | IrKind::Symbol) {
            if let Some(t) = node.text() {
                parts.push(first_line(t));
            }
        }
        cur = node.parent;
    }
    parts.reverse();
    parts.join(" › ")
}

fn first_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.chars().count() <= 80 {
        line.to_string()
    } else {
        line.chars().take(79).chain(std::iter::once('…')).collect()
    }
}

fn chunk_kind(k: IrKind) -> ChunkKind {
    match k {
        IrKind::Symbol | IrKind::CodeBlock => ChunkKind::Code,
        IrKind::TableCell | IrKind::TableRow | IrKind::Table => ChunkKind::TableBand,
        IrKind::Metadata => ChunkKind::Metadata,
        _ => ChunkKind::Text,
    }
}

/// Split a node that exceeds the hard ceiling on its own.
///
/// Splits on blank lines where possible so the pieces are still statements or
/// paragraphs rather than arbitrary byte ranges, and each piece keeps a span
/// that actually points at it.
fn split_oversized(
    node: &IrNode,
    idx: usize,
    prefix: &str,
    kind: ChunkKind,
    text: &str,
    policy: &ChunkPolicy,
    provenance: ProvenanceClass,
) -> Vec<Chunk> {
    let base = span_start(&node.span);
    let mut out = Vec::new();
    let mut start = 0usize;

    while start < text.len() {
        let mut end = (start + policy.max_bytes).min(text.len());

        // Land on a char boundary BEFORE slicing. Slicing first and fixing the
        // boundary afterwards panics the moment the cut lands inside a
        // multi-byte character — which on a real corpus is immediate: a box
        // drawing glyph in a Markdown table was enough.
        while end < text.len() && !text.is_char_boundary(end) {
            end -= 1;
        }

        // Then back off to a blank line, else a newline, so the pieces are
        // still paragraphs or statements rather than arbitrary byte ranges.
        if end < text.len() {
            let window = &text[start..end];
            end = window
                .rfind("\n\n")
                .map(|p| start + p + 1)
                .or_else(|| window.rfind('\n').map(|p| start + p + 1))
                .unwrap_or(end);
        }

        // A single line longer than the ceiling has no break to back off to.
        // Take the whole boundary-safe window rather than looping forever.
        if end <= start {
            end = (start + policy.max_bytes).min(text.len());
            while end < text.len() && !text.is_char_boundary(end) {
                end += 1;
            }
        }
        let piece = &text[start..end];
        if !piece.trim().is_empty() {
            out.push(Chunk {
                context_prefix: prefix.to_string(),
                text: piece.to_string(),
                span: base
                    .map(|b| SourceSpan::Bytes {
                        start: b + start as u64,
                        end: b + end as u64,
                    })
                    .unwrap_or_else(|| node.span.clone()),
                root_node: idx,
                kind,
                provenance,
                text_hash: ContentHash::of(piece.as_bytes()),
                byte_len: piece.len(),
            });
        }
        start = end;
    }
    out
}

/// Chunk one table (**TBL-011**).
///
/// Two shapes come out of here:
///
/// - **One schema chunk.** Columns, inferred types and numeric ranges. A
///   question about what a file *contains* is answered by this chunk; no band
///   of rows states its own shape.
/// - **Row bands**, each one opening with the caption, any title rows above the
///   header, and the header line. Repeating them costs a few dozen bytes per
///   band and is the difference between a band being findable by the column
///   name and being forty numbers with no nouns in them (CHK-005).
///
/// A table that failed reconstruction (TBL-018) gets neither: its text comes
/// out as an ordinary text chunk so the content stays discoverable, because a
/// grid we could not rebuild is still words somebody wrote.
fn table_chunks(
    artifact: &ParsedArtifact,
    t: &crate::table::TableIr,
    policy: &ChunkPolicy,
) -> Vec<Chunk> {
    let prefix = context_prefix(artifact, t.node);

    if !t.is_usable() {
        let text = t
            .cells
            .iter()
            .map(|c| c.raw_text.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if text.trim().is_empty() {
            return Vec::new();
        }
        return vec![Chunk {
            context_prefix: prefix,
            span: t.span.clone(),
            root_node: t.node,
            kind: ChunkKind::Text,
            provenance: t.provenance,
            text_hash: ContentHash::of(text.as_bytes()),
            byte_len: text.len(),
            text,
        }];
    }

    // Caption, then any rows above the header — an exported CSV's title row is
    // its caption in all but name.
    let mut lead = Vec::new();
    if let Some(c) = &t.caption {
        lead.push(c.clone());
    }
    for r in 0..t.header.preamble_rows {
        let row = t
            .row(r)
            .map(|c| c.raw_text.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if !row.is_empty() {
            lead.push(row);
        }
    }
    let lead = lead.join("\n");
    let header = crate::table::header_line(t);

    let mut out = Vec::new();
    out.push(finish_table_chunk(
        crate::table::schema_text(t, &lead),
        t.span.clone(),
        t.node,
        ChunkKind::TableSchema,
        &prefix,
        t.provenance,
    ));

    // The band preamble, repeated on every band.
    let mut opening = String::new();
    if !lead.is_empty() {
        opening.push_str(&lead);
        opening.push('\n');
    }
    if let Some(h) = &header {
        opening.push_str(h);
        opening.push('\n');
    }

    let mut body = String::new();
    let mut span: Option<SourceSpan> = None;

    for row in t.header.body_start()..t.n_rows {
        let line = (0..t.n_cols)
            .map(|c| {
                t.cell(row, c)
                    .map(|c| c.raw_text.trim())
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" | ");
        for c in t.row(row) {
            span = Some(match span {
                Some(s) => merge_spans(&s, &c.span),
                None => c.span.clone(),
            });
        }
        body.push_str(&line);
        body.push('\n');

        if opening.len() + body.len() >= policy.target_bytes {
            out.push(finish_table_chunk(
                format!("{opening}{body}"),
                span.take().unwrap_or_else(|| t.span.clone()),
                t.node,
                ChunkKind::TableBand,
                &prefix,
                t.provenance,
            ));
            body.clear();
        }
    }
    if !body.trim().is_empty() {
        out.push(finish_table_chunk(
            format!("{opening}{body}"),
            span.unwrap_or_else(|| t.span.clone()),
            t.node,
            ChunkKind::TableBand,
            &prefix,
            t.provenance,
        ));
    }
    out
}

fn finish_table_chunk(
    text: String,
    span: SourceSpan,
    root_node: usize,
    kind: ChunkKind,
    prefix: &str,
    provenance: ProvenanceClass,
) -> Chunk {
    Chunk {
        context_prefix: prefix.to_owned(),
        text_hash: ContentHash::of(text.as_bytes()),
        byte_len: text.len(),
        text,
        span,
        root_node,
        kind,
        provenance,
    }
}

/// Fold chunks below `min_bytes` into a neighbour sharing their context.
///
/// A 12-byte chunk cannot rank meaningfully and pollutes the result list; it is
/// better attached to the text it sits beside.
fn merge_runts(chunks: Vec<Chunk>, policy: &ChunkPolicy) -> Vec<Chunk> {
    let mut out: Vec<Chunk> = Vec::with_capacity(chunks.len());
    for c in chunks {
        // Table chunks are never merged: two small tables under one heading are
        // two tables, and a chunk holding both would cite rows from a grid it
        // does not name.
        let mergeable = !c.kind.is_table()
            && out.last().is_some_and(|prev| {
                prev.context_prefix == c.context_prefix
                    && prev.kind == c.kind
                    && prev.byte_len + c.byte_len <= policy.max_bytes
                    && (prev.byte_len < policy.min_bytes || c.byte_len < policy.min_bytes)
            });
        if mergeable {
            let prev = out.last_mut().expect("checked by `mergeable`");
            prev.text.push('\n');
            prev.text.push_str(&c.text);
            prev.span = merge_spans(&prev.span, &c.span);
            prev.byte_len = prev.text.len();
            prev.text_hash = ContentHash::of(prev.text.as_bytes());
        } else {
            out.push(c);
        }
    }
    out
}

fn span_start(s: &SourceSpan) -> Option<u64> {
    match s {
        SourceSpan::Bytes { start, .. } => Some(*start),
        _ => None,
    }
}

/// Union of two spans, when they are the same kind and orderable.
///
/// Falls back to the first: a merged chunk with a span pointing at its opening
/// is worse than one pointing at the whole region, but far better than a span
/// that points somewhere the text is not.
fn merge_spans(a: &SourceSpan, b: &SourceSpan) -> SourceSpan {
    match (a, b) {
        (SourceSpan::Bytes { start: s1, end: e1 }, SourceSpan::Bytes { start: s2, end: e2 }) => {
            SourceSpan::Bytes {
                start: (*s1).min(*s2),
                end: (*e1).max(*e2),
            }
        }
        (SourceSpan::Lines { start: s1, end: e1 }, SourceSpan::Lines { start: s2, end: e2 }) => {
            SourceSpan::Lines {
                start: (*s1).min(*s2),
                end: (*e1).max(*e2),
            }
        }
        _ => a.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::{BudgetGuard, Budgets};
    use crate::parser::{ContentParser, FileProbe, ParseInput};

    fn parse_with(p: &dyn ContentParser, name: &str, src: &str) -> ParsedArtifact {
        let probe = FileProbe::new(name, src.len() as u64);
        p.parse(ParseInput {
            bytes: src.as_bytes(),
            probe: &probe,
            budget: BudgetGuard::new(Budgets::default()),
        })
        .unwrap()
    }

    fn parse_md(src: &str) -> ParsedArtifact {
        parse_with(&crate::markdown::MarkdownParser, "t.md", src)
    }

    fn parse_rs(src: &str) -> ParsedArtifact {
        parse_with(&crate::code::CodeParser, "t.rs", src)
    }

    #[test]
    fn every_chunk_carries_a_precise_span() {
        // Invariant #1 survives chunking. A chunk whose span is `Whole` is a
        // citation that means "somewhere in this file", which is not the
        // product's promise.
        let a = parse_md("# Title\n\nSome prose here that is long enough to matter.\n");
        for c in chunk(&a, &ChunkPolicy::default()) {
            assert!(c.span.is_precise(), "imprecise span on {c:?}");
        }
    }

    #[test]
    fn chunks_carry_their_heading_chain_as_context() {
        // CHK-002. This is the breadcrumb the UI renders, so it must be built
        // from the parent chain rather than guessed from indentation.
        let a = parse_md(
            "# Authentication\n\n## Refresh token rotation\n\n\
             Tokens rotate on each use; the previous token is revoked.\n",
        );
        let chunks = chunk(&a, &ChunkPolicy::default());
        let deepest = chunks
            .iter()
            .find(|c| c.text.contains("Tokens rotate"))
            .expect("body chunk");
        assert!(
            deepest.context_prefix.contains("Authentication"),
            "prefix was {:?}",
            deepest.context_prefix
        );
        assert!(deepest.context_prefix.contains("Refresh token rotation"));
    }

    #[test]
    fn a_symbol_is_never_split_when_it_fits() {
        // CHK-004. Half a function is not a retrieval unit.
        let src = "fn refresh_token(ctx: &Ctx) -> Result<Token> {\n    \
                   let claims = decode(ctx)?;\n    Ok(mint(claims))\n}\n";
        let a = parse_rs(src);
        let chunks = chunk(&a, &ChunkPolicy::default());
        let n = chunks
            .iter()
            .filter(|c| c.text.contains("refresh_token"))
            .count();
        assert_eq!(n, 1, "symbol appeared in {n} chunks: {chunks:#?}");
    }

    #[test]
    fn an_oversized_symbol_splits_at_line_boundaries_not_mid_token() {
        let body = (0..400)
            .map(|i| format!("    let v{i} = compute({i});"))
            .collect::<Vec<_>>()
            .join("\n");
        let src = format!("fn huge() {{\n{body}\n}}\n");
        let a = parse_rs(&src);
        let chunks = chunk(&a, &ChunkPolicy::default());
        assert!(chunks.len() > 1, "should have split");
        for c in &chunks {
            assert!(c.byte_len <= ChunkPolicy::default().max_bytes + 1);
            // No piece should begin mid-identifier.
            assert!(
                !c.text.starts_with("et v") && !c.text.starts_with("ompute"),
                "split mid-token: {:?}",
                &c.text[..c.text.len().min(40)]
            );
        }
    }

    #[test]
    fn identical_text_hashes_identically() {
        // The basis for reusing embeddings across unchanged chunks (EMB-008).
        let a = parse_md("# H\n\nexactly the same body text here for hashing.\n");
        let b = parse_md("# H\n\nexactly the same body text here for hashing.\n");
        let ca = chunk(&a, &ChunkPolicy::default());
        let cb = chunk(&b, &ChunkPolicy::default());
        assert_eq!(ca.len(), cb.len());
        for (x, y) in ca.iter().zip(cb.iter()) {
            assert_eq!(x.text_hash, y.text_hash);
        }
    }

    #[test]
    fn runts_are_folded_into_their_neighbours() {
        // A list of one-word items should not produce one chunk per word.
        let a = parse_md("# H\n\n- a\n- b\n- c\n- d\n- e\n");
        let chunks = chunk(&a, &ChunkPolicy::default());
        assert!(
            chunks.len() <= 2,
            "expected runts merged, got {} chunks: {chunks:#?}",
            chunks.len()
        );
    }

    #[test]
    fn a_metadata_only_artifact_yields_no_chunks() {
        // An empty file is `ParLowYield` from the Markdown parser, so the
        // router falls through to the metadata-only terminal. That artifact has
        // one `Metadata` node with a `Whole` span and no text — there is
        // nothing to retrieve, so it must produce no chunks rather than one
        // empty one that would pollute every result list.
        let router = crate::router::ParserRouter::with_default_parsers();
        let probe = FileProbe::new("empty.md", 0);
        let a = router
            .parse(b"", &probe)
            .expect("the chain always terminates in success");
        assert_eq!(a.outcome, crate::ir::ParseOutcome::MetadataOnly);
        assert!(
            chunk(&a, &ChunkPolicy::default()).is_empty(),
            "metadata-only artifacts have nothing to chunk"
        );
    }

    #[test]
    fn whitespace_only_nodes_are_dropped() {
        let a = parse_md("# H\n\n   \n\n\t\n");
        for c in chunk(&a, &ChunkPolicy::default()) {
            assert!(!c.text.trim().is_empty());
        }
    }

    #[test]
    fn oversized_text_splits_safely_around_multibyte_characters() {
        // Found by running on the real corpus: a box-drawing glyph straddling
        // the cut point panicked `split_oversized`. Any multi-byte character
        // at the ceiling reproduces it.
        let policy = ChunkPolicy::default();
        for filler in ["─", "→", "日", "🦴", "é"] {
            // One long unbroken line, so there is no newline to back off to —
            // the case that also exercises the no-break fallback.
            let body = filler.repeat(policy.max_bytes * 2);
            let a = parse_md(&format!("# H\n\n{body}\n"));
            let chunks = chunk(&a, &policy);
            assert!(!chunks.is_empty(), "{filler}: produced nothing");
            let rejoined: String = chunks.iter().map(|c| c.text.as_str()).collect();
            assert!(
                rejoined.contains(filler),
                "{filler}: content lost in splitting"
            );
        }
    }

    #[test]
    fn a_single_unbroken_line_longer_than_the_ceiling_terminates() {
        // The loop must make progress even with no newline anywhere.
        let policy = ChunkPolicy::default();
        let a = parse_md(&format!("# H\n\n{}\n", "x".repeat(policy.max_bytes * 3)));
        let chunks = chunk(&a, &policy);
        assert!(
            chunks.len() >= 3,
            "expected several pieces, got {}",
            chunks.len()
        );
    }

    #[test]
    fn every_table_emits_a_schema_chunk_and_bands_that_repeat_the_header() {
        // TBL-011. The band is what holds the numbers; the schema chunk is what
        // a question about the table's *shape* matches against.
        let rows: String = (0..200)
            .map(|i| format!("| part-{i} | {i} | {}.50 |\n", i * 2))
            .collect();
        let a = parse_md(&format!(
            "# Stock\n\n| part | qty | price |\n|---|---|---|\n{rows}"
        ));
        let chunks = chunk(&a, &ChunkPolicy::default());

        let schema: Vec<_> = chunks
            .iter()
            .filter(|c| c.kind == ChunkKind::TableSchema)
            .collect();
        assert_eq!(schema.len(), 1, "exactly one schema chunk per table");
        assert!(
            schema[0].text.contains("qty (integer"),
            "{}",
            schema[0].text
        );
        assert!(schema[0].context_prefix.contains("Stock"));

        let bands: Vec<_> = chunks
            .iter()
            .filter(|c| c.kind == ChunkKind::TableBand)
            .collect();
        assert!(bands.len() > 1, "200 rows should band");
        for b in &bands {
            assert!(
                b.text.starts_with("part | qty | price"),
                "every band repeats the header: {:?}",
                &b.text[..b.text.len().min(40)]
            );
            assert!(b.span.is_precise());
        }
        // The rows are not also emitted as loose cell text.
        assert!(
            !chunks
                .iter()
                .any(|c| c.kind == ChunkKind::Text && c.text.contains("part-7")),
            "a table's rows must not be chunked twice"
        );
    }

    #[test]
    fn a_bands_span_covers_the_rows_it_holds() {
        let src = "| part | qty |\n|---|---|\n| bolt | 12 |\n| nut | 144 |\n";
        let a = parse_md(src);
        let band = chunk(&a, &ChunkPolicy::default())
            .into_iter()
            .find(|c| c.kind == ChunkKind::TableBand)
            .expect("a band");
        let SourceSpan::Bytes { start, end } = band.span else {
            panic!("expected a byte range, got {:?}", band.span);
        };
        let covered = &src[start as usize..end as usize];
        assert!(covered.contains("bolt"), "{covered:?}");
        assert!(covered.contains("144"), "{covered:?}");
    }

    #[test]
    fn a_table_that_failed_reconstruction_is_still_discoverable_as_text() {
        // TBL-018. One column is a list, not a table — and dropping it would
        // lose the words, which is the outcome the requirement forbids.
        let a = parse_md("| notes |\n|---|\n| shipment delayed at customs |\n");
        let chunks = chunk(&a, &ChunkPolicy::default());
        assert!(
            chunks.iter().all(|c| !c.kind.is_table()),
            "a failed reconstruction is not badged as a table"
        );
        assert!(
            chunks
                .iter()
                .any(|c| c.text.contains("shipment delayed at customs")),
            "the text survives: {chunks:#?}"
        );
    }

    #[test]
    fn a_caption_and_a_title_row_ride_on_every_band() {
        let a = parse_with(
            &crate::csv::CsvParser,
            "q.csv",
            "Quarterly results\npart,qty\nbolt,12\nnut,144\n",
        );
        for c in chunk(&a, &ChunkPolicy::default()) {
            assert!(
                c.text.contains("Quarterly results"),
                "the title row is context for every chunk of the table: {:?}",
                c.text
            );
        }
    }

    #[test]
    fn two_small_tables_under_one_heading_do_not_merge_into_one_chunk() {
        let a = parse_md(
            "# H\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n\
             text between\n\n| c | d |\n|---|---|\n| 3 | 4 |\n",
        );
        let bands: Vec<_> = chunk(&a, &ChunkPolicy::default())
            .into_iter()
            .filter(|c| c.kind == ChunkKind::TableBand)
            .collect();
        assert_eq!(bands.len(), 2, "{bands:#?}");
        assert!(bands[0].text.contains('1') && !bands[0].text.contains('3'));
    }

    #[test]
    fn merged_spans_cover_both_inputs() {
        let a = SourceSpan::Bytes { start: 10, end: 20 };
        let b = SourceSpan::Bytes { start: 30, end: 40 };
        assert_eq!(
            merge_spans(&a, &b),
            SourceSpan::Bytes { start: 10, end: 40 }
        );
    }

    #[test]
    fn context_prefix_is_bounded_against_a_long_heading() {
        let long = "x".repeat(500);
        let a = parse_md(&format!(
            "# {long}\n\nbody text long enough to be a chunk.\n"
        ));
        for c in chunk(&a, &ChunkPolicy::default()) {
            assert!(c.context_prefix.chars().count() <= 200, "prefix unbounded");
        }
    }
}
