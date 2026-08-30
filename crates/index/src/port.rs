//! The `TextIndex` port ([LLD §2.1](../../../docs/LLD.md)).
//!
//! Five methods, no lifetimes, no leaked engine types, no `rusqlite` in a
//! signature. A port that exposes the engine is not a port — the whole reason
//! this seam exists is that D3 was reopened once and could be reopened again.
//!
//! The types here are the *contract*, so they are shaped by what a search
//! result has to be able to render ([UX §4](../../../docs/UX.md),
//! [GUI §5.2](../../../docs/GUI.md)) rather than by what FTS5 happens to
//! return:
//!
//! | UX element | Field |
//! |---|---|
//! | `path:line`, jumpable | [`TextHit::path`] + [`TextHit::span`] |
//! | 2–3 lines of matched content | [`TextHit::snippet`] |
//! | highlighting inside that content | [`Snippet::matches`] |
//! | breadcrumb, dimmed, last | [`TextHit::title`] (the chunker's context prefix) |
//! | age (`2d`, `3w`) | [`TextHit::modified`] |
//! | citation badge | [`TextHit::provenance`] |
//! | `origin = SELF` down-weight (§113.3, invariant #13) | [`TextHit::origin`] |
//!
//! Fusion is **not** this crate's job (Part 6 §113 belongs to `query`). What is
//! this crate's job is producing one branch's ranked candidates and the
//! per-field weights that branch was scored with.

use marrow_core::{
    ChunkId, FileId, Origin, ProvenanceClass, Result, SourceSpan, Timestamp, VersionId, WorkspaceId,
};

/// A query with more distinct terms than this is refused rather than truncated.
///
/// Truncating silently answers a different question than the one asked. The
/// bound exists because query text is untrusted input: a 10 KB paste must cost
/// a bounded amount of work, not a 2,000-term FTS5 expression.
pub const MAX_QUERY_TERMS: usize = 64;

/// Individual terms longer than this are truncated to it.
///
/// Unlike the term *count*, an over-long single term is not a different
/// question — no tokenizer produces a 400-character token from real prose, so
/// clipping it changes nothing a user meant.
pub const MAX_TERM_CHARS: usize = 128;

/// The indexed fields. Fixed, small, and never derived from user input — the
/// column names go into SQL as identifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TextField {
    /// The file's path. Cheap filename matching lives here (§113.2's path
    /// branch is separate; this is the lexical branch seeing the same text).
    Path,
    /// The chunk's structural context prefix — `fn refresh_token › impl
    /// TokenService` (CHK-002). Weighted above the body by default.
    Title,
    /// The chunk text itself.
    Body,
}

impl TextField {
    pub const ALL: [TextField; 3] = [TextField::Path, TextField::Title, TextField::Body];

    /// The FTS5 column name. A fixed identifier, never interpolated user input.
    pub const fn column(self) -> &'static str {
        match self {
            TextField::Path => "path",
            TextField::Title => "title",
            TextField::Body => "body",
        }
    }

    /// Zero-based column index, as FTS5's auxiliary functions want it.
    pub const fn column_index(self) -> i32 {
        match self {
            TextField::Path => 0,
            TextField::Title => 1,
            TextField::Body => 2,
        }
    }
}

/// Per-field BM25 weights.
///
/// **These are parameters, not constants** (Part 6 §113.4: "weights live in
/// config, not code, and are versioned"). [`Default`] is the v1 baseline to be
/// measured and replaced, and it is spelled here exactly so that a config layer
/// has something to override rather than a number buried in a query string.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FieldWeights {
    pub path: f64,
    pub title: f64,
    pub body: f64,
}

impl Default for FieldWeights {
    fn default() -> Self {
        // Title above path above body: a hit in the structural breadcrumb is
        // the strongest signal the lexical branch has, and a hit in the path is
        // stronger than one in a long body. Unmeasured; that is the point of
        // §113.4's tuning protocol.
        Self {
            path: 2.0,
            title: 3.0,
            body: 1.0,
        }
    }
}

impl FieldWeights {
    pub fn get(self, f: TextField) -> f64 {
        match f {
            TextField::Path => self.path,
            TextField::Title => self.title,
            TextField::Body => self.body,
        }
    }
}

/// How the query text is interpreted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MatchMode {
    /// Every term must appear somewhere in the document, in any order.
    #[default]
    Terms,
    /// The terms must appear adjacently, in order.
    Phrase,
    /// As [`MatchMode::Terms`], but the final term matches as a prefix — this
    /// is the as-you-type mode (GUI §5.2: results render at t=8 ms).
    Prefix,
    /// **Any** term may match; BM25 ranks the rest.
    ///
    /// For a natural-language question rather than a search box. "When does
    /// the lease renew?" as a conjunctive query requires a document
    /// containing *when*, *does*, *the*, *lease* and *renew* — which the lease
    /// itself does not, because it says "renews". Every other mode returns
    /// nothing here, and nothing is the one answer a retrieval layer must not
    /// give when the document is right there.
    ///
    /// Documents matching more terms still rank higher: BM25 sums per-term
    /// contributions, so this loses precision rather than ordering.
    Any,
}

/// Filters applied to the document metadata, not to the FTS expression.
///
/// They are ordinary bound parameters against `text_index_docs`, which is why
/// a path glob cannot become FTS5 syntax no matter what it contains.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filters {
    pub workspace: Option<WorkspaceId>,
    /// SQLite `GLOB` syntax (`*`, `?`, `[...]`), case-sensitive. Bound as a
    /// parameter.
    pub path_glob: Option<String>,
    /// Lower-cased, without the leading dot. Empty means "any".
    pub extensions: Vec<String>,
    /// Inclusive bounds on the source file's mtime.
    pub modified_after: Option<Timestamp>,
    pub modified_before: Option<Timestamp>,
}

impl Filters {
    pub fn is_empty(&self) -> bool {
        self.workspace.is_none()
            && self.path_glob.is_none()
            && self.extensions.is_empty()
            && self.modified_after.is_none()
            && self.modified_before.is_none()
    }
}

/// Bounds on the returned snippet. A search result is 2–3 lines (UX §4); a
/// pager is a different product.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnippetOptions {
    /// Tokens of context around the match. FTS5 caps this at 64.
    pub tokens: u32,
    /// Hard bound on the returned snippet, in characters, applied after FTS5
    /// has chosen the window.
    pub max_chars: usize,
    /// Which column the snippet comes from. `None` lets FTS5 pick the
    /// best-matching one, which is right for a result row.
    ///
    /// It is wrong for evidence. A query containing "lease" against a file
    /// named `lease.md` matches the *path* column best, so FTS5 returns the
    /// filename as the snippet — and a model handed a path and asked about
    /// the contents will say, correctly, that it was given nothing to read.
    /// Pin [`TextField::Body`] when the snippet is going to a model.
    pub column: Option<TextField>,
}

impl Default for SnippetOptions {
    fn default() -> Self {
        Self {
            tokens: 24,
            max_chars: 320,
            column: None,
        }
    }
}

/// FTS5's own limit on `snippet()`'s token count.
pub const MAX_SNIPPET_TOKENS: u32 = 64;

/// One lexical query.
#[derive(Clone, Debug)]
pub struct TextQuery {
    /// Raw user input. **Untrusted** — never interpolated into an FTS5
    /// expression; see `fts5::match_expression`.
    pub text: String,
    pub mode: MatchMode,
    /// Fields to search. Empty means all of them.
    pub fields: Vec<TextField>,
    pub filters: Filters,
    pub weights: FieldWeights,
    pub snippet: SnippetOptions,
    /// Candidate depth. §113.1's default is 100 per branch.
    pub limit: usize,
}

impl TextQuery {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            mode: MatchMode::default(),
            fields: Vec::new(),
            filters: Filters::default(),
            weights: FieldWeights::default(),
            snippet: SnippetOptions::default(),
            limit: 100,
        }
    }

    pub fn mode(mut self, mode: MatchMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn phrase(self) -> Self {
        self.mode(MatchMode::Phrase)
    }

    pub fn prefix(self) -> Self {
        self.mode(MatchMode::Prefix)
    }

    pub fn in_fields(mut self, fields: impl IntoIterator<Item = TextField>) -> Self {
        self.fields = fields.into_iter().collect();
        self.fields.sort_unstable();
        self.fields.dedup();
        self
    }

    pub fn with_weights(mut self, weights: FieldWeights) -> Self {
        self.weights = weights;
        self
    }

    pub fn with_filters(mut self, filters: Filters) -> Self {
        self.filters = filters;
        self
    }

    pub fn with_snippet(mut self, snippet: SnippetOptions) -> Self {
        self.snippet = snippet;
        self
    }

    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// The fields this query actually searches, in column order.
    pub fn effective_fields(&self) -> Vec<TextField> {
        if self.fields.is_empty() {
            TextField::ALL.to_vec()
        } else {
            self.fields.clone()
        }
    }
}

/// One indexable unit: a chunk, plus the file facts a hit has to carry.
///
/// It is a join of three canonical tables (`chunks`, `file_versions`, `files`),
/// which is the reason the FTS5 adapter syncs explicitly rather than by trigger
/// — see the module note on `fts5`.
#[derive(Clone, Debug)]
pub struct TextDoc {
    pub chunk_id: ChunkId,
    pub file_id: FileId,
    pub version_id: VersionId,
    pub workspace_id: WorkspaceId,
    /// The file's current path. Display and filtering only — **never
    /// identity** (invariant #2); `file_id` is the key.
    pub path: String,
    /// Structural context prefix (CHK-002). May be empty.
    pub title: String,
    pub body: String,
    /// Invariant #1: where in the source this came from.
    pub span: SourceSpan,
    pub provenance: ProvenanceClass,
    /// Invariant #13: `SelfWritten` is indexed and findable, but the `query`
    /// crate down-weights it and bars it from evidence authority.
    pub origin: Origin,
    /// The source file's mtime, for the recency filter and the `2d`/`3w` age.
    pub modified: Timestamp,
}

impl TextDoc {
    /// Lower-cased extension without the dot, derived from [`Self::path`].
    pub fn extension(&self) -> String {
        extension_of(&self.path)
    }
}

/// Lower-cased extension of a path, without the dot. `""` when there is none.
///
/// Deliberately string-only: this crate does not depend on `marrow-scan` and
/// must not start canonicalizing paths, which is that crate's job.
pub fn extension_of(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match name.rfind('.') {
        // A leading dot is a dotfile, not an extension: `.gitignore` has none.
        Some(0) | None => String::new(),
        Some(i) => name[i + 1..].to_ascii_lowercase(),
    }
}

/// A byte range inside [`Snippet::text`] that matched the query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MatchRange {
    pub start: usize,
    pub end: usize,
}

/// Matched content with the offsets a renderer needs to highlight it.
///
/// Offsets are **byte offsets into `text`**, always on `char` boundaries, and
/// always non-overlapping and ascending.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snippet {
    pub text: String,
    pub matches: Vec<MatchRange>,
}

impl Snippet {
    /// The snippet with `open`/`close` wrapped around each match. For tests and
    /// for renderers that emit markup rather than terminal attributes.
    pub fn highlighted(&self, open: &str, close: &str) -> String {
        let mut out = String::with_capacity(self.text.len() + self.matches.len() * 8);
        let mut at = 0usize;
        for m in &self.matches {
            if m.start < at || m.end > self.text.len() || m.start > m.end {
                continue;
            }
            out.push_str(&self.text[at..m.start]);
            out.push_str(open);
            out.push_str(&self.text[m.start..m.end]);
            out.push_str(close);
            at = m.end;
        }
        out.push_str(&self.text[at..]);
        out
    }

    /// The matched substrings themselves.
    pub fn matched_text(&self) -> Vec<&str> {
        self.matches
            .iter()
            .filter(|m| m.start <= m.end && m.end <= self.text.len())
            .map(|m| &self.text[m.start..m.end])
            .collect()
    }
}

/// One ranked result from the lexical branch.
#[derive(Clone, Debug)]
pub struct TextHit {
    pub chunk_id: ChunkId,
    pub file_id: FileId,
    pub version_id: VersionId,
    pub workspace_id: WorkspaceId,
    pub path: String,
    /// Structural breadcrumb, for the dimmed last line of a result.
    pub title: String,
    /// Higher is better. This is `-bm25()`, because FTS5 returns a score where
    /// *more negative* is better and every consumer expects the opposite.
    /// It is a **branch-local** score: §113.2 fuses by rank, not by score, so
    /// nothing outside this crate should compare it against another branch's.
    pub score: f32,
    pub span: SourceSpan,
    pub snippet: Snippet,
    pub provenance: ProvenanceClass,
    pub origin: Origin,
    pub modified: Timestamp,
}

/// Canonical state, streamed. The argument to [`TextIndex::rebuild_from`].
///
/// Not `Send + Sync`: a source is consumed on the calling thread, which lets it
/// be a live SQLite statement over the very connection doing the rebuild.
pub trait ChunkSource {
    /// Call `sink` once per document that should be searchable.
    ///
    /// An error from `sink` aborts the walk and propagates.
    fn for_each_chunk(&self, sink: &mut dyn FnMut(TextDoc) -> Result<()>) -> Result<()>;
}

/// Lexical retrieval. Implemented by SQLite FTS5 (D3).
pub trait TextIndex: Send + Sync {
    /// Insert or replace documents. Replacing is by `chunk_id`.
    fn upsert(&self, docs: &[TextDoc]) -> Result<()>;

    /// Remove documents. Ids that are not indexed are not an error.
    fn delete(&self, ids: &[ChunkId]) -> Result<()>;

    fn search(&self, q: &TextQuery) -> Result<Vec<TextHit>>;

    /// Everything here is rebuildable from canonical state. Non-negotiable.
    fn rebuild_from(&self, src: &dyn ChunkSource) -> Result<()>;

    /// How many documents are indexed. The number `marrow status` reports.
    fn doc_count(&self) -> Result<u64>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_come_from_the_last_dot_of_the_last_component() {
        assert_eq!(extension_of("src/auth/token.rs"), "rs");
        assert_eq!(extension_of("a.b/c"), "");
        assert_eq!(extension_of("archive.TAR.GZ"), "gz");
        assert_eq!(extension_of(".gitignore"), "", "a dotfile has no extension");
        assert_eq!(extension_of("/x/.config/settings"), "");
        assert_eq!(extension_of("notes"), "");
        assert_eq!(extension_of("C:\\docs\\report.DOCX"), "docx");
    }

    #[test]
    fn highlighting_reassembles_the_original_text() {
        let s = Snippet {
            text: "the refresh token rotates".to_string(),
            matches: vec![
                MatchRange { start: 4, end: 11 },
                MatchRange { start: 12, end: 17 },
            ],
        };
        assert_eq!(s.matched_text(), vec!["refresh", "token"]);
        assert_eq!(
            s.highlighted("[", "]"),
            "the [refresh] [token] rotates",
            "markers must land on the matched words, not near them"
        );
    }

    #[test]
    fn highlighting_ignores_out_of_range_offsets_rather_than_panicking() {
        // Offsets come from a parsed FTS5 snippet. A malformed one must degrade
        // to "no highlight", never to a slice panic in the renderer.
        let s = Snippet {
            text: "short".to_string(),
            matches: vec![MatchRange { start: 2, end: 900 }],
        };
        assert_eq!(s.highlighted("<", ">"), "short");
        assert!(s.matched_text().is_empty());
    }

    #[test]
    fn default_weights_rank_the_breadcrumb_above_the_body() {
        let w = FieldWeights::default();
        assert!(w.title > w.path && w.path > w.body);
        assert_eq!(w.get(TextField::Title), w.title);
    }

    #[test]
    fn field_selection_is_deduplicated_and_ordered() {
        let q = TextQuery::new("x").in_fields([TextField::Body, TextField::Path, TextField::Body]);
        assert_eq!(q.fields, vec![TextField::Path, TextField::Body]);
        assert_eq!(TextQuery::new("x").effective_fields(), TextField::ALL);
    }
}
