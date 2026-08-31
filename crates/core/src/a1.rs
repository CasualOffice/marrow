//! A1 addressing — the coordinate system a workbook is written in.
//!
//! `Sheet1!B4` is not a rendering choice. It is the address the file uses in
//! its own formulas, the address Excel's name box accepts, and the address the
//! user already thinks in, which is what makes [`SourceSpan::Cells`] a citation
//! a person can be *taken to* rather than a number they have to trust.
//!
//! Small enough to be obvious, shared because three modules need it to agree:
//! [`crate::xlsx`] writes the addresses, [`crate::table`] renders named ranges,
//! and [`crate::chunk`] widens a band's span across the rows it covers. Two
//! implementations of "which column is `AA`" would eventually disagree, and the
//! one that is wrong would be wrong in a citation.

use marrow_core::SourceSpan;

/// Excel's own limits. Anything outside them is a malformed file rather than a
/// big one, and clamping is what stops a crafted `r="1048577000"` from
/// producing an address no reader can resolve.
pub const MAX_ROW: u32 = 1_048_575;
pub const MAX_COL: u32 = 16_383;

/// A zero-based column index as `A`, `Z`, `AA`, `XFD`.
pub fn column_name(col: u32) -> String {
    let mut n = col.min(MAX_COL) as u64 + 1;
    let mut out = Vec::new();
    while n > 0 {
        let rem = ((n - 1) % 26) as u8;
        out.push(b'A' + rem);
        n = (n - 1) / 26;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_else(|_| "A".to_owned())
}

/// A zero-based `(row, col)` as `B4`.
pub fn cell_ref(row: u32, col: u32) -> String {
    format!("{}{}", column_name(col), row.min(MAX_ROW) + 1)
}

/// A zero-based rectangle as `B4` (when it is one cell) or `B4:D9`.
pub fn range_ref(row0: u32, col0: u32, row1: u32, col1: u32) -> String {
    if row0 == row1 && col0 == col1 {
        cell_ref(row0, col0)
    } else {
        format!("{}:{}", cell_ref(row0, col0), cell_ref(row1, col1))
    }
}

/// Parse `B4` or `$B$4` back to a zero-based `(row, col)`.
///
/// `$` is an absolute-reference marker, not part of the address, and a defined
/// name is almost always written with them.
pub fn parse_cell_ref(s: &str) -> Option<(u32, u32)> {
    let s = s.trim();
    let mut col = 0u64;
    let mut seen_letter = false;
    let mut rest = s;
    for (i, c) in s.char_indices() {
        match c {
            '$' if !seen_letter => {}
            'A'..='Z' | 'a'..='z' => {
                seen_letter = true;
                col = col * 26 + (c.to_ascii_uppercase() as u64 - 'A' as u64 + 1);
                if col > MAX_COL as u64 + 1 {
                    return None;
                }
            }
            _ => {
                rest = &s[i..];
                break;
            }
        }
        rest = &s[i + c.len_utf8()..];
    }
    if !seen_letter {
        return None;
    }
    let row: u64 = rest.trim_start_matches('$').parse().ok()?;
    if row == 0 || row > MAX_ROW as u64 + 1 {
        return None;
    }
    Some(((row - 1) as u32, (col - 1) as u32))
}

/// Parse `B4` or `B4:D9` into a zero-based `(row0, col0, row1, col1)`.
pub fn parse_range_ref(s: &str) -> Option<(u32, u32, u32, u32)> {
    match s.split_once(':') {
        Some((a, b)) => {
            let (r0, c0) = parse_cell_ref(a)?;
            let (r1, c1) = parse_cell_ref(b)?;
            Some((r0.min(r1), c0.min(c1), r0.max(r1), c0.max(c1)))
        }
        None => {
            let (r, c) = parse_cell_ref(s)?;
            Some((r, c, r, c))
        }
    }
}

/// Split `Sheet1!$B$2:$B$10` or `'Q1 Revenue'!A1` into its sheet and its range.
///
/// The quoted form is not decoration: a sheet name containing a space, a
/// bracket or an apostrophe must be quoted, and `''` is how an apostrophe is
/// escaped inside the quotes. Getting that wrong attaches a named range to the
/// wrong sheet, which is a citation pointing somewhere real and untrue.
pub fn split_sheet_ref(reference: &str) -> Option<(String, String)> {
    let r = reference.trim();
    // The last `!` outside quotes: a 3-D reference `Sheet1:Sheet3!A1` and a
    // quoted name can both contain earlier ones.
    let mut in_quote = false;
    let mut split_at = None;
    for (i, c) in r.char_indices() {
        match c {
            '\'' => in_quote = !in_quote,
            '!' if !in_quote => split_at = Some(i),
            _ => {}
        }
    }
    let i = split_at?;
    let (sheet, range) = (&r[..i], &r[i + 1..]);
    let sheet = sheet.trim();
    let sheet = match sheet.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        Some(inner) => inner.replace("''", "'"),
        None => sheet.to_owned(),
    };
    if sheet.is_empty() || range.is_empty() {
        return None;
    }
    Some((sheet, range.trim().to_owned()))
}

/// Union of two [`SourceSpan::Cells`] on the same sheet, or `None`.
///
/// Widening is only legitimate when both ends are real: two ranges on the same
/// sheet have a bounding box that is itself a true address. Across sheets there
/// is no such box, and inventing one would be exactly the fabricated coordinate
/// the span variants exist to prevent.
pub fn union_cells(a: &SourceSpan, b: &SourceSpan) -> Option<SourceSpan> {
    let (
        SourceSpan::Cells {
            sheet: s1,
            range: r1,
        },
        SourceSpan::Cells {
            sheet: s2,
            range: r2,
        },
    ) = (a, b)
    else {
        return None;
    };
    if s1 != s2 {
        return None;
    }
    let (ar0, ac0, ar1, ac1) = parse_range_ref(r1)?;
    let (br0, bc0, br1, bc1) = parse_range_ref(r2)?;
    Some(SourceSpan::Cells {
        sheet: s1.clone(),
        range: range_ref(ar0.min(br0), ac0.min(bc0), ar1.max(br1), ac1.max(bc1)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_names_carry_past_z() {
        assert_eq!(column_name(0), "A");
        assert_eq!(column_name(25), "Z");
        assert_eq!(column_name(26), "AA");
        assert_eq!(column_name(701), "ZZ");
        assert_eq!(column_name(702), "AAA");
        assert_eq!(column_name(MAX_COL), "XFD");
    }

    #[test]
    fn an_address_round_trips_through_its_text() {
        // The property that matters: a citation printed as `Sheet1!B4` must
        // resolve back to the cell it was written from.
        for (row, col) in [(0, 0), (3, 1), (99, 26), (1_048_574, 16_383)] {
            let text = cell_ref(row, col);
            assert_eq!(parse_cell_ref(&text), Some((row, col)), "{text}");
        }
    }

    #[test]
    fn absolute_markers_are_not_part_of_the_address() {
        assert_eq!(parse_cell_ref("$B$4"), Some((3, 1)));
        assert_eq!(parse_cell_ref("B$4"), Some((3, 1)));
        assert_eq!(parse_range_ref("$B$2:$B$10"), Some((1, 1, 9, 1)));
    }

    #[test]
    fn a_range_written_backwards_still_names_the_same_box() {
        assert_eq!(parse_range_ref("D9:B4"), Some((3, 1, 8, 3)));
    }

    #[test]
    fn nonsense_is_rejected_rather_than_clamped() {
        // A clamp here invents an address. `None` lets the caller keep the
        // span it already had, which is at worst narrow and never wrong.
        assert_eq!(parse_cell_ref("4"), None);
        assert_eq!(parse_cell_ref("B0"), None);
        assert_eq!(parse_cell_ref("B"), None);
        assert_eq!(parse_cell_ref(""), None);
        assert_eq!(parse_cell_ref("B1048577"), None);
    }

    #[test]
    fn a_quoted_sheet_name_survives_its_apostrophes() {
        assert_eq!(
            split_sheet_ref("Sheet1!$B$2:$B$10"),
            Some(("Sheet1".into(), "$B$2:$B$10".into()))
        );
        assert_eq!(
            split_sheet_ref("'Q1 Revenue'!A1"),
            Some(("Q1 Revenue".into(), "A1".into()))
        );
        assert_eq!(
            split_sheet_ref("'Bob''s data'!A1:B2"),
            Some(("Bob's data".into(), "A1:B2".into()))
        );
        // A bare name with no sheet is a workbook-scoped constant, not a range.
        assert_eq!(split_sheet_ref("TAX_RATE"), None);
    }

    #[test]
    fn spans_widen_within_a_sheet_and_never_across_one() {
        let a = SourceSpan::Cells {
            sheet: "Data".into(),
            range: "B2".into(),
        };
        let b = SourceSpan::Cells {
            sheet: "Data".into(),
            range: "D5".into(),
        };
        assert_eq!(
            union_cells(&a, &b),
            Some(SourceSpan::Cells {
                sheet: "Data".into(),
                range: "B2:D5".into()
            })
        );
        let other = SourceSpan::Cells {
            sheet: "Summary".into(),
            range: "A1".into(),
        };
        assert_eq!(union_cells(&a, &other), None, "no box spans two sheets");
    }
}
