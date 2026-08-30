//! **Grading a table must cost what its cells cost, not what its address says.**
//!
//! `grade` decided whether a reconstruction was exact by painting every square
//! of the bounding box into a `Vec<bool>`. `n_rows` and `n_cols` are the far
//! edge of that box, and a workbook with a value in A1 and one in XFD1048576
//! has two cells and a box of 17,179,869,184 squares — so grading a two-cell
//! table asked the allocator for **17.2 GB**.
//!
//! It never crashed, for two reasons that are both accidents. `vec![false; n]`
//! allocates zeroed, so macOS maps the pages lazily and nothing touches them;
//! and `all()` short-circuits on the first hole, which in a sparse table is
//! index 1. Neither is a guarantee — an allocator that refuses aborts the
//! process uncatchably — and a reservation that large is not free even unread.
//!
//! Which is why this test counts bytes *requested* rather than measuring
//! resident memory: the bug is the request, and the request is what the
//! platform was quietly forgiving.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;

static LARGEST: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        LARGEST.fetch_max(l.size(), Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        // The one the old `vec![false; n]` took, and the one whose laziness hid
        // the size of the request.
        LARGEST.fetch_max(l.size(), Ordering::Relaxed);
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        LARGEST.fetch_max(new, Ordering::Relaxed);
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static A: Counting = Counting;

use marrow_parse::ir::{ArtifactBuilder, IrKind, IrNode, NodeAttrs, ParserTier};
use marrow_parse::table::{tables_in, Reconstruction};
use marrow_parse::{BudgetGuard, Budgets};

/// A sheet holding exactly these cells — what the XLSX parser emits for a
/// sparse workbook, which is careful not to fill the box in.
fn sparse_sheet(cells: &[(u32, u32, &str)]) -> marrow_parse::ir::ParsedArtifact {
    let span = |r: u32, c: u32| marrow_core::SourceSpan::Cells {
        sheet: "Sheet1".to_owned(),
        range: format!("R{r}C{c}"),
    };
    let mut b = ArtifactBuilder::new(
        "test",
        "1",
        ParserTier::T2,
        BudgetGuard::new(Budgets::default()),
    );
    let table = b
        .push(None, IrNode::structural(IrKind::Table, span(0, 0)))
        .unwrap();
    for (r, c, text) in cells {
        let row = b
            .push(
                Some(table),
                IrNode::structural(IrKind::TableRow, span(*r, *c)).with_attrs(NodeAttrs {
                    row: Some(*r),
                    ..NodeAttrs::default()
                }),
            )
            .unwrap();
        b.push(
            Some(row),
            IrNode::content(IrKind::TableCell, span(*r, *c), *text).with_attrs(NodeAttrs {
                row: Some(*r),
                col: Some(*c),
                ..NodeAttrs::default()
            }),
        )
        .unwrap();
    }
    b.finish()
}

#[test]
fn grading_a_sparse_sheet_does_not_reserve_its_bounding_box() {
    // Excel's real maximum, which is what one stray cell reference produces.
    let a = sparse_sheet(&[(0, 0, "opening"), (1_048_575, 16_383, "corner")]);

    LARGEST.store(0, Ordering::Relaxed);
    let tables = tables_in(&a);
    let largest = LARGEST.load(Ordering::Relaxed);

    assert_eq!(tables.len(), 1, "the fixture must produce one table");
    assert_eq!(
        tables[0].reconstruction,
        Reconstruction::Degraded,
        "a box with two cells in it is not an exact reconstruction"
    );
    assert!(
        largest < 8 * 1024 * 1024,
        "grading two cells asked the allocator for {largest} bytes \
         ({:.1} GB) — the bounding box, not the table",
        largest as f64 / 1e9
    );
}

#[test]
fn an_ordinary_table_is_still_graded_exactly() {
    // The painted path has to keep working, or the ceiling would have bought
    // bounded memory by making every grade a guess.
    let full = sparse_sheet(&[(0, 0, "a"), (0, 1, "b"), (1, 0, "1"), (1, 1, "2")]);
    assert_eq!(tables_in(&full)[0].reconstruction, Reconstruction::Exact);

    let holed = sparse_sheet(&[(0, 0, "a"), (0, 1, "b"), (1, 1, "2")]);
    assert_eq!(
        tables_in(&holed)[0].reconstruction,
        Reconstruction::Degraded
    );
}
