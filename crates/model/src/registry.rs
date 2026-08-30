//! What models exist, what they can do, and whether they are here yet.

use std::collections::BTreeMap;

use marrow_hw::{KvPrecision, ModelShape, Quantization, RuntimeKind};
use serde::{Deserialize, Serialize};

use crate::breaker::Breaker;

/// The file format the weights are in.
///
/// LLM-037: an MLX build and a GGUF of the same weights are **separate
/// entries** with separate digests. They are different bytes with different
/// footprints, and conflating them makes a resumed download verify against the
/// wrong file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    Mlx,
    Gguf,
    Safetensors,
}

impl Format {
    pub fn runtime(self) -> RuntimeKind {
        match self {
            Format::Mlx => RuntimeKind::Mlx,
            Format::Gguf | Format::Safetensors => RuntimeKind::Gguf,
        }
    }
}

/// What the model can actually do. Claimed here, verified before use — a
/// capability the model does not have must show as disabled with a reason
/// (GEN-013), never be silently dropped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub tools: bool,
    pub structured_output: bool,
    /// Whether a thinking budget is honoured. The Fast/Thorough switch is
    /// disabled with a reason when this is false (§145).
    pub reasoning: bool,
    pub vision: bool,
    pub multilingual: bool,
    /// Whether it is an embedding model rather than a generative one.
    pub embedding: bool,
}

/// Licence facts, on the row and before the download button (LIC-004).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Licence {
    pub spdx_or_name: String,
    pub url: Option<String>,
    /// `None` means we have not established it, which is different from `false`
    /// and must not be displayed as either yes or no.
    pub commercial_use: Option<bool>,
}

/// Where an entry came from (§138.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Source {
    /// Ships with the app. Curated and licence-checked.
    Catalogue,
    /// Detected in an already-installed runtime — a library the user curated.
    Detected { runtime: String },
    /// A path the user pointed at. No integrity claim is made, because none
    /// can be.
    UserSupplied { path: String },
}

/// One file in a model's manifest, with the digest that proves it.
///
/// A model is a **directory** — weights, tokenizer, config — not a blob, so a
/// single `sha256` field never fitted. Each file carries its own digest and is
/// verified as it lands; the manifest as a whole is addressed by
/// [`Entry::manifest_digest`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    /// Path within the model directory. Always relative, never absolute, and
    /// never containing `..` — checked by [`Artifact::is_safe`].
    pub path: String,
    /// SHA-256 of the file's contents, lowercase hex.
    pub sha256: String,
    pub size: u64,
}

impl Artifact {
    /// Whether this path may be written under a model directory.
    ///
    /// The manifest is data that came from a server. A `path` of `../../bin/sh`
    /// would be a remote write primitive, so it is checked here rather than
    /// trusted because we typed the catalogue ourselves — the same manifest
    /// shape will one day come from a user-supplied source.
    pub fn is_safe(&self) -> bool {
        !self.path.is_empty()
            && !self.path.starts_with('/')
            && !self.path.contains('\\')
            && self
                .path
                .split('/')
                .all(|c| !c.is_empty() && c != "." && c != "..")
            && self.sha256.len() == 64
            && self
                .sha256
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    }
}

/// One model, as the supervisor sees it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub id: String,
    pub display_name: String,
    pub family: String,
    pub params_b: f64,
    pub quantization: Quantization,
    pub format: Format,
    /// The model's maximum. Never what it is sized against — Qwen3.5-4B
    /// advertises 262144, which at its real 128 KB/token is 34 GB of cache.
    pub context_limit: u32,
    /// What it will actually be run at, and therefore what admission sizes
    /// against.
    pub default_context: u32,
    /// Measured KV bytes per token at FP16, computed from the model's own
    /// config as `2 x layers x kv_heads x head_dim x 2`. `None` falls back to
    /// the conservative constant in `marrow_hw::sizing`, and the UI says which.
    pub kv_bytes_per_token: Option<u32>,
    /// Real on-disk size, summed from the manifest. Beats the quantization
    /// formula, which under-predicted every pinned model.
    pub weights_bytes: Option<u64>,
    pub capabilities: Capabilities,
    pub licence: Licence,
    /// What this entry is for in the shortlist — why it is in the catalogue
    /// rather than one of the thousand others.
    pub role: String,
    pub source: Source,
    /// Where the files come from, e.g. `mlx-community/Qwen3.5-4B-MLX-4bit`.
    pub repo: Option<String>,
    /// The **commit** the manifest was pinned against, never a branch. A
    /// manifest that points at `main` is not a manifest.
    pub revision: Option<String>,
    /// Every file needed to load the model, each with its own digest.
    pub files: Vec<Artifact>,
    /// SHA-256 over the manifest itself, and therefore the name of the
    /// directory the files land in (SUP-014). Two models can never collide and
    /// a partial download can never be mistaken for a complete one.
    pub manifest_digest: Option<String>,
    pub installed: bool,
    /// Persisted, because a breaker that forgets on relaunch is not a breaker.
    #[serde(default)]
    pub breaker: Breaker,
}

impl Entry {
    /// The memory shape, for [`marrow_hw::assess`].
    ///
    /// `context` is clamped to the model's ceiling, but callers should pass
    /// [`Entry::default_context`] rather than the ceiling: sizing Qwen3.5-4B
    /// at its advertised 262144 asks for 34 GB of KV cache and rejects a model
    /// that runs comfortably at 8k.
    pub fn shape(&self, context: u32, kv_precision: KvPrecision) -> ModelShape {
        ModelShape {
            params_b: self.params_b,
            quantization: self.quantization,
            context: context.min(self.context_limit),
            weights_bytes: self.weights_bytes,
            kv_bytes_per_token: self.kv_bytes_per_token,
            kv_precision,
            runtime: match &self.source {
                // An external server's buffers live in its address space.
                Source::Detected { .. } => RuntimeKind::External,
                _ => self.format.runtime(),
            },
            embedding_resident: !self.capabilities.embedding,
        }
    }

    /// Whether a download can be offered.
    ///
    /// SUP-014 / PKG-011: no verified manifest, no download. A model we cannot
    /// verify is one we cannot tell apart from a corrupted or substituted one.
    /// An unsafe path anywhere in the manifest disqualifies the whole entry —
    /// a manifest is only as trustworthy as its worst row.
    pub fn downloadable(&self) -> bool {
        !self.installed
            && self.manifest_digest.is_some()
            && self.repo.is_some()
            && self.revision.is_some()
            && !self.files.is_empty()
            && self.files.iter().all(Artifact::is_safe)
    }

    /// Total bytes to fetch. Shown before the button, never after it
    /// (SKEL-005: real bytes, never an indeterminate bar).
    pub fn download_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// The URL for one file, pinned to the revision.
    pub fn file_url(&self, file: &Artifact) -> Option<String> {
        Some(format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            self.repo.as_ref()?,
            self.revision.as_ref()?,
            file.path
        ))
    }

    /// SHA-256 over `path\0sha256\0size\n` per file, sorted by path.
    ///
    /// Deterministic and order-independent, so the same set of files always
    /// names the same directory — which is what makes a resumed download
    /// resume against the right bytes.
    pub fn compute_manifest_digest(files: &[Artifact]) -> String {
        let mut sorted: Vec<&Artifact> = files.iter().collect();
        sorted.sort_by(|a, b| a.path.cmp(&b.path));
        let mut h = blake3::Hasher::new();
        for f in sorted {
            h.update(f.path.as_bytes());
            h.update(b"\0");
            h.update(f.sha256.as_bytes());
            h.update(b"\0");
            h.update(f.size.to_string().as_bytes());
            h.update(b"\n");
        }
        h.finalize().to_hex().to_string()
    }

    /// Why the Fast/Thorough switch is disabled, if it is (GEN-013).
    /// `None` for an embedding model: the switch is not *disabled* there, it
    /// is irrelevant. "EmbeddingGemma 300M answers directly" is a sentence
    /// about something that never answers at all.
    pub fn reasoning_unavailable_because(&self) -> Option<String> {
        (!self.capabilities.reasoning && !self.capabilities.embedding)
            .then(|| format!("{} answers directly.", self.display_name))
    }
}

/// Every known model, keyed by id.
///
/// Ordered, so the Models page renders the same way twice — an unstable list is
/// a list nobody can point at in a bug report.
#[derive(Clone, Debug, Default)]
pub struct Registry {
    entries: BTreeMap<String, Entry>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The built-in catalogue. **Never fetched from the network** — a catalogue
    /// that updates itself is a channel by which a machine starts running
    /// something the user did not choose (§138.2).
    pub fn with_builtin_catalogue() -> Self {
        let mut r = Self::new();
        for e in crate::catalogue::builtin() {
            r.insert(e);
        }
        r
    }

    pub fn insert(&mut self, e: Entry) {
        self.entries.insert(e.id.clone(), e);
    }

    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.entries.get(id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Entry> {
        self.entries.get_mut(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Entry> {
        self.entries.values_mut()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn installed(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values().filter(|e| e.installed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue;

    #[test]
    fn a_model_without_a_manifest_is_never_downloadable() {
        // SUP-014. An unverifiable download cannot be told apart from a
        // substituted one, so it is not offered at all.
        let mut e = catalogue::builtin().into_iter().next().unwrap();
        assert!(e.downloadable(), "the pinned entry should be downloadable");
        e.manifest_digest = None;
        assert!(!e.downloadable());
    }

    #[test]
    fn one_unsafe_path_disqualifies_the_whole_manifest() {
        // A manifest is only as trustworthy as its worst row: a single
        // `../` turns a download into a remote write primitive.
        let mut e = catalogue::builtin().into_iter().next().unwrap();
        e.files.push(Artifact {
            path: "../../../../etc/authorized_keys".into(),
            sha256: "b".repeat(64),
            size: 1,
        });
        assert!(!e.downloadable(), "one bad row must block the entry");
    }

    #[test]
    fn an_already_installed_model_is_not_offered_for_download() {
        let mut e = catalogue::builtin().into_iter().next().unwrap();
        e.installed = true;
        assert!(!e.downloadable());
    }

    #[test]
    fn an_external_model_does_not_charge_us_for_its_runtime_buffers() {
        // Ollama's arenas are Ollama's. Counting them twice refuses models
        // that run fine.
        let mut e = catalogue::builtin().into_iter().next().unwrap();
        e.source = Source::Detected {
            runtime: "ollama".into(),
        };
        assert_eq!(
            e.shape(8192, KvPrecision::F16).runtime,
            RuntimeKind::External
        );
    }

    #[test]
    fn a_context_request_is_clamped_to_the_models_limit() {
        // Sizing a 32k model as though it were 128k rejects a model that works.
        let e = catalogue::builtin().into_iter().next().unwrap();
        let s = e.shape(1_000_000, KvPrecision::F16);
        assert_eq!(s.context, e.context_limit);
    }

    #[test]
    fn a_model_without_reasoning_says_so_rather_than_ignoring_the_flag() {
        // GEN-013. Silently dropping the flag makes Thorough a lie.
        let mut e = catalogue::builtin().into_iter().next().unwrap();
        e.capabilities.reasoning = false;
        let why = e.reasoning_unavailable_because().expect("must explain");
        assert!(why.contains(&e.display_name), "{why}");
        e.capabilities.reasoning = true;
        assert!(e.reasoning_unavailable_because().is_none());
    }

    #[test]
    fn the_registry_iterates_in_a_stable_order() {
        // An unstable list is one nobody can point at in a bug report.
        let r = Registry::with_builtin_catalogue();
        let a: Vec<_> = r.iter().map(|e| &e.id).collect();
        let b: Vec<_> = r.iter().map(|e| &e.id).collect();
        assert_eq!(a, b);
        assert!(!a.is_empty());
    }
}
