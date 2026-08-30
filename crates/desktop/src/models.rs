//! The model hub: everything the Models page reads.
//!
//! Owns the one long-lived model thread the supervisor runs on ([Part 8 §142])
//! and the registry it supervises. The window talks to it through snapshots,
//! never by reaching into its state — the supervisor's whole job is to be the
//! single place a decision is made.
//!
//! **Nothing can run yet.** S4 brings the worker process; until then this
//! answers "what could run here, and what is stopping it", which is exactly the
//! part that must be right before a single byte is downloaded (§150, S1).
//!
//! [Part 8 §142]: ../../../docs/Part_8_Model_Runtime.md

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use marrow_hw::{
    assess, choose, default_profile, offerable, KvPrecision, Machine, Probe, Profile, Sampler,
    Workload,
};
use marrow_model::detect::{self, Scan};
use marrow_model::download::{self, Https, Progress, Stage};
use marrow_model::queue::Cancel;
use marrow_model::registry::Registry;
use marrow_model::scratch::ModelWorkspace;
use marrow_model::supervisor::{self, Command, Event, ModelState, Supervisor};
use serde::Serialize;

/// How often the machine is sampled while the app is open.
///
/// Two seconds: fast enough that an admission decision is never made on a
/// picture of the machine from before the user opened a browser, slow enough
/// that the sampler is invisible in Activity Monitor.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(2);

/// One row on the Models page.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRow {
    pub id: String,
    pub display_name: String,
    pub family: String,
    pub params_b: f64,
    pub quantization: String,
    pub format: String,
    pub context_limit: u32,
    pub role: String,
    /// `catalogue` · `detected` · `user_supplied`
    pub source: String,
    /// The runtime it was detected in, when it was detected.
    pub detected_in: Option<String>,
    pub installed: bool,
    pub downloadable: bool,
    /// Why the download button is absent, phrased for a human. `None` when
    /// there is nothing to explain.
    pub blocked_reason: Option<String>,
    /// Where the weights come from, and the **commit** they are pinned to.
    pub repo: Option<String>,
    pub revision_short: Option<String>,
    pub file_count: usize,
    pub download_bytes: u64,
    /// The context it is sized at, and the ceiling it advertises. Both, because
    /// a model with a 262144 ceiling sized at 8k needs the difference explained.
    pub run_context: u32,
    /// True when the KV figure was read from the model's own config rather
    /// than falling back to the conservative constant.
    pub kv_measured: bool,
    /// Live transfer state, when one is running.
    pub progress: Option<Progress>,
    pub licence: String,
    pub licence_url: Option<String>,
    /// `None` means not established, which is neither yes nor no (LIC-004).
    pub commercial_use: Option<bool>,
    pub capabilities: Vec<String>,
    /// Why the Fast/Thorough switch is disabled for this model (GEN-013).
    pub reasoning_unavailable: Option<String>,
    /// `comfortable` · `tight` · `too_large`
    pub fit: String,
    pub fit_reason: String,
    /// weights · KV · runtime · embedder · reserve, already formatted.
    pub breakdown: String,
    pub required_bytes: u64,
    pub state: ModelState,
    pub consecutive_failures: u32,
    pub suspended_reason: Option<String>,
}

/// One choice on the AI preference control.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileRow {
    pub id: String,
    pub label: String,
    pub detail: String,
    pub generator_params_b: f64,
    pub selected: bool,
    pub available: bool,
    /// Why it is unavailable, with the arithmetic (TIER-026).
    pub unavailable_reason: Option<String>,
}

/// What the whole page renders from.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsSnapshot {
    /// The probe's own words, so the recommendation is inspectable.
    pub machine: String,
    pub tier_headline: String,
    pub unified_memory: bool,
    pub total_bytes: u64,
    /// From the sampler, not the probe (LLM-019).
    pub available_bytes: u64,
    pub sustained_load: f32,
    pub thermal: String,
    /// True when the sampler has stopped reporting. The page says so rather
    /// than showing a stale number as though it were current (HW-015).
    pub sample_stale: bool,
    pub resident_bytes: u64,
    /// Why the model directory is unusable, if it is. Reported rather than
    /// worked around (SUP-011).
    pub models_dir_problem: Option<String>,
    pub detected: Vec<DetectedRow>,
    /// Runtimes that answered but could not be read. Never silent, because
    /// silence looks identical to "nothing installed".
    pub detection_problems: Vec<String>,
    pub profiles: Vec<ProfileRow>,
    pub router: RoleRow,
    pub generator: RoleRow,
    pub embedder: RoleRow,
    pub models: Vec<ModelRow>,
    /// One sentence about what this build can and cannot do yet. Shown at the
    /// top of the page, because a page full of models that cannot run needs to
    /// say so before the user clicks one.
    pub runtime_status: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedRow {
    pub runtime: String,
    pub port: u16,
    pub model_count: usize,
}

/// One tier of the tiered design (§139.5), so the page can show where the
/// memory goes rather than only a total.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleRow {
    pub workload: String,
    pub params_b: f64,
    pub resident: bool,
    pub why: String,
}

/// Live state shared with the supervisor thread.
#[derive(Debug)]
pub struct Hub {
    machine: Machine,
    sampler: Sampler,
    /// The registry is the supervisor's, but the page needs to read it and a
    /// download thread needs to flip `installed` when it finishes. Shared
    /// rather than copied, so those three never disagree.
    registry: Arc<Mutex<Registry>>,
    scan: Mutex<Scan>,
    profile: Mutex<Profile>,
    /// State the supervisor has reported, so the page does not have to ask a
    /// thread a question and wait for the answer.
    states: Arc<Mutex<Vec<(String, ModelState)>>>,
    /// Where weights live. `None` when the directory could not be opened —
    /// which happens when it would sit inside an indexed folder, and is
    /// reported rather than worked around.
    workspace: Option<ModelWorkspace>,
    workspace_problem: Option<String>,
    /// One entry per transfer in flight or recently finished, so a page that
    /// refetches every four seconds keeps showing the bar.
    downloads: Arc<Mutex<BTreeMap<String, Progress>>>,
    cancels: Mutex<BTreeMap<String, Cancel>>,
    commands: Sender<Command>,
    _supervisor: JoinHandle<()>,
    _events: JoinHandle<()>,
}

impl Hub {
    /// Probe the machine, detect what is already installed, and start the
    /// supervisor thread.
    ///
    /// Detection runs here rather than lazily because it is the difference
    /// between the page opening with the user's own models on it and the page
    /// opening empty and filling in a moment later.
    pub fn start(models_dir: PathBuf, indexed_roots: &[PathBuf]) -> Self {
        let machine = Probe::run();
        let scan = detect::scan();

        // SUP-011: refuses outright if it would sit inside an indexed folder,
        // because a model writing there would have its own output re-indexed
        // and cited back.
        let (workspace, workspace_problem) = match ModelWorkspace::open(&models_dir, indexed_roots)
        {
            Ok(w) => {
                // SUP-015: orphaned scratch from a previous crash, before
                // anything can hand out a new directory.
                if let Err(e) = w.clean_orphaned_scratch() {
                    tracing::warn!("could not clean orphaned scratch: {e}");
                }
                (Some(w), None)
            }
            Err(e) => (None, Some(e.message().to_string())),
        };

        let mut registry = Registry::with_builtin_catalogue();
        // A model whose weights are already on disk is installed, whatever the
        // catalogue's default says.
        if let Some(w) = &workspace {
            for e in registry.iter_mut() {
                if let Some(d) = &e.manifest_digest {
                    e.installed = w.is_installed(d);
                }
            }
        }
        for e in scan.entries.iter().cloned() {
            registry.insert(e);
        }

        let sampler = Sampler::new(machine.cpu_cores, SAMPLE_INTERVAL);
        // One reading before anything asks, so the first paint shows a real
        // number rather than the pessimistic unknown.
        sampler.tick();

        let (ctx, crx) = mpsc::channel();
        let (etx, erx) = mpsc::channel();
        let supervisor = Supervisor::new(machine.clone(), registry.clone());
        let sup_sampler = Sampler::new(machine.cpu_cores, SAMPLE_INTERVAL);
        let handle = std::thread::Builder::new()
            .name("marrow-supervisor".into())
            .spawn(move || supervisor::run(supervisor, sup_sampler, crx, etx))
            .expect("supervisor thread");

        let states: Arc<Mutex<Vec<(String, ModelState)>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&states);
        let events = std::thread::Builder::new()
            .name("marrow-supervisor-events".into())
            .spawn(move || {
                for event in erx {
                    if let Event::StateChanged {
                        model_id,
                        to,
                        reason,
                        ..
                    } = event
                    {
                        tracing::info!(model = %model_id, ?to, %reason, "model state");
                        let Ok(mut s) = sink.lock() else { return };
                        match s.iter_mut().find(|(id, _)| *id == model_id) {
                            Some(slot) => slot.1 = to,
                            None => s.push((model_id, to)),
                        }
                    }
                }
            })
            .expect("supervisor event thread");

        let profile = default_profile(&machine);
        Self {
            machine,
            sampler,
            registry: Arc::new(Mutex::new(registry)),
            scan: Mutex::new(scan),
            profile: Mutex::new(profile),
            states,
            workspace,
            workspace_problem,
            downloads: Arc::new(Mutex::new(BTreeMap::new())),
            cancels: Mutex::new(BTreeMap::new()),
            commands: ctx,
            _supervisor: handle,
            _events: events,
        }
    }

    /// Re-run detection. Cheap, and the answer changes whenever the user starts
    /// or stops Ollama — which they will do while this page is open.
    pub fn refresh_detection(&self) {
        let fresh = detect::scan();
        let mut registry = Registry::with_builtin_catalogue();
        for e in fresh.entries.iter().cloned() {
            registry.insert(e);
        }
        if let Ok(mut r) = self.registry.lock() {
            *r = registry;
        }
        if let Ok(mut s) = self.scan.lock() {
            *s = fresh;
        }
    }

    /// Start fetching a model.
    ///
    /// Runs on its own thread and reports into `downloads`, so a page that
    /// refetches every four seconds keeps showing the same bar rather than a
    /// value that appears and vanishes.
    pub fn start_download(&self, model_id: &str) -> marrow_core::Result<()> {
        let Some(workspace) = self.workspace.clone() else {
            return Err(marrow_core::Error::new(
                marrow_core::Code::CfgInvalid,
                self.workspace_problem
                    .clone()
                    .unwrap_or_else(|| "The model directory is unavailable.".into()),
            ));
        };
        let entry = self
            .registry
            .lock()
            .ok()
            .and_then(|r| r.get(model_id).cloned())
            .ok_or_else(|| {
                marrow_core::Error::new(
                    marrow_core::Code::ModNotInstalled,
                    format!("No model called {model_id}."),
                )
            })?;
        if !entry.downloadable() {
            return Err(marrow_core::Error::new(
                marrow_core::Code::ModIntegrityFailed,
                format!(
                    "{} has no verified manifest, so it cannot be downloaded.",
                    entry.display_name
                ),
            ));
        }

        let cancel = Cancel::new();
        {
            let mut c = self.cancels.lock().map_err(|_| poisoned())?;
            if c.contains_key(model_id) {
                // Two transfers of the same model would race on the same
                // partial directory and each would see the other's bytes.
                return Err(marrow_core::Error::new(
                    marrow_core::Code::ModQueueFull,
                    format!("{} is already downloading.", entry.display_name),
                ));
            }
            c.insert(model_id.to_string(), cancel.clone());
        }

        let downloads = Arc::clone(&self.downloads);
        let registry_slot = Arc::clone(&self.registry);
        let id = model_id.to_string();
        std::thread::Builder::new()
            .name(format!("marrow-download-{id}"))
            .spawn(move || {
                let mut report = |p: Progress| {
                    if let Ok(mut d) = downloads.lock() {
                        d.insert(p.model_id.clone(), p);
                    }
                };
                let result =
                    download::download(&entry, &workspace, &Https, &cancel, &mut report);
                match result {
                    Ok(_) => {
                        // Flip the registry entry, so the next snapshot shows
                        // it installed without waiting for a re-detect.
                        if let Ok(mut r) = registry_slot.lock() {
                            if let Some(e) = r.get_mut(&entry.id) {
                                e.installed = true;
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(model = %entry.id, code = %e.code(), "download failed: {}", e.message());
                        if let Ok(mut d) = downloads.lock() {
                            d.insert(
                                entry.id.clone(),
                                Progress {
                                    model_id: entry.id.clone(),
                                    stage: Stage::Failed {
                                        code: e.code().as_str().to_string(),
                                        reason: e.message().to_string(),
                                    },
                                    bytes_done: 0,
                                    bytes_total: entry.download_bytes(),
                                    bytes_per_sec: 0,
                                    eta_secs: None,
                                },
                            );
                        }
                    }
                }
            })
            .map_err(|e| {
                marrow_core::Error::new(
                    marrow_core::Code::IntInvariantViolated,
                    "Could not start the download thread.",
                )
                .with_source(e)
            })?;

        // Only clear the in-flight marker once the thread has been handed the
        // cancel token, so a cancel arriving immediately still reaches it.
        Ok(())
    }

    /// Cancel a transfer. What was fetched is kept, so starting again resumes.
    pub fn cancel_download(&self, model_id: &str) -> bool {
        match self.cancels.lock() {
            Ok(mut c) => match c.remove(model_id) {
                Some(cancel) => {
                    cancel.cancel();
                    true
                }
                None => false,
            },
            Err(_) => false,
        }
    }

    /// Clear a finished or failed transfer from the page.
    pub fn dismiss_download(&self, model_id: &str) {
        if let Ok(mut d) = self.downloads.lock() {
            d.remove(model_id);
        }
        let _ = self.cancels.lock().map(|mut c| c.remove(model_id));
    }

    pub fn set_profile(&self, id: &str) -> Option<Profile> {
        let p = match id {
            "efficient" => Profile::Efficient,
            "balanced" => Profile::Balanced,
            "larger_local" => Profile::LargerLocal,
            "cloud" => Profile::Cloud,
            _ => return None,
        };
        // TIER-028: takes effect on the next request, never interrupting one
        // in flight. Nothing is in flight yet, but the ordering is the point.
        *self.profile.lock().ok()? = p;
        Some(p)
    }

    pub fn snapshot(&self) -> ModelsSnapshot {
        self.sampler.tick();
        let conditions = self.sampler.conditions(SAMPLE_INTERVAL * 4);
        let available = conditions.min_available_bytes;

        let states = self.states.lock().map(|s| s.clone()).unwrap_or_default();
        let scan = self.scan.lock().map(|s| s.clone()).unwrap_or_default();
        let profile = self.profile.lock().map(|p| *p).unwrap_or_default();
        let downloads = self.downloads.lock().map(|d| d.clone()).unwrap_or_default();

        // Reap transfers that have finished, so a second click starts a new
        // one instead of being told it is already downloading.
        if let Ok(mut c) = self.cancels.lock() {
            c.retain(|id, _| {
                !matches!(
                    downloads.get(id).map(|p| &p.stage),
                    Some(Stage::Ready) | Some(Stage::Failed { .. }) | Some(Stage::Cancelled)
                )
            });
        }

        let models = self
            .registry
            .lock()
            .map(|r| {
                r.iter()
                    .map(|e| {
                        // The run context, never the ceiling — see
                        // `Entry::shape`. Sizing Qwen3.5-4B at its advertised
                        // 262144 asks for 34 GB of cache.
                        let verdict = assess(
                            &self.machine,
                            &e.shape(e.default_context, KvPrecision::F16),
                            available,
                        );
                        let state = states
                            .iter()
                            .find(|(id, _)| id == &e.id)
                            .map(|(_, s)| s.clone())
                            .unwrap_or(if e.installed {
                                ModelState::Installed
                            } else {
                                ModelState::Absent
                            });
                        row(e, &verdict, state, downloads.get(&e.id).cloned())
                    })
                    .collect()
            })
            .unwrap_or_default();

        ModelsSnapshot {
            machine: self.machine.summary(),
            tier_headline: self.machine.tier.headline().to_string(),
            unified_memory: self.machine.unified_memory,
            total_bytes: self.machine.total_memory_bytes,
            available_bytes: available,
            sustained_load: conditions.sustained_load,
            thermal: format!("{:?}", conditions.latest.thermal).to_lowercase(),
            sample_stale: conditions.stale,
            resident_bytes: 0,
            models_dir_problem: self.workspace_problem.clone(),
            detected: scan
                .detected
                .iter()
                .map(|d| DetectedRow {
                    runtime: d.runtime.label().to_string(),
                    port: d.port,
                    model_count: d.model_count,
                })
                .collect(),
            detection_problems: scan.problems.clone(),
            profiles: self.profile_rows(profile),
            router: role_row(profile, Workload::Routing),
            generator: role_row(profile, Workload::Generation),
            embedder: role_row(profile, Workload::Embedding),
            models,
            runtime_status: RUNTIME_STATUS.to_string(),
        }
    }

    fn profile_rows(&self, selected: Profile) -> Vec<ProfileRow> {
        [
            ("efficient", Profile::Efficient),
            ("balanced", Profile::Balanced),
            ("larger_local", Profile::LargerLocal),
            ("cloud", Profile::Cloud),
        ]
        .into_iter()
        .map(|(id, p)| {
            let availability = offerable(&self.machine, p);
            ProfileRow {
                id: id.to_string(),
                label: p.label().to_string(),
                detail: p.detail().to_string(),
                generator_params_b: choose(p, Workload::Generation).params_b,
                selected: p == selected,
                available: availability.is_ok(),
                unavailable_reason: availability.err(),
            }
        })
        .collect()
    }

    /// Stop the supervisor thread. Called on window close so a relaunch does
    /// not leave one sampling behind it.
    pub fn shutdown(&self) {
        let _ = self.commands.send(Command::Shutdown);
    }
}

/// The one sentence at the top of the page. It says what this build cannot do,
/// because a page listing four models that cannot run must not read as a page
/// of four models that can.
const RUNTIME_STATUS: &str = "No inference runtime is wired up yet, so nothing here can \
     answer a question. This page reports what this machine could run and what \
     is stopping each model, which is the part that has to be right before \
     anything is downloaded.";

fn poisoned() -> marrow_core::Error {
    marrow_core::Error::invariant("the model registry lock was poisoned")
}

fn role_row(profile: Profile, workload: Workload) -> RoleRow {
    let c = choose(profile, workload);
    RoleRow {
        workload: format!("{workload:?}").to_lowercase(),
        params_b: c.params_b,
        resident: c.resident,
        why: c.why.to_string(),
    }
}

fn row(
    e: &marrow_model::registry::Entry,
    v: &marrow_hw::Verdict,
    state: ModelState,
    progress: Option<Progress>,
) -> ModelRow {
    let (source, detected_in) = match &e.source {
        marrow_model::registry::Source::Catalogue => ("catalogue".to_string(), None),
        marrow_model::registry::Source::Detected { runtime } => {
            ("detected".to_string(), Some(runtime.clone()))
        }
        marrow_model::registry::Source::UserSupplied { .. } => ("user_supplied".to_string(), None),
    };

    let blocked_reason = if e.installed {
        None
    } else if !e.downloadable() {
        // Never a button that cannot work: say what is missing instead.
        Some(
            "No verified manifest for this model, so it cannot be downloaded. \
             A download that cannot be checked cannot be told apart from a \
             corrupted or substituted one."
                .to_string(),
        )
    } else if !v.offerable() {
        Some(v.reason.clone())
    } else {
        None
    };

    let mut capabilities = Vec::new();
    for (on, name) in [
        (e.capabilities.tools, "tools"),
        (e.capabilities.structured_output, "structured output"),
        (e.capabilities.reasoning, "reasoning"),
        (e.capabilities.vision, "vision"),
        (e.capabilities.multilingual, "multilingual"),
        (e.capabilities.embedding, "embedding"),
    ] {
        if on {
            capabilities.push(name.to_string());
        }
    }

    let suspended_reason = match &state {
        ModelState::Suspended { reason } => Some(reason.clone()),
        _ => None,
    };

    ModelRow {
        id: e.id.clone(),
        display_name: e.display_name.clone(),
        family: e.family.clone(),
        params_b: e.params_b,
        quantization: e.quantization.label().to_string(),
        format: format!("{:?}", e.format).to_lowercase(),
        context_limit: e.context_limit,
        role: e.role.clone(),
        source,
        detected_in,
        installed: e.installed,
        // A model that cannot fit is listed, and its download is not offered:
        // pulling 3 GB for something that will be refused at admission is a
        // waste the user cannot undo.
        downloadable: e.downloadable() && v.offerable(),
        blocked_reason,
        repo: e.repo.clone(),
        revision_short: e
            .revision
            .as_ref()
            .map(|r| r[..12.min(r.len())].to_string()),
        file_count: e.files.len(),
        download_bytes: e.download_bytes(),
        run_context: e.default_context,
        kv_measured: e.kv_bytes_per_token.is_some(),
        progress,
        licence: e.licence.spdx_or_name.clone(),
        licence_url: e.licence.url.clone(),
        commercial_use: e.licence.commercial_use,
        capabilities,
        reasoning_unavailable: e.reasoning_unavailable_because(),
        fit: format!("{:?}", v.fit)
            .chars()
            .flat_map(|c| {
                if c.is_uppercase() {
                    vec!['_', c.to_ascii_lowercase()]
                } else {
                    vec![c]
                }
            })
            .skip(1)
            .collect(),
        fit_reason: v.reason.clone(),
        breakdown: v.breakdown.clone(),
        required_bytes: v.requirement.total(),
        state,
        consecutive_failures: e.breaker.consecutive_failures,
        suspended_reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hub whose model directory is a fresh temporary one, so a test never
    /// touches (or is confused by) the real `~/.local/share/marrow/models`.
    fn test_hub() -> (tempfile::TempDir, Hub) {
        let t = tempfile::tempdir().unwrap();
        let hub = Hub::start(t.path().join("models"), &[]);
        (t, hub)
    }

    #[test]
    fn the_page_says_that_nothing_can_run_yet() {
        // A page listing four models that cannot run must not read as a page
        // of four models that can.
        assert!(RUNTIME_STATUS.contains("No inference runtime"));
        assert!(RUNTIME_STATUS.contains("answer a question"));
        assert!(
            !RUNTIME_STATUS.contains("coming soon"),
            "say what is true now, not what is planned"
        );
    }

    #[test]
    fn a_catalogue_model_is_downloadable_and_names_its_source() {
        // The blocker this replaced: every row used to say "no verified
        // digest yet". They are pinned now, and the row must show where from.
        let e = marrow_model::catalogue::builtin()
            .into_iter()
            .next()
            .unwrap();
        let machine = Machine {
            total_memory_bytes: 17_179_869_184,
            unified_memory: true,
            ..Machine::unknown()
        };
        let v = assess(
            &machine,
            &e.shape(e.default_context, KvPrecision::F16),
            9_000_000_000,
        );
        let r = row(&e, &v, ModelState::Absent, None);
        assert!(r.downloadable, "{:?}", r.blocked_reason);
        assert_eq!(r.blocked_reason, None);
        assert!(r.repo.as_deref().unwrap().starts_with("mlx-community/"));
        assert_eq!(r.revision_short.as_deref().unwrap().len(), 12);
        assert!(r.file_count > 1, "a model is a directory, not a blob");
        assert!(r.download_bytes > 100_000_000);
        assert!(
            r.kv_measured,
            "the pinned entries carry a measured KV figure"
        );
    }

    #[test]
    fn a_model_that_does_not_fit_is_not_offered_for_download() {
        // Pulling 3 GB for something admission will refuse is a waste the
        // user cannot undo.
        let e = marrow_model::catalogue::builtin()
            .into_iter()
            .next()
            .unwrap();
        let tiny = Machine {
            total_memory_bytes: 4_000_000_000,
            unified_memory: true,
            ..Machine::unknown()
        };
        let v = assess(
            &tiny,
            &e.shape(e.default_context, KvPrecision::F16),
            1_000_000,
        );
        let r = row(&e, &v, ModelState::Absent, None);
        assert!(!r.downloadable);
        assert!(r.blocked_reason.as_deref().unwrap().contains("GB"));
    }

    #[test]
    fn the_run_context_is_reported_beside_the_ceiling() {
        // A model advertising 262144 and sized at 8192 needs the difference
        // shown, or the page looks like it is ignoring the model's own spec.
        let e = marrow_model::catalogue::builtin()
            .into_iter()
            .find(|e| e.id == "qwen3.5-4b-mlx-q4")
            .unwrap();
        let machine = Machine {
            total_memory_bytes: 17_179_869_184,
            unified_memory: true,
            ..Machine::unknown()
        };
        let v = assess(
            &machine,
            &e.shape(e.default_context, KvPrecision::F16),
            9_000_000_000,
        );
        let r = row(&e, &v, ModelState::Absent, None);
        assert_eq!(r.run_context, 8192);
        assert!(r.context_limit > r.run_context * 8);
    }

    #[test]
    fn fit_serialises_as_snake_case_for_the_ui_to_branch_on() {
        // The UI branches on this, so it must not be `TooLarge` one day and
        // `too_large` the next.
        let e = marrow_model::catalogue::builtin()
            .into_iter()
            .next()
            .unwrap();
        let machine = Machine {
            total_memory_bytes: 4_000_000_000,
            unified_memory: true,
            ..Machine::unknown()
        };
        let v = assess(&machine, &e.shape(8192, KvPrecision::F16), 1_000_000);
        assert_eq!(row(&e, &v, ModelState::Absent, None).fit, "too_large");
    }

    #[test]
    fn an_installed_model_has_nothing_to_explain() {
        let mut e = marrow_model::catalogue::builtin()
            .into_iter()
            .next()
            .unwrap();
        e.installed = true;
        let machine = Machine {
            total_memory_bytes: 17_179_869_184,
            unified_memory: true,
            ..Machine::unknown()
        };
        let v = assess(&machine, &e.shape(8192, KvPrecision::F16), 9_000_000_000);
        assert_eq!(
            row(&e, &v, ModelState::Installed, None).blocked_reason,
            None
        );
    }

    #[test]
    fn the_hub_starts_and_stops_without_leaking_a_thread() {
        let (_tmp, hub) = test_hub();
        let s = hub.snapshot();
        assert!(!s.machine.is_empty());
        assert!(!s.models.is_empty(), "the catalogue must always be listed");
        assert_eq!(s.profiles.len(), 4);
        assert_eq!(s.profiles.iter().filter(|p| p.selected).count(), 1);
        hub.shutdown();
    }

    #[test]
    fn the_snapshot_reports_live_memory_not_the_probe() {
        // LLM-019: a recommendation made at launch is wrong by the time it is
        // acted on.
        let (_tmp, hub) = test_hub();
        let s = hub.snapshot();
        assert!(s.available_bytes > 0, "the sampler must have run");
        assert!(
            s.available_bytes < s.total_bytes,
            "free must be less than total: {} vs {}",
            s.available_bytes,
            s.total_bytes
        );
        hub.shutdown();
    }

    #[test]
    fn the_tiering_is_visible_rather_than_only_a_total() {
        // §139.5: the router and the embedder are resident; the generator is
        // not. If the page cannot show that, the design is invisible.
        let (_tmp, hub) = test_hub();
        let s = hub.snapshot();
        assert!(s.router.resident);
        assert!(s.embedder.resident);
        assert!(!s.generator.resident);
        assert!(s.generator.params_b > s.router.params_b);
        hub.shutdown();
    }

    #[test]
    fn every_catalogue_model_offers_a_download_now() {
        // The blocker, as the page sees it.
        let (_t, hub) = test_hub();
        let s = hub.snapshot();
        let catalogue: Vec<_> = s
            .models
            .iter()
            .filter(|m| m.source == "catalogue")
            .collect();
        assert!(catalogue.len() >= 6, "expected the full shortlist");
        for m in catalogue {
            assert!(
                m.downloadable || m.fit == "too_large",
                "{} is neither downloadable nor too large: {:?}",
                m.id,
                m.blocked_reason
            );
        }
        hub.shutdown();
    }

    #[test]
    fn a_download_of_an_unknown_model_is_refused_by_name() {
        let (_t, hub) = test_hub();
        let e = hub.start_download("no-such-model").unwrap_err();
        assert!(e.message().contains("no-such-model"), "{}", e.message());
        hub.shutdown();
    }

    #[test]
    fn cancelling_a_download_that_is_not_running_is_not_an_error() {
        // The page can send it after the transfer already finished.
        let (_t, hub) = test_hub();
        assert!(!hub.cancel_download("qwen3.5-4b-mlx-q4"));
        hub.dismiss_download("qwen3.5-4b-mlx-q4");
        hub.shutdown();
    }

    #[test]
    fn a_model_directory_inside_an_indexed_folder_is_reported_not_worked_around() {
        // SUP-011 / invariant #13. The page says so; it does not quietly pick
        // somewhere else, which would leave the user's setting a lie.
        let t = tempfile::tempdir().unwrap();
        let indexed = t.path().join("Documents");
        std::fs::create_dir_all(&indexed).unwrap();
        let hub = Hub::start(indexed.join("models"), std::slice::from_ref(&indexed));
        let s = hub.snapshot();
        let why = s.models_dir_problem.expect("must report it");
        assert!(why.contains("cited back"), "{why}");
        // And nothing may be offered for download while there is nowhere to
        // put it.
        assert!(hub.start_download("qwen3.5-4b-mlx-q4").is_err());
        hub.shutdown();
    }

    #[test]
    fn an_unknown_profile_id_is_rejected_rather_than_defaulted() {
        // Silently falling back to Balanced would make a typo in the UI look
        // like a working control.
        let (_tmp, hub) = test_hub();
        assert!(hub.set_profile("nonsense").is_none());
        assert_eq!(hub.set_profile("efficient"), Some(Profile::Efficient));
        assert!(hub
            .snapshot()
            .profiles
            .iter()
            .any(|p| p.id == "efficient" && p.selected));
        hub.shutdown();
    }
}
