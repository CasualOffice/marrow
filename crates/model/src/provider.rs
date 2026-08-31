//! The generation seam (Part 8 §140, LLM-029).
//!
//! Local and cloud implement the same trait. **No branch outside the gateway
//! knows which kind it is** — the moment one does, every feature grows a
//! local-vs-cloud fork and the two drift.

use marrow_core::{Code, Result};
use serde::Serialize;

use crate::envelope::Envelope;
use crate::queue::Cancel;
use crate::request::Reasoning;

/// Where the work physically happened. Shown during every generation
/// (UX-012), because "local" and "a server in another country" are not
/// interchangeable facts about the user's documents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Boundary {
    Local,
    /// An OpenAI-compatible endpoint the user runs.
    Private,
    Cloud,
}

impl Boundary {
    pub fn label(self) -> &'static str {
        match self {
            Boundary::Local => "on this device",
            Boundary::Private => "on your own server",
            Boundary::Cloud => "sent to a cloud provider",
        }
    }

    /// The same fact as a phrase that follows "a model" — for the sentences
    /// where [`Boundary::label`] would have to be bent into the grammar.
    pub fn running_where(self) -> &'static str {
        match self {
            Boundary::Local => "running on this device",
            Boundary::Private => "running on a server you run",
            Boundary::Cloud => "running at a cloud provider",
        }
    }

    /// Whether the content has to cross the network to get there. The one
    /// question every disclosure surface actually asks.
    pub fn leaves_the_device(self) -> bool {
        !matches!(self, Boundary::Local)
    }

    /// The wire spelling, matching the `Serialize` impl so the UI and the
    /// stored conversation agree on one set of strings.
    pub fn as_wire(self) -> &'static str {
        match self {
            Boundary::Local => "local",
            Boundary::Private => "private",
            Boundary::Cloud => "cloud",
        }
    }
}

/// Why generation stopped. `Length` matters: an answer cut off mid-sentence
/// must be labelled, not presented as complete.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    Stop,
    Length,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub prompt_tokens: u32,
    pub output_tokens: u32,
    /// Counted separately from the answer, because Thorough's cost is the
    /// thing the user chose to pay and hiding it inside `output_tokens` makes
    /// the two modes look the same on the bill.
    pub thinking_tokens: u32,
    /// Prompt tokens served from the KV cache rather than re-prefilled
    /// (LLM-045). "Why was the second question faster" must be answerable.
    pub cached_prefix_tokens: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Completion {
    pub text: String,
    /// The model's working, kept but never cited (GEN-014, GEN-015).
    pub thinking: Option<String>,
    pub usage: Usage,
    pub stop_reason: StopReason,
    pub boundary: Boundary,
    pub model_id: String,
}

impl Completion {
    /// Announce this completion on the stream, then hand it back.
    ///
    /// Every successful exit from a provider goes through here, so a
    /// completion cannot be returned that the stream never announced. Building
    /// the [`Finish`] *from* the completion is the point: two hand-written
    /// copies of "what it cost and why it stopped" drift, and the one the user
    /// reads in the footer would be the one that drifted.
    #[must_use]
    pub fn announced(self, on_event: &mut dyn FnMut(StreamEvent)) -> Self {
        on_event(StreamEvent::Finish(Finish::new(
            self.usage,
            self.stop_reason,
        )));
        self
    }
}

/// Something a provider needs to say that is not part of the answer and is
/// not a failure.
///
/// The mid-stream case the old shape had nowhere to put. A provider that
/// ignored `reasoning_effort`, that has no thinking channel, or that is about
/// to hit an account limit has to tell someone, and with only tokens and a
/// `Result` it had exactly two options — swallow it into a `warn!` nobody
/// reads, or promote it to an error and throw away an answer that is fine.
///
/// `#[non_exhaustive]`: the fields are where this grows. Claude Code's stream
/// carries `rate_limit_event { rateLimitType: "five_hour", overageStatus:
/// "rejected" }` mid-generation, which wants a reset time and a window name
/// that no field here holds yet. That is a field to add, not a variant on
/// [`StreamEvent`] and not a new trait method — the distinction Vercel's
/// `LanguageModelV3` kept for three major versions by growing its payload
/// types and never its arity.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Notice {
    /// Cause and action, in one sentence, in the words the user reads. The
    /// same bar §108 sets for errors: "the request failed" is a defect there
    /// and "something happened" is a defect here.
    pub message: String,
    /// The §108 class, when one fits.
    ///
    /// `None` is the honest answer and the common one. §108 classifies
    /// *failures*, and "your five-hour limit resets at 14:00" is not one — the
    /// answer being streamed underneath it is real and complete. Forcing a
    /// code here would mean either inventing a class for every advisory a
    /// provider can invent, or filing them all under a failure code the caller
    /// then has to learn to ignore. Both are how a per-provider quirk list
    /// starts.
    pub code: Option<Code>,
}

impl Notice {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: None,
        }
    }

    /// For the advisories that *are* a §108 class — an unsupported capability
    /// that was skipped rather than refused, say.
    #[must_use]
    pub fn with_code(mut self, code: Code) -> Self {
        self.code = Some(code);
        self
    }
}

/// The out-of-band facts about a finished generation, delivered in band.
///
/// `#[non_exhaustive]`, and a struct rather than fields on the variant, for a
/// reason that is already true: one request can bill **two** models — an
/// auxiliary call plus the main one — so [`Completion::model_id`] is not
/// always the whole truth about who did the work. Whatever expresses that
/// (a per-model breakdown, a repeated attribution) is a field here, added
/// without touching the trait, the enum, or any consumer that does not want
/// it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Finish {
    pub usage: Usage,
    pub stop_reason: StopReason,
}

impl Finish {
    pub fn new(usage: Usage, stop_reason: StopReason) -> Self {
        Self { usage, stop_reason }
    }
}

/// One thing that happened while an answer was being generated.
///
/// This replaces a `FnMut(Token)` callback, which could carry text and
/// thinking and nothing else. Three facts arrive *during* a generation and had
/// nowhere to go:
///
/// 1. **What it cost and why it stopped.** They were the function's return
///    value, so a consumer rendering the stream could not see them until the
///    call was over — and a consumer that is not the caller (a supervisor
///    subscriber, MCP) never saw them at all. [`Finish`] is now the last event.
/// 2. **A warning.** See [`Notice`].
/// 3. **A failure after 200 tokens.** The text had already been emitted; the
///    old shape then forced a choice between returning `Err` and discarding
///    the completion that carried it, or lying about a clean finish.
///
/// **An error is not a variant here — it stays the `Result`.** Case 3 is not
/// solved by making failure an event; it is solved by [`Finish`] being one.
/// Once the answer reaches the consumer through the stream, an `Err` no longer
/// competes with it: the tokens stand, no `Finish` is emitted, and the caller
/// gets an error it cannot ignore because `?` will not let it. As a variant it
/// would be a fact every implementor must remember to *also* return and every
/// consumer must remember to check, which is the same class of mistake as an
/// ignored return value with none of the compiler's help.
///
/// **The enum is exhaustive on purpose** — no `#[non_exhaustive]`. A stream
/// event nobody renders is a silently dropped fact, so adding a variant
/// *should* break every consumer and make them decide. Growth belongs in the
/// payload types, which are `#[non_exhaustive]` precisely so it can go there.
///
/// The order is: `Text`/`Thinking`/`Notice` in any interleaving, then at most
/// one `Finish`, which is the last event. A `Notice` never terminates
/// anything. On `Err` there is no `Finish`.
///
/// Nothing here names a provider kind, and nothing may (LLM-029). "Local" and
/// "cloud" differ in their [`Boundary`], which is the [`Completion`]'s to
/// report; a consumer that could tell them apart from the stream would grow
/// the local-vs-cloud fork the seam exists to prevent.
// Struct variants, not tuples, and `rename_all_fields`: this type rides inside
// `supervisor::Event`, which the UI subscribes to. serde cannot serialise an
// internally-tagged newtype variant wrapping a bare `String` at all, and a
// field that keeps its Rust spelling arrives at a camelCase reader as
// `undefined` with nothing failing — the exact bug the note on that enum
// records.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
pub enum StreamEvent {
    /// Part of the answer.
    Text { text: String },
    /// Part of the model's reasoning. Kept separate at the wire level so the
    /// UI never has to guess which half of a stream it is rendering (GEN-014),
    /// and never citable (GEN-015).
    Thinking { text: String },
    /// Something the caller must be told. Does not end the stream.
    Notice(Notice),
    /// The last event. Says what the generation cost and why it stopped, while
    /// the consumer is still watching.
    Finish(Finish),
}

impl StreamEvent {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    pub fn thinking(text: impl Into<String>) -> Self {
        Self::Thinking { text: text.into() }
    }

    pub fn notice(message: impl Into<String>) -> Self {
        Self::Notice(Notice::new(message))
    }
}

/// What a caller asks for.
#[derive(Clone, Debug)]
pub struct GenerateRequest<'a> {
    pub model_id: &'a str,
    pub envelope: &'a Envelope,
    pub reasoning: Reasoning,
    pub max_output_tokens: u32,
    pub cancel: &'a Cancel,
}

/// Anything that can turn an envelope into an answer.
pub trait GenerationProvider: Send + Sync {
    fn boundary(&self) -> Boundary;

    /// A name for the UI: "Qwen 3.5 4B via MLX", not "local" (LLM-039).
    fn describe(&self) -> String;

    /// Generate, streaming [`StreamEvent`]s to `on_event` as they happen.
    ///
    /// Still two methods and still one return type. The trait's *arity* was
    /// never the problem — a third method is what a codebase adds when it
    /// cannot say something in the payload it already has, and rig only grew
    /// `capabilities()` when it had to. The stream was the problem.
    ///
    /// **[`Completion`] stays the return type**, even though [`Finish`] now
    /// carries its usage and stop reason. It is not a duplicate: the stream
    /// gives a *watcher* the facts as they arrive, and the return value gives
    /// a *caller* the whole answer without keeping its own accumulator. The
    /// supervisor books completions, the tests assert on them, and the MLX
    /// worker's `done` line carries a complete `thinking` string that can
    /// differ from the concatenated deltas. Returning `Result<()>` would mean
    /// four separate accumulators that can each drift from what streamed.
    ///
    /// Implementors: emit `Finish` on every `Ok` path — [`Completion::announced`]
    /// does it from the completion itself — and none on an `Err`, including
    /// an `Err` raised after text has already been emitted. That text stands.
    fn generate(
        &self,
        request: GenerateRequest<'_>,
        on_event: &mut dyn FnMut(StreamEvent),
    ) -> Result<Completion>;
}

/// Anything that can turn text into vectors.
///
/// Separate from generation on purpose: the embedder is resident and the
/// generator is not (§139.5), so they have different lifecycles and a single
/// trait would force them to share one.
pub trait EmbeddingProvider: Send + Sync {
    fn describe(&self) -> String;
    fn dimensions(&self) -> usize;
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_execution_boundary_is_stated_in_words_not_a_flag() {
        // UX-012: it is visible during every generation, so it must read as a
        // sentence rather than as an enum name.
        assert!(Boundary::Local.label().contains("this device"));
        assert!(Boundary::Cloud.label().contains("cloud"));
        for b in [Boundary::Local, Boundary::Private, Boundary::Cloud] {
            assert!(b.label().len() > 10, "{b:?}");
        }
    }

    #[test]
    fn thinking_tokens_are_counted_apart_from_the_answer() {
        // GEN-016: Thorough's cost is what the user chose to pay. Folding it
        // into `output_tokens` makes the two modes look identical.
        let u = Usage {
            prompt_tokens: 800,
            output_tokens: 120,
            thinking_tokens: 900,
            cached_prefix_tokens: 700,
        };
        assert_ne!(u.thinking_tokens, 0);
        assert!(
            u.thinking_tokens > u.output_tokens,
            "a Thorough answer thinks more than it says"
        );
    }

    fn completion(text: &str) -> Completion {
        Completion {
            text: text.into(),
            thinking: None,
            usage: Usage {
                prompt_tokens: 812,
                output_tokens: 11,
                thinking_tokens: 0,
                cached_prefix_tokens: 768,
            },
            stop_reason: StopReason::Length,
            boundary: Boundary::Local,
            model_id: "qwen3.5-4b-mlx-q4".into(),
        }
    }

    #[test]
    fn finish_carries_the_usage_the_return_value_carries() {
        // The whole point of the event: a consumer watching the stream learns
        // what the answer cost and why it stopped *while it is still
        // watching*, not from a return value it may never see. Built from the
        // completion so the two cannot disagree.
        let mut seen = Vec::new();
        let c = completion("the answer").announced(&mut |e| seen.push(e));

        let [StreamEvent::Finish(f)] = seen.as_slice() else {
            panic!("expected exactly one Finish, got {seen:?}")
        };
        assert_eq!(f.usage, c.usage);
        assert_eq!(f.usage.prompt_tokens, 812);
        assert_eq!(
            f.stop_reason,
            StopReason::Length,
            "an answer cut off mid-sentence must be labelled on the stream too"
        );
    }

    #[test]
    fn a_notice_is_not_terminal_and_carries_a_sentence() {
        // A warning that ends the stream is an error wearing a friendlier
        // name. This is the shape of the case that motivated it: an answer
        // arrives, something is worth saying half way through, and the answer
        // keeps arriving afterwards.
        let stream = [
            StreamEvent::text("It renews "),
            StreamEvent::Notice(Notice::new(
                "Your five-hour limit resets at 14:00. Answers until then may \
                 be refused.",
            )),
            StreamEvent::text("on 31 December 2026 [E1]."),
            StreamEvent::Finish(Finish::new(Usage::default(), StopReason::Stop)),
        ];

        let notices = stream
            .iter()
            .filter(|e| matches!(e, StreamEvent::Notice(_)))
            .count();
        assert_eq!(notices, 1);
        let after: Vec<_> = stream
            .iter()
            .skip_while(|e| !matches!(e, StreamEvent::Notice(_)))
            .skip(1)
            .collect();
        assert!(
            after.iter().any(|e| matches!(e, StreamEvent::Text { .. })),
            "text must still arrive after a notice"
        );
        assert!(
            matches!(stream.last(), Some(StreamEvent::Finish(_))),
            "and the stream must still finish"
        );

        let StreamEvent::Notice(n) = &stream[1] else {
            panic!("expected a notice")
        };
        assert!(n.message.contains("14:00"), "cause");
        assert!(n.message.contains("may"), "and what it means for the user");
        assert_eq!(
            n.code, None,
            "§108 classifies failures, and a limit that has not been hit is not one"
        );
    }

    #[test]
    fn a_notice_can_still_carry_a_class_when_one_fits() {
        let n = Notice::new(
            "That endpoint has no thinking channel, so this \
                             answer was not reasoned through.",
        )
        .with_code(Code::ModUnsupportedCapability);
        assert_eq!(n.code, Some(Code::ModUnsupportedCapability));
    }

    #[test]
    fn every_stream_event_survives_the_trip_to_the_window() {
        // This type rides inside `supervisor::Event`, whose own note records a
        // window reading `undefined` because a field kept its Rust spelling.
        // serde also refuses outright to serialise an internally-tagged
        // newtype variant wrapping a bare string, which is why `Text` is a
        // struct variant: a stream event that cannot be sent is a stream event
        // the UI never renders.
        let events = vec![
            StreamEvent::text("hello"),
            StreamEvent::thinking("the lease says…"),
            StreamEvent::notice("Reasoning effort was ignored."),
            StreamEvent::Finish(Finish::new(
                Usage {
                    prompt_tokens: 1,
                    output_tokens: 2,
                    thinking_tokens: 3,
                    cached_prefix_tokens: 4,
                },
                StopReason::Stop,
            )),
        ];
        let json = serde_json::to_string(&events).expect("every event serialises");
        assert!(json.contains(r#""kind":"text""#), "{json}");
        assert!(json.contains(r#""kind":"thinking""#), "{json}");
        assert!(json.contains(r#""kind":"notice""#), "{json}");
        assert!(json.contains(r#""kind":"finish""#), "{json}");
        assert!(json.contains(r#""stopReason":"stop""#), "{json}");
        assert!(json.contains(r#""cachedPrefixTokens":4"#), "{json}");
    }
}
