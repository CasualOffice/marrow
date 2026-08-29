//! What is asked of a model, and how hard it should think about it.

use std::time::Duration;

use marrow_core::{RequestId, Timestamp, WorkspaceId};
use serde::{Deserialize, Serialize};

/// Fast or thorough (Part 8 §145).
///
/// One field, not two code paths. The user chooses per request, because a
/// classifier that chose for them would be right most of the time and
/// unaccountable all of the time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode", content = "budget")]
pub enum Reasoning {
    /// Answer directly. The default (GEN-011) — most questions are lookups, and
    /// defaulting to Thorough spends the user's battery on "where is my invoice".
    #[default]
    Off,
    /// Think for up to this many tokens before answering.
    Budget(u32),
}

impl Reasoning {
    /// The budget the Thorough switch selects when the user has not tuned it.
    pub const THOROUGH: Reasoning = Reasoning::Budget(4096);

    pub fn thinking_tokens(self) -> u32 {
        match self {
            Reasoning::Off => 0,
            Reasoning::Budget(t) => t,
        }
    }

    /// The two words the UI shows.
    pub fn label(self) -> &'static str {
        match self {
            Reasoning::Off => "Fast",
            Reasoning::Budget(_) => "Thorough",
        }
    }

    pub fn is_on(self) -> bool {
        matches!(self, Reasoning::Budget(t) if t > 0)
    }
}

/// Strict priority (SUP-005). Not weighted: a user waiting always wins.
///
/// `Ord` puts the most important first, so a queue can `max()` without
/// remembering which direction is which.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    /// Background enrichment. Yields to everything.
    Enrichment,
    /// The user started it, but is not watching the cursor blink.
    Background,
    /// Someone is looking at a skeleton right now.
    Interactive,
}

/// A request for the supervisor to admit, queue and run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub id: RequestId,
    pub model_id: String,
    pub priority: Priority,
    pub reasoning: Reasoning,
    /// Whose content this is. Carried so a KV-cache entry can never be reused
    /// across a classification boundary (LLM-043).
    pub workspace: Option<WorkspaceId>,
    /// Tokens of prompt, so admission can size the KV cache it will need.
    pub prompt_tokens: u32,
    /// Ceiling on the answer.
    pub max_output_tokens: u32,
    /// After this, the request is dropped rather than run (SUP-006).
    pub deadline: Timestamp,
}

impl Request {
    pub fn new(model_id: impl Into<String>, priority: Priority, ttl: Duration) -> Self {
        Self {
            id: RequestId::new(),
            model_id: model_id.into(),
            priority,
            reasoning: Reasoning::Off,
            workspace: None,
            prompt_tokens: 0,
            max_output_tokens: 1024,
            deadline: Timestamp::from_millis(Timestamp::now().as_millis() + ttl.as_millis() as i64),
        }
    }

    pub fn with_reasoning(mut self, r: Reasoning) -> Self {
        self.reasoning = r;
        self
    }

    pub fn with_prompt_tokens(mut self, n: u32) -> Self {
        self.prompt_tokens = n;
        self
    }

    pub fn with_workspace(mut self, w: WorkspaceId) -> Self {
        self.workspace = Some(w);
        self
    }

    /// Total tokens this request will occupy.
    ///
    /// GEN-016: Thorough costs more, so it must *account* for more. A mode that
    /// changes cost but not accounting is a mode that lies to the queue.
    pub fn token_budget(&self) -> u32 {
        self.prompt_tokens + self.reasoning.thinking_tokens() + self.max_output_tokens
    }

    pub fn expired(&self, now: Timestamp) -> bool {
        now.as_millis() > self.deadline.as_millis()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_is_the_default() {
        // GEN-011. Thorough by default spends the battery on lookups.
        let r = Request::new("qwen2.5:7b", Priority::Interactive, Duration::from_secs(30));
        assert_eq!(r.reasoning, Reasoning::Off);
        assert_eq!(r.reasoning.label(), "Fast");
        assert!(!r.reasoning.is_on());
    }

    #[test]
    fn thorough_costs_more_in_the_accounting_not_just_in_wall_clock() {
        // GEN-016. If the two modes budget the same, the queue is being lied to.
        let base = Request::new("m", Priority::Interactive, Duration::from_secs(30))
            .with_prompt_tokens(2000);
        let thorough = base.clone().with_reasoning(Reasoning::THOROUGH);
        assert!(
            thorough.token_budget() > base.token_budget(),
            "{} must exceed {}",
            thorough.token_budget(),
            base.token_budget()
        );
        assert_eq!(
            thorough.token_budget() - base.token_budget(),
            Reasoning::THOROUGH.thinking_tokens()
        );
    }

    #[test]
    fn a_zero_budget_is_not_thinking() {
        // `Budget(0)` would otherwise label itself Thorough and do nothing,
        // which is exactly the silent lie GEN-013 forbids.
        assert!(!Reasoning::Budget(0).is_on());
        assert!(Reasoning::Budget(1).is_on());
    }

    #[test]
    fn an_interactive_request_outranks_background_and_enrichment() {
        // SUP-005, strict rather than weighted.
        assert!(Priority::Interactive > Priority::Background);
        assert!(Priority::Background > Priority::Enrichment);
        let mut v = [
            Priority::Enrichment,
            Priority::Interactive,
            Priority::Background,
        ];
        v.sort();
        assert_eq!(v[2], Priority::Interactive);
    }

    #[test]
    fn a_request_whose_asker_has_gone_is_expired() {
        // SUP-006. Running it burns memory for an answer nobody will read.
        let r = Request::new("m", Priority::Interactive, Duration::from_millis(0));
        assert!(r.expired(Timestamp::from_millis(r.deadline.as_millis() + 1)));
        assert!(!r.expired(Timestamp::from_millis(r.deadline.as_millis())));
    }

    #[test]
    fn reasoning_round_trips_through_json() {
        // It crosses the IPC boundary to the desktop UI and back.
        for r in [Reasoning::Off, Reasoning::THOROUGH, Reasoning::Budget(512)] {
            let s = serde_json::to_string(&r).unwrap();
            assert_eq!(serde_json::from_str::<Reasoning>(&s).unwrap(), r, "{s}");
        }
    }
}
