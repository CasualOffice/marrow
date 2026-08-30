//! `marrow embed` — build semantic search over what is already indexed.
//!
//! Separate from `index` on purpose. Indexing must work with no model, no GPU
//! and no network (hard rule 10); embedding is the optional layer on top, it
//! needs weights on disk, and it takes minutes. Folding it into `index` would
//! make the fast path wait for the slow one.
//!
//! Resumable and idempotent (hard rule 7): the backfill re-asks the store each
//! batch, so Ctrl-C loses at most one batch and running it again continues.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use marrow_core::{Code, Error, Result};
use marrow_index::SqliteVectorIndex;
use marrow_model::backfill;
use marrow_model::catalogue;
use marrow_model::embed::Embedder;
use marrow_model::scratch::ModelWorkspace;
use marrow_model::worker::Runtime;
use marrow_store::Store;

use crate::render::{self, Style};
use crate::waiting;

pub fn run(store: &Store, data_dir: &Path, style: Style, out: &mut dyn Write) -> Result<()> {
    let remaining = backfill::remaining(store)?;
    if remaining == 0 {
        writeln!(out, "{}", style.dim("Every chunk already has a vector."))?;
        return Ok(());
    }

    let runtime = Runtime::discover(data_dir, worker_script())
        .ok_or_else(|| Error::new(Code::ModNotInstalled, Runtime::setup_hint(data_dir)))?;

    // The same guard the desktop applies: weights must not live inside a
    // folder Marrow indexes, or it embeds its own model files.
    let workspace = ModelWorkspace::open(data_dir.join("models"), &indexed_roots(store)?)?;

    let entry = catalogue::builtin()
        .into_iter()
        .find(|e| e.capabilities.embedding)
        .ok_or_else(|| {
            Error::new(
                Code::ModNotInstalled,
                "No embedding model is in the catalogue.",
            )
        })?;
    let digest = entry.manifest_digest.as_deref().ok_or_else(|| {
        Error::new(
            Code::ModNotInstalled,
            format!("{} has no local weights.", entry.display_name),
        )
    })?;
    let dir = workspace.weights_dir(digest);
    if !dir.is_dir() {
        return Err(Error::new(
            Code::ModNotInstalled,
            format!(
                "{} is not downloaded, so semantic search cannot be built. \
                 Get it from the Models page in the desktop app — about 210 MB.",
                entry.display_name
            ),
        ));
    }

    writeln!(
        out,
        "{}",
        style.dim(&format!(
            "{} chunks to embed, using {}.",
            render::count(remaining),
            entry.display_name
        ))
    )?;

    let embedder = Embedder::start(&runtime, &entry.id, &dir)?;
    let vectors = SqliteVectorIndex::open(store)?;
    let progress = Arc::new(backfill::Progress::default());
    let cancel = waiting::install_model_interrupt_handler();

    // A meter rather than a line per batch: 1,700 lines of "embedded 32" is
    // scrollback, not progress. Drawn on stderr, so stdout stays a clean pipe.
    let meter = waiting::Meter::new();
    meter.set_label(entry.display_name.to_string());
    let mut spinner = waiting::Spinner::start(Arc::clone(&meter), "chunks");
    let stop = Arc::new(AtomicBool::new(false));
    let pump = {
        let (p, m, s) = (Arc::clone(&progress), Arc::clone(&meter), Arc::clone(&stop));
        std::thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                m.set(p.snapshot().0);
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        })
    };

    let started = std::time::Instant::now();
    let outcome = backfill::run(store, &vectors, &embedder, &cancel, &progress);
    stop.store(true, Ordering::Relaxed);
    let _ = pump.join();
    spinner.finish();
    let elapsed = started.elapsed();
    let outcome = outcome?;

    let rate = outcome.embedded as f64 / elapsed.as_secs_f64().max(f64::EPSILON);
    writeln!(
        out,
        "{}",
        style.dim(&format!(
            "{} embedded in {} ({rate:.0}/s).",
            render::count(outcome.embedded),
            render::duration(elapsed.as_millis())
        ))
    )?;

    // Both early exits say how much is left. "32 could not be embedded" on its
    // own reads as "the rest finished", and a failed batch **stops the run** —
    // so it would be claiming a complete index over a 3%-built one.
    let left = backfill::remaining(store)?;
    let why = if outcome.cancelled {
        // What was embedded stays embedded, and saying so is what makes Ctrl-C
        // a reasonable thing to press.
        Some("Stopped".to_string())
    } else if outcome.failed > 0 {
        Some(format!(
            "Stopped after a batch failed; {} chunks skipped",
            render::count(outcome.failed)
        ))
    } else {
        None
    };
    if let Some(why) = why {
        writeln!(
            out,
            "{}",
            style.warn(&format!(
                "{why}. {} still to do — run `marrow embed` again to continue.",
                render::count(left)
            ))
        )?;
    }
    Ok(())
}

/// Roots Marrow indexes, so the model workspace can refuse to sit inside one.
fn indexed_roots(store: &Store) -> Result<Vec<PathBuf>> {
    let conn = store.reader()?;
    let mut stmt = conn
        .prepare("SELECT canonical_path FROM workspace_roots")
        .map_err(|e| marrow_store::map_sqlite(e, "listing roots"))?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| marrow_store::map_sqlite(e, "listing roots"))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(PathBuf::from(
            row.map_err(|e| marrow_store::map_sqlite(e, "reading a root"))?,
        ));
    }
    Ok(out)
}

fn worker_script() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("mlx_worker.py")))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../model/worker/mlx_worker.py")
        })
}
