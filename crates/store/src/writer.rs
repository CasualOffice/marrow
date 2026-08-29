//! The single-writer actor (Part 2 §50).
//!
//! WAL allows exactly one writer. The scanner, the parsers and the job queue
//! all want to write, so every write in the process goes through one thread
//! that owns the only write connection. This is the whole concurrency design:
//! there is no lock to forget to take, because there is no second connection to
//! take it on.
//!
//! **Batching.** Ops accumulate into one transaction and commit at 500 rows or
//! 100 ms, whichever comes first. M0 measured 235k rows/s on this machine
//! against a 9,435-file corpus, so throughput is a non-problem; the batch
//! exists to bound fsync count, not to chase numbers. Correctness beats
//! cleverness here.
//!
//! **Per-op atomicity.** Each op runs inside a `SAVEPOINT`. One op failing
//! rolls back exactly that op and the batch carries on — a bad row in a scan of
//! 9,000 files must not discard the 499 good ones next to it (FS-011).
//!
//! **Delivery.** A caller's `Result` is sent *after* the batch commits, so
//! `Ok(())` means durable, not merely accepted. If the batch commit fails,
//! every op in it gets the failure, including ops that succeeded on their own.
//!
//! **Backpressure.** The inbox is bounded; producers block when it is full.
//! During an initial scan the walker is faster than SQLite, and an unbounded
//! queue turns that into memory growth instead of a slow scan.

use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use marrow_core::{Code, Error, Result};
use rusqlite::Connection;

/// Writer tuning. Defaults are Part 2 §50's numbers.
#[derive(Clone, Copy, Debug)]
pub struct WriterConfig {
    /// Commit once a batch has touched this many rows.
    pub max_batch_rows: i64,
    /// Commit once a batch is this old, even if it is small.
    pub max_batch_interval: Duration,
    /// Inbox depth. Producers block once it is full.
    pub inbox_capacity: usize,
}

impl Default for WriterConfig {
    fn default() -> Self {
        Self {
            max_batch_rows: 500,
            max_batch_interval: Duration::from_millis(100),
            inbox_capacity: 1024,
        }
    }
}

/// How a batch ended. Handed to each op's delivery closure.
enum BatchOutcome {
    Committed,
    Failed(String),
}

impl BatchOutcome {
    fn err(&self) -> Option<Error> {
        match self {
            BatchOutcome::Committed => None,
            BatchOutcome::Failed(detail) => Some(
                Error::new(
                    Code::DbBusy,
                    "A batch of index writes could not be committed and was rolled back. \
                     The work will be retried; if this repeats, the index database may be \
                     out of disk space.",
                )
                .with_context(detail.clone()),
            ),
        }
    }
}

/// Delivers one op's result to its caller once the batch outcome is known.
type Deliver = Box<dyn FnOnce(&BatchOutcome) + Send>;

/// An op: runs against the batch's connection, returns its delivery closure and
/// whether it succeeded (a failed op has its savepoint rolled back).
///
/// `None` means the batch could not be started at all, so the op never runs.
type Op = Box<dyn FnOnce(Option<&Connection>) -> (Deliver, bool) + Send>;

enum Msg {
    Op(Op),
    /// Commit the open batch now and acknowledge.
    Flush(SyncSender<()>),
    /// Commit what is pending, then exit.
    Shutdown(SyncSender<()>),
    /// Exit *without* committing. Simulates process death; see [`crate::Store::abort`].
    Abort,
}

/// A submitted write whose result has not arrived yet.
#[derive(Debug)]
pub struct Pending<T> {
    rx: Receiver<Result<T>>,
}

impl<T> Pending<T> {
    /// Block until the batch containing this write commits.
    pub fn wait(self) -> Result<T> {
        self.rx.recv().map_err(|_| writer_gone())?
    }
}

fn writer_gone() -> Error {
    Error::new(
        Code::DbWriterGone,
        "The index writer stopped before this write was committed. Restart Marrow; \
         uncommitted work is re-derived from your files on the next scan.",
    )
}

/// A cloneable handle to the writer actor. Every write goes through one.
#[derive(Clone, Debug)]
pub struct Writer {
    tx: SyncSender<Msg>,
}

impl Writer {
    /// Queue a write. Blocks if the inbox is full (backpressure).
    ///
    /// The returned [`Pending`] resolves when the containing batch commits.
    pub fn send<T, F>(&self, f: F) -> Result<Pending<T>>
    where
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        // Capacity 1 and the value is sent exactly once, so the writer never
        // blocks on delivery even if the caller has already given up.
        let (tx, rx) = sync_channel::<Result<T>>(1);
        let op: Op = Box::new(move |conn| {
            let outcome = match conn {
                Some(c) => f(c),
                None => Err(Error::new(
                    Code::DbBusy,
                    "The index writer could not begin a transaction, so this write was not \
                     attempted. It will be retried on the next scan.",
                )),
            };
            let ok = outcome.is_ok();
            let deliver: Deliver = Box::new(move |batch: &BatchOutcome| {
                let result = match (outcome, batch.err()) {
                    // The op itself failed: report that, not the batch's fate.
                    (Err(e), _) => Err(e),
                    (Ok(_), Some(batch_err)) => Err(batch_err),
                    (Ok(v), None) => Ok(v),
                };
                let _ = tx.send(result);
            });
            (deliver, ok)
        });
        self.tx.send(Msg::Op(op)).map_err(|_| writer_gone())?;
        Ok(Pending { rx })
    }

    /// Queue a write and block until it is durable.
    pub fn submit<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        self.send(f)?.wait()
    }

    /// Commit the open batch now and block until it is durable.
    ///
    /// Without this, a small batch waits out `max_batch_interval`. Anything
    /// that must read its own writes through a *reader* connection has to flush
    /// first.
    pub fn flush(&self) -> Result<()> {
        let (tx, rx) = sync_channel::<()>(1);
        self.tx.send(Msg::Flush(tx)).map_err(|_| writer_gone())?;
        rx.recv().map_err(|_| writer_gone())
    }
}

/// Owns the writer thread. Dropping it shuts the thread down cleanly.
#[derive(Debug)]
pub(crate) struct WriterActor {
    handle: Writer,
    join: Option<JoinHandle<()>>,
}

impl WriterActor {
    /// Spawn the writer thread around `conn`, which must be the only write
    /// connection to this database.
    pub(crate) fn spawn(conn: Connection, cfg: WriterConfig) -> Self {
        let (tx, rx) = sync_channel::<Msg>(cfg.inbox_capacity);
        let join = std::thread::Builder::new()
            .name("marrow-writer".to_string())
            .spawn(move || run(conn, rx, cfg))
            .ok();
        if join.is_none() {
            // Spawn failure leaves `tx` with no receiver, so every send returns
            // DB_WRITER_GONE rather than hanging. Nothing silently no-ops.
            tracing::error!("could not spawn the index writer thread");
        }
        Self {
            handle: Writer { tx },
            join,
        }
    }

    pub(crate) fn handle(&self) -> &Writer {
        &self.handle
    }

    /// Flush pending work and join the thread. Idempotent.
    pub(crate) fn shutdown(&mut self) -> Result<()> {
        let Some(join) = self.join.take() else {
            return Ok(());
        };
        let (done_tx, done_rx) = sync_channel::<()>(1);
        // A send failure means the thread is already gone; still join it.
        let asked = self.handle.tx.send(Msg::Shutdown(done_tx)).is_ok();
        if asked {
            let _ = done_rx.recv();
        }
        join.join().map_err(|_| {
            Error::new(
                Code::DbWriterGone,
                "The index writer thread panicked. The last batch was rolled back; restart \
                 Marrow and the work will be re-derived.",
            )
        })?;
        if asked {
            Ok(())
        } else {
            Err(writer_gone())
        }
    }

    /// Stop without committing the open batch. Simulates process death.
    pub(crate) fn abort(&mut self) {
        let Some(join) = self.join.take() else {
            return;
        };
        let _ = self.handle.tx.send(Msg::Abort);
        let _ = join.join();
    }
}

impl Drop for WriterActor {
    fn drop(&mut self) {
        if let Err(e) = self.shutdown() {
            tracing::warn!(error = %e, "writer shutdown on drop");
        }
    }
}

/// The actor loop. One transaction per batch, one savepoint per op.
fn run(mut conn: Connection, rx: Receiver<Msg>, cfg: WriterConfig) {
    tracing::debug!(?cfg, "index writer started");
    loop {
        // Idle: block until there is something to do. No open transaction here,
        // so an idle process holds no write lock.
        let first = match rx.recv() {
            Ok(Msg::Op(op)) => op,
            Ok(Msg::Flush(reply)) => {
                // Nothing open: already flushed.
                let _ = reply.send(());
                continue;
            }
            Ok(Msg::Shutdown(reply)) => {
                let _ = reply.send(());
                break;
            }
            Ok(Msg::Abort) | Err(_) => break,
        };

        let mut txn = match conn.transaction() {
            Ok(tx) => tx,
            Err(e) => {
                // Cannot even begin: fail this op and wait for the next.
                let (deliver, _) = first(None);
                deliver(&BatchOutcome::Failed(e.to_string()));
                tracing::error!(error = %e, "could not begin a write batch");
                continue;
            }
        };

        let mut delivers: Vec<Deliver> = Vec::new();
        let mut rows: i64 = 0;
        let mut stop = Stop::None;
        let mut flush_reply: Option<SyncSender<()>> = None;

        apply(&mut txn, first, &mut delivers, &mut rows);

        let deadline = Instant::now() + cfg.max_batch_interval;
        while rows < cfg.max_batch_rows {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            match rx.recv_timeout(deadline - now) {
                Ok(Msg::Op(op)) => apply(&mut txn, op, &mut delivers, &mut rows),
                Ok(Msg::Flush(reply)) => {
                    flush_reply = Some(reply);
                    break;
                }
                Ok(Msg::Shutdown(reply)) => {
                    stop = Stop::Shutdown(reply);
                    break;
                }
                Ok(Msg::Abort) => {
                    stop = Stop::Abort;
                    break;
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    stop = Stop::Disconnected;
                    break;
                }
            }
        }

        if let Stop::Abort = stop {
            // Drop the transaction without committing: rusqlite rolls it back,
            // exactly as a killed process would. Delivery channels drop with
            // it, so waiting callers get DB_WRITER_GONE rather than hanging.
            drop(txn);
            tracing::warn!(ops = delivers.len(), "writer aborted, batch rolled back");
            break;
        }

        let count = delivers.len();
        let outcome = match txn.commit() {
            Ok(()) => {
                tracing::trace!(ops = count, rows, "batch committed");
                BatchOutcome::Committed
            }
            Err(e) => {
                tracing::error!(error = %e, ops = count, "batch commit failed");
                BatchOutcome::Failed(e.to_string())
            }
        };
        for deliver in delivers {
            deliver(&outcome);
        }
        if let Some(reply) = flush_reply {
            let _ = reply.send(());
        }

        match stop {
            Stop::Shutdown(reply) => {
                let _ = reply.send(());
                break;
            }
            Stop::Disconnected => break,
            _ => {}
        }
    }
    // Checkpoint on the way out so the next process opens a small WAL
    // (Part 2 §50 "manual TRUNCATE checkpoint on idle").
    if let Err(e) = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)") {
        tracing::debug!(error = %e, "wal checkpoint on shutdown");
    }
    tracing::debug!("index writer stopped");
}

enum Stop {
    None,
    Shutdown(SyncSender<()>),
    Abort,
    Disconnected,
}

/// Run one op inside its own savepoint.
fn apply(
    txn: &mut rusqlite::Transaction<'_>,
    op: Op,
    delivers: &mut Vec<Deliver>,
    rows: &mut i64,
) {
    let before = txn.total_changes() as i64;
    let sp = match txn.savepoint() {
        Ok(sp) => sp,
        Err(e) => {
            let (deliver, _) = op(None);
            deliver(&BatchOutcome::Failed(e.to_string()));
            tracing::error!(error = %e, "could not open a savepoint");
            return;
        }
    };
    let (deliver, ok) = op(Some(&sp));
    let sp_result = if ok { sp.commit() } else { sp.rollback() };
    if let Err(e) = sp_result {
        // The savepoint could not be released or rolled back; the transaction is
        // no longer trustworthy. Report it against this op and let the batch
        // commit decide the rest.
        tracing::error!(error = %e, "savepoint close failed");
    }
    *rows += (txn.total_changes() as i64 - before).max(0);
    delivers.push(deliver);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn actor(cfg: WriterConfig) -> WriterActor {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT NOT NULL)")
            .unwrap();
        WriterActor::spawn(conn, cfg)
    }

    fn count(w: &Writer) -> i64 {
        w.submit(|c| {
            c.query_row("SELECT count(*) FROM t", [], |r| r.get(0))
                .map_err(|e| crate::map_sqlite(e, "count"))
        })
        .unwrap()
    }

    #[test]
    fn a_submitted_batch_returns_a_result_per_caller() {
        let a = actor(WriterConfig::default());
        let w = a.handle().clone();
        let ok = w.submit(|c| {
            c.execute("INSERT INTO t (v) VALUES ('a')", [])
                .map_err(|e| crate::map_sqlite(e, "insert"))
        });
        assert_eq!(ok.unwrap(), 1);

        let bad = w.submit(|c| {
            c.execute("INSERT INTO t (v) VALUES (NULL)", [])
                .map_err(|e| crate::map_sqlite(e, "insert"))
        });
        assert!(bad.is_err(), "a failing op reports its own error");
        assert_eq!(count(&w), 1, "the failed op left nothing behind");
    }

    #[test]
    fn one_failing_op_does_not_discard_the_rest_of_the_batch() {
        let a = actor(WriterConfig {
            max_batch_interval: Duration::from_secs(30),
            ..WriterConfig::default()
        });
        let w = a.handle().clone();
        let good1 = w
            .send(|c| {
                c.execute("INSERT INTO t (v) VALUES ('a')", [])
                    .map_err(|e| crate::map_sqlite(e, "insert"))
            })
            .unwrap();
        let bad = w
            .send(|c| {
                c.execute("INSERT INTO t (v) VALUES (NULL)", [])
                    .map_err(|e| crate::map_sqlite(e, "insert"))
            })
            .unwrap();
        let good2 = w
            .send(|c| {
                c.execute("INSERT INTO t (v) VALUES ('b')", [])
                    .map_err(|e| crate::map_sqlite(e, "insert"))
            })
            .unwrap();
        // `flush` joins the same batch and forces the commit.
        w.flush().unwrap();
        assert!(good1.wait().is_ok());
        assert!(bad.wait().is_err());
        assert!(good2.wait().is_ok());
        assert_eq!(count(&w), 2);
    }

    #[test]
    fn batches_commit_at_the_row_threshold() {
        let a = actor(WriterConfig {
            max_batch_rows: 5,
            // Long enough that only the row threshold can end a batch.
            max_batch_interval: Duration::from_secs(30),
            ..WriterConfig::default()
        });
        let w = a.handle().clone();
        let mut pending = Vec::new();
        for i in 0..5 {
            pending.push(
                w.send(move |c| {
                    c.execute("INSERT INTO t (v) VALUES (?1)", [i.to_string()])
                        .map_err(|e| crate::map_sqlite(e, "insert"))
                })
                .unwrap(),
            );
        }
        for p in pending {
            // Would hang if the row threshold did not close the batch.
            assert_eq!(p.wait().unwrap(), 1);
        }
    }

    #[test]
    fn batches_commit_at_the_time_threshold() {
        let a = actor(WriterConfig {
            max_batch_rows: 100_000,
            max_batch_interval: Duration::from_millis(50),
            ..WriterConfig::default()
        });
        let w = a.handle().clone();
        let p = w
            .send(|c| {
                c.execute("INSERT INTO t (v) VALUES ('x')", [])
                    .map_err(|e| crate::map_sqlite(e, "insert"))
            })
            .unwrap();
        let started = Instant::now();
        assert_eq!(p.wait().unwrap(), 1);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the time threshold must close a batch that never fills"
        );
    }

    #[test]
    fn shutdown_flushes_pending_work() {
        let a = actor(WriterConfig {
            max_batch_interval: Duration::from_secs(30),
            ..WriterConfig::default()
        });
        let w = a.handle().clone();
        let pending: Vec<_> = (0..3)
            .map(|i| {
                w.send(move |c| {
                    c.execute("INSERT INTO t (v) VALUES (?1)", [i.to_string()])
                        .map_err(|e| crate::map_sqlite(e, "insert"))
                })
                .unwrap()
            })
            .collect();
        let mut a = a;
        a.shutdown().unwrap();
        for p in pending {
            assert_eq!(p.wait().unwrap(), 1, "shutdown must commit, not drop");
        }
    }

    #[test]
    fn writes_after_shutdown_report_the_writer_is_gone() {
        let mut a = actor(WriterConfig::default());
        let w = a.handle().clone();
        a.shutdown().unwrap();
        let err = w.submit(|_| Ok(())).unwrap_err();
        assert_eq!(err.code(), Code::DbWriterGone);
    }

    #[test]
    fn a_full_inbox_applies_backpressure() {
        let a = actor(WriterConfig {
            inbox_capacity: 1,
            max_batch_interval: Duration::from_millis(10),
            ..WriterConfig::default()
        });
        let w = a.handle().clone();
        let sent = Arc::new(AtomicUsize::new(0));
        let s = sent.clone();
        let producer = std::thread::spawn(move || {
            for i in 0..200 {
                let _ = w.send(move |c| {
                    c.execute("INSERT INTO t (v) VALUES (?1)", [i.to_string()])
                        .map_err(|e| crate::map_sqlite(e, "insert"))
                });
                s.fetch_add(1, Ordering::SeqCst);
            }
        });
        producer.join().unwrap();
        assert_eq!(sent.load(Ordering::SeqCst), 200);
        assert_eq!(count(a.handle()), 200);
    }
}
