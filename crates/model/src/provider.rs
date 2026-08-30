//! The generation seam (Part 8 §140, LLM-029).
//!
//! Local and cloud implement the same trait. **No branch outside the gateway
//! knows which kind it is** — the moment one does, every feature grows a
//! local-vs-cloud fork and the two drift.

use marrow_core::Result;
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

/// A token as it arrives.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Token {
    /// Part of the answer.
    Text(String),
    /// Part of the model's reasoning. Kept separate at the wire level so the
    /// UI never has to guess which half of a stream it is rendering.
    Thinking(String),
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

    /// Generate, streaming tokens to `on_token` as they arrive.
    fn generate(
        &self,
        request: GenerateRequest<'_>,
        on_token: &mut dyn FnMut(Token),
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
}
