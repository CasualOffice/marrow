//! The unified Table IR (Part 5 §99.2, `TBL`).
//!
//! # Why this is derived from the node arena rather than built by each parser
//!
//! **TBL-001** says every source normalizes into one structure and that
//! downstream never branches on the source format. The cheapest way to make
//! that true — rather than merely intended — is to give the parsers no say in
//! it. A parser's whole job is to emit [`IrKind::Table`] / [`IrKind::TableRow`]
//! / [`IrKind::TableCell`] nodes with honest spans; [`tables_in`] is the single
//! function that turns those into a [`TableIr`]. A Markdown table and a CSV
//! arrive here as the same shape because they arrive through the same code, not
//! because three parsers were each careful.
//!
//! It also means header detection, type inference and chunking are written
//! once. XLSX and DOCX inherit all three by emitting the same nodes — and that
//! claim was tested rather than asserted: neither [`crate::xlsx`] nor
//! [`crate::docx`] contains a line of header logic or a type classifier, and
//! the only thing that had to grow to admit them was the cell struct, by two
//! fields the text formats leave `None`.
//!
//! # Spans (TBL-002)
//!
//! A cell's `source_span` is the parser's, untouched. This module never invents
//! one and never widens one:
//!
//! | Source | Span | Why |
//! |---|---|---|
//! | CSV / TSV | [`SourceSpan::Bytes`] | A delimited file is text. The bytes are the cell as the user would see it in an editor, and there is no sheet to name. §99.5 says "EXACT — byte range". |
//! | Markdown | [`SourceSpan::Bytes`] | Same: a `.md` table is a run of characters in a text file. |
//! | HTML | [`SourceSpan::Bytes`] | The `<td>`'s inner range. A DOM path would be a coordinate we invented; the byte range is one the file actually has. |
//! | **XLSX** | [`SourceSpan::Cells`] | `Sheet1!B4` is the address the *file itself* uses. Excel's box takes it, the formulas in the workbook are written in it, and the user already thinks in it. |
//! | **DOCX** | [`SourceSpan::XPath`] | A `.docx` is deflated XML inside a zip. A byte offset would index a decompressed stream that exists nowhere on disk — there is no editor position it resolves to. The element path is a real address in a tree the file really has. |
//!
//! [`SourceSpan::Cells`] was deliberately unused until XLSX, and the reason is
//! the same reason XLSX earns it: it names a sheet and an A1 range, which is a
//! true address in a workbook and a fiction anywhere else. A citation has to
//! resolve to something the user can be taken to, and "Sheet1!B4" of a CSV is
//! not that — the CSV has no B4, it has bytes 137..139. The rule is not "use
//! the richest variant available", it is "use the coordinate system the source
//! is actually written in".
//!
//! DOCX is the same rule reaching the opposite conclusion from HTML. HTML has
//! byte offsets that a person can act on, so a DOM path there would be an
//! invention; DOCX has no such offsets at all, so the XML path is not the
//! second-best answer but the only true one.
//!
//! A ragged row leaves a hole in the grid. The hole stays a hole: synthesising
//! a cell for it would mean synthesising a location, which is the one thing
//! invariant #1 exists to prevent. The table is flagged
//! [`Reconstruction::Degraded`] instead.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use marrow_core::{ProvenanceClass, SourceSpan};
use serde::{Deserialize, Serialize};

use crate::ir::{IrKind, ParsedArtifact};

/// What a column's values turned out to be (TBL-005).
///
/// A subset of §99.2's list. `enum` is not here: it needs a cardinality policy
/// nothing has asked for yet.
///
/// `formula` is not here either, and XLSX arriving is what settled that. §99.2
/// lists it as a *column* type, but a formula column still holds numbers —
/// `=B2*C2` down a price column is a decimal column whose cells happen to be
/// computed — and typing it `formula` would take the one fact §99.3's
/// arithmetic needs and replace it with the fact that it was calculated. So the
/// formula lives on the cell ([`TableCell::formula`]) beside the type rather
/// than instead of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ColumnType {
    /// Every value in the column was blank.
    Empty,
    /// Text, or a mixture with no majority. The honest fallback.
    String,
    Integer,
    Decimal,
    Currency,
    Percent,
    Boolean,
    /// ISO-8601 calendar date, `YYYY-MM-DD`.
    Date,
    /// ISO-8601 date and time.
    DateTime,
    /// UUID or ULID. Deliberately narrow — a loose "looks like an identifier"
    /// rule labels half of every text column.
    Id,
}

impl ColumnType {
    pub const fn as_str(self) -> &'static str {
        match self {
            ColumnType::Empty => "empty",
            ColumnType::String => "string",
            ColumnType::Integer => "integer",
            ColumnType::Decimal => "decimal",
            ColumnType::Currency => "currency",
            ColumnType::Percent => "percent",
            ColumnType::Boolean => "boolean",
            ColumnType::Date => "date",
            ColumnType::DateTime => "datetime",
            ColumnType::Id => "id",
        }
    }

    /// Whether a value of this type is a quantity the arithmetic in §99.3 could
    /// one day operate on.
    pub const fn is_numeric(self) -> bool {
        matches!(
            self,
            ColumnType::Integer | ColumnType::Decimal | ColumnType::Currency | ColumnType::Percent
        )
    }

    /// Whether this reads as prose rather than as a value. The header-detection
    /// signal in [`detect_header`] turns on the shift from one to the other.
    const fn is_texty(self) -> bool {
        matches!(self, ColumnType::String | ColumnType::Empty)
    }
}

/// A cell's typed reading. **Never replaces [`TableCell::raw_text`]** — TBL-005
/// is explicit that the raw text is always retained alongside, because a number
/// that parsed is still a string somebody wrote, and the string is what a
/// citation has to show.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum CellValue {
    /// Blank. It still has a position, which is why the cell exists at all.
    Empty,
    /// No typed reading beyond the raw text.
    Text,
    Integer(i64),
    /// Decimal, currency and percent all carry the number as written. No
    /// scaling: `12%` is `12.0`, not `0.12`. Rescaling here would be a unit
    /// conversion, and TBL-009 says unit handling blocks rather than coerces.
    Number(f64),
    Boolean(bool),
    /// Normalised to ISO-8601. The raw text is still the source of truth.
    Timestamp,
    /// An identifier — not a quantity, whatever it is made of.
    Identifier,
}

impl CellValue {
    /// The number this cell holds, if it holds one.
    pub fn as_f64(self) -> Option<f64> {
        match self {
            CellValue::Integer(i) => Some(i as f64),
            CellValue::Number(n) => Some(n),
            _ => None,
        }
    }
}

/// One cell. **TBL-002**: `span` is where it came from, exactly.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TableCell {
    pub row: u32,
    pub col: u32,
    /// 1 unless the source said otherwise (HTML `rowspan`). TBL-004.
    pub rowspan: u32,
    pub colspan: u32,
    /// **Always populated.** TBL-005.
    pub raw_text: String,
    pub value: CellValue,
    pub value_type: ColumnType,
    pub span: SourceSpan,
    /// TBL-013. 1.0 for anything read from bytes; below that only where reading
    /// the text was itself a guess (OCR).
    pub confidence: f32,
    /// **TBL-007 / PAR-007.** The formula this cell's value was computed from,
    /// as written. `None` for a literal, and for every format that has no
    /// formulas.
    ///
    /// [`TableCell::raw_text`] is unaffected: it stays the *cached result*,
    /// because that is what the cell shows and what §99.3 computes over. The
    /// formula is the provenance of the number, not a substitute for it — a
    /// column of `=SUM(...)` that reported its text as `=SUM(...)` would be a
    /// column of strings, and every numeric question against it would fail.
    pub formula: Option<String>,
}

impl TableCell {
    /// The typed reading as `table_cells.typed_value` stores it, or `None`
    /// where there is nothing beyond the raw text.
    ///
    /// Lives here rather than at the persistence boundary so there is exactly
    /// one rendering of a value: two would eventually disagree, and the one in
    /// the database is the one a citation shows.
    pub fn typed_value(&self) -> Option<String> {
        match self.value {
            CellValue::Empty | CellValue::Text => None,
            CellValue::Integer(i) => Some(i.to_string()),
            CellValue::Number(n) => Some(fmt_num(n)),
            CellValue::Boolean(b) => Some(b.to_string()),
            // Already ISO-8601 by the time it classified as one, so the raw
            // text is the normalised form.
            CellValue::Timestamp | CellValue::Identifier => Some(self.raw_text.trim().to_owned()),
        }
    }
}

/// A name the workbook gives to a range (**TBL-007**, PAR-007).
///
/// §99.2 files named ranges under `relations`, alongside formula dependencies
/// and cross-sheet references. Only this half is built: the *declared*
/// relations, which the file states outright. A resolved dependency graph — B7
/// reads B2:B6, this sheet reads that one — is derivable from
/// [`TableCell::formula`] and is deliberately not derived here. It would be an
/// index with nowhere to live (no `table_relations` table exists) and it is the
/// first step of the knowledge graph [D43] refused until three real questions
/// ask for it. The formula string is the record; resolving it is a later
/// decision, not a lost one.
///
/// [D43]: ../../../DECISIONS.md
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NamedRange {
    /// The defined name, e.g. `Revenue`. Lifted from the file.
    pub name: String,
    /// The reference exactly as the workbook wrote it, e.g. `Sheet1!$B$2:$B$10`.
    pub target: String,
    /// Where it points, resolved to a [`SourceSpan::Cells`] the citation layer
    /// can navigate to.
    pub span: SourceSpan,
}

/// A cell that covers more than its own square (TBL-004).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergedRegion {
    pub row: u32,
    pub col: u32,
    pub rowspan: u32,
    pub colspan: u32,
}

/// Where the header is, and how sure we are (TBL-003).
///
/// `confidence` is recorded whether or not a header was accepted: "we looked
/// and the best row scored 0.4" is a different fact from "we did not look", and
/// the UI has to be able to tell them apart.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Header {
    /// Row index of the header, or `None` when nothing scored well enough.
    /// **Not assumed to be 0** — a title row or a blank leading row pushes it
    /// down, and both are ordinary in real files.
    pub row: Option<u32>,
    /// How many rows the header occupies. 0 or 1 for the sources built so far.
    pub rows: u32,
    /// Rows above the header — a title, a blank line, an export banner. Kept as
    /// cells like any other row; this only says they are not the body.
    pub preamble_rows: u32,
    /// `[0, 1]`.
    pub confidence: f32,
}

impl Header {
    /// First body row.
    pub const fn body_start(&self) -> u32 {
        self.preamble_rows + self.rows
    }
}

/// Whether the grid came back whole (TBL-018).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reconstruction {
    /// Every row is the same width and every square is accounted for.
    Exact,
    /// Ragged rows or holes. Everything found is still here and still cited;
    /// the shape is what is uncertain.
    Degraded,
    /// Not a table after all — one row, or one column, or no cells. **The text
    /// is not dropped**: [`table_chunks`] emits it as an ordinary text chunk so
    /// the content stays discoverable, flagged rather than lost.
    Failed,
}

impl Reconstruction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Reconstruction::Exact => "EXACT",
            Reconstruction::Degraded => "DEGRADED",
            Reconstruction::Failed => "FAILED",
        }
    }
}

/// One table, normalized. TBL-001: this is the only table type downstream sees.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TableIr {
    /// Arena index of the [`IrKind::Table`] node this came from.
    pub node: usize,
    /// **Invariant #1**, at table scope.
    pub span: SourceSpan,
    pub n_rows: u32,
    pub n_cols: u32,
    pub header: Header,
    /// Header text per column, empty where the header cell was blank or absent.
    pub column_names: Vec<String>,
    /// One per column, inferred over the body rows only (TBL-005).
    pub column_types: Vec<ColumnType>,
    /// `<caption>`, where the format has one. Not guessed from nearby prose:
    /// the heading chain already carries that, and a guessed caption that names
    /// the wrong table is worse than none.
    pub caption: Option<String>,
    pub merged_regions: Vec<MergedRegion>,
    /// TBL-007. Empty for every source that has no such concept.
    pub named_ranges: Vec<NamedRange>,
    /// Row-major, ascending. Holes are absent, never synthesised.
    pub cells: Vec<TableCell>,
    pub provenance: ProvenanceClass,
    /// Which engine produced it (§99.5).
    pub extraction_method: &'static str,
    pub reconstruction: Reconstruction,
}

impl TableIr {
    /// The cell at `(row, col)`, if the source had one there.
    pub fn cell(&self, row: u32, col: u32) -> Option<&TableCell> {
        self.cells
            .binary_search_by_key(&(row, col), |c| (c.row, c.col))
            .ok()
            .map(|i| &self.cells[i])
    }

    /// Cells of one row, in column order.
    pub fn row(&self, row: u32) -> impl Iterator<Item = &TableCell> {
        self.cells.iter().filter(move |c| c.row == row)
    }

    /// Whether this reconstructed well enough to be treated as a table.
    pub const fn is_usable(&self) -> bool {
        !matches!(self.reconstruction, Reconstruction::Failed)
    }

    /// Number of body rows (everything below the header).
    pub fn body_rows(&self) -> u32 {
        self.n_rows.saturating_sub(self.header.body_start())
    }
}

/// Every table in an artifact, in document order.
///
/// The one door into the Table IR. See the module docs for why parsers do not
/// build these themselves.
pub fn tables_in(artifact: &ParsedArtifact) -> Vec<TableIr> {
    artifact
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.kind == IrKind::Table)
        .map(|(i, _)| build(artifact, i))
        .collect()
}

/// Arena indices that belong to `table` — the table node and everything under
/// it. Used by the chunker so a table's rows are not also chunked as loose text.
pub fn descendants_of(artifact: &ParsedArtifact, table: usize) -> Vec<bool> {
    let mut owned = vec![false; artifact.nodes.len()];
    if table < owned.len() {
        owned[table] = true;
    }
    // `parent` always points backwards (`ParsedArtifact::validate` enforces it),
    // so one forward pass closes the set.
    for (i, n) in artifact.nodes.iter().enumerate() {
        if let Some(p) = n.parent {
            if owned.get(p).copied().unwrap_or(false) {
                owned[i] = true;
            }
        }
    }
    owned
}

fn build(artifact: &ParsedArtifact, table_idx: usize) -> TableIr {
    let table_node = &artifact.nodes[table_idx];
    let owned = descendants_of(artifact, table_idx);

    // A caption is a content child of the table that is not a row or a cell —
    // HTML's `<caption>`. Markdown and CSV have no such element, so they get
    // `None` rather than a guess.
    let mut caption = None;
    let mut cells: Vec<TableCell> = Vec::new();
    let mut named_ranges: Vec<NamedRange> = Vec::new();

    for (i, n) in artifact.nodes.iter().enumerate() {
        if i == table_idx || !owned[i] {
            continue;
        }
        match n.kind {
            IrKind::TableCell => {
                let raw = n.text().unwrap_or_default().to_owned();
                // **The inheritance point.** Every source's cell arrives here as
                // text and is classified by the same function. XLSX hands over a
                // value it already knew the type of; re-deriving it from the
                // text is what keeps TBL-001 true, and the two agree because the
                // parser's job is to render the value honestly rather than to
                // pre-empt this.
                let (value_type, value) = classify(&raw);
                cells.push(TableCell {
                    row: n.attrs.row.unwrap_or(0),
                    col: n.attrs.col.unwrap_or(0),
                    rowspan: n.attrs.rowspan.unwrap_or(1).max(1),
                    colspan: n.attrs.colspan.unwrap_or(1).max(1),
                    raw_text: raw,
                    value,
                    value_type,
                    span: n.span.clone(),
                    confidence: n.attrs.confidence.unwrap_or(1.0),
                    formula: n.attrs.formula.clone(),
                });
            }
            IrKind::NamedRange => {
                if let Some(name) = n.attrs.name.clone() {
                    named_ranges.push(NamedRange {
                        name,
                        target: n.text().unwrap_or_default().to_owned(),
                        span: n.span.clone(),
                    });
                }
            }
            IrKind::Paragraph if caption.is_none() && n.parent == Some(table_idx) => {
                caption = n
                    .text()
                    .map(|t| t.trim().to_owned())
                    .filter(|t| !t.is_empty());
            }
            _ => {}
        }
    }
    named_ranges.sort_by(|a, b| a.name.cmp(&b.name));

    cells.sort_by_key(|c| (c.row, c.col));

    let n_rows = cells.iter().map(|c| c.row + 1).max().unwrap_or(0);
    let n_cols = cells.iter().map(|c| c.col + c.colspan).max().unwrap_or(0);

    let merged_regions = cells
        .iter()
        .filter(|c| c.rowspan > 1 || c.colspan > 1)
        .map(|c| MergedRegion {
            row: c.row,
            col: c.col,
            rowspan: c.rowspan,
            colspan: c.colspan,
        })
        .collect();

    let header = detect_header(&cells, n_rows, n_cols);
    let column_names = header_names(&cells, &header, n_cols);
    let column_types = infer_column_types(&cells, &header, n_cols);
    let reconstruction = grade(&cells, n_rows, n_cols);

    TableIr {
        node: table_idx,
        span: table_node.span.clone(),
        n_rows,
        n_cols,
        header,
        column_names,
        column_types,
        caption,
        merged_regions,
        named_ranges,
        cells,
        // The table can be no better than the parse it came out of, and can be
        // worse: a ragged grid is a degraded reading of exact bytes.
        provenance: match reconstruction {
            Reconstruction::Exact => artifact.provenance,
            _ => artifact.provenance.max(ProvenanceClass::Degraded),
        },
        extraction_method: extraction_method(artifact.parser_id),
        reconstruction,
    }
}

/// §99.5's engine column, as the value `table_ir.extraction_method` stores.
fn extraction_method(parser_id: &str) -> &'static str {
    match parser_id {
        "csv" => "native_delimited",
        "markdown" => "native_markdown",
        "html" => "native_html",
        "xlsx" => "native_xlsx",
        "docx" => "native_ooxml",
        _ => "native",
    }
}

fn grade(cells: &[TableCell], n_rows: u32, n_cols: u32) -> Reconstruction {
    if n_cols < 2 || n_rows < 2 || cells.is_empty() {
        // One column is a list and one row is a sentence. Saying so is TBL-018:
        // the content still gets chunked, just not as a table.
        return Reconstruction::Failed;
    }
    // Every square either holds a cell or is covered by a span. Anything else is
    // a hole, and a hole means the shape is a guess.
    let mut covered = vec![false; (n_rows as usize) * (n_cols as usize)];
    for c in cells {
        for r in c.row..(c.row + c.rowspan).min(n_rows) {
            for col in c.col..(c.col + c.colspan).min(n_cols) {
                covered[(r as usize) * (n_cols as usize) + col as usize] = true;
            }
        }
    }
    if covered.iter().all(|c| *c) {
        Reconstruction::Exact
    } else {
        Reconstruction::Degraded
    }
}

// ---------------------------------------------------------------- header

/// Weights. They sum to 1.0, so `confidence` is directly readable as "how much
/// of the available evidence pointed at this row".
const W_COMPLETE: f32 = 0.25;
const W_NON_NUMERIC: f32 = 0.30;
const W_DISTINCT: f32 = 0.15;
const W_TYPE_SHIFT: f32 = 0.30;

/// Below this, no row is called the header.
const HEADER_THRESHOLD: f32 = 0.5;

/// How far down the file we will look for a header before giving up. Enough for
/// a title, a blank line and an export banner; not enough to find a "header" in
/// the middle of the data.
const MAX_PREAMBLE_ROWS: u32 = 3;

/// Infer which row is the header, and how sure we are (**TBL-003**).
///
/// Row 0 is a candidate, not the answer. The signals, in weight order:
///
/// 1. **Type shift** (0.30) — some column where this row reads as text and the
///    body below reads as a number, a date or a boolean. The strongest evidence
///    there is, and the only one that survives a table of well-named columns
///    full of well-named values.
/// 2. **Nothing numeric** (0.30) — a row with `1,240` in it is data.
/// 3. **Complete** (0.25) — every column filled. Common but not required: a
///    corner cell above a row-label column is routinely blank.
/// 4. **Distinct** (0.15) — repeated header names happen in exports, so this is
///    evidence rather than a rule.
///
/// **Known limit**, stated rather than hidden: a header row made of numbers —
/// `| | 2023 | 2024 |` — scores 0.40 and is reported as *no header, confidence
/// 0.40*. That is the honest answer, because a row of numbers above rows of
/// numbers genuinely could be data, and TBL-003 asks for a confidence rather
/// than a confident answer. Every row is still in the IR either way.
///
/// Width is a **gate**, not a signal: a candidate must be as wide as the table.
/// That is what lets a one-cell title row above the real header be skipped
/// instead of winning on the three signals it trivially satisfies.
fn detect_header(cells: &[TableCell], n_rows: u32, n_cols: u32) -> Header {
    let none = |confidence: f32| Header {
        row: None,
        rows: 0,
        preamble_rows: 0,
        confidence,
    };
    if n_rows < 2 || n_cols == 0 {
        return none(0.0);
    }

    let mut best = 0.0f32;
    let limit = MAX_PREAMBLE_ROWS.min(n_rows.saturating_sub(1));
    for h in 0..=limit {
        // Gate 1: the candidate must span the table.
        let width = row_width(cells, h);
        if width != n_cols {
            continue;
        }
        // Gate 2: everything above it must look like preamble — narrower than
        // the table, or blank. A full-width row above the candidate is data,
        // and data above a header means we are guessing.
        if (0..h).any(|r| row_width(cells, r) == n_cols) {
            continue;
        }

        let score = score_header(cells, h, n_cols);
        if score > best {
            best = score;
            if score >= HEADER_THRESHOLD {
                return Header {
                    row: Some(h),
                    rows: 1,
                    preamble_rows: h,
                    confidence: round2(score),
                };
            }
        }
    }
    // Nothing convinced us. The rows are all still here — the caller decides
    // what to do with a table whose columns have no names, and the confidence
    // says how close it came.
    none(round2(best))
}

fn score_header(cells: &[TableCell], h: u32, n_cols: u32) -> f32 {
    let head: Vec<&TableCell> = cells.iter().filter(|c| c.row == h).collect();

    // The **corner cell may be blank** and the row still counts as complete. A
    // table whose first column holds row labels writes its header as
    // `| | 2023 | 2024 |`, and that is idiomatic rather than damaged. Found on
    // the real corpus, where requiring every cell was scoring perfectly good
    // headers at 0.45 — just under the threshold. Nothing after the first may
    // be blank, so a blank row still scores nothing here.
    let complete =
        head.len() as u32 == n_cols && head.iter().skip(1).all(|c| !c.raw_text.trim().is_empty());

    let non_numeric = head.iter().all(|c| !c.value_type.is_numeric());

    let mut seen: Vec<String> = head
        .iter()
        .map(|c| c.raw_text.trim().to_lowercase())
        .collect();
    let before = seen.len();
    seen.sort_unstable();
    seen.dedup();
    let distinct = seen.len() == before;

    // The shift: a column where the candidate is text and something below is
    // not. Looked for across the body rather than in the first body row alone,
    // because the row under a header is often a units row.
    let type_shift = head.iter().any(|hc| {
        hc.value_type.is_texty()
            && cells
                .iter()
                .any(|c| c.row > h && c.col == hc.col && !c.value_type.is_texty())
    });

    (complete as u8 as f32) * W_COMPLETE
        + (non_numeric as u8 as f32) * W_NON_NUMERIC
        + (distinct as u8 as f32) * W_DISTINCT
        + (type_shift as u8 as f32) * W_TYPE_SHIFT
}

fn row_width(cells: &[TableCell], row: u32) -> u32 {
    cells
        .iter()
        .filter(|c| c.row == row)
        .map(|c| c.col + c.colspan)
        .max()
        .unwrap_or(0)
}

fn round2(v: f32) -> f32 {
    (v * 100.0).round() / 100.0
}

fn header_names(cells: &[TableCell], header: &Header, n_cols: u32) -> Vec<String> {
    let mut names = vec![String::new(); n_cols as usize];
    let Some(h) = header.row else { return names };
    for c in cells.iter().filter(|c| c.row == h) {
        if let Some(slot) = names.get_mut(c.col as usize) {
            *slot = c.raw_text.trim().to_owned();
        }
    }
    names
}

// ------------------------------------------------------------ column types

/// The share of a column's non-empty cells that must agree before the column
/// takes their type. Below it, `String` — a column that is 60% numbers is not
/// a numeric column, it is a mess, and calling it numeric is how a sum ends up
/// silently skipping rows.
const TYPE_MAJORITY: f32 = 0.8;

fn infer_column_types(cells: &[TableCell], header: &Header, n_cols: u32) -> Vec<ColumnType> {
    let body_start = header.body_start();
    (0..n_cols)
        .map(|col| {
            let mut counts: BTreeMap<ColumnType, u32> = BTreeMap::new();
            let mut total = 0u32;
            for c in cells.iter().filter(|c| {
                c.col == col && c.row >= body_start && c.value_type != ColumnType::Empty
            }) {
                *counts.entry(c.value_type).or_insert(0) += 1;
                total += 1;
            }
            if total == 0 {
                return ColumnType::Empty;
            }
            // Integers are decimals that happened to be round. Folding them
            // before the vote stops a price column of `4` and `4.50` from
            // coming out as `String`.
            let ints = counts.remove(&ColumnType::Integer).unwrap_or(0);
            if ints > 0 {
                let decimals = counts.entry(ColumnType::Decimal).or_insert(0);
                *decimals += ints;
                if *decimals == ints {
                    // Only integers: keep the narrower type.
                    counts.remove(&ColumnType::Decimal);
                    counts.insert(ColumnType::Integer, ints);
                }
            }
            let (kind, n) = counts
                .into_iter()
                .max_by_key(|(_, n)| *n)
                .unwrap_or((ColumnType::String, 0));
            if (n as f32) / (total as f32) >= TYPE_MAJORITY {
                kind
            } else {
                ColumnType::String
            }
        })
        .collect()
}

/// Currency symbols worth recognising without a locale database.
const CURRENCY: [char; 6] = ['$', '€', '£', '¥', '₹', '₽'];

/// Read one cell. Deterministic, no locale, no guessing at ambiguous dates.
///
/// `12/03/2024` is deliberately **not** a date here: it is the third of December
/// to half the world and the twelfth of March to the other half, and a citation
/// that silently picks one is worse than a citation that says "text".
fn classify(raw: &str) -> (ColumnType, CellValue) {
    let t = raw.trim();
    if t.is_empty() {
        return (ColumnType::Empty, CellValue::Empty);
    }

    match t.to_ascii_lowercase().as_str() {
        "true" | "yes" => return (ColumnType::Boolean, CellValue::Boolean(true)),
        "false" | "no" => return (ColumnType::Boolean, CellValue::Boolean(false)),
        _ => {}
    }

    if let Some(body) = t.strip_suffix('%') {
        if let Some(n) = parse_number(body) {
            return (ColumnType::Percent, CellValue::Number(n));
        }
    }

    let currency = t.starts_with(CURRENCY) || t.ends_with(CURRENCY);
    if currency {
        let body = t.trim_start_matches(CURRENCY).trim_end_matches(CURRENCY);
        if let Some(n) = parse_number(body) {
            return (ColumnType::Currency, CellValue::Number(n));
        }
    }

    if is_iso_date(t) {
        return (ColumnType::Date, CellValue::Timestamp);
    }
    if is_iso_datetime(t) {
        return (ColumnType::DateTime, CellValue::Timestamp);
    }
    if is_uuid(t) || is_ulid(t) || is_zero_padded(t) {
        return (ColumnType::Id, CellValue::Identifier);
    }

    if let Some(n) = parse_number(t) {
        if !t.contains(['.', 'e', 'E']) && n.abs() < 9.007e15 {
            return (ColumnType::Integer, CellValue::Integer(n as i64));
        }
        return (ColumnType::Decimal, CellValue::Number(n));
    }

    (ColumnType::String, CellValue::Text)
}

/// Parse a number, tolerating thousands separators and a trailing sign.
///
/// `(1,234)` is accounting notation for a negative and turns up in every
/// exported financial table.
fn parse_number(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let (t, negate) = match t.strip_prefix('(').and_then(|x| x.strip_suffix(')')) {
        Some(inner) => (inner.trim(), true),
        None => (t, false),
    };
    let cleaned: String = t.chars().filter(|c| *c != ',' && *c != '_').collect();
    if cleaned.is_empty() || !cleaned.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    let n: f64 = cleaned.parse().ok()?;
    if !n.is_finite() {
        return None;
    }
    Some(if negate { -n } else { n })
}

fn is_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[8..].iter().all(u8::is_ascii_digit)
}

fn is_iso_datetime(s: &str) -> bool {
    match s.split_once(['T', ' ']) {
        Some((d, rest)) => {
            is_iso_date(d) && rest.len() >= 5 && rest.as_bytes()[2] == b':' && rest.is_ascii()
        }
        None => false,
    }
}

/// `007`, `0042`, `+0080`. Digits with a leading zero, so not a quantity.
///
/// Found on the real corpus in a `zip` column, where it was classifying as
/// `integer`. Two things go wrong when it does: the leading zero is what makes
/// a postcode a postcode, and §99.3's arithmetic would then happily offer to
/// sum a column of them. The raw text keeps the zero either way (TBL-005) —
/// this is about what the column *is*.
fn is_zero_padded(s: &str) -> bool {
    let t = s.strip_prefix(['+', '-']).unwrap_or(s);
    t.len() > 1 && t.starts_with('0') && t.bytes().all(|c| c.is_ascii_digit())
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.as_bytes().iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

fn is_ulid(s: &str) -> bool {
    // Crockford base32, no I, L, O or U.
    s.len() == 26
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (c.is_ascii_uppercase() && !b"ILOU".contains(&c)))
}

// ---------------------------------------------------------------- chunking

/// Render the header row as one line, for repetition on every band (TBL-011).
pub fn header_line(t: &TableIr) -> Option<String> {
    let h = t.header.row?;
    let line = render_row(t, h);
    (!line.trim().is_empty()).then_some(line)
}

/// One row as `a | b | c`, holes included as blanks so columns stay aligned.
fn render_row(t: &TableIr, row: u32) -> String {
    (0..t.n_cols)
        .map(|c| {
            t.cell(row, c)
                .map(|c| c.raw_text.trim())
                .unwrap_or_default()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

/// The schema chunk (TBL-011): what the columns are, not what is in them.
///
/// This is the chunk semantic search actually matches — a question phrased as
/// "which file has revenue by quarter" is about the *shape* of a table, and no
/// band of forty rows says that anywhere in its text.
pub fn schema_text(t: &TableIr, caption_and_context: &str) -> String {
    let mut s = String::new();
    if !caption_and_context.is_empty() {
        let _ = writeln!(s, "{caption_and_context}");
    }
    let _ = writeln!(s, "Table: {} columns × {} rows", t.n_cols, t.body_rows());
    if t.header.row.is_none() {
        let _ = writeln!(
            s,
            "No header row was detected (confidence {:.2}); columns are numbered.",
            t.header.confidence
        );
    }
    let _ = writeln!(s, "Columns:");
    for col in 0..t.n_cols {
        let name = t
            .column_names
            .get(col as usize)
            .map(String::as_str)
            .unwrap_or_default();
        let name = if name.is_empty() {
            format!("column {}", col + 1)
        } else {
            name.to_owned()
        };
        let ty = t
            .column_types
            .get(col as usize)
            .copied()
            .unwrap_or(ColumnType::String);
        match numeric_range(t, col) {
            Some((lo, hi)) => {
                let _ = writeln!(s, "- {name} ({}, {lo} to {hi})", ty.as_str());
            }
            None => {
                let _ = writeln!(s, "- {name} ({})", ty.as_str());
            }
        }
    }
    // TBL-012 wants named ranges matched exactly in lexical search, the way a
    // symbol is. The schema chunk is where that lands without a second index:
    // it is one chunk per table, it is already the chunk that describes shape
    // rather than contents, and "which sheet has the Revenue range" is exactly
    // a shape question.
    if !t.named_ranges.is_empty() {
        let _ = writeln!(s, "Named ranges:");
        for r in &t.named_ranges {
            let _ = writeln!(s, "- {} ({})", r.name, r.target);
        }
    }
    s
}

/// Range of a numeric column, for the schema chunk. Formatted from the parsed
/// numbers rather than the raw text so `1,240` and `1240` compare.
fn numeric_range(t: &TableIr, col: u32) -> Option<(String, String)> {
    if !t.column_types.get(col as usize)?.is_numeric() {
        return None;
    }
    let body_start = t.header.body_start();
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for v in t
        .cells
        .iter()
        .filter(|c| c.col == col && c.row >= body_start)
        .filter_map(|c| c.value.as_f64())
    {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    (lo <= hi).then(|| (fmt_num(lo), fmt_num(hi)))
}

fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::{BudgetGuard, Budgets};
    use crate::parser::{ContentParser, FileProbe, ParseInput};

    fn parse_csv(name: &str, src: &str) -> ParsedArtifact {
        let probe = FileProbe::new(name, src.len() as u64);
        crate::csv::CsvParser
            .parse(ParseInput {
                bytes: src.as_bytes(),
                probe: &probe,
                budget: BudgetGuard::new(Budgets::default()),
            })
            .expect("fixture must parse")
    }

    fn parse_md(src: &str) -> ParsedArtifact {
        let probe = FileProbe::new("t.md", src.len() as u64);
        crate::markdown::MarkdownParser
            .parse(ParseInput {
                bytes: src.as_bytes(),
                probe: &probe,
                budget: BudgetGuard::new(Budgets::default()),
            })
            .expect("fixture must parse")
    }

    fn one(a: &ParsedArtifact) -> TableIr {
        let mut t = tables_in(a);
        assert_eq!(t.len(), 1, "expected exactly one table");
        t.remove(0)
    }

    #[test]
    fn a_markdown_table_and_a_csv_arrive_as_the_same_thing() {
        // TBL-001. The two parsers share no code below the IR; if the shapes
        // ever diverge, they diverge here first.
        let csv = one(&parse_csv("t.csv", "name,qty\nbolt,12\nnut,144\n"));
        let md = one(&parse_md(
            "| name | qty |\n|---|---|\n| bolt | 12 |\n| nut | 144 |\n",
        ));

        assert_eq!(csv.n_rows, md.n_rows);
        assert_eq!(csv.n_cols, md.n_cols);
        assert_eq!(csv.column_names, md.column_names);
        assert_eq!(csv.column_types, md.column_types);
        assert_eq!(csv.header.row, md.header.row);
        assert_eq!(csv.header.rows, md.header.rows);
        // Only the provenance of *where* differs, which is the point of a span.
        assert_eq!(csv.cell(1, 1).unwrap().raw_text, "12");
        assert_eq!(md.cell(1, 1).unwrap().raw_text, "12");
    }

    #[test]
    fn every_cell_keeps_a_precise_span_that_resolves_to_its_own_bytes() {
        // TBL-002, and invariant #1 in its table form.
        let src = "name,qty\nbolt,12\nnut,144\n";
        let t = one(&parse_csv("t.csv", src));
        assert!(!t.cells.is_empty());
        for c in &t.cells {
            assert!(c.span.is_precise(), "imprecise span on {c:?}");
            let SourceSpan::Bytes { start, end } = c.span else {
                panic!(
                    "a delimited file is text; its cells are byte ranges, not {:?}",
                    c.span
                );
            };
            assert_eq!(
                &src[start as usize..end as usize],
                c.raw_text,
                "span must resolve to the cell's own bytes"
            );
        }
    }

    #[test]
    fn the_header_is_inferred_with_a_confidence_not_assumed_to_be_row_zero() {
        // TBL-003. A title row above the header is ordinary in exported files.
        let t = one(&parse_csv(
            "q.csv",
            "Quarterly results\nname,q1,q2\nbolt,12,14\nnut,144,150\n",
        ));
        assert_eq!(t.header.row, Some(1), "the title row is not the header");
        assert_eq!(t.header.preamble_rows, 1);
        assert!(t.header.confidence >= 0.9, "{:?}", t.header);
        assert_eq!(t.column_names, vec!["name", "q1", "q2"]);
        // TBL-003: the rows we decided against are still here.
        assert_eq!(t.cell(0, 0).unwrap().raw_text, "Quarterly results");
    }

    #[test]
    fn a_headerless_table_records_the_confidence_it_fell_short_by() {
        let t = one(&parse_csv("n.csv", "1,2,3\n4,5,6\n7,8,9\n"));
        assert_eq!(t.header.row, None);
        assert_eq!(t.header.body_start(), 0, "every row is body");
        assert!(
            t.header.confidence < HEADER_THRESHOLD,
            "a rejection still records how close it came: {:?}",
            t.header
        );
        assert!(t.column_names.iter().all(String::is_empty));
    }

    #[test]
    fn an_all_text_table_still_gets_a_header() {
        // No type shift to lean on, so this is the case a shift-only rule fails.
        let t = one(&parse_csv(
            "c.csv",
            "city,country\nOslo,Norway\nLima,Peru\n",
        ));
        assert_eq!(t.header.row, Some(0));
        assert_eq!(t.column_types, vec![ColumnType::String, ColumnType::String]);
    }

    #[test]
    fn raw_text_survives_type_inference() {
        // TBL-005. `1,240` parsed to 1240, and it is still the string somebody
        // typed — the citation has to show what is in the file.
        let t = one(&parse_csv(
            "m.csv",
            "item,amount\nwidget,\"1,240\"\ncog,\"2,000\"\n",
        ));
        assert_eq!(t.column_types[1], ColumnType::Integer);
        let cell = t.cell(1, 1).unwrap();
        assert_eq!(cell.raw_text, "1,240");
        assert_eq!(cell.value, CellValue::Integer(1240));
    }

    #[test]
    fn column_types_cover_the_shapes_real_tables_use() {
        let t = one(&parse_csv(
            "t.csv",
            "when,share,cost,ok,note\n\
             2024-01-05,12%,$4.50,yes,alpha\n\
             2024-02-06,8%,$3.00,no,beta\n",
        ));
        assert_eq!(
            t.column_types,
            vec![
                ColumnType::Date,
                ColumnType::Percent,
                ColumnType::Currency,
                ColumnType::Boolean,
                ColumnType::String,
            ]
        );
        // No rescaling: 12% is twelve, because turning it into 0.12 is a unit
        // conversion and TBL-009 says those are never silent.
        assert_eq!(t.cell(1, 1).unwrap().value, CellValue::Number(12.0));
    }

    #[test]
    fn an_ambiguous_date_stays_text_rather_than_picking_a_hemisphere() {
        let (ty, _) = classify("12/03/2024");
        assert_eq!(ty, ColumnType::String);
    }

    #[test]
    fn accounting_negatives_and_thousands_separators_parse() {
        assert_eq!(parse_number("(1,234.50)"), Some(-1234.5));
        assert_eq!(parse_number("1_000"), Some(1000.0));
        assert_eq!(parse_number("abc"), None);
        assert_eq!(parse_number(""), None);
    }

    #[test]
    fn a_mixed_column_is_string_rather_than_confidently_numeric() {
        let t = one(&parse_csv("x.csv", "k,v\na,1\nb,two\nc,3\nd,four\ne,5\n"));
        assert_eq!(
            t.column_types[1],
            ColumnType::String,
            "60% numbers is not a numeric column"
        );
    }

    #[test]
    fn a_ragged_table_is_flagged_rather_than_squared_off() {
        // TBL-018 and invariant #1 together: the missing square gets no cell,
        // because a cell would need a location and there is none.
        let t = one(&parse_csv("r.csv", "a,b,c\n1,2\n3,4,5\n"));
        assert_eq!(t.reconstruction, Reconstruction::Degraded);
        assert_eq!(t.provenance, ProvenanceClass::Degraded);
        assert!(t.cell(1, 2).is_none(), "no invented cell for the hole");
        assert_eq!(t.cell(2, 2).unwrap().raw_text, "5");
    }

    #[test]
    fn a_single_column_is_a_list_not_a_table() {
        let a = parse_md("| only |\n|---|\n| one |\n");
        let t = one(&a);
        assert_eq!(t.reconstruction, Reconstruction::Failed);
        assert!(!t.is_usable());
    }

    #[test]
    fn the_schema_chunk_names_columns_types_and_ranges() {
        let t = one(&parse_csv(
            "p.csv",
            "part,qty,price\nbolt,12,0.40\nnut,144,0.02\n",
        ));
        let s = schema_text(&t, "Parts");
        assert!(s.contains("Parts"), "{s}");
        assert!(s.contains("qty (integer, 12 to 144)"), "{s}");
        assert!(s.contains("price (decimal"), "{s}");
        assert!(s.contains("3 columns × 2 rows"), "{s}");
    }

    #[test]
    fn the_header_line_is_what_every_band_repeats() {
        let t = one(&parse_csv("p.csv", "part,qty\nbolt,12\n"));
        assert_eq!(header_line(&t).as_deref(), Some("part | qty"));
    }

    #[test]
    fn a_header_may_have_a_blank_corner_cell() {
        // `| | 2023 | 2024 |` over a row-label column. Found on the real
        // corpus, where insisting every header cell be filled scored this at
        // 0.45 and lost the column names.
        let t = one(&parse_md(
            "|  | State | Owner |\n|---|---|---|\n| north | open | ana |\n| south | done | bo |\n",
        ));
        assert_eq!(t.header.row, Some(0), "{:?}", t.header);
        assert_eq!(t.column_names, vec!["", "State", "Owner"]);
    }

    #[test]
    fn a_numeric_header_row_is_reported_as_uncertain_rather_than_guessed() {
        // `| | 2023 | 2024 |` is genuinely ambiguous — those could be data —
        // and TBL-003 asks for a confidence, not a confident answer. The rows
        // are all still here and the confidence says how close it came.
        let t = one(&parse_md(
            "|  | 2023 | 2024 |\n|---|---|---|\n| north | 10 | 12 |\n| south | 8 | 9 |\n",
        ));
        assert_eq!(t.header.row, None);
        assert!(t.header.confidence > 0.0 && t.header.confidence < HEADER_THRESHOLD);
        assert_eq!(
            t.cell(0, 1).unwrap().raw_text,
            "2023",
            "nothing was dropped"
        );
    }

    #[test]
    fn a_zero_padded_number_is_an_identifier_not_a_quantity() {
        // A `zip` column of `007`, `0042`, `+0080` on the real corpus. Summing
        // postcodes is never the right answer.
        let t = one(&parse_csv("z.csv", "city,zip\nOslo,0042\nLima,0080\n"));
        assert_eq!(t.column_types[1], ColumnType::Id);
        let cell = t.cell(1, 1).unwrap();
        assert_eq!(
            cell.raw_text, "0042",
            "the zero is what makes it a postcode"
        );
        assert_eq!(cell.value.as_f64(), None);
    }

    #[test]
    fn identifiers_are_recognised_narrowly() {
        assert_eq!(classify("01J8Z9QK7R4W6P2N3M5T8V0XYZ").0, ColumnType::Id);
        assert_eq!(
            classify("3f2504e0-4f89-11d3-9a0c-0305e82c3301").0,
            ColumnType::Id
        );
        // A word is not an identifier, however much it looks like one.
        assert_eq!(classify("PRODUCTION").0, ColumnType::String);
    }
}
