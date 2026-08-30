//! HTML (T1) — headings, prose and **tables**.
//!
//! Added for the tables. §99.5 lists HTML alongside Markdown as a native,
//! `EXACT` table source "including `colspan`/`rowspan`", and the corpus has
//! real ones: exported spreadsheets, clipboard fragments, generated
//! documentation. Until now those files reached [`crate::text`], which indexed
//! their markup — `<td>` and `class="num"` are not content, and the number
//! between them was never a cell.
//!
//! # Why a scanner rather than an HTML5 tree builder
//!
//! The same argument as [`crate::csv`]: a DOM library gives excellent *nodes*
//! and no *positions*. `html5ever` normalizes, re-parents and synthesises
//! elements — that is what conformance means — and the byte offset a cell came
//! from does not survive it. Invariant #1 needs the offset more than it needs
//! conformance, so this is a tag scanner that hands every block the range it
//! was found at.
//!
//! What that costs, stated plainly: no implied-`<tbody>` machinery, no
//! foster-parenting of stray content out of a table, no character-encoding
//! sniffing from `<meta>` (that is [`crate::decode`]'s job here). Malformed
//! nesting is closed heuristically rather than per the spec's algorithm. For
//! the files this is aimed at — machine-generated tables — that is the whole
//! job, and when it is wrong the table is *flagged* rather than wrong-and-quiet
//! (TBL-018, via [`crate::table::Reconstruction`]).

use std::ops::Range;

use marrow_core::{Code, Error, Result, SourceSpan};

use crate::decode;
use crate::ir::{
    ArtifactBuilder, IrKind, IrNode, LineIndex, NodeAttrs, ParseOutcome, ParseWarning,
    ParsedArtifact, ParserTier,
};
use crate::parser::{ContentParser, FileProbe, ParseInput};

/// The T1 HTML parser.
#[derive(Clone, Copy, Debug, Default)]
pub struct HtmlParser;

impl HtmlParser {
    pub const ID: &'static str = "html";
    pub const VERSION: &'static str = "1";

    const EXTENSIONS: &'static [&'static str] = &["html", "htm", "xhtml"];
}

impl ContentParser for HtmlParser {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn version(&self) -> &'static str {
        Self::VERSION
    }

    fn tier(&self) -> ParserTier {
        ParserTier::T1
    }

    fn handles(&self, probe: &FileProbe) -> bool {
        probe.has_any_extension(Self::EXTENSIONS) || probe.mime_hint.as_deref() == Some("text/html")
    }

    fn parse(&self, input: ParseInput<'_>) -> Result<ParsedArtifact> {
        let decoded = decode::decode(input.bytes)?;
        let src = decoded.text.as_str();
        if src.trim().is_empty() {
            return Err(Error::new(
                Code::ParLowYield,
                "This HTML file is empty, so only its metadata is indexed.",
            ));
        }
        if !src.contains('<') {
            // Named `.html` and containing no markup. The plain-text parser is
            // a better reader of it than this one, so hand it on.
            return Err(Error::new(
                Code::ParUnsupported,
                "This file is named as HTML but contains no markup, so it is indexed as plain \
                 text instead.",
            ));
        }

        let mut b = ArtifactBuilder::new(Self::ID, Self::VERSION, self.tier(), input.budget);
        b.degrade_provenance(decoded.provenance_ceiling());
        Walker::new(src).run(&mut b)?;

        if b.node_count() == 0 {
            return Err(Error::new(
                Code::ParLowYield,
                "This HTML file contains only markup and no readable text, so only its metadata \
                 is indexed.",
            ));
        }
        if decoded.is_low_yield() {
            b.warn(ParseWarning::new(
                Code::ParLowYield,
                "This HTML file did not decode cleanly as text; the structure was still \
                 extracted. Re-save it as UTF-8 for exact provenance.",
            ));
            b.set_outcome(ParseOutcome::LowYield);
        }
        Ok(b.finish())
    }
}

// ------------------------------------------------------------------ scanner

#[derive(Debug)]
enum Token<'a> {
    Text(Range<usize>),
    Start {
        name: &'a str,
        attrs: &'a str,
        self_closing: bool,
        range: Range<usize>,
    },
    End {
        name: &'a str,
        range: Range<usize>,
    },
}

/// Elements that never have an end tag.
const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// Elements whose contents are not prose.
const RAW: &[&str] = &["script", "style", "template", "svg", "noscript"];

/// Yield tokens with source ranges. Never allocates for the document.
fn tokenize<'a>(src: &'a str, mut on: impl FnMut(Token<'a>) -> Result<()>) -> Result<()> {
    let b = src.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        let Some(lt) = src[i..].find('<').map(|p| i + p) else {
            on(Token::Text(i..src.len()))?;
            break;
        };
        if lt > i {
            on(Token::Text(i..lt))?;
        }
        let rest = &b[lt + 1..];
        match rest.first() {
            // Comment, CDATA or doctype. Skipped: a comment is not content, and
            // `<!--` is also how half the web hides markup from old browsers.
            Some(b'!') => {
                i = if src[lt..].starts_with("<!--") {
                    src[lt + 4..]
                        .find("-->")
                        .map(|p| lt + 4 + p + 3)
                        .unwrap_or(src.len())
                } else {
                    tag_end(src, lt)
                };
            }
            Some(b'/') => {
                let end = tag_end(src, lt);
                let name = name_at(src, lt + 2);
                if !name.is_empty() {
                    on(Token::End {
                        name,
                        range: lt..end,
                    })?;
                }
                i = end;
            }
            Some(c) if c.is_ascii_alphabetic() => {
                let end = tag_end(src, lt);
                let name = name_at(src, lt + 1);
                let after_name = lt + 1 + name.len();
                let close = end.saturating_sub(1).max(after_name);
                let attrs = src.get(after_name..close).unwrap_or("");
                let self_closing = attrs.trim_end().ends_with('/') || VOID.contains(&name);
                if RAW.contains(&name) && !self_closing {
                    // Skip the element's contents wholesale.
                    let needle = format!("</{name}");
                    i = src[end..]
                        .to_ascii_lowercase()
                        .find(&needle)
                        .map(|p| tag_end(src, end + p))
                        .unwrap_or(src.len());
                    continue;
                }
                on(Token::Start {
                    name,
                    attrs,
                    self_closing,
                    range: lt..end,
                })?;
                i = end;
            }
            // A bare `<` in text. Not markup; keep it as content.
            _ => {
                on(Token::Text(lt..lt + 1))?;
                i = lt + 1;
            }
        }
    }
    Ok(())
}

/// Byte just past the `>` that closes the tag starting at `lt`.
fn tag_end(src: &str, lt: usize) -> usize {
    let b = src.as_bytes();
    let mut i = lt + 1;
    let mut quote = 0u8;
    while i < b.len() {
        match b[i] {
            q @ (b'"' | b'\'') if quote == 0 => quote = q,
            q if q == quote => quote = 0,
            b'>' if quote == 0 => return i + 1,
            _ => {}
        }
        i += 1;
    }
    src.len()
}

/// The lowercase-insensitive tag name at `at`. Returned as a slice of `src`, so
/// comparisons are `eq_ignore_ascii_case` rather than allocating.
fn name_at(src: &str, at: usize) -> &str {
    let b = src.as_bytes();
    let mut end = at;
    while end < b.len() && (b[end].is_ascii_alphanumeric() || b[end] == b'-') {
        end += 1;
    }
    src.get(at..end).unwrap_or("")
}

/// One attribute's value, unquoted and entity-decoded.
///
/// The value comes back **lowercased**, because the search runs over a
/// lowercased copy. That is fine for the two attributes this parser reads —
/// `rowspan` and `colspan` are integers — and it is why nothing else should
/// use this without fixing that first.
fn attr(attrs: &str, want: &str) -> Option<String> {
    let lower = attrs.to_ascii_lowercase();
    let mut from = 0usize;
    while let Some(p) = lower[from..].find(want) {
        let at = from + p;
        // Must be preceded by whitespace or start-of-string, and followed by
        // `=`, so `rowspan` does not match inside `data-rowspan`.
        let before_ok = at == 0 || lower.as_bytes()[at - 1].is_ascii_whitespace();
        let after = lower[at + want.len()..].trim_start();
        if before_ok && after.starts_with('=') {
            let v = after[1..].trim_start();
            let value = match v.as_bytes().first() {
                Some(q @ (b'"' | b'\'')) => {
                    let q = *q as char;
                    v[1..].split(q).next().unwrap_or("")
                }
                _ => v.split_whitespace().next().unwrap_or(""),
            };
            return Some(decode_entities(value));
        }
        from = at + want.len();
    }
    None
}

/// Decode the entity set that actually turns up in table cells.
///
/// Not the full HTML5 named-character-reference table (2,231 entries, and a
/// binary-search trie to go with it). An unknown entity is left as written,
/// which is legible and honest, rather than dropped.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        let Some(semi) = tail[..tail.len().min(12)].find(';') else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let name = &tail[1..semi];
        let replacement = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" | "#160" => Some(' '),
            "mdash" => Some('—'),
            "ndash" => Some('–'),
            "hellip" => Some('…'),
            "times" => Some('×'),
            _ => name
                .strip_prefix('#')
                .and_then(|n| match n.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => n.parse::<u32>().ok(),
                })
                .and_then(char::from_u32),
        };
        match replacement {
            Some(c) => out.push(c),
            None => out.push_str(&tail[..=semi]),
        }
        rest = &tail[semi + 1..];
    }
    out.push_str(rest);
    out
}

// ------------------------------------------------------------------- walker

/// A block whose text is collected until its end tag.
#[derive(Debug)]
struct Open {
    kind: IrKind,
    name: String,
    range: Range<usize>,
    text: String,
    attrs: NodeAttrs,
    /// A `<caption>`: emitted as the table's own child so
    /// [`crate::table::TableIr::caption`] can find it.
    caption_of: Option<usize>,
}

/// One `<table>` being walked. A stack of these, because tables nest.
#[derive(Debug)]
struct TableCtx {
    idx: usize,
    row: Option<usize>,
    row_no: u32,
    col_cursor: u32,
    /// Remaining rows each column is occupied for by an earlier `rowspan`.
    carry: Vec<u32>,
    header: Vec<String>,
    in_head: bool,
    open_row: bool,
}

struct Walker<'a> {
    src: &'a str,
    lines: LineIndex,
    headings: Vec<(u8, usize)>,
    open: Vec<Open>,
    tables: Vec<TableCtx>,
    /// Text found outside any block, and where it came from.
    loose: Option<(Range<usize>, String)>,
    /// End of the token last seen, so a container's span can be widened to its
    /// closing tag rather than stopping at its opening one.
    pos: usize,
}

/// Flow containers. Not blocks themselves — they hold no text of their own —
/// but their boundaries end a block that the document forgot to close.
const CONTAINERS: &[&str] = &[
    "div", "section", "article", "main", "aside", "header", "footer", "nav", "ul", "ol", "dl",
    "form", "figure", "details", "body", "html", "table", "tr", "thead", "tbody", "tfoot", "hr",
];

/// Elements we turn into blocks, and what kind of node they become.
fn block_kind(name: &str) -> Option<IrKind> {
    match name {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => Some(IrKind::Heading),
        "p" | "li" | "blockquote" | "dd" | "dt" | "figcaption" | "caption" | "pre" | "summary" => {
            Some(IrKind::Paragraph)
        }
        "td" | "th" => Some(IrKind::TableCell),
        _ => None,
    }
}

/// Blocks that real documents leave unclosed.
fn auto_closes(open: &str, incoming: &str) -> bool {
    match open {
        "p" => incoming != "a" && incoming != "span" && incoming != "em" && incoming != "strong",
        "li" => matches!(incoming, "li" | "ul" | "ol"),
        "td" | "th" => matches!(incoming, "td" | "th" | "tr" | "tbody" | "thead" | "tfoot"),
        "dd" | "dt" => matches!(incoming, "dd" | "dt" | "dl"),
        _ => false,
    }
}

impl<'a> Walker<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src,
            lines: LineIndex::new(src),
            headings: Vec::new(),
            open: Vec::new(),
            tables: Vec::new(),
            loose: None,
            pos: 0,
        }
    }

    fn run(&mut self, b: &mut ArtifactBuilder) -> Result<()> {
        // Collected first so the borrow of `self` in the callback does not
        // fight the mutation. The document is already in memory; the tokens are
        // ranges and short name slices, not copies of it.
        let mut tokens: Vec<Token<'a>> = Vec::new();
        tokenize(self.src, |t| {
            tokens.push(t);
            Ok(())
        })?;

        let mut lowered: Vec<String> = Vec::with_capacity(tokens.len());
        for t in &tokens {
            lowered.push(match t {
                Token::Start { name, .. } | Token::End { name, .. } => name.to_ascii_lowercase(),
                Token::Text(_) => String::new(),
            });
        }

        for (t, name) in tokens.iter().zip(lowered.iter()) {
            b.budget().check_time()?;
            match t {
                Token::Text(range) => {
                    self.pos = range.end;
                    self.text(range.clone());
                }
                Token::Start {
                    attrs,
                    self_closing,
                    range,
                    ..
                } => {
                    self.pos = range.end;
                    self.start(name, attrs, range.clone(), b)?;
                    // A void element opens no block, so there is nothing to
                    // close. `<br>` is the one that carries meaning.
                    if *self_closing && name == "br" {
                        self.push_text("\n");
                    }
                }
                Token::End { range, .. } => {
                    self.pos = range.end;
                    self.end(name, b)?;
                }
            }
        }

        // Close whatever the document left open.
        while !self.open.is_empty() {
            self.close_block(b)?;
        }
        self.flush_loose(b)?;
        Ok(())
    }

    fn start(
        &mut self,
        name: &str,
        attrs: &str,
        range: Range<usize>,
        b: &mut ArtifactBuilder,
    ) -> Result<()> {
        // Implicit closes first: `<p>a<p>b` is two paragraphs, not one.
        while self.open.last().is_some_and(|o| auto_closes(&o.name, name)) {
            self.close_block(b)?;
        }

        match name {
            "table" => {
                self.flush_loose(b)?;
                let node = IrNode::structural(IrKind::Table, bytes(&range))
                    .with_attrs(NodeAttrs::default().with_lines(&self.lines, &range));
                let parent = self.current_parent();
                let idx = b.push(parent, node)?;
                self.tables.push(TableCtx {
                    idx,
                    row: None,
                    row_no: 0,
                    col_cursor: 0,
                    carry: Vec::new(),
                    header: Vec::new(),
                    in_head: false,
                    open_row: false,
                });
            }
            "thead" => {
                if let Some(t) = self.tables.last_mut() {
                    t.in_head = true;
                }
            }
            "tbody" | "tfoot" => {
                if let Some(t) = self.tables.last_mut() {
                    t.in_head = false;
                }
            }
            "tr" => {
                self.end_row(b)?;
                let Some(t) = self.tables.last_mut() else {
                    return Ok(());
                };
                let node =
                    IrNode::structural(IrKind::TableRow, bytes(&range)).with_attrs(NodeAttrs {
                        row: Some(t.row_no),
                        ..NodeAttrs::default().with_lines(&self.lines, &range)
                    });
                let table = t.idx;
                t.col_cursor = 0;
                t.open_row = true;
                let row = b.push(Some(table), node)?;
                if let Some(t) = self.tables.last_mut() {
                    t.row = Some(row);
                }
            }
            _ => {
                let Some(kind) = block_kind(name) else {
                    return Ok(());
                };
                self.flush_loose(b)?;
                let mut node_attrs = NodeAttrs::default();
                let mut caption_of = None;
                match kind {
                    IrKind::Heading => {
                        node_attrs.level = name.as_bytes().get(1).map(|c| c - b'0');
                    }
                    IrKind::TableCell => {
                        let Some(t) = self.tables.last_mut() else {
                            // A `<td>` outside a table. Keep the text as prose
                            // rather than losing it.
                            self.open_block(IrKind::Paragraph, name, range, NodeAttrs::default());
                            return Ok(());
                        };
                        let rowspan = attr(attrs, "rowspan")
                            .and_then(|v| v.trim().parse::<u32>().ok())
                            .unwrap_or(1)
                            .clamp(1, MAX_SPAN);
                        let colspan = attr(attrs, "colspan")
                            .and_then(|v| v.trim().parse::<u32>().ok())
                            .unwrap_or(1)
                            .clamp(1, MAX_SPAN);
                        // Step over columns an earlier `rowspan` still occupies,
                        // so a cell's column index is where it renders.
                        while t.carry.get(t.col_cursor as usize).copied().unwrap_or(0) > 0 {
                            t.col_cursor += 1;
                        }
                        let col = t.col_cursor;
                        let needed = (col + colspan) as usize;
                        if t.carry.len() < needed {
                            t.carry.resize(needed, 0);
                        }
                        for c in col..col + colspan {
                            t.carry[c as usize] = rowspan;
                        }
                        t.col_cursor = col + colspan;
                        node_attrs.row = Some(t.row_no);
                        node_attrs.col = Some(col);
                        node_attrs.rowspan = Some(rowspan);
                        node_attrs.colspan = Some(colspan);
                        node_attrs.column_name = t.header.get(col as usize).cloned();
                    }
                    _ if name == "caption" => {
                        caption_of = self.tables.last().map(|t| t.idx);
                    }
                    _ => {}
                }
                self.open_block_with(kind, name, range, node_attrs, caption_of);
            }
        }
        Ok(())
    }

    fn end(&mut self, name: &str, b: &mut ArtifactBuilder) -> Result<()> {
        match name {
            "table" => {
                self.end_row(b)?;
                while self
                    .open
                    .last()
                    .is_some_and(|o| o.kind == IrKind::TableCell)
                {
                    self.close_block(b)?;
                }
                if let Some(t) = self.tables.pop() {
                    // A table is not its opening tag. Its span is the region.
                    b.widen_span(t.idx, self.pos as u64);
                }
            }
            "tr" => self.end_row(b)?,
            "thead" => {
                self.end_row(b)?;
                if let Some(t) = self.tables.last_mut() {
                    t.in_head = false;
                }
            }
            "tbody" | "tfoot" => self.end_row(b)?,
            _ => {
                if block_kind(name).is_some() {
                    // Close up to and including the matching open block. An
                    // unmatched end tag closes nothing.
                    if self.open.iter().any(|o| o.name == name) {
                        while self.open.last().is_some_and(|o| o.name != name) {
                            self.close_block(b)?;
                        }
                        self.close_block(b)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Finish the row in progress: close its cells and advance the row counter.
    fn end_row(&mut self, b: &mut ArtifactBuilder) -> Result<()> {
        while self
            .open
            .last()
            .is_some_and(|o| o.kind == IrKind::TableCell)
        {
            self.close_block(b)?;
        }
        let pos = self.pos as u64;
        let Some(t) = self.tables.last_mut() else {
            return Ok(());
        };
        if !t.open_row {
            return Ok(());
        }
        for c in &mut t.carry {
            *c = c.saturating_sub(1);
        }
        let row = t.row.take();
        t.row_no += 1;
        t.open_row = false;
        t.in_head = false;
        if let Some(row) = row {
            b.widen_span(row, pos);
        }
        Ok(())
    }

    fn text(&mut self, range: Range<usize>) {
        let raw = self.src.get(range.clone()).unwrap_or("");
        if raw.trim().is_empty() {
            // Inter-tag whitespace. Kept only inside an open block, where it
            // separates words.
            if self.open.last().is_some() && raw.contains(char::is_whitespace) {
                self.push_text(" ");
            }
            return;
        }
        let text = decode_entities(raw);
        if self.open.last().is_some() {
            self.push_text(&text);
            return;
        }
        // Loose text: `<div>` and `<span>` are not blocks, and a document that
        // never opens a `<p>` would otherwise index nothing at all.
        match &mut self.loose {
            Some((r, s)) => {
                r.end = range.end;
                s.push_str(&text);
            }
            None => self.loose = Some((range, text)),
        }
    }

    fn push_text(&mut self, t: &str) {
        if let Some(o) = self.open.last_mut() {
            o.text.push_str(t);
        }
    }

    fn open_block(&mut self, kind: IrKind, name: &str, range: Range<usize>, attrs: NodeAttrs) {
        self.open_block_with(kind, name, range, attrs, None);
    }

    fn open_block_with(
        &mut self,
        kind: IrKind,
        name: &str,
        range: Range<usize>,
        attrs: NodeAttrs,
        caption_of: Option<usize>,
    ) {
        self.open.push(Open {
            kind,
            name: name.to_owned(),
            range,
            text: String::new(),
            attrs,
            caption_of,
        });
    }

    fn close_block(&mut self, b: &mut ArtifactBuilder) -> Result<()> {
        let Some(done) = self.open.pop() else {
            return Ok(());
        };
        // A table cell is kept even when empty: its square is part of the grid
        // and its position is real. Anything else empty is markup, not content.
        if done.text.trim().is_empty() && done.kind != IrKind::TableCell {
            return Ok(());
        }
        // Nested blocks contribute their text upwards as well, so a paragraph
        // inside a cell reads as the cell's content.
        if let Some(parent) = self.open.last_mut() {
            if parent.kind == IrKind::TableCell && done.kind != IrKind::TableCell {
                if !parent.text.is_empty() {
                    parent.text.push(' ');
                }
                parent.text.push_str(done.text.trim());
                return Ok(());
            }
        }
        self.emit(done, b)?;
        Ok(())
    }

    fn emit(&mut self, done: Open, b: &mut ArtifactBuilder) -> Result<usize> {
        let range = self.body_range(&done);
        let (text, clipped) = b.budget().clamp_text(collapse(&done.text).trim());
        let attrs = done.attrs.clone().with_lines(&self.lines, &range);
        let level = attrs.level;

        let parent = if let Some(table) = done.caption_of {
            Some(table)
        } else if done.kind == IrKind::Heading {
            let level = level.unwrap_or(1);
            while self.headings.last().is_some_and(|(l, _)| *l >= level) {
                self.headings.pop();
            }
            self.headings.last().map(|(_, i)| *i)
        } else if done.kind == IrKind::TableCell {
            self.tables
                .last()
                .and_then(|t| t.row)
                .or(self.current_parent())
        } else {
            self.current_parent()
        };

        let node = IrNode::content_in(done.kind, self.src, range, text.clone())?.with_attrs(attrs);
        let idx = b.push(parent, node)?;

        if clipped {
            b.warn(ParseWarning::new(
                Code::ParTruncated,
                "A block in this file was larger than the per-node text budget and its text was \
                 clipped. The byte span still covers the whole block.",
            ));
            b.set_outcome(ParseOutcome::Partial);
        }
        if done.kind == IrKind::Heading {
            self.headings.push((level.unwrap_or(1), idx));
        }
        // Header cells name the columns below them, which is what makes a body
        // cell worth retrieving (CHK-002's analogue for tables).
        if done.kind == IrKind::TableCell {
            if let Some(t) = self.tables.last_mut() {
                if t.in_head || t.row_no == 0 {
                    let col = done.attrs.col.unwrap_or(0) as usize;
                    if t.header.len() <= col {
                        t.header.resize(col + 1, String::new());
                    }
                    t.header[col] = text;
                }
            }
        }
        Ok(idx)
    }

    /// The block's content range — inside the start tag, up to the end tag.
    ///
    /// Trimmed to the text it holds so `&src[span]` is the cell, not the cell
    /// plus its markup. `is_verbatim` is still false whenever entities were
    /// decoded or inner tags stripped, which is exactly what it is for.
    fn body_range(&self, done: &Open) -> Range<usize> {
        let start = done.range.end.min(self.src.len());
        // The block ends where the next thing began; the tokenizer does not
        // hand that back, so take everything from the start tag to the end of
        // the collected text's last occurrence — bounded by the document.
        let end = self.next_boundary(start);
        trim_range(self.src, start..end.max(start))
    }

    /// End of the current block's content: the next `<`, walked forward over
    /// inline tags until one that closes or opens a block.
    fn next_boundary(&self, from: usize) -> usize {
        let mut i = from;
        loop {
            let Some(p) = self.src[i..].find('<').map(|p| i + p) else {
                return self.src.len();
            };
            let after = &self.src[p + 1..];
            let name_start = match after.as_bytes().first() {
                Some(b'/') => p + 2,
                _ => p + 1,
            };
            let name = name_at(self.src, name_start).to_ascii_lowercase();
            // Inline markup — `<em>`, `<a>`, `<span>` — is *inside* the block,
            // so it must not end it. Only a block or a flow container does.
            if block_kind(&name).is_some() || CONTAINERS.contains(&name.as_str()) {
                return p;
            }
            i = tag_end(self.src, p);
        }
    }

    fn current_parent(&self) -> Option<usize> {
        self.tables
            .last()
            .map(|t| t.row.unwrap_or(t.idx))
            .or_else(|| self.headings.last().map(|(_, i)| *i))
    }

    fn flush_loose(&mut self, b: &mut ArtifactBuilder) -> Result<()> {
        let Some((range, text)) = self.loose.take() else {
            return Ok(());
        };
        let text = collapse(&text);
        if text.trim().is_empty() {
            return Ok(());
        }
        let range = trim_range(self.src, range);
        let (text, _) = b.budget().clamp_text(text.trim());
        let node = IrNode::content_in(IrKind::Paragraph, self.src, range.clone(), text)?
            .with_attrs(NodeAttrs::default().with_lines(&self.lines, &range));
        let parent = self.current_parent();
        b.push(parent, node)?;
        Ok(())
    }
}

/// A `rowspan="1000000"` is a denial-of-service dressed as a table.
const MAX_SPAN: u32 = 1000;

fn bytes(range: &Range<usize>) -> SourceSpan {
    SourceSpan::Bytes {
        start: range.start as u64,
        end: range.end as u64,
    }
}

fn trim_range(src: &str, range: Range<usize>) -> Range<usize> {
    let Some(s) = src.get(range.clone()) else {
        return range;
    };
    let start = range.start + (s.len() - s.trim_start().len());
    let end = range.start + s.trim_end().len();
    start.min(end)..end
}

/// Collapse runs of whitespace, which HTML treats as one space anyway.
fn collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c.is_whitespace() && c != '\n' {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::{BudgetGuard, Budgets};
    use crate::table::{tables_in, ColumnType, Reconstruction};

    fn parse(src: &str) -> ParsedArtifact {
        let probe = FileProbe::new("page.html", src.len() as u64);
        HtmlParser
            .parse(ParseInput {
                bytes: src.as_bytes(),
                probe: &probe,
                budget: BudgetGuard::new(Budgets::default()),
            })
            .expect("fixture must parse")
    }

    #[test]
    fn an_html_table_becomes_the_same_table_ir_as_a_csv() {
        let a = parse(
            "<html><body><h1>Stock</h1><table>\
             <tr><th>part</th><th>qty</th></tr>\
             <tr><td>bolt</td><td>12</td></tr>\
             <tr><td>nut</td><td>144</td></tr>\
             </table></body></html>",
        );
        a.validate().unwrap();
        let t = &tables_in(&a)[0];
        assert_eq!((t.n_rows, t.n_cols), (3, 2));
        assert_eq!(t.header.row, Some(0));
        assert_eq!(t.column_names, vec!["part", "qty"]);
        assert_eq!(
            t.column_types,
            vec![ColumnType::String, ColumnType::Integer]
        );
        assert_eq!(t.reconstruction, Reconstruction::Exact);
        assert_eq!(t.extraction_method, "native_html");
    }

    #[test]
    fn every_cell_span_resolves_to_the_cell_in_the_markup() {
        // TBL-002 for HTML: the byte range is the `<td>`'s content, not the DOM
        // path, because the byte range is a place the file actually has.
        let src = "<table><tr><th>a</th><th>b</th></tr><tr><td>1</td><td>2</td></tr></table>";
        let a = parse(src);
        let t = &tables_in(&a)[0];
        for c in &t.cells {
            let SourceSpan::Bytes { start, end } = c.span else {
                panic!("expected a byte range, got {:?}", c.span);
            };
            assert_eq!(&src[start as usize..end as usize], c.raw_text);
        }
    }

    #[test]
    fn colspan_and_rowspan_are_preserved_and_shift_the_columns_below() {
        // TBL-004. The `region` cell owns two rows, so `north` in the second
        // row is column 1, not column 0.
        let a = parse(
            "<table>\
             <tr><th>region</th><th>site</th><th>units</th></tr>\
             <tr><td rowspan=\"2\">EU</td><td>north</td><td>10</td></tr>\
             <tr><td>south</td><td>20</td></tr>\
             </table>",
        );
        let t = &tables_in(&a)[0];
        assert_eq!(t.merged_regions.len(), 1);
        assert_eq!(t.merged_regions[0].rowspan, 2);
        assert_eq!(t.cell(2, 1).unwrap().raw_text, "south");
        assert!(t.cell(2, 0).is_none(), "covered by the rowspan above");
        // Every square is accounted for, by a cell or by a span.
        assert_eq!(t.reconstruction, Reconstruction::Exact);
    }

    #[test]
    fn a_caption_is_the_tables_caption_and_not_a_stray_paragraph() {
        let a = parse(
            "<table><caption>Units shipped</caption>\
             <tr><th>a</th><th>b</th></tr><tr><td>1</td><td>2</td></tr></table>",
        );
        let t = &tables_in(&a)[0];
        assert_eq!(t.caption.as_deref(), Some("Units shipped"));
    }

    #[test]
    fn entities_are_decoded_and_the_span_still_covers_the_source() {
        let src = "<table><tr><th>name</th><th>co</th></tr>\
                   <tr><td>R&amp;D</td><td>10&nbsp;%</td></tr></table>";
        let a = parse(src);
        let t = &tables_in(&a)[0];
        let cell = t.cell(1, 0).unwrap();
        assert_eq!(cell.raw_text, "R&D");
        let SourceSpan::Bytes { start, end } = cell.span else {
            unreachable!()
        };
        assert_eq!(&src[start as usize..end as usize], "R&amp;D");
    }

    #[test]
    fn script_and_style_contents_are_not_content() {
        let a = parse(
            "<html><head><style>td { color: red }</style></head>\
             <body><script>var x = \"hello\";</script><p>Real text.</p></body></html>",
        );
        let texts: Vec<&str> = a.nodes.iter().filter_map(|n| n.text()).collect();
        assert!(texts.iter().any(|t| t.contains("Real text")));
        assert!(
            !texts
                .iter()
                .any(|t| t.contains("var x") || t.contains("color")),
            "{texts:?}"
        );
    }

    #[test]
    fn headings_chain_so_a_table_carries_its_section() {
        let a = parse("<h1>One</h1><h2>Two</h2><p>Body text here.</p>");
        let headings: Vec<usize> = a
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.kind == IrKind::Heading)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(headings.len(), 2);
        assert_eq!(a.nodes[headings[1]].parent, Some(headings[0]));
        let para = a
            .nodes
            .iter()
            .find(|n| n.kind == IrKind::Paragraph)
            .unwrap();
        assert_eq!(para.parent, Some(headings[1]));
    }

    #[test]
    fn unclosed_paragraphs_and_cells_still_produce_separate_nodes() {
        let a = parse("<p>first<p>second<table><tr><td>a<td>b<tr><td>c<td>d</table>");
        let paras = a
            .nodes
            .iter()
            .filter(|n| n.kind == IrKind::Paragraph)
            .count();
        assert_eq!(paras, 2, "two paragraphs, not one run-on");
        let t = &tables_in(&a)[0];
        assert_eq!((t.n_rows, t.n_cols), (2, 2));
        assert_eq!(t.cell(1, 1).unwrap().raw_text, "d");
    }

    #[test]
    fn a_file_named_html_with_no_markup_is_handed_on() {
        let probe = FileProbe::new("notes.html", 5);
        let e = HtmlParser
            .parse(ParseInput {
                bytes: b"plain",
                probe: &probe,
                budget: BudgetGuard::new(Budgets::default()),
            })
            .unwrap_err();
        assert_eq!(e.code(), Code::ParUnsupported);
    }

    #[test]
    fn text_outside_any_block_is_still_indexed() {
        let a = parse("<div>Loose but real text.</div>");
        assert!(a
            .nodes
            .iter()
            .any(|n| n.text().is_some_and(|t| t.contains("Loose but real"))));
    }

    #[test]
    fn attributes_are_matched_whole_not_as_substrings() {
        assert_eq!(
            attr(" data-rowspan=\"9\" rowspan=\"2\"", "rowspan").as_deref(),
            Some("2")
        );
        assert_eq!(attr(" colspan=3 ", "colspan").as_deref(), Some("3"));
        assert_eq!(attr(" class=\"x\"", "rowspan"), None);
    }

    #[test]
    fn a_hostile_rowspan_cannot_allocate_the_process_to_death() {
        let a = parse("<table><tr><td rowspan=\"99999999\">x</td><td>y</td></tr></table>");
        let t = &tables_in(&a)[0];
        assert_eq!(t.cells[0].rowspan, MAX_SPAN);
    }
}
