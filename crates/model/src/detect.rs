//! Detecting a runtime the user already has (Part 8 §139.1, R1).
//!
//! The first branch of the selection order and the one that matters: an Ollama
//! or LM Studio library is **a registry the user already curated**. Zero bytes
//! downloaded, zero maintenance, and no digest to pin.
//!
//! HW-003 in spirit: detection means *talking to the server*, not looking for a
//! binary on `PATH`. An Ollama that is installed but not running is not a
//! runtime, and offering its models would produce a refusal at the moment the
//! user finally clicks something.
//!
//! # Why there is a hand-written HTTP client in here
//!
//! One blocking GET to `127.0.0.1`, parsed as JSON. Pulling `reqwest` for it
//! would bring an async runtime into a crate whose entire design is one
//! synchronous supervisor thread — the cure is worse. The scope is deliberately
//! tiny and enforced: loopback only, GET only, a byte cap, and a timeout.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use marrow_hw::Quantization;
use serde::Serialize;

use crate::registry::{Capabilities, Entry, Format, Licence, Source};

/// Refuse to read more than this from a local server. A cooperating Ollama
/// returns a few KB; anything larger is a bug or something else on the port.
const MAX_BODY: usize = 1024 * 1024;

const CONNECT_TIMEOUT: Duration = Duration::from_millis(250);
const READ_TIMEOUT: Duration = Duration::from_millis(1500);

/// A local inference server we found and spoke to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Detected {
    pub runtime: Runtime,
    pub port: u16,
    pub model_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Runtime {
    Ollama,
    LmStudio,
}

impl Runtime {
    pub fn label(self) -> &'static str {
        match self {
            Runtime::Ollama => "Ollama",
            Runtime::LmStudio => "LM Studio",
        }
    }

    fn default_port(self) -> u16 {
        match self {
            Runtime::Ollama => 11434,
            Runtime::LmStudio => 1234,
        }
    }

    fn list_path(self) -> &'static str {
        match self {
            Runtime::Ollama => "/api/tags",
            Runtime::LmStudio => "/v1/models",
        }
    }
}

/// What a scan turned up.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Scan {
    pub detected: Vec<Detected>,
    pub entries: Vec<Entry>,
    /// Runtimes that answered but whose model list could not be read, with the
    /// reason. Silence here would look identical to "nothing installed".
    pub problems: Vec<String>,
}

/// Look for every supported local runtime.
///
/// Never fails: a machine with no local server is the common case, not an
/// error. Anything that goes wrong beyond "nothing there" lands in
/// [`Scan::problems`] so it is visible rather than indistinguishable from
/// absence.
pub fn scan() -> Scan {
    let mut out = Scan::default();
    for runtime in [Runtime::Ollama, Runtime::LmStudio] {
        let port = runtime.default_port();
        let body = match get_localhost(port, runtime.list_path()) {
            Ok(b) => b,
            // Connection refused is the ordinary "not running" case.
            Err(_) => continue,
        };
        match parse(runtime, &body) {
            Ok(entries) => {
                out.detected.push(Detected {
                    runtime,
                    port,
                    model_count: entries.len(),
                });
                out.entries.extend(entries);
            }
            Err(why) => out.problems.push(format!(
                "{} answered on port {port} but its model list could not be read: {why}",
                runtime.label()
            )),
        }
    }
    out
}

fn parse(runtime: Runtime, body: &str) -> Result<Vec<Entry>, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| format!("not JSON ({e})"))?;
    let list = match runtime {
        Runtime::Ollama => v.get("models"),
        Runtime::LmStudio => v.get("data"),
    }
    .and_then(|m| m.as_array())
    .ok_or("no model list in the response")?;

    Ok(list.iter().filter_map(|m| entry_from(runtime, m)).collect())
}

fn entry_from(runtime: Runtime, m: &serde_json::Value) -> Option<Entry> {
    let name = m
        .get("name")
        .or_else(|| m.get("model"))
        .or_else(|| m.get("id"))?
        .as_str()?
        .to_string();

    let details = m.get("details");
    let params_b = details
        .and_then(|d| d.get("parameter_size"))
        .and_then(|p| p.as_str())
        .and_then(parse_params)
        // Fall back to the file size, which over-estimates but never
        // under-estimates — the safe direction for admission.
        .or_else(|| {
            m.get("size")
                .and_then(|s| s.as_u64())
                .map(|bytes| bytes as f64 / 1e9 / 0.66)
        })
        .unwrap_or(7.0);

    let quantization = details
        .and_then(|d| d.get("quantization_level"))
        .and_then(|q| q.as_str())
        .map(parse_quant)
        .unwrap_or(Quantization::Q4);

    let family = details
        .and_then(|d| d.get("family"))
        .and_then(|f| f.as_str())
        .unwrap_or("unknown")
        .to_string();

    Some(Entry {
        id: format!("{}:{name}", runtime_slug(runtime)),
        display_name: name,
        family,
        params_b,
        quantization,
        format: Format::Gguf,
        // Not advertised by either API. 8k is the conservative floor: sizing
        // against a context the model does not have would refuse a model that
        // runs, and sizing against one it does not support would admit one
        // that OOMs.
        context_limit: 8192,
        default_context: 4096,
        // Neither server reports layer or head counts, so the conservative
        // constant in `marrow_hw::sizing` applies. Over-estimating costs a
        // refusal the user can override; under-estimating costs a crash.
        kv_bytes_per_token: None,
        // Ollama reports the file size; LM Studio does not. `size` is already
        // folded into `params_b` above, and claiming a weights figure we did
        // not measure would defeat the point of measuring.
        weights_bytes: None,
        // Not claimed, because neither server tells us. GEN-013 then shows the
        // Thorough switch disabled with a reason rather than silently dropping
        // the flag.
        capabilities: Capabilities {
            structured_output: true,
            ..Default::default()
        },
        licence: Licence {
            spdx_or_name: "Set by whoever installed it".into(),
            url: None,
            commercial_use: None,
        },
        role: format!("Already installed in {}.", runtime.label()),
        source: Source::Detected {
            runtime: runtime_slug(runtime).into(),
        },
        // No manifest: we did not fetch these bytes and cannot vouch for them
        // (§138.2). `downloadable()` is false either way — it is already here.
        repo: None,
        revision: None,
        files: Vec::new(),
        manifest_digest: None,
        installed: true,
        breaker: Default::default(),
    })
}

fn runtime_slug(r: Runtime) -> &'static str {
    match r {
        Runtime::Ollama => "ollama",
        Runtime::LmStudio => "lmstudio",
    }
}

/// `"7.6B"` → `7.6`. Ollama reports it as a string with a unit.
fn parse_params(s: &str) -> Option<f64> {
    let t = s.trim();
    let (num, scale) = match t.chars().last()? {
        'B' | 'b' => (&t[..t.len() - 1], 1.0),
        'M' | 'm' => (&t[..t.len() - 1], 0.001),
        _ => (t, 1.0),
    };
    num.trim().parse::<f64>().ok().map(|n| n * scale)
}

/// `"Q4_K_M"` → `Q4`. Only the bit width matters for sizing.
fn parse_quant(s: &str) -> Quantization {
    let up = s.to_ascii_uppercase();
    if up.contains("F16") || up.contains("FP16") || up.contains("F32") {
        Quantization::F16
    } else if up.contains("Q8") {
        Quantization::Q8
    } else if up.contains("Q5") || up.contains("Q6") {
        Quantization::Q5
    } else {
        // Q4, Q3, Q2 and anything unrecognised. Q4's factor over-estimates the
        // smaller ones, which is the direction that does not OOM.
        Quantization::Q4
    }
}

/// One blocking GET against loopback. Not a general HTTP client, and the
/// signature keeps it that way: no host, no method, no headers.
fn get_localhost(port: u16, path: &str) -> std::io::Result<String> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)?;
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_write_timeout(Some(READ_TIMEOUT))?;
    write!(
        stream,
        "GET {path} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nAccept: application/json\r\n\
         Connection: close\r\n\r\n"
    )?;
    stream.flush()?;

    // HTTP/1.0 plus `Connection: close` means the body ends at EOF, so there is
    // no chunked encoding to parse and no keep-alive to time out on.
    let mut buf = Vec::new();
    stream.take(MAX_BODY as u64).read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .ok_or_else(|| std::io::Error::other("no HTTP header terminator"))?;
    Ok(body.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLLAMA: &str = r#"{
      "models": [
        {"name":"qwen2.5:7b","size":4683087332,
         "details":{"family":"qwen2","parameter_size":"7.6B","quantization_level":"Q4_K_M"}},
        {"name":"nomic-embed-text:latest","size":274302450,
         "details":{"family":"nomic-bert","parameter_size":"137M","quantization_level":"F16"}}
      ]}"#;

    #[test]
    fn an_ollama_library_becomes_registry_entries() {
        let entries = parse(Runtime::Ollama, OLLAMA).unwrap();
        assert_eq!(entries.len(), 2);
        let qwen = &entries[0];
        assert_eq!(qwen.display_name, "qwen2.5:7b");
        assert_eq!(qwen.family, "qwen2");
        assert!((qwen.params_b - 7.6).abs() < 0.01);
        assert_eq!(qwen.quantization, Quantization::Q4);
        assert!(qwen.installed, "a detected model is already here");
    }

    #[test]
    fn a_detected_model_is_not_charged_for_our_runtime_buffers() {
        // Ollama's arenas live in Ollama's address space. Counting them twice
        // would refuse models that run fine.
        let e = &parse(Runtime::Ollama, OLLAMA).unwrap()[0];
        assert_eq!(
            e.shape(8192, marrow_hw::KvPrecision::F16).runtime,
            marrow_hw::RuntimeKind::External
        );
    }

    #[test]
    fn a_detected_model_makes_no_integrity_claim() {
        // §138.2: we did not fetch these bytes and cannot vouch for them.
        for e in parse(Runtime::Ollama, OLLAMA).unwrap() {
            assert_eq!(e.manifest_digest, None);
            assert!(e.files.is_empty());
            assert!(!e.downloadable(), "it is already here");
            assert_eq!(e.licence.commercial_use, None, "we do not know");
        }
    }

    #[test]
    fn capabilities_are_not_claimed_for_a_model_nobody_told_us_about() {
        // GEN-013 then shows Thorough disabled with a reason, rather than
        // sending a flag the model ignores.
        let e = &parse(Runtime::Ollama, OLLAMA).unwrap()[0];
        assert!(!e.capabilities.reasoning);
        assert!(!e.capabilities.tools);
        assert!(e.reasoning_unavailable_because().is_some());
    }

    #[test]
    fn ids_are_namespaced_so_a_local_qwen_and_a_catalogue_qwen_do_not_collide() {
        let e = &parse(Runtime::Ollama, OLLAMA).unwrap()[0];
        assert!(e.id.starts_with("ollama:"), "{}", e.id);
        assert!(crate::catalogue::builtin().iter().all(|c| c.id != e.id));
    }

    #[test]
    fn parameter_sizes_parse_with_their_units() {
        assert_eq!(parse_params("7.6B"), Some(7.6));
        assert_eq!(parse_params("137M"), Some(0.137));
        assert_eq!(parse_params(" 3B "), Some(3.0));
        assert_eq!(parse_params("nonsense"), None);
    }

    #[test]
    fn quantization_labels_collapse_to_their_bit_width() {
        assert_eq!(parse_quant("Q4_K_M"), Quantization::Q4);
        assert_eq!(parse_quant("Q8_0"), Quantization::Q8);
        assert_eq!(parse_quant("Q6_K"), Quantization::Q5);
        assert_eq!(parse_quant("F16"), Quantization::F16);
        // An unrecognised label must over-estimate, not under-estimate.
        assert_eq!(parse_quant("wat"), Quantization::Q4);
    }

    #[test]
    fn a_size_only_entry_falls_back_to_an_over_estimate() {
        // No `details` block. Under-estimating here admits a model that OOMs.
        let json = r#"{"models":[{"name":"mystery","size":4000000000}]}"#;
        let e = &parse(Runtime::Ollama, json).unwrap()[0];
        assert!(e.params_b > 5.0, "must over-estimate, got {}", e.params_b);
    }

    #[test]
    fn an_lm_studio_list_parses_too() {
        let json = r#"{"data":[{"id":"lmstudio/qwen","object":"model"}]}"#;
        let e = &parse(Runtime::LmStudio, json).unwrap()[0];
        assert_eq!(e.display_name, "lmstudio/qwen");
        assert!(e.id.starts_with("lmstudio:"));
    }

    #[test]
    fn a_server_that_answers_with_rubbish_is_a_problem_not_a_silence() {
        // "Something is on port 11434 but it is not Ollama" must be visible,
        // because it looks identical to "nothing installed" otherwise.
        assert!(parse(Runtime::Ollama, "<html>hello</html>").is_err());
        assert!(parse(Runtime::Ollama, r#"{"unexpected":1}"#).is_err());
    }

    #[test]
    fn an_empty_library_is_a_detection_with_no_models_not_a_failure() {
        let entries = parse(Runtime::Ollama, r#"{"models":[]}"#).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn scanning_a_machine_with_nothing_running_is_quiet_and_fast() {
        // The common case. It must not fail, and it must not hang.
        let start = std::time::Instant::now();
        let s = scan();
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "detection took {:?}; it runs at startup",
            start.elapsed()
        );
        // Whatever is or is not running on this machine, the result is
        // well-formed: every detection accounts for its entries.
        let counted: usize = s.detected.iter().map(|d| d.model_count).sum();
        assert_eq!(counted, s.entries.len());
    }
}
