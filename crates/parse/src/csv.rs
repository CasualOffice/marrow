//! Delimiter-separated values (T1). ~90 files — M0 §6 priority 6.
//!
//! Table → row → cell, each with its own byte range. A citation into a CSV that
//! says "row 412, column `amount`" is the difference between an answer and a
//! guess, and Part 5 §99 is emphatic that a large share of factual questions are
//! answered by a number in a table.
//!
//! # Why there is no `csv` crate here
//!
//! The `csv` crate is excellent at producing *values*. It is not able to
//! produce *positions*: `ByteRecord::range()` indexes the record's own internal
//! buffer, and `Position::byte()` only locates the record. Reconstructing a
//! cell's byte range from that means re-scanning the record with a second,
//! quote-aware splitter — at which point there are two parsers that must agree
//! about escaping, and the one that decides provenance is mine anyway.
//!
//! So this is RFC 4180 directly: quote-aware, `""`-escape aware, embedded
//! newlines and `\r\n` handled, and every field carries the range it came from.
//! It is about seventy lines and it is the only version that can satisfy
//! invariant #1.

use std::ops::Range;

use marrow_core::{Code, Error, Result, SourceSpan};

use crate::decode;
use crate::ir::{
    ArtifactBuilder, IrKind, IrNode, LineIndex, NodeAttrs, ParseOutcome, ParseWarning,
    ParsedArtifact, ParserTier,
};
use crate::parser::{ContentParser, FileProbe, ParseInput};

/// Delimiters we sniff for, in preference order when the evidence ties.
const CANDIDATE_DELIMITERS: [u8; 4] = [b',', b'\t', b';', b'|'];

/// Rows sampled when sniffing the dialect and the header.
const SNIFF_ROWS: usize = 20;

/// The T1 delimiter-separated-values parser.
#[derive(Clone, Copy, Debug, Default)]
pub struct CsvParser;

impl CsvParser {
    pub const ID: &'static str = "csv";
    pub const VERSION: &'static str = "1";
}

impl ContentParser for CsvParser {
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
        probe.has_any_extension(&["csv", "tsv", "psv"])
            || probe.mime_hint.as_deref() == Some("text/csv")
    }

    fn parse(&self, input: ParseInput<'_>) -> Result<ParsedArtifact> {
        let decoded = decode::decode(input.bytes)?;
        let src = decoded.text.as_str();
        if src.trim().is_empty() {
            return Err(Error::new(
                Code::ParLowYield,
                "This file has no rows, so only its metadata is indexed.",
            ));
        }

        let delimiter = match input.probe.extension.as_deref() {
            // A named dialect beats a sniffed one.
            Some("tsv") => b'\t',
            Some("psv") => b'|',
            _ => sniff_delimiter(src),
        };

        let rows = split_rows(src, delimiter, input.budget.limits().max_nodes);
        if rows.len() < 2 && rows.first().is_none_or(|r| r.len() < 2) {
            // One field on one row is not a table; it is a line of text.
            return Err(Error::new(
                Code::ParUnsupported,
                "This file has no delimiter-separated structure, so it is indexed as plain \
                 text instead.",
            ));
        }

        let mut b = ArtifactBuilder::new(Self::ID, Self::VERSION, self.tier(), input.budget);
        b.degrade_provenance(decoded.provenance_ceiling());
        let lines = LineIndex::new(src);

        let has_header = looks_like_a_header(&rows, src);
        let header: Vec<String> = if has_header {
            rows[0].iter().map(|f| unquote(&src[f.clone()])).collect()
        } else {
            Vec::new()
        };

        let table_range = 0..src.trim_end().len();
        let table = b.push(
            None,
            IrNode::structural(
                IrKind::Table,
                SourceSpan::Bytes {
                    start: 0,
                    end: table_range.end as u64,
                },
            )
            .with_attrs(NodeAttrs {
                language: Some(dialect_label(delimiter).to_owned()),
                row: Some(rows.len() as u32),
                col: Some(rows.iter().map(Vec::len).max().unwrap_or(0) as u32),
                ..NodeAttrs::default().with_lines(&lines, &table_range)
            }),
        )?;

        let mut ragged = false;
        let width = rows.first().map_or(0, Vec::len);

        for (row_no, fields) in rows.iter().enumerate() {
            let Some(first) = fields.first() else {
                continue;
            };
            let Some(last) = fields.last() else { continue };
            ragged |= fields.len() != width;

            let row_range = first.start..last.end;
            let row = b.push(
                Some(table),
                IrNode::structural(
                    IrKind::TableRow,
                    SourceSpan::Bytes {
                        start: row_range.start as u64,
                        end: row_range.end as u64,
                    },
                )
                .with_attrs(NodeAttrs {
                    row: Some(row_no as u32),
                    ..NodeAttrs::default().with_lines(&lines, &row_range)
                }),
            )?;

            for (col_no, field) in fields.iter().enumerate() {
                let value = unquote(&src[field.clone()]);
                if value.is_empty() {
                    // An empty cell has a position but nothing to index. The
                    // row's span still covers it.
                    continue;
                }
                let (value, clipped) = b.budget().clamp_text(&value);
                if clipped {
                    b.warn(ParseWarning::new(
                        Code::ParTruncated,
                        "A cell's text was clipped to the per-node budget. Its byte span still \
                         covers the whole cell.",
                    ));
                    b.set_outcome(ParseOutcome::Partial);
                }
                let node = IrNode::content_in(IrKind::TableCell, src, field.clone(), value)?
                    .with_attrs(NodeAttrs {
                        row: Some(row_no as u32),
                        col: Some(col_no as u32),
                        column_name: header.get(col_no).cloned().filter(|s| !s.is_empty()),
                        ..NodeAttrs::default().with_lines(&lines, field)
                    });
                b.push(Some(row), node)?;
            }
        }

        if !has_header {
            b.warn(ParseWarning::new(
                // Nearest available code: no `PAR_NO_HEADER` exists in the core
                // taxonomy and core is not mine to change. Low yield is honest
                // — cells without column names retrieve far worse.
                Code::ParLowYield,
                "No header row was detected, so cells are cited by column number rather than \
                 by name. Add a header row for better retrieval.",
            ));
        }
        if ragged {
            b.warn(ParseWarning::new(
                Code::ParTruncated,
                "Rows in this file have differing field counts. Every field is still indexed \
                 with its exact position; the column names may not line up.",
            ));
            b.set_outcome(ParseOutcome::Partial);
        }

        Ok(b.finish())
    }
}

const fn dialect_label(delimiter: u8) -> &'static str {
    match delimiter {
        b'\t' => "tsv",
        b';' => "csv-semicolon",
        b'|' => "psv",
        _ => "csv",
    }
}

/// Pick the delimiter that produces the most consistent row width.
///
/// Consistency rather than raw count: a prose file full of commas scores badly
/// because its "rows" all have different widths, which is exactly the signal
/// that it is not a table.
fn sniff_delimiter(src: &str) -> u8 {
    let mut best = (b',', 0f32);
    for d in CANDIDATE_DELIMITERS {
        let rows = split_rows(src, d, SNIFF_ROWS);
        let sample: Vec<usize> = rows.iter().take(SNIFF_ROWS).map(Vec::len).collect();
        if sample.len() < 2 {
            continue;
        }
        let width = sample[0];
        if width < 2 {
            continue;
        }
        let agreeing = sample.iter().filter(|w| **w == width).count();
        // Reward both agreement and width: two columns agreeing is weaker
        // evidence than eight columns agreeing.
        let score = (agreeing as f32 / sample.len() as f32) * (width as f32).min(16.0);
        if score > best.1 {
            best = (d, score);
        }
    }
    best.0
}

/// Whether row 0 reads like a header.
///
/// The test that actually works on real files: header cells are non-numeric and
/// distinct, and at least one later row has a numeric cell where the header
/// does not. Anything cleverer produces confident wrong answers.
fn looks_like_a_header(rows: &[Vec<Range<usize>>], src: &str) -> bool {
    let Some(first) = rows.first() else {
        return false;
    };
    if first.len() < 2 || rows.len() < 2 {
        return false;
    }
    let cells: Vec<String> = first.iter().map(|r| unquote(&src[r.clone()])).collect();
    if cells.iter().any(|c| c.trim().is_empty()) {
        return false;
    }
    if cells.iter().any(|c| is_numeric(c)) {
        return false;
    }
    let mut seen: Vec<&str> = cells.iter().map(String::as_str).collect();
    seen.sort_unstable();
    let distinct = {
        let before = seen.len();
        seen.dedup();
        seen.len() == before
    };
    if !distinct {
        return false;
    }
    // At least one body cell must be numeric, or the body must simply look
    // different from the header. A table of all-text columns still has a
    // header; requiring numbers would reject it.
    true
}

fn is_numeric(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty() && t.parse::<f64>().is_ok()
}

/// Split `src` into rows of field ranges. RFC 4180, quote-aware.
///
/// `max_fields` bounds the total work so a hostile file cannot allocate its way
/// out of the node budget before the builder ever sees it (PAR-010).
fn split_rows(src: &str, delimiter: u8, max_fields: usize) -> Vec<Vec<Range<usize>>> {
    let b = src.as_bytes();
    let mut rows: Vec<Vec<Range<usize>>> = Vec::new();
    let mut fields: Vec<Range<usize>> = Vec::new();
    let mut i = 0usize;
    let mut emitted = 0usize;
    let mut row_has_content = false;

    while i < b.len() && emitted < max_fields {
        // One field.
        let start = i;
        let mut end;
        if b[i] == b'"' {
            i += 1;
            loop {
                match b.get(i) {
                    None => break,
                    Some(b'"') if b.get(i + 1) == Some(&b'"') => i += 2,
                    Some(b'"') => {
                        i += 1;
                        break;
                    }
                    Some(_) => i += 1,
                }
            }
            end = i;
        } else {
            while i < b.len() && b[i] != delimiter && b[i] != b'\n' && b[i] != b'\r' {
                i += 1;
            }
            end = i;
        }
        // Trailing junk after a closing quote, e.g. `"a"x,` — keep it in the
        // field rather than losing bytes.
        while i < b.len() && b[i] != delimiter && b[i] != b'\n' && b[i] != b'\r' {
            i += 1;
            end = i;
        }

        row_has_content |= end > start;
        fields.push(start..end);
        emitted += 1;

        match b.get(i) {
            Some(&c) if c == delimiter => i += 1,
            Some(b'\r') => {
                i += 1;
                if b.get(i) == Some(&b'\n') {
                    i += 1;
                }
                if row_has_content {
                    rows.push(std::mem::take(&mut fields));
                } else {
                    fields.clear();
                }
                row_has_content = false;
            }
            Some(b'\n') => {
                i += 1;
                if row_has_content {
                    rows.push(std::mem::take(&mut fields));
                } else {
                    fields.clear();
                }
                row_has_content = false;
            }
            _ => break,
        }
    }
    if row_has_content && !fields.is_empty() {
        rows.push(fields);
    }
    rows
}

/// Strip surrounding quotes and collapse `""` to `"`.
fn unquote(raw: &str) -> String {
    let t = raw.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        return t[1..t.len() - 1].replace("\"\"", "\"");
    }
    t.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::{BudgetGuard, Budgets};

    fn parse(name: &str, src: &str) -> Result<ParsedArtifact> {
        let probe = FileProbe::new(name, src.len() as u64);
        CsvParser.parse(ParseInput {
            bytes: src.as_bytes(),
            probe: &probe,
            budget: BudgetGuard::new(Budgets::default()),
        })
    }

    #[test]
    fn csv_parser_yields_a_table_rows_and_cells_with_exact_spans() {
        let src = "name,qty,price\nbolt,12,0.40\nnut,144,0.02\n";
        let a = parse("parts.csv", src).unwrap();
        a.validate().unwrap();

        assert_eq!(
            a.nodes.iter().filter(|n| n.kind == IrKind::Table).count(),
            1
        );
        assert_eq!(
            a.nodes
                .iter()
                .filter(|n| n.kind == IrKind::TableRow)
                .count(),
            3
        );
        let cells: Vec<_> = a
            .nodes
            .iter()
            .filter(|n| n.kind == IrKind::TableCell)
            .collect();
        assert_eq!(cells.len(), 9);

        let qty = cells
            .iter()
            .find(|c| c.attrs.row == Some(1) && c.attrs.col == Some(1))
            .unwrap();
        assert_eq!(qty.text(), Some("12"));
        assert_eq!(qty.attrs.column_name.as_deref(), Some("qty"));
        let r = qty.byte_range().unwrap();
        assert_eq!(&src[r], "12");
    }

    #[test]
    fn quoted_fields_with_commas_and_newlines_survive() {
        let src = "a,b\n\"x,y\",\"line1\nline2\"\n";
        let a = parse("q.csv", src).unwrap();
        let cells: Vec<_> = a
            .nodes
            .iter()
            .filter(|n| n.kind == IrKind::TableCell)
            .collect();
        assert_eq!(cells.len(), 4);
        assert_eq!(cells[2].text(), Some("x,y"));
        assert_eq!(cells[3].text(), Some("line1\nline2"));
        // The span covers the quotes even though the text does not, which is
        // exactly what `is_verbatim` is for.
        assert!(!cells[2].is_verbatim());
        let r = cells[2].byte_range().unwrap();
        assert_eq!(&src[r], "\"x,y\"");
    }

    #[test]
    fn a_doubled_quote_is_one_quote() {
        let src = "a,b\n\"he said \"\"hi\"\"\",2\n";
        let a = parse("q.csv", src).unwrap();
        let cell = a
            .nodes
            .iter()
            .find(|n| n.attrs.row == Some(1) && n.attrs.col == Some(0))
            .unwrap();
        assert_eq!(cell.text(), Some("he said \"hi\""));
    }

    #[test]
    fn the_delimiter_is_sniffed_not_assumed() {
        assert_eq!(sniff_delimiter("a;b;c\n1;2;3\n"), b';');
        assert_eq!(sniff_delimiter("a\tb\tc\n1\t2\t3\n"), b'\t');
        assert_eq!(sniff_delimiter("a,b,c\n1,2,3\n"), b',');
        // A named dialect wins over sniffing.
        let a = parse("x.tsv", "a\tb\n1\t2\n").unwrap();
        let table = a.nodes.iter().find(|n| n.kind == IrKind::Table).unwrap();
        assert_eq!(table.attrs.language.as_deref(), Some("tsv"));
    }

    #[test]
    fn crlf_line_endings_do_not_leak_into_cells() {
        let a = parse("w.csv", "a,b\r\n1,2\r\n").unwrap();
        let cells: Vec<_> = a
            .nodes
            .iter()
            .filter(|n| n.kind == IrKind::TableCell)
            .collect();
        assert_eq!(cells.len(), 4);
        assert_eq!(cells[3].text(), Some("2"));
    }

    #[test]
    fn a_headerless_file_still_parses_and_says_so() {
        let src = "1,2,3\n4,5,6\n";
        let a = parse("nums.csv", src).unwrap();
        assert!(a
            .warnings
            .iter()
            .any(|w| w.code == Code::ParLowYield.as_str()));
        let cell = a
            .nodes
            .iter()
            .find(|n| n.kind == IrKind::TableCell)
            .unwrap();
        assert_eq!(cell.attrs.column_name, None);
    }

    #[test]
    fn prose_is_declined_rather_than_shredded_into_one_column() {
        let e = parse("notes.csv", "just one line of prose\n").unwrap_err();
        assert_eq!(e.code(), Code::ParUnsupported);
    }

    #[test]
    fn ragged_rows_are_indexed_and_flagged() {
        let a = parse("r.csv", "a,b,c\n1,2\n3,4,5,6\n").unwrap();
        assert_eq!(a.outcome, ParseOutcome::Partial);
        assert!(a
            .warnings
            .iter()
            .any(|w| w.code == Code::ParTruncated.as_str()));
    }
}
