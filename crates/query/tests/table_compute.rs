//! `marrow table sum` over a real store: what it adds, and what it refuses.
//!
//! The unit tests beside [`marrow_query::table`] cover classification. These
//! cover the part a user meets — a range resolved against a workbook, with the
//! answer or the refusal that comes back.

use marrow_core::VersionId;
use marrow_query::table::{compute, Op};
use marrow_store::Store;

/// One sheet with the cells given as `(row, col, typed_value, value_type)`.
fn workbook(cells: &[(i64, i64, &str, &str)]) -> (tempfile::TempDir, Store, VersionId) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_with_migrations(dir.path().join("m.sqlite"), marrow_index::MIGRATIONS)
        .expect("store");
    // Real ULIDs: these columns are ULID `TEXT` and `VersionId` refuses
    // anything else, so a fixture using 'v' fails to parse rather than to
    // insert — which looks like a bug in the code under test.
    let version = VersionId::new();
    let vid = version.to_string();
    let rows: Vec<String> = cells
        .iter()
        .map(|(r, c, v, t)| {
            // The unit the parser would have recorded from the cell's text.
            let u = match *t {
                "currency" => "'$'",
                "percent" => "'%'",
                _ => "NULL",
            };
            format!(
                "INSERT INTO table_cells (cell_id, table_id, row_idx, col_idx, raw_text,
                                          typed_value, value_type, unit, cell_span)
                 VALUES ('c{r}_{c}', 't', {r}, {c}, '{v}', '{v}', '{t}', {u}, '{{}}');"
            )
        })
        .collect();
    let sql = format!(
        "INSERT INTO workspaces (workspace_id, name, created_at, updated_at)
              VALUES ('ws', 'notes', 0, 0);
         INSERT INTO workspace_roots (root_id, workspace_id, canonical_path, created_at)
              VALUES ('r', 'ws', '/tmp', 0);
         INSERT INTO files (file_id, workspace_id, root_id, current_path, created_at, updated_at)
              VALUES ('f', 'ws', 'r', '/tmp/b.xlsx', 0, 0);
         INSERT INTO file_versions (version_id, file_id, path_at_observation, size_bytes,
                                    mtime_ms, content_hash, observed_at)
              VALUES ('{vid}', 'f', '/tmp/b.xlsx', 1, 0, 'h', 0);
         INSERT INTO table_ir (table_id, version_id, source_span, n_rows, n_cols,
                               extraction_method, provenance_class)
              VALUES ('t', '{vid}', '{{\"kind\":\"cells\",\"sheet\":\"Q2\",\"range\":\"A1\"}}',
                      9, 9, 'NATIVE', 'EXACT');
         {}",
        rows.join("\n")
    );
    store
        .writer()
        .submit(move |c| {
            c.execute_batch(&sql)
                .map_err(|e| marrow_store::map_sqlite(e, "fixture"))
        })
        .expect("submit");
    store.flush().expect("flush");
    (dir, store, version)
}

#[test]
fn a_column_of_amounts_adds_up() {
    let (_d, store, v) = workbook(&[
        (0, 0, "10.5", "decimal"),
        (1, 0, "4", "integer"),
        (2, 0, "5.5", "decimal"),
    ]);
    let conn = store.reader().expect("reader");
    let got = compute(&conn, v, Op::Sum, "Q2!A1:A3", None).expect("a sum");
    assert_eq!(got.value, Some(20.0));
    assert_eq!(got.contributing, 3);
    // An integer beside a decimal is not a unit mismatch. Refusing to add `4`
    // to `10.5` would be pedantry rather than safety.
    assert!(!got.is_partial(), "nothing should have been skipped");
}

#[test]
fn a_range_mixing_currency_and_percent_is_refused_rather_than_added() {
    // **M3: a unit mismatch blocks the operation.** Both parse as `f64`, so
    // adding them produces a number with no meaning that looks exactly as
    // confident as a correct one — which is the failure mode this whole
    // command exists to avoid.
    let (_d, store, v) = workbook(&[
        (0, 0, "1200", "currency"),
        (1, 0, "0.45", "percent"),
        (2, 0, "800", "currency"),
    ]);
    let conn = store.reader().expect("reader");
    let err = compute(&conn, v, Op::Sum, "Q2!A1:A3", None).expect_err("must refuse");
    let msg = err.message();
    // Names the units, not the internal type names — "$" is what the cell
    // shows and what the reader will look for.
    assert!(msg.contains('$'), "must say what it found: {msg}");
    assert!(msg.contains("percent"), "must say what it found: {msg}");
    // Names a cell of each kind, because "mixed units" sends the reader
    // hunting through forty rows and two addresses end the search.
    assert!(msg.contains("Q2!A1"), "must name a cell: {msg}");
    assert!(msg.contains("Q2!A2"), "must name the other: {msg}");
}

#[test]
fn counting_a_mixed_range_still_works_because_it_combines_nothing() {
    let (_d, store, v) = workbook(&[(0, 0, "1200", "currency"), (1, 0, "0.45", "percent")]);
    let conn = store.reader().expect("reader");
    let got = compute(&conn, v, Op::Count, "Q2!A1:A2", None).expect("count is safe");
    assert_eq!(got.value, Some(2.0));
}

#[test]
fn a_cell_that_is_not_a_number_is_named_rather_than_quietly_dropped() {
    let (_d, store, v) = workbook(&[
        (0, 0, "10", "decimal"),
        (1, 0, "n/a", "string"),
        (2, 0, "", "empty"),
    ]);
    let conn = store.reader().expect("reader");
    let got = compute(&conn, v, Op::Sum, "Q2!A1:A3", None).expect("a sum");
    assert_eq!(got.value, Some(10.0));
    assert_eq!(got.contributing, 1);
    // A total over three cells where two held no number is not a total over
    // three cells, and the caller must be able to say which two.
    assert_eq!(got.skipped.len(), 2);
    let refs: Vec<&str> = got.skipped.iter().map(|s| s.reference.as_str()).collect();
    assert!(refs.contains(&"Q2!A2"), "{refs:?}");
    assert_eq!(
        got.skipped
            .iter()
            .find(|s| s.reference == "Q2!A3")
            .map(|s| s.reason),
        Some("blank")
    );
}

#[test]
fn dollars_and_euros_are_not_the_same_unit_even_though_both_are_currency() {
    // **TBL-006.** The classifier already recognised six currency symbols and
    // then discarded which, so `Currency` alone could not separate them and a
    // range of dollars and euros added to one confident number — the same
    // silent coercion the kind check refuses, one level finer.
    let dir = tempfile::tempdir().expect("tempdir");
    let store = marrow_store::Store::open_with_migrations(
        dir.path().join("m.sqlite"),
        marrow_index::MIGRATIONS,
    )
    .expect("store");
    let version = marrow_core::VersionId::new();
    let vid = version.to_string();
    let sql = format!(
        "INSERT INTO workspaces (workspace_id, name, created_at, updated_at)
              VALUES ('ws', 'notes', 0, 0);
         INSERT INTO workspace_roots (root_id, workspace_id, canonical_path, created_at)
              VALUES ('r', 'ws', '/tmp', 0);
         INSERT INTO files (file_id, workspace_id, root_id, current_path, created_at, updated_at)
              VALUES ('f', 'ws', 'r', '/tmp/b.xlsx', 0, 0);
         INSERT INTO file_versions (version_id, file_id, path_at_observation, size_bytes,
                                    mtime_ms, content_hash, observed_at)
              VALUES ('{vid}', 'f', '/tmp/b.xlsx', 1, 0, 'h', 0);
         INSERT INTO table_ir (table_id, version_id, source_span, n_rows, n_cols,
                               extraction_method, provenance_class)
              VALUES ('t', '{vid}', '{{\"kind\":\"cells\",\"sheet\":\"Q2\",\"range\":\"A1\"}}',
                      9, 9, 'NATIVE', 'EXACT');
         INSERT INTO table_cells (cell_id, table_id, row_idx, col_idx, raw_text,
                                  typed_value, value_type, unit, cell_span)
              VALUES ('a', 't', 0, 0, '$1200', '1200', 'currency', '$', '{{}}');
         INSERT INTO table_cells (cell_id, table_id, row_idx, col_idx, raw_text,
                                  typed_value, value_type, unit, cell_span)
              VALUES ('b', 't', 1, 0, '€900', '900', 'currency', '€', '{{}}');"
    );
    store
        .writer()
        .submit(move |c| {
            c.execute_batch(&sql)
                .map_err(|e| marrow_store::map_sqlite(e, "fixture"))
        })
        .expect("submit");
    store.flush().expect("flush");

    let conn = store.reader().expect("reader");
    let err = compute(&conn, version, Op::Sum, "Q2!A1:A2", None).expect_err("must refuse");
    let msg = err.message();
    assert!(
        msg.contains('$') && msg.contains('€'),
        "must name both: {msg}"
    );
}

#[test]
fn a_bare_number_beside_a_dollar_amount_is_not_a_mismatch() {
    // The false alarm the unit check must not raise. A column of dollars where
    // one row omits the symbol is the commonest table there is, and refusing to
    // total it would make the guard worse than the bug it prevents.
    let (_d, store, v) = workbook(&[(0, 0, "1200", "currency"), (1, 0, "900", "currency")]);
    let conn = store.reader().expect("reader");
    let got = compute(&conn, v, Op::Sum, "Q2!A1:A2", None).expect("one unit, one kind");
    assert_eq!(got.value, Some(2100.0));
}

/// `--where` narrows the rows without making the user restate the range in A1,
/// which is the whole point: expressing "only the Rent rows" as cell addresses
/// requires already knowing which rows those are.
mod filtering {
    use super::*;
    use marrow_query::table::Where;

    /// Column A is the category, column B the amount.
    fn ledger() -> (
        tempfile::TempDir,
        marrow_store::Store,
        marrow_core::VersionId,
    ) {
        workbook(&[
            (0, 0, "Rent", "string"),
            (0, 1, "1200", "decimal"),
            (1, 0, "Food", "string"),
            (1, 1, "300", "decimal"),
            (2, 0, "Rent", "string"),
            (2, 1, "800", "decimal"),
        ])
    }

    #[test]
    fn the_filter_column_may_sit_outside_the_range_being_totalled() {
        // **The case the design turns on.** "Total column B where column A is
        // Rent" is the ordinary shape of the question, and A is not in B2:B3.
        // Deciding which rows qualify inside the range loop would silently only
        // ever match a column the user was already summing.
        let (_d, store, v) = ledger();
        let conn = store.reader().expect("reader");
        let w = Where::parse("A=Rent").expect("a filter");
        let got = compute(&conn, v, Op::Sum, "Q2!B1:B3", Some(&w)).expect("a sum");
        assert_eq!(got.value, Some(2000.0), "1200 + 800, not 2300");
        assert_eq!(got.contributing, 2);
    }

    #[test]
    fn matching_no_row_is_refused_rather_than_reported_as_zero() {
        // Zero is a number and a reader acts on it. "No row matched" answers a
        // different question and has to be distinguishable from "the matching
        // rows summed to nothing".
        let (_d, store, v) = ledger();
        let conn = store.reader().expect("reader");
        let w = Where::parse("A=Holiday").expect("a filter");
        let err = compute(&conn, v, Op::Sum, "Q2!B1:B3", Some(&w)).expect_err("must refuse");
        assert!(err.message().contains("Holiday"), "{}", err.message());
        assert!(err.message().contains('A'), "must name the column");
    }

    #[test]
    fn an_excluded_row_is_not_reported_as_a_skipped_cell() {
        // A row the filter never asked about is not a cell that failed to
        // answer. Listing it under "did not count" would bury the ones that
        // were asked and could not.
        let (_d, store, v) = ledger();
        let conn = store.reader().expect("reader");
        let w = Where::parse("A=Rent").expect("a filter");
        let got = compute(&conn, v, Op::Sum, "Q2!B1:B3", Some(&w)).expect("a sum");
        assert!(!got.is_partial(), "skipped: {:?}", got.skipped);
    }

    #[test]
    fn a_filter_matches_what_the_sheet_shows_not_its_typed_value() {
        // Trimmed and case-insensitive: a filter is a question about what the
        // column reads like, typed from what the person saw.
        let (_d, store, v) = ledger();
        let conn = store.reader().expect("reader");
        let w = Where::parse("A=  rENT  ").expect("a filter");
        let got = compute(&conn, v, Op::Sum, "Q2!B1:B3", Some(&w)).expect("a sum");
        assert_eq!(got.value, Some(2000.0));
    }

    #[test]
    fn a_filter_without_an_equals_is_not_a_filter() {
        assert_eq!(Where::parse("A"), None);
        assert_eq!(Where::parse("A="), None);
        assert_eq!(Where::parse("=Rent"), None);
        // A value may contain `=`; only the first one splits.
        assert_eq!(Where::parse("A=x=y").map(|w| w.equals), Some("x=y".into()));
    }

    #[test]
    fn the_result_says_what_narrowed_it() {
        // A total of one cell out of a range of three is a different claim
        // from a total of one cell, and only this carries the difference.
        let (_d, store, v) = ledger();
        let conn = store.reader().expect("reader");
        let w = Where::parse("A=Food").expect("a filter");
        let got = compute(&conn, v, Op::Sum, "Q2!B1:B3", Some(&w)).expect("a sum");
        assert_eq!(
            got.filtered_by.as_ref().map(|f| f.equals.as_str()),
            Some("Food")
        );
        assert_eq!(got.value, Some(300.0));
    }
}

#[test]
fn a_range_with_no_numbers_says_so_rather_than_totalling_zero() {
    // **The convention that an empty sum is zero is a fact about arithmetic,
    // not about the user's spreadsheet.** A column of `n/a` totalling `0`
    // reads as "this cost nothing" when it means "there is nothing here to
    // add", and a reader acts on the first.
    let (_d, store, v) = workbook(&[(0, 0, "n/a", "string"), (1, 0, "tbd", "string")]);
    let conn = store.reader().expect("reader");
    let got = compute(&conn, v, Op::Sum, "Q2!A1:A2", None).expect("not an error");
    assert_eq!(got.value, None, "no number is not zero");
    assert_eq!(got.contributing, 0);
    assert_eq!(got.skipped.len(), 2, "and it names both");

    // Counting is the exception: counting nothing is a real answer to the
    // question that was asked.
    let counted = compute(&conn, v, Op::Count, "Q2!A1:A2", None).expect("a count");
    assert_eq!(counted.value, Some(0.0));
}

/// `--by A` is `--where A=…` run once per distinct value, so every guard that
/// makes a single total honest applies to each group without being rewritten.
mod grouping {
    use super::*;
    use marrow_query::table::compute_by;

    fn ledger() -> (
        tempfile::TempDir,
        marrow_store::Store,
        marrow_core::VersionId,
    ) {
        workbook(&[
            (0, 0, "Rent", "string"),
            (0, 1, "1200", "decimal"),
            (1, 0, "Food", "string"),
            (1, 1, "300", "decimal"),
            (2, 0, "Rent", "string"),
            (2, 1, "800", "decimal"),
            (3, 0, "Travel", "string"),
            (3, 1, "n/a", "string"),
        ])
    }

    #[test]
    fn each_key_gets_its_own_total() {
        let (_d, store, v) = ledger();
        let conn = store.reader().expect("reader");
        let g = compute_by(&conn, v, Op::Sum, "Q2!B1:B4", 0).expect("groups");
        let got: Vec<(&str, Option<f64>)> = g
            .iter()
            .map(|x| (x.key.as_str(), x.computed.value))
            .collect();
        assert_eq!(
            got,
            vec![
                ("Rent", Some(2000.0)),
                ("Food", Some(300.0)),
                // Its only row is `n/a`. Zero would read as "Travel cost
                // nothing" rather than "there is nothing here to add".
                ("Travel", None),
            ]
        );
    }

    #[test]
    fn keys_keep_the_sheets_own_order_rather_than_being_sorted() {
        // A ledger is usually chronological, and re-sorting it alphabetically
        // discards that for no gain. `Rent` comes first because it is first.
        let (_d, store, v) = ledger();
        let conn = store.reader().expect("reader");
        let g = compute_by(&conn, v, Op::Sum, "Q2!B1:B4", 0).expect("groups");
        assert_eq!(g.first().map(|x| x.key.as_str()), Some("Rent"));
    }

    #[test]
    fn a_group_carries_the_cells_it_could_not_count() {
        let (_d, store, v) = ledger();
        let conn = store.reader().expect("reader");
        let g = compute_by(&conn, v, Op::Sum, "Q2!B1:B4", 0).expect("groups");
        let travel = g.iter().find(|x| x.key == "Travel").expect("travel");
        assert_eq!(travel.computed.skipped.len(), 1);
        assert_eq!(travel.computed.skipped[0].reference, "Q2!B4");
    }

    #[test]
    fn a_column_of_blanks_has_nothing_to_group_by_and_says_so() {
        // An empty-string bucket would put unrelated rows together under a
        // heading that reads as a mistake.
        let (_d, store, v) = workbook(&[(0, 0, "", "empty"), (0, 1, "5", "decimal")]);
        let conn = store.reader().expect("reader");
        let err = compute_by(&conn, v, Op::Sum, "Q2!B1:B1", 0).expect_err("must refuse");
        assert!(err.message().contains('A'), "{}", err.message());
    }
}

/// `lookup` reads a cell rather than combining several, so citing it is the
/// whole product: an answer that cannot be checked against the file is worth
/// less than no answer.
mod looking_up {
    use super::*;
    use marrow_query::table::{lookup, Where};

    fn ledger() -> (
        tempfile::TempDir,
        marrow_store::Store,
        marrow_core::VersionId,
    ) {
        workbook(&[
            (0, 0, "Rent", "string"),
            (0, 1, "1200", "decimal"),
            (1, 0, "Food", "string"),
            (1, 1, "300", "decimal"),
            (2, 0, "Rent", "string"),
            (2, 1, "800", "decimal"),
        ])
    }

    #[test]
    fn every_matching_row_comes_back_not_just_the_first() {
        // **The defect this exists to avoid.** Two rents in a ledger is
        // normal, and being handed one of them without being told there is
        // another is how a person acts on half a figure.
        let (_d, store, v) = ledger();
        let conn = store.reader().expect("reader");
        let w = Where::parse("A=Rent").expect("a filter");
        let found = lookup(&conn, v, "Q2!A1:B3", &w, 1).expect("matches");
        assert_eq!(found.len(), 2, "both rents: {found:?}");
        assert_eq!(found[0].value, "1200");
        assert_eq!(found[1].value, "800");
    }

    #[test]
    fn each_answer_names_the_cell_it_came_from() {
        let (_d, store, v) = ledger();
        let conn = store.reader().expect("reader");
        let w = Where::parse("A=Rent").expect("a filter");
        let found = lookup(&conn, v, "Q2!A1:B3", &w, 1).expect("matches");
        let refs: Vec<&str> = found.iter().map(|f| f.reference.as_str()).collect();
        assert_eq!(refs, vec!["Q2!B1", "Q2!B3"]);
    }

    #[test]
    fn the_value_is_what_the_sheet_shows_not_the_typed_reading() {
        // A lookup answers "what does the sheet say here", so `$1,200` is the
        // answer and `1200` is not.
        let (_d, store, v) = workbook(&[(0, 0, "Rent", "string"), (0, 1, "$1,200", "currency")]);
        let conn = store.reader().expect("reader");
        let w = Where::parse("A=Rent").expect("a filter");
        let found = lookup(&conn, v, "Q2!A1:B1", &w, 1).expect("matches");
        assert_eq!(found[0].value, "$1,200");
    }

    #[test]
    fn a_matching_row_with_an_empty_cell_is_still_a_match() {
        // The blank is the answer. Reporting nothing would say the row does
        // not exist, which is a different and wrong claim.
        let (_d, store, v) = workbook(&[(0, 0, "Rent", "string"), (0, 1, "", "empty")]);
        let conn = store.reader().expect("reader");
        let w = Where::parse("A=Rent").expect("a filter");
        let found = lookup(&conn, v, "Q2!A1:B1", &w, 1).expect("a match");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].value, "");
        assert_eq!(found[0].reference, "Q2!B1");
    }

    #[test]
    fn no_match_is_an_error_rather_than_an_empty_list() {
        // An empty list reads as "the answer is nothing"; this reads as "the
        // question does not apply to this range".
        let (_d, store, v) = ledger();
        let conn = store.reader().expect("reader");
        let w = Where::parse("A=Holiday").expect("a filter");
        let err = lookup(&conn, v, "Q2!A1:B3", &w, 1).expect_err("must refuse");
        assert!(err.message().contains("Holiday"), "{}", err.message());
    }
}
