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

use marrow_core::{Code, ProvenanceClass, Result, SourceSpan};
use marrow_model::envelope::{Builder, Envelope, Evidence, Role, Turn};

use crate::models::Conversation;
use marrow_model::provider::Token;
use marrow_model::queue::Cancel;
use serde::Serialize;

use crate::models::Hub;
use crate::state::Core;
use crate::state::RetrievedChunk;

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
    convo: &mut Conversation,
) -> Result<(Envelope, Vec<Citation>, Vec<Excluded>)> {
    let fresh = core.retrieve(&retrieval_terms(question), MAX_CHUNKS)?;
    let chunks = carry_forward(&mut convo.sent, fresh);

    let mut builder = Builder::new(SYSTEM, question);
    let mut citations = Vec::new();
    let mut excluded = Vec::new();

    for (i, hit) in chunks.iter().enumerate() {
        let id = format!("E{}", i + 1);
        // Invariant #9 is enforced inside the envelope, but the *reason* has
        // to be collected here where the path is still known.
        let citable = hit.origin.can_support_a_claim();
        if !citable {
            excluded.push(Excluded {
                relative_path: hit.relative_path.clone(),
                reason: "written by Marrow itself, so it cannot support a claim".into(),
            });
        }
        builder = builder.evidence(evidence_from(&id, hit));
        if citable {
            citations.push(Citation {
                id,
                path: hit.path.clone(),
                relative_path: hit.relative_path.clone(),
                location: hit.location.clone(),
                line: hit.line,
                excerpt: preview(&hit.text),
                provenance: provenance_label(hit.provenance),
            });
        }
    }

    Ok((
        builder
            .history(history.iter().cloned())
            .finish(&mut convo.session),
        citations,
        excluded,
    ))
}

fn evidence_from(id: &str, hit: &RetrievedChunk) -> Evidence {
    Evidence {
        id: id.to_string(),
        text: hit.text.clone(),
        source: hit.relative_path.clone(),
        span: match hit.line {
            Some(l) => SourceSpan::Lines { start: l, end: l },
            None => SourceSpan::Whole,
        },
        provenance: hit.provenance,
        external: false,
        origin: hit.origin,
    }
}

fn provenance_label(p: ProvenanceClass) -> String {
    match p {
        ProvenanceClass::Exact => "exact",
        ProvenanceClass::Degraded => "degraded",
        ProvenanceClass::Approximate => "approximate",
        ProvenanceClass::MetadataOnly => "metadata_only",
    }
    .to_string()
}

/// What the citation list shows. The model gets the whole chunk; the reader
/// gets enough to recognise it.
fn preview(text: &str) -> String {
    const MAX: usize = 220;
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= MAX {
        return flat;
    }
    let cut: String = flat.chars().take(MAX).collect();
    format!("{}…", cut.trim_end())
}

/// Keep the evidence already sent, and append what is new.
///
/// Retrieval is question-dependent, so a follow-up that simply re-retrieves
/// produces a different evidence set — and a prompt whose prefix moves reuses
/// nothing. Measured before this existed: zero of 552 tokens on the second
/// turn of a conversation about the same document.
///
/// Order never changes, because reordering is the same as replacing. A chunk
/// this turn's retrieval returns again is refreshed in place, so an edit
/// between turns is picked up; one it does not return is kept as it was.
/// That is the right reading of a conversation — a carried chunk is what the
/// two of you were already looking at, and it was shown with a citation at the
/// time — but it does mean a file edited between turns can still be quoted
/// from the copy that was retrieved, until it is retrieved again.
fn carry_forward(
    carried: &mut Vec<RetrievedChunk>,
    fresh: Vec<RetrievedChunk>,
) -> Vec<RetrievedChunk> {
    // A conversation that ranges widely would otherwise grow its prompt
    // forever. Past the cap the set is rebuilt from this turn's retrieval:
    // one cache miss, and then stable again.
    if carried.len() >= MAX_CARRIED {
        carried.clear();
    }

    let mut by_location: std::collections::HashMap<String, RetrievedChunk> =
        fresh.into_iter().map(|c| (c.location.clone(), c)).collect();

    for slot in carried.iter_mut() {
        if let Some(refreshed) = by_location.remove(&slot.location) {
            *slot = refreshed;
        }
    }
    // Whatever is left is new to this conversation, in retrieval order.
    let mut added: Vec<RetrievedChunk> = by_location.into_values().collect();
    added.sort_by(|a, b| a.location.cmp(&b.location));
    carried.extend(added);
    carried.clone()
}

/// Past this many carried chunks the set is rebuilt. Twice the per-turn budget:
/// enough for a conversation that wanders a little, short of one that wanders
/// into a prompt nobody can afford.
const MAX_CARRIED: usize = MAX_CHUNKS * 2;

/// Words too common to help, and common enough to hurt.
///
/// BM25 already gives them almost no weight, so this is not about scoring —
/// it is about the term cap: a long question full of "the" and "of" can hit
/// the index's limit and be refused outright, and the words it drops for that
/// are the ones that mattered.
const STOPWORDS: &[&str] = &[
    "a", "an", "the", "and", "or", "but", "if", "of", "in", "on", "at", "to", "for", "with", "is",
    "are", "was", "were", "be", "been", "am", "do", "does", "did", "have", "has", "had", "what",
    "when", "where", "who", "whom", "which", "why", "how", "that", "this", "these", "those", "it",
    "its", "as", "by", "from", "my", "our", "me", "i", "you", "your", "can", "could", "would",
    "should", "will", "shall", "may", "might", "about", "please", "tell",
    // What is left of a contraction after the apostrophe splits it. "What's"
    // becomes "what" and "s", and the "s" retrieves nothing but noise.
    "s", "t", "d", "m", "ll", "re", "ve",
];

/// A question, reduced to the words worth retrieving on.
///
/// Not a router — that comes later and will rewrite the query properly
/// (ASK-001). This is the floor beneath it, and the floor has to work on its
/// own, because ASK-004 says a broken router degrades to the product that
/// already worked.
fn retrieval_terms(question: &str) -> String {
    let kept: Vec<&str> = question
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        // `--` splits into a token of hyphens under this rule; a term with no
        // letter or digit in it is punctuation, and the index refuses those.
        .filter(|w| w.chars().any(|c| c.is_alphanumeric()))
        .filter(|w| !STOPWORDS.contains(&w.to_ascii_lowercase().as_str()))
        .collect();
    // A question made entirely of stopwords is still a question. Searching for
    // nothing would refuse; searching for the original at least tries.
    if kept.is_empty() {
        question.to_string()
    } else {
        kept.join(" ")
    }
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
    let mut convo = hub.session_for(conversation);

    let assembled = assemble(core, question, history, &mut convo);
    hub.keep_session(conversation, convo);

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

    use marrow_core::Origin;

    fn chunk(location: &str, text: &str, citable: bool) -> RetrievedChunk {
        let rel = location.split(':').next().unwrap_or(location);
        RetrievedChunk {
            path: format!("/root/{rel}"),
            relative_path: rel.into(),
            location: location.into(),
            line: Some(1),
            text: text.into(),
            provenance: ProvenanceClass::Exact,
            origin: if citable {
                Origin::User
            } else {
                Origin::SelfWritten
            },
        }
    }

    #[test]
    fn an_evidence_block_carries_the_whole_chunk_not_the_rows_two_lines() {
        // The first end-to-end run produced evidence blocks containing a path
        // and no text, because the row's excerpt is shaped for a result list.
        // The model reported it correctly and the answer was useless.
        let long = (1..=40)
            .map(|i| format!("line {i} of the agreement"))
            .collect::<Vec<_>>()
            .join("\n");
        let e = evidence_from("E1", &chunk("lease.md", &long, true));
        assert_eq!(e.text, long, "the whole chunk must reach the model");
        assert!(e.text.lines().count() > 2);
        // And the citation list shows enough to recognise, not the whole thing.
        assert!(preview(&long).len() < long.len());
        assert!(preview(&long).ends_with('…'));
    }

    #[test]
    fn a_self_written_file_is_excluded_and_the_reason_is_kept() {
        // Invariant #9. The envelope drops it; this layer is where the path is
        // still known, so it is where the reason has to be collected.
        let e = evidence_from("E1", &chunk("notes.md", "as I said", false));
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
        let with = evidence_from("E1", &chunk("a.md", "x", true));
        assert_eq!(with.span, SourceSpan::Lines { start: 1, end: 1 });
        let mut no_line = chunk("a.md", "x", true);
        no_line.line = None;
        assert_eq!(evidence_from("E1", &no_line).span, SourceSpan::Whole);
    }

    #[test]
    fn provenance_survives_the_trip_to_the_envelope() {
        // It drives the citation badge; defaulting everything to `exact` would
        // make a degraded extraction look like a quotation.
        let mut h = chunk("a.pdf", "x", true);
        h.provenance = ProvenanceClass::Degraded;
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
    fn a_follow_up_keeps_the_earlier_evidence_in_place_and_appends() {
        // The prefix must not move. Before this, the second turn of a
        // conversation about one document reused zero of 552 tokens.
        let mut carried = Vec::new();
        let first = carry_forward(&mut carried, vec![chunk("lease.md:1", "renews 2031", true)]);
        assert_eq!(first.len(), 1);

        let second = carry_forward(
            &mut carried,
            vec![chunk("handbook.md:4", "deliveries 07:00", true)],
        );
        assert_eq!(second.len(), 2);
        assert_eq!(
            second[0].location, "lease.md:1",
            "the earlier chunk keeps its place"
        );
        assert_eq!(second[1].location, "handbook.md:4");
    }

    #[test]
    fn a_chunk_retrieved_again_is_refreshed_in_place_rather_than_duplicated() {
        // An edit between turns should be picked up; appending a second copy
        // would both waste the budget and put two versions in front of the
        // model at once.
        let mut carried = Vec::new();
        carry_forward(
            &mut carried,
            vec![chunk("lease.md:1", "rent is 2,400", true)],
        );
        let out = carry_forward(
            &mut carried,
            vec![chunk("lease.md:1", "rent is 2,417", true)],
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "rent is 2,417");
    }

    #[test]
    fn a_wandering_conversation_rebuilds_rather_than_growing_without_bound() {
        // One cache miss beats a prompt nobody can afford.
        let mut carried = Vec::new();
        for i in 0..MAX_CARRIED {
            carry_forward(&mut carried, vec![chunk(&format!("f{i}.md:1"), "x", true)]);
        }
        assert_eq!(carried.len(), MAX_CARRIED);
        let out = carry_forward(&mut carried, vec![chunk("new.md:1", "y", true)]);
        assert_eq!(out.len(), 1, "the set was rebuilt from this turn");
    }

    #[test]
    fn a_question_is_reduced_to_the_words_worth_retrieving_on() {
        assert_eq!(
            retrieval_terms("When does the lease renew and what is the rent?"),
            "lease renew rent"
        );
        assert_eq!(retrieval_terms("Where is my invoice?"), "invoice");
    }

    #[test]
    fn punctuation_and_case_do_not_survive_into_the_query() {
        // The index refuses a query of pure punctuation, so a question ending
        // in "?!" must not carry it through.
        assert_eq!(
            retrieval_terms("What's the rent -- exactly?!"),
            "rent exactly"
        );
    }

    #[test]
    fn a_question_made_only_of_stopwords_still_searches_for_something() {
        // Reducing it to nothing would turn a strange question into a refusal
        // rather than an empty result, and those read very differently.
        assert_eq!(retrieval_terms("what is it?"), "what is it?");
    }

    #[test]
    fn the_chunk_budget_is_in_the_documented_range() {
        // ASK-003: fewer starves the answer, more dilutes it.
        assert!((5..=15).contains(&MAX_CHUNKS));
    }
}
