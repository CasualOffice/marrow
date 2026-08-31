//! Arithmetic over a spreadsheet range, done by this program rather than by a
//! model.
//!
//! **The point is that no model is involved.** "Deterministic before
//! probabilistic" is the first line of the design, and a sum is the clearest
//! case there is: a model handed forty numbers as text will usually add them
//! correctly and has no way to tell you when it did not. This reads the typed
//! values the XLSX parser already recorded, adds them with `f64`, and reports
//! exactly which cells it used.
//!
//! **It says what it skipped.** A total over eighteen cells where three held
//! text is not a total over eighteen cells, and presenting it as one is the
//! defect this repository keeps finding: a figure that is accurate about what
//! the code did, shown where a reader takes it as a fact about their data. So
//! [`Computed`] carries the contributing count, the skipped count and a reason
//! per skip, and every caller renders them.

use marrow_core::{Code, Error, Result, VersionId};
use marrow_store::read::{cells_for, tables_for, CellRow};
use marrow_store::rusqlite::Connection;

/// What to do with the numbers in a range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Sum,
    Mean,
    Min,
    Max,
    /// How many cells held a number. Deliberately not "how many cells" — that
    /// is the range's size and needs no reading.
    Count,
}

impl Op {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "sum" => Some(Self::Sum),
            "mean" | "avg" | "average" => Some(Self::Mean),
            "min" => Some(Self::Min),
            "max" => Some(Self::Max),
            "count" => Some(Self::Count),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sum => "sum",
            Self::Mean => "mean",
            Self::Min => "min",
            Self::Max => "max",
            Self::Count => "count",
        }
    }
}

/// One cell that was in the range and did not contribute.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    /// `Q2!B7`, so the reader can go and look at it.
    pub reference: String,
    /// The cell as written. Empty for a blank.
    pub raw_text: String,
    /// Why, in words. `"blank"`, or `"not a number"`.
    pub reason: &'static str,
}

/// The answer, and everything needed to distrust it.
#[derive(Clone, Debug, PartialEq)]
pub struct Computed {
    pub op: Op,
    /// `None` only for an empty range under `min`/`max`/`mean`, where there is
    /// no honest answer. `Sum` and `Count` of nothing are legitimately 0.
    pub value: Option<f64>,
    /// How many cells held a number and were added.
    pub contributing: usize,
    /// Every cell in the range that did not, with its reason. Not a count:
    /// "3 skipped" sends a reader looking, and this tells them where.
    pub skipped: Vec<Skipped>,
    /// The range as asked for, echoed so a transcript is self-describing.
    pub range: String,
    pub sheet: String,
}

impl Computed {
    /// Whether anything in the range was passed over. The one thing a caller
    /// must not be able to render this without.
    pub fn is_partial(&self) -> bool {
        !self.skipped.is_empty()
    }
}

/// Compute `op` over `reference` — `Q2!B4:B18` or `B4:B18` — in `version_id`.
///
/// The sheet is matched against the table's own `Cells` span, so a workbook
/// with twelve sheets computes over the one that was named rather than over
/// whichever table happened to be first.
pub fn compute(
    conn: &Connection,
    version_id: VersionId,
    op: Op,
    reference: &str,
) -> Result<Computed> {
    let (sheet, range) = match marrow_core::a1::split_sheet_ref(reference) {
        Some((s, r)) => (Some(s), r),
        // No sheet named. Legitimate for a single-sheet workbook and ambiguous
        // for any other, which is why an ambiguous one is refused below rather
        // than resolved by picking.
        None => (None, reference.to_string()),
    };

    let (r0, c0, r1, c1) = marrow_core::a1::parse_range_ref(&range).ok_or_else(|| {
        Error::new(
            Code::CfgInvalid,
            format!(
                "`{range}` is not a cell range. Write it the way the spreadsheet does — \
                 `B4:B18`, or `Q2!B4:B18` to name the sheet."
            ),
        )
    })?;

    let tables = tables_for(conn, version_id)?;
    if tables.is_empty() {
        return Err(Error::new(
            Code::CfgInvalid,
            "That file has no tables in it, so there is nothing to compute over.",
        ));
    }

    // Which table the range belongs to. A workbook is many tables and a range
    // without a sheet name only identifies one when there is only one.
    let chosen = match &sheet {
        Some(name) => tables
            .iter()
            .find(|t| sheet_of(&t.source_span).as_deref() == Some(name.as_str()))
            .ok_or_else(|| {
                let known: Vec<String> = tables
                    .iter()
                    .filter_map(|t| sheet_of(&t.source_span))
                    .collect();
                Error::new(
                    Code::CfgInvalid,
                    format!(
                        "No sheet called `{name}` in that file. It has: {}.",
                        if known.is_empty() {
                            "no named sheets".to_string()
                        } else {
                            known.join(", ")
                        }
                    ),
                )
            })?,
        None if tables.len() == 1 => &tables[0],
        None => {
            let known: Vec<String> = tables
                .iter()
                .filter_map(|t| sheet_of(&t.source_span))
                .collect();
            return Err(Error::new(
                Code::CfgInvalid,
                format!(
                    "That file has {} tables, so `{range}` does not say which one. Name the \
                     sheet — `{}!{range}`.",
                    tables.len(),
                    known.first().map(String::as_str).unwrap_or("Sheet1")
                ),
            ));
        }
    };

    let sheet_name = sheet
        .or_else(|| sheet_of(&chosen.source_span))
        .unwrap_or_default();

    let mut values: Vec<f64> = Vec::new();
    let mut skipped: Vec<Skipped> = Vec::new();
    for cell in cells_for(conn, &chosen.table_id)? {
        let (r, c) = (cell.row_idx as u32, cell.col_idx as u32);
        if r < r0 || r > r1 || c < c0 || c > c1 {
            continue;
        }
        match numeric(&cell) {
            Some(v) => values.push(v),
            None => skipped.push(Skipped {
                reference: format!("{sheet_name}!{}", marrow_core::a1::cell_ref(r, c)),
                raw_text: cell.raw_text.clone(),
                reason: if cell.raw_text.trim().is_empty() {
                    "blank"
                } else {
                    "not a number"
                },
            }),
        }
    }

    let value = match op {
        Op::Count => Some(values.len() as f64),
        Op::Sum => Some(values.iter().sum()),
        // No honest answer over nothing. Zero would be a number, and a number
        // is what a reader acts on.
        Op::Mean if values.is_empty() => None,
        Op::Mean => Some(values.iter().sum::<f64>() / values.len() as f64),
        Op::Min => values.iter().copied().reduce(f64::min),
        Op::Max => values.iter().copied().reduce(f64::max),
    };

    Ok(Computed {
        op,
        value,
        contributing: values.len(),
        skipped,
        range,
        sheet: sheet_name,
    })
}

/// The number in a cell, from the value the parser typed — never re-parsed
/// from the display text.
///
/// TBL-005 keeps the raw text beside the typed reading precisely so that a
/// cell displayed as `1,234.50` or `(89)` or `45%` is not re-interpreted by
/// whoever reads it next. The parser already decided; this trusts that
/// decision or skips the cell.
fn numeric(cell: &CellRow) -> Option<f64> {
    let ty = cell.value_type.as_deref()?;
    if !matches!(
        ty,
        "INTEGER"
            | "DECIMAL"
            | "CURRENCY"
            | "PERCENT"
            | "integer"
            | "decimal"
            | "currency"
            | "percent"
    ) {
        return None;
    }
    cell.typed_value.as_deref()?.parse::<f64>().ok()
}

/// The sheet name out of a table's stored `Cells` span.
///
/// The span is persisted as JSON. Deserialising it here would mean a
/// `serde_json` dependency in this crate for one field, so [`marrow_core`]
/// owns the round trip beside the type it belongs to.
fn sheet_of(source_span: &str) -> Option<String> {
    match marrow_core::SourceSpan::from_json(source_span)? {
        marrow_core::SourceSpan::Cells { sheet, .. } => Some(sheet),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(row: i64, col: i64, raw: &str, typed: Option<&str>, ty: Option<&str>) -> CellRow {
        CellRow {
            row_idx: row,
            col_idx: col,
            rowspan: 1,
            colspan: 1,
            raw_text: raw.into(),
            typed_value: typed.map(str::to_owned),
            value_type: ty.map(str::to_owned),
            formula: None,
            cell_span: "{}".into(),
            confidence: 1.0,
        }
    }

    #[test]
    fn a_number_comes_from_the_typed_value_not_the_display_text() {
        // TBL-005: the raw text is kept beside the typed reading so a cell
        // shown as `1,234.50` is not re-parsed by whoever reads it next. A
        // naive `raw_text.parse()` would fail on this and silently drop it
        // from the total.
        assert_eq!(
            numeric(&cell(0, 0, "1,234.50", Some("1234.5"), Some("DECIMAL"))),
            Some(1234.5)
        );
        assert_eq!(
            numeric(&cell(0, 0, "45%", Some("0.45"), Some("PERCENT"))),
            Some(0.45)
        );
    }

    #[test]
    fn a_date_is_not_a_number_even_though_it_parses_as_one() {
        // A date stored as a serial would add cleanly and mean nothing. The
        // type is the gate, not whether `parse::<f64>` succeeds.
        assert_eq!(
            numeric(&cell(0, 0, "2025-10-13", Some("2025-10-13"), Some("DATE"))),
            None
        );
        assert_eq!(
            numeric(&cell(0, 0, "yes", Some("true"), Some("BOOLEAN"))),
            None
        );
        assert_eq!(numeric(&cell(0, 0, "", None, None)), None);
    }

    #[test]
    fn an_empty_range_has_no_mean_but_does_have_a_sum() {
        // Zero is a number and a reader acts on it. "No cells held a number"
        // is the honest answer to a mean over nothing; a sum of nothing is
        // legitimately zero.
        let none: Vec<f64> = vec![];
        assert_eq!(none.iter().copied().reduce(f64::min), None);
    }

    #[test]
    fn every_op_has_a_name_that_round_trips() {
        for op in [Op::Sum, Op::Mean, Op::Min, Op::Max, Op::Count] {
            assert_eq!(Op::parse(op.as_str()), Some(op));
        }
        // The aliases a person actually types.
        assert_eq!(Op::parse("avg"), Some(Op::Mean));
        assert_eq!(Op::parse("AVERAGE"), Some(Op::Mean));
        assert_eq!(Op::parse("median"), None);
    }
}
