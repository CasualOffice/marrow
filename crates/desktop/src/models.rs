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
    assess, choose, default_profile, offerable, KvPrecision, Machine, Probe, Profile, Requirement,
    Sampler, Workload,
};
use marrow_model::detect::{self, Scan};
use marrow_model::download::{self, Https, Progress, Stage};
use marrow_model::envelope::{Envelope, Session};
use marrow_model::openai::{OpenAiProvider, SystemDns};
use marrow_model::provider::{
    Boundary, Completion, GenerateRequest, GenerationProvider, StreamEvent,
};
use marrow_model::queue::Cancel;
use marrow_model::registry::Registry;
use marrow_model::request::Reasoning;
use marrow_model::scratch::ModelWorkspace;
use marrow_model::secrets::{Keyring, Secret, SecretStore};
use marrow_model::supervisor::{self, Command, Event, ModelState, Supervisor};
use marrow_model::worker::{MlxProvider, Runtime, Worker};
use marrow_model::Embedder;
use serde::Serialize;

use crate::prefs;

/// How often the machine is sampled while the app is open.
///
/// Two seconds: fast enough that an admission decision is never made on a
/// picture of the machine from before the user opened a browser, slow enough
/// that the sampler is invisible in Activity Monitor.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(2);

/// The keyring account the remote provider's key is filed under.
///
/// Fixed rather than taken from the window: an account name that arrives over
/// IPC is a way to name any entry in the user's keychain.
const REMOTE_KEY_ACCOUNT: &str = "cloud-provider";

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
    /// Which local model the user pinned, if any. `None` is "whatever fits" —
    /// shown as such, so an automatic choice is visible as a choice rather
    /// than looking like the only model there is.
    pub pinned_model_id: Option<String>,
    /// Which model would answer a question right now, local or remote. The
    /// page had no way to say this: a user with two models installed could not
    /// tell which one they were about to use.
    pub active_model: Option<String>,
    /// How much of the index semantic search actually covers.
    ///
    /// Shown because the alternative is a user whose results are quietly worse
    /// than they will be in ten minutes, with nothing saying why.
    pub semantic: SemanticStatus,
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
    /// One sentence about what the runtime can do right now. Shown at the top
    /// of the page, because a page full of models that cannot run needs to say
    /// so before the user clicks one.
    pub runtime_status: String,
    /// True when an inference runtime was found. False means every model on
    /// the page can be downloaded and none can answer.
    pub runtime_ready: bool,
    /// The commands that would create one. Named, because "MLX is not
    /// available" is a dead end and this is something the user can do.
    pub runtime_setup: Option<String>,
    /// Whether this build has an archive it can actually install. False means
    /// the button is not offered at all — a button that always fails is worse
    /// than no button, and the hint below it is then the only route.
    pub runtime_installable: bool,
    /// How big the download is, so the offer states its cost before it is
    /// accepted rather than after.
    pub runtime_download_bytes: u64,
    /// Where an install is, while one is running and for a moment after.
    pub runtime_install: Option<marrow_model::runtime::Install>,
    /// The remote endpoint, if one is configured. On this page because
    /// `runtime_status` used to say "nothing leaves this device" as a
    /// constant, and that sentence is only true while this is `None` or off.
    pub remote: ProviderStatus,
}

/// Which generator answers, and where it runs.
///
/// **This is the only place local and remote are told apart** (LLM-029). The
/// discriminant is a *private* field: the ask pipeline holds one of these,
/// reports its boundary, and hands it back to
/// [`Hub::generate_with_progress`] — and it cannot branch on which sort it
/// got, because outside this module there is nothing to branch on. That is a
/// stronger guarantee than a rule about where `if` statements may go.
#[derive(Clone, Debug)]
pub struct Selection {
    kind: Kind,
    /// What the model is called where it runs.
    pub model_id: String,
    /// What to call it on screen (LLM-039). Never "local" and never "cloud".
    pub display: String,
    /// Decided by the provider, stated for every generation (UX-012, LLM-034).
    pub boundary: Boundary,
    /// Where the request goes. `None` when it goes nowhere.
    pub destination: Option<String>,
}

#[derive(Clone, Debug)]
enum Kind {
    Local,
    /// The provider itself, not a copy of its configuration. The boundary the
    /// user is shown and the connection that is made are then the same object,
    /// so they cannot come to disagree between the disclosure and the request.
    Remote(Arc<OpenAiProvider>),
}

/// What the Settings page shows about the remote endpoint.
///
/// **There is no key field.** Whether one is *stored* is a fact the page needs;
/// what it is, is not.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStatus {
    pub configured: bool,
    pub enabled: bool,
    pub label: String,
    pub base_url: String,
    pub model: String,
    pub max_output_tokens: u32,
    pub reasoning_effort: Option<String>,
    /// `local` · `private` · `cloud`, and the same words the answer footer
    /// uses. Resolved from the endpoint's address, so it is a fact rather than
    /// a claim the user typed.
    pub boundary: Option<String>,
    pub boundary_label: Option<String>,
    /// The addresses the connection would be pinned to. A boundary the user
    /// cannot check is a claim rather than a fact.
    pub addresses: Vec<String>,
    /// Whether a key is in the keychain for it.
    pub has_key: bool,
    /// Why it cannot be used, if it cannot. Named rather than discovered on
    /// the first question.
    pub problem: Option<String>,
    /// Which workspaces forbid it outright, and why (MOD-004, LLM-032).
    pub blocked_by: Option<String>,
}

/// Whether semantic search is on, and how far along it is.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticStatus {
    /// True once an embedder has actually loaded.
    pub ready: bool,
    /// Chunks with a vector, and chunks without.
    pub embedded: u64,
    pub remaining: u64,
    pub failed: u64,
    pub running: bool,
    /// Why it is unavailable. `None` when it is fine.
    pub problem: Option<String>,
    pub model: Option<String>,
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
    /// The Python interpreter and worker script, when one was found.
    /// LLM-036: found is not the same as verified — starting a worker proves
    /// it, and this only says a candidate exists.
    ///
    /// Behind a lock because it is no longer decided once at startup: the app
    /// can now install a runtime while the window is open, and a field settled
    /// at `Hub::start` would leave the user looking at "no runtime" until they
    /// quit and reopened the thing that had just told them it was ready.
    runtime: Arc<Mutex<Option<Runtime>>>,
    /// Where the runtime install is, while one is running and for a moment
    /// after, so a page that refetches every four seconds keeps the same bar.
    runtime_install: Arc<Mutex<Option<marrow_model::runtime::Install>>>,
    runtime_cancel: Mutex<Option<Cancel>>,
    data_dir: PathBuf,
    /// One entry per transfer in flight or recently finished, so a page that
    /// refetches every four seconds keeps showing the bar.
    downloads: Arc<Mutex<BTreeMap<String, Progress>>>,
    cancels: Mutex<BTreeMap<String, Cancel>>,
    /// The loaded model, if any. One at a time: this machine has room for one
    /// 4B and the things around it, and juggling two would spend the budget
    /// the whole tiering exists to protect (§139.5).
    loaded: Mutex<Option<Loaded>>,
    /// Which local model the user pinned, if they did. `None` is "whatever
    /// fits", which is the old behaviour kept as the default rather than as
    /// the only option.
    pinned: Mutex<Option<String>>,
    /// Answers in flight, so Escape reaches the right one.
    asks: Mutex<BTreeMap<String, Cancel>>,
    /// The embedder, resident once loaded.
    embedder: Mutex<Option<std::sync::Arc<Embedder>>>,
    /// Why it could not be loaded, said once.
    embedder_problem: Mutex<Option<String>>,
    /// How far the backfill has got, shared with the thread doing it.
    backfill: Arc<marrow_model::backfill::Progress>,
    /// Present while one is running.
    backfill_cancel: Arc<Mutex<Option<Cancel>>>,
    /// The remote endpoint the user configured, if any. Held here rather than
    /// re-read per question so the Settings page and the ask path cannot
    /// disagree about what is configured.
    remote: Mutex<Option<prefs::RemoteProvider>>,
    /// Where a provider key is read from (LLM-030). A trait rather than the
    /// keychain directly, so a test never prompts for a login password.
    secrets: Arc<dyn SecretStore>,
    /// The last answer [`Hub::provider_status`] gave, and when.
    ///
    /// The Models page refetches every four seconds, and that call resolves a
    /// hostname and reads the keychain. Neither is free and neither changes
    /// between paints: a DNS query every four seconds for the life of an open
    /// window is a lot of traffic to produce a label, and a keychain read is
    /// the thing macOS may put a dialog in front of.
    provider_cache: Mutex<Option<(std::time::Instant, ProviderStatus)>>,
    /// One conversation's accumulated state: the delimiter, and the evidence
    /// already sent. Held here rather than in the window because both exist to
    /// make the prompt cache hit, and the window has no business knowing that.
    sessions: Mutex<BTreeMap<String, Conversation>>,
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

        let data_dir = models_dir.parent().unwrap_or(&models_dir).to_path_buf();
        let runtime = Runtime::discover(&data_dir, worker_script());

        // What the user chose, if they ever did. `default_profile` is the
        // fallback and not a stored value: the hardware default is allowed to
        // move between builds, and it should keep moving for someone who has
        // never expressed an opinion. An unreadable preferences file lands here
        // too, silently — a corrupt preference must not stop the window opening.
        let saved = prefs::load(&data_dir);
        let profile = saved
            .ai_profile
            .unwrap_or_else(|| default_profile(&machine));
        Self {
            remote: Mutex::new(saved.remote_provider),
            pinned: Mutex::new(saved.generator_model_id),
            // Constructing this touches nothing: `keyring` opens the keychain
            // on the first read, and the first read happens only when a remote
            // provider actually answers a question.
            secrets: Arc::new(Keyring),
            provider_cache: Mutex::new(None),
            runtime: Arc::new(Mutex::new(runtime)),
            runtime_install: Arc::new(Mutex::new(None)),
            runtime_cancel: Mutex::new(None),
            data_dir,
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
            loaded: Mutex::new(None),
            asks: Mutex::new(BTreeMap::new()),
            embedder: Mutex::new(None),
            embedder_problem: Mutex::new(None),
            backfill: Arc::new(marrow_model::backfill::Progress::default()),
            backfill_cancel: Arc::new(Mutex::new(None)),
            sessions: Mutex::new(BTreeMap::new()),
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

    /// The runtime, if there is one.
    ///
    /// Read through the lock rather than from a field, because
    /// [`Hub::install_runtime`] can put one there while the window is open.
    pub fn runtime(&self) -> Option<Runtime> {
        self.runtime.lock().ok().and_then(|r| r.clone())
    }

    /// Install the MLX runtime.
    ///
    /// **The reason this exists.** Up to v0.0.4 the app shipped
    /// `mlx_worker.py` in the bundle and expected the interpreter to already
    /// be in the user's data directory — where only the author's hand-made
    /// venv ever put one. Every release verified "the worker is in
    /// `Contents/Resources`" on the machine that built it, which is the one
    /// machine where the missing half cannot be missing. On any other Mac the
    /// app indexed happily and could not answer a question, and the printed
    /// fix began with a command macOS does not have.
    ///
    /// Runs on its own thread and reports into `runtime_install`, the same
    /// shape as a model download, so the page renders it the same way.
    pub fn install_runtime(&self) -> marrow_core::Result<()> {
        if self.runtime().is_some() {
            return Ok(());
        }
        {
            let mut slot = self.runtime_install.lock().map_err(|_| poisoned())?;
            // Already running. Not an error — the user pressed the button
            // twice, which is what people do when something takes four
            // minutes and says nothing.
            if slot.as_ref().is_some_and(|p| !p.is_settled()) {
                return Ok(());
            }
            *slot = None;
        }

        let cancel = Cancel::new();
        if let Ok(mut c) = self.runtime_cancel.lock() {
            *c = Some(cancel.clone());
        }

        let progress = Arc::clone(&self.runtime_install);
        let data_dir = self.data_dir.clone();
        let script = worker_script();
        let runtime_slot = Arc::clone(&self.runtime);

        std::thread::Builder::new()
            .name("marrow-runtime-install".into())
            .spawn(move || {
                let mut sink = |p: marrow_model::runtime::Install| {
                    if let Ok(mut s) = progress.lock() {
                        *s = Some(p);
                    }
                };
                let outcome = marrow_model::runtime::install(
                    &data_dir,
                    script,
                    &marrow_model::runtime::ARCHIVE,
                    &marrow_model::Https,
                    &cancel,
                    &mut sink,
                );
                match outcome {
                    Ok(runtime) => {
                        // Published before the stage flips to Ready, so a page
                        // that reads "ready" and immediately asks a question
                        // cannot find the runtime still absent.
                        if let Ok(mut r) = runtime_slot.lock() {
                            *r = Some(runtime);
                        }
                        if let Ok(mut s) = progress.lock() {
                            *s = Some(marrow_model::runtime::Install {
                                stage: marrow_model::runtime::Stage::Ready,
                                ..s.clone().unwrap_or_else(ready_install)
                            });
                        }
                        tracing::info!("model runtime installed");
                    }
                    Err(e) => {
                        // The code, not just the sentence: MOD_CANCELLED and
                        // MOD_INTEGRITY_FAILED are different rows on the page
                        // and only one of them is worth retrying.
                        tracing::warn!(code = %e.code(), "runtime install failed: {}", e.message());
                        if let Ok(mut s) = progress.lock() {
                            let stage = if e.code() == marrow_core::Code::ModCancelled {
                                marrow_model::runtime::Stage::Cancelled
                            } else {
                                marrow_model::runtime::Stage::Failed {
                                    code: e.code().to_string(),
                                    reason: e.message().to_string(),
                                }
                            };
                            *s = Some(marrow_model::runtime::Install {
                                stage,
                                ..s.clone().unwrap_or_else(ready_install)
                            });
                        }
                    }
                }
            })
            .map_err(|e| {
                marrow_core::Error::new(
                    marrow_core::Code::CfgInvalid,
                    "Could not start setting up the model runtime.",
                )
                .with_source(e)
            })?;
        Ok(())
    }

    /// Stop an install in flight. What was downloaded is kept.
    pub fn cancel_runtime_install(&self) -> bool {
        let Ok(mut c) = self.runtime_cancel.lock() else {
            return false;
        };
        match c.take() {
            Some(cancel) => {
                cancel.cancel();
                true
            }
            None => false,
        }
    }

    /// Clear a settled install row, so a failure the user has read stops being
    /// shown forever.
    pub fn dismiss_runtime_install(&self) {
        if let Ok(mut s) = self.runtime_install.lock() {
            if s.as_ref().is_some_and(|p| p.is_settled()) {
                *s = None;
            }
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
        // **And it survives the process.** This lived in the mutex and nowhere
        // else, so a user who picked Efficient found Balanced waiting for them
        // at the next launch — a control that appears to work and then quietly
        // undoes itself, which is worse than one that is not offered. Written
        // after the in-memory change, so a failure to persist costs the
        // persistence and not the choice.
        crate::prefs::set_ai_profile(&self.data_dir, p);
        Some(p)
    }

    /// What this runtime is, in words, for the envelope's FACT block.
    ///
    /// **"What model are you using?" is not a question about the corpus**, and
    /// until this existed it was answered like one: the pipeline retrieved
    /// chunks containing the word "model", found Rust version pins and a
    /// pricing note about model downloads, and reported that no model name was
    /// listed in the documents — while the footer of that very answer read
    /// `qwen3.5-4b-mlx-q4`. The system knew; it just never told itself.
    ///
    /// **Every sentence here has to stay true when the answer is remote.** It
    /// used to end "No question, file or answer is sent to any external
    /// service", unconditionally — a statement that was true when it was
    /// written, is false the moment a cloud endpoint is configured, and would
    /// have been repeated to the user in the model's own voice.
    pub fn identity(&self, selection: &Selection, thorough: bool) -> String {
        let name = match selection.kind {
            Kind::Local => self
                .registry
                .lock()
                .ok()
                .and_then(|r| {
                    r.iter()
                        .find(|e| e.id == selection.model_id)
                        .map(|e| e.display_name.clone())
                })
                .unwrap_or_else(|| selection.model_id.clone()),
            Kind::Remote(_) => selection.display.clone(),
        };
        let where_it_runs = match (selection.boundary, selection.destination.as_deref()) {
            (Boundary::Local, _) => "locally on this machine, through MLX on Apple Silicon. \
                 No question, file or answer is sent to any external service."
                .to_string(),
            (Boundary::Private, Some(host)) => format!(
                "on {host}, which is a server the user runs. This question and every \
                 evidence block below were sent there over the network to produce this \
                 answer; they did not go to anybody else."
            ),
            (Boundary::Cloud, Some(host)) => format!(
                "at {host}, which is a service run by somebody else. This question and \
                 every evidence block below were sent there over the network to produce \
                 this answer, under the user's own agreement with that provider."
            ),
            (_, None) => "on an endpoint whose address could not be determined".to_string(),
        };
        format!(
            "You are Marrow. You are running {name} (`{}`) {where_it_runs} You are \
             currently in {} mode. This block is what you know about yourself: answer \
             questions about which model you are, or where you run, from it — never from \
             the evidence blocks, which are the user's own files and describe their \
             projects, not you.",
            selection.model_id,
            if thorough {
                "Thorough (you reason before answering)"
            } else {
                "Fast (you answer directly)"
            }
        )
    }

    /// Which generator answers this question, and where it runs.
    ///
    /// The gateway. A configured, enabled remote endpoint wins — the user
    /// turned it on and can turn it off — and otherwise the local choice
    /// [`pick_generator`] makes from the pin, the profile and what fits.
    ///
    /// Returns the reason rather than `None`: "there is nothing to answer
    /// with" and "the endpoint you configured does not resolve" are different
    /// problems with different remedies, and a caller handed an `Option` can
    /// only report the first.
    pub fn generator(&self) -> marrow_core::Result<Selection> {
        let configured = self
            .remote
            .lock()
            .ok()
            .and_then(|r| r.clone())
            .filter(|r| r.enabled);
        if let Some(remote) = configured {
            // Resolved **here**, before anything is retrieved: the boundary
            // gates context assembly (LLM-032), so it has to be known before
            // there is any context to assemble.
            let provider = Arc::new(OpenAiProvider::connect(
                remote.endpoint.clone(),
                remote.label.clone(),
                Arc::clone(&self.secrets),
                Arc::new(marrow_model::openai::Https),
                &SystemDns,
            )?);
            return Ok(Selection {
                model_id: remote.endpoint.model.clone(),
                display: format!("{} · {}", remote.label, remote.endpoint.model),
                boundary: provider.boundary(),
                destination: Some(provider.host().to_string()),
                kind: Kind::Remote(provider),
            });
        }
        match self.local_generator() {
            Some(model_id) => Ok(Selection {
                display: model_id.clone(),
                model_id,
                boundary: Boundary::Local,
                destination: None,
                kind: Kind::Local,
            }),
            None => Err(marrow_core::Error::new(
                marrow_core::Code::ModNotInstalled,
                self.no_generator_message(),
            )),
        }
    }

    /// Why there is nothing to answer with, and what to do about it.
    fn no_generator_message(&self) -> String {
        if self.runtime().is_none() {
            "No inference runtime is installed, so questions cannot be answered yet. \
             Search still works. The Models page has the two commands that install one, \
             or you can point Marrow at an OpenAI-compatible endpoint in Settings."
                .into()
        } else {
            "No model is installed. Download one from the Models page — the \
             recommended one is about 3 GB — or point Marrow at an OpenAI-compatible \
             endpoint in Settings."
                .into()
        }
    }

    /// The installed generative model this machine, this pin and this profile
    /// point at.
    ///
    /// Reads the live memory sample and then hands the decision to
    /// [`pick_generator`], which is where the rules are written down and where
    /// they are tested — a decision made inside a method that needs a running
    /// supervisor, a real registry and whatever memory the machine happened to
    /// have free is a decision no test can pin.
    fn local_generator(&self) -> Option<String> {
        self.sampler.tick();
        let available = self
            .sampler
            .conditions(SAMPLE_INTERVAL * 4)
            .min_available_bytes;
        let registry = self.registry.lock().ok()?;
        let pinned = self.pinned.lock().ok().and_then(|p| p.clone());
        let profile = self.profile.lock().map(|p| *p).unwrap_or_default();
        pick_generator(
            registry.iter(),
            &self.machine,
            available,
            profile,
            pinned.as_deref(),
        )
    }

    /// Generate an answer, loading the model on first use (LLM-024).
    ///
    /// Launch never waits on 4 GB of weights; the first question does, and it
    /// says so through the `Loading` state while it happens.
    pub fn generate(
        &self,
        selection: &Selection,
        envelope: &Envelope,
        thorough: bool,
        cancel: &Cancel,
        on_event: &mut dyn FnMut(StreamEvent),
    ) -> marrow_core::Result<Completion> {
        self.generate_with_progress(
            selection,
            envelope,
            thorough,
            cancel,
            &mut |_, _| {},
            on_event,
        )
    }

    /// As [`Hub::generate`], reporting what it is doing while it does it.
    ///
    /// The first question of a session loads several gigabytes of weights, and
    /// until this existed the window showed nothing between Enter and the first
    /// token. A system with no progress looks slow whether or not it is.
    ///
    /// **The one branch on where the work happens lives here** (LLM-029).
    /// Below it there is a `dyn GenerationProvider` and nothing else, and
    /// above it the pipeline holds a [`Selection`] it cannot inspect.
    pub fn generate_with_progress(
        &self,
        selection: &Selection,
        envelope: &Envelope,
        thorough: bool,
        cancel: &Cancel,
        on_stage: &mut dyn FnMut(&str, &str),
        on_event: &mut dyn FnMut(StreamEvent),
    ) -> marrow_core::Result<Completion> {
        let reasoning = if thorough {
            Reasoning::THOROUGH
        } else {
            Reasoning::Off
        };
        match &selection.kind {
            Kind::Remote(provider) => {
                // Nothing to load and nothing to warm: the stage line says
                // what is actually happening, which is that the excerpts are
                // going somewhere (LLM-033).
                on_stage(
                    "thinking",
                    &format!(
                        "Sending the question and {} excerpt(s) to {}",
                        envelope.disclosure.evidence_blocks,
                        provider.host()
                    ),
                );
                provider.generate(
                    GenerateRequest {
                        model_id: &selection.model_id,
                        envelope,
                        reasoning,
                        // The local budget is a memory question; a remote one
                        // is a cost question, and the user set it.
                        max_output_tokens: provider.endpoint().max_output_tokens,
                        cancel,
                    },
                    on_event,
                )
            }
            Kind::Local => self.generate_locally(
                &selection.model_id,
                envelope,
                reasoning,
                thorough,
                cancel,
                on_stage,
                on_event,
            ),
        }
    }

    #[allow(clippy::too_many_arguments)] // Each is a distinct input; a struct
                                         // would move the list rather than shorten it.
    fn generate_locally(
        &self,
        model_id: &str,
        envelope: &Envelope,
        reasoning: Reasoning,
        thorough: bool,
        cancel: &Cancel,
        on_stage: &mut dyn FnMut(&str, &str),
        on_event: &mut dyn FnMut(StreamEvent),
    ) -> marrow_core::Result<Completion> {
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
        if entry.capabilities.reasoning || !thorough {
            // Fine either way.
        } else {
            // GEN-013: refused with the reason, never silently downgraded.
            return Err(marrow_core::Error::new(
                marrow_core::Code::ModUnsupportedCapability,
                entry
                    .reasoning_unavailable_because()
                    .unwrap_or_else(|| "This model answers directly.".into()),
            ));
        }

        let mut slot = self.loaded.lock().map_err(|_| poisoned())?;
        if slot.as_ref().map(|l| l.model_id.as_str()) != Some(model_id) {
            // SKEL-006: the stage, named. "Loading" for ten seconds with no
            // subject is indistinguishable from hung.
            on_stage(
                "loading",
                &format!(
                    "Loading {} — first question of the session",
                    entry.display_name
                ),
            );
            // Swapping models means the old one's weights and cache go first;
            // holding both would be exactly the memory spike admission exists
            // to prevent.
            *slot = None;
            let runtime = self.runtime().ok_or_else(|| {
                marrow_core::Error::new(
                    marrow_core::Code::CfgInvalid,
                    Runtime::setup_hint(&self.data_dir),
                )
            })?;
            let workspace = self.workspace.clone().ok_or_else(|| {
                marrow_core::Error::new(
                    marrow_core::Code::CfgInvalid,
                    self.workspace_problem
                        .clone()
                        .unwrap_or_else(|| "The model directory is unavailable.".into()),
                )
            })?;
            let digest = entry.manifest_digest.as_deref().ok_or_else(|| {
                marrow_core::Error::new(
                    marrow_core::Code::ModNotInstalled,
                    format!("{} has no local weights to load.", entry.display_name),
                )
            })?;
            let dir = workspace.weights_dir(digest);
            if !dir.is_dir() {
                return Err(marrow_core::Error::new(
                    marrow_core::Code::ModNotInstalled,
                    format!("{} is not downloaded yet.", entry.display_name),
                ));
            }
            let mut worker = Worker::start(&runtime)?;
            worker.load(model_id, &dir)?;

            // The model's own footprint plus half again. Set to the bare
            // estimate it would kill every model whose estimate was slightly
            // low — a worse failure than the runaway it prevents — and set to
            // the machine's total it would never fire before the OS did.
            let budget = Requirement::estimate(
                &self.machine,
                &entry.shape(entry.default_context, KvPrecision::F16),
            )
            .ai_footprint()
                * 3
                / 2;

            *slot = Some(Loaded {
                model_id: model_id.to_string(),
                provider: MlxProvider::new(worker, model_id, entry.display_name.clone())
                    .with_memory_budget(budget),
            });
        }

        on_stage(
            "thinking",
            if thorough {
                "Reading the evidence and reasoning"
            } else {
                "Reading the evidence"
            },
        );
        let provider = &slot.as_ref().expect("just loaded").provider;
        provider.generate(
            GenerateRequest {
                model_id,
                envelope,
                reasoning,
                max_output_tokens: answer_budget(
                    &self.machine,
                    &entry,
                    envelope,
                    thorough,
                    self.sampler
                        .conditions(SAMPLE_INTERVAL * 4)
                        .min_available_bytes,
                ),
                cancel,
            },
            on_event,
        )
    }

    /// What the Settings page shows about the remote endpoint, and what the
    /// ask path would refuse.
    ///
    /// Resolves the address, because the boundary is a fact about where the
    /// packets go rather than about what the user typed — and a page that
    /// says "on your own server" for `api.openai.com` would be the exact
    /// failure UX-012 exists to prevent.
    pub fn provider_status(&self) -> ProviderStatus {
        /// Long enough that a page refetching every four seconds resolves once
        /// a minute; short enough that a laptop moving between networks is
        /// re-labelled before anyone reads the old one.
        const CACHE_FOR: Duration = Duration::from_secs(60);

        if let Ok(cache) = self.provider_cache.lock() {
            if let Some((at, status)) = cache.as_ref() {
                if at.elapsed() < CACHE_FOR {
                    return status.clone();
                }
            }
        }
        let status = self.resolve_provider_status();
        if let Ok(mut cache) = self.provider_cache.lock() {
            *cache = Some((std::time::Instant::now(), status.clone()));
        }
        status
    }

    fn resolve_provider_status(&self) -> ProviderStatus {
        let Some(remote) = self.remote.lock().ok().and_then(|r| r.clone()) else {
            return ProviderStatus::default();
        };
        let mut status = ProviderStatus {
            configured: true,
            enabled: remote.enabled,
            label: remote.label.clone(),
            base_url: remote.endpoint.base_url.clone(),
            model: remote.endpoint.model.clone(),
            max_output_tokens: remote.endpoint.max_output_tokens,
            reasoning_effort: remote.endpoint.reasoning_effort.clone(),
            has_key: matches!(
                self.secrets.get(&remote.endpoint.key_account),
                Ok(Some(ref k)) if !k.is_empty()
            ),
            ..ProviderStatus::default()
        };
        match OpenAiProvider::connect(
            remote.endpoint.clone(),
            remote.label.clone(),
            Arc::clone(&self.secrets),
            Arc::new(marrow_model::openai::Https),
            &SystemDns,
        ) {
            Ok(p) => {
                status.boundary = Some(p.boundary().as_wire().to_string());
                status.boundary_label = Some(p.boundary().label().to_string());
                status.addresses = p.addresses();
            }
            Err(e) => status.problem = Some(e.message().to_string()),
        }
        status
    }

    /// Save the endpoint, and the key if one was given.
    ///
    /// The key goes to the OS keyring and **is not returned, logged or written
    /// to the preferences file** (LLM-030). `None` leaves whatever is already
    /// stored alone, so editing the model name does not require re-typing it.
    pub fn set_remote_provider(
        &self,
        mut provider: prefs::RemoteProvider,
        key: Option<String>,
    ) -> marrow_core::Result<ProviderStatus> {
        // The account is Marrow's to choose. Taking it from the window would
        // let a caller name any entry in the user's keychain.
        provider.endpoint.key_account = REMOTE_KEY_ACCOUNT.to_string();
        if provider.label.trim().is_empty() {
            provider.label = "Remote provider".into();
        }
        provider.endpoint.max_output_tokens = provider.endpoint.max_output_tokens.clamp(256, 8192);
        // Refuses an address that cannot be used before it is saved, so the
        // failure lands on the field the user is looking at rather than on
        // their next question.
        OpenAiProvider::connect(
            provider.endpoint.clone(),
            provider.label.clone(),
            Arc::clone(&self.secrets),
            Arc::new(marrow_model::openai::Https),
            &SystemDns,
        )?;
        if let Some(key) = key {
            let secret = Secret::new(key);
            if secret.is_empty() {
                self.secrets.delete(REMOTE_KEY_ACCOUNT)?;
            } else {
                self.secrets.put(REMOTE_KEY_ACCOUNT, &secret)?;
            }
        }
        *self.remote.lock().map_err(|_| poisoned())? = Some(provider.clone());
        self.forget_provider_status();
        if let Err(e) = prefs::set_remote_provider(&self.data_dir, Some(provider)) {
            tracing::warn!(error = %e, "could not save the provider; it applies until Marrow is closed");
        }
        Ok(self.provider_status())
    }

    /// Drop the cached status, so a change is visible on the next paint
    /// rather than up to a minute later.
    fn forget_provider_status(&self) {
        if let Ok(mut cache) = self.provider_cache.lock() {
            *cache = None;
        }
    }

    /// Forget the endpoint **and the key**.
    ///
    /// Both, together: leaving a key in the keychain for a provider the user
    /// has removed is exactly the kind of thing nobody goes back and cleans up.
    /// Pin a local model, or return to choosing automatically with `None`.
    ///
    /// Refuses an id that is not installed rather than storing it: a
    /// preference that silently does nothing is how "there is no way to choose
    /// a model" and "I chose one and it ignored me" become the same bug report.
    pub fn set_generator_model(&self, model_id: Option<String>) -> marrow_core::Result<()> {
        if let Some(id) = &model_id {
            let ok = self
                .registry
                .lock()
                .map_err(|_| poisoned())?
                .iter()
                .any(|e| &e.id == id && e.installed && !e.capabilities.embedding);
            if !ok {
                return Err(marrow_core::Error::new(
                    marrow_core::Code::ModNotInstalled,
                    format!(
                        "`{id}` is not an installed model that can answer questions. \
                         Download it from the Models page first."
                    ),
                ));
            }
        }
        *self.pinned.lock().map_err(|_| poisoned())? = model_id.clone();
        if let Err(e) = prefs::set_generator_model(&self.data_dir, model_id) {
            tracing::warn!(error = %e, "could not save the model choice; it applies until Marrow is closed");
        }
        Ok(())
    }

    /// What to call the user, if they have said.
    pub fn user_name(&self) -> Option<String> {
        prefs::load(&self.data_dir).user_name
    }

    /// Record — or clear — the user's name. Returns what was stored.
    pub fn set_user_name(&self, name: Option<String>) -> marrow_core::Result<Option<String>> {
        prefs::set_user_name(&self.data_dir, name).map_err(|e| {
            marrow_core::Error::new(
                marrow_core::Code::FsPermissionDenied,
                "Your name could not be saved, so it will be forgotten when Marrow closes. \
                 The preferences file could not be written.",
            )
            .with_source(e)
        })?;
        Ok(prefs::load(&self.data_dir).user_name)
    }

    /// The pinned model, if one is set. `None` means "whatever fits".
    pub fn pinned_model(&self) -> Option<String> {
        self.pinned.lock().ok().and_then(|p| p.clone())
    }

    pub fn clear_remote_provider(&self) -> marrow_core::Result<ProviderStatus> {
        *self.remote.lock().map_err(|_| poisoned())? = None;
        self.forget_provider_status();
        if let Err(e) = prefs::set_remote_provider(&self.data_dir, None) {
            tracing::warn!(error = %e, "could not clear the saved provider");
        }
        self.secrets.delete(REMOTE_KEY_ACCOUNT)?;
        Ok(self.provider_status())
    }

    /// Whether semantic search is on, and how far along it is.
    ///
    /// Deliberately does **not** load the embedder to find out — a page
    /// refreshing every four seconds would otherwise pull 200 MB into memory
    /// the first time someone opened it.
    pub fn semantic_status(&self) -> SemanticStatus {
        let (embedded, remaining, failed) = self.backfill.snapshot();
        SemanticStatus {
            ready: self.embedder.lock().map(|e| e.is_some()).unwrap_or(false),
            embedded,
            remaining,
            failed,
            running: self
                .backfill_cancel
                .lock()
                .map(|c| c.is_some())
                .unwrap_or(false),
            problem: self.embedder_problem(),
            model: self.registry.lock().ok().and_then(|r| {
                r.iter()
                    .find(|e| e.capabilities.embedding)
                    .map(|e| e.id.clone())
            }),
        }
    }

    /// What the loaded model is costing right now (LLM-053).
    ///
    /// Reads the two locks in sequence, never nested: `loaded` is taken by
    /// `generate` for the whole duration of a request, and holding `registry`
    /// across it would make a snapshot wait on an answer.
    fn resident_bytes(&self) -> u64 {
        let id = match self.loaded.lock() {
            Ok(slot) => slot.as_ref().map(|l| l.model_id.clone()),
            Err(_) => None,
        };
        let Some(id) = id else { return 0 };
        self.registry
            .lock()
            .ok()
            .and_then(|r| r.get(&id).and_then(|e| e.weights_bytes))
            .unwrap_or(0)
    }

    /// A conversation's state, creating it on first use.
    ///
    /// Taken out and handed back rather than borrowed, so a long generation
    /// never holds the map locked.
    pub fn session_for(&self, conversation: &str) -> Conversation {
        self.sessions
            .lock()
            .ok()
            .and_then(|mut m| m.remove(conversation))
            .unwrap_or_default()
    }

    pub fn keep_session(&self, conversation: &str, session: Conversation) {
        if let Ok(mut m) = self.sessions.lock() {
            // A conversation nobody returns to would otherwise keep a
            // delimiter forever. Sixteen is more threads than anyone has open.
            if m.len() >= 16 {
                let oldest = m.keys().next().cloned();
                if let Some(k) = oldest {
                    m.remove(&k);
                }
            }
            m.insert(conversation.to_string(), session);
        }
    }

    /// Forget a conversation's session. Called when the thread is cleared.
    pub fn forget_session(&self, conversation: &str) {
        let _ = self.sessions.lock().map(|mut m| m.remove(conversation));
    }

    /// Track an answer in progress so it can be cancelled by id.
    pub fn register_ask(&self, cancel: Cancel) -> String {
        let id = marrow_core::RequestId::new().to_string();
        if let Ok(mut m) = self.asks.lock() {
            m.insert(id.clone(), cancel);
        }
        id
    }

    pub fn finish_ask(&self, id: &str) {
        let _ = self.asks.lock().map(|mut m| m.remove(id));
    }

    /// UX §10: felt within 500 ms. The worker polls its cancel on a 100 ms
    /// slice, so this lands well inside that.
    pub fn cancel_ask(&self, id: &str) -> bool {
        match self.asks.lock() {
            Ok(m) => m.get(id).map(|c| c.cancel()).is_some(),
            Err(_) => false,
        }
    }

    /// Embed a question, if there is an embedder.
    ///
    /// Returns `None` rather than an error when there is no embedding model,
    /// no runtime, or the model will not load: search has to work without it
    /// (hard rule 10), so its absence is a missing branch and not a failure.
    /// The reason is logged once by `embedder()`.
    pub fn embed_query(&self, question: &str) -> Option<marrow_index::Embedding> {
        let e = self.embedder()?;
        match e.embed_one(question) {
            Ok(v) => Some(v),
            Err(err) => {
                tracing::warn!(error = %err, "could not embed the question");
                None
            }
        }
    }

    /// The resident embedder, loading it on first use.
    ///
    /// Resident, unlike the generator: it runs once per query and once per
    /// chunk, so its residency is earned by call volume. Evicting it between
    /// questions would pay a load for every search, and search is the product.
    pub fn embedder(&self) -> Option<std::sync::Arc<Embedder>> {
        if let Ok(slot) = self.embedder.lock() {
            if let Some(e) = slot.as_ref() {
                return Some(std::sync::Arc::clone(e));
            }
        }
        let started = self.start_embedder();
        match started {
            Ok(e) => {
                if let Ok(mut slot) = self.embedder.lock() {
                    *slot = Some(std::sync::Arc::clone(&e));
                }
                Some(e)
            }
            Err(err) => {
                // Once. A warning per keystroke would be the loudest thing in
                // the log and would say the same thing every time.
                if let Ok(mut said) = self.embedder_problem.lock() {
                    if said.is_none() {
                        tracing::info!(reason = %err.message(), "semantic search is unavailable");
                        *said = Some(err.message().to_string());
                    }
                }
                None
            }
        }
    }

    /// Why semantic search is unavailable, if it is. Shown rather than left to
    /// be inferred from results that are slightly worse than expected.
    pub fn embedder_problem(&self) -> Option<String> {
        self.embedder_problem.lock().ok().and_then(|p| p.clone())
    }

    fn start_embedder(&self) -> marrow_core::Result<std::sync::Arc<Embedder>> {
        let runtime = self.runtime().ok_or_else(|| {
            marrow_core::Error::new(
                marrow_core::Code::CfgInvalid,
                Runtime::setup_hint(&self.data_dir),
            )
        })?;
        let workspace = self.workspace.clone().ok_or_else(|| {
            marrow_core::Error::new(
                marrow_core::Code::CfgInvalid,
                "The model directory is unavailable, so the embedding model cannot be loaded.",
            )
        })?;
        let entry = self
            .registry
            .lock()
            .ok()
            .and_then(|r| r.iter().find(|e| e.capabilities.embedding).cloned())
            .ok_or_else(|| {
                marrow_core::Error::new(
                    marrow_core::Code::ModNotInstalled,
                    "No embedding model is in the catalogue.",
                )
            })?;
        let digest = entry.manifest_digest.as_deref().ok_or_else(|| {
            marrow_core::Error::new(
                marrow_core::Code::ModNotInstalled,
                format!("{} has no local weights.", entry.display_name),
            )
        })?;
        let dir = workspace.weights_dir(digest);
        if !dir.is_dir() {
            return Err(marrow_core::Error::new(
                marrow_core::Code::ModNotInstalled,
                format!(
                    "{} is not downloaded yet, so semantic search is off. \
                     Download it from the Models page — it is about 210 MB.",
                    entry.display_name
                ),
            ));
        }
        Embedder::start(&runtime, &entry.id, &dir).map(std::sync::Arc::new)
    }

    /// Embed every chunk that has no vector yet.
    ///
    /// Runs on its own thread and reports through `backfill`, so a page that
    /// refetches keeps showing the same figures rather than a value that
    /// appears and vanishes. Idempotent and resumable: it re-asks the store
    /// each batch, so an interrupted run loses at most one batch.
    pub fn start_backfill(
        &self,
        core: std::sync::Arc<crate::state::Core>,
    ) -> marrow_core::Result<()> {
        let Some(embedder) = self.embedder() else {
            return Err(marrow_core::Error::new(
                marrow_core::Code::ModNotInstalled,
                self.embedder_problem().unwrap_or_else(|| {
                    "No embedding model is available, so semantic search cannot be \
                     built. Search still works without it."
                        .into()
                }),
            ));
        };
        {
            let mut running = self.backfill_cancel.lock().map_err(|_| poisoned())?;
            if running.is_some() {
                return Err(marrow_core::Error::new(
                    marrow_core::Code::ModQueueFull,
                    "A backfill is already running.",
                ));
            }
            *running = Some(Cancel::new());
        }
        let cancel = self
            .backfill_cancel
            .lock()
            .ok()
            .and_then(|c| c.clone())
            .unwrap_or_default();
        let progress = Arc::clone(&self.backfill);
        let done = Arc::clone(&self.backfill_cancel_slot());

        std::thread::Builder::new()
            .name("marrow-backfill".into())
            .spawn(move || {
                let out = marrow_model::backfill::run(
                    core.store(),
                    core.vectors(),
                    &embedder,
                    &cancel,
                    &progress,
                );
                match out {
                    Ok(o) => tracing::info!(
                        embedded = o.embedded,
                        failed = o.failed,
                        cancelled = o.cancelled,
                        "backfill finished"
                    ),
                    Err(e) => tracing::warn!(error = %e, "backfill failed"),
                }
                if let Ok(mut slot) = done.lock() {
                    *slot = None;
                }
            })
            .map_err(|e| {
                marrow_core::Error::new(
                    marrow_core::Code::IntInvariantViolated,
                    "Could not start the backfill thread.",
                )
                .with_source(e)
            })?;
        Ok(())
    }

    fn backfill_cancel_slot(&self) -> Arc<Mutex<Option<Cancel>>> {
        Arc::clone(&self.backfill_cancel)
    }

    /// Stop a backfill. What is embedded stays embedded.
    pub fn stop_backfill(&self) -> bool {
        match self.backfill_cancel.lock() {
            Ok(c) => c.as_ref().map(|c| c.cancel()).is_some(),
            Err(_) => false,
        }
    }

    /// Release the loaded model. LLM-049: weights, cache and buffers.
    pub fn release_model(&self) {
        if let Ok(mut slot) = self.loaded.lock() {
            *slot = None;
        }
    }

    /// Delete an installed model's weights. Returns the bytes freed.
    ///
    /// **The app could download 3.1 GB and never undo it.** There was no
    /// command, no button and no path back: the only way to remove a model was
    /// to find the directory by hand — on a machine where a full disk had
    /// already stopped SQLite writing once.
    ///
    /// Three things happen before the files go, in this order, because each
    /// one is a way for the delete to leave something worse than it found:
    ///
    /// 1. **Unloaded if it is the model in memory.** Removing the weights under
    ///    a live worker leaves a process holding a deleted file, and the next
    ///    question fails somewhere far from here.
    /// 2. **Unpinned if it is the pinned one.** `local_generator` already
    ///    ignores a pin that is not installed, but leaving the id in
    ///    `preferences.json` means the page keeps naming a model that is gone.
    /// 3. **The registry entry is marked not-installed**, so the page stops
    ///    offering to answer with it before the disk work begins rather than
    ///    after.
    ///
    /// A model that is not installed is not an error. The caller asked for it
    /// to be gone and it is gone; raising would make a second click on a
    /// working button fail.
    pub fn delete_model(&self, model_id: &str) -> marrow_core::Result<u64> {
        let Some(workspace) = self.workspace.as_ref() else {
            return Err(marrow_core::Error::new(
                marrow_core::Code::CfgInvalid,
                self.workspace_problem.clone().unwrap_or_else(|| {
                    "The model directory could not be opened, so nothing can be removed from it."
                        .into()
                }),
            ));
        };

        // The manifest digest names the directory, exactly as the download
        // path composes it — so a model is deleted from the same identity it
        // was installed under, and the id never reaches the filesystem.
        let digest = {
            let registry = self.registry.lock().map_err(|_| poisoned())?;
            let found = registry
                .iter()
                .find(|e| e.id == model_id)
                .and_then(|e| e.manifest_digest.clone());
            drop(registry);
            found
        };
        let Some(digest) = digest else {
            return Err(marrow_core::Error::new(
                marrow_core::Code::ModNotInstalled,
                format!(
                    "`{model_id}` has no pinned manifest, so this build cannot tell which \
                     files on disk are its weights. Nothing was removed."
                ),
            ));
        };

        if self
            .loaded
            .lock()
            .map_err(|_| poisoned())?
            .as_ref()
            .is_some_and(|l| l.model_id == model_id)
        {
            self.release_model();
        }
        if self.pinned_model().as_deref() == Some(model_id) {
            self.set_generator_model(None)?;
        }
        if let Ok(mut registry) = self.registry.lock() {
            if let Some(e) = registry.iter_mut().find(|e| e.id == model_id) {
                e.installed = false;
            }
        }

        let freed = workspace.delete_weights(&digest)?;
        tracing::info!(model = %model_id, freed_bytes = freed, "deleted a model's weights");
        Ok(freed)
    }

    pub fn snapshot(&self) -> ModelsSnapshot {
        let remote = self.provider_status();
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
            pinned_model_id: self.pinned_model(),
            // What would actually answer. A remote provider wins over any
            // local pin, which is the one thing a page showing a pinned local
            // model must not hide.
            //
            // Read from the stored endpoint rather than by calling
            // `generator()`: that constructs an `OpenAiProvider`, which
            // resolves the host to decide its boundary, and this page refetches
            // every four seconds. A label is not worth a DNS lookup on a timer.
            active_model: if remote.configured && remote.enabled {
                Some(format!("{} · {}", remote.label, remote.model))
            } else {
                self.local_generator()
            },
            machine: self.machine.summary(),
            tier_headline: self.machine.tier.headline().to_string(),
            unified_memory: self.machine.unified_memory,
            total_bytes: self.machine.total_memory_bytes,
            available_bytes: available,
            sustained_load: conditions.sustained_load,
            thermal: format!("{:?}", conditions.latest.thermal).to_lowercase(),
            sample_stale: conditions.stale,
            resident_bytes: self.resident_bytes(),
            semantic: self.semantic_status(),
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
            runtime_ready: self.runtime().is_some(),
            runtime_status: match (&self.runtime(), remote.enabled) {
                // "Nothing leaves this device" was a constant, and it stops
                // being true the moment the user turns on an endpoint. The
                // sentence is now assembled from what is actually configured.
                (_, true) => format!(
                    "Answers are being generated by {} at {}, so the question and the \
                     excerpts it uses leave this device. Turn it off in Settings to go \
                     back to answering locally.",
                    remote.label,
                    remote
                        .boundary_label
                        .clone()
                        .unwrap_or_else(|| remote.base_url.clone())
                ),
                (Some(_), false) => RUNTIME_READY.to_string(),
                (None, false) => RUNTIME_MISSING.to_string(),
            },
            runtime_setup: self
                .runtime()
                .is_none()
                .then(|| Runtime::setup_hint(&self.data_dir)),
            runtime_installable: marrow_model::runtime::ARCHIVE.is_pinned(),
            runtime_download_bytes: marrow_model::runtime::ARCHIVE.size,
            runtime_install: self.runtime_install.lock().ok().and_then(|p| p.clone()),
            remote,
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

/// The sentence at the top of the page when a runtime is present.
/// Only reached when **no** remote endpoint is enabled — `snapshot` chooses,
/// and the last clause of this sentence is why that choice exists.
const RUNTIME_READY: &str = "MLX is available on this machine. A model that is \
     installed and fits can answer questions locally — nothing leaves this device.";

/// And when it is not. A page listing six models that cannot run must not read
/// as a page of six models that can, and it must name the fix rather than the
/// problem.
const RUNTIME_MISSING: &str = "No inference runtime is installed, so nothing here \
     can answer a question yet. Models can still be downloaded, and search works \
     without one.";

/// What a conversation accumulates.
///
/// Both fields exist for the same reason: a prompt whose prefix is identical
/// across turns can be reused from the KV cache, and one whose prefix moves
/// cannot. The delimiter is the obvious half. The evidence is the other:
/// retrieval is question-dependent, so a follow-up that simply re-retrieves
/// produces a different evidence set and reuses nothing — measured, zero of
/// 552 tokens.
#[derive(Debug, Default)]
pub struct Conversation {
    pub session: Session,
    /// Chunks already sent, oldest first. New ones are appended; the order
    /// never changes, because reordering is the same as replacing.
    pub sent: Vec<crate::state::RetrievedChunk>,
}

/// How many tokens the answer may use.
///
/// **The budget is a memory question, not a subtraction.** The previous version
/// took `default_context` — a flat 8,192 — and subtracted the prompt from it.
/// That reads as arithmetic about a window, but `default_context` is a
/// *planning* number used to size the memory watchdog; MLX allocates KV lazily
/// as tokens arrive, so nothing about 8,192 is a wall the model hits.
///
/// The subtraction had a reported consequence. A question that retrieved 41
/// sources produced a 29 KB prompt, about 7,424 tokens: `8192 - 7424 = 768`,
/// clamped back up to the 1,024 floor. The user had asked for an HTML page and
/// got roughly 4,000 characters of one. **The clamp is what made it silent** —
/// an overrun became a floor instead of a report, so the answer stopped
/// mid-output with nothing saying why.
///
/// What actually bounds the answer is what the machine can hold: KV grows with
/// every token of prompt *and* answer, at a rate that is a property of the
/// architecture. So this asks how much answer fits in the memory that is free
/// right now, rather than what is left of a constant.
///
/// The prompt is estimated from bytes rather than tokenized, because tokenizing
/// here would mean a round trip to the worker before the worker can be asked to
/// do anything, and the estimate only has to be conservative. Four bytes per
/// token is low for English prose and about right for code and markup, which is
/// what this index mostly holds.
fn answer_budget(
    machine: &Machine,
    entry: &marrow_model::Entry,
    envelope: &Envelope,
    thorough: bool,
    available_bytes: u64,
) -> u32 {
    /// Never below this: an answer that cannot finish a paragraph is not worth
    /// the load, and if the machine cannot afford even this then the request
    /// should have been refused at admission rather than half-answered.
    const FLOOR: u32 = 1_024;
    /// Never above this either. A model that runs away produces minutes of
    /// tokens nobody reads, and the queue has to be able to promise an end.
    const CEILING: u32 = 4_096;

    let prompt = (envelope.text.len() / 4) as u32;
    let thinking = if thorough {
        Reasoning::THOROUGH.thinking_tokens()
    } else {
        0
    };
    let fixed = prompt.saturating_add(thinking);

    // Walk down from the ceiling to the floor, asking each time whether the
    // conversation's **KV** still fits. Halving rather than stepping keeps this
    // to three probes; the estimate is conservative enough that a finer search
    // would be false precision.
    //
    // **Only the KV, because only the KV is still to be paid for.** This runs
    // with the model already loaded, so its weights, its runtime overhead and
    // the embedding model are resident — they are part of what is *used*, not
    // what is free — and `headroom_bytes` is memory deliberately left alone
    // rather than memory being asked for. `Requirement::total()` is all four
    // plus the KV, and that number answers a different question: "can this
    // machine load this model at all". Asking it here made every answer pay for
    // the model a second time, beside itself.
    //
    // The arithmetic is why every footer read the same. A 4B q4 model is about
    // 2.5 GB of weights and unified-memory headroom is another 2.5 GB, so the
    // check wanted roughly 6 GB free before it would consider *any* answer
    // length. A 16 GB laptop with a browser open has less, so the walk failed
    // at 4,096, failed at 2,048, and floored at 1,024 — on every question,
    // reported as `993 tokens ... cut off at the token limit` every time.
    //
    // Half of what is free, not all of it. The KV is what this decision
    // allocates, and planning to consume everything that happens to be free at
    // one instant is how a laptop is pushed into swap by a long answer.
    let mut answer = CEILING;
    while answer > FLOOR {
        let shape = entry.shape(fixed.saturating_add(answer), KvPrecision::F16);
        if Requirement::estimate(machine, &shape).kv_cache_bytes <= available_bytes / 2 {
            break;
        }
        answer /= 2;
    }
    let answer = answer.max(FLOOR);

    if answer < CEILING {
        // Said out loud, because a shortened answer that stops mid-sentence is
        // exactly what the user reported twice and could not explain.
        tracing::info!(
            prompt_tokens = prompt,
            thinking_tokens = thinking,
            answer_tokens = answer,
            available_mb = available_bytes / 1_048_576,
            "the answer budget was reduced to fit the memory that is free"
        );
    }
    answer
}

/// A model held in a worker process.
#[derive(Debug)]
struct Loaded {
    model_id: String,
    provider: MlxProvider,
}

/// Where the worker script lives beside the binary.
///
/// Shipped next to the executable rather than embedded, so a broken worker can
/// be read and fixed without a rebuild — this is a personal tool, and that
/// trade is the right way round.
///
/// **Three places, because a `.app` is not a directory of loose files.** This
/// used to check next to the executable and then fall back to a path baked in
/// at compile time by `CARGO_MANIFEST_DIR`. In a bundle neither is right: Tauri
/// puts resources in `Contents/Resources` while the binary sits in
/// `Contents/MacOS`, so the first check missed and the fallback resolved to the
/// *build machine's* source tree — on a release build, a directory on a GitHub
/// runner that has never existed on any user's disk.
///
/// The consequence was total and silent. `Runtime::discover` only checked for a
/// Python interpreter, so it reported a healthy runtime, the worker was started
/// with a script that was not there, and it died immediately — surfacing as
/// "the model runtime stopped", which describes a process that never began. No
/// released build could answer a question on any machine.
///
/// The compile-time fallback is kept for `cargo run` from a checkout and only
/// there: shipping a binary that reaches for an absolute path on the machine
/// that built it is how this happened in the first place.
/// A settled install row with no measurements in it.
///
/// Only ever used to carry a terminal stage when the install ended before any
/// progress landed — a refusal on the first line, say. The numbers are zero
/// because they are unknown, not because nothing was transferred.
fn ready_install() -> marrow_model::runtime::Install {
    marrow_model::runtime::Install {
        stage: marrow_model::runtime::Stage::Ready,
        bytes_done: 0,
        bytes_total: marrow_model::runtime::ARCHIVE.size,
        bytes_per_sec: 0,
        eta_secs: None,
    }
}

fn worker_script() -> PathBuf {
    let exe = std::env::current_exe().ok();
    let beside = exe
        .as_ref()
        .and_then(|p| p.parent())
        .map(|d| d.join("mlx_worker.py"));
    // `Contents/MacOS/marrow-desktop` -> `Contents/Resources/mlx_worker.py`.
    let bundled = exe
        .as_ref()
        .and_then(|p| p.parent())
        .and_then(|d| d.parent())
        .map(|c| c.join("Resources").join("mlx_worker.py"));

    beside
        .into_iter()
        .chain(bundled)
        .find(|p| p.is_file())
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../model/worker/mlx_worker.py")
        })
}

fn poisoned() -> marrow_core::Error {
    marrow_core::Error::invariant("the model registry lock was poisoned")
}

/// Which installed model answers a question, given a pin and a profile.
///
/// **The bug this exists to fix: the AI preference persisted and changed
/// nothing.** `set_ai_profile` wrote `preferences.json`, the choice survived a
/// restart, the radio list showed it selected — and the only readers of
/// `choose(profile, …)` were the caption on that same radio list and a display
/// table. Picking Efficient on an 8 GB machine still loaded the 4B. A control
/// that persists a choice and has no effect is worse than one that is not
/// offered, because the user has no way to find out.
///
/// Three rules, in order, and the order is the whole design:
///
/// 1. **An explicit pin wins over a preference.** A profile is a standing
///    preference about memory and battery; a pin is "use this model". Letting a
///    profile veto a pin would make the Models page's own model list a
///    suggestion. Not filtered by `offerable` either — that is a memory reading
///    taken at this instant, and letting a browser being open silently swap the
///    user's model out is the quiet substitution the pin exists to refuse.
///    Admission and the memory budget still apply downstream; what this decides
///    is *which* model, not whether there is room.
/// 2. **The profile is a ceiling, not a target.** `choose(profile,
///    Generation).params_b` is the parameter class the user asked to stay
///    within — 2B, 4B, 8B — so the answer is the largest installed model at or
///    under it. Largest, because within a budget a bigger model is better at
///    the one job this is for.
/// 3. **The ceiling never makes the app stop answering.** If nothing installed
///    fits under it, the smallest model that does not is used and the
///    substitution is logged. A user who installed only a 4B and then chose
///    Efficient asked for less memory, not for questions to start failing —
///    and `no_generator_message` would have told them to download a model they
///    already have.
///
/// Profile::Cloud is the exception to rule 2: its budget is 0.0 B because
/// nothing loads locally, which is a statement about a remote endpoint and not
/// a local ceiling. Reaching here at all means no remote provider is enabled,
/// so it falls through to the unconstrained choice rather than to rule 3's
/// fallback, which would otherwise hand every Cloud user the smallest model on
/// the machine.
fn pick_generator<'a>(
    entries: impl Iterator<Item = &'a marrow_model::registry::Entry>,
    machine: &Machine,
    available: u64,
    profile: Profile,
    pinned: Option<&str>,
) -> Option<String> {
    // Embedders are excluded — they do not answer — and so is anything not
    // installed. Collected once because the pin is checked against the same
    // set the automatic choice draws from.
    let installed: Vec<&marrow_model::registry::Entry> = entries
        .filter(|e| e.installed && !e.capabilities.embedding)
        .collect();

    // Rule 1. An id that is no longer installed is ignored rather than
    // refused: models are deleted from the Models page, and a stale preference
    // must not stop questions being answered.
    if let Some(pinned) = pinned {
        if installed.iter().any(|e| e.id == pinned) {
            return Some(pinned.to_string());
        }
        tracing::info!(
            model = %pinned,
            "the pinned model is not installed; choosing automatically"
        );
    }

    let mut candidates: Vec<(f64, &str)> = installed
        .iter()
        .filter(|e| {
            assess(
                machine,
                &e.shape(e.default_context, KvPrecision::F16),
                available,
            )
            .offerable()
        })
        .map(|e| (e.params_b, e.id.as_str()))
        .collect();
    // Largest first, and ties broken by id so the same registry never produces
    // two different answers on two runs — a generator that changes under you
    // between questions is indistinguishable from a bug.
    candidates.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.cmp(b.1))
    });

    let ceiling = choose(profile, Workload::Generation).params_b;
    if ceiling <= 0.0 {
        return candidates.first().map(|(_, id)| id.to_string());
    }

    // Rule 2, then rule 3.
    if let Some((_, id)) = candidates.iter().find(|(p, _)| *p <= ceiling) {
        return Some(id.to_string());
    }
    let smallest = candidates.last().copied();
    if let Some((params_b, id)) = smallest {
        tracing::info!(
            model = %id,
            params_b,
            ceiling,
            profile = ?profile,
            "nothing installed fits under the profile's budget; using the smallest that is"
        );
    }
    smallest.map(|(_, id)| id.to_string())
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

    fn envelope_of(bytes: usize) -> Envelope {
        marrow_model::envelope::Builder::new("sys", "x".repeat(bytes))
            .finish(&mut marrow_model::envelope::RandomNonce)
    }

    /// A machine with plenty free, so the budget is not memory-limited.
    fn roomy() -> (Machine, u64) {
        let m = Probe::run();
        let free = m.total_memory_bytes;
        (m, free)
    }

    fn qwen() -> marrow_model::Entry {
        marrow_model::catalogue::builtin()
            .into_iter()
            .find(|e| e.id == "qwen3.5-4b-mlx-q4")
            .expect("the primary model is in the catalogue")
    }

    #[test]
    fn the_answer_budget_is_what_the_window_has_left_not_a_flat_number() {
        // The reported bug: a flat 1,024 cut ordinary answers off mid-sentence.
        // Measured at 859 tokens for a one-document summary, so the ceiling was
        // one long answer away at all times.
        let (m, free) = roomy();
        let small = answer_budget(&m, &qwen(), &envelope_of(400), false, free);
        assert!(
            small > 1_024,
            "a short prompt should leave room, got {small}"
        );
        assert!(small <= 4_096, "and still be bounded, got {small}");
    }

    /// **The reported truncation, as arithmetic.**
    ///
    /// 41 retrieved sources came to 29 KB of prompt. Subtracting that from a
    /// flat 8,192 left 768, which the floor raised back to 1,024 — so a request
    /// to generate an HTML page was answered with about 4,000 characters and
    /// stopped mid-output. A prompt that large is ordinary on a real index, and
    /// it must not be the thing that decides how long the answer may be.
    #[test]
    fn a_large_prompt_does_not_starve_the_answer_when_the_memory_is_there() {
        let (m, free) = roomy();
        // 29 KB, the size the reported case actually sent.
        let budget = answer_budget(&m, &qwen(), &envelope_of(29 * 1024), false, free);
        assert!(
            budget > 1_024,
            "a 29 KB prompt collapsed the answer to the floor again, got {budget}"
        );
    }

    #[test]
    fn a_resident_model_is_not_charged_for_its_own_weights_again() {
        // **The reported case, and it is every run on a real desktop.** By the
        // time this is called the model is loaded: its weights, its runtime
        // overhead and the embedding model are resident, and the headroom
        // reserve is the memory deliberately *not* being used. All four are in
        // `Requirement::total()`, and comparing that against what is free
        // demands the machine fit the model a second time beside itself.
        //
        // A 4B q4 model is about 2.5 GB of weights and the unified-memory
        // headroom is another 2.5 GB, so the check needed roughly 6 GB free
        // before it would even consider the answer. A 16 GB laptop with a
        // browser open does not have that, so the walk failed at 4,096, failed
        // at 2,048 and floored at 1,024 — on every question, which is exactly
        // what the footers said: `993 tokens ... cut off at the token limit`,
        // over and over.
        //
        // Four gigabytes free is an ordinary desktop, not a starved one.
        let (m, _) = roomy();
        let free = 4 * 1_024 * 1_024 * 1_024;
        let budget = answer_budget(&m, &qwen(), &envelope_of(29 * 1024), false, free);
        assert!(
            budget > 1_024,
            "an ordinary desktop was given the floor again, got {budget}"
        );
    }

    #[test]
    fn a_machine_with_no_memory_free_still_gets_a_usable_floor() {
        // At that point the request should have been refused at admission. A
        // half-answer is the one outcome that is worse than a refusal, because
        // it looks like an answer.
        let (m, _) = roomy();
        let starved = answer_budget(&m, &qwen(), &envelope_of(200_000), false, 0);
        assert_eq!(starved, 1_024, "the floor holds");
    }

    #[test]
    fn thorough_takes_its_thinking_out_of_the_same_budget() {
        // Otherwise the two sum past what the machine can hold and the model is
        // cut off by the runtime instead of by us — which reports as a crash.
        let (m, _) = roomy();
        let e = envelope_of(4_000);
        // Deliberately tight, so the thinking allowance is what decides.
        let tight = Requirement::estimate(&m, &qwen().shape(6_000, KvPrecision::F16)).total();
        assert!(
            answer_budget(&m, &qwen(), &e, true, tight)
                <= answer_budget(&m, &qwen(), &e, false, tight),
            "thinking must come out of the same budget"
        );
    }

    #[test]
    fn the_page_states_the_runtime_situation_either_way() {
        // A page listing six models that cannot run must not read as a page of
        // six models that can — and when there is no runtime it must name the
        // fix rather than the problem.
        assert!(RUNTIME_MISSING.contains("No inference runtime"));
        assert!(
            RUNTIME_MISSING.contains("search works"),
            "search is unaffected"
        );
        assert!(RUNTIME_READY.contains("nothing leaves this device"));
        for s in [RUNTIME_MISSING, RUNTIME_READY] {
            assert!(
                !s.contains("coming soon"),
                "say what is true now, not what is planned"
            );
        }
    }

    #[test]
    fn a_missing_runtime_comes_with_the_commands_that_create_one() {
        let t = tempfile::tempdir().unwrap();
        // A data directory with no `runtime/mlx` in it.
        let hub = Hub::start(t.path().join("models"), &[]);
        let s = hub.snapshot();
        assert!(!s.runtime_ready);
        let setup = s.runtime_setup.expect("must name the fix");
        assert!(setup.contains("mlx-lm"), "{setup}");
        assert!(setup.contains("venv"), "{setup}");
        hub.shutdown();
    }

    /// **The state every released build up to v0.0.4 was in on every machine
    /// but the one that built it**, asserted rather than described.
    ///
    /// A fresh data directory has no `runtime/mlx`, because nothing in the
    /// bundle ever put one there. What the page offers in that state was a
    /// paragraph of commands whose first line macOS cannot run. It must now be
    /// something clickable, with its cost stated before the click.
    #[test]
    fn a_machine_with_no_runtime_is_offered_one_it_can_actually_install() {
        let t = tempfile::tempdir().unwrap();
        let hub = Hub::start(t.path().join("models"), &[]);
        let s = hub.snapshot();

        assert!(!s.runtime_ready, "a fresh data directory has no runtime");
        assert!(
            s.runtime_installable,
            "a build that cannot install one leaves the user where v0.0.4 did"
        );
        assert!(
            s.runtime_download_bytes > 0,
            "the offer states its cost before it is accepted"
        );
        assert!(
            s.runtime_install.is_none(),
            "nothing is in flight until the user asks"
        );
        hub.shutdown();
    }

    /// An install is never started over a runtime that already works.
    ///
    /// The author's hand-made venv predates all of this and is the only
    /// runtime on the machine that found the bug. An installer that replaced
    /// it would be a worse bug than the one it fixes.
    #[test]
    fn a_machine_that_already_has_a_runtime_is_left_alone() {
        let t = tempfile::tempdir().unwrap();
        let bin = t.path().join("runtime/mlx/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("python"), "#!/bin/sh\n").unwrap();

        let hub = Hub::start(t.path().join("models"), &[]);
        let before = hub.snapshot();
        hub.install_runtime().expect("a no-op, not an error");
        let after = hub.snapshot();

        assert_eq!(before.runtime_ready, after.runtime_ready);
        assert!(
            after.runtime_install.is_none(),
            "nothing was started: {:?}",
            after.runtime_install
        );
        assert!(bin.join("python").exists(), "and nothing was removed");
        hub.shutdown();
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
        // SUP-011 and the `origin = SELF` rule. The page says so; it does not quietly pick
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

    /// A library of installed models, sized so every rule has something to
    /// choose between: a 0.6B router, a 3B, a 4B, an 8B and an embedder.
    ///
    /// The 8B is a clone of the 4B with its parameter count moved, because the
    /// catalogue tops out at 4B and `LargerLocal` has to have something to
    /// select or the test proves nothing.
    fn library() -> Vec<marrow_model::Entry> {
        let mut entries: Vec<marrow_model::Entry> = marrow_model::catalogue::builtin()
            .into_iter()
            .filter(|e| {
                [
                    "qwen3-0.6b-mlx-q4",
                    "granite-4.1-3b-mlx-q4",
                    "embeddinggemma-300m-mlx-q4",
                ]
                .contains(&e.id.as_str())
                    || e.id == "qwen3.5-4b-mlx-q4"
            })
            .collect();
        let mut eight = qwen();
        eight.id = "pretend-8b-mlx-q4".into();
        eight.params_b = 8.0;
        entries.push(eight);
        for e in &mut entries {
            e.installed = true;
        }
        entries
    }

    /// A machine large enough that nothing is refused for memory, so what the
    /// tests below observe is the profile and not the sampler.
    fn big_machine() -> (Machine, u64) {
        let m = Machine {
            total_memory_bytes: 64 * 1_073_741_824,
            unified_memory: true,
            ..Machine::unknown()
        };
        let free = 48 * 1_073_741_824;
        (m, free)
    }

    #[test]
    fn the_ai_profile_chooses_the_generator_rather_than_only_captioning_itself() {
        // **The audit's finding.** `set_ai_profile` persisted a choice across
        // restarts and `local_generator` never read it, so all four radio
        // buttons produced the same model. The same registry and the same
        // machine must now give three different answers.
        let (m, free) = big_machine();
        let lib = library();
        let at = |p| pick_generator(lib.iter(), &m, free, p, None);
        assert_eq!(at(Profile::Efficient).as_deref(), Some("qwen3-0.6b-mlx-q4"));
        assert_eq!(at(Profile::Balanced).as_deref(), Some("qwen3.5-4b-mlx-q4"));
        assert_eq!(
            at(Profile::LargerLocal).as_deref(),
            Some("pretend-8b-mlx-q4")
        );
        // And never the embedder, at any profile: it does not answer.
        for p in [Profile::Efficient, Profile::Balanced, Profile::LargerLocal] {
            assert_ne!(at(p).as_deref(), Some("embeddinggemma-300m-mlx-q4"));
        }
    }

    #[test]
    fn an_explicit_pin_beats_the_profile() {
        // A profile is a standing preference; a pin is "use this model". If the
        // preference could veto the pin, the model list on the Models page
        // would be a suggestion — which is the same defect as the profile
        // doing nothing, pointed the other way.
        let (m, free) = big_machine();
        let lib = library();
        let picked = pick_generator(
            lib.iter(),
            &m,
            free,
            Profile::Efficient,
            Some("pretend-8b-mlx-q4"),
        );
        assert_eq!(picked.as_deref(), Some("pretend-8b-mlx-q4"));
    }

    #[test]
    fn a_pin_that_is_no_longer_installed_falls_back_to_the_profile() {
        // Models are deleted from the Models page. A stale preference must not
        // stop questions being answered.
        let (m, free) = big_machine();
        let lib = library();
        let picked = pick_generator(
            lib.iter(),
            &m,
            free,
            Profile::Balanced,
            Some("deleted-last-week"),
        );
        assert_eq!(picked.as_deref(), Some("qwen3.5-4b-mlx-q4"));
    }

    #[test]
    fn the_profile_ceiling_never_leaves_a_question_unanswerable() {
        // Efficient with only a 4B installed asked for less memory, not for
        // answering to stop. Returning `None` here would raise
        // MOD_NOT_INSTALLED and tell the user to download a model they have.
        let (m, free) = big_machine();
        let only_a_4b: Vec<marrow_model::Entry> = library()
            .into_iter()
            .filter(|e| e.id == "qwen3.5-4b-mlx-q4")
            .collect();
        let picked = pick_generator(only_a_4b.iter(), &m, free, Profile::Efficient, None);
        assert_eq!(picked.as_deref(), Some("qwen3.5-4b-mlx-q4"));
    }

    #[test]
    fn the_cloud_profile_still_answers_locally_when_no_endpoint_is_configured() {
        // Cloud's budget is 0.0 B because nothing loads locally — a statement
        // about a remote endpoint, not a local ceiling. Reading it as one would
        // hand every Cloud user the smallest model on the machine, which is a
        // silent downgrade rather than the frontier model they chose.
        let (m, free) = big_machine();
        let lib = library();
        let picked = pick_generator(lib.iter(), &m, free, Profile::Cloud, None);
        assert_eq!(picked.as_deref(), Some("pretend-8b-mlx-q4"));
    }

    #[test]
    fn nothing_installed_means_nothing_to_answer_with_at_every_profile() {
        // The one case that must still be `None`: the caller turns it into
        // MOD_NOT_INSTALLED with the download instructions.
        let (m, free) = big_machine();
        let none: Vec<marrow_model::Entry> = Vec::new();
        for p in [
            Profile::Efficient,
            Profile::Balanced,
            Profile::LargerLocal,
            Profile::Cloud,
        ] {
            assert_eq!(pick_generator(none.iter(), &m, free, p, None), None);
        }
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
