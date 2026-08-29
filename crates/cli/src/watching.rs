//! `marrow watch` — one watcher thread per root, one shared cancel.
//!
//! # Thread management
//!
//! Each root gets its own thread, because a watcher blocks on its own channel
//! and there is no portable way to select across several. They share:
//!
//! - one [`Cancel`], so `Ctrl-C` reaches every thread at its next boundary;
//! - one `mpsc` back to the main thread, which owns all rendering.
//!
//! **The main thread does every write.** Threads that render interleave
//! mid-line, and a progress line half-overwritten by another root's output is
//! the kind of thing people file as a display bug and never trust again.
//!
//! # Waiting
//!
//! Shutdown joins every thread rather than detaching. A watcher dropped
//! mid-batch would leave the store's writer with queued work and no one to
//! flush it — the index would be a few files behind with nothing saying so.

use std::io::Write;
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use marrow_core::{Result, RootId, WorkspaceId};
use marrow_ingest::{Cancel, IngestPolicy, Progress};
use marrow_scan::{AuthorizedRoot, Health};

use crate::render::{self, Style};

/// One root being watched.
pub struct Target {
    pub name: String,
    pub path: String,
    pub workspace_id: WorkspaceId,
    pub root_id: RootId,
}

/// What a watcher thread tells the main thread.
///
/// Every variant names the workspace, because with several roots "3 changed"
/// with no attribution is not actionable.
enum Report {
    Started {
        name: String,
        health: Health,
    },
    HealthChanged {
        name: String,
        health: Health,
    },
    Applied {
        name: String,
        files: u64,
        chunks: u64,
    },
    Swept {
        name: String,
        files: u64,
        reason: &'static str,
    },
    Failed {
        name: String,
        message: String,
    },
    Stopped {
        name: String,
    },
}

/// Watch every target until interrupted.
///
/// Returns once every thread has joined and the store has been flushed.
pub fn run(
    store: &marrow_store::Store,
    targets: Vec<Target>,
    cancel: &Cancel,
    style: Style,
    out: &mut impl Write,
) -> Result<()> {
    let (tx, rx) = channel::<Report>();
    let policy = Arc::new(IngestPolicy::default());

    // `std::thread::scope` so the threads may borrow the store rather than
    // forcing it behind an Arc that only exists to satisfy `'static`. Handles
    // live inside the scope: a `Vec` declared outside would outlive the borrows
    // the threads hold.
    std::thread::scope(|scope| -> Result<()> {
        let mut handles = Vec::with_capacity(targets.len());
        for t in &targets {
            let tx = tx.clone();
            let cancel = cancel.clone();
            let policy = Arc::clone(&policy);
            handles.push(scope.spawn(move || watch_one(store, t, &policy, &cancel, &tx)));
        }
        // The main thread's own sender must go, or `rx` never disconnects and
        // the render loop waits forever for threads that have already stopped.
        drop(tx);

        render_loop(rx, cancel, style, out, targets.len())?;

        for h in handles {
            // A panicking watcher thread must not take the others with it, and
            // must not be silently lost either.
            if h.join().is_err() {
                let _ = writeln!(out, "{}", style.err("a watcher thread panicked"));
            }
        }
        Ok(())
    })?;

    // Queued writes belong to us until they are on disk.
    store.flush()?;
    Ok(())
}

/// One root's loop. Owns no output.
fn watch_one(
    store: &marrow_store::Store,
    t: &Target,
    policy: &IngestPolicy,
    cancel: &Cancel,
    tx: &Sender<Report>,
) {
    let root = match AuthorizedRoot::open(&t.path) {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.send(Report::Failed {
                name: t.name.clone(),
                message: e.message().to_string(),
            });
            return;
        }
    };
    let mut watcher = match marrow_scan::Watcher::open(&root) {
        Ok(w) => w,
        Err(e) => {
            let _ = tx.send(Report::Failed {
                name: t.name.clone(),
                message: e.message().to_string(),
            });
            return;
        }
    };
    let index = match marrow_index::Fts5Index::open(store) {
        Ok(i) => i,
        Err(e) => {
            let _ = tx.send(Report::Failed {
                name: t.name.clone(),
                message: e.message().to_string(),
            });
            return;
        }
    };

    let mut health = watcher.health().clone();
    let _ = tx.send(Report::Started {
        name: t.name.clone(),
        health: health.clone(),
    });

    // A degraded watcher makes the sweep the primary mechanism, so the interval
    // is re-read every loop rather than fixed at startup (WATCH-010).
    let mut last_sweep = Instant::now();

    loop {
        if cancel.is_cancelled() {
            break;
        }
        // Short poll so cancellation is honoured well inside UX §10's 500 ms,
        // rather than after a full watch timeout.
        let Some(hints) = watcher.next_batch(Duration::from_millis(250)) else {
            break;
        };

        if *watcher.health() != health {
            health = watcher.health().clone();
            let _ = tx.send(Report::HealthChanged {
                name: t.name.clone(),
                health: health.clone(),
            });
        }

        let due = last_sweep.elapsed() >= marrow_scan::reconcile_interval(&health);
        if hints.rescan_required || due {
            let reason = if hints.rescan_required {
                "events were lost"
            } else {
                "scheduled sweep"
            };
            last_sweep = Instant::now();
            let progress = Arc::new(Progress::new());
            match marrow_ingest::ingest_root_with_index(
                store,
                t.workspace_id,
                t.root_id,
                &root,
                policy,
                &progress,
                cancel,
                Some(&index),
            ) {
                Ok(o) => {
                    let _ = tx.send(Report::Swept {
                        name: t.name.clone(),
                        files: o.stored,
                        reason,
                    });
                }
                Err(e) => {
                    let _ = tx.send(Report::Failed {
                        name: t.name.clone(),
                        message: e.message().to_string(),
                    });
                }
            }
            continue;
        }

        if hints.touched.is_empty() {
            continue;
        }

        let progress = Arc::new(Progress::new());
        match marrow_ingest::apply_hints(
            store,
            t.workspace_id,
            t.root_id,
            &root,
            policy,
            &hints.touched,
            &progress,
            cancel,
            Some(&index),
        ) {
            Ok(o) if o.stored > 0 => {
                let _ = tx.send(Report::Applied {
                    name: t.name.clone(),
                    files: o.stored,
                    chunks: o.chunks,
                });
            }
            Ok(_) => {}
            Err(e) => {
                let _ = tx.send(Report::Failed {
                    name: t.name.clone(),
                    message: e.message().to_string(),
                });
            }
        }
    }

    let _ = tx.send(Report::Stopped {
        name: t.name.clone(),
    });
}

/// The only thread that writes.
fn render_loop(
    rx: std::sync::mpsc::Receiver<Report>,
    cancel: &Cancel,
    style: Style,
    out: &mut impl Write,
    expected: usize,
) -> Result<()> {
    let mut stopped = 0usize;
    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(r) => {
                render_one(&r, style, out)?;
                if matches!(r, Report::Stopped { .. }) {
                    stopped += 1;
                    if stopped == expected {
                        break;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                // Nothing to draw. The timeout exists so cancellation is
                // noticed even when every root is idle.
                if cancel.is_cancelled() && stopped == 0 {
                    // Threads will report Stopped shortly; keep draining.
                    continue;
                }
            }
            // Every sender gone: the threads are finished.
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(())
}

fn render_one(r: &Report, style: Style, out: &mut impl Write) -> Result<()> {
    match r {
        Report::Started { name, health } => {
            writeln!(
                out,
                "{:<14} {}",
                style.bold(name),
                match health {
                    Health::Live => style.ok("live"),
                    h => style.warn(h.label()),
                }
            )?;
            // WATCH-009: never silently degraded.
            if let Some(reason) = health.reason() {
                writeln!(out, "  {}", style.warn(reason))?;
            }
            writeln!(
                out,
                "  {}",
                style.dim(&format!(
                    "sweeping every {}",
                    render::duration(marrow_scan::reconcile_interval(health).as_millis())
                ))
            )?;
        }
        Report::HealthChanged { name, health } => {
            writeln!(
                out,
                "{} {:<14} {}",
                style.warn("⚠"),
                style.bold(name),
                style.warn(health.label())
            )?;
            if let Some(reason) = health.reason() {
                writeln!(out, "  {}", style.warn(reason))?;
            }
        }
        Report::Applied {
            name,
            files,
            chunks,
        } => {
            writeln!(
                out,
                "{:<14} {} changed · {} chunks",
                style.dim(name),
                render::count(*files),
                render::count(*chunks)
            )?;
        }
        Report::Swept {
            name,
            files,
            reason,
        } => {
            writeln!(
                out,
                "{:<14} {} — {} changed",
                style.dim(name),
                style.dim(reason),
                render::count(*files)
            )?;
        }
        Report::Failed { name, message } => {
            writeln!(
                out,
                "{} {:<14} {}",
                style.err("✗"),
                style.bold(name),
                message
            )?;
        }
        Report::Stopped { name } => {
            writeln!(out, "{:<14} {}", style.dim(name), style.dim("stopped"))?;
        }
    }
    out.flush()?;
    Ok(())
}
