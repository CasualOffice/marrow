//! The built-in shortlist, **generated from real manifests**.
//!
//! | Model | Why it is here |
//! |---|---|
//! | Qwen 3.5 4B | Primary candidate — the default generator |
//! | Nemotron 3 Nano 4B | Reasoning and agent behaviour |
//! | Granite 4.1 3B | Tool calling and structured output |
//! | Gemma 3 4B | General quality, and a second opinion on Apple Silicon |
//! | Qwen 3 0.6B | The router (§139.5) — resident during ingest |
//! | EmbeddingGemma 300M | The embedder — resident always, because search is the product |
//!
//! ```text
//!   user question
//!        ↓
//!   0.6B  intent / router          structured output, not prose
//!        ↓
//!   search · graph · metadata      embedder resident
//!        ↓
//!   top 5–15 chunks
//!        ↓
//!   4B   answer                    loaded on demand, unloaded when idle
//!        ↓
//!   answer + citations
//! ```
//!
//! # Every digest here is real
//!
//! This file is **generated**, not typed. Each entry names a HuggingFace repo
//! and a **commit**, and lists every file with its SHA-256 and size:
//!
//! - Large files carry a published LFS `sha256` oid — taken as-is.
//! - Small files (config, tokenizer settings) publish only a git blob SHA-1,
//!   which is not a content digest, so they were fetched and hashed. A
//!   manifest with a hole in it is not a manifest.
//!
//! Pinned to a commit rather than to `main`, because a manifest pointing at a
//! moving branch cannot be verified twice.
//!
//! `kv_bytes_per_token` is likewise measured — `2 × layers × kv_heads ×
//! head_dim × 2`, read from each model's own `config.json`. It replaced an
//! estimate that under-predicted the real figure by up to 2×, which is the
//! direction that OOMs mid-generation.
//!
//! To re-pin after adding a model: re-run `pin-catalogue.py` and regenerate.

use marrow_hw::Quantization;

use crate::registry::{Artifact, Capabilities, Entry, Format, Licence, Source};

/// The generated shape of one catalogue row.
struct Pin {
    id: &'static str,
    display_name: &'static str,
    family: &'static str,
    params_b: f64,
    repo: &'static str,
    revision: &'static str,
    context_limit: u32,
    default_context: u32,
    kv_bytes_per_token: u32,
    capabilities: Capabilities,
    licence: Licence,
    role: &'static str,
    files: Vec<Artifact>,
}

fn f(path: &str, sha256: &str, size: u64) -> Artifact {
    Artifact {
        path: path.into(),
        sha256: sha256.into(),
        size,
    }
}

fn entry(p: Pin) -> Entry {
    let weights_bytes = p.files.iter().map(|a| a.size).sum();
    let manifest_digest = Entry::compute_manifest_digest(&p.files);
    Entry {
        id: p.id.into(),
        display_name: p.display_name.into(),
        family: p.family.into(),
        params_b: p.params_b,
        // Every pinned model is an MLX 4-bit build (§139.1.1).
        quantization: Quantization::Q4,
        format: Format::Mlx,
        context_limit: p.context_limit,
        default_context: p.default_context,
        kv_bytes_per_token: Some(p.kv_bytes_per_token),
        weights_bytes: Some(weights_bytes),
        capabilities: p.capabilities,
        licence: p.licence,
        role: p.role.into(),
        source: Source::Catalogue,
        repo: Some(p.repo.into()),
        revision: Some(p.revision.into()),
        files: p.files,
        manifest_digest: Some(manifest_digest),
        installed: false,
        breaker: Default::default(),
    }
}

/// The catalogue. Read-only, compiled in, **never fetched at runtime** — a
/// catalogue that updates itself is a channel by which a machine starts
/// running something the user did not choose (§138.2).
pub fn builtin() -> Vec<Entry> {
    vec![
        // mlx-community/Qwen3.5-4B-MLX-4bit
        // 32 layers x 4 KV heads x 256 head dim
        //   -> 128 KB/token at f16
        // 3.06 GB across 10 files
        entry(Pin {
            id: "qwen3.5-4b-mlx-q4",
            display_name: "Qwen 3.5 4B",
            family: "qwen",
            params_b: 4.0,
            repo: "mlx-community/Qwen3.5-4B-MLX-4bit",
            revision: "32f3e8ecf65426fc3306969496342d504bfa13f3",
            context_limit: 262144,
            default_context: 8192,
            kv_bytes_per_token: 131072,
            capabilities: Capabilities {
                tools: true,
                structured_output: true,
                reasoning: true,
                multilingual: true,
                ..Default::default()
            },
            licence: Licence {
                spdx_or_name: "Apache-2.0".into(),
                url: Some("https://www.apache.org/licenses/LICENSE-2.0".into()),
                commercial_use: Some(true),
            },
            role: "Primary candidate. Routes the question and writes the answer.",
            files: vec![
            f("chat_template.jinja", "a4aee8afcf2e0711942cf848899be66016f8d14a889ff9ede07bca099c28f715", 7756),
            f("config.json", "f3efc81b2ea8d96a45301037d3ccccbcccdef44a961845c87f286aaddbc6eaaa", 3366),
            f("model.safetensors", "5fb9acd0246866381cf8c5c354c6db1019f6498eec4ccb4f5edcc71ffeacb2db", 3034300695),
            f("model.safetensors.index.json", "52e534c41f7b97708329c85f762e5882bf48bd5955a422c6ae74eba321e6048a", 101944),
            f("preprocessor_config.json", "27225450ac9c6529872ee1924fcb0962ff5634834f817040f444118116f4e516", 390),
            f("processor_config.json", "14932921ca485d458a04dafd8069fbb0a4505622a48208d19ed247115801385b", 1300),
            f("tokenizer.json", "87a7830d63fcf43bf241c3c5242e96e62dd3fdc29224ca26fed8ea333db72de4", 19989343),
            f("tokenizer_config.json", "e98f1901ac6f0adff67b1d540bfa0c36ac1a0cf59eb72ed78146ef89aafa1182", 1139),
            f("video_preprocessor_config.json", "7768af27c1fafa9cc9011c1dc20067e03f8915e03b63504550e11d5066986d13", 385),
            f("vocab.json", "ce99b4cb2983d118806ce0a8b777a35b093e2000a503ebde25853284c9dfa003", 6722759),
            ],
        }),
        // mlx-community/NVIDIA-Nemotron-3-Nano-4B-4bit
        // 42 layers x 8 KV heads x 128 head dim
        //   -> 168 KB/token at f16
        // 2.25 GB across 11 files
        entry(Pin {
            id: "nemotron-3-nano-4b-mlx-q4",
            display_name: "Nemotron 3 Nano 4B",
            family: "nemotron",
            params_b: 4.0,
            repo: "mlx-community/NVIDIA-Nemotron-3-Nano-4B-4bit",
            revision: "c4d79ba1901d99806ef757642a552acebb851a35",
            context_limit: 262144,
            default_context: 8192,
            kv_bytes_per_token: 172032,
            capabilities: Capabilities {
                tools: true,
                structured_output: true,
                reasoning: true,
                ..Default::default()
            },
            licence: Licence {
                spdx_or_name: "NVIDIA Open Model Licence".into(),
                url: None,
                commercial_use: None,
            },
            role: "Reasoning and agent behaviour — the Thorough-mode comparison.",
            files: vec![
            f("__init__.py", "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", 0),
            f("chat_template.jinja", "ab7813c3abdd9cb655905a410728b26c7884eca45ddfab8d9f931553485a7862", 10504),
            f("config.json", "8ce56fadc46e425aa3a7350cd8faa32f8aaf12b3d99bd395b44040c20010cf85", 1772),
            f("configuration_nemotron_h.py", "07fa66e5b3da7e6a71c1a263e3dd68da11c8afa9178b47c49510ba628746fcff", 12119),
            f("generation_config.json", "676822c7918b11500095f25dd35405ed73ef77020d0941458c91ee8ef7ef60ad", 188),
            f("model.safetensors", "8750133f3cbe7a0f3eeffbdd3b0a4ba5287d1abca86b5d9afbd8e827373dfe29", 2237078664),
            f("model.safetensors.index.json", "42c5a9e03e377104a4c63c6833bbc53569bb771c440ac86337b17e0de5ad887f", 31344),
            f("modeling_nemotron_h.py", "ea982af0b805f181573f919ecb001d5bbc0153459923cf4b2f1ccae194e415a4", 78629),
            f("nano_v3_reasoning_parser.py", "aafb12208054504f619cbdd01837e1532a482ad937ed987bfe9a13fb812ae2b7", 798),
            f("tokenizer.json", "623c34567aebb18582765289fbe23d901c62704d6518d71866e0e58db892b5b7", 17077484),
            f("tokenizer_config.json", "2440a4b36fd030f0c610c75bc8f5563af193e73c1da5130471300f4ff277b99d", 372),
            ],
        }),
        // mlx-community/granite-4.1-3b-4bit
        // 40 layers x 8 KV heads x 64 head dim
        //   -> 80 KB/token at f16
        // 2.13 GB across 7 files
        entry(Pin {
            id: "granite-4.1-3b-mlx-q4",
            display_name: "Granite 4.1 3B",
            family: "granite",
            params_b: 3.0,
            repo: "mlx-community/granite-4.1-3b-4bit",
            revision: "b1b476b5a17c46b7d6cd663b4a8ed44b66720aef",
            context_limit: 131072,
            default_context: 8192,
            kv_bytes_per_token: 81920,
            capabilities: Capabilities {
                tools: true,
                structured_output: true,
                ..Default::default()
            },
            licence: Licence {
                spdx_or_name: "Apache-2.0".into(),
                url: Some("https://www.apache.org/licenses/LICENSE-2.0".into()),
                commercial_use: Some(true),
            },
            role: "Tool calling and structured output — the MCP-facing workload.",
            files: vec![
            f("chat_template.jinja", "fed2756d2d24e127b951dcf139d0b03ab7db8ef23a456128ebc9c2db4901d476", 6099),
            f("config.json", "4e7692aa044e9faefc7ef89a09844f4db1d038e08ee9862eaf34cae209c0651f", 1067),
            f("generation_config.json", "9117fb03fed79dbb459373edeef9a3eec966bce52a1842e03a6716e83117f0d1", 147),
            f("model.safetensors", "cff9d052cc3c68ea66b3d364788eb96fca2be82868d9ad92bd968e73b125194d", 2127162429),
            f("model.safetensors.index.json", "7e986d9c490d1ae27e1acbc2d3227ed90c5e5d38872813fd7a8591f073b30165", 65320),
            f("tokenizer.json", "24665f2815ee47aba521e4710bfc52f8ca045a552ec3f60925cce5e0edecb657", 7153434),
            f("tokenizer_config.json", "fb2f8b5d9980cc7c2e4aa3361b34d3f43c6ff94f675276ac2e32fb2e9b751594", 418),
            ],
        }),
        // mlx-community/gemma-3-4b-it-qat-4bit
        // 34 layers x 4 KV heads x 256 head dim
        //   -> 136 KB/token at f16
        // 3.03 GB across 12 files
        entry(Pin {
            id: "gemma-3-4b-it-mlx-q4",
            display_name: "Gemma 3 4B",
            family: "gemma",
            params_b: 4.0,
            repo: "mlx-community/gemma-3-4b-it-qat-4bit",
            revision: "3d9ef289111449933c22761961f16a5df237ce2a",
            context_limit: 131072,
            default_context: 8192,
            kv_bytes_per_token: 139264,
            capabilities: Capabilities {
                structured_output: true,
                vision: true,
                multilingual: true,
                ..Default::default()
            },
            licence: Licence {
                spdx_or_name: "Gemma Terms of Use".into(),
                url: Some("https://ai.google.dev/gemma/terms".into()),
                commercial_use: None,
            },
            role: "General quality, and a second opinion on Apple Silicon.",
            files: vec![
            f("added_tokens.json", "50b2f405ba56a26d4913fd772089992252d7f942123cc0a034d96424221ba946", 35),
            f("chat_template.json", "fe16baf728db49457cde32802cd7efc0ac8a7a9877dbe22fe3322b2d9dc6ccd9", 1615),
            f("config.json", "ee036bd0fae8bb5791217540f33e0b85f6fd56db37218100ddea01c442fcee24", 7237),
            f("generation_config.json", "13da6aad6852a008419f46df754b3452fc96387a4213c25811509d474e5a4776", 173),
            f("model.safetensors", "b4bb1430e6090d163d33822dbad1c3e6cb16276f0c8f55cf27a6a240640bfa61", 2995351814),
            f("model.safetensors.index.json", "77f4b67de084c31c7bcd373b039908108eee6c6181607e6d53da730e5f0bc659", 90558),
            f("preprocessor_config.json", "f688d6bb20c5017601c4011de7ca656da8485b540b05013efdaf986c0fcc918d", 570),
            f("processor_config.json", "3ffd5f11778dc73e2b69b3c00535e4121e1badf7018136263cd17b5b34fbaa53", 70),
            f("special_tokens_map.json", "2f7b0adf4fb469770bb1490e3e35df87b1dc578246c5e7e6fc76ecf33213a397", 662),
            f("tokenizer.json", "4667f2089529e8e7657cfb6d1c19910ae71ff5f28aa7ab2ff2763330affad795", 33384568),
            f("tokenizer.model", "1299c11d7cf632ef3b4e11937501358ada021bbdf7c47638d13c0ee982f2e79c", 4689074),
            f("tokenizer_config.json", "bfe25c2735e395407beb78456ea9a6984a1f00d8c16fa04a8b75f2a614cf53e1", 1156999),
            ],
        }),
        // mlx-community/Qwen3-0.6B-4bit
        // 28 layers x 8 KV heads x 128 head dim
        //   -> 112 KB/token at f16
        // 0.35 GB across 9 files
        entry(Pin {
            id: "qwen3-0.6b-mlx-q4",
            display_name: "Qwen 3 0.6B",
            family: "qwen",
            params_b: 0.6,
            repo: "mlx-community/Qwen3-0.6B-4bit",
            revision: "73e3e38d981303bc594367cd910ea6eb48349da8",
            context_limit: 40960,
            default_context: 4096,
            kv_bytes_per_token: 114688,
            capabilities: Capabilities {
                structured_output: true,
                multilingual: true,
                ..Default::default()
            },
            licence: Licence {
                spdx_or_name: "Apache-2.0".into(),
                url: Some("https://www.apache.org/licenses/LICENSE-2.0".into()),
                commercial_use: Some(true),
            },
            role: "The router. Intent, query rewrite and NER — runs once per file, so it must be cheap.",
            files: vec![
            f("added_tokens.json", "c0284b582e14987fbd3d5a2cb2bd139084371ed9acbae488829a1c900833c680", 707),
            f("config.json", "15d3ac26c043ae477273ed5802ee0f0b33bb14f18c9d3dd70910c02d906e3f1f", 937),
            f("merges.txt", "8831e4f1a044471340f7c0a83d7bd71306a5b867e95fd870f74d0c5308a904d5", 1671853),
            f("model.safetensors", "392e8d466d56100ada00eb82031fb854297fc9e389b7d303eba3af114e87bce2", 335450584),
            f("model.safetensors.index.json", "7b294141456f6904936db03c00bca50fb5f6198f652fe8483f9cd2a1018accfb", 49731),
            f("special_tokens_map.json", "76862e765266b85aa9459767e33cbaf13970f327a0e88d1c65846c2ddd3a1ecd", 613),
            f("tokenizer.json", "aeb13307a71acd8fe81861d94ad54ab689df773318809eed3cbe794b4492dae4", 11422654),
            f("tokenizer_config.json", "253153d0738ceb4c668d2eff957714dd2bea0b56de772a9fdccd96cbf517e6a0", 9706),
            f("vocab.json", "ca10d7e9fb3ed18575dd1e277a2579c16d108e32f27439684afa0e10b1440910", 2776833),
            ],
        }),
        // mlx-community/embeddinggemma-300m-4bit
        // 24 layers x 1 KV heads x 256 head dim
        //   -> 24 KB/token at f16
        // 0.21 GB across 12 files
        entry(Pin {
            id: "embeddinggemma-300m-mlx-q4",
            display_name: "EmbeddingGemma 300M",
            family: "gemma",
            params_b: 0.3,
            repo: "mlx-community/embeddinggemma-300m-4bit",
            revision: "5d9ef074df3957afc5c77127f208fddbc3c54187",
            context_limit: 2048,
            default_context: 2048,
            kv_bytes_per_token: 24576,
            capabilities: Capabilities {
                multilingual: true,
                embedding: true,
                ..Default::default()
            },
            licence: Licence {
                spdx_or_name: "Gemma Terms of Use".into(),
                url: Some("https://ai.google.dev/gemma/terms".into()),
                commercial_use: None,
            },
            role: "The embedder. Semantic search runs on this, so it stays loaded.",
            files: vec![
            f("added_tokens.json", "50b2f405ba56a26d4913fd772089992252d7f942123cc0a034d96424221ba946", 35),
            f("config.json", "8f7e856558357fea02487ff368eac6dd899fcc557290e34efa70ffd4d6ea78e8", 1726),
            f("config_sentence_transformers.json", "8eadac15526f83d8950aa8d962a7f4f6e3d678bea71689960194561f33a5f64f", 997),
            f("generation_config.json", "1fb1efd221c1ca88a736d1b36cb47d754c177677e222acb3b1e5424c5d664870", 133),
            f("model.safetensors", "f2366c4c0dfdac15b30548ee44c9e06d7e7c0eb0bd13d26279f953fe2c9b278a", 173210751),
            f("model.safetensors.index.json", "98a2ae36bcda747cb785d60ed521bd5a31c36406fc23117ad8435bc4cb32735b", 46809),
            f("modules.json", "5b5649645fb756dad1a8e2efe7872d3bb32bc00b93c95f276dd17f474eedccdc", 573),
            f("sentence_bert_config.json", "5ea26221ce733ace29a3897360e7c6ac8816b2ca0f7306657d69e594fece7325", 58),
            f("special_tokens_map.json", "2f7b0adf4fb469770bb1490e3e35df87b1dc578246c5e7e6fc76ecf33213a397", 662),
            f("tokenizer.json", "6852f8d561078cc0cebe70ca03c5bfdd0d60a45f9d2e0e1e4cc05b68e9ec329e", 33385008),
            f("tokenizer.model", "1299c11d7cf632ef3b4e11937501358ada021bbdf7c47638d13c0ee982f2e79c", 4689074),
            f("tokenizer_config.json", "9076840490613047bc9115963ee96b7702018b0d26ba644240bf856efda93118", 1155346),
            ],
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use marrow_hw::{assess, KvPrecision, Machine};
    use std::collections::HashSet;

    fn m4_air() -> Machine {
        Machine {
            total_memory_bytes: 17_179_869_184,
            cpu_cores: 10,
            unified_memory: true,
            ..Machine::unknown()
        }
    }

    #[test]
    fn every_model_carries_a_complete_verified_manifest() {
        // The whole point of the pinning pass. A model with a partial
        // manifest downloads files nobody checked.
        for e in builtin() {
            assert!(e.downloadable(), "{} is not downloadable", e.id);
            assert!(e.repo.is_some(), "{} has no repo", e.id);
            assert!(!e.files.is_empty(), "{} has no files", e.id);
            for a in &e.files {
                assert!(a.is_safe(), "{}: unsafe manifest row {:?}", e.id, a.path);
                assert_eq!(a.sha256.len(), 64, "{}: {} has no sha256", e.id, a.path);
                // Zero-length files are legitimate — Nemotron ships an empty
                // `__init__.py` — but they still carry the digest of the
                // empty string, so nothing is exempt from verification.
                if a.size == 0 {
                    assert_eq!(
                        a.sha256,
                        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                        "{}: {} is empty but does not carry the empty digest",
                        e.id,
                        a.path
                    );
                }
            }
        }
    }

    #[test]
    fn every_model_is_pinned_to_a_commit_not_a_branch() {
        // A manifest that points at a moving branch cannot be verified twice.
        for e in builtin() {
            let rev = e.revision.as_deref().unwrap_or("");
            assert_eq!(rev.len(), 40, "{}: {rev:?} is not a commit", e.id);
            assert!(
                rev.chars().all(|c| c.is_ascii_hexdigit()),
                "{}: {rev:?}",
                e.id
            );
            assert_ne!(rev, "main");
        }
    }

    #[test]
    fn a_models_weights_directory_is_named_by_its_manifest() {
        // SUP-014: two models can never collide, and the same set of files
        // always names the same directory — which is what lets a resumed
        // download resume against the right bytes.
        let names: HashSet<_> = builtin()
            .iter()
            .map(|e| e.manifest_digest.clone().unwrap())
            .collect();
        assert_eq!(names.len(), builtin().len(), "two models share a directory");
        for e in builtin() {
            assert_eq!(
                e.manifest_digest.as_deref().unwrap(),
                Entry::compute_manifest_digest(&e.files),
                "{}: digest does not match its own manifest",
                e.id
            );
        }
    }

    #[test]
    fn the_manifest_digest_does_not_depend_on_file_order() {
        // Otherwise a re-pin that lists files differently would orphan every
        // download the user already has.
        let e = builtin().into_iter().next().unwrap();
        let mut shuffled = e.files.clone();
        shuffled.reverse();
        assert_eq!(
            Entry::compute_manifest_digest(&e.files),
            Entry::compute_manifest_digest(&shuffled)
        );
    }

    #[test]
    fn changing_one_byte_of_one_file_changes_the_directory() {
        let e = builtin().into_iter().next().unwrap();
        let mut tampered = e.files.clone();
        tampered[0].size += 1;
        assert_ne!(
            Entry::compute_manifest_digest(&e.files),
            Entry::compute_manifest_digest(&tampered)
        );
    }

    #[test]
    fn urls_are_built_from_the_pinned_revision() {
        let e = builtin().into_iter().next().unwrap();
        let url = e.file_url(&e.files[0]).unwrap();
        assert!(url.starts_with("https://huggingface.co/"), "{url}");
        assert!(url.contains(e.revision.as_deref().unwrap()), "{url}");
        assert!(
            !url.contains("/main/"),
            "must not resolve against a branch: {url}"
        );
    }

    #[test]
    fn every_generative_model_fits_the_development_machine_at_its_run_context() {
        // The shortlist exists because of the memory budget. A model in it
        // that does not fit is a shortlist that was not checked.
        for e in builtin() {
            let v = assess(
                &m4_air(),
                &e.shape(e.default_context, KvPrecision::F16),
                8_000_000_000,
            );
            assert!(v.offerable(), "{} does not fit: {}", e.id, v.reason);
        }
    }

    #[test]
    fn the_run_context_is_well_below_the_advertised_ceiling() {
        // Qwen3.5-4B advertises 262144. At its real 128 KB/token that is 34 GB
        // of cache. Sizing against the ceiling would reject every model here.
        for e in builtin() {
            assert!(
                e.default_context <= e.context_limit,
                "{}: run context above the ceiling",
                e.id
            );
        }
        let qwen = builtin()
            .into_iter()
            .find(|e| e.id == "qwen3.5-4b-mlx-q4")
            .unwrap();
        assert!(qwen.context_limit >= 131_072, "expected a long ceiling");
        assert_eq!(qwen.default_context, 8192);
    }

    #[test]
    fn kv_per_token_is_measured_for_every_model() {
        // Read from each model's own config, not guessed. The constant
        // fallback exists for detected models, not for these.
        for e in builtin() {
            let kv = e.kv_bytes_per_token.unwrap_or(0);
            assert!(kv > 0, "{} has no measured KV figure", e.id);
            // 2 x layers x kv_heads x head_dim x 2 bytes is always even, and
            // in practice a multiple of 1 KB.
            assert_eq!(kv % 1024, 0, "{}: {kv} is not a plausible KV figure", e.id);
        }
    }

    #[test]
    fn weights_come_from_the_manifest_rather_than_the_quantization_formula() {
        for e in builtin() {
            let declared = e.weights_bytes.expect("must carry a real size");
            assert_eq!(
                declared,
                e.download_bytes(),
                "{}: size disagrees with its files",
                e.id
            );
            assert!(
                declared > 100_000_000,
                "{}: {declared} bytes is implausible",
                e.id
            );
        }
    }

    #[test]
    fn the_shortlist_covers_every_role_the_pipeline_needs() {
        // Router, generator, embedder, tools. A catalogue missing any one of
        // them cannot serve the pipeline in §148 at all.
        let all = builtin();
        assert!(all.iter().any(|e| e.capabilities.embedding), "no embedder");
        assert!(
            all.iter().any(|e| e.capabilities.tools),
            "no tool-calling model"
        );
        assert!(
            all.iter().any(|e| e.capabilities.reasoning),
            "no Thorough-capable model"
        );
        assert!(
            all.iter()
                .any(|e| e.params_b < 1.0 && e.capabilities.structured_output),
            "no small structured-output model to route with"
        );
    }

    #[test]
    fn an_embedding_model_does_not_claim_it_answers_directly() {
        // GEN-013 is about a *disabled* switch. On a model that never answers
        // at all the switch is irrelevant, and saying "answers directly" about
        // an embedder is simply false.
        let e = builtin()
            .into_iter()
            .find(|e| e.capabilities.embedding)
            .unwrap();
        assert_eq!(e.reasoning_unavailable_because(), None);
        let generator = builtin()
            .into_iter()
            .find(|e| !e.capabilities.reasoning && !e.capabilities.embedding)
            .unwrap();
        assert!(generator.reasoning_unavailable_because().is_some());
    }

    #[test]
    fn every_model_says_why_it_is_in_the_list() {
        // Six models with no stated role is a list that grows to forty.
        for e in builtin() {
            assert!(e.role.len() > 20, "{}: {:?} is not a reason", e.id, e.role);
        }
    }

    #[test]
    fn commercial_use_is_unknown_rather_than_assumed_where_it_is_unknown() {
        // LIC-004. `None` renders as "not established", never as yes or no.
        let all = builtin();
        let nemotron = all.iter().find(|e| e.family == "nemotron").unwrap();
        assert_eq!(nemotron.licence.commercial_use, None);
        let granite = all.iter().find(|e| e.family == "granite").unwrap();
        assert_eq!(granite.licence.commercial_use, Some(true));
    }

    #[test]
    fn ids_are_unique_and_name_their_format_and_quantization() {
        let ids: HashSet<_> = builtin().iter().map(|e| e.id.clone()).collect();
        assert_eq!(ids.len(), builtin().len(), "duplicate model id");
        for e in builtin() {
            assert!(e.id.contains("mlx"), "{} must name its format", e.id);
            assert!(e.id.contains("q4"), "{} must name its quantization", e.id);
        }
    }

    #[test]
    fn a_manifest_row_that_escapes_its_directory_is_refused() {
        // The manifest is data from a server. Today we generated it; tomorrow
        // a user-supplied source will use the same shape.
        for bad in ["../etc/passwd", "/etc/passwd", "a/../../b", "", "./x"] {
            let a = Artifact {
                path: bad.into(),
                sha256: "a".repeat(64),
                size: 1,
            };
            assert!(!a.is_safe(), "{bad:?} must be refused");
        }
        assert!(Artifact {
            path: "model.safetensors".into(),
            sha256: "a".repeat(64),
            size: 1,
        }
        .is_safe());
    }

    #[test]
    fn a_short_or_uppercase_digest_is_refused() {
        // A 40-character value is a git SHA-1, not a content digest, and
        // accepting one would verify nothing.
        for sha in ["a".repeat(40), "A".repeat(64), String::new()] {
            let a = Artifact {
                path: "x".into(),
                sha256: sha.clone(),
                size: 1,
            };
            assert!(!a.is_safe(), "{sha:?} must be refused");
        }
    }
}
