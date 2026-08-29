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
    pub context_limit: u32,
    /// Measured KV bytes per token at FP16, where it is known. `None` means the
    /// sizing estimate is used, and the UI says which.
    pub kv_bytes_per_token: Option<u32>,
    pub capabilities: Capabilities,
    pub licence: Licence,
    /// What this entry is for in the shortlist — why it is in the catalogue
    /// rather than one of the thousand others.
    pub role: String,
    pub source: Source,
    /// Content address. `None` means no integrity claim, which means
    /// downloading is not offered (SUP-014).
    pub sha256: Option<String>,
    pub download_url: Option<String>,
    pub installed: bool,
    /// Persisted, because a breaker that forgets on relaunch is not a breaker.
    #[serde(default)]
    pub breaker: Breaker,
}

impl Entry {
    /// The memory shape, for [`marrow_hw::assess`].
    pub fn shape(&self, context: u32, kv_precision: KvPrecision) -> ModelShape {
        ModelShape {
            params_b: self.params_b,
            quantization: self.quantization,
            context: context.min(self.context_limit),
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
    /// SUP-014 / PKG-011: no digest, no download. A model we cannot verify is
    /// one we cannot tell apart from a corrupted or substituted one.
    pub fn downloadable(&self) -> bool {
        !self.installed && self.sha256.is_some() && self.download_url.is_some()
    }

    /// Why the Fast/Thorough switch is disabled, if it is (GEN-013).
    pub fn reasoning_unavailable_because(&self) -> Option<String> {
        (!self.capabilities.reasoning).then(|| format!("{} answers directly.", self.display_name))
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
    fn a_model_without_a_digest_is_never_downloadable() {
        // SUP-014. An unverifiable download cannot be told apart from a
        // substituted one, so it is not offered at all.
        let mut e = catalogue::builtin().into_iter().next().unwrap();
        e.sha256 = None;
        assert!(!e.downloadable());
        e.sha256 = Some("0".repeat(64));
        e.download_url = Some("https://example.invalid/x".into());
        assert!(e.downloadable());
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
