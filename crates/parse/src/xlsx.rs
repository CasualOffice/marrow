//! XLSX (T2) — worksheets as tables, cited by cell reference.
//!
//! §99.5 lists XLSX first among table sources and marks it `EXACT — sheet +
//! cell ref`. This is the parser that finally uses [`SourceSpan::Cells`], which
//! has existed unused since the core model was written: `Sheet1!B4` is the
//! address the *workbook itself* is written in — its formulas say `B4`, Excel's
//! name box accepts `B4`, and the user already thinks in `B4`. Every text
//! format before this one had byte offsets a person could act on, so a cell
//! reference there would have been a coordinate we invented. Here it is the
//! only true one. See the table in [`crate::table`]'s module docs.
//!
//! # This parser contains no table logic, and that is the point
//!
//! Header detection, column-type inference, merged-region bookkeeping and
//! chunking all live in [`crate::table`], derived from the node arena. This
//! module's entire job is to emit `Table` / `TableRow` / `TableCell` nodes with
//! honest spans, exactly as [`crate::csv`] does. There is no header heuristic
//! here, no classifier, and no "if xlsx" anywhere downstream — a worksheet and
//! a CSV of the same grid produce the same [`crate::table::TableIr`].
//!
//! # Why `calamine`
//!
//! Not for the zip and not for the XML — those are twenty lines each. For
//! `xl/styles.xml`. **A date in a workbook is a number**: `2025-10-13` is
//! stored as `45943`, and whether that cell is a date or a quantity is decided
//! by a format code in a completely different part of the archive, resolved
//! through `cellXfs`. Getting it wrong does not produce a formatting glitch, it
//! produces a column of five-digit integers that §99.3 would happily average.
//! `calamine` already does that resolution, and reimplementing it badly is the
//! one way this parser could be actively harmful.
//!
//! **What `calamine` will not tell us**, recorded because it bounds what TBL-006
//! can be built on later: the format *code* is private. `CellFormat` is
//! resolved to `DateTime`, `TimeDelta` or `Other`, so `0.00%` and `$#,##0` both
//! arrive as `Other` and a percent cell is indistinguishable from a decimal.
//! That is why `table_cells.number_format` stays unwritten here. The stored
//! value is not lost — a cell showing `42%` holds `0.42` and we record `0.42`,
//! which is what the file contains and what arithmetic needs. What is missing
//! is only the knowledge that it is *displayed* as a percentage, which is a
//! unit question (TBL-006), not a value question.
//!
//! # Bounds
//!
//! A workbook is a zip, so [`crate::ooxml`] gates the archive before anything
//! inflates. After that the sheet itself is the hostile surface: `calamine`'s
//! `worksheet_range` builds a **dense** grid from the sparse cells it finds, so
//! two cells at `A1` and `XFD1048576` ask it for a 17-billion-element vector.
//! This module therefore never calls it — it streams
//! [`calamine::XlsxCellReader`] and applies its own ceilings *before* the
//! bounding box is materialised.

use std::collections::{HashMap, HashSet};
use std::io::Cursor;

use calamine::{DataRef, Dimensions, ExcelDateTime, Reader, Sheet, SheetType, Xlsx, XlsxError};
use marrow_core::{Code, Error, Result, SourceSpan};

use crate::a1;
use crate::ir::{
    ArtifactBuilder, IrKind, IrNode, NodeAttrs, ParseOutcome, ParseWarning, ParsedArtifact,
    ParserTier,
};
use crate::ooxml;
use crate::parser::{ContentParser, FileProbe, ParseInput};

/// Worksheets read from one workbook. A workbook with more sheets than this is
/// a database, and the ones past the ceiling stay findable by file metadata.
const MAX_SHEETS: usize = 64;

/// Cells collected from one sheet before we stop reading it.
const MAX_CELLS_PER_SHEET: usize = 20_000;

/// Squares we will fill in to close a sheet's bounding box.
///
/// Blanks inside a grid are emitted as cells for the reason [`crate::csv`]
/// gives: a grid with its holes removed is a grid whose row widths are a guess,
/// and header detection is the first thing that then goes wrong. But a sheet
/// with a value in `A1` and another in `Z900` has a bounding box of 23,400
/// squares and two of them are real, so past this ceiling the sparse cells are
/// emitted alone and `table.rs` grades the result `DEGRADED` —
/// which is the honest description of a grid we did not fill in.
const MAX_GRID_CELLS: usize = 60_000;

/// Nodes left spare so the last row emitted is a whole row rather than half of
/// one, and so the builder never trips its own budget mid-table.
const NODE_HEADROOM: usize = 128;

/// A merge wider or taller than this is a denial-of-service dressed as a
/// spreadsheet, exactly as `rowspan="1000000"` is in HTML.
const MAX_MERGE_SPAN: u32 = 10_000;

/// The T2 workbook parser.
#[derive(Clone, Copy, Debug, Default)]
pub struct XlsxParser;

impl XlsxParser {
    pub const ID: &'static str = "xlsx";
    pub const VERSION: &'static str = "1";

    /// `xlsb` is deliberately absent: it is a different (binary) container that
    /// `calamine` reads through a different reader, and no file has asked.
    const EXTENSIONS: &'static [&'static str] = &["xlsx", "xlsm"];
}

impl ContentParser for XlsxParser {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn version(&self) -> &'static str {
        Self::VERSION
    }

    fn tier(&self) -> ParserTier {
        // T2: native, structural provenance — "cell ref" is the example Part 3
        // §63 gives for the tier.
        ParserTier::T2
    }

    fn handles(&self, probe: &FileProbe) -> bool {
        probe.has_any_extension(Self::EXTENSIONS)
    }

    fn parse(&self, input: ParseInput<'_>) -> Result<ParsedArtifact> {
        // FS-014: the extension is a hint. A `.xlsx` that is really a PDF must
        // fall through the chain rather than die inside `calamine`.
        if !ooxml::looks_like_zip(input.bytes) {
            return Err(Error::new(
                Code::ParUnsupported,
                "This file is named as a workbook but is not a zip archive, so another parser \
                 was tried instead.",
            ));
        }
        let preflight = ooxml::preflight(input.bytes)?;

        let mut book: Xlsx<_> = Xlsx::new(Cursor::new(input.bytes)).map_err(open_error)?;

        let mut b = ArtifactBuilder::new(Self::ID, Self::VERSION, self.tier(), input.budget);
        if preflight.suspicious_names > 0 {
            b.warn(ParseWarning::new(
                Code::FsPathEscapeBlocked,
                "This workbook contains archive entries whose names point outside the archive. \
                 Nothing was extracted to disk, so the file is safe to index, but treat its \
                 origin with suspicion.",
            ));
        }

        // Defined names are workbook-scoped and the sheet loop below takes
        // `&mut book`, so they are taken now and owned.
        let named: Vec<(String, String)> = book.defined_names().to_vec();

        let sheets: Vec<Sheet> = book
            .sheets_metadata()
            .iter()
            // A chart sheet or a dialog sheet has no cells. Hidden worksheets
            // are kept: a hidden sheet is usually where the lookup tables live,
            // and "not shown in Excel" is not "not in the file".
            .filter(|s| s.typ == SheetType::WorkSheet)
            .take(MAX_SHEETS)
            .cloned()
            .collect();
        if sheets.is_empty() {
            return Err(Error::new(
                Code::ParLowYield,
                "This workbook has no worksheets, so only its metadata is indexed.",
            ));
        }

        let mut truncated = false;
        // Which arena node is which sheet's table, so a defined name can find
        // the table it points into without the builder having to expose its
        // arena.
        let mut tables: Vec<(String, usize)> = Vec::new();
        for sheet in &sheets {
            match read_sheet(&mut book, &sheet.name, &mut b)? {
                SheetOutcome::Emitted {
                    truncated: t,
                    table,
                } => {
                    truncated |= t;
                    tables.push((sheet.name.clone(), table));
                }
                SheetOutcome::Empty => {}
                SheetOutcome::Unreadable(e) => {
                    // TBL-018 at workbook scope: one unreadable sheet must not
                    // cost the other twenty.
                    b.warn(
                        ParseWarning::new(
                            Code::ParCorrupt,
                            format!(
                                "Sheet `{}` could not be read, so its cells are not indexed. \
                                 The rest of the workbook was.",
                                sheet.name
                            ),
                        )
                        .at(sheet_span(&sheet.name, "A1")),
                    );
                    b.set_outcome(ParseOutcome::Partial);
                    tracing::warn!(sheet = %sheet.name, error = %e, "worksheet unreadable");
                }
            }
        }

        emit_named_ranges(&named, &tables, &mut b)?;

        if truncated {
            b.warn(ParseWarning::new(
                Code::ParTruncated,
                "This workbook is larger than the per-file cell budget, so some rows were not \
                 indexed. Every row that was indexed keeps its exact cell reference.",
            ));
            b.set_outcome(ParseOutcome::Partial);
        }

        let mut artifact = b.finish();
        if artifact.nodes.is_empty() {
            // Nothing to index. Returning an `Err` here is the obvious move and
            // it silently drops every warning the builder collected — including
            // "this archive contains an entry that escapes the archive root",
            // which is the one finding that must not be lost precisely because
            // the file has nothing else to say. A metadata-only artifact is the
            // shape that carries both facts: PAR-013's "still a file", and why.
            artifact.warnings.push(ParseWarning::new(
                Code::ParLowYield,
                "Every sheet in this workbook is empty, so only its metadata is indexed.",
            ));
            return Ok(ParsedArtifact::metadata_only(artifact.warnings));
        }
        Ok(artifact)
    }
}

enum SheetOutcome {
    Emitted { truncated: bool, table: usize },
    Empty,
    Unreadable(XlsxError),
}

/// One cell as it came off the stream, in absolute worksheet coordinates.
struct RawCell {
    row: u32,
    col: u32,
    text: String,
    formula: Option<String>,
}

fn read_sheet<RS: std::io::Read + std::io::Seek>(
    book: &mut Xlsx<RS>,
    name: &str,
    b: &mut ArtifactBuilder,
) -> Result<SheetOutcome> {
    // Merges first: the cell reader below borrows the workbook for as long as
    // it lives, and this needs the same borrow.
    let merges = book.merge_cells_by_sheet_name(name).unwrap_or_default();

    let mut cells: Vec<RawCell> = Vec::new();
    let mut truncated = false;
    {
        let mut reader = match book.worksheet_cells_reader(name) {
            Ok(r) => r,
            Err(e) => return Ok(SheetOutcome::Unreadable(e)),
        };
        loop {
            // The wall clock is checked inside the stream, not only around it:
            // a sheet is the one place in this parser where the work is
            // proportional to attacker-controlled input.
            b.budget().check_time()?;
            let record = match reader.next_cell_with_formula() {
                Ok(Some(r)) => r,
                Ok(None) => break,
                Err(e) => return Ok(SheetOutcome::Unreadable(e)),
            };
            let (row, col) = record.pos;
            if row > a1::MAX_ROW || col > a1::MAX_COL {
                // Outside Excel's own address space. There is no cell
                // reference for it, so there is no citation for it either.
                continue;
            }
            let text = cell_text(&record.value);
            let formula = record.formula.filter(|f| !f.trim().is_empty());
            if text.is_empty() && formula.is_none() {
                continue;
            }
            let (text, clipped) = b.budget().clamp_text(&text);
            if clipped {
                b.warn(ParseWarning::new(
                    Code::ParTruncated,
                    "A cell's text was clipped to the per-node budget. Its cell reference still \
                     names the whole cell.",
                ));
                b.set_outcome(ParseOutcome::Partial);
            }
            cells.push(RawCell {
                row,
                col,
                text,
                formula,
            });
            if cells.len() >= MAX_CELLS_PER_SHEET {
                truncated = true;
                break;
            }
        }
    }

    if cells.is_empty() {
        return Ok(SheetOutcome::Empty);
    }

    // A truncated read stops mid-row, and half a row would be reported as a
    // ragged table rather than as a clipped one. Drop back to the last row we
    // saw the end of.
    if truncated {
        let last = cells.last().map(|c| c.row).unwrap_or(0);
        // Unless that *is* the whole sheet — one row of 20,000 columns is a
        // clipped row, and dropping it would turn a truncation into a deletion.
        if cells.iter().any(|c| c.row < last) {
            cells.retain(|c| c.row < last);
        }
    }

    let (row0, col0, row1, col1) = bounding_box(&cells);
    let n_rows = (row1 - row0 + 1) as usize;
    let n_cols = (col1 - col0 + 1) as usize;
    let dense = n_rows.saturating_mul(n_cols) <= MAX_GRID_CELLS;

    // Merges the bounding box actually contains, keyed by their anchor.
    let spans = MergeMap::new(&merges, row0, col0, row1, col1);

    let table = b.push(
        None,
        IrNode::structural(
            IrKind::Table,
            sheet_span(name, &a1::range_ref(row0, col0, row1, col1)),
        )
        .with_attrs(NodeAttrs {
            language: Some("xlsx".to_owned()),
            row: Some(n_rows as u32),
            col: Some(n_cols as u32),
            ..NodeAttrs::default()
        }),
    )?;

    // The sheet name is the table's caption. Not a guess in the sense
    // `TableIr::caption` warns about — it is a label the author wrote and the
    // only name this table has, and it is what makes "the Q1 Revenue sheet"
    // findable at all in a twelve-sheet workbook.
    b.push(
        Some(table),
        IrNode::content(
            IrKind::Paragraph,
            sheet_span(name, &a1::range_ref(row0, col0, row1, col1)),
            name,
        ),
    )?;

    // Indexed rather than scanned. Filling a 60,000-square box by searching a
    // 20,000-cell vector for each square is 1.2 billion comparisons for a sheet
    // that opens instantly in Excel — the kind of quadratic that never shows up
    // on a fixture and hangs on a real workbook.
    let by_pos: HashMap<(u32, u32), &RawCell> = cells.iter().map(|c| ((c.row, c.col), c)).collect();
    let max_nodes = b.budget().limits().max_nodes;

    // In the dense case every row of the box gets a node, blanks included. In
    // the sparse case only the rows that hold something do — walking a box of
    // fifty thousand empty rows to skip them would be work proportional to an
    // address the file merely mentioned.
    let rows: Vec<u32> = if dense {
        (row0..=row1).collect()
    } else {
        let mut r: Vec<u32> = cells.iter().map(|c| c.row).collect();
        r.sort_unstable();
        r.dedup();
        r
    };

    for r in rows {
        if b.node_count() + n_cols + NODE_HEADROOM >= max_nodes {
            truncated = true;
            break;
        }
        let row_idx = r - row0;
        let row_node = b.push(
            Some(table),
            IrNode::structural(
                IrKind::TableRow,
                sheet_span(name, &a1::range_ref(r, col0, r, col1)),
            )
            .with_attrs(NodeAttrs {
                row: Some(row_idx),
                ..NodeAttrs::default()
            }),
        )?;

        let columns: Vec<u32> = if dense {
            (col0..=col1).collect()
        } else {
            let mut c: Vec<u32> = cells.iter().filter(|c| c.row == r).map(|c| c.col).collect();
            c.sort_unstable();
            c
        };
        for c in columns {
            if spans.is_covered(r, c) {
                // Inside somebody else's merge. TBL-004 keeps the region on the
                // anchor; a second cell here would be the same square twice.
                continue;
            }
            let (rowspan, colspan) = spans.at(r, c);
            let found = by_pos.get(&(r, c));
            let text = found.map(|x| x.text.clone()).unwrap_or_default();
            let formula = found.and_then(|x| x.formula.clone());
            let range = a1::range_ref(r, c, r + rowspan - 1, c + colspan - 1);
            b.push(
                Some(row_node),
                IrNode::content(IrKind::TableCell, sheet_span(name, &range), text).with_attrs(
                    NodeAttrs {
                        row: Some(row_idx),
                        col: Some(c - col0),
                        rowspan: Some(rowspan),
                        colspan: Some(colspan),
                        formula,
                        ..NodeAttrs::default()
                    },
                ),
            )?;
        }
    }

    Ok(SheetOutcome::Emitted { truncated, table })
}

/// TBL-007. A defined name becomes a node under the table it points into.
///
/// Names that resolve to no sheet (`TAX_RATE = 0.2`), to a sheet we did not
/// emit, or to something that is not a range at all are skipped rather than
/// stored with an invented location. `_xlnm.*` is Excel's own bookkeeping —
/// print areas and print titles — and is not a name anyone would search for.
fn emit_named_ranges(
    named: &[(String, String)],
    tables: &[(String, usize)],
    b: &mut ArtifactBuilder,
) -> Result<()> {
    for (name, reference) in named {
        if name.starts_with("_xlnm") {
            continue;
        }
        let Some((sheet, range)) = a1::split_sheet_ref(reference) else {
            continue;
        };
        let Some((r0, c0, r1, c1)) = a1::parse_range_ref(&range) else {
            continue;
        };
        let Some((_, table)) = tables.iter().find(|(s, _)| *s == sheet) else {
            continue;
        };
        b.push(
            Some(*table),
            IrNode::content(
                IrKind::NamedRange,
                sheet_span(&sheet, &a1::range_ref(r0, c0, r1, c1)),
                reference.clone(),
            )
            .with_attrs(NodeAttrs {
                name: Some(name.clone()),
                ..NodeAttrs::default()
            }),
        )?;
    }
    Ok(())
}

/// Merged regions, resolved to per-square answers.
///
/// Resolved up front into two maps rather than searched per square. A sheet may
/// declare any number of merges and each square would otherwise be tested
/// against every one of them, which is the same quadratic the cell lookup above
/// avoids — and here the multiplier is attacker-chosen.
struct MergeMap {
    /// Anchor → `(rowspan, colspan)`.
    anchors: HashMap<(u32, u32), (u32, u32)>,
    /// Squares inside somebody else's merge. Bounded by [`MAX_GRID_CELLS`]:
    /// overlapping merges are malformed, so a set larger than the box is a
    /// crafted file rather than a spreadsheet.
    covered: HashSet<(u32, u32)>,
}

impl MergeMap {
    fn new(merges: &[Dimensions], row0: u32, col0: u32, row1: u32, col1: u32) -> Self {
        let mut anchors = HashMap::new();
        let mut covered = HashSet::new();
        for d in merges {
            let (ar, ac) = d.start;
            if ar < row0 || ac < col0 || ar > row1 || ac > col1 {
                continue;
            }
            anchors.insert(
                (ar, ac),
                (
                    d.end
                        .0
                        .saturating_sub(ar)
                        .saturating_add(1)
                        .min(MAX_MERGE_SPAN),
                    d.end
                        .1
                        .saturating_sub(ac)
                        .saturating_add(1)
                        .min(MAX_MERGE_SPAN),
                ),
            );
            for r in ar..=d.end.0.min(row1) {
                for c in ac..=d.end.1.min(col1) {
                    if (r, c) == (ar, ac) {
                        continue;
                    }
                    if covered.len() >= MAX_GRID_CELLS {
                        return Self { anchors, covered };
                    }
                    covered.insert((r, c));
                }
            }
        }
        Self { anchors, covered }
    }

    /// Span of the cell anchored at `(row, col)`; `(1, 1)` when it is ordinary.
    fn at(&self, row: u32, col: u32) -> (u32, u32) {
        self.anchors.get(&(row, col)).copied().unwrap_or((1, 1))
    }

    /// Whether `(row, col)` is inside a merge it does not anchor.
    fn is_covered(&self, row: u32, col: u32) -> bool {
        self.covered.contains(&(row, col))
    }
}

fn bounding_box(cells: &[RawCell]) -> (u32, u32, u32, u32) {
    let mut row0 = u32::MAX;
    let mut col0 = u32::MAX;
    let mut row1 = 0;
    let mut col1 = 0;
    for c in cells {
        row0 = row0.min(c.row);
        col0 = col0.min(c.col);
        row1 = row1.max(c.row);
        col1 = col1.max(c.col);
    }
    (row0, col0, row1, col1)
}

fn sheet_span(sheet: &str, range: &str) -> SourceSpan {
    SourceSpan::Cells {
        sheet: sheet.to_owned(),
        range: range.to_owned(),
    }
}

/// A cell's value as text (**TBL-005**).
///
/// The rendering is chosen so that [`crate::table`]'s classifier reaches the
/// same conclusion the workbook already holds — a date comes back as ISO-8601
/// because that is what `classify` reads as a date, a boolean as `TRUE` because
/// that is what a spreadsheet shows. That is not a coincidence to be relied on
/// quietly: it is how the inheritance in TBL-001 is *paid for*. The parser's
/// obligation is to render the value honestly, and honest rendering is what
/// makes re-deriving the type from text agree with the type the file declared.
fn cell_text(v: &DataRef<'_>) -> String {
    match v {
        DataRef::Empty => String::new(),
        DataRef::String(s) => s.clone(),
        DataRef::SharedString(s) => (*s).to_owned(),
        DataRef::Int(i) => i.to_string(),
        DataRef::Float(f) => number_text(*f),
        // What Excel shows, and what `classify` reads as a boolean.
        DataRef::Bool(x) => if *x { "TRUE" } else { "FALSE" }.to_owned(),
        DataRef::DateTime(dt) => datetime_text(dt),
        DataRef::DateTimeIso(s) | DataRef::DurationIso(s) => s.clone(),
        // `#DIV/0!`. An error cell keeps the text the sheet displays: it is
        // information about the workbook, and dropping it would leave a hole
        // that reads as an empty cell.
        DataRef::Error(e) => e.to_string(),
    }
}

fn number_text(f: f64) -> String {
    if !f.is_finite() {
        return String::new();
    }
    if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{}", f as i64)
    } else {
        format!("{f}")
    }
}

/// An Excel serial as ISO-8601.
///
/// The serial is not preserved separately because the ISO form is a lossless
/// re-encoding of it to the millisecond Excel itself stores, and `45943` is not
/// a string any citation should show a human.
fn datetime_text(dt: &ExcelDateTime) -> String {
    if dt.is_duration() {
        // A duration has no calendar date to render. `[h]:mm:ss` is what the
        // sheet shows and it stays text — there is no `duration` column type,
        // and inventing one for a case no file has asked for is §99's scope
        // discipline in miniature.
        let total_ms = (dt.as_f64() * 86_400_000.0).round().max(0.0) as u64;
        let (h, m, s) = (
            total_ms / 3_600_000,
            (total_ms / 60_000) % 60,
            (total_ms / 1_000) % 60,
        );
        return format!("{h}:{m:02}:{s:02}");
    }
    let (y, mo, d, h, mi, s, _ms) = dt.to_ymd_hms_milli();
    if h == 0 && mi == 0 && s == 0 {
        format!("{y:04}-{mo:02}-{d:02}")
    } else {
        format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}")
    }
}

/// A workbook that will not open.
///
/// There is no `PAR_ENCRYPTED` in the §108 taxonomy and `marrow-core` is not
/// mine to change, so a password-protected file reports `PAR_CORRUPT` with a
/// message that says what actually happened. `PAR_UNSUPPORTED` would be the
/// wrong shape: it makes the router fall through *silently*, and "your workbook
/// has a password" is precisely the thing the user needs told.
fn open_error(e: XlsxError) -> Error {
    let message = if matches!(e, XlsxError::Password) {
        "This workbook is password-protected, so its contents cannot be read. It stays \
         findable by name; remove the password and re-index to search inside it."
    } else {
        "This workbook could not be opened. It may be damaged, or may not be an XLSX file \
         despite its name; it stays findable by name."
    };
    Error::new(Code::ParCorrupt, message).with_context(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::{BudgetGuard, Budgets};
    use crate::ooxml::test_zip::zip_of;
    use crate::table::{tables_in, ColumnType, Reconstruction};

    /// A workbook, assembled part by part.
    ///
    /// Hand-built rather than produced by a writer crate: the tests below are
    /// about `styles.xml` deciding what a number *means*, and a fixture that
    /// hides that decision behind a builder API would test the builder.
    struct Book {
        sheets: Vec<(String, String)>,
        strings: Vec<String>,
        defined: Vec<(String, String)>,
    }

    impl Book {
        fn new() -> Self {
            Self {
                sheets: Vec::new(),
                strings: Vec::new(),
                defined: Vec::new(),
            }
        }

        fn sheet(mut self, name: &str, body: &str) -> Self {
            self.sheets.push((name.to_owned(), body.to_owned()));
            self
        }

        fn shared(mut self, s: &[&str]) -> Self {
            self.strings = s.iter().map(|x| (*x).to_owned()).collect();
            self
        }

        fn defined_name(mut self, name: &str, reference: &str) -> Self {
            self.defined.push((name.to_owned(), reference.to_owned()));
            self
        }

        fn build(&self) -> Vec<u8> {
            let mut sheet_tags = String::new();
            let mut rels = String::new();
            let mut types = String::new();
            let mut parts: Vec<(String, Vec<u8>)> = Vec::new();
            for (i, (name, body)) in self.sheets.iter().enumerate() {
                let n = i + 1;
                sheet_tags.push_str(&format!(
                    "<sheet name=\"{name}\" sheetId=\"{n}\" r:id=\"rId{n}\"/>"
                ));
                rels.push_str(&format!(
                    "<Relationship Id=\"rId{n}\" Type=\"http://schemas.openxmlformats.org/\
                     officeDocument/2006/relationships/worksheet\" \
                     Target=\"worksheets/sheet{n}.xml\"/>"
                ));
                types.push_str(&format!(
                    "<Override PartName=\"/xl/worksheets/sheet{n}.xml\" ContentType=\"\
                     application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>"
                ));
                parts.push((
                    format!("xl/worksheets/sheet{n}.xml"),
                    format!(
                        "<?xml version=\"1.0\"?><worksheet xmlns=\"http://schemas.openxmlformats\
                         .org/spreadsheetml/2006/main\"><sheetData>{body}</sheetData></worksheet>"
                    )
                    .into_bytes(),
                ));
            }
            let defined = if self.defined.is_empty() {
                String::new()
            } else {
                let inner: String = self
                    .defined
                    .iter()
                    .map(|(n, r)| format!("<definedName name=\"{n}\">{r}</definedName>"))
                    .collect();
                format!("<definedNames>{inner}</definedNames>")
            };
            let sst: String = self
                .strings
                .iter()
                .map(|s| format!("<si><t>{s}</t></si>"))
                .collect();

            let mut all: Vec<(String, Vec<u8>)> = vec![
                (
                    "[Content_Types].xml".into(),
                    format!(
                        "<?xml version=\"1.0\"?><Types xmlns=\"http://schemas.openxmlformats.org/\
                         package/2006/content-types\"><Default Extension=\"xml\" ContentType=\"\
                         application/xml\"/><Override PartName=\"/xl/workbook.xml\" ContentType=\
                         \"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.\
                         main+xml\"/>{types}</Types>"
                    )
                    .into_bytes(),
                ),
                (
                    "_rels/.rels".into(),
                    b"<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats\
                      .org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://\
                      schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\
                      \" Target=\"xl/workbook.xml\"/></Relationships>"
                        .to_vec(),
                ),
                (
                    "xl/workbook.xml".into(),
                    format!(
                        "<?xml version=\"1.0\"?><workbook xmlns=\"http://schemas.openxmlformats\
                         .org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats\
                         .org/officeDocument/2006/relationships\"><sheets>{sheet_tags}</sheets>\
                         {defined}</workbook>"
                    )
                    .into_bytes(),
                ),
                (
                    "xl/_rels/workbook.xml.rels".into(),
                    format!(
                        "<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas\
                         .openxmlformats.org/package/2006/relationships\">{rels}<Relationship \
                         Id=\"rIdS\" Type=\"http://schemas.openxmlformats.org/officeDocument/\
                         2006/relationships/sharedStrings\" Target=\"sharedStrings.xml\"/>\
                         <Relationship Id=\"rIdY\" Type=\"http://schemas.openxmlformats.org/\
                         officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>\
                         </Relationships>"
                    )
                    .into_bytes(),
                ),
                (
                    "xl/sharedStrings.xml".into(),
                    format!(
                        "<?xml version=\"1.0\"?><sst xmlns=\"http://schemas.openxmlformats.org/\
                         spreadsheetml/2006/main\" count=\"{}\" uniqueCount=\"{}\">{sst}</sst>",
                        self.strings.len(),
                        self.strings.len()
                    )
                    .into_bytes(),
                ),
                // `numFmtId="14"` is the built-in `mm-dd-yy`. Style index 1
                // therefore means "this number is a date", which is the whole
                // reason `calamine` is here.
                (
                    "xl/styles.xml".into(),
                    b"<?xml version=\"1.0\"?><styleSheet xmlns=\"http://schemas.openxmlformats\
                      .org/spreadsheetml/2006/main\"><cellXfs count=\"2\"><xf numFmtId=\"0\"/>\
                      <xf numFmtId=\"14\" applyNumberFormat=\"1\"/></cellXfs></styleSheet>"
                        .to_vec(),
                ),
            ];
            all.extend(parts);
            let borrowed: Vec<(&str, &[u8])> = all
                .iter()
                .map(|(n, b)| (n.as_str(), b.as_slice()))
                .collect();
            zip_of(&borrowed)
        }
    }

    /// `<c>` for a shared string by index.
    fn s(cell: &str, idx: usize) -> String {
        format!("<c r=\"{cell}\" t=\"s\"><v>{idx}</v></c>")
    }
    /// `<c>` for a number.
    fn n(cell: &str, v: &str) -> String {
        format!("<c r=\"{cell}\"><v>{v}</v></c>")
    }
    /// `<c>` for a number carrying a formula.
    fn f(cell: &str, formula: &str, v: &str) -> String {
        format!("<c r=\"{cell}\"><f>{formula}</f><v>{v}</v></c>")
    }
    /// `<c>` for a date serial, styled as a date.
    fn date(cell: &str, serial: &str) -> String {
        format!("<c r=\"{cell}\" s=\"1\"><v>{serial}</v></c>")
    }
    fn row(n: u32, cells: &[String]) -> String {
        format!("<row r=\"{n}\">{}</row>", cells.concat())
    }

    fn parse(bytes: &[u8]) -> Result<ParsedArtifact> {
        let probe = FileProbe::new("book.xlsx", bytes.len() as u64);
        XlsxParser.parse(ParseInput {
            bytes,
            probe: &probe,
            budget: BudgetGuard::new(Budgets::default()),
        })
    }

    fn one_table(bytes: &[u8]) -> crate::table::TableIr {
        let a = parse(bytes).expect("fixture must parse");
        a.validate().expect("fixture must validate");
        let mut t = tables_in(&a);
        assert_eq!(t.len(), 1, "expected exactly one table");
        t.remove(0)
    }

    /// A three-column sheet: header, then two rows of data.
    fn simple_book() -> Vec<u8> {
        Book::new()
            .shared(&["part", "qty", "price", "bolt", "nut"])
            .sheet(
                "Parts",
                &[
                    row(1, &[s("A1", 0), s("B1", 1), s("C1", 2)]),
                    row(2, &[s("A2", 3), n("B2", "12"), n("C2", "0.4")]),
                    row(3, &[s("A3", 4), n("B3", "144"), n("C3", "0.02")]),
                ]
                .concat(),
            )
            .build()
    }

    #[test]
    fn a_worksheet_becomes_the_same_table_ir_as_the_equivalent_csv() {
        // **TBL-001, and the claim this whole parser was written to test.**
        // Nothing in `xlsx.rs` detects a header or infers a type; if the two
        // shapes ever diverge, they diverge in `table.rs` and both move.
        let xlsx = one_table(&simple_book());

        let probe = FileProbe::new("parts.csv", 0);
        let csv_src = "part,qty,price\nbolt,12,0.4\nnut,144,0.02\n";
        let csv = crate::csv::CsvParser
            .parse(ParseInput {
                bytes: csv_src.as_bytes(),
                probe: &probe,
                budget: BudgetGuard::new(Budgets::default()),
            })
            .unwrap();
        let csv = tables_in(&csv).remove(0);

        assert_eq!(xlsx.n_rows, csv.n_rows);
        assert_eq!(xlsx.n_cols, csv.n_cols);
        assert_eq!(xlsx.header.row, csv.header.row);
        assert_eq!(xlsx.header.confidence, csv.header.confidence);
        assert_eq!(xlsx.column_names, csv.column_names);
        assert_eq!(xlsx.column_types, csv.column_types);
        assert_eq!(xlsx.reconstruction, csv.reconstruction);
        // Only *where* differs, which is the entire point of a span.
        assert!(matches!(xlsx.cells[0].span, SourceSpan::Cells { .. }));
        assert!(matches!(csv.cells[0].span, SourceSpan::Bytes { .. }));
    }

    #[test]
    fn every_cell_is_cited_by_the_address_the_workbook_uses() {
        // TBL-002. `Sheet1!B4` resolves in Excel's own name box; a byte offset
        // into a deflated zip member does not resolve anywhere.
        let t = one_table(&simple_book());
        for c in &t.cells {
            let SourceSpan::Cells { sheet, range } = &c.span else {
                panic!(
                    "a workbook cell must carry a cell reference, not {:?}",
                    c.span
                );
            };
            assert_eq!(sheet, "Parts");
            assert_eq!(
                a1::parse_range_ref(range).map(|(r, col, _, _)| (r, col)),
                Some((c.row, c.col)),
                "the address must resolve back to the cell it names"
            );
        }
        assert_eq!(t.cell(1, 1).unwrap().raw_text, "12");
        let SourceSpan::Cells { range, .. } = &t.cell(1, 1).unwrap().span else {
            unreachable!()
        };
        assert_eq!(range, "B2");
    }

    #[test]
    fn a_date_is_read_as_a_date_and_not_as_a_five_digit_number() {
        // The reason `calamine` is a dependency. `45943` is 2025-10-13, and
        // only `styles.xml` says so.
        let bytes = Book::new()
            .shared(&["when", "amount"])
            .sheet(
                "Log",
                &[
                    row(1, &[s("A1", 0), s("B1", 1)]),
                    row(2, &[date("A2", "45943"), n("B2", "10")]),
                    row(3, &[date("A3", "45944"), n("B3", "20")]),
                ]
                .concat(),
            )
            .build();
        let t = one_table(&bytes);
        assert_eq!(t.column_types[0], ColumnType::Date, "{:?}", t.column_types);
        // TBL-005: the raw text is the ISO form a citation can show, not the
        // serial and not a locale-formatted string.
        assert_eq!(t.cell(1, 0).unwrap().raw_text, "2025-10-13");
    }

    #[test]
    fn a_formula_is_preserved_beside_its_value_not_instead_of_it() {
        // TBL-007. The cached result is what §99.3 computes over; the formula
        // is where that number came from.
        let bytes = Book::new()
            .shared(&["qty", "price", "total"])
            .sheet(
                "Order",
                &[
                    row(1, &[s("A1", 0), s("B1", 1), s("C1", 2)]),
                    row(2, &[n("A2", "3"), n("B2", "4"), f("C2", "A2*B2", "12")]),
                    row(3, &[n("A3", "5"), n("B3", "6"), f("C3", "A3*B3", "30")]),
                ]
                .concat(),
            )
            .build();
        let t = one_table(&bytes);
        let cell = t.cell(1, 2).unwrap();
        assert_eq!(cell.formula.as_deref(), Some("A2*B2"));
        assert_eq!(cell.raw_text, "12", "the value stays the value");
        assert_eq!(
            t.column_types[2],
            ColumnType::Integer,
            "a computed column is still a numeric column"
        );
        assert!(
            t.cell(1, 0).unwrap().formula.is_none(),
            "a literal cell has no formula"
        );
    }

    #[test]
    fn named_ranges_are_preserved_and_reach_the_schema_chunk() {
        // TBL-007 for the declared half, and TBL-012: the schema chunk is where
        // a named range becomes searchable.
        let bytes = Book::new()
            .shared(&["month", "revenue"])
            .sheet(
                "Q1",
                &[
                    row(1, &[s("A1", 0), s("B1", 1)]),
                    row(2, &[s("A2", 0), n("B2", "100")]),
                    row(3, &[s("A3", 1), n("B3", "200")]),
                ]
                .concat(),
            )
            .defined_name("Revenue", "Q1!$B$2:$B$3")
            .defined_name("TAX_RATE", "0.2")
            .defined_name("_xlnm.Print_Area", "Q1!$A$1:$B$3")
            .build();
        let t = one_table(&bytes);
        assert_eq!(t.named_ranges.len(), 1, "{:?}", t.named_ranges);
        let r = &t.named_ranges[0];
        assert_eq!(r.name, "Revenue");
        assert_eq!(r.target, "Q1!$B$2:$B$3");
        assert_eq!(
            r.span,
            SourceSpan::Cells {
                sheet: "Q1".into(),
                range: "B2:B3".into()
            },
            "a name is stored with the address it resolves to"
        );
        let schema = crate::table::schema_text(&t, "");
        assert!(schema.contains("Revenue (Q1!$B$2:$B$3)"), "{schema}");
    }

    #[test]
    fn each_sheet_is_its_own_table_captioned_by_its_name() {
        let bytes = Book::new()
            .shared(&["a", "b"])
            .sheet(
                "First",
                &[
                    row(1, &[s("A1", 0), s("B1", 1)]),
                    row(2, &[n("A2", "1"), n("B2", "2")]),
                ]
                .concat(),
            )
            .sheet(
                "Second",
                &[
                    row(1, &[s("A1", 0), s("B1", 1)]),
                    row(2, &[n("A2", "3"), n("B2", "4")]),
                ]
                .concat(),
            )
            .build();
        let a = parse(&bytes).unwrap();
        a.validate().unwrap();
        let tables = tables_in(&a);
        assert_eq!(tables.len(), 2);
        assert_eq!(tables[0].caption.as_deref(), Some("First"));
        assert_eq!(tables[1].caption.as_deref(), Some("Second"));
        assert_eq!(tables[1].cell(1, 0).unwrap().raw_text, "3");
    }

    #[test]
    fn a_sheet_that_does_not_start_at_a1_is_anchored_without_losing_its_address() {
        // Real workbooks start at C5 all the time. The grid must be zero-based
        // so header detection sees row 0, and the citation must still say C5.
        let bytes = Book::new()
            .shared(&["name", "score"])
            .sheet(
                "Report",
                &[
                    row(5, &[s("C5", 0), s("D5", 1)]),
                    row(6, &[s("C6", 0), n("D6", "7")]),
                    row(7, &[s("C7", 1), n("D7", "9")]),
                ]
                .concat(),
            )
            .build();
        let t = one_table(&bytes);
        assert_eq!(t.n_cols, 2);
        assert_eq!(t.header.row, Some(0), "{:?}", t.header);
        let SourceSpan::Cells { range, .. } = &t.cell(0, 0).unwrap().span else {
            unreachable!()
        };
        assert_eq!(range, "C5", "grid row 0 is still sheet row 5");
    }

    #[test]
    fn a_merged_header_keeps_its_region_and_leaves_no_second_cell() {
        // TBL-004. The covered squares get no cell of their own, exactly as in
        // the HTML `rowspan` case.
        let body = format!(
            "{}{}{}",
            row(1, &[s("A1", 0)]),
            row(2, &[s("A2", 1), s("B2", 2)]),
            row(3, &[n("A3", "1"), n("B3", "2")])
        );
        let mut bytes_book = Book::new().shared(&["Region totals", "north", "south"]);
        bytes_book = bytes_book.sheet("M", &body);
        let mut bytes = bytes_book.build();
        // Splice a `mergeCells` block into the sheet part.
        bytes = with_merge(&bytes, "xl/worksheets/sheet1.xml", "A1:B1");

        let t = one_table(&bytes);
        assert_eq!(t.merged_regions.len(), 1, "{:?}", t.merged_regions);
        assert_eq!(t.merged_regions[0].colspan, 2);
        assert!(t.cell(0, 1).is_none(), "covered by the merge to its left");
        assert_eq!(t.cell(0, 0).unwrap().raw_text, "Region totals");
        assert_eq!(t.reconstruction, Reconstruction::Exact);
    }

    /// Rewrite one sheet part to carry a `<mergeCells>` block.
    fn with_merge(zip_bytes: &[u8], part: &str, range: &str) -> Vec<u8> {
        let xml = String::from_utf8(ooxml::read_part(zip_bytes, part).unwrap().unwrap()).unwrap();
        let merged = xml.replace(
            "</sheetData>",
            &format!(
                "</sheetData><mergeCells count=\"1\"><mergeCell ref=\"{range}\"/></mergeCells>"
            ),
        );
        // Rebuild the archive with the patched part.
        let mut zip = zip::ZipArchive::new(Cursor::new(zip_bytes)).unwrap();
        let mut parts: Vec<(String, Vec<u8>)> = Vec::new();
        for i in 0..zip.len() {
            let mut e = zip.by_index(i).unwrap();
            let name = e.name().to_owned();
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut e, &mut buf).unwrap();
            if name == part {
                buf = merged.clone().into_bytes();
            }
            parts.push((name, buf));
        }
        let borrowed: Vec<(&str, &[u8])> = parts
            .iter()
            .map(|(n, b)| (n.as_str(), b.as_slice()))
            .collect();
        zip_of(&borrowed)
    }

    #[test]
    fn an_error_cell_keeps_the_text_the_sheet_shows() {
        let bytes = Book::new()
            .shared(&["a", "b"])
            .sheet(
                "E",
                &[
                    row(1, &[s("A1", 0), s("B1", 1)]),
                    row(
                        2,
                        &[
                            n("A2", "1"),
                            "<c r=\"B2\" t=\"e\"><f>1/0</f><v>#DIV/0!</v></c>".to_owned(),
                        ],
                    ),
                    row(3, &[n("A3", "2"), n("B3", "5")]),
                ]
                .concat(),
            )
            .build();
        let t = one_table(&bytes);
        assert_eq!(t.cell(1, 1).unwrap().raw_text, "#DIV/0!");
        assert_eq!(t.cell(1, 1).unwrap().formula.as_deref(), Some("1/0"));
    }

    #[test]
    fn a_file_named_xlsx_that_is_not_a_zip_falls_through_the_chain() {
        // FS-014. `ParUnsupported` is the code that makes the router try the
        // next parser silently rather than reporting a broken file.
        let e = parse(b"%PDF-1.7 not really a workbook").unwrap_err();
        assert_eq!(e.code(), Code::ParUnsupported);
    }

    #[test]
    fn a_sparse_sheet_is_not_filled_in_and_says_so() {
        // Two real cells 50,000 rows apart. Filling that box is 100,002 cells
        // of invention to hold two facts; `DEGRADED` is the honest description
        // of a grid we did not fill in, and it is what TBL-018 asks for.
        let far_row = 50_000u32;
        let far_cell = format!(
            "<row r=\"{}\">{}</row>",
            far_row + 1,
            n(&a1::cell_ref(far_row, 1), "5")
        );
        let body = format!("{}{far_cell}", row(1, &[s("A1", 0), s("B1", 1)]));
        let bytes = Book::new()
            .shared(&["x", "y"])
            .sheet("Sparse", &body)
            .build();
        let t = one_table(&bytes);
        assert_eq!(t.n_rows, far_row + 1);
        assert!(
            t.cells.len() < 10,
            "the box must not be materialised: {} cells",
            t.cells.len()
        );
        assert_eq!(t.reconstruction, Reconstruction::Degraded);
    }

    #[test]
    fn a_cell_address_outside_excels_own_range_is_dropped_rather_than_cited() {
        // There is no `B1048577`, so there is no citation for it. The rest of
        // the sheet is unaffected.
        let bytes = Book::new()
            .shared(&["a", "b"])
            .sheet(
                "Bad",
                &format!(
                    "{}{}{}",
                    row(1, &[s("A1", 0), s("B1", 1)]),
                    row(2, &[n("A2", "1"), n("B2", "2")]),
                    "<row r=\"2000000\"><c r=\"A2000000\"><v>9</v></c></row>"
                ),
            )
            .build();
        let t = one_table(&bytes);
        assert_eq!(t.n_rows, 2, "{:?}", t.cells);
    }

    #[test]
    fn numbers_render_without_inventing_precision() {
        assert_eq!(number_text(12.0), "12");
        assert_eq!(number_text(0.5), "0.5");
        assert_eq!(number_text(f64::NAN), "");
    }

    #[test]
    fn a_percentage_is_stored_as_the_value_the_file_holds() {
        // The number-format finding, pinned. Excel shows `42%`; the file holds
        // 0.42 and so do we. Rendering `42%` would need the format code, which
        // `calamine` does not expose — inventing it is how a ratio silently
        // becomes a percentage in an arithmetic answer (TBL-009).
        let bytes = Book::new()
            .shared(&["metric", "share"])
            .sheet(
                "P",
                &[
                    row(1, &[s("A1", 0), s("B1", 1)]),
                    row(2, &[s("A2", 0), n("B2", "0.42")]),
                    row(3, &[s("A3", 1), n("B3", "0.13")]),
                ]
                .concat(),
            )
            .build();
        let t = one_table(&bytes);
        assert_eq!(t.cell(1, 1).unwrap().raw_text, "0.42");
        assert_eq!(t.column_types[1], ColumnType::Decimal);
    }
}
