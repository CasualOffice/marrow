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

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
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

        let due = last_sweep.elapsed() >= marrow_scan::reconcile_interval(&health);
        if hints.rescan_required || due {
            last_sweep = Instant::now();
            // A lost event demands a full sweep. Reconciliation is what makes
            // the index correct; the watcher is only a hint (WATCH-001).
            let why = if hints.rescan_required {
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
