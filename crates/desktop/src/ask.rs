//! The ask pipeline (Part 8 §148).
//!
//! ```text
//!   question
//!      ↓
//!   search · metadata            §113 fusion
//!      ↓
//!   top 5–15 chunks              ASK-003
//!      ↓
//!   the context envelope         §114 — every chunk labelled untrusted
//!      ↓
//!   4B answer                    streamed, token by token
//!      ↓
//!   answer + citations
//! ```
//!
//! ASK-004: **a broken router degrades to the product that already worked.**
//! There is no router here yet, so retrieval is plain search over the question
//! — which is exactly the fallback the finished pipeline keeps.

use std::sync::Arc;

use marrow_core::{Code, Origin, ProvenanceClass, Result, SourceSpan};
use marrow_model::envelope::{Builder, Envelope, Evidence, Role, Session, Turn};
use marrow_model::provider::Token;
use marrow_model::queue::Cancel;
use serde::Serialize;

use crate::commands::SearchHit;
use crate::models::Hub;
use crate::state::Core;

/// How many chunks reach the model (ASK-003). Fewer starves the answer; more
/// dilutes it and blows the context budget §114 exists to protect.
const MAX_CHUNKS: usize = 12;

/// The runtime template. Assembled here, in the binary — **no retrieved text
/// ever reaches it** (§114.1).
const SYSTEM: &str = "\
You are Marrow, answering questions about the user's own files. Answer only \
from the evidence blocks provided. Every claim must cite the block it came \
from, like [E1]. If the evidence does not contain the answer, say so plainly \
rather than guessing. Use Markdown. When a diagram would be clearer than \
prose, write a ```mermaid block.";

/// What the window receives while an answer is being produced.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum AskEvent {
    /// Retrieval finished. Sent before the first token so the citation list
    /// can render while the model is still thinking.
    Sources {
        hits: Vec<Citation>,
        /// What was retrieved but not sent, and why. §114 drops SELF-written
        /// content; saying nothing would look like it was never found.
        excluded: Vec<Excluded>,
        /// UX-013: what left the device, even when the answer is local.
        bytes: usize,
        distinct_sources: usize,
        /// UX-012: local, private or cloud, stated for every generation.
        boundary: String,
        model: String,
    },
    /// Part of the answer.
    Token {
        text: String,
    },
    /// Part of the model's reasoning (GEN-014). Rendered collapsed, never
    /// cited (GEN-015).
    Thinking {
        text: String,
    },
    Done {
        prompt_tokens: u32,
        output_tokens: u32,
        thinking_tokens: u32,
        cached_prefix_tokens: u32,
        /// `stop` | `length` | `cancelled`. A truncated answer must be
        /// labelled, not presented as complete.
        stop_reason: String,
        elapsed_ms: u64,
    },
    Failed {
        code: String,
        message: String,
    },
}

/// An earlier exchange, as the window sends it back.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PriorTurn {
    /// `user` or `assistant`. Anything else is treated as the model's own
    /// words, which is the conservative reading.
    pub role: String,
    pub text: String,
}

/// Convert what the window sent into envelope turns.
pub fn turns_from(history: &[PriorTurn]) -> Vec<Turn> {
    history
        .iter()
        .map(|t| Turn {
            role: if t.role == "user" {
                Role::User
            } else {
                Role::Assistant
            },
            text: t.text.clone(),
        })
        .collect()
}

/// One evidence block, as the UI shows it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Citation {
    pub id: String,
    pub path: String,
    pub relative_path: String,
    pub location: String,
    pub line: Option<u32>,
    pub excerpt: String,
    pub provenance: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Excluded {
    pub relative_path: String,
    pub reason: String,
}

/// Retrieve, assemble, and hand back the envelope plus what the UI must show.
///
/// Separated from generation so the whole assembly — including the ordering
/// and the exclusion rules — is testable without a model.
pub fn assemble(
    core: &Core,
    question: &str,
    history: &[Turn],
    session: &mut Session,
) -> Result<(Envelope, Vec<Citation>, Vec<Excluded>)> {
    let response = core.search(question, MAX_CHUNKS)?;

    let mut builder = Builder::new(SYSTEM, question);
    let mut citations = Vec::new();
    let mut excluded = Vec::new();

    for (i, hit) in response.hits.iter().enumerate() {
        let id = format!("E{}", i + 1);
        // Invariant #9 is enforced inside the envelope, but the *reason* has
        // to be collected here where the path is still known.
        if !hit.citable {
            excluded.push(Excluded {
                relative_path: hit.relative_path.clone(),
                reason: "written by Marrow itself, so it cannot support a claim".into(),
            });
        }
        builder = builder.evidence(evidence_from(&id, hit));
        if hit.citable {
            citations.push(Citation {
                id,
                path: hit.path.clone(),
                relative_path: hit.relative_path.clone(),
                location: hit.location.clone(),
                line: hit.line,
                excerpt: strip_markers(&hit.excerpt),
                provenance: hit.provenance.clone(),
            });
        }
    }

    Ok((
        builder.history(history.iter().cloned()).finish(session),
        citations,
        excluded,
    ))
}

fn evidence_from(id: &str, hit: &SearchHit) -> Evidence {
    Evidence {
        id: id.to_string(),
        text: strip_markers(&hit.excerpt),
        source: hit.relative_path.clone(),
        span: match hit.line {
            Some(l) => SourceSpan::Lines { start: l, end: l },
            None => SourceSpan::Whole,
        },
        provenance: match hit.provenance.as_str() {
            "degraded" => ProvenanceClass::Degraded,
            "approximate" => ProvenanceClass::Approximate,
            "metadata_only" => ProvenanceClass::MetadataOnly,
            _ => ProvenanceClass::Exact,
        },
        external: false,
        origin: if hit.citable {
            Origin::User
        } else {
            Origin::SelfWritten
        },
    }
}

/// The search layer marks matches with U+0001/U+0002 for the UI to highlight.
/// Those are for the human, not the model — sending them would be sending a
/// rendering artefact and asking the model to reason about it.
fn strip_markers(s: &str) -> String {
    s.chars()
        .filter(|c| *c != '\u{1}' && *c != '\u{2}')
        .collect()
}

/// Run one question end to end, streaming to `emit`.
#[allow(clippy::too_many_arguments)] // Each is a distinct input the window
                                     // has; a struct would move the list rather than shorten it.
pub fn run(
    core: &Arc<Core>,
    hub: &Arc<Hub>,
    conversation: &str,
    question: &str,
    history: &[Turn],
    thorough: bool,
    cancel: &Cancel,
    emit: &mut dyn FnMut(AskEvent),
) {
    let started = std::time::Instant::now();
    // One session per conversation, so the delimiter — and therefore the whole
    // preamble — is byte-identical across turns and the KV prefix cache has
    // something to reuse. A fresh session per question reused 3% of the prompt;
    // a shared one reuses about 80%.
    let mut session = hub.session_for(conversation);

    let assembled = assemble(core, question, history, &mut session);
    hub.keep_session(conversation, session);

    let (envelope, citations, excluded) = match assembled {
        Ok(v) => v,
        Err(e) => {
            emit(AskEvent::Failed {
                code: e.code().as_str().into(),
                message: e.message().into(),
            });
            return;
        }
    };

    let generator = match hub.generator() {
        Some(g) => g,
        None => {
            emit(AskEvent::Failed {
                code: Code::ModNotInstalled.as_str().into(),
                message: no_generator_message(hub),
            });
            return;
        }
    };

    emit(AskEvent::Sources {
        hits: citations,
        excluded,
        bytes: envelope.disclosure.bytes,
        distinct_sources: envelope.disclosure.distinct_sources,
        boundary: "local".into(),
        model: generator.clone(),
    });

    match hub.generate(&generator, &envelope, thorough, cancel, &mut |t| match t {
        Token::Text(text) => emit(AskEvent::Token { text }),
        Token::Thinking(text) => emit(AskEvent::Thinking { text }),
    }) {
        Ok(c) => emit(AskEvent::Done {
            prompt_tokens: c.usage.prompt_tokens,
            output_tokens: c.usage.output_tokens,
            thinking_tokens: c.usage.thinking_tokens,
            cached_prefix_tokens: c.usage.cached_prefix_tokens,
            stop_reason: format!("{:?}", c.stop_reason).to_lowercase(),
            elapsed_ms: started.elapsed().as_millis() as u64,
        }),
        Err(e) => emit(AskEvent::Failed {
            code: e.code().as_str().into(),
            message: e.message().into(),
        }),
    }
}

/// Why there is nothing to answer with, and what to do about it.
fn no_generator_message(hub: &Hub) -> String {
    let s = hub.snapshot();
    if !s.runtime_ready {
        "No inference runtime is installed, so questions cannot be answered yet. \
         Search still works. The Models page has the two commands that install one."
            .into()
    } else {
        "No model is installed. Download one from the Models page — the \
         recommended one is about 3 GB."
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(rel: &str, excerpt: &str, citable: bool) -> SearchHit {
        SearchHit {
            rank: 1,
            path: format!("/root/{rel}"),
            relative_path: rel.into(),
            location: format!("{rel}:1"),
            line: Some(1),
            breadcrumb: String::new(),
            excerpt: excerpt.into(),
            provenance: "exact".into(),
            reason: "exact".into(),
            citable,
            modified_ms: 0,
            file_id: "01ABC".into(),
        }
    }

    #[test]
    fn highlight_markers_never_reach_the_model() {
        // They are a rendering artefact for the human. Sending them asks the
        // model to reason about control characters.
        let e = evidence_from("E1", &hit("a.md", "the \u{1}lease\u{2} renews", true));
        assert_eq!(e.text, "the lease renews");
        assert!(!e.text.contains('\u{1}'));
    }

    #[test]
    fn a_self_written_file_is_excluded_and_the_reason_is_kept() {
        // Invariant #9. The envelope drops it; this layer is where the path is
        // still known, so it is where the reason has to be collected.
        let e = evidence_from("E1", &hit("notes.md", "as I said", false));
        assert_eq!(e.origin, Origin::SelfWritten);
        assert!(!e.origin.can_support_a_claim());
    }

    #[test]
    fn the_system_prompt_is_a_template_and_contains_no_retrieved_text() {
        // §114.1, and the thing that is easy to break by accident later.
        assert!(SYSTEM.contains("Answer only from the evidence"));
        assert!(
            SYSTEM.contains("say so plainly"),
            "must license 'I don't know'"
        );
        assert!(
            SYSTEM.contains("mermaid"),
            "diagrams are part of the answer format"
        );
    }

    #[test]
    fn a_line_becomes_a_span_and_a_missing_line_does_not_pretend() {
        // Invariant #1. `Whole` is honest; a fabricated line number is not.
        let with = evidence_from("E1", &hit("a.md", "x", true));
        assert_eq!(with.span, SourceSpan::Lines { start: 1, end: 1 });
        let mut no_line = hit("a.md", "x", true);
        no_line.line = None;
        assert_eq!(evidence_from("E1", &no_line).span, SourceSpan::Whole);
    }

    #[test]
    fn provenance_survives_the_trip_to_the_envelope() {
        // It drives the citation badge; defaulting everything to `exact` would
        // make a degraded extraction look like a quotation.
        let mut h = hit("a.pdf", "x", true);
        h.provenance = "degraded".into();
        assert_eq!(
            evidence_from("E1", &h).provenance,
            ProvenanceClass::Degraded
        );
    }

    #[test]
    fn an_unknown_role_is_read_as_the_models_own_words() {
        // The conservative direction: an assistant turn cannot support a
        // claim, so mislabelling one as `user` would promote it.
        let turns = turns_from(&[
            PriorTurn {
                role: "user".into(),
                text: "q".into(),
            },
            PriorTurn {
                role: "assistant".into(),
                text: "a".into(),
            },
            PriorTurn {
                role: "wat".into(),
                text: "?".into(),
            },
        ]);
        assert_eq!(turns[0].role, Role::User);
        assert_eq!(turns[1].role, Role::Assistant);
        assert_eq!(
            turns[2].role,
            Role::Assistant,
            "unknown must not become User"
        );
    }

    #[test]
    fn the_chunk_budget_is_in_the_documented_range() {
        // ASK-003: fewer starves the answer, more dilutes it.
        assert!((5..=15).contains(&MAX_CHUNKS));
    }
}
