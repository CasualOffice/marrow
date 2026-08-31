//! DOCX (T2) — WordprocessingML tables and prose, cited by XML path.
//!
//! §99.5: `DOCX / PPTX | Native OOXML | EXACT — XML path | Table structure is
//! explicit in the format`. It is explicit — `w:tbl` / `w:tr` / `w:tc` is
//! exactly the Table/Row/Cell shape [`crate::table`] already derives an IR
//! from, so this parser has no table logic of its own beyond reading the
//! element names.
//!
//! # Why the span is an XML path and not a byte range
//!
//! Everything before this file could point at bytes because the bytes existed.
//! A `.docx` is deflated XML inside a zip: offset 4,182 of `word/document.xml`
//! is an offset into a stream that is nowhere on disk, and no editor, tool or
//! human can be taken to it. Recording it would satisfy the `source_span`
//! rule in form and break it in substance.
//!
//! `/word/document.xml#/w:document/w:body/w:tbl[2]/w:tr[3]/w:tc[1]` is an
//! address the file really has: unzip the part and any XPath engine resolves
//! it, Word's own object model uses the same tree, and it survives the document
//! being re-saved with different compression. The part name is in the path
//! because tables live in footers and footnotes too, and a path without it
//! would be ambiguous the moment that becomes true.
//!
//! Indices are 1-based per sibling name, as XPath is — `w:tc[1]` is the first
//! `w:tc` child, not the first child.
//!
//! # Why the XML is read directly rather than through a DOCX crate
//!
//! The same argument [`crate::csv`] makes about the `csv` crate and
//! [`crate::html`] makes about tree builders, and it lands harder here: the
//! available DOCX crates (`docx-rs`, `docx-rust`) deserialize a document into
//! typed structs, and the *position in the tree* — the only thing this parser
//! needs — is precisely what deserialization discards. Rebuilding a path from a
//! typed tree means walking it in parallel with a second reader that has to
//! agree about element ordering, which is two parsers where one will do.
//!
//! `quick-xml` is a streaming reader, so the path is simply the stack, and the
//! stack is free.
//!
//! # What this reads, and what it does not
//!
//! `word/document.xml` only. Headers, footers, footnotes, endnotes and comments
//! are separate parts that also contain tables, and none of the corpus has
//! asked yet — adding one is a second call to [`ooxml::read_part`] and the same
//! walker. Field codes, revision marks and content controls are ignored:
//! `w:t` runs are the text, which is what Word itself would put on a clipboard.
//!
//! Prose is emitted alongside the tables. Not scope creep but its absence would
//! be: a parser that claims `.docx`, indexes its two tables and silently
//! discards forty pages of text is worse than no parser at all, because the
//! router would stop at it and the file would never reach the metadata-only
//! terminal that at least says "nothing was read".

use std::collections::HashMap;

use marrow_core::{Code, Error, Result, SourceSpan};
use quick_xml::escape::unescape;
use quick_xml::events::Event;
use quick_xml::Reader;

use crate::ir::{
    ArtifactBuilder, IrKind, IrNode, NodeAttrs, ParseOutcome, ParseWarning, ParsedArtifact,
    ParserTier,
};
use crate::ooxml;
use crate::parser::{ContentParser, FileProbe, ParseInput};

/// The part this parser reads.
const BODY_PART: &str = "word/document.xml";

/// XML elements descended into before the rest is treated as a leaf.
///
/// Word nests: a table in a cell in a table in a text box. `max_depth` in
/// [`crate::budget`] is the hard stop for the *arena*; this is the stop for the
/// walker, and it is lower because a document 32 tables deep is a fuzzer's
/// output rather than a report.
const MAX_XML_DEPTH: usize = 32;

/// Tables read from one document.
const MAX_TABLES: usize = 512;

/// Rows read from one table, and cells read from one row. Word's own limits are
/// far higher; these are the point past which the file is a database.
const MAX_ROWS_PER_TABLE: usize = 5_000;
const MAX_CELLS_PER_ROW: usize = 512;

/// A `w:gridSpan` or a vertical merge run wider than this is a denial-of-
/// service dressed as a table, exactly as `rowspan="1000000"` is in HTML.
const MAX_SPAN: u32 = 1_000;

/// The T2 WordprocessingML parser.
#[derive(Clone, Copy, Debug, Default)]
pub struct DocxParser;

impl DocxParser {
    pub const ID: &'static str = "docx";
    pub const VERSION: &'static str = "1";

    /// `doc` is deliberately absent: the pre-2007 binary format is a different
    /// container entirely and no file has asked.
    const EXTENSIONS: &'static [&'static str] = &["docx", "docm"];
}

impl ContentParser for DocxParser {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn version(&self) -> &'static str {
        Self::VERSION
    }

    fn tier(&self) -> ParserTier {
        // T2: native, structural provenance — "XML path" is the example Part 3
        // §63 gives for the tier.
        ParserTier::T2
    }

    fn handles(&self, probe: &FileProbe) -> bool {
        probe.has_any_extension(Self::EXTENSIONS)
    }

    fn parse(&self, input: ParseInput<'_>) -> Result<ParsedArtifact> {
        // FS-014: confirm against the bytes, not the name.
        if !ooxml::looks_like_zip(input.bytes) {
            return Err(Error::new(
                Code::ParUnsupported,
                "This file is named as a Word document but is not a zip archive, so another \
                 parser was tried instead.",
            ));
        }
        let preflight = ooxml::preflight(input.bytes)?;
        let Some(xml) = ooxml::read_part(input.bytes, BODY_PART)? else {
            return Err(Error::new(
                Code::ParCorrupt,
                "This file is a zip archive but has no `word/document.xml`, so it is not a Word \
                 document despite its name. It stays findable by name.",
            ));
        };

        let mut b = ArtifactBuilder::new(Self::ID, Self::VERSION, self.tier(), input.budget);
        if preflight.suspicious_names > 0 {
            b.warn(ParseWarning::new(
                Code::FsPathEscapeBlocked,
                "This document contains archive entries whose names point outside the archive. \
                 Nothing was extracted to disk, so the file is safe to index, but treat its \
                 origin with suspicion.",
            ));
        }

        let body = Walker::new(&xml).run(&mut b)?;
        emit(&body, None, &mut b)?;

        let mut artifact = b.finish();
        if artifact.nodes.is_empty() {
            // Nothing to index. Returning an `Err` here would be the obvious
            // move and it silently drops every warning the builder collected —
            // including "this archive contains an entry that escapes the archive
            // root", which is the one finding that must never be lost because the
            // file has nothing else to say. A metadata-only artifact is the shape
            // that carries both facts: PAR-013's "still a file", and why.
            artifact.warnings.push(ParseWarning::new(
                Code::ParLowYield,
                "This document contains no readable text, so only its metadata is indexed.",
            ));
            return Ok(ParsedArtifact::metadata_only(artifact.warnings));
        }
        Ok(artifact)
    }
}

// ------------------------------------------------------------------- drafts

/// A node before it reaches the arena.
///
/// The walk builds a tree and a second pass flattens it, because a cell's text
/// is only known at `</w:tc>` while its children have to be pushed after it —
/// [`ArtifactBuilder::push`] needs the parent's index, so the parent must exist
/// first. Emitting on the closing tag would invert that; a draft tree is the
/// smaller of the two prices.
#[derive(Debug)]
struct Draft {
    kind: IrKind,
    path: String,
    /// `None` for structural nodes (a table, a row); `Some` for anything
    /// carrying file text.
    text: Option<String>,
    attrs: NodeAttrs,
    children: Vec<Draft>,
}

impl Draft {
    fn structural(kind: IrKind, path: String) -> Self {
        Self {
            kind,
            path,
            text: None,
            attrs: NodeAttrs::default(),
            children: Vec::new(),
        }
    }
}

fn emit(drafts: &[Draft], parent: Option<usize>, b: &mut ArtifactBuilder) -> Result<()> {
    for d in drafts {
        let span = SourceSpan::XPath {
            path: d.path.clone(),
        };
        let node = match &d.text {
            Some(t) => IrNode::content(d.kind, span, t.clone()),
            None => IrNode::structural(d.kind, span),
        };
        let idx = b.push(parent, node.with_attrs(d.attrs.clone()))?;
        emit(&d.children, Some(idx), b)?;
    }
    Ok(())
}

// ------------------------------------------------------------------- walker

/// One open element: its name, and how many of each child name it has seen.
///
/// The counts are what make the path an XPath rather than a breadcrumb —
/// `w:tc[3]` is the third `w:tc` among its siblings, and only the parent knows
/// that number.
#[derive(Debug, Default)]
struct Frame {
    name: String,
    seen: HashMap<String, u32>,
}

/// A table under construction.
#[derive(Debug)]
struct TableCtx {
    draft: Draft,
    row: u32,
    /// Whether this table is kept. A document past [`MAX_TABLES`] still has its
    /// elements walked — the stacks have to stay balanced — but the draft is
    /// discarded when it closes.
    keep: bool,
    /// Grid column → where the cell anchoring a vertical merge lives, as
    /// `(row index, cell index)` in `draft`. Word writes the continuation cells
    /// out in full, so the anchor has to be found again to extend its `rowspan`.
    vmerge: HashMap<u32, (usize, usize)>,
}

/// A row under construction.
#[derive(Debug)]
struct RowCtx {
    draft: Draft,
    /// Next free grid column, advanced by each cell's `gridSpan`.
    grid_col: u32,
    cells: usize,
}

/// A cell collecting the text of its paragraphs.
#[derive(Debug)]
struct CellCtx {
    draft: Draft,
    grid_col: u32,
    colspan: u32,
    /// A `w:vMerge` continuation contributes its square to the anchor above and
    /// gets no node of its own.
    merge_continue: bool,
    paragraphs: Vec<String>,
}

/// A paragraph collecting its runs.
#[derive(Debug)]
struct ParaCtx {
    text: String,
    path: String,
    level: Option<u8>,
}

struct Walker<'a> {
    xml: &'a [u8],
}

impl<'a> Walker<'a> {
    fn new(xml: &'a [u8]) -> Self {
        Self { xml }
    }

    /// Everything is a stack, including the row and the cell.
    ///
    /// A single `Option<CellCtx>` was the first version and it is wrong for a
    /// reason worth recording: Word puts tables inside cells, so the nested
    /// table's `</w:tc>` would clear the outer cell and the outer cell would
    /// never be emitted at all — its text would silently reappear as body
    /// prose. Nested tables are rare, and "rare" is exactly the shape of a bug
    /// that survives to production.
    fn run(self, b: &mut ArtifactBuilder) -> Result<Vec<Draft>> {
        let mut reader = Reader::from_reader(self.xml);
        reader.config_mut().trim_text(false);

        let mut buf = Vec::new();
        let mut path: Vec<Frame> = Vec::new();
        let mut out: Vec<Draft> = Vec::new();

        let mut tables: Vec<TableCtx> = Vec::new();
        let mut rows: Vec<RowCtx> = Vec::new();
        let mut cells: Vec<CellCtx> = Vec::new();
        let mut para: Option<ParaCtx> = None;
        let mut deep = false;
        let mut tables_seen = 0usize;

        loop {
            b.budget().check_time()?;
            buf.clear();
            let event = reader.read_event_into(&mut buf).map_err(xml_error)?;
            match event {
                Event::Eof => break,
                Event::Start(e) => {
                    let name = local(e.name().as_ref());
                    let here = enter(&mut path, &name);
                    if path.len() > MAX_XML_DEPTH {
                        deep = true;
                        continue;
                    }
                    match name.as_str() {
                        "tbl" => {
                            tables_seen += 1;
                            tables.push(TableCtx {
                                draft: Draft::structural(IrKind::Table, here),
                                row: 0,
                                keep: tables_seen <= MAX_TABLES,
                                vmerge: HashMap::new(),
                            });
                        }
                        "tr" if !tables.is_empty() => {
                            let n = tables.last().map_or(0, |t| t.row);
                            rows.push(RowCtx {
                                draft: Draft {
                                    attrs: NodeAttrs {
                                        row: Some(n),
                                        ..NodeAttrs::default()
                                    },
                                    ..Draft::structural(IrKind::TableRow, here)
                                },
                                grid_col: 0,
                                cells: 0,
                            });
                        }
                        "tc" if !rows.is_empty() => {
                            let grid_col = rows.last().map_or(0, |r| r.grid_col);
                            cells.push(CellCtx {
                                draft: Draft::structural(IrKind::TableCell, here),
                                grid_col,
                                colspan: 1,
                                merge_continue: false,
                                paragraphs: Vec::new(),
                            });
                        }
                        "p" => {
                            para = Some(ParaCtx {
                                text: String::new(),
                                path: here,
                                level: None,
                            });
                        }
                        _ => {}
                    }
                }
                Event::Empty(e) => {
                    let name = local(e.name().as_ref());
                    enter(&mut path, &name);
                    path.pop();
                    match name.as_str() {
                        "gridSpan" => {
                            if let Some(c) = cells.last_mut() {
                                c.colspan = val(&e)
                                    .and_then(|v| v.parse::<u32>().ok())
                                    .unwrap_or(1)
                                    .clamp(1, MAX_SPAN);
                            }
                        }
                        "vMerge" => {
                            if let Some(c) = cells.last_mut() {
                                // No `w:val` means "continue"; only `restart`
                                // begins a new merge.
                                c.merge_continue = val(&e).is_none_or(|v| v != "restart");
                            }
                        }
                        "tblCaption" | "tblDescription" => {
                            if let (Some(t), Some(text)) = (tables.last_mut(), val(&e)) {
                                add_caption(t, text);
                            }
                        }
                        "pStyle" => {
                            if let (Some(p), Some(v)) = (para.as_mut(), val(&e)) {
                                p.level = heading_level(&v);
                            }
                        }
                        // A run break inside a paragraph is a line break.
                        "br" | "cr" => push_text(&mut para, "\n"),
                        "tab" => push_text(&mut para, "\t"),
                        _ => {}
                    }
                }
                Event::Text(e) => {
                    if para.is_some() && in_text_run(&path) {
                        let raw = e.decode().map_err(|_| decode_error())?;
                        let text = unescape(&raw).unwrap_or_else(|_| raw.clone());
                        push_text(&mut para, &text);
                    }
                }
                // `quick-xml` reports `&amp;` and `&#x2014;` as their own
                // events rather than folding them into the surrounding text.
                // Ignoring them does not corrupt a character, it *deletes* one
                // — an ampersand in a company name simply vanishes — which is
                // the kind of silent loss a corpus never reports.
                Event::GeneralRef(e) => {
                    if para.is_some() && in_text_run(&path) {
                        let name = e.decode().map_err(|_| decode_error())?;
                        if let Ok(text) = unescape(&format!("&{name};")) {
                            push_text(&mut para, &text);
                        }
                    }
                }
                Event::End(e) => {
                    let name = local(e.name().as_ref());
                    if path.len() > MAX_XML_DEPTH {
                        path.pop();
                        continue;
                    }
                    match name.as_str() {
                        "p" => {
                            if let Some(p) = para.take() {
                                close_paragraph(p, cells.last_mut(), &mut out, b);
                            }
                        }
                        "tc" => {
                            if let (Some(c), Some(r), Some(t)) =
                                (cells.pop(), rows.last_mut(), tables.last_mut())
                            {
                                close_cell(c, r, t);
                            }
                        }
                        "tr" => {
                            if let (Some(r), Some(t)) = (rows.pop(), tables.last_mut()) {
                                if t.draft.children.len() < MAX_ROWS_PER_TABLE {
                                    t.draft.children.push(r.draft);
                                }
                                t.row += 1;
                            }
                        }
                        "tbl" => {
                            if let Some(t) = tables.pop() {
                                if t.keep {
                                    // A nested table hangs off the cell it is
                                    // in, so `descendants_of` still claims it
                                    // for the outer table and the chunker does
                                    // not emit it twice.
                                    match cells.last_mut() {
                                        Some(cell) => cell.draft.children.push(t.draft),
                                        None => out.push(t.draft),
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                    path.pop();
                }
                _ => {}
            }
        }

        if deep {
            b.warn(ParseWarning::new(
                Code::ParTruncated,
                "Part of this document nests deeper than the parser descends, so that part was \
                 not indexed. Deep nesting is the standard shape of a parser denial-of-service; \
                 the rest of the document was indexed normally.",
            ));
            b.set_outcome(ParseOutcome::Partial);
        }
        if tables_seen > MAX_TABLES {
            b.warn(ParseWarning::new(
                Code::ParTruncated,
                format!(
                    "This document has more than {MAX_TABLES} tables; the ones past that are \
                     not indexed as tables."
                ),
            ));
            b.set_outcome(ParseOutcome::Partial);
        }
        Ok(out)
    }
}

/// Push a frame for `name` and return the XPath of the element being entered.
fn enter(path: &mut Vec<Frame>, name: &str) -> String {
    let n = match path.last_mut() {
        Some(parent) => {
            let c = parent.seen.entry(name.to_owned()).or_insert(0);
            *c += 1;
            *c
        }
        None => 1,
    };
    let mut s = String::with_capacity(64);
    s.push_str(BODY_PART);
    s.push('#');
    for f in path.iter() {
        s.push_str("/w:");
        s.push_str(&f.name);
    }
    // The index of the element itself, which the frames above do not carry.
    let _ = std::fmt::Write::write_fmt(&mut s, format_args!("/w:{name}[{n}]"));
    path.push(Frame {
        name: format!("{name}[{n}]"),
        seen: HashMap::new(),
    });
    s
}

/// Whether the innermost open element is a `w:t`, the only text Word means as
/// content. `w:instrText` (a field code) and `w:delText` (deleted revision) are
/// deliberately excluded — neither is what the document says.
fn in_text_run(path: &[Frame]) -> bool {
    path.last().is_some_and(|f| f.name.starts_with("t["))
}

fn push_text(para: &mut Option<ParaCtx>, s: &str) {
    if let Some(p) = para.as_mut() {
        p.text.push_str(s);
    }
}

fn close_paragraph(
    p: ParaCtx,
    cell: Option<&mut CellCtx>,
    out: &mut Vec<Draft>,
    b: &mut ArtifactBuilder,
) {
    let (text, clipped) = b.budget().clamp_text(&p.text);
    if clipped {
        b.warn(ParseWarning::new(
            Code::ParTruncated,
            "A paragraph's text was clipped to the per-node budget. Its XML path still names \
             the whole paragraph.",
        ));
        b.set_outcome(ParseOutcome::Partial);
    }
    match cell {
        // Inside a cell the paragraph is not its own node: the cell is the unit
        // `table.rs` reads, and a cell of three paragraphs is one cell.
        Some(c) => c.paragraphs.push(text),
        None => {
            if text.trim().is_empty() {
                return;
            }
            let kind = if p.level.is_some() {
                IrKind::Heading
            } else {
                IrKind::Paragraph
            };
            out.push(Draft {
                kind,
                path: p.path,
                text: Some(text),
                attrs: NodeAttrs {
                    level: p.level,
                    ..NodeAttrs::default()
                },
                children: Vec::new(),
            });
        }
    }
}

fn close_cell(mut c: CellCtx, r: &mut RowCtx, t: &mut TableCtx) {
    let colspan = c.colspan;
    let grid_col = c.grid_col;
    r.grid_col = r.grid_col.saturating_add(colspan);

    if c.merge_continue {
        // TBL-004: the square belongs to the cell above. Extend that cell's
        // `rowspan` and emit nothing here — a second node would put two cells
        // on one square, which `table.rs` would read as an overlap rather than
        // as a merge.
        if let Some((row_i, cell_i)) = t.vmerge.get(&grid_col).copied() {
            if let Some(anchor) = t
                .draft
                .children
                .get_mut(row_i)
                .and_then(|row| row.children.get_mut(cell_i))
            {
                let cur = anchor.attrs.rowspan.unwrap_or(1);
                anchor.attrs.rowspan = Some((cur + 1).min(MAX_SPAN));
                return;
            }
        }
        // A continuation with no anchor above it — a table that starts
        // mid-merge, which happens when a document is edited by hand. Fall
        // through and treat it as an ordinary cell rather than losing its text.
    }

    if r.cells >= MAX_CELLS_PER_ROW {
        return;
    }
    c.draft.text = Some(c.paragraphs.join("\n"));
    c.draft.attrs.row = r.draft.attrs.row;
    c.draft.attrs.col = Some(grid_col);
    c.draft.attrs.colspan = Some(colspan);
    c.draft.attrs.rowspan = Some(1);
    let cell_i = r.draft.children.len();
    let row_i = t.draft.children.len();
    r.draft.children.push(c.draft);
    r.cells += 1;

    // Where a vertical merge starting here would have to look for its anchor.
    for col in grid_col..grid_col.saturating_add(colspan) {
        t.vmerge.insert(col, (row_i, cell_i));
    }
}

/// A `w:tblCaption` becomes a paragraph child of the table, which is where
/// `table.rs` looks for a caption — the same shape HTML's `<caption>` takes.
fn add_caption(t: &mut TableCtx, text: String) {
    if t.draft.children.iter().any(|c| c.kind == IrKind::Paragraph) {
        return;
    }
    let path = format!("{}/w:tblPr/w:tblCaption", t.draft.path);
    t.draft.children.insert(
        0,
        Draft {
            kind: IrKind::Paragraph,
            path,
            text: Some(text),
            attrs: NodeAttrs::default(),
            children: Vec::new(),
        },
    );
}

/// `Heading3` → 3. Word's built-in style names; a custom style is not guessed
/// at, because a heading level that is wrong reorganises the whole document
/// outline in the chunker's context prefix.
fn heading_level(style: &str) -> Option<u8> {
    let rest = style
        .strip_prefix("Heading")
        .or_else(|| style.strip_prefix("heading"))?;
    rest.trim()
        .parse::<u8>()
        .ok()
        .filter(|n| (1..=6).contains(n))
}

fn local(name: &[u8]) -> String {
    let s = String::from_utf8_lossy(name);
    match s.rsplit_once(':') {
        Some((_, local)) => local.to_owned(),
        None => s.into_owned(),
    }
}

fn val(e: &quick_xml::events::BytesStart<'_>) -> Option<String> {
    let attr = e
        .attributes()
        .flatten()
        .find(|a| local(a.key.as_ref()) == "val")?;
    // Decoded here rather than through `Attribute::decode_and_unescape_value`,
    // which needs a `Decoder` that only the reader owns. An OOXML part is UTF-8
    // by the standard, and a `w:val` that is not is a lost attribute rather
    // than a lost document.
    let raw = String::from_utf8_lossy(&attr.value);
    Some(unescape(&raw).unwrap_or_else(|_| raw.clone()).into_owned())
}

fn xml_error(e: quick_xml::Error) -> Error {
    Error::new(
        Code::ParCorrupt,
        "This document's XML is malformed, so its content could not be read. It stays findable \
         by name; re-saving it in Word usually repairs it.",
    )
    .with_context(e.to_string())
}

fn decode_error() -> Error {
    Error::new(
        Code::ParCorrupt,
        "This document's XML is not valid UTF-8, so its text could not be read. It stays \
         findable by name; re-saving it in Word usually repairs it.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::{BudgetGuard, Budgets};
    use crate::ooxml::test_zip::zip_of;
    use crate::table::{tables_in, ColumnType, Reconstruction, TableIr};

    /// A `.docx` around one `w:body`. Every other part is irrelevant to this
    /// parser and its absence is part of what the fixture asserts.
    fn docx(body: &str) -> Vec<u8> {
        let document = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
             <w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
             <w:body>{body}</w:body></w:document>"
        );
        zip_of(&[
            ("[Content_Types].xml", b"<Types/>"),
            ("word/document.xml", document.as_bytes()),
        ])
    }

    /// `<w:p>` with one run of text.
    fn p(text: &str) -> String {
        format!("<w:p><w:r><w:t>{text}</w:t></w:r></w:p>")
    }

    fn tc(text: &str) -> String {
        format!("<w:tc>{}</w:tc>", p(text))
    }

    fn tr(cells: &[&str]) -> String {
        let inner: String = cells.iter().map(|c| tc(c)).collect();
        format!("<w:tr>{inner}</w:tr>")
    }

    fn tbl(rows: &[String]) -> String {
        format!("<w:tbl>{}</w:tbl>", rows.concat())
    }

    #[test]
    fn a_table_inside_a_cell_does_not_land_on_the_outer_tables_grid() {
        // Word puts tables inside cells, and this parser attaches a nested
        // table to the cell that holds it on purpose — so the chunker's
        // ownership check claims it for the outer table and does not emit its
        // rows again as loose text.
        //
        // Building the outer grid from that same set was the bug. A nested
        // table numbers its rows from zero like anybody else, so its cells
        // landed on the outer table's own (0,0) and (0,1) and the write died
        // on `UNIQUE(table_id, row_idx, col_idx)`. One real document in the
        // corpus was silently unindexed, reported only as
        // `INT_INVARIANT_VIOLATED` with no path.
        let inner = tbl(&[tr(&["in-a", "in-b"])]);
        let outer = format!(
            "<w:tbl><w:tr><w:tc>{}{inner}</w:tc><w:tc>{}</w:tc></w:tr></w:tbl>",
            p("out-a"),
            p("out-b"),
        );
        let a = parse(&docx(&outer)).expect("fixture must parse");
        a.validate().unwrap();

        let tables = crate::table::tables_in(&a);
        assert_eq!(tables.len(), 2, "the nested table is still its own table");

        for (i, t) in tables.iter().enumerate() {
            let mut seen = std::collections::HashSet::new();
            for c in &t.cells {
                assert!(
                    seen.insert((c.row, c.col)),
                    "table {i} has two cells at ({}, {})",
                    c.row,
                    c.col
                );
            }
        }

        // And the outer table holds its own two cells, not four.
        let outer_ir = tables
            .iter()
            .find(|t| t.cells.iter().any(|c| c.raw_text.contains("out-a")))
            .expect("the outer table");
        assert_eq!(
            outer_ir.cells.len(),
            2,
            "the inner table's cells were counted as the outer table's: {:?}",
            outer_ir
                .cells
                .iter()
                .map(|c| &c.raw_text)
                .collect::<Vec<_>>()
        );
    }

    fn parse(bytes: &[u8]) -> Result<ParsedArtifact> {
        let probe = FileProbe::new("doc.docx", bytes.len() as u64);
        DocxParser.parse(ParseInput {
            bytes,
            probe: &probe,
            budget: BudgetGuard::new(Budgets::default()),
        })
    }

    fn one_table(bytes: &[u8]) -> TableIr {
        let a = parse(bytes).expect("fixture must parse");
        a.validate().expect("fixture must validate");
        let mut t = tables_in(&a);
        assert_eq!(t.len(), 1, "expected exactly one table");
        t.remove(0)
    }

    fn simple() -> Vec<u8> {
        docx(&tbl(&[
            tr(&["part", "qty", "price"]),
            tr(&["bolt", "12", "0.4"]),
            tr(&["nut", "144", "0.02"]),
        ]))
    }

    #[test]
    fn a_word_table_becomes_the_same_table_ir_as_the_equivalent_csv() {
        // TBL-001. `docx.rs` contains no header rule and no type classifier;
        // both come from `table.rs`, which never learns the format's name.
        let docx_t = one_table(&simple());

        let probe = FileProbe::new("parts.csv", 0);
        let csv = crate::csv::CsvParser
            .parse(ParseInput {
                bytes: b"part,qty,price\nbolt,12,0.4\nnut,144,0.02\n",
                probe: &probe,
                budget: BudgetGuard::new(Budgets::default()),
            })
            .unwrap();
        let csv = tables_in(&csv).remove(0);

        assert_eq!(docx_t.n_rows, csv.n_rows);
        assert_eq!(docx_t.n_cols, csv.n_cols);
        assert_eq!(docx_t.header.row, csv.header.row);
        assert_eq!(docx_t.header.confidence, csv.header.confidence);
        assert_eq!(docx_t.column_names, csv.column_names);
        assert_eq!(docx_t.column_types, csv.column_types);
        assert_eq!(docx_t.reconstruction, Reconstruction::Exact);
        assert_eq!(docx_t.column_types[1], ColumnType::Integer);
    }

    #[test]
    fn every_cell_is_cited_by_a_path_that_names_its_element() {
        // TBL-002 in the form DOCX can honour: not a byte offset into a
        // stream that exists nowhere, but an address in the tree the file has.
        let t = one_table(&simple());
        for c in &t.cells {
            let SourceSpan::XPath { path } = &c.span else {
                panic!("a Word cell must carry an XML path, not {:?}", c.span);
            };
            assert!(path.starts_with("word/document.xml#/"), "{path}");
        }
        let SourceSpan::XPath { path } = &t.cell(1, 1).unwrap().span else {
            unreachable!()
        };
        assert_eq!(
            path, "word/document.xml#/w:document[1]/w:body[1]/w:tbl[1]/w:tr[2]/w:tc[2]",
            "1-based per sibling name, as XPath is"
        );
    }

    #[test]
    fn a_second_table_is_indexed_separately_from_the_first() {
        // The index in the path is what distinguishes them, so this is really a
        // test that the sibling counter is per parent rather than global.
        let bytes = docx(&format!(
            "{}{}{}",
            tbl(&[tr(&["a", "b"]), tr(&["1", "2"])]),
            p("between"),
            tbl(&[tr(&["c", "d"]), tr(&["3", "4"])])
        ));
        let a = parse(&bytes).unwrap();
        a.validate().unwrap();
        let tables = tables_in(&a);
        assert_eq!(tables.len(), 2);
        let SourceSpan::XPath { path } = &tables[1].span else {
            unreachable!()
        };
        assert!(path.ends_with("/w:tbl[2]"), "{path}");
    }

    #[test]
    fn a_grid_span_is_a_colspan_and_shifts_the_columns_after_it() {
        // TBL-004.
        let merged = "<w:tc><w:tcPr><w:gridSpan w:val=\"2\"/></w:tcPr>\
                      <w:p><w:r><w:t>Region totals</w:t></w:r></w:p></w:tc>";
        let bytes = docx(&tbl(&[
            format!("<w:tr>{merged}</w:tr>"),
            tr(&["north", "south"]),
            tr(&["10", "12"]),
        ]));
        let t = one_table(&bytes);
        assert_eq!(t.n_cols, 2);
        assert_eq!(t.merged_regions.len(), 1, "{:?}", t.merged_regions);
        assert_eq!(t.merged_regions[0].colspan, 2);
        assert_eq!(t.cell(0, 0).unwrap().raw_text, "Region totals");
        assert!(t.cell(0, 1).is_none(), "covered by the span to its left");
        assert_eq!(t.reconstruction, Reconstruction::Exact);
    }

    #[test]
    fn a_vertical_merge_extends_the_anchor_rather_than_repeating_the_cell() {
        // Word writes the continuation cell out in full. Emitting it would put
        // two nodes on one square; extending the anchor is what TBL-004 means
        // by "preserved, never silently flattened".
        let restart = "<w:tc><w:tcPr><w:vMerge w:val=\"restart\"/></w:tcPr>\
                       <w:p><w:r><w:t>EU</w:t></w:r></w:p></w:tc>";
        let cont = "<w:tc><w:tcPr><w:vMerge/></w:tcPr><w:p><w:r><w:t></w:t></w:r></w:p></w:tc>";
        let bytes = docx(&tbl(&[
            tr(&["region", "site"]),
            format!("<w:tr>{restart}{}</w:tr>", tc("north")),
            format!("<w:tr>{cont}{}</w:tr>", tc("south")),
        ]));
        let t = one_table(&bytes);
        assert_eq!(t.merged_regions.len(), 1, "{:?}", t.merged_regions);
        assert_eq!(t.merged_regions[0].rowspan, 2);
        assert_eq!(t.cell(1, 0).unwrap().raw_text, "EU");
        assert!(t.cell(2, 0).is_none(), "covered by the merge above");
        assert_eq!(t.cell(2, 1).unwrap().raw_text, "south");
    }

    #[test]
    fn a_cell_of_several_paragraphs_is_one_cell() {
        let cell = format!("<w:tc>{}{}</w:tc>", p("first"), p("second"));
        let bytes = docx(&tbl(&[
            tr(&["a", "b"]),
            format!("<w:tr>{cell}{}</w:tr>", tc("x")),
        ]));
        let t = one_table(&bytes);
        assert_eq!(t.cell(1, 0).unwrap().raw_text, "first\nsecond");
    }

    #[test]
    fn a_table_caption_is_the_tables_caption() {
        let bytes = docx(&format!(
            "<w:tbl><w:tblPr><w:tblCaption w:val=\"Units shipped\"/></w:tblPr>{}{}</w:tbl>",
            tr(&["a", "b"]),
            tr(&["1", "2"])
        ));
        let t = one_table(&bytes);
        assert_eq!(t.caption.as_deref(), Some("Units shipped"));
    }

    #[test]
    fn prose_outside_a_table_is_indexed_with_its_heading() {
        let bytes = docx(&format!(
            "{}{}",
            "<w:p><w:pPr><w:pStyle w:val=\"Heading1\"/></w:pPr>\
             <w:r><w:t>Results</w:t></w:r></w:p>",
            p("Revenue grew.")
        ));
        let a = parse(&bytes).unwrap();
        a.validate().unwrap();
        let heading = a
            .nodes
            .iter()
            .find(|n| n.kind == IrKind::Heading)
            .expect("a styled paragraph is a heading");
        assert_eq!(heading.text(), Some("Results"));
        assert_eq!(heading.attrs.level, Some(1));
        assert!(a
            .nodes
            .iter()
            .any(|n| n.kind == IrKind::Paragraph && n.text() == Some("Revenue grew.")));
    }

    #[test]
    fn breaks_and_tabs_survive_as_characters_rather_than_disappearing() {
        let bytes = docx(&p("a").replace(
            "<w:t>a</w:t>",
            "<w:t>a</w:t><w:br/><w:t>b</w:t><w:tab/><w:t>c</w:t>",
        ));
        let a = parse(&bytes).unwrap();
        let para = a
            .nodes
            .iter()
            .find(|n| n.kind == IrKind::Paragraph)
            .unwrap();
        assert_eq!(para.text(), Some("a\nb\tc"));
    }

    #[test]
    fn xml_entities_are_decoded_once_and_only_once() {
        let bytes = docx(&p("Tom &amp; Jerry &lt;3"));
        let a = parse(&bytes).unwrap();
        let para = a
            .nodes
            .iter()
            .find(|n| n.kind == IrKind::Paragraph)
            .unwrap();
        assert_eq!(para.text(), Some("Tom & Jerry <3"));
    }

    #[test]
    fn a_nested_table_belongs_to_the_cell_it_is_in() {
        let inner = tbl(&[tr(&["x", "y"]), tr(&["1", "2"])]);
        let outer_cell = format!("<w:tc>{}{}</w:tc>", p("outer"), inner);
        let bytes = docx(&tbl(&[
            tr(&["a", "b"]),
            format!("<w:tr>{outer_cell}{}</w:tr>", tc("z")),
        ]));
        let a = parse(&bytes).unwrap();
        a.validate().unwrap();
        let tables = tables_in(&a);
        assert_eq!(tables.len(), 2, "both tables are in the IR");
        // The chunker claims the outer table's descendants first, so the
        // nested one is not also chunked as a loose run of cells.
        let owned = crate::table::descendants_of(&a, tables[0].node);
        assert!(
            owned[tables[1].node],
            "the nested table is inside the outer"
        );
    }

    #[test]
    fn a_traversing_entry_name_is_still_reported_when_there_is_nothing_to_index() {
        // The bug this pins: the obvious `return Err(low_yield)` throws away
        // every warning the builder collected, so a document whose archive
        // tries a path traversal *and* has no content reported nothing at all
        // — the one file where the finding is the only thing worth having.
        // Found on the real corpus (`path-traversal.docx`).
        let document = b"<?xml version=\"1.0\"?><w:document xmlns:w=\"http://schemas\
                         .openxmlformats.org/wordprocessingml/2006/main\"><w:body/></w:document>";
        let bytes = zip_of(&[
            ("word/document.xml", document.as_slice()),
            ("../outside.xml", b"x"),
        ]);
        let a = parse(&bytes).expect("a flagged file is still a file (PAR-013)");
        a.validate().unwrap();
        assert_eq!(a.outcome, crate::ir::ParseOutcome::MetadataOnly);
        assert!(
            a.warnings
                .iter()
                .any(|w| w.code == Code::FsPathEscapeBlocked.as_str()),
            "{:?}",
            a.warnings
        );
    }

    #[test]
    fn a_file_named_docx_that_is_not_a_zip_falls_through_the_chain() {
        let e = parse(b"Not a Word document at all").unwrap_err();
        assert_eq!(e.code(), Code::ParUnsupported);
    }

    #[test]
    fn a_zip_without_a_document_part_is_reported_rather_than_parsed_as_empty() {
        let bytes = zip_of(&[("mimetype", b"application/vnd.oasis.opendocument.text")]);
        let e = parse(&bytes).unwrap_err();
        assert_eq!(e.code(), Code::ParCorrupt);
        assert!(e.code().isolates_to_one_file());
    }

    #[test]
    fn malformed_xml_isolates_to_one_file() {
        let bytes = zip_of(&[("word/document.xml", b"<w:document><w:body><w:p>")]);
        // Either an error or a partial parse is acceptable; a panic and a
        // fatal error are not.
        match parse(&bytes) {
            Ok(a) => a.validate().unwrap(),
            Err(e) => assert!(e.code().isolates_to_one_file(), "{e}"),
        }
    }

    #[test]
    fn a_deeply_nested_document_is_bounded_rather_than_recursed() {
        // The cheapest denial-of-service in every structured format.
        let mut body = String::new();
        for _ in 0..2_000 {
            body.push_str("<w:tbl><w:tr><w:tc>");
        }
        body.push_str("<w:p><w:r><w:t>deep</w:t></w:r></w:p>");
        for _ in 0..2_000 {
            body.push_str("</w:tc></w:tr></w:tbl>");
        }
        let bytes = docx(&body);
        // Bounded, and it terminates: that is the whole assertion.
        let _ = parse(&bytes);
    }

    #[test]
    fn heading_levels_come_from_word_styles_and_nothing_else() {
        assert_eq!(heading_level("Heading1"), Some(1));
        assert_eq!(heading_level("heading6"), Some(6));
        assert_eq!(heading_level("Heading7"), None);
        assert_eq!(heading_level("BodyText"), None);
        assert_eq!(heading_level("Heading"), None);
    }
}
