//! Will this model fit, and what do we tell the user if not.
//!
//! Pure arithmetic over a [`Machine`](crate::Machine) and a model's shape. No
//! I/O, so every verdict is testable without a machine that has 39 GB of VRAM.
//!
//! The rule that shapes the whole module: **a model that will not fit is
//! listed, disabled, with the number** (LLM-016). Hiding it produces "why isn't
//! Llama 70B here?" and no way to answer.
//!
//! # The budget, measured
//!
//! The planning estimate for this was 3–4 GB. Pinning the real files
//! (`mlx-community/Qwen3.5-4B-MLX-4bit`, `embeddinggemma-300m-4bit`) showed
//! that under-predicted, and under-predicting is the direction that OOMs:
//!
//! ```text
//!                         estimated      measured
//! 4B Q4 weights           2.5–3.0 GB     3.06 GB
//! KV cache @ 8k f16       200–700 MB     1.07 GB      ← 1.8x the estimate
//! MLX runtime / buffers   200–500 MB     ~350 MB
//! embedding model         100–300 MB     0.21 GB
//! ───────────────────────────────────────────────
//! typical AI footprint    3–4 GB         4.70 GB @ 8k · 4.16 GB @ 4k
//! ```
//!
//! Every term is a field on [`Requirement`]. The two usually left out —
//! runtime buffers and the resident embedding model — are why a model that
//! "fits" by weight arithmetic swaps in practice. The KV cache is why one that
//! fits at 4k does not at 8k.

use serde::{Deserialize, Serialize};

use crate::probe::Machine;

/// Bytes per parameter at each quantization, relative to FP16.
///
/// The rule of thumb is ~2 GB per billion parameters at FP16; quantization
/// scales that. These are approximations and are documented as such, because a
/// GGUF's real size depends on which tensors were quantized and how.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Quantization {
    F16,
    Q8,
    Q5,
    Q4,
}

impl Quantization {
    pub fn factor(self) -> f64 {
        match self {
            Quantization::F16 => 1.0,
            // Each of these is above the clean bits-per-weight ratio, on
            // purpose. A "4-bit" checkpoint is not four bits per weight: it
            // carries a scale and a zero-point per group (≈0.5 bit at group
            // size 64) and keeps the embedding and output tensors at higher
            // precision. The clean 0.25 under-predicts a real 4B Q4 by roughly
            // 400 MB, and under-predicting is the direction that OOMs.
            Quantization::Q8 => 0.53,
            Quantization::Q5 => 0.40,
            Quantization::Q4 => 0.33,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Quantization::F16 => "f16",
            Quantization::Q8 => "Q8",
            Quantization::Q5 => "Q5",
            Quantization::Q4 => "Q4",
        }
    }
}

/// Precision of the KV cache itself (LLM-046).
///
/// Quantizing it roughly halves the cache, and it is a quality trade — so it is
/// off by default and labelled, never silent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KvPrecision {
    #[default]
    F16,
    Q8,
}

impl KvPrecision {
    pub fn factor(self) -> f64 {
        match self {
            KvPrecision::F16 => 1.0,
            KvPrecision::Q8 => 0.5,
        }
    }
}

/// Which runtime will host the model. It is not free, and pretending it is
/// costs ~200–500 MB of the budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeKind {
    /// Apple MLX (Part 8 §139.1.1). Unified memory, no host↔device copy.
    #[default]
    Mlx,
    /// llama.cpp or the embedded Rust runtime.
    Gguf,
    /// An already-running Ollama / LM Studio server. Its buffers are in *its*
    /// address space, so they are not ours to count — but its weights still
    /// occupy the same physical memory, which is why this is not zero.
    External,
}

impl RuntimeKind {
    /// Working buffers, scratch arenas and the graph, excluding weights.
    fn overhead_bytes(self) -> u64 {
        match self {
            RuntimeKind::Mlx => 350_000_000,
            RuntimeKind::Gguf => 300_000_000,
            RuntimeKind::External => 0,
        }
    }
}

/// How much memory the model will actually occupy on this machine.
///
/// `elem_bytes` for the cache is folded into `kv_cache_bytes`, so the field is
/// the real figure rather than a base to be scaled later.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Requirement {
    pub weights_bytes: u64,
    /// Counted, not ignored. The KV cache is the usual reason a model loads and
    /// then dies partway through a long context.
    pub kv_cache_bytes: u64,
    /// Runtime buffers. Not weights, not cache, and not zero.
    pub runtime_bytes: u64,
    /// The embedding model, which is resident whenever search is being used —
    /// which is always, because that is the product.
    pub embedding_bytes: u64,
    /// Discrete GPUs keep 15% free; unified memory keeps an OS reserve.
    pub headroom_bytes: u64,
}

/// The shape of a model, as far as memory is concerned.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelShape {
    pub params_b: f64,
    pub quantization: Quantization,
    /// The context it will be **run** at, not its maximum. Sizing against a
    /// 128k maximum rejects models that work perfectly well at 8k.
    pub context: u32,
    /// The real on-disk size, when the registry has a pinned manifest. Beats
    /// `params_b × quantization` every time: a "4-bit" checkpoint's real size
    /// depends on which tensors were quantized and at what group size, and the
    /// manifest knows and the formula does not.
    pub weights_bytes: Option<u64>,
    /// Measured KV bytes per token at FP16, read from the model's own config
    /// (`2 × layers × kv_heads × head_dim × 2`). `None` falls back to the
    /// constant below.
    pub kv_bytes_per_token: Option<u32>,
    pub kv_precision: KvPrecision,
    pub runtime: RuntimeKind,
    /// Whether an embedding model is resident alongside. It is, in Marrow.
    pub embedding_resident: bool,
}

impl ModelShape {
    pub fn new(params_b: f64, quantization: Quantization, context: u32) -> Self {
        Self {
            params_b,
            quantization,
            context,
            weights_bytes: None,
            kv_bytes_per_token: None,
            kv_precision: KvPrecision::F16,
            runtime: RuntimeKind::Mlx,
            embedding_resident: true,
        }
    }

    /// FP16 KV bytes per token when nothing has measured it.
    ///
    /// **A constant, not a curve, and that is the finding.** KV per token is
    /// `2 × layers × kv_heads × head_dim × 2` — an architectural property, not
    /// a function of parameter count. The pinned catalogue makes that plain:
    ///
    /// ```text
    /// Qwen3-0.6B        112 KB/token      ← smaller model, more cache
    /// granite-4.1-3b     80 KB/token
    /// Qwen3.5-4B        128 KB/token
    /// gemma-3-4b        136 KB/token
    /// Nemotron-3-4B     168 KB/token      ← the worst case
    /// ```
    ///
    /// A 0.6B model needing more cache per token than a 3B one is not noise;
    /// it is GQA head counts and head dimensions varying independently of
    /// size. An earlier `√params` fit predicted 50 KB for that 0.6B model —
    /// less than half the truth, in the direction that OOMs mid-generation.
    ///
    /// So the fallback is the worst case observed, rounded down slightly. It
    /// over-estimates most models, which costs a refusal that an override can
    /// clear; under-estimating costs a crash that nothing can.
    fn kv_per_token_f16(&self) -> f64 {
        160_000.0
    }
}

/// The typical resident cost of the embedding model (a small ~100–300 MB
/// bi-encoder). Not scaled by the generation model: it is its own thing.
const EMBEDDING_BYTES: u64 = 200_000_000;

impl Requirement {
    /// What must be free for this to run.
    pub fn total(&self) -> u64 {
        self.ai_footprint() + self.headroom_bytes
    }

    /// What Marrow itself will occupy — the "typical AI footprint" line.
    /// Excludes the OS reserve, which is not ours.
    pub fn ai_footprint(&self) -> u64 {
        self.weights_bytes + self.kv_cache_bytes + self.runtime_bytes + self.embedding_bytes
    }

    /// What is released when the model is unloaded after an idle timeout
    /// (LLM-025). The embedding model stays: search still works.
    pub fn releasable_on_unload(&self) -> u64 {
        self.weights_bytes + self.kv_cache_bytes + self.runtime_bytes
    }

    pub fn estimate(machine: &Machine, shape: &ModelShape) -> Self {
        const BYTES_PER_B_F16: f64 = 2.0 * 1_000_000_000.0;
        let weights = shape.weights_bytes.unwrap_or_else(|| {
            (shape.params_b * BYTES_PER_B_F16 * shape.quantization.factor()) as u64
        });

        let per_token = shape
            .kv_bytes_per_token
            .map(f64::from)
            .unwrap_or_else(|| shape.kv_per_token_f16());
        let kv = (per_token * f64::from(shape.context) * shape.kv_precision.factor()) as u64;

        let headroom = if machine.unified_memory {
            // Unified memory is shared with the OS and every other app; a
            // fixed reserve is more honest than a percentage of a number that
            // is not really ours.
            2_500_000_000
        } else {
            (machine.total_memory_bytes as f64 * 0.15) as u64
        };

        Self {
            weights_bytes: weights,
            kv_cache_bytes: kv,
            runtime_bytes: shape.runtime.overhead_bytes(),
            embedding_bytes: if shape.embedding_resident {
                EMBEDDING_BYTES
            } else {
                0
            },
            headroom_bytes: headroom,
        }
    }

    /// The KV-cache byte cap for prefix reuse (LLM-044): a fraction of the
    /// model's own footprint, not a fixed number. A 1 GB cache beside a 4 GB
    /// model is a different decision from one beside a 40 GB model.
    pub fn cache_budget(&self) -> u64 {
        self.weights_bytes / 4
    }
}

/// Whether a model fits, and how comfortably.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    /// Fits in what is free right now.
    Comfortable,
    /// Fits the machine, but not with what is currently free. Offerable with a
    /// warning — closing other apps would make it work.
    Tight,
    /// Does not fit this machine at all.
    TooLarge,
}

/// A fit decision, with the numbers that produced it.
///
/// Every field here exists so the UI can say "needs 9.1 GB, this machine has
/// 5.4 GB free" rather than "insufficient resources" (LLM-017).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Verdict {
    pub fit: Fit,
    pub requirement: Requirement,
    pub available_bytes: u64,
    pub total_bytes: u64,
    /// One sentence, already phrased for a human.
    pub reason: String,
    /// The breakdown, so "why does a 3 GB model need 6 GB free" is answerable
    /// without reading this source file.
    pub breakdown: String,
}

impl Verdict {
    pub fn offerable(&self) -> bool {
        !matches!(self.fit, Fit::TooLarge)
    }
}

/// Decide whether a model fits, against **live** availability.
///
/// `available` comes from the sampler, not the probe: a recommendation made at
/// launch is wrong by the time the user acts on it (LLM-019).
pub fn assess(machine: &Machine, shape: &ModelShape, available_bytes: u64) -> Verdict {
    let requirement = Requirement::estimate(machine, shape);
    let need = requirement.total();
    let total = machine.total_memory_bytes;

    let (fit, reason) = if need <= available_bytes {
        (
            Fit::Comfortable,
            format!(
                "Needs about {}, and {} is free.",
                gb(need),
                gb(available_bytes)
            ),
        )
    } else if need <= total {
        (
            Fit::Tight,
            format!(
                "Needs about {}, but only {} is free of {}. Closing other \
                 applications would make room.",
                gb(need),
                gb(available_bytes),
                gb(total)
            ),
        )
    } else {
        (
            Fit::TooLarge,
            format!(
                "Needs about {}, and this machine has {} in total. It cannot run here.",
                gb(need),
                gb(total)
            ),
        )
    };

    let breakdown = format!(
        "weights {} · KV cache {} · runtime {} · embedding model {} · OS reserve {}",
        gb(requirement.weights_bytes),
        gb(requirement.kv_cache_bytes),
        gb(requirement.runtime_bytes),
        gb(requirement.embedding_bytes),
        gb(requirement.headroom_bytes),
    );

    Verdict {
        fit,
        requirement,
        available_bytes,
        total_bytes: total,
        reason,
        breakdown,
    }
}

/// Of two models that fit the same budget, which to prefer.
///
/// Part 5 §96.2: **a larger model at Q4 generally beats a smaller one at Q8**
/// for the same memory. Encoded rather than left to whoever writes the list.
pub fn prefer(a: (f64, Quantization), b: (f64, Quantization)) -> std::cmp::Ordering {
    a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)
}

fn gb(bytes: u64) -> String {
    let g = bytes as f64 / 1_000_000_000.0;
    if g < 1.0 {
        format!("{:.0} MB", bytes as f64 / 1_000_000.0)
    } else {
        format!("{g:.1} GB")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::Machine;

    /// The machine this was developed on: 16 GB unified, Apple Silicon.
    fn m4_air() -> Machine {
        Machine {
            total_memory_bytes: 17_179_869_184,
            cpu_cores: 10,
            unified_memory: true,
            ..Machine::unknown()
        }
    }

    fn workstation() -> Machine {
        Machine {
            total_memory_bytes: 128_000_000_000,
            cpu_cores: 32,
            unified_memory: false,
            ..Machine::unknown()
        }
    }

    fn shape(params: f64, q: Quantization, ctx: u32) -> ModelShape {
        ModelShape::new(params, q, ctx)
    }

    /// The real `mlx-community/Qwen3.5-4B-MLX-4bit`, as pinned in the
    /// catalogue: 3.06 GB of weights, 32 layers x 4 KV heads x 256 head dim.
    fn qwen35_4b() -> ModelShape {
        ModelShape {
            weights_bytes: Some(3_063_000_000),
            kv_bytes_per_token: Some(131_072),
            ..ModelShape::new(4.0, Quantization::Q4, 8192)
        }
    }

    /// The real `mlx-community/embeddinggemma-300m-4bit`.
    const EMBEDDER_BYTES: u64 = 210_000_000;

    #[test]
    fn the_measured_four_b_footprint_is_what_the_page_reports() {
        // The planning estimate was 3-4 GB. The real files say otherwise, and
        // the numbers here are the real ones — a test pinning a figure the
        // pinned manifests contradict is worse than no test.
        //
        //                    estimated      measured
        //   weights          2.5-3.0 GB     3.06 GB
        //   KV @ 8k f16      200-700 MB     1.07 GB
        //   runtime          200-500 MB     ~350 MB
        //   embedder         100-300 MB     0.21 GB
        //   total            3-4 GB         4.70 GB
        let r = Requirement::estimate(&m4_air(), &qwen35_4b());
        let gb = |b: u64| b as f64 / 1e9;

        assert!(
            (3.0..=3.2).contains(&gb(r.weights_bytes)),
            "{:.2} GB",
            gb(r.weights_bytes)
        );
        assert!(
            (1.0..=1.1).contains(&gb(r.kv_cache_bytes)),
            "{:.2} GB",
            gb(r.kv_cache_bytes)
        );
        assert!(
            (0.2..=0.5).contains(&gb(r.runtime_bytes)),
            "{:.2} GB",
            gb(r.runtime_bytes)
        );
        assert!(
            (4.5..=4.9).contains(&gb(r.ai_footprint())),
            "measured footprint is 4.70 GB at 8k; got {:.2} GB",
            gb(r.ai_footprint())
        );
    }

    #[test]
    fn halving_the_context_saves_half_a_gigabyte() {
        // The single most effective lever on this machine, and the reason
        // `context` is "what it will be run at", not the model's maximum.
        let at_8k = Requirement::estimate(&m4_air(), &qwen35_4b());
        let at_4k = Requirement::estimate(
            &m4_air(),
            &ModelShape {
                context: 4096,
                ..qwen35_4b()
            },
        );
        let saved = (at_8k.ai_footprint() - at_4k.ai_footprint()) as f64 / 1e9;
        assert!(
            (0.5..=0.6).contains(&saved),
            "expected ~0.54 GB saved, got {saved:.2}"
        );
    }

    #[test]
    fn sizing_a_model_at_its_maximum_context_would_reject_every_model_that_works() {
        // Qwen3.5-4B advertises 262144. At 128 KB/token that is 34 GB of
        // cache — twice this machine. The distinction between the ceiling and
        // the run context is not a nicety.
        let at_max = Requirement::estimate(
            &m4_air(),
            &ModelShape {
                context: 262_144,
                ..qwen35_4b()
            },
        );
        assert!(at_max.total() > m4_air().total_memory_bytes * 2);
        assert_eq!(
            assess(&m4_air(), &qwen35_4b(), 9_000_000_000).fit,
            Fit::Comfortable,
            "the same model at its run context fits"
        );
    }

    #[test]
    fn a_measured_weight_size_beats_the_quantization_formula() {
        // A "4-bit" checkpoint's real size depends on group size and on which
        // tensors stayed at higher precision. The manifest knows; the formula
        // guesses.
        let formula = Requirement::estimate(&m4_air(), &shape(4.0, Quantization::Q4, 8192));
        let measured = Requirement::estimate(&m4_air(), &qwen35_4b());
        assert_ne!(formula.weights_bytes, measured.weights_bytes);
        assert_eq!(measured.weights_bytes, 3_063_000_000);
        assert!(
            formula.weights_bytes < measured.weights_bytes,
            "the formula under-predicted this one; that is why the manifest wins"
        );
    }

    #[test]
    fn the_embedder_stays_when_the_generator_goes() {
        let r = Requirement::estimate(&m4_air(), &qwen35_4b());
        assert!(r.embedding_bytes > 0);
        assert!(
            r.embedding_bytes < EMBEDDER_BYTES * 2,
            "the reserve should be near the real 0.21 GB embedder"
        );
    }

    #[test]
    fn the_kv_fallback_is_a_constant_because_the_data_says_it_is_not_a_curve() {
        // Qwen3-0.6B needs 112 KB/token; granite-4.1-3b needs 80. A model five
        // times smaller wanting more cache is not noise, it is head geometry —
        // so a size-based fit is unsound at any calibration.
        let small = ModelShape::new(0.6, Quantization::Q4, 8192);
        let large = ModelShape::new(8.0, Quantization::Q4, 8192);
        let kv = |sh: &ModelShape| Requirement::estimate(&m4_air(), sh).kv_cache_bytes;
        assert_eq!(
            kv(&small),
            kv(&large),
            "the fallback must not pretend to scale"
        );
        // And it must cover the worst case seen (Nemotron, 168 KB/token) close
        // enough that a refusal is the failure mode, not an OOM.
        let worst = 168_000u64 * 8192;
        assert!(
            kv(&small) as f64 >= worst as f64 * 0.9,
            "fallback {} is far under the observed worst case {worst}",
            kv(&small)
        );
    }

    #[test]
    fn the_footprint_excludes_the_os_reserve_but_the_requirement_includes_it() {
        // Two different questions: "how much will Marrow use" and "how much
        // must be free". Conflating them is how a 3.5 GB model gets offered on
        // a machine with 3.6 GB free.
        let r = Requirement::estimate(&m4_air(), &shape(4.0, Quantization::Q4, 8192));
        assert_eq!(r.total(), r.ai_footprint() + r.headroom_bytes);
        assert!(r.total() > r.ai_footprint());
    }

    #[test]
    fn unloading_releases_the_cache_and_the_runtime_but_not_the_embedder() {
        // LLM-025 plus the lifecycle: after the idle timeout the model goes,
        // and search still works — which means the embedder stays.
        let r = Requirement::estimate(&m4_air(), &shape(4.0, Quantization::Q4, 8192));
        assert_eq!(
            r.releasable_on_unload(),
            r.ai_footprint() - r.embedding_bytes
        );
        assert!(
            r.releasable_on_unload() > r.weights_bytes,
            "cache must be released too"
        );
    }

    #[test]
    fn a_7b_at_q4_fits_16gb_comfortably() {
        let v = assess(
            &m4_air(),
            &shape(7.0, Quantization::Q4, 8192),
            9_000_000_000,
        );
        assert_eq!(v.fit, Fit::Comfortable, "{}", v.reason);
        assert!(v.offerable());
    }

    #[test]
    fn a_70b_never_fits_16gb_and_says_so_with_numbers() {
        // LLM-016: listed, disabled, with the arithmetic — not hidden.
        let v = assess(
            &m4_air(),
            &shape(70.0, Quantization::Q4, 8192),
            9_000_000_000,
        );
        assert_eq!(v.fit, Fit::TooLarge);
        assert!(!v.offerable());
        assert!(
            v.reason.contains("GB"),
            "reason must carry numbers: {}",
            v.reason
        );
        assert!(
            v.reason.contains("cannot run here"),
            "reason must be plain: {}",
            v.reason
        );
    }

    #[test]
    fn a_model_that_fits_the_machine_but_not_free_memory_is_tight_not_refused() {
        // Closing a browser would make this work, so refusing outright would be
        // wrong — and saying "insufficient resources" would hide the fix.
        let v = assess(
            &m4_air(),
            &shape(13.0, Quantization::Q4, 8192),
            2_000_000_000,
        );
        assert_eq!(v.fit, Fit::Tight, "{}", v.reason);
        assert!(v.offerable(), "tight still offerable");
        assert!(
            v.reason.contains("Closing other applications"),
            "must name the remedy: {}",
            v.reason
        );
    }

    #[test]
    fn the_kv_cache_is_counted_and_scales_with_context() {
        // The usual cause of "it loaded, then died partway through".
        let short = Requirement::estimate(&m4_air(), &shape(7.0, Quantization::Q4, 2048));
        let long = Requirement::estimate(&m4_air(), &shape(7.0, Quantization::Q4, 131_072));
        assert!(long.total() > short.total());
        assert!(short.kv_cache_bytes > 0, "kv must not be ignored");
        assert_eq!(
            long.kv_cache_bytes / short.kv_cache_bytes,
            64,
            "64× the context is 64× the cache"
        );
    }

    #[test]
    fn a_measured_kv_figure_beats_the_estimate() {
        // The estimate exists because catalogues do not carry layer counts. A
        // real number must override it rather than sit beside it unused.
        let mut s = shape(4.0, Quantization::Q4, 8192);
        s.kv_bytes_per_token = Some(147_456);
        let r = Requirement::estimate(&m4_air(), &s);
        assert_eq!(r.kv_cache_bytes, 147_456 * 8192);
    }

    #[test]
    fn quantizing_the_kv_cache_roughly_halves_it() {
        // LLM-046. Off by default, so the default must be the expensive one.
        let f16 = Requirement::estimate(&m4_air(), &shape(4.0, Quantization::Q4, 8192));
        let mut s = shape(4.0, Quantization::Q4, 8192);
        s.kv_precision = KvPrecision::Q8;
        let q8 = Requirement::estimate(&m4_air(), &s);
        assert_eq!(q8.kv_cache_bytes * 2, f16.kv_cache_bytes);
        assert_eq!(
            KvPrecision::default(),
            KvPrecision::F16,
            "quantized KV is opt-in"
        );
    }

    #[test]
    fn an_external_runtime_adds_no_buffers_of_ours() {
        // Ollama's arenas are in Ollama's address space. Counting them twice
        // would refuse models that run fine.
        let mut s = shape(7.0, Quantization::Q4, 8192);
        s.runtime = RuntimeKind::External;
        assert_eq!(Requirement::estimate(&m4_air(), &s).runtime_bytes, 0);
        assert!(
            Requirement::estimate(&m4_air(), &shape(7.0, Quantization::Q4, 8192)).runtime_bytes > 0
        );
    }

    #[test]
    fn quantization_scales_the_weights_monotonically_and_never_optimistically() {
        let w = |q| Requirement::estimate(&m4_air(), &shape(7.0, q, 8192)).weights_bytes;
        let (f16, q8, q5, q4) = (
            w(Quantization::F16),
            w(Quantization::Q8),
            w(Quantization::Q5),
            w(Quantization::Q4),
        );
        assert!(q4 < q5 && q5 < q8 && q8 < f16, "{q4} {q5} {q8} {f16}");
        // Each factor must exceed its clean bits-per-weight ratio, because
        // scales, zero-points and unquantized embeddings are real bytes.
        assert!(
            q4 as f64 / f16 as f64 > 0.25,
            "Q4 must not claim a clean quarter"
        );
        assert!(
            q8 as f64 / f16 as f64 > 0.50,
            "Q8 must not claim a clean half"
        );
    }

    #[test]
    fn unified_memory_uses_a_reserve_not_a_percentage() {
        // 15% of 128 GB is 19 GB, which would be absurd to hold back on a
        // machine that shares memory with the OS in the first place.
        let unified = Requirement::estimate(&m4_air(), &shape(7.0, Quantization::Q4, 8192));
        let discrete = Requirement::estimate(&workstation(), &shape(7.0, Quantization::Q4, 8192));
        assert_eq!(unified.headroom_bytes, 2_500_000_000);
        assert!(discrete.headroom_bytes > unified.headroom_bytes);
    }

    #[test]
    fn the_cache_budget_scales_with_the_model_not_a_fixed_number() {
        // LLM-044.
        let small = Requirement::estimate(&m4_air(), &shape(4.0, Quantization::Q4, 8192));
        let large = Requirement::estimate(&workstation(), &shape(70.0, Quantization::Q4, 8192));
        assert!(large.cache_budget() > small.cache_budget() * 10);
    }

    #[test]
    fn a_bigger_model_at_q4_is_preferred_over_a_smaller_one_at_q8() {
        use std::cmp::Ordering;
        // Part 5 §96.2, encoded so it does not depend on list order.
        assert_eq!(
            prefer((14.0, Quantization::Q4), (7.0, Quantization::Q8)),
            Ordering::Greater
        );
    }

    #[test]
    fn every_verdict_carries_the_numbers_that_produced_it() {
        // LLM-017. A verdict without them is "insufficient resources", which
        // tells the user nothing they can act on.
        for (params, avail) in [(4.0, 9e9), (13.0, 2e9), (70.0, 9e9)] {
            let v = assess(
                &m4_air(),
                &shape(params, Quantization::Q4, 8192),
                avail as u64,
            );
            assert!(v.requirement.total() > 0);
            assert!(v.reason.contains("Needs about"), "{}", v.reason);
            for term in [
                "weights",
                "KV cache",
                "runtime",
                "embedding model",
                "OS reserve",
            ] {
                assert!(
                    v.breakdown.contains(term),
                    "breakdown missing {term}: {}",
                    v.breakdown
                );
            }
        }
    }
}
