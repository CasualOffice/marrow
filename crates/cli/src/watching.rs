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
    json: bool,
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

        render_loop(rx, cancel, json, style, out, targets.len())?;

        for h in handles {
            // A panicking watcher thread must not take the others with it, and
            // must not be silently lost either.
            if h.join().is_err() {
                let panicked = Report::Failed {
                    // No name: the handle does not carry which target it was.
                    // Saying so beats attributing the panic to the wrong root.
                    name: String::new(),
                    message: "a watcher thread panicked".to_string(),
                };
                let _ = render_one(&panicked, json, style, out);
            }
        }
        Ok(())
    })?;

    // Queued writes belong to us until they are on disk.
    store.flush()?;
    Ok(())
}

/// One root's loop. Owns no output.
/// Persist how fresh this root is, and who is watching it.
///
/// **The database is the only channel to the other processes.** The MCP server
/// and a second terminal are separate, short-lived processes; freshness that
/// lives only in this one's memory cannot be reported by the surface an agent
/// actually calls, and `marrow watch` running in one window while `index_status`
/// says "nothing is watching" is exactly the confusion this avoids.
fn mark(store: &marrow_store::Store, t: &Target, health: &marrow_scan::Health) {
    let h = match health {
        marrow_scan::Health::Live => marrow_store::read::WatcherHealth::Live,
        marrow_scan::Health::Degraded(_) => marrow_store::read::WatcherHealth::Degraded,
        marrow_scan::Health::PollOnly(_) => marrow_store::read::WatcherHealth::PollOnly,
    };
    if let Err(e) = store.mark_reconciled(t.root_id, h, marrow_core::Timestamp::now()) {
        tracing::warn!(error = %e, "could not record watcher health");
    }
}

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

    // **Sweep before listening.** A watcher is not live the instant it opens,
    // and nothing at all was listening while this process was not running — so
    // a change in either window emits no event and would wait for the next
    // scheduled sweep, six hours away. The ingest is idempotent, so on an
    // unchanged corpus this costs one walk and stores nothing.
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
            mark(store, t, &health);
            let _ = tx.send(Report::Swept {
                name: t.name.clone(),
                files: o.stored,
                reason: "started watching",
            });
        }
        Err(e) => {
            let _ = tx.send(Report::Failed {
                name: t.name.clone(),
                message: e.message().to_string(),
            });
        }
    }

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
            mark(store, t, &health);
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
                    mark(store, t, &health);
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
                mark(store, t, &health);
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

    // A watcher that has exited must not leave the database claiming someone is
    // still listening — that is precisely the "stale index looks fresh" state.
    if let Err(e) = store.mark_reconciled(
        t.root_id,
        marrow_store::read::WatcherHealth::Unavailable,
        marrow_core::Timestamp::now(),
    ) {
        tracing::warn!(error = %e, "could not record that the watcher stopped");
    }
    let _ = tx.send(Report::Stopped {
        name: t.name.clone(),
    });
}

/// The only thread that writes.
fn render_loop(
    rx: std::sync::mpsc::Receiver<Report>,
    cancel: &Cancel,
    json: bool,
    style: Style,
    out: &mut impl Write,
    expected: usize,
) -> Result<()> {
    let mut stopped = 0usize;
    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(r) => {
                render_one(&r, json, style, out)?;
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

/// One event as one JSON object, for a `marrow watch --json` consumer.
///
/// A stream, not a summary: `watch` never finishes on its own, so a single
/// closing object would arrive only when the user gives up, which is after the
/// events they were waiting for. One object per line is what a reader can
/// consume incrementally, and `at_ms` is what lets it order or age them —
/// epoch milliseconds, like every other timestamp here.
fn event(r: &Report) -> serde_json::Value {
    let at_ms = marrow_core::Timestamp::now().as_millis();
    let mut v = match r {
        Report::Started { name, health } => serde_json::json!({
            "event": "started",
            "workspace": name,
            "watcher": health.label(),
            // WATCH-009: a degraded watcher is never silent, on this surface
            // either. Always present, `null` when healthy, so a reader can
            // branch on it without knowing which variants carry one.
            "reason": health.reason(),
            "sweep_interval_ms": marrow_scan::reconcile_interval(health).as_millis() as u64,
        }),
        Report::HealthChanged { name, health } => serde_json::json!({
            "event": "health_changed",
            "workspace": name,
            "watcher": health.label(),
            "reason": health.reason(),
        }),
        Report::Applied {
            name,
            files,
            chunks,
        } => serde_json::json!({
            "event": "applied",
            "workspace": name,
            "files": files,
            "chunks": chunks,
        }),
        Report::Swept {
            name,
            files,
            reason,
        } => serde_json::json!({
            "event": "swept",
            "workspace": name,
            "files": files,
            "reason": reason,
        }),
        Report::Failed { name, message } => serde_json::json!({
            "event": "failed",
            "workspace": name,
            "message": message,
        }),
        Report::Stopped { name } => serde_json::json!({
            "event": "stopped",
            "workspace": name,
        }),
    };
    if let Some(o) = v.as_object_mut() {
        o.insert("schema".into(), "marrow.watch.event/1".into());
        o.insert("at_ms".into(), at_ms.into());
    }
    v
}

fn render_one(r: &Report, json: bool, style: Style, out: &mut impl Write) -> Result<()> {
    if json {
        writeln!(out, "{}", event(r))?;
        // Flushed per event for the same reason the human view is: a consumer
        // reading this pipe is waiting on the event, and a buffered line is an
        // event that has not happened yet as far as it can tell.
        out.flush()?;
        return Ok(());
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn json_of(r: &Report) -> serde_json::Value {
        let mut buf = Vec::new();
        render_one(r, true, Style::plain(), &mut buf).expect("rendering an event");
        let line = String::from_utf8(buf).expect("events are utf-8");
        serde_json::from_str(line.trim_end()).expect("one object per line")
    }

    #[test]
    fn every_event_carries_its_schema_and_a_timestamp() {
        let events = [
            Report::Started {
                name: "notes".into(),
                health: Health::Live,
            },
            Report::HealthChanged {
                name: "notes".into(),
                health: Health::PollOnly("the volume does not support events"),
            },
            Report::Applied {
                name: "notes".into(),
                files: 3,
                chunks: 12,
            },
            Report::Swept {
                name: "notes".into(),
                files: 1,
                reason: "scheduled sweep",
            },
            Report::Failed {
                name: "notes".into(),
                message: "the folder is gone".into(),
            },
            Report::Stopped {
                name: "notes".into(),
            },
        ];
        for e in &events {
            let v = json_of(e);
            assert_eq!(v["schema"], "marrow.watch.event/1");
            assert!(v["event"].is_string(), "{v}");
            assert!(v["at_ms"].as_i64().is_some_and(|t| t > 0), "{v}");
        }
    }

    #[test]
    fn a_degraded_watcher_says_why_in_json_too() {
        // WATCH-009 on this surface: a consumer that only reads the label
        // would otherwise see `poll-only` with nothing to act on.
        let v = json_of(&Report::HealthChanged {
            name: "notes".into(),
            health: Health::Degraded("the kernel queue overflowed"),
        });
        assert_eq!(v["watcher"], "degraded");
        assert_eq!(v["reason"], "the kernel queue overflowed");
    }

    #[test]
    fn a_healthy_watcher_reports_a_null_reason_rather_than_omitting_it() {
        let v = json_of(&Report::Started {
            name: "notes".into(),
            health: Health::Live,
        });
        assert_eq!(v["watcher"], "live");
        assert!(v["reason"].is_null());
        assert!(v["sweep_interval_ms"].as_u64().is_some_and(|m| m > 0));
    }
}
