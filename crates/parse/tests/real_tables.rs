//! Table extraction against real files, not fixtures.
//!
//! Ignored by default and driven by an environment variable, for the reason
//! CLAUDE.md gives: nothing in this repository may contain a real path from the
//! author's disk. Point it at a directory and run it:
//!
//! ```text
//! MARROW_TABLE_CORPUS=~/Desktop/some/folder \
//!   cargo test -p marrow-parse --test real_tables real_corpus_tables -- --ignored --nocapture
//! ```
//!
//! It walks for `.csv`, `.md` and `.html`, routes each file through the real
//! parser chain, and prints every table it found with its header confidence,
//! inferred column types and one resolved cell span. A span that does not
//! resolve back to its own bytes fails the test — on a real corpus that is the
//! assertion worth having, because fixtures are written by the person who wrote
//! the parser and real files are not.

use std::path::{Path, PathBuf};

use marrow_core::SourceSpan;
use marrow_parse::chunk::{chunk, ChunkKind, ChunkPolicy};
use marrow_parse::parser::FileProbe;
use marrow_parse::router::ParserRouter;
use marrow_parse::table::{tables_in, TableIr};

const WANTED: &[&str] = &["csv", "tsv", "md", "markdown", "html", "htm"];

/// Directories that are never worth walking.
const SKIP: &[&str] = &[
    "node_modules",
    "target",
    ".git",
    ".venv",
    "site-packages",
    "dist",
    "build",
];

fn walk(dir: &Path, out: &mut Vec<PathBuf>, budget: usize) {
    if out.len() >= budget {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            if !SKIP.contains(&name.as_str()) && !name.starts_with('.') {
                walk(&p, out, budget);
            }
        } else if p
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| WANTED.contains(&x))
        {
            out.push(p);
        }
        if out.len() >= budget {
            return;
        }
    }
}

fn render(path: &Path, src: &str, t: &TableIr) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "\n  {} [{}]  {}×{}  reconstruction={}  provenance={:?}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        t.extraction_method,
        t.n_rows,
        t.n_cols,
        t.reconstruction.as_str(),
        t.provenance,
    );
    let _ = writeln!(
        s,
        "    header: row={:?} preamble={} confidence={:.2}",
        t.header.row, t.header.preamble_rows, t.header.confidence
    );
    if let Some(c) = &t.caption {
        let _ = writeln!(s, "    caption: {c}");
    }
    for col in 0..t.n_cols.min(12) {
        let name = t
            .column_names
            .get(col as usize)
            .filter(|n| !n.is_empty())
            .cloned()
            .unwrap_or_else(|| format!("(column {})", col + 1));
        let ty = t.column_types.get(col as usize);
        let _ = writeln!(
            s,
            "    col {col}: {name:<28} {}",
            ty.map(|t| t.as_str()).unwrap_or("?")
        );
    }
    // One cell, with its span resolved back into the file. This is the whole
    // claim the project makes, printed.
    if let Some(cell) = t
        .cells
        .iter()
        .find(|c| c.row >= t.header.body_start() && !c.raw_text.trim().is_empty())
    {
        if let SourceSpan::Bytes { start, end } = cell.span {
            let _ = writeln!(
                s,
                "    cell (r{},c{}) raw={:?} typed={:?} span=bytes {}..{} -> {:?}",
                cell.row,
                cell.col,
                cell.raw_text,
                cell.typed_value(),
                start,
                end,
                &src[start as usize..end as usize],
            );
        }
    }
    s
}

#[test]
#[ignore = "needs MARROW_TABLE_CORPUS pointing at a real directory"]
fn real_corpus_tables() {
    let Ok(root) = std::env::var("MARROW_TABLE_CORPUS") else {
        panic!("set MARROW_TABLE_CORPUS to a directory to run this");
    };
    let root = PathBuf::from(shellexpand(&root));
    let mut files = Vec::new();
    walk(&root, &mut files, 4000);
    files.sort();
    assert!(!files.is_empty(), "no candidate files under {root:?}");

    let router = ParserRouter::with_default_parsers();
    let policy = ChunkPolicy::default();
    let mut tables_seen = 0usize;
    let mut printed = 0usize;
    let mut with_header = 0usize;
    let mut degraded = 0usize;
    let mut schema_chunks = 0usize;
    let mut band_chunks = 0usize;

    for path in &files {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        if bytes.len() > 4 * 1024 * 1024 {
            continue;
        }
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let probe = FileProbe::new(name, bytes.len() as u64);
        let Ok(artifact) = router.parse(&bytes, &probe) else {
            continue;
        };
        artifact.validate().expect("a real file must validate");
        let Ok(src) = std::str::from_utf8(&bytes) else {
            continue;
        };

        for t in tables_in(&artifact) {
            tables_seen += 1;
            if t.header.row.is_some() {
                with_header += 1;
            }
            if !matches!(t.reconstruction, marrow_parse::table::Reconstruction::Exact) {
                degraded += 1;
            }
            // **TBL-002 on real bytes.** Every cell's span must resolve to the
            // cell's own text.
            for c in &t.cells {
                let SourceSpan::Bytes { start, end } = c.span else {
                    panic!(
                        "{path:?}: a text table produced a non-byte span: {:?}",
                        c.span
                    );
                };
                assert!(
                    src.get(start as usize..end as usize).is_some(),
                    "{path:?}: span {start}..{end} is not a character boundary"
                );
            }
            if printed < 12 && t.is_usable() && t.n_rows > 2 {
                print!("{}", render(path, src, &t));
                printed += 1;
            }
        }

        for c in chunk(&artifact, &policy) {
            match c.kind {
                ChunkKind::TableSchema => schema_chunks += 1,
                ChunkKind::TableBand => band_chunks += 1,
                _ => {}
            }
        }
    }

    println!(
        "\n{} files · {tables_seen} tables · {with_header} with a header · \
         {degraded} not exactly reconstructed · {schema_chunks} schema chunks · \
         {band_chunks} band chunks",
        files.len()
    );
    assert!(tables_seen > 0, "no tables found under {root:?}");
    // TBL-011: every usable table emits exactly one schema chunk, so the two
    // counts move together.
    assert!(schema_chunks > 0);
}

/// `~` only. Enough for an environment variable a human typed.
fn shellexpand(s: &str) -> String {
    match s.strip_prefix("~/") {
        Some(rest) => match std::env::var("HOME") {
            Ok(home) => format!("{home}/{rest}"),
            Err(_) => s.to_owned(),
        },
        None => s.to_owned(),
    }
}
