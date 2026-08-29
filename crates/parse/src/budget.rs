//! Parse budgets (PAR-010, PAR-011).
//!
//! This is a security surface, not a performance knob. Every input to a parser
//! is hostile until proven otherwise: a 4 KB YAML file can nest ten thousand
//! deep, a 200-byte Markdown file can expand into a million list items, a CSV
//! can declare a row with two million columns. The defence is the same in every
//! case — count, compare, stop.
//!
//! Three rules:
//!
//! 1. **Exceeding a budget is an ordinary [`Code::ParBudgetExceeded`] error.**
//!    `Class::Parse` isolates to one file, so the router degrades that file to
//!    metadata-only and the workspace keeps running (FS-011, NFR-001).
//! 2. **Never panic.** A budget check that panics has converted a bounded
//!    failure into an unbounded one.
//! 3. **Checked at the point of growth**, not after. Checking node count after
//!    building the vector is not a budget, it is a post-mortem.

use std::time::{Duration, Instant};

use marrow_core::{Code, Error, Result};

/// Warnings are bounded too: a file that produces a warning per line would
/// otherwise turn a 50 MB input into a 50 MB warning list.
pub const MAX_WARNINGS: usize = 64;

/// Limits applied to a single file's parse.
///
/// Defaults are sized from the real corpus (bench/M0-corpus.md): nothing on
/// disk is ≥ 500 MB, 70.6% of files are under 64 KB, and the largest bucket
/// tops out at 50 MB. A file above `max_file_bytes` is not an error — it is a
/// file we choose to index by metadata alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budgets {
    /// Above this, the router does not attempt a content parse at all.
    pub max_file_bytes: u64,
    /// Ceiling on IR nodes from one file.
    pub max_nodes: usize,
    /// Ceiling on IR tree depth. Deep nesting is the cheapest denial-of-service
    /// in every structured format.
    pub max_depth: u16,
    /// Wall-clock ceiling for one file.
    pub max_wall_clock: Duration,
    /// Ceiling on the text one node may carry, so a single 40 MB "paragraph"
    /// cannot be smuggled past `max_nodes`.
    pub max_node_text_bytes: usize,
    /// How deep the structured parsers descend into nested tables/objects
    /// before treating the remainder as one leaf value. Distinct from
    /// `max_depth`, which is the hard stop; this is the useful-detail cutoff.
    pub max_structured_depth: u16,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            max_file_bytes: 50 * 1024 * 1024,
            max_nodes: 100_000,
            max_depth: 64,
            max_wall_clock: Duration::from_secs(10),
            max_node_text_bytes: 4 * 1024 * 1024,
            max_structured_depth: 3,
        }
    }
}

impl Budgets {
    /// Tight budgets for tests and for the "just tell me the shape of it" path.
    pub fn small() -> Self {
        Self {
            max_file_bytes: 1024 * 1024,
            max_nodes: 1_000,
            max_depth: 8,
            max_wall_clock: Duration::from_secs(2),
            max_node_text_bytes: 64 * 1024,
            max_structured_depth: 3,
        }
    }
}

/// A live budget: the limits plus the clock they are measured against.
///
/// Cloned per parse attempt, so a parser that burned nine seconds does not
/// hand a nine-second-old deadline to the next parser in the chain.
#[derive(Clone, Copy, Debug)]
pub struct BudgetGuard {
    limits: Budgets,
    started: Instant,
}

impl BudgetGuard {
    pub fn new(limits: Budgets) -> Self {
        Self {
            limits,
            started: Instant::now(),
        }
    }

    pub fn limits(&self) -> &Budgets {
        &self.limits
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// The wall-clock guard.
    ///
    /// Uses `ParBudgetExceeded` rather than the arguably better-fitting
    /// `ParTimeout`, because `ParTimeout` is retryable and a deterministic
    /// parser that ran out of time on these bytes will run out of time on them
    /// again. A retryable code here would produce an infinite reprocessing loop.
    pub fn check_time(&self) -> Result<()> {
        if self.started.elapsed() > self.limits.max_wall_clock {
            return Err(budget_error(
                "Parsing took longer than the per-file time budget.",
                format!(
                    "{} ms elapsed, budget {} ms",
                    self.started.elapsed().as_millis(),
                    self.limits.max_wall_clock.as_millis()
                ),
            ));
        }
        Ok(())
    }

    /// Called before every node is appended. `current` is the count so far.
    pub fn check_node(&self, current: usize) -> Result<()> {
        if current >= self.limits.max_nodes {
            return Err(budget_error(
                "This file produces more structure than the per-file node budget allows.",
                format!("node budget {}", self.limits.max_nodes),
            ));
        }
        Ok(())
    }

    pub fn check_depth(&self, depth: u16) -> Result<()> {
        if depth > self.limits.max_depth {
            return Err(budget_error(
                "This file nests deeper than the per-file depth budget allows. Deeply nested \
                 input is the standard shape of a parser denial-of-service.",
                format!("depth {depth}, budget {}", self.limits.max_depth),
            ));
        }
        Ok(())
    }

    pub fn check_file_size(&self, bytes: u64) -> Result<()> {
        if bytes > self.limits.max_file_bytes {
            return Err(budget_error(
                "This file is larger than the per-file parse budget, so it is indexed by \
                 metadata only. Raise `max_file_bytes` in the workspace config to parse it.",
                format!("{bytes} bytes, budget {} bytes", self.limits.max_file_bytes),
            ));
        }
        Ok(())
    }

    /// Truncate `text` to the per-node ceiling, reporting whether it was cut.
    ///
    /// Returns owned text because every caller stores it; borrowing here just
    /// moves the allocation to the call site.
    pub fn clamp_text(&self, text: &str) -> (String, bool) {
        let cap = self.limits.max_node_text_bytes;
        if text.len() <= cap {
            return (text.to_owned(), false);
        }
        // Never split a codepoint: walk back to a boundary.
        let mut end = cap;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        (text[..end].to_owned(), true)
    }
}

fn budget_error(message: &str, context: String) -> Error {
    Error::new(Code::ParBudgetExceeded, message).with_context(context)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_errors_isolate_to_one_file() {
        // This is what lets the router degrade instead of aborting the run.
        assert!(Code::ParBudgetExceeded.isolates_to_one_file());
        assert!(!Code::ParBudgetExceeded.retryable());
    }

    #[test]
    fn node_and_depth_budgets_fire_at_the_limit_not_after() {
        let g = BudgetGuard::new(Budgets {
            max_nodes: 2,
            max_depth: 1,
            ..Budgets::default()
        });
        g.check_node(0).unwrap();
        g.check_node(1).unwrap();
        assert_eq!(g.check_node(2).unwrap_err().code(), Code::ParBudgetExceeded);
        g.check_depth(1).unwrap();
        assert_eq!(
            g.check_depth(2).unwrap_err().code(),
            Code::ParBudgetExceeded
        );
    }

    #[test]
    fn the_file_size_budget_names_the_setting_to_change() {
        let g = BudgetGuard::new(Budgets::default());
        let e = g.check_file_size(60 * 1024 * 1024).unwrap_err();
        assert_eq!(e.code(), Code::ParBudgetExceeded);
        assert!(
            e.message().contains("max_file_bytes"),
            "SUP-001: name the action"
        );
    }

    #[test]
    fn clamping_text_never_splits_a_codepoint() {
        let g = BudgetGuard::new(Budgets {
            max_node_text_bytes: 3,
            ..Budgets::default()
        });
        let (t, cut) = g.clamp_text("aé");
        assert!(!cut, "3 bytes fits exactly");
        assert_eq!(t, "aé");
        let (t, cut) = g.clamp_text("aébb");
        assert!(cut);
        assert_eq!(t, "aé");
        let (t, cut) = g.clamp_text("aaéb");
        assert!(cut);
        assert_eq!(t, "aa", "must back off the boundary rather than panic");
    }

    #[test]
    fn the_wall_clock_guard_is_not_retryable() {
        let g = BudgetGuard::new(Budgets {
            max_wall_clock: Duration::from_nanos(1),
            ..Budgets::default()
        });
        std::thread::sleep(Duration::from_millis(2));
        let e = g.check_time().unwrap_err();
        assert_eq!(e.code(), Code::ParBudgetExceeded);
        assert!(
            !e.retryable(),
            "retrying a deterministic parse loops forever"
        );
    }
}
