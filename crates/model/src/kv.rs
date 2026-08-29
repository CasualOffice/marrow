//! KV prefix cache accounting (Part 8 §139.1.2).
//!
//! Marrow's prompts share a large prefix by construction: the system
//! instructions, the untrusted-evidence envelope's framing (§114), and often
//! the same document excerpts across a follow-up question. Recomputing that on
//! every request is the largest avoidable cost in the feature, and on a 16 GB
//! machine it is paid in the resource that is already scarce.
//!
//! ```text
//! request 1   [ system | envelope | doc A | doc B | question 1 ]
//!                 └──────── prefix, computed once ────────┘
//! request 2   [ system | envelope | doc A | doc B | question 2 ]
//!                 └──────── prefix, reused ───────────────┘
//! ```
//!
//! This module owns the **bookkeeping** — which prefix, whose content, how many
//! bytes, what to evict. The tensors themselves live in the worker process.

use std::collections::HashMap;

use marrow_core::WorkspaceId;
use serde::Serialize;

/// Identity of a cached prefix.
///
/// LLM-041: reuse is **prefix-exact**. A partial or fuzzy match is a
/// wrong-answer generator, not an optimisation — so the key is a hash of the
/// exact token sequence, and a one-token difference is a different key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PrefixKey([u8; 16]);

impl PrefixKey {
    /// Hash a token prefix. Tokens, not text: two different strings can
    /// tokenize identically and two identical strings cannot tokenize
    /// differently, so tokens are the honest unit.
    pub fn of(tokens: &[u32]) -> Self {
        let mut h = blake3::Hasher::new();
        for t in tokens {
            h.update(&t.to_le_bytes());
        }
        let mut out = [0u8; 16];
        out.copy_from_slice(&h.finalize().as_bytes()[..16]);
        Self(out)
    }

    pub fn to_hex(self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

impl std::fmt::Display for PrefixKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// Who the cached content belongs to.
///
/// LLM-043: a cached prefix is never reused across a classification boundary.
/// The whole point of the boundary is that this content does not reach that
/// provider, and a cache hit would carry it there invisibly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Scope {
    pub workspace: Option<WorkspaceId>,
    /// The workspace's classification at the time of caching. Compared by
    /// value: a workspace whose classification tightened must not serve hits
    /// cached under the looser one.
    pub classification: u8,
}

/// What was cached, and what it costs.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheEntry {
    pub tokens: u32,
    pub bytes: u64,
    /// Monotonic use counter, for LRU.
    last_used: u64,
}

/// Hits and misses, so "why was the second question faster" is answerable
/// (LLM-045).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    /// Reuse refused because the scope differed. Counted separately: it is a
    /// policy outcome, not a cache miss, and conflating them would hide a
    /// misconfigured classification behind a low hit rate.
    pub scope_refusals: u64,
}

impl Stats {
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
}

/// LRU prefix cache with a byte cap.
#[derive(Debug)]
pub struct PrefixCache {
    entries: HashMap<(Scope, PrefixKey), CacheEntry>,
    /// LLM-044: a fraction of the model's own footprint, not a fixed number. A
    /// 1 GB cache beside a 4 GB model is a different decision from one beside
    /// a 40 GB model.
    capacity_bytes: u64,
    used_bytes: u64,
    clock: u64,
    pub stats: Stats,
}

impl PrefixCache {
    pub fn new(capacity_bytes: u64) -> Self {
        Self {
            entries: HashMap::new(),
            capacity_bytes,
            used_bytes: 0,
            clock: 0,
            stats: Stats::default(),
        }
    }

    pub fn capacity_bytes(&self) -> u64 {
        self.capacity_bytes
    }

    /// LLM-042: the cache is counted in live memory accounting. A cache that
    /// grows without appearing in admission is how a model that fit at load
    /// stops fitting at request 40.
    pub fn used_bytes(&self) -> u64 {
        self.used_bytes
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Look up a prefix for this scope.
    pub fn get(&mut self, scope: Scope, key: PrefixKey) -> Option<CacheEntry> {
        self.clock += 1;
        let clock = self.clock;
        if let Some(e) = self.entries.get_mut(&(scope, key)) {
            e.last_used = clock;
            self.stats.hits += 1;
            return Some(*e);
        }
        // Distinguish "not cached" from "cached, but not for you". The second
        // is a policy outcome and must not read as a cold cache.
        if self.entries.keys().any(|(_, k)| *k == key) {
            self.stats.scope_refusals += 1;
        }
        self.stats.misses += 1;
        None
    }

    /// Record a newly computed prefix. Evicts LRU entries to fit.
    ///
    /// An entry larger than the whole cache is **not** stored — evicting
    /// everything for something that will itself be evicted next request is
    /// worse than not caching it.
    pub fn insert(&mut self, scope: Scope, key: PrefixKey, tokens: u32, bytes: u64) -> bool {
        if bytes > self.capacity_bytes {
            return false;
        }
        self.clock += 1;
        if let Some(old) = self.entries.remove(&(scope, key)) {
            self.used_bytes -= old.bytes;
        }
        while self.used_bytes + bytes > self.capacity_bytes {
            if !self.evict_one() {
                return false;
            }
        }
        self.used_bytes += bytes;
        self.entries.insert(
            (scope, key),
            CacheEntry {
                tokens,
                bytes,
                last_used: self.clock,
            },
        );
        true
    }

    fn evict_one(&mut self) -> bool {
        // Ties broken by key so eviction order is deterministic; an
        // unpredictable cache is one nobody can reproduce a bug in.
        let victim = self
            .entries
            .iter()
            .min_by_key(|(k, e)| (e.last_used, **k))
            .map(|(k, _)| *k);
        match victim {
            Some(k) => {
                if let Some(e) = self.entries.remove(&k) {
                    self.used_bytes -= e.bytes;
                }
                self.stats.evictions += 1;
                true
            }
            None => false,
        }
    }

    /// Drop everything. Called when the model is unloaded — LLM-049: a model
    /// unloaded while its cache stays resident has not been unloaded.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.used_bytes = 0;
    }

    /// Drop everything belonging to one workspace, for when its classification
    /// changes.
    pub fn invalidate_workspace(&mut self, workspace: WorkspaceId) {
        let doomed: Vec<_> = self
            .entries
            .keys()
            .filter(|(s, _)| s.workspace == Some(workspace))
            .copied()
            .collect();
        for k in doomed {
            if let Some(e) = self.entries.remove(&k) {
                self.used_bytes -= e.bytes;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(class: u8) -> Scope {
        Scope {
            workspace: Some(WorkspaceId::new()),
            classification: class,
        }
    }

    #[test]
    fn a_repeated_prefix_hits() {
        // The whole point: a follow-up question reuses the system prompt, the
        // envelope and the same documents.
        let mut c = PrefixCache::new(1_000_000);
        let s = scope(0);
        let k = PrefixKey::of(&[1, 2, 3, 4]);
        assert!(c.get(s, k).is_none());
        c.insert(s, k, 4, 1000);
        assert!(c.get(s, k).is_some());
        assert_eq!(c.stats.hits, 1);
        assert_eq!(c.stats.misses, 1);
    }

    #[test]
    fn one_different_token_is_a_different_prefix() {
        // LLM-041. Fuzzy matching here is a wrong-answer generator: the model
        // would continue from a state that never produced this prompt.
        let mut c = PrefixCache::new(1_000_000);
        let s = scope(0);
        c.insert(s, PrefixKey::of(&[1, 2, 3, 4]), 4, 1000);
        assert!(c.get(s, PrefixKey::of(&[1, 2, 3, 5])).is_none());
        assert!(
            c.get(s, PrefixKey::of(&[1, 2, 3])).is_none(),
            "a shorter prefix is not a hit"
        );
    }

    #[test]
    fn a_prefix_is_never_reused_across_a_classification_boundary() {
        // LLM-043. The boundary exists precisely so this content does not
        // reach that provider; a cache hit would carry it there invisibly.
        let mut c = PrefixCache::new(1_000_000);
        let ws = WorkspaceId::new();
        let public = Scope {
            workspace: Some(ws),
            classification: 0,
        };
        let confidential = Scope {
            workspace: Some(ws),
            classification: 2,
        };
        let k = PrefixKey::of(&[9, 9, 9]);
        c.insert(confidential, k, 3, 500);
        assert!(
            c.get(public, k).is_none(),
            "same workspace, tighter class, must miss"
        );
        assert_eq!(
            c.stats.scope_refusals, 1,
            "a scope refusal is not a cold cache"
        );
    }

    #[test]
    fn a_prefix_is_never_reused_across_workspaces() {
        let mut c = PrefixCache::new(1_000_000);
        let k = PrefixKey::of(&[7]);
        c.insert(scope(0), k, 1, 100);
        assert!(c.get(scope(0), k).is_none(), "different workspace id");
    }

    #[test]
    fn the_cache_evicts_least_recently_used_when_it_fills() {
        let mut c = PrefixCache::new(300);
        let s = scope(0);
        let (a, b, d) = (
            PrefixKey::of(&[1]),
            PrefixKey::of(&[2]),
            PrefixKey::of(&[3]),
        );
        c.insert(s, a, 1, 100);
        c.insert(s, b, 1, 100);
        c.get(s, a); // a is now more recent than b
        c.insert(s, d, 1, 150);
        assert!(c.get(s, b).is_none(), "b was least recently used");
        assert!(c.get(s, a).is_some(), "a was touched and must survive");
        assert!(c.stats.evictions >= 1);
    }

    #[test]
    fn the_cache_never_exceeds_its_cap() {
        // LLM-042: it is counted in admission, so it must not be able to lie.
        let mut c = PrefixCache::new(1000);
        let s = scope(0);
        for i in 0..50u32 {
            c.insert(s, PrefixKey::of(&[i]), 1, 100);
            assert!(c.used_bytes() <= 1000, "used {} over cap", c.used_bytes());
        }
    }

    #[test]
    fn an_entry_larger_than_the_whole_cache_is_not_stored() {
        // Evicting everything for something that will itself be evicted next
        // request is worse than not caching it.
        let mut c = PrefixCache::new(1000);
        let s = scope(0);
        c.insert(s, PrefixKey::of(&[1]), 1, 500);
        assert!(!c.insert(s, PrefixKey::of(&[2]), 1, 5000));
        assert!(
            c.get(s, PrefixKey::of(&[1])).is_some(),
            "the existing entry must survive"
        );
    }

    #[test]
    fn reinserting_the_same_key_does_not_double_count_its_bytes() {
        let mut c = PrefixCache::new(1000);
        let s = scope(0);
        let k = PrefixKey::of(&[1]);
        c.insert(s, k, 1, 400);
        c.insert(s, k, 1, 400);
        assert_eq!(c.used_bytes(), 400);
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn unloading_the_model_releases_the_cache() {
        // LLM-049. A model unloaded while its cache stays resident has not
        // been unloaded.
        let mut c = PrefixCache::new(1_000_000);
        let s = scope(0);
        for i in 0..5u32 {
            c.insert(s, PrefixKey::of(&[i]), 1, 1000);
        }
        assert!(c.used_bytes() > 0);
        c.clear();
        assert_eq!(c.used_bytes(), 0);
        assert!(c.is_empty());
    }

    #[test]
    fn a_workspace_can_be_invalidated_without_clearing_everything() {
        // A classification change must not cost every other workspace its
        // cache.
        let mut c = PrefixCache::new(1_000_000);
        let doomed_ws = WorkspaceId::new();
        let doomed = Scope {
            workspace: Some(doomed_ws),
            classification: 0,
        };
        let keep = scope(0);
        c.insert(doomed, PrefixKey::of(&[1]), 1, 100);
        c.insert(keep, PrefixKey::of(&[2]), 1, 100);
        c.invalidate_workspace(doomed_ws);
        assert_eq!(c.used_bytes(), 100);
        assert!(c.get(keep, PrefixKey::of(&[2])).is_some());
    }

    #[test]
    fn the_hit_rate_is_observable_and_starts_at_zero_rather_than_one() {
        // LLM-045. An empty cache reporting 100% would look like a working
        // optimisation that never ran.
        let mut c = PrefixCache::new(1000);
        assert_eq!(c.stats.hit_rate(), 0.0);
        let s = scope(0);
        let k = PrefixKey::of(&[1]);
        c.get(s, k);
        c.insert(s, k, 1, 10);
        c.get(s, k);
        assert_eq!(c.stats.hits, 1);
        assert_eq!(c.stats.misses, 1);
        assert!((c.stats.hit_rate() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn eviction_order_is_deterministic() {
        // An unpredictable cache is one nobody can reproduce a bug in.
        let run = || {
            let mut c = PrefixCache::new(250);
            let s = Scope {
                workspace: None,
                classification: 0,
            };
            let mut order = Vec::new();
            for i in 0..5u32 {
                c.insert(s, PrefixKey::of(&[i]), 1, 100);
                order.push(c.len());
            }
            order
        };
        assert_eq!(run(), run());
    }
}
