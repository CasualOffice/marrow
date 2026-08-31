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

use marrow_core::{ProvenanceClass, Result, SourceSpan};
use marrow_model::envelope::{Builder, Envelope, Evidence, Fact, Role, Turn};

use crate::models::Conversation;
use marrow_model::provider::StreamEvent;
use marrow_model::queue::Cancel;
use serde::Serialize;

use crate::models::Hub;
use crate::state::Core;
use crate::state::RetrievedChunk;

/// How many chunks reach the model (ASK-003). Fewer starves the answer; more
/// dilutes it and blows the context budget §114 exists to protect.
const MAX_CHUNKS: usize = 12;

/// The most prompt the evidence may occupy, in bytes.
///
/// **A count of chunks is not a bound on a context window.** Twelve chunks of
/// a dense document is a very different prompt from twelve chunks of a sparse
/// one, and the reported truncation came from exactly that: retrieval produced
/// 29 KB, the answer budget subtracted it from the window, and what was left
/// was clamped up to a floor the model then overran mid-sentence.
///
/// Bounded in bytes because bytes are what we have before the worker has
/// tokenized anything. Four bytes per token is conservative for code and
/// markup, so 16 KB is about 4,000 tokens — half of the 8,192 the runtime
/// plans around, which leaves the other half for the answer and the thinking.
const MAX_EVIDENCE_BYTES: usize = 16 * 1024;

/// The runtime template. Assembled here, in the binary — **no retrieved text
/// ever reaches it** (§114.1).
/// The instructions. **No retrieved text ever reaches this** (§114.1).
///
/// The first version said "Answer only from the evidence blocks provided", and
/// that sentence made the model refuse work it could do. Asked to "generate an
/// HTML page about pitching STT to our clients" — over a corpus that documents
/// the STT service in detail — it replied that the evidence contained nothing
/// about *pitching* and declined. It had read the word "generate an HTML page
/// about X" as a claim needing support, rather than as an instruction about the
/// shape of the answer.
///
/// So the two are now separated explicitly. **What may be asserted** about the
/// user's files comes from the evidence and is cited. **What form the answer
/// takes** — prose, a table, a diagram, a page, a pitch — is the user's to
/// choose, and choosing it is not a claim about anything. Presenting what the
/// evidence says persuasively is still only saying what the evidence says;
/// inventing a capability it does not mention is not, and that is the line the
/// model has to hold.
const SYSTEM: &str = "\
You are Marrow. You answer from the user's own files.

**Facts come from the evidence.** Every statement about what the user's files, \
projects or systems contain must come from an evidence block and cite it, like \
[E1]. Never invent a detail, a capability or a number that is not there. If the \
evidence does not contain what was asked for, say exactly which part is missing.

**The form of the answer is the user's instruction, not a claim.** If they ask \
for a page, a table, a diagram, a summary, a plan or a pitch, produce that \
thing, built from the evidence you have. Asking for a different shape is never \
a reason to refuse: shaping and presenting what the evidence says is still \
saying what the evidence says. Refuse only when the *facts* are absent, and \
then name the facts rather than the format.

**A citation is a mark, not a subject.** Write `[E1]` after the sentence it \
supports and nothing more. Never write *about* the evidence blocks — not \
\"E24 mentions\", not \"according to E9\", not a table with a column of block \
ids. The reader chose these files; they do not know what E24 is and have no \
reason to care. Name the file, or say the thing, and cite it.

**Answer, do not narrate.** No \"the user is asking me to\", no \"let me search \
through the evidence\", no plan for the answer before the answer. Begin with \
the answer itself. Deliberating on the page spends the whole reply on \
preamble and leaves the reader nothing.

Use Markdown. Write a ```mermaid block when a diagram is clearer than prose, \
and a ```html block when asked for a page — both are rendered for the user.";

/// What the window receives while an answer is being produced.
///
/// **`rename_all_fields`, not just `rename_all`.** On an enum, `rename_all`
/// renames the *variants*; the fields inside them keep their Rust spelling.
/// The window reads camelCase, so every multi-word field arrived as
/// `undefined` and every answer's footer read `tokens in NaNm NaNs`. Nothing
/// failed and nothing logged — the values simply were not there.
#[derive(Clone, Debug, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
pub enum AskEvent {
    /// The handle `cancel_ask` needs, sent before anything else happens.
    ///
    /// It used to reach the window only as the command's **return value**,
    /// which resolves when the answer is finished — so for the entire time the
    /// Stop button was on screen there was nothing for it to cancel, and both
    /// it and Esc did nothing at all. A cancel handle that arrives after the
    /// work it cancels is not a handle.
    Started {
        id: String,
    },
    /// What the pipeline is doing right now.
    ///
    /// Between pressing Enter and the first token there is retrieval, possibly
    /// a multi-second model load, and a prefill — and until this event existed
    /// the window showed nothing at all for all of it. "It feels slow" is what
    /// a system with no progress looks like even when it is not slow, and the
    /// first question of a session genuinely does load several gigabytes.
    Stage {
        stage: String,
        detail: String,
    },
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
        /// The distinct projects the evidence came from, as folder names
        /// relative to the workspace root. More than one means the answer is
        /// stitched together across unrelated bodies of work, and the reader
        /// has to be told: "what is STT?" was answered from the STT service,
        /// an MFA setting and a code of conduct, and read as one coherent
        /// account of a single thing.
        projects: Vec<String>,
        /// UX-012: local, private or cloud, stated for every generation
        /// (LLM-034). `local` · `private` · `cloud`.
        boundary: String,
        /// The same fact in the words the user reads, so the window never has
        /// to keep its own copy of them.
        boundary_label: String,
        /// The host the excerpts are being sent to, or `null` when they are
        /// not being sent anywhere (LLM-033).
        destination: Option<String>,
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
    /// Something the provider needed to say that is not part of the answer and
    /// is not a failure — a setting it could not honour, a frame it could not
    /// read, an account limit about to bite. Rendered beside the answer, which
    /// keeps arriving: a warning that ended the stream would be an error
    /// wearing a friendlier name.
    Notice {
        message: String,
        /// The §108 class when one fits, and absent when none does — §108
        /// classifies failures and this is not one.
        code: Option<String>,
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
    /// Whether this answer stopped because it ran out of budget rather than
    /// because it was finished.
    ///
    /// The runtime has always known: `stop_reason` is computed, sent to the
    /// window, and printed in the footer the user reads — "cut off at the token
    /// limit". It was then dropped on the floor. The next turn carried the
    /// truncated text with nothing marking it as truncated, so the model read
    /// its own half-finished answer as a finished one, and "continue" produced
    /// a fresh preamble and started the whole thing again. Three times in a
    /// row, in the reported case, each one cut off at the same place.
    ///
    /// `default` because conversations persisted before this existed have no
    /// such field, and the safe reading of an unknown answer is "complete".
    #[serde(default)]
    pub truncated: bool,
}

/// Whether this question is resuming an answer that was cut off.
///
/// True only when the turn immediately before it is an assistant turn that ran
/// out of budget. Anything earlier is history the model has already moved past,
/// and a truncation three questions ago is not what "continue" means.
pub fn resuming(history: &[PriorTurn]) -> bool {
    history
        .last()
        .is_some_and(|t| t.role == "assistant" && t.truncated)
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
///
/// `Deserialize` as well, because a conversation is persisted with the
/// citations it was answered from and the window sends them back verbatim when
/// the turn finishes. Re-deriving them at save time would risk storing
/// something the reader was never shown.
#[derive(Clone, Debug, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Citation {
    pub id: String,
    pub path: String,
    pub relative_path: String,
    pub location: String,
    pub line: Option<u32>,
    /// Where in the file, structurally — not just its rendering.
    ///
    /// `line` cannot express a PDF page, a bounding box or a spreadsheet cell,
    /// and those are the citations this product is for. The window renders the
    /// page or the cell from this today; the bbox is here so that showing the
    /// region itself does not need another trip through the boundary.
    pub span: marrow_core::SourceSpan,
    pub excerpt: String,
    pub provenance: String,
}

#[derive(Clone, Debug, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Excluded {
    pub relative_path: String,
    pub reason: String,
}

/// What the runtime knows about itself, as FACT blocks.
///
/// These go in as facts rather than as evidence because they are deterministic
/// knowledge this program has about its own state. The evidence blocks are the
/// user's files, which describe *their* projects and not this program — which
/// is why "what model are you using?" was once answered out of the corpus, with
/// GPT-4 and Llama-3 offered as guesses while the footer named the real one.
///
/// Grouped because they are one idea and because two more loose parameters on
/// `assemble` is how a signature stops being readable.
#[derive(Clone, Debug, Default)]
pub struct RuntimeFacts {
    /// What this runtime is: the model answering, and where it runs.
    pub identity: Option<String>,
    /// The answer before this one stopped at its token limit. The model cannot
    /// know this — from inside the history a truncated turn looks exactly like
    /// a finished one — so the runtime has to say it.
    pub resuming: bool,
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
    embedding: Option<&marrow_index::Embedding>,
    runtime: RuntimeFacts,
    // `scope`: a subtree, relative to the workspace root, the answer is
    // confined to. A workspace is routinely one folder holding many unrelated
    // projects, and there was no way to say which one a question was about.
    scope: Option<&str>,
) -> Result<(Envelope, Vec<Citation>, Vec<Excluded>)> {
    // `retrieve` reduces the question for the lexical branch itself; the
    // embedding is of the question as asked, because the words that carry no
    // lexical weight still carry meaning.
    let fresh = core.retrieve(question, MAX_CHUNKS, embedding, scope)?;
    if let Some(fragment) = crate::state::scope_fragment(scope) {
        // The scope governs the whole prompt, not only this turn's retrieval.
        // Evidence carried over from an unscoped question would otherwise sit
        // in front of the model exactly as if it had been retrieved, and a
        // scoped follow-up would go on answering from the project the user had
        // just said they did not mean. It costs a cache miss on the turn the
        // scope changes, which is honest: the prompt genuinely is different.
        convo.sent.retain(|c| c.path.contains(&fragment));
    }
    let chunks = carry_forward(&mut convo.sent, fresh);

    let mut builder = Builder::new(SYSTEM, question);
    let mut facts = 0;
    if let Some(text) = runtime.identity {
        facts += 1;
        builder = builder.fact(Fact {
            id: format!("F{facts}"),
            text,
            source: "the running system".into(),
            span: None,
        });
    }
    if runtime.resuming {
        // The model cannot tell from inside that its last turn was cut off: a
        // truncated answer and a finished one look identical in the history. So
        // it read its own half-written page as complete, and "continue"
        // produced another introduction and started over — three times, each
        // stopping at the same place.
        facts += 1;
        builder = builder.fact(Fact {
            id: format!("F{facts}"),
            text: "Your previous answer stopped because it reached its token limit, not \
                   because it was complete. Resume from exactly where it stopped. Do not \
                   re-introduce the topic, restate what you already wrote, or begin again."
                .into(),
            source: "the running system".into(),
            span: None,
        });
    }
    let mut citations = Vec::new();
    let mut excluded = Vec::new();

    let mut spent = 0usize;
    for (i, hit) in chunks.iter().enumerate() {
        // Stop before the evidence eats the window rather than after. Dropping
        // the tail is right because retrieval already ranked it: what goes is
        // what mattered least, and saying so is what stops a short answer
        // looking like the model had nothing to say.
        if spent + hit.text.len() > MAX_EVIDENCE_BYTES && i > 0 {
            excluded.push(Excluded {
                relative_path: hit.relative_path.clone(),
                reason: "the context window was full; this ranked below what was sent".into(),
            });
            continue;
        }
        spent += hit.text.len();
        let id = format!("E{}", i + 1);
        // The `origin = SELF` rule is enforced inside the envelope, but the *reason* has
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
                span: hit.span.clone(),
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

/// The distinct projects a set of citations came from.
///
/// A workspace is one granted folder, and this one is `~/Desktop/melp` — many
/// unrelated services under it. Answering across all of them is sometimes
/// right and sometimes nonsense, and the reader is the only one who can tell
/// which; they can only tell if they are told.
///
/// **How deep a project is cannot be fixed in advance.** Under `~/Desktop/melp`
/// every path begins `services/`, so the first segment names the container and
/// distinguishes nothing; under a folder of sibling projects the first segment
/// is exactly right and a second would name a source directory. So the depth is
/// chosen by what actually separates the paths: one segment when that already
/// tells them apart, two when it does not. A file sitting directly in the
/// workspace root is in no project and contributes nothing — its own filename
/// is not a project name.
pub(crate) fn projects_of(citations: &[Citation]) -> Vec<String> {
    let folders: Vec<Vec<&str>> = citations
        .iter()
        .map(|c| c.relative_path.split('/').collect::<Vec<_>>())
        .filter(|segments| segments.len() > 1)
        .map(|segments| segments[..segments.len() - 1].to_vec())
        .collect();

    let depth = |n: usize| -> std::collections::BTreeSet<String> {
        folders
            .iter()
            .map(|f| f.iter().take(n).copied().collect::<Vec<_>>().join("/"))
            .collect()
    };

    let top = depth(1);
    if top.len() > 1 {
        return top.into_iter().collect();
    }
    depth(2).into_iter().collect()
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

/// Run one question end to end, streaming to `emit`.
#[allow(clippy::too_many_arguments)] // Each is a distinct input the window
                                     // has; a struct would move the list rather than shorten it.
pub fn run(
    core: &Arc<Core>,
    hub: &Arc<Hub>,
    conversation: &str,
    question: &str,
    history: &[Turn],
    // The previous answer ran out of budget. See `assemble`.
    resuming: bool,
    thorough: bool,
    // A subtree the answer is confined to, relative to the workspace root.
    scope: Option<&str>,
    cancel: &Cancel,
    emit: &mut dyn FnMut(AskEvent),
) {
    let started = std::time::Instant::now();

    // ── before anything is read off the disk ──────────────────────────────
    //
    // Two questions in this order, and both before retrieval:
    //
    //   1. **Who is answering, and where do they run?** The envelope has to be
    //      able to say which model is answering — "what model are you using?"
    //      was previously answered from the corpus, which retrieved chunks
    //      containing the word "model" and reported that none named one, while
    //      the footer of that same answer named it.
    //   2. **Is that allowed?** LLM-032 requires the classification check to
    //      happen *before* context assembly. Assembling first and refusing
    //      afterwards would already have read the forbidden files into a
    //      buffer built for sending them.
    //
    // Neither branches on what kind of generator it got: the first asks the
    // gateway, the second asks the policy, and the boundary is a value that
    // travels between them.
    let generator = match hub.generator() {
        Ok(g) => g,
        Err(e) => {
            emit(AskEvent::Failed {
                code: e.code().as_str().into(),
                message: e.message().into(),
            });
            return;
        }
    };
    if let Err(e) = core.permit_generation(generator.boundary) {
        emit(AskEvent::Failed {
            code: e.code().as_str().into(),
            message: e.message().into(),
        });
        return;
    }

    emit(AskEvent::Stage {
        stage: "retrieving".into(),
        detail: "Searching your files".into(),
    });
    // One session per conversation, so the delimiter — and therefore the whole
    // preamble — is byte-identical across turns and the KV prefix cache has
    // something to reuse. A fresh session per question reused 3% of the prompt;
    // a shared one reuses about 80%.
    let mut convo = hub.session_for(conversation);

    // Embedding the question is what turns the semantic branch on. `None`
    // when no embedding model is installed or the backfill has not run, and
    // that is the ordinary state rather than a failure.
    let embedding = hub.embed_query(question);

    let assembled = assemble(
        core,
        question,
        history,
        &mut convo,
        embedding.as_ref(),
        RuntimeFacts {
            identity: Some(hub.identity(&generator, thorough)),
            resuming,
        },
        scope,
    );
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

    emit(AskEvent::Sources {
        projects: projects_of(&citations),
        hits: citations,
        excluded,
        // LLM-033: what left the device — excerpts, files, bytes — taken from
        // the envelope's own disclosure rather than recounted here. Two counts
        // of the same thing drift, and this is the one that must not.
        bytes: envelope.disclosure.bytes,
        distinct_sources: envelope.disclosure.distinct_sources,
        boundary: generator.boundary.as_wire().into(),
        boundary_label: generator.boundary.label().into(),
        destination: generator.destination.clone(),
        model: generator.display.clone(),
    });

    // Two callbacks, one sink. `RefCell` rather than threading a channel
    // through the hub: the borrow is dynamic but the calls are strictly
    // sequential on one thread, so it cannot actually overlap.
    let sink = std::cell::RefCell::new(emit);
    let outcome = hub.generate_with_progress(
        &generator,
        &envelope,
        thorough,
        cancel,
        &mut |stage: &str, detail: &str| {
            (sink.borrow_mut())(AskEvent::Stage {
                stage: stage.into(),
                detail: detail.into(),
            })
        },
        &mut |e| match e {
            StreamEvent::Text { text } => (sink.borrow_mut())(AskEvent::Token { text }),
            StreamEvent::Thinking { text } => (sink.borrow_mut())(AskEvent::Thinking { text }),
            StreamEvent::Notice(n) => (sink.borrow_mut())(AskEvent::Notice {
                message: n.message,
                code: n.code.map(|c| c.as_str().to_string()),
            }),
            // `Done` is emitted below from the returned `Completion`, which
            // carries the same usage and stop reason. Two events saying one
            // thing are two things that can drift.
            StreamEvent::Finish(_) => {}
        },
    );
    let emit = sink.into_inner();
    match outcome {
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

// `no_generator_message` moved into the hub: it is the gateway that knows
// there is nothing to answer with, and it now has a remedy to offer that this
// module has no business knowing about ("or point Marrow at an endpoint").

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
            span: marrow_core::SourceSpan::Lines { start: 1, end: 1 },
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
        // The `origin = SELF` rule. The envelope drops it; this layer is where the path is
        // still known, so it is where the reason has to be collected.
        let e = evidence_from("E1", &chunk("notes.md", "as I said", false));
        assert_eq!(e.origin, Origin::SelfWritten);
        assert!(!e.origin.can_support_a_claim());
    }

    #[test]
    fn the_system_prompt_is_a_template_and_contains_no_retrieved_text() {
        // §114.1, and the thing that is easy to break by accident later.
        //
        // Pinned as properties rather than as a sentence. The previous version
        // asserted the literal "Answer only from the evidence" — which is the
        // exact phrasing that made the model refuse to build a page out of
        // evidence it had, because it read a format request as a claim. A test
        // that pins the wording of a prompt outlives the reason for the wording.
        assert!(
            SYSTEM.contains("cite") && SYSTEM.contains("[E1]"),
            "claims about the user's files must be cited"
        );
        assert!(
            SYSTEM.contains("Never invent"),
            "the model must not fill a gap with plausible detail"
        );
        assert!(
            SYSTEM.contains("name the facts rather than the format"),
            "refusing a shape rather than a missing fact is the bug this replaced"
        );
        assert!(
            SYSTEM.contains("mermaid") && SYSTEM.contains("html"),
            "diagrams and pages are part of the answer format"
        );
        // Both reported from real use. The model wrote paragraphs *about* its
        // evidence blocks — "E24 mentions four repos", a table with a column of
        // ids — to a reader who has no idea what E24 is. And it narrated its
        // own deliberation onto the page, spending the whole budget on
        // preamble before the answer had started.
        assert!(
            SYSTEM.contains("A citation is a mark, not a subject"),
            "the model must cite blocks, not write about them"
        );
        assert!(
            SYSTEM.contains("Answer, do not narrate"),
            "deliberation on the page is what ate the token budget"
        );
    }

    #[test]
    fn a_line_becomes_a_span_and_a_missing_line_does_not_pretend() {
        // A `source_span` on every node. `Whole` is honest; a fabricated line number is not.
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
    fn a_truncated_answer_is_carried_into_the_next_turn() {
        // The reported bug, three times over: an answer stopped at the token
        // limit, "continue" was asked, and the model wrote a fresh
        // introduction and started again — stopping at the same place each
        // time. From inside the history a truncated turn and a finished one are
        // identical, so nothing told it there was anything to resume. The
        // runtime knew: it is what prints "cut off at the token limit" under
        // the answer.
        let cut = |truncated| PriorTurn {
            role: "assistant".into(),
            text: "half an answer".into(),
            truncated,
        };
        let ask = PriorTurn {
            role: "user".into(),
            text: "continue".into(),
            truncated: false,
        };

        assert!(resuming(&[cut(true)]), "a cut-off answer is resumable");
        assert!(!resuming(&[cut(false)]), "a finished answer is not");
        assert!(!resuming(&[]), "a first question resumes nothing");

        // Only the turn immediately before. A truncation the user has already
        // moved past is history, not something "continue" refers to.
        assert!(
            !resuming(&[cut(true), ask]),
            "a truncation two turns back is not what continue means"
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
                truncated: false,
            },
            PriorTurn {
                role: "assistant".into(),
                text: "a".into(),
                truncated: false,
            },
            PriorTurn {
                role: "wat".into(),
                text: "?".into(),
                truncated: false,
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

    fn cite(relative_path: &str) -> Citation {
        Citation {
            id: "E1".into(),
            path: format!("/root/{relative_path}"),
            relative_path: relative_path.into(),
            location: relative_path.into(),
            line: None,
            span: marrow_core::SourceSpan::Whole,
            excerpt: String::new(),
            provenance: "exact".into(),
        }
    }

    #[test]
    fn the_projects_are_named_at_the_depth_that_actually_separates_them() {
        // The reported case: one workspace, many services, and a first segment
        // that names the container rather than the project.
        let melp = projects_of(&[
            cite("services/STT/README.md"),
            cite("services/vault/src/mfa.rs"),
            cite("services/STT/docs/stream.md"),
        ]);
        assert_eq!(melp, vec!["services/STT", "services/vault"]);

        // And where the first segment does separate them, a second would name
        // a source directory rather than a project.
        let siblings = projects_of(&[cite("marrow/src/lib.rs"), cite("enclave/src/lib.rs")]);
        assert_eq!(siblings, vec!["enclave", "marrow"]);
    }

    #[test]
    fn one_project_is_not_reported_as_several_and_a_loose_file_is_in_none() {
        // The signal only means something if it is absent when the evidence
        // really does come from one place.
        assert_eq!(
            projects_of(&[cite("services/STT/a.md"), cite("services/STT/b.md")]).len(),
            1
        );
        // A file in the workspace root belongs to no project; calling it one
        // would name the file and claim the answer spanned two.
        assert_eq!(projects_of(&[cite("README.md")]), Vec::<String>::new());
        assert_eq!(
            projects_of(&[cite("README.md"), cite("services/STT/a.md")]),
            vec!["services/STT"]
        );
    }

    #[test]
    fn the_chunk_budget_is_in_the_documented_range() {
        // ASK-003: fewer starves the answer, more dilutes it.
        assert!((5..=15).contains(&MAX_CHUNKS));
    }
}

#[cfg(test)]
mod wire {
    use super::*;

    /// The window reads these field names. `rename_all` on an enum renames the
    /// **variants**, not their fields — so every multi-word field went out as
    /// snake_case while the UI read camelCase, and every answer's footer said
    /// `tokens in NaNm NaNs`. Nothing failed; the values were simply not there.
    #[test]
    fn every_event_field_reaches_the_window_under_the_name_it_reads() {
        let done = serde_json::to_value(AskEvent::Done {
            prompt_tokens: 1,
            output_tokens: 2,
            thinking_tokens: 3,
            cached_prefix_tokens: 4,
            stop_reason: "stop".into(),
            elapsed_ms: 5,
        })
        .expect("serialize");
        for k in [
            "promptTokens",
            "outputTokens",
            "thinkingTokens",
            "cachedPrefixTokens",
            "stopReason",
            "elapsedMs",
        ] {
            assert!(done.get(k).is_some(), "Done is missing `{k}`: {done}");
        }

        let sources = serde_json::to_value(AskEvent::Sources {
            hits: Vec::new(),
            excluded: Vec::new(),
            bytes: 10,
            distinct_sources: 3,
            projects: vec!["services/STT".into()],
            boundary: "cloud".into(),
            boundary_label: "sent to a cloud provider".into(),
            destination: Some("api.example.com".into()),
            model: "m".into(),
        })
        .expect("serialize");
        for k in [
            "distinctSources",
            "projects",
            "boundary",
            "boundaryLabel",
            "destination",
        ] {
            assert!(
                sources.get(k).is_some(),
                "Sources is missing `{k}`: {sources}"
            );
        }
    }
}
