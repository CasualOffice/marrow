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
    let got = compute(&conn, v, Op::Sum, "Q2!A1:A3").expect("a sum");
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
    let err = compute(&conn, v, Op::Sum, "Q2!A1:A3").expect_err("must refuse");
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
    let got = compute(&conn, v, Op::Count, "Q2!A1:A2").expect("count is safe");
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
    let got = compute(&conn, v, Op::Sum, "Q2!A1:A3").expect("a sum");
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
    let err = compute(&conn, version, Op::Sum, "Q2!A1:A2").expect_err("must refuse");
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
    let got = compute(&conn, v, Op::Sum, "Q2!A1:A2").expect("one unit, one kind");
    assert_eq!(got.value, Some(2100.0));
}
