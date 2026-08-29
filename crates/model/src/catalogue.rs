//! The built-in shortlist.
//!
//! Four models, each in the list for a different reason, all in the 3–4B class
//! because that is what the memory budget allows (`marrow_hw::sizing`):
//!
//! | Model | Why it is here |
//! |---|---|
//! | Qwen 3.5 4B | Primary candidate — the default generator and router |
//! | Nemotron Nano ~4B | Reasoning and agent behaviour |
//! | Granite 4.x 3B | Tool calling and structured output |
//! | Gemma (small) | General quality, and a second opinion on Apple Silicon |
//!
//! One model serves two roles in the ask pipeline, which is why a single
//! resident 4B is the target rather than a router plus a generator:
//!
//! ```text
//!   user question
//!        ↓
//!   4B  intent / router          ← same weights, same KV prefix
//!        ↓
//!   search · graph · metadata
//!        ↓
//!   top 5–15 chunks
//!        ↓
//!   4B  answer                   ← same weights, prefix reused (LLM-040)
//!        ↓
//!   answer + citations
//! ```
//!
//! # Digests are not guessed
//!
//! Every entry here ships with `sha256: None` until the real file has been
//! fetched and hashed on a machine we control. [`Entry::downloadable`] is
//! therefore false, and the Models page says *"digest not yet pinned"* rather
//! than offering a download it cannot verify. A plausible-looking wrong hash
//! would be worse than no hash: it would fail verification after a 3 GB pull
//! and look like a corrupt network.

use marrow_hw::Quantization;

use crate::registry::{Capabilities, Entry, Format, Licence, Source};

/// Bytes per token of KV cache at FP16, where it has been measured. `None`
/// falls back to the sizing estimate, and the UI says which it used.
type KvPerToken = Option<u32>;

#[allow(clippy::too_many_arguments)] // A catalogue row has this many facts;
// bundling them into a struct would only move the same list one line up.
fn entry(
    id: &str,
    display_name: &str,
    family: &str,
    params_b: f64,
    context_limit: u32,
    kv_bytes_per_token: KvPerToken,
    capabilities: Capabilities,
    licence: Licence,
    role: &str,
) -> Entry {
    Entry {
        id: id.into(),
        display_name: display_name.into(),
        family: family.into(),
        params_b,
        // MLX 4-bit is the target format on the development machine (§139.1.1).
        quantization: Quantization::Q4,
        format: Format::Mlx,
        context_limit,
        kv_bytes_per_token,
        capabilities,
        licence,
        role: role.into(),
        source: Source::Catalogue,
        sha256: None,
        download_url: None,
        installed: false,
        breaker: Default::default(),
    }
}

fn apache2() -> Licence {
    Licence {
        spdx_or_name: "Apache-2.0".into(),
        url: Some("https://www.apache.org/licenses/LICENSE-2.0".into()),
        commercial_use: Some(true),
    }
}

/// The catalogue. Read-only, compiled in, never fetched (§138.2).
pub fn builtin() -> Vec<Entry> {
    vec![
        entry(
            "qwen3.5-4b-mlx-q4",
            "Qwen 3.5 4B",
            "qwen",
            4.0,
            32_768,
            None,
            Capabilities {
                tools: true,
                structured_output: true,
                reasoning: true,
                multilingual: true,
                ..Default::default()
            },
            apache2(),
            "Primary candidate. Routes the question and writes the answer.",
        ),
        entry(
            "nemotron-nano-4b-mlx-q4",
            "Nemotron Nano 4B",
            "nemotron",
            4.0,
            32_768,
            None,
            Capabilities {
                tools: true,
                structured_output: true,
                reasoning: true,
                ..Default::default()
            },
            Licence {
                spdx_or_name: "NVIDIA Open Model Licence".into(),
                url: None,
                // Not established here. `None` is not `false`, and the UI must
                // not render it as either (LIC-004).
                commercial_use: None,
            },
            "Reasoning and agent behaviour — the Thorough-mode comparison.",
        ),
        entry(
            "granite-4-3b-mlx-q4",
            "Granite 4 3B",
            "granite",
            3.0,
            32_768,
            None,
            Capabilities {
                tools: true,
                structured_output: true,
                reasoning: false,
                ..Default::default()
            },
            apache2(),
            "Tool calling and structured output — the MCP-facing workload.",
        ),
        entry(
            "gemma-4b-mlx-q4",
            "Gemma 4B",
            "gemma",
            4.0,
            8_192,
            None,
            Capabilities {
                tools: false,
                structured_output: true,
                reasoning: false,
                multilingual: true,
                ..Default::default()
            },
            Licence {
                spdx_or_name: "Gemma Terms of Use".into(),
                url: None,
                commercial_use: None,
            },
            "General quality, and a second opinion on Apple Silicon.",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use marrow_hw::{assess, KvPrecision, Machine};

    fn m4_air() -> Machine {
        Machine {
            total_memory_bytes: 17_179_869_184,
            cpu_cores: 10,
            unified_memory: true,
            ..Machine::unknown()
        }
    }

    #[test]
    fn nothing_in_the_catalogue_claims_a_digest_it_does_not_have() {
        // A plausible-looking wrong hash is worse than none: it fails after a
        // 3 GB pull and looks like a corrupt network.
        for e in builtin() {
            assert_eq!(e.sha256, None, "{} must not carry a guessed digest", e.id);
            assert!(
                !e.downloadable(),
                "{} must not offer an unverifiable download",
                e.id
            );
        }
    }

    #[test]
    fn every_catalogue_model_fits_the_development_machine() {
        // The shortlist exists because of the memory budget. A model in it
        // that does not fit is a shortlist that was not checked.
        for e in builtin() {
            let v = assess(&m4_air(), &e.shape(8192, KvPrecision::F16), 8_000_000_000);
            assert!(v.offerable(), "{} does not fit: {}", e.id, v.reason);
            assert!(
                v.requirement.ai_footprint() < 5_000_000_000,
                "{} footprint {} exceeds the ~3–4 GB budget",
                e.id,
                v.breakdown
            );
        }
    }

    #[test]
    fn every_model_says_why_it_is_in_the_list() {
        // Four models with no stated role is a list that grows to forty.
        for e in builtin() {
            assert!(!e.role.is_empty(), "{} has no role", e.id);
            assert!(e.role.len() > 20, "{}: {:?} is not a reason", e.id, e.role);
        }
    }

    #[test]
    fn commercial_use_is_unknown_rather_than_assumed_where_it_is_unknown() {
        // LIC-004. `None` renders as "not established", never as yes or no.
        let entries = builtin();
        let nemotron = entries.iter().find(|e| e.family == "nemotron").unwrap();
        assert_eq!(nemotron.licence.commercial_use, None);
        let qwen = entries.iter().find(|e| e.family == "qwen").unwrap();
        assert_eq!(qwen.licence.commercial_use, Some(true));
    }

    #[test]
    fn ids_are_unique_and_name_their_format_and_quantization() {
        // The id is what a resumed download verifies against, and an MLX build
        // is not a GGUF (LLM-037).
        let ids: std::collections::HashSet<_> = builtin().iter().map(|e| e.id.clone()).collect();
        assert_eq!(ids.len(), builtin().len(), "duplicate model id");
        for e in builtin() {
            assert!(e.id.contains("mlx"), "{} must name its format", e.id);
            assert!(e.id.contains("q4"), "{} must name its quantization", e.id);
        }
    }

    #[test]
    fn the_shortlist_covers_the_roles_the_pipeline_needs() {
        // Router + answer + tools. A catalogue with no tool-calling model
        // cannot serve the MCP surface at all.
        let all = builtin();
        assert!(
            all.iter().any(|e| e.capabilities.tools),
            "no tool-calling model"
        );
        assert!(
            all.iter().any(|e| e.capabilities.reasoning),
            "no Thorough-capable model"
        );
        assert!(
            all.iter().all(|e| e.capabilities.structured_output),
            "every model must do structured output; the router depends on it"
        );
    }
}
