//! Keep the index fresh while the app is open.
//!
//! **A stale index is worse than no index.** No index gives an empty answer
//! and the user knows to run a scan; a stale one answers confidently about a
//! disk it has not looked at, and nothing in the result says so. This app had
//! no watcher at all — the index only moved when somebody typed `marrow
//! index` in a terminal — so an answer's freshness was a function of when the
//! author last remembered.
//!
//! One thread per root. Watchers are hints, not truth (WATCH-001): a lost
//! event demands a sweep rather than being swallowed, and the interval is
//! re-read every loop because a degraded watcher makes the sweep the primary
//! mechanism rather than a backstop.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use marrow_core::{Result, RootId, Timestamp, WorkspaceId};
use marrow_ingest::{Cancel, IngestPolicy, Progress};
use marrow_scan::{AuthorizedRoot, Health};
use marrow_store::read::WatcherHealth;
use marrow_store::Store;

use crate::state::Core;

/// How one root is doing, for the surfaces that report freshness.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootStatus {
    pub name: String,
    /// `live` | `degraded` | `poll-only` | `stopped`.
    pub health: String,
    /// Why, when it is not live. Never a bare label (A11Y-003, WATCH-009).
    pub reason: Option<String>,
    /// When this root was last known to agree with the disk. `None` means it
    /// never has been in this session — which is a thing to say, not hide.
    pub last_change_ms: Option<i64>,
    pub files_applied: u64,
}

#[derive(Debug, Default)]
struct RootState {
    health: Mutex<(String, Option<String>)>,
    last_change_ms: AtomicI64,
    files_applied: AtomicU64,
    /// Set by [`Watchers::sweep_now`], cleared by the watcher thread when it
    /// begins the sweep.
    ///
    /// A flag rather than a second ingest, because the thread that owns this
    /// root is already the one sweeping it: starting another over the same
    /// root would double the walk, double the hashing, and have two producers
    /// racing to write the same versions through the one writer actor.
    sweep_requested: AtomicBool,
}

/// Handles for every running watcher. Dropping this stops them.
#[derive(Debug)]
pub struct Watchers {
    cancel: Cancel,
    /// Grows when a folder is granted while the app is running.
    roots: Mutex<Vec<(String, Arc<RootState>)>>,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

impl Watchers {
    /// Start one watcher per active root.
    ///
    /// Never fatal. A root that cannot be watched leaves the rest running and
    /// says why — refusing to open the app because one folder is on a
    /// disconnected volume would be the wrong trade.
    pub fn start(core: Arc<Core>) -> Result<Self> {
        let targets = targets(core.store())?;
        let cancel = Cancel::new();
        let mut roots = Vec::new();
        let mut threads = Vec::new();

        for t in targets {
            let state = Arc::new(RootState {
                health: Mutex::new(("starting".into(), None)),
                ..RootState::default()
            });
            roots.push((t.name.clone(), Arc::clone(&state)));

            let core = Arc::clone(&core);
            let cancel = cancel.clone();
            let built = std::thread::Builder::new()
                .name(format!("marrow-watch-{}", t.name))
                .spawn(move || watch_one(&core, &t, &state, &cancel));
            match built {
                Ok(h) => threads.push(h),
                Err(e) => tracing::warn!(error = %e, "could not start a watcher thread"),
            }
        }

        Ok(Self {
            cancel,
            roots: Mutex::new(roots),
            threads: Mutex::new(threads),
        })
    }

    /// What every root is doing, for `index_health` and the sidebar.
    pub fn status(&self) -> Vec<RootStatus> {
        let roots = match self.roots.lock() {
            Ok(r) => r.clone(),
            Err(_) => return Vec::new(),
        };
        roots
            .iter()
            .map(|(name, s)| {
                let (health, reason) = s
                    .health
                    .lock()
                    .map(|g| g.clone())
                    .unwrap_or_else(|_| ("unknown".into(), None));
                let last = s.last_change_ms.load(Ordering::Relaxed);
                RootStatus {
                    name: name.clone(),
                    health,
                    reason,
                    last_change_ms: (last > 0).then_some(last),
                    files_applied: s.files_applied.load(Ordering::Relaxed),
                }
            })
            .collect()
    }

    /// Ask every running watcher to reconcile at its next loop iteration.
    ///
    /// Returns how many roots were asked, so the caller can say what it started
    /// rather than claiming something happened.
    ///
    /// This is what "Run an index" means from the window. It does **not** start
    /// an ingest here: the watcher thread for a root is already the thing that
    /// sweeps it, and a second concurrent walk of the same tree would duplicate
    /// every hash and race the first one through the single writer actor. The
    /// wait is bounded by the loop's 250 ms poll, which is below the threshold
    /// where a person wonders whether the button did anything.
    ///
    /// The sweep is the same idempotent, resumable ingest as every other one
    /// (invariant #7), so pressing this on an unchanged corpus costs one walk
    /// and stores nothing.
    ///
    /// A root whose thread has stopped is skipped rather than counted: nothing
    /// would ever pick the flag up, and a button that reports "checking your
    /// folders" while nothing checks them is how the index went nine hours
    /// stale without saying so. When *nothing* can be asked, that is the whole
    /// answer, and it is an error with the reason in it.
    pub fn sweep_now(&self) -> Result<usize> {
        let roots = self.roots.lock().map_err(|_| {
            marrow_core::Error::invariant("the watcher list was poisoned by a panic")
        })?;
        if roots.is_empty() {
            return Err(marrow_core::Error::new(
                marrow_core::Code::CfgInvalid,
                "Marrow has not been given a folder yet, so there is nothing to \
                 index. Add one with “Add a folder”.",
            ));
        }

        let mut asked = 0usize;
        let mut stopped: Vec<String> = Vec::new();
        for (name, state) in roots.iter() {
            let (health, why) = state
                .health
                .lock()
                .map(|g| g.clone())
                .unwrap_or_else(|_| ("unknown".into(), None));
            if health == "stopped" {
                stopped.push(match why {
                    Some(r) => format!("{name} ({r})"),
                    None => name.clone(),
                });
                continue;
            }
            state.sweep_requested.store(true, Ordering::Release);
            asked += 1;
        }

        if asked == 0 {
            return Err(marrow_core::Error::new(
                marrow_core::Code::CfgInvalid,
                format!(
                    "Nothing is watching {}, so there is no thread left to run the \
                     sweep. Reopening Marrow starts the watchers again.",
                    stopped.join(", ")
                ),
            ));
        }
        Ok(asked)
    }

    /// True when at least one root is being watched at all.
    pub fn any_live(&self) -> bool {
        self.status()
            .iter()
            .any(|r| r.health == "live" || r.health == "degraded")
    }

    /// Begin watching a root added after startup.
    ///
    /// The alternative is restarting every watcher, which drops the FSEvents
    /// streams for folders that were fine — a newly granted folder must not
    /// cost the others a gap in coverage.
    pub fn watch_also(&self, core: Arc<Core>, root_id: RootId) -> Result<()> {
        let Some(t) = targets(core.store())?
            .into_iter()
            .find(|t| t.root_id == root_id)
        else {
            return Err(marrow_core::Error::invariant(
                "asked to watch a root that is not in the store",
            ));
        };
        let state = Arc::new(RootState {
            health: Mutex::new(("starting".into(), None)),
            ..RootState::default()
        });
        if let Ok(mut roots) = self.roots.lock() {
            roots.push((t.name.clone(), Arc::clone(&state)));
        }
        let cancel = self.cancel.clone();
        let handle = std::thread::Builder::new()
            .name(format!("marrow-watch-{}", t.name))
            .spawn(move || watch_one(&core, &t, &state, &cancel))
            .map_err(|e| {
                marrow_core::Error::new(
                    marrow_core::Code::CfgInvalid,
                    format!("Could not start a watcher for the new folder: {e}"),
                )
            })?;
        if let Ok(mut hs) = self.threads.lock() {
            hs.push(handle);
        }
        Ok(())
    }

    pub fn stop(&self) {
        self.cancel.cancel();
        if let Ok(mut hs) = self.threads.lock() {
            for h in hs.drain(..) {
                let _ = h.join();
            }
        }
    }
}

impl Drop for Watchers {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Target {
    name: String,
    path: String,
    workspace_id: WorkspaceId,
    root_id: RootId,
}

fn targets(store: &Store) -> Result<Vec<Target>> {
    let conn = store.reader()?;
    let mut stmt = conn
        .prepare(
            "SELECT w.name, r.canonical_path, r.workspace_id, r.root_id
               FROM workspace_roots r
               JOIN workspaces w ON w.workspace_id = r.workspace_id
              WHERE w.status = 'ACTIVE'",
        )
        .map_err(|e| marrow_store::map_sqlite(e, "listing roots to watch"))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(|e| marrow_store::map_sqlite(e, "listing roots to watch"))?;

    let mut out = Vec::new();
    for row in rows {
        let (name, path, ws, root) =
            row.map_err(|e| marrow_store::map_sqlite(e, "reading a root to watch"))?;
        let (Ok(workspace_id), Ok(root_id)) = (ws.parse(), root.parse()) else {
            continue;
        };
        out.push(Target {
            name,
            path,
            workspace_id,
            root_id,
        });
    }
    Ok(out)
}

fn watch_one(core: &Core, t: &Target, state: &Arc<RootState>, cancel: &Cancel) {
    let root = match AuthorizedRoot::open(&t.path) {
        Ok(r) => r,
        Err(e) => return stopped(core, t, state, e.message()),
    };
    let mut watcher = match marrow_scan::Watcher::open(&root) {
        Ok(w) => w,
        Err(e) => return stopped(core, t, state, e.message()),
    };

    let policy = IngestPolicy::default();
    let mut health = watcher.health().clone();
    set_health(core, t, state, &health);

    // **Sweep first, then watch.** Two gaps close here, and the second is the
    // big one:
    //
    //   1. A watcher is not listening the instant it is opened. A change made
    //      in that window produces no event, so without this it waits for the
    //      next scheduled sweep — six hours by default.
    //   2. Everything that changed *while the app was shut*. That is the
    //      ordinary case, not the edge one: you work on files all day with
    //      Marrow closed, open it, and every answer is drawn from whenever you
    //      last ran a scan. Waiting six hours to notice is indistinguishable
    //      from not having a watcher at all.
    //
    // It is the same idempotent, resumable ingest the manual scan runs, so on
    // an unchanged corpus it costs one walk and stores nothing.
    let mut last_sweep = Instant::now();
    sweep(core, t, &root, &policy, state, cancel, &health, "opened");

    loop {
        if cancel.is_cancelled() {
            break;
        }
        // Short poll so quitting the app is not held up by a watch timeout.
        let Some(hints) = watcher.next_batch(Duration::from_millis(250)) else {
            break;
        };
        if *watcher.health() != health {
            health = watcher.health().clone();
            set_health(core, t, state, &health);
        }

        // Cleared *before* the sweep, not after: a request that arrives while
        // one is already running asked about a disk state that sweep may have
        // walked past, and one redundant idempotent pass is the cheaper
        // mistake.
        let asked = state.sweep_requested.swap(false, Ordering::AcqRel);
        let due = last_sweep.elapsed() >= marrow_scan::reconcile_interval(&health);
        if asked || hints.rescan_required || due {
            last_sweep = Instant::now();
            // A lost event demands a full sweep. Reconciliation is what makes
            // the index correct; the watcher is only a hint (WATCH-001).
            let why = if asked {
                "asked from the app"
            } else if hints.rescan_required {
                "events were lost"
            } else {
                "scheduled sweep"
            };
            sweep(core, t, &root, &policy, state, cancel, &health, why);
            continue;
        }
        if hints.touched.is_empty() {
            continue;
        }

        let outcome = marrow_ingest::apply_hints(
            core.store(),
            t.workspace_id,
            t.root_id,
            &root,
            &policy,
            &hints.touched,
            &Arc::new(Progress::new()),
            cancel,
            Some(core.index()),
        );
        record(core, t, state, &health, outcome, "changed");
    }

    stopped(core, t, state, "the app stopped watching this folder");
}

#[allow(clippy::too_many_arguments)] // Each is a distinct input; a struct
                                     // would move the list, not shorten it.
fn sweep(
    core: &Core,
    t: &Target,
    root: &AuthorizedRoot,
    policy: &IngestPolicy,
    state: &Arc<RootState>,
    cancel: &Cancel,
    health: &Health,
    why: &str,
) {
    let outcome = marrow_ingest::ingest_root_with_index(
        core.store(),
        t.workspace_id,
        t.root_id,
        root,
        policy,
        &Arc::new(Progress::new()),
        cancel,
        Some(core.index()),
    );
    record(core, t, state, health, outcome, why);
}

/// Publish the result of one update, and persist the freshness it establishes.
///
/// The timestamp goes to the **database**, not just this struct: the MCP
/// server and the CLI are separate, short-lived processes, and freshness that
/// only lives in this app's memory cannot be reported by the surface an agent
/// actually calls.
fn record(
    core: &Core,
    t: &Target,
    state: &Arc<RootState>,
    health: &Health,
    outcome: Result<marrow_ingest::IngestOutcome>,
    why: &str,
) {
    match outcome {
        Ok(o) => {
            let now = Timestamp::now();
            state
                .last_change_ms
                .store(now.as_millis(), Ordering::Relaxed);
            if o.stored > 0 {
                state.files_applied.fetch_add(o.stored, Ordering::Relaxed);
                tracing::info!(root = %t.name, files = o.stored, reason = why, "index updated");
            }
            if let Err(e) = core
                .store()
                .mark_reconciled(t.root_id, sql_health(health), now)
            {
                tracing::warn!(error = %e, "could not record reconciliation");
            }
        }
        Err(e) => tracing::warn!(error = %e, root = %t.name, "an index update failed"),
    }
}

fn set_health(core: &Core, t: &Target, state: &Arc<RootState>, health: &Health) {
    if let Ok(mut g) = state.health.lock() {
        *g = (
            health.label().to_string(),
            health.reason().map(str::to_string),
        );
    }
    if let Err(e) = core
        .store()
        .mark_reconciled(t.root_id, sql_health(health), Timestamp::now())
    {
        tracing::warn!(error = %e, "could not record watcher health");
    }
}

fn stopped(core: &Core, t: &Target, state: &Arc<RootState>, why: &str) {
    tracing::warn!(root = %t.name, reason = why, "not watching this folder");
    if let Ok(mut g) = state.health.lock() {
        *g = ("stopped".into(), Some(why.to_string()));
    }
    // UNAVAILABLE, not LIVE. The column's schema default says LIVE, which is
    // what made "nobody is watching" indistinguishable from "everything is
    // fine" for every reader of this database.
    if let Err(e) =
        core.store()
            .mark_reconciled(t.root_id, WatcherHealth::Unavailable, Timestamp::now())
    {
        tracing::warn!(error = %e, "could not record that a watcher stopped");
    }
}

fn sql_health(h: &Health) -> WatcherHealth {
    match h {
        Health::Live => WatcherHealth::Live,
        Health::Degraded(_) => WatcherHealth::Degraded,
        Health::PollOnly(_) => WatcherHealth::PollOnly,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Watchers` with the given roots and no threads behind them.
    ///
    /// `sweep_now` only ever touches the flags and the health labels, so the
    /// threads are exactly the part that does not need to exist for these —
    /// and starting real ones would put a filesystem walk inside a unit test.
    fn watchers(roots: &[(&str, &str)]) -> Watchers {
        Watchers {
            cancel: Cancel::new(),
            roots: Mutex::new(
                roots
                    .iter()
                    .map(|(name, health)| {
                        (
                            (*name).to_string(),
                            Arc::new(RootState {
                                health: Mutex::new(((*health).to_string(), None)),
                                ..RootState::default()
                            }),
                        )
                    })
                    .collect(),
            ),
            threads: Mutex::new(Vec::new()),
        }
    }

    fn requested(w: &Watchers, name: &str) -> bool {
        let roots = w.roots.lock().expect("not poisoned");
        let (_, s) = roots.iter().find(|(n, _)| n == name).expect("that root");
        s.sweep_requested.load(Ordering::Acquire)
    }

    #[test]
    fn a_sweep_request_reaches_every_running_root() {
        // "Run an index" means all of them, not the first one: a second folder
        // that quietly never reconciles is the failure this app already had.
        let w = watchers(&[("notes", "live"), ("photos", "poll-only")]);
        assert_eq!(w.sweep_now().expect("both are watched"), 2);
        assert!(requested(&w, "notes"));
        assert!(requested(&w, "photos"));
    }

    #[test]
    fn a_stopped_root_is_not_counted_as_asked() {
        // Nothing would ever pick the flag up. Counting it would make the
        // window report "checking your folders" while nothing checked them,
        // which is the exact shape of the staleness bug this page reports on.
        let w = watchers(&[("notes", "live"), ("archive", "stopped")]);
        assert_eq!(w.sweep_now().expect("one is watched"), 1);
        assert!(requested(&w, "notes"));
        assert!(!requested(&w, "archive"));
    }

    #[test]
    fn when_nothing_is_watching_the_refusal_names_the_folder_and_the_way_out() {
        // A refusal that does not say which folder or what to do is a dead
        // button with extra steps.
        let w = watchers(&[("archive", "stopped")]);
        let e = w.sweep_now().expect_err("nothing can sweep");
        assert_eq!(e.code(), marrow_core::Code::CfgInvalid);
        assert!(e.message().contains("archive"), "{}", e.message());
        assert!(e.message().contains("Reopening"), "{}", e.message());
    }

    #[test]
    fn with_no_folders_at_all_the_refusal_points_at_adding_one() {
        // The empty install. "Nothing happened" would be true and useless.
        let e = watchers(&[]).sweep_now().expect_err("no roots");
        assert!(e.message().contains("Add a folder"), "{}", e.message());
    }
}
