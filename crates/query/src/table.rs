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

/// Restrict a computation to the rows a column matches.
///
/// **The point is to narrow a range without retyping it as A1.** "Total the
/// amounts, but only the rows where the category is Rent" is the question
/// people actually have about a table, and expressing it as a cell range
/// requires already knowing which rows those are — which is the thing being
/// asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Where {
    /// The column to test, as a letter: `A`, `B`, `AA`.
    pub column: u32,
    /// Compared against the cell's text, trimmed, ignoring case. Not the typed
    /// value: a filter is a question about what the sheet *says*, and matching
    /// `1200` against a cell displayed `$1,200` would surprise the person who
    /// read the column before typing it.
    pub equals: String,
}

impl Where {
    /// `A=Rent`. The first `=` splits it, so a value may contain one.
    pub fn parse(s: &str) -> Option<Self> {
        let (col, value) = s.split_once('=')?;
        let col = col.trim();
        if col.is_empty() || value.trim().is_empty() {
            return None;
        }
        // `parse_cell_ref` wants a row too, so borrow its column arithmetic by
        // asking about row 1 and keeping the column.
        let (_, column) = marrow_core::a1::parse_cell_ref(&format!("{col}1"))?;
        Some(Self {
            column,
            equals: value.trim().to_owned(),
        })
    }

    fn matches(&self, text: &str) -> bool {
        text.trim().eq_ignore_ascii_case(&self.equals)
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
    /// `None` when no cell in the range held a number.
    ///
    /// **Including `sum`.** The convention that an empty sum is zero is a fact
    /// about arithmetic, not about the user's spreadsheet: a column of `n/a`
    /// totalling `0` reads as "this cost nothing" when it means "there is
    /// nothing here to add", and a reader acts on the first. `count` is the
    /// exception and genuinely is 0 — counting nothing is a real answer to the
    /// question that was asked.
    pub value: Option<f64>,
    /// How many cells held a number and were added.
    pub contributing: usize,
    /// Every cell in the range that did not, with its reason. Not a count:
    /// "3 skipped" sends a reader looking, and this tells them where.
    pub skipped: Vec<Skipped>,
    /// The range as asked for, echoed so a transcript is self-describing.
    pub range: String,
    pub sheet: String,
    /// The filter that narrowed it, if one did. A total of four cells out of a
    /// range of forty is a different claim from a total of four cells, and the
    /// renderer cannot say which without this.
    pub filtered_by: Option<Where>,
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
/// A range, resolved against one table of one file.
///
/// Shared because `compute` and `compute_by` must agree on which sheet a bare
/// `B2:B18` means and on how an ambiguous one is refused. Two copies of that
/// would eventually disagree, and the one that was wrong would be wrong about
/// which numbers a citation refers to.
struct Resolved {
    table_id: String,
    sheet: String,
    range: String,
    bounds: (u32, u32, u32, u32),
}

fn resolve(conn: &Connection, version_id: VersionId, reference: &str) -> Result<Resolved> {
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

    Ok(Resolved {
        table_id: chosen.table_id.clone(),
        sheet: sheet_name,
        range,
        bounds: (r0, c0, r1, c1),
    })
}

/// Compute `op` over `reference` — `Q2!B4:B18` or `B4:B18` — in `version_id`,
/// optionally only over the rows `filter` matches.
pub fn compute(
    conn: &Connection,
    version_id: VersionId,
    op: Op,
    reference: &str,
    filter: Option<&Where>,
) -> Result<Computed> {
    let r = resolve(conn, version_id, reference)?;
    let (r0, c0, r1, c1) = r.bounds;
    let sheet_name = r.sheet;
    let range = r.range;
    let chosen_id = r.table_id;
    let cells = cells_for(conn, &chosen_id)?;

    // **The filter column need not be inside the range.** "Total column B where
    // column A is Rent" is the ordinary shape of the question, so which rows
    // qualify is decided over the whole table first, bounded to the range's
    // rows. Deciding it inside the range loop would silently only ever match a
    // column the user was already summing.
    let rows_wanted: Option<std::collections::HashSet<u32>> = filter.map(|f| {
        cells
            .iter()
            .filter(|c| {
                let r = c.row_idx as u32;
                r >= r0 && r <= r1 && c.col_idx as u32 == f.column && f.matches(&c.raw_text)
            })
            .map(|c| c.row_idx as u32)
            .collect()
    });

    let mut values: Vec<f64> = Vec::new();
    let mut kinds: Vec<(Kind, Option<String>, String)> = Vec::new();
    let mut skipped: Vec<Skipped> = Vec::new();
    for cell in &cells {
        let (r, c) = (cell.row_idx as u32, cell.col_idx as u32);
        if r < r0 || r > r1 || c < c0 || c > c1 {
            continue;
        }
        // A row the filter excluded is not a skipped cell. It was never asked
        // about, and listing it under "did not count" would bury the cells that
        // were asked about and could not answer.
        if rows_wanted.as_ref().is_some_and(|w| !w.contains(&r)) {
            continue;
        }
        match numeric(cell) {
            Some((v, kind)) => {
                kinds.push((kind, cell.unit.clone(), at(&sheet_name, r, c)));
                values.push(v)
            }
            None => skipped.push(Skipped {
                reference: at(&sheet_name, r, c),
                raw_text: cell.raw_text.clone(),
                reason: if cell.raw_text.trim().is_empty() {
                    "blank"
                } else {
                    "not a number"
                },
            }),
        }
    }

    // A filter that matched nothing is not a total of zero. Zero is a number
    // and a reader acts on it; "no row matched" is the answer to a different
    // question and has to be distinguishable from "the matching rows summed
    // to nothing".
    if let (Some(w), Some(f)) = (&rows_wanted, filter) {
        if w.is_empty() {
            return Err(Error::new(
                Code::CfgInvalid,
                format!(
                    "No row in that range has `{}` in column {}. Nothing was added, which \
                     is not the same as a total of zero.",
                    f.equals,
                    marrow_core::a1::column_name(f.column),
                ),
            ));
        }
    }

    // **A unit mismatch blocks the operation rather than coercing.** Counting
    // is exempt — it does not combine the values, so there is nothing to
    // coerce.
    if op != Op::Count {
        // Two independent questions, because they fail differently. Different
        // *kinds* — a percentage beside an amount — can never be added. Same
        // kind, different *units* — dollars beside euros — cannot either.
        //
        // A cell that states no unit joins whatever kind it is: a bare `1200`
        // in a column of dollars is a dollar amount, and refusing that sum
        // would be a false alarm on the commonest table there is.
        //
        // Each group keeps the first cell that put it there, so the refusal can
        // name an address rather than a category.
        let mut groups: Vec<(Kind, Option<String>, &str)> = Vec::new();
        for (k, u, at) in &kinds {
            let same = groups.iter().any(|(gk, gu, _)| gk == k && gu == u);
            if !same {
                groups.push((*k, u.clone(), at.as_str()));
            }
        }
        // Collapse the unit-less members into a kind that already has a unit:
        // they are the same quantity, described less completely.
        let kinds_with_a_unit: Vec<Kind> = groups
            .iter()
            .filter(|(_, u, _)| u.is_some())
            .map(|(k, _, _)| *k)
            .collect();
        groups.retain(|(k, u, _)| u.is_some() || !kinds_with_a_unit.contains(k));

        if groups.len() > 1 {
            let examples: Vec<String> = groups
                .iter()
                .map(|(k, u, at)| format!("{} ({at})", k.describe(u.as_deref())))
                .collect();
            return Err(Error::new(
                Code::CfgInvalid,
                format!(
                    "That range mixes {}, which cannot be added together — the answer \
                     would be a number with no meaning. Narrow the range to one of them, \
                     or use `count`.",
                    examples.join(" and ")
                ),
            ));
        }
    }

    let value = match op {
        Op::Count => Some(values.len() as f64),
        // No honest answer over nothing — zero is a number, and a number is
        // what a reader acts on. See `Computed::value`.
        _ if values.is_empty() => None,
        Op::Sum => Some(values.iter().sum()),
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
        filtered_by: filter.cloned(),
    })
}

/// What kind of quantity a cell holds, for the purpose of adding it up.
///
/// **Not a display detail.** A percent is a ratio and a currency is an amount;
/// adding one to the other produces a number with no meaning, and the number
/// looks exactly as confident as a correct one. M3 asks for a unit mismatch to
/// block the operation rather than coerce silently, and this is the smallest
/// thing that can tell one from the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// A bare count or measure. Integers and decimals together: `3` and `3.5`
    /// are the same kind of thing, and refusing to add them would be pedantry.
    Number,
    Currency,
    Percent,
}

impl Kind {
    fn describe(self, unit: Option<&str>) -> String {
        match (self, unit) {
            // **The symbol, when the cell gave one.** `Currency` alone cannot
            // separate dollars from euros, and adding those is the same silent
            // coercion one level finer — a number as confident as a right one.
            (Self::Currency, Some(u)) => format!("amounts in {u}"),
            (Self::Currency, None) => "currency amounts".into(),
            (Self::Number, _) => "plain numbers".into(),
            (Self::Percent, _) => "percentages".into(),
        }
    }
}

/// The number in a cell and what kind of number it is, from the value the
/// parser typed — never re-parsed from the display text.
///
/// TBL-005 keeps the raw text beside the typed reading precisely so that a
/// cell displayed as `1,234.50` or `(89)` or `45%` is not re-interpreted by
/// whoever reads it next. The parser already decided; this trusts that
/// decision or skips the cell.
fn numeric(cell: &CellRow) -> Option<(f64, Kind)> {
    let kind = match cell.value_type.as_deref()?.to_ascii_lowercase().as_str() {
        "integer" | "decimal" => Kind::Number,
        "currency" => Kind::Currency,
        "percent" => Kind::Percent,
        // Dates parse as numbers and mean nothing when added. The type is the
        // gate, not whether `parse::<f64>` happens to succeed.
        _ => return None,
    };
    Some((cell.typed_value.as_deref()?.parse::<f64>().ok()?, kind))
}

/// `Q2!B4`, or just `B4` when the source has no sheets.
///
/// A Markdown or HTML table has no sheet name, and a bare leading `!` in a
/// citation reads like a typo rather than an address.
fn at(sheet: &str, row: u32, col: u32) -> String {
    let cell = marrow_core::a1::cell_ref(row, col);
    if sheet.is_empty() {
        cell
    } else {
        format!("{sheet}!{cell}")
    }
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
            unit: None,
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
            numeric(&cell(0, 0, "1,234.50", Some("1234.5"), Some("DECIMAL"))).map(|(v, _)| v),
            Some(1234.5)
        );
        assert_eq!(
            numeric(&cell(0, 0, "45%", Some("0.45"), Some("PERCENT"))).map(|(v, _)| v),
            Some(0.45)
        );
    }

    #[test]
    fn a_percent_and_a_currency_are_different_kinds_of_number() {
        // The whole point of tracking the kind: `45%` and `$1,234.50` both
        // parse as `f64` and adding them produces a number with no meaning
        // that looks exactly as confident as a correct one.
        assert_eq!(
            numeric(&cell(0, 0, "$1,234.50", Some("1234.5"), Some("currency"))).map(|(_, k)| k),
            Some(Kind::Currency)
        );
        assert_eq!(
            numeric(&cell(0, 0, "45%", Some("0.45"), Some("percent"))).map(|(_, k)| k),
            Some(Kind::Percent)
        );
        // Integers and decimals are the same kind. Refusing to add `3` to
        // `3.5` would be pedantry, not safety.
        assert_eq!(
            numeric(&cell(0, 0, "3", Some("3"), Some("integer"))).map(|(_, k)| k),
            numeric(&cell(0, 0, "3.5", Some("3.5"), Some("decimal"))).map(|(_, k)| k)
        );
    }

    #[test]
    fn a_kind_describes_itself_in_words_a_message_can_use() {
        // The refusal names what it found, because "mixed units" sends the
        // reader hunting through forty rows.
        for k in [Kind::Number, Kind::Currency, Kind::Percent] {
            assert!(!k.describe(None).is_empty());
        }
        assert_ne!(Kind::Currency.describe(None), Kind::Percent.describe(None));
        // And the symbol shows when the cell gave one: `Currency` alone
        // cannot separate dollars from euros.
        assert_ne!(
            Kind::Currency.describe(Some("$")),
            Kind::Currency.describe(Some("€"))
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
    fn nothing_to_add_is_not_the_same_answer_as_zero() {
        // A column of `n/a` totalling `0` reads as "this cost nothing" when it
        // means "there is nothing here to add", and a reader acts on the
        // first. The empty-sum-is-zero convention is a fact about arithmetic,
        // not about the user's spreadsheet.
        let none: Vec<f64> = vec![];
        assert_eq!(none.iter().copied().reduce(f64::min), None);
        assert_eq!(none.iter().sum::<f64>(), 0.0, "which is why it is not used");
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

/// One key and what the rows under it come to.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    /// The value in the grouping column, as the sheet writes it.
    pub key: String,
    pub computed: Computed,
}

/// The same computation, once per distinct value of a column.
///
/// "What did each category come to" is the question a total cannot answer, and
/// the one a person asks second. It is [`compute`] run per key rather than a
/// second implementation, so every guard it took to make a single total honest
/// — the unit mismatch, the named skips, the refusal to report no-match as zero
/// — applies to each group without being written twice.
///
/// Per group is also the right granularity for the unit check: a table whose
/// rent rows are in pounds and travel rows in euros has two internally
/// consistent groups, and refusing the whole breakdown because the *table*
/// mixes units would be a false alarm.
pub fn compute_by(
    conn: &Connection,
    version_id: VersionId,
    op: Op,
    reference: &str,
    by: u32,
) -> Result<Vec<Group>> {
    let r = resolve(conn, version_id, reference)?;
    let (r0, _, r1, _) = r.bounds;

    // Distinct values in document order, not sorted: a sheet's own order is
    // information — a ledger is usually chronological — and re-sorting it
    // alphabetically discards that for no gain.
    let mut keys: Vec<String> = Vec::new();
    for cell in cells_for(conn, &r.table_id)? {
        let row = cell.row_idx as u32;
        if row < r0 || row > r1 || cell.col_idx as u32 != by {
            continue;
        }
        let key = cell.raw_text.trim();
        // A blank in the grouping column is not a group. It is a row nobody
        // named, and inventing an empty-string bucket for it would put
        // unrelated rows together under a heading that reads as a mistake.
        if key.is_empty() || keys.iter().any(|k| k.eq_ignore_ascii_case(key)) {
            continue;
        }
        keys.push(key.to_owned());
    }

    if keys.is_empty() {
        return Err(Error::new(
            Code::CfgInvalid,
            format!(
                "Column {} has nothing to group by in that range — every cell in it is \
                 blank.",
                marrow_core::a1::column_name(by),
            ),
        ));
    }

    keys.into_iter()
        .map(|key| {
            let w = Where {
                column: by,
                equals: key.clone(),
            };
            compute(conn, version_id, op, reference, Some(&w))
                .map(|computed| Group { key, computed })
        })
        .collect()
}

/// One cell a lookup found, and where it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// The cell as written. **Not the typed value**: a lookup answers "what
    /// does the sheet say here", and `$1,200` is the answer, not `1200`.
    pub value: String,
    /// `Q2!C7`. The whole reason this is a tool rather than a guess — an answer
    /// that cannot be checked against the file is worth less than no answer.
    pub reference: String,
    /// 0-based, as the sheet is stored.
    pub row: u32,
}

/// The cells of one column in every row another column matches.
///
/// **Every match, never the first.** A lookup that quietly returns one row when
/// three qualify is the same defect this whole command exists to avoid: an
/// answer that is accurate about what the code did and wrong about the user's
/// data. Two rents in a ledger is normal, and being shown one of them without
/// being told there is another is how a person acts on half a figure.
pub fn lookup(
    conn: &Connection,
    version_id: VersionId,
    reference: &str,
    filter: &Where,
    get: u32,
) -> Result<Vec<Found>> {
    let r = resolve(conn, version_id, reference)?;
    let (r0, _, r1, _) = r.bounds;
    let cells = cells_for(conn, &r.table_id)?;

    let mut rows: Vec<u32> = cells
        .iter()
        .filter(|c| {
            let row = c.row_idx as u32;
            row >= r0
                && row <= r1
                && c.col_idx as u32 == filter.column
                && filter.matches(&c.raw_text)
        })
        .map(|c| c.row_idx as u32)
        .collect();
    rows.sort_unstable();
    rows.dedup();

    if rows.is_empty() {
        return Err(Error::new(
            Code::CfgInvalid,
            format!(
                "No row in that range has `{}` in column {}.",
                filter.equals,
                marrow_core::a1::column_name(filter.column),
            ),
        ));
    }

    Ok(rows
        .into_iter()
        .map(|row| {
            // A matching row whose wanted cell is empty is still a match. The
            // blank is the answer — reporting nothing at all would say the row
            // does not exist, which is a different and wrong claim.
            let value = cells
                .iter()
                .find(|c| c.row_idx as u32 == row && c.col_idx as u32 == get)
                .map(|c| c.raw_text.trim().to_owned())
                .unwrap_or_default();
            Found {
                value,
                reference: at(&r.sheet, row, get),
                row,
            }
        })
        .collect())
}
