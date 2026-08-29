//! Interruption and the waiting experience.
//!
//! Two rules from [UX §10], and they pull against each other:
//!
//! - **Progress only for work over ~500 ms, and on stderr.** A spinner that
//!   flashes for 80 ms is worse than nothing, and stdout must stay a clean pipe.
//! - **`Ctrl-C` cancels within 500 ms, leaves the index consistent, exits 5.**
//!
//! Both are about the same thing: the user must always be able to tell whether
//! the tool is working or stuck, and must always be able to stop it without
//! wondering what state they left behind.
//!
//! [UX §10]: ../../../docs/UX.md

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use marrow_ingest::Cancel;

/// Work must run at least this long before anything is drawn.
///
/// A `marrow index` over an unchanged corpus finishes in under a second; a
/// progress line that appears and vanishes reads as a glitch.
pub const SHOW_AFTER: Duration = Duration::from_millis(500);

/// Redraw cadence. Fast enough to look live, slow enough not to be the reason
/// the terminal is busy.
const TICK: Duration = Duration::from_millis(100);

/// Install a `Ctrl-C` handler that cancels cooperatively.
///
/// The first press asks every stage to stop at its next boundary, which is what
/// leaves the index consistent. **The second press exits immediately** — a tool
/// that ignores a second Ctrl-C is one people learn to `kill -9`, and that is
/// the case where state is actually at risk.
///
/// Returns the token to hand to the pipeline. Safe to call once per process; a
/// second call is a no-op that logs.
pub fn install_interrupt_handler() -> Cancel {
    let cancel = Cancel::new();
    let armed = Arc::new(AtomicBool::new(false));

    let c = cancel.clone();
    let a = Arc::clone(&armed);
    let installed = ctrlc::set_handler(move || {
        if a.swap(true, Ordering::SeqCst) {
            // Second press: they mean it.
            let _ = writeln!(std::io::stderr(), "\ninterrupted");
            std::process::exit(crate::EXIT_INTERRUPTED);
        }
        let _ = writeln!(
            std::io::stderr(),
            "\nstopping — finishing the current file, then leaving the index consistent"
        );
        c.cancel();
    });

    if installed.is_err() {
        // Not fatal: without a handler Ctrl-C terminates the process, and the
        // WAL plus the idempotent job keys make that resumable. Say so rather
        // than pretending cancellation works.
        tracing::warn!("could not install a Ctrl-C handler; interrupt will terminate abruptly");
    }
    cancel
}

/// A live counter the pipeline updates and the renderer reads.
///
/// Deliberately not a channel: a producer that has to send a progress message
/// is a producer that can block on rendering. Relaxed atomics cost nothing and
/// cannot stall the work being measured.
#[derive(Debug, Default)]
pub struct Meter {
    done: AtomicU64,
    label: std::sync::Mutex<String>,
}

impl Meter {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn set(&self, done: u64) {
        self.done.store(done, Ordering::Relaxed);
    }

    pub fn set_label(&self, label: impl Into<String>) {
        if let Ok(mut l) = self.label.lock() {
            *l = label.into();
        }
    }

    fn snapshot(&self) -> (u64, String) {
        let label = self.label.lock().map(|l| l.clone()).unwrap_or_default();
        (self.done.load(Ordering::Relaxed), label)
    }
}

/// Draws progress on stderr while work runs, and cleans up after itself.
///
/// Silent when stderr is not a terminal: a redirected log full of carriage
/// returns is worse than no progress at all.
pub struct Spinner {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Spinner {
    /// Start watching `meter`. Nothing is drawn until [`SHOW_AFTER`] has passed.
    pub fn start(meter: Arc<Meter>, noun: &'static str) -> Self {
        let stop = Arc::new(AtomicBool::new(false));

        if !std::io::stderr().is_terminal() {
            return Self { stop, handle: None };
        }

        let s = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            let started = Instant::now();
            let mut drew = false;
            // Braille dots: one cell wide in every terminal, unlike an emoji.
            const FRAMES: [char; 8] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧'];
            let mut frame = 0usize;

            while !s.load(Ordering::Relaxed) {
                std::thread::sleep(TICK);
                if started.elapsed() < SHOW_AFTER {
                    continue;
                }
                let (done, label) = meter.snapshot();
                let mut err = std::io::stderr();
                let _ = write!(
                    err,
                    "\r\x1b[2K  {} {} {}{}",
                    FRAMES[frame % FRAMES.len()],
                    crate::render::count(done),
                    noun,
                    if label.is_empty() {
                        String::new()
                    } else {
                        format!("  {label}")
                    }
                );
                let _ = err.flush();
                drew = true;
                frame += 1;
            }

            // Erase the line rather than leaving a stale frame above the
            // result. A finished spinner is noise.
            if drew {
                let mut err = std::io::stderr();
                let _ = write!(err, "\r\x1b[2K");
                let _ = err.flush();
            }
        });

        Self {
            stop,
            handle: Some(handle),
        }
    }

    /// Stop drawing and clear the line. Idempotent.
    pub fn finish(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        // So an early return or a `?` still leaves the terminal clean.
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spinner_that_is_never_drawn_still_joins_cleanly() {
        // The common case: work finishes before SHOW_AFTER.
        let m = Meter::new();
        let mut s = Spinner::start(Arc::clone(&m), "files");
        m.set(3);
        s.finish();
        s.finish(); // idempotent
    }

    #[test]
    fn dropping_a_spinner_stops_its_thread() {
        let m = Meter::new();
        {
            let _s = Spinner::start(Arc::clone(&m), "files");
        }
        // If the thread outlived the drop this would hang under a test timeout.
    }

    #[test]
    fn the_meter_is_readable_while_being_written() {
        let m = Meter::new();
        let w = Arc::clone(&m);
        let t = std::thread::spawn(move || {
            for i in 0..1000 {
                w.set(i);
            }
        });
        for _ in 0..100 {
            let _ = m.snapshot();
        }
        t.join().unwrap();
        assert_eq!(m.snapshot().0, 999);
    }

    #[test]
    fn labels_survive_a_round_trip() {
        let m = Meter::new();
        m.set_label("parsing");
        assert_eq!(m.snapshot().1, "parsing");
    }
}
