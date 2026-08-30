//! The FTS5 adapter (D3).
//!
//! # Why this lives in the canonical database
//!
//! D3 chose SQLite FTS5 over Tantivy for **transactional consistency**: the
//! index is in the same database as the row it derives from, so an index update
//! happens in the same transaction as that row. There is no window where the
//! two disagree and nothing to reconcile.
//!
//! That property is a property of *how this module is called*, not of FTS5. It
//! survives only if the canonical write and the index write share one
//! transaction. So the primitives here take a plain `&Connection`:
//!
//! ```ignore
//! store.writer().submit(move |conn| {
//!     read::insert_chunk(conn, &chunk)?;          // canonical
//!     fts5::upsert_docs(conn, &[doc])?;           // derived — same transaction
//!     Ok(())
//! })?;
//! ```
//!
//! `Transaction` and `Savepoint` both deref to `&Connection`, so the same
//! function body runs inside the writer actor's batch and on a bare connection.
//! [`Fts5Index`] wraps these for callers that only touch the index (rebuild,
//! search, maintenance) — but the ingest path **must** use the free functions,
//! or D3's whole argument is thrown away. `index_and_canonical_write_share_one_transaction`
//! is the test that says so.
//!
//! # Triggers vs explicit sync
//!
//! **Explicit sync for writes, one trigger for deletes.** Both halves are
//! deliberate.
//!
//! Writes are explicit because a [`TextDoc`] is a join of three canonical
//! tables — `chunks` (text, context prefix, provenance), `file_versions`
//! (mtime) and `files` (path, origin). The FTS5 external-content/trigger
//! pattern is the right one when the indexed columns are a projection of
//! exactly one table; here it would need triggers on three tables with three
//! different fan-outs, and the worst of them is a rename: `files.current_path`
//! changes and *every* chunk of that file needs a new `path` column, with no
//! `chunks` row having been touched. A trigger set that has to reproduce a
//! three-way join is not simpler than a function call, it is the same logic
//! written where it cannot be tested or traced. Triggers would also fire during
//! [`rebuild`], doubling the work of the one operation that is already the slow
//! path.
//!
//! Deletes are a trigger because `text_index_docs` is the single source of
//! truth for FTS5 rowids, and the delete does *not* always come from
//! application code: `text_index_docs.chunk_id` is `REFERENCES chunks(chunk_id)
//! ON DELETE CASCADE`, so dropping a file version cascades to its chunks and
//! from there to the docs, entirely inside SQLite. The `AFTER DELETE` trigger
//! is what stops that leaving orphaned FTS5 rows that nothing can ever reach
//! again — see `deleted_chunks_leave_no_orphan_docs`. It is one trigger, on one
//! table, doing one row.
//!
//! # Tokenizer
//!
//! `unicode61 remove_diacritics 2`.
//!
//! - `unicode61` is the only built-in tokenizer that is Unicode-aware.
//!   `ascii` mis-tokenizes every non-English document in the corpus; `porter`
//!   is a stemmer wrapper that would make `search --literal`-shaped
//!   expectations ("I typed `parses`, why did `parsing` match?") wrong in a way
//!   that is hard to explain, and stemming English only is worse than not
//!   stemming when the corpus is mixed.
//! - `remove_diacritics 2` rather than the default `1`: version 1 only strips
//!   diacritics from codepoints below U+0800, which silently leaves most of
//!   Vietnamese, and a good deal of Central European text, un-folded. `2` is
//!   the fixed version and there is no reason to inherit the bug.
//! - `_` is left as a separator (the default). `refresh_token` therefore
//!   indexes as `refresh` + `token`, so searching either word finds it. Exact
//!   `FOO_BAR` matching is what `literal.rs` (CAP-005) is for, and it is exact
//!   there rather than approximate here.
//! - `prefix = '2 3'` builds prefix indexes for 2- and 3-character prefixes, so
//!   the as-you-type path (GUI §5.2, results at t=8 ms) does not scan. It costs
//!   index size; at M0's ~34k documents that is a rounding error.
//!
//! # Untrusted query text
//!
//! Query text is content, and content is untrusted (invariant #12). It is
//! never interpolated into an FTS5 expression. [`match_expression`] tokenizes
//! the input itself and re-emits every token as a quoted FTS5 string, so the
//! only operators in the expression are ones this module wrote. See
//! `query_syntax_cannot_be_injected`.

use std::sync::Mutex;

use marrow_core::{
    ChunkId, Code, Error, FileId, Origin, ProvenanceClass, Result, SourceSpan, Timestamp,
    VersionId, WorkspaceId,
};
use marrow_store::migrate::Migration;
use marrow_store::rusqlite::{self, params, types::ToSql, Connection, Row};
use marrow_store::{map_sqlite, ReadConn, Store, Writer};

use crate::port::{
    ChunkSource, Filters, MatchMode, MatchRange, Snippet, TextDoc, TextField, TextHit, TextIndex,
    TextQuery, MAX_QUERY_TERMS, MAX_SNIPPET_TOKENS, MAX_TERM_CHARS,
};

/// The metadata table. One row per indexed chunk; owns the FTS5 rowid.
pub const DOCS_TABLE: &str = "text_index_docs";
/// The FTS5 virtual table.
pub const FTS_TABLE: &str = "text_index";

/// Marker characters wrapped around matches by `snippet()`, then stripped by
/// [`parse_snippet`] to produce byte offsets.
///
/// U+0001/U+0002 rather than `<b>`/`[[`: any printable delimiter can occur in
/// real content, and a delimiter collision would shift every offset after it.
/// These two are stripped from document text at index time (see
/// [`sanitize`]), so inside a snippet they can only have come from FTS5.
const MARK_OPEN: char = '\u{1}';
const MARK_CLOSE: char = '\u{2}';
/// What `snippet()` puts where it truncated.
const ELLIPSIS: &str = "…";

/// How many documents a [`Fts5Index::rebuild_from`] sends per writer batch.
const REBUILD_BATCH: usize = 500;

/// Cap on ids per `DELETE ... IN (...)`, well under SQLite's variable limit.
const DELETE_CHUNK: usize = 500;

// ---------------------------------------------------------------- migration

/// Schema version this migration writes.
pub const TEXT_INDEX_VERSION: i64 = 2;

/// `schema_meta` key recording that the text index tables are present.
pub const VERSION_META_KEY: &str = "text_index_version";

/// The text-index tables, as a [`Migration`] in `marrow-store`'s numbered chain.
///
/// Version 2 continues `crates/store/src/migrate.rs`'s chain — v1 is the M1
/// core schema, and this depends on it (the foreign keys point at `chunks` and
/// `files`, which v1 creates).
///
/// **Installation goes through the store's runner.** `crates/store` owns
/// `MIGRATIONS`, and appending to it is a one-line edit in a crate this change
/// is not allowed to touch, so [`ensure_installed`] applies exactly this
/// `Migration` value with the same semantics the runner uses — one transaction,
/// forward only, version recorded in `schema_meta` — and no-ops once the tables
/// exist. When `MIGRATIONS` can be edited, append `marrow_index::fts5::MIGRATION`
/// to it and bump `marrow_core::SCHEMA_VERSION` to 2: `ensure_installed` then
/// finds the tables already there and does nothing, so the two orders converge
/// and there is never a second chain to keep in step.
pub const MIGRATION: Migration = Migration {
    version: TEXT_INDEX_VERSION,
    name: "m1_text_index_fts5",
    up: TEXT_INDEX_V2,
};

/// Migration 2: the lexical index.
pub const TEXT_INDEX_V2: &str = r#"
-- Doc-level facts a search result must carry (UX §4, GUI §5.2). Separate from
-- the FTS5 table because FTS5 columns are all text and all tokenized: filtering
-- on a real INTEGER mtime or a GLOB-able path needs ordinary SQLite columns and
-- ordinary SQLite indexes.
CREATE TABLE text_index_docs (
    -- Equals text_index.rowid. INTEGER PRIMARY KEY so it is the SQLite rowid
    -- here too, which keeps the join a rowid lookup on both sides.
    doc_id              INTEGER PRIMARY KEY,
    -- ON DELETE CASCADE is the machine-checkable form of "derived data does not
    -- outlive what it derives from": drop a file version and its chunks go, and
    -- their index docs go with them without any application code running.
    chunk_id            TEXT NOT NULL UNIQUE
                          REFERENCES chunks(chunk_id) ON DELETE CASCADE,
    -- Invariant #2: derived data is keyed on file_id, never on a path.
    file_id             TEXT NOT NULL REFERENCES files(file_id) ON DELETE CASCADE,
    version_id          TEXT NOT NULL,
    workspace_id        TEXT NOT NULL,
    -- Display and filtering only. Updated on rename; not identity.
    path                TEXT NOT NULL,
    extension           TEXT NOT NULL DEFAULT '',
    -- Invariant #1: a hit that cannot say where it came from is not a citation.
    source_span         TEXT NOT NULL CHECK (json_valid(source_span)),
    provenance_class    TEXT NOT NULL CHECK (provenance_class IN
                          ('EXACT','DEGRADED','APPROXIMATE','METADATA_ONLY')),
    -- Invariant #13: SELF content is findable, and the query layer refuses to
    -- let it support a claim. Carried here so that refusal needs no extra join.
    origin              TEXT NOT NULL CHECK (origin IN ('USER','SELF')),
    modified_ms         INTEGER NOT NULL
);
CREATE INDEX idx_text_docs_file ON text_index_docs(file_id);
CREATE INDEX idx_text_docs_ws   ON text_index_docs(workspace_id, modified_ms);
CREATE INDEX idx_text_docs_ext  ON text_index_docs(extension);

-- See the module note for the tokenizer and prefix-index reasoning.
CREATE VIRTUAL TABLE text_index USING fts5(
    path,
    title,
    body,
    tokenize = 'unicode61 remove_diacritics 2',
    prefix = '2 3'
);

-- The one trigger. Deletes reach text_index_docs through the FK cascade above,
-- with no application code in the loop, so the FTS5 row has to be removed here
-- or it is orphaned forever.
CREATE TRIGGER text_index_docs_ad AFTER DELETE ON text_index_docs BEGIN
    DELETE FROM text_index WHERE rowid = old.doc_id;
END;
"#;

/// Whether the text-index tables exist on this connection.
pub fn is_installed(conn: &Connection) -> Result<bool> {
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type IN ('table','view') AND name IN (?1, ?2)",
            [DOCS_TABLE, FTS_TABLE],
            |r| r.get(0),
        )
        .map_err(|e| map_sqlite(e, "Could not check whether the text index exists."))?;
    Ok(n == 2)
}

/// Apply [`MIGRATION`] if it has not been applied. Idempotent.
///
/// Must run inside the caller's transaction — the DDL and the `schema_meta` row
/// commit together or not at all, exactly as the store's runner does it.
pub fn ensure_installed(conn: &Connection) -> Result<()> {
    if is_installed(conn)? {
        return Ok(());
    }
    let span = tracing::info_span!(
        "migration",
        version = MIGRATION.version,
        name = MIGRATION.name
    );
    let _e = span.enter();
    conn.execute_batch(MIGRATION.up).map_err(|e| {
        map_sqlite(
            e,
            "The text index could not be created, so search is unavailable. Delete the index \
             directory to rebuild it from your files.",
        )
        .with_context(format!(
            "migration {} ({})",
            MIGRATION.version, MIGRATION.name
        ))
    })?;
    conn.execute(
        "INSERT INTO schema_meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [VERSION_META_KEY, &TEXT_INDEX_VERSION.to_string()],
    )
    .map_err(|e| map_sqlite(e, "Could not record the text index schema version."))?;
    tracing::info!("migration applied");
    Ok(())
}

// -------------------------------------------------------------- SQL codecs

fn provenance_sql(p: ProvenanceClass) -> &'static str {
    match p {
        ProvenanceClass::Exact => "EXACT",
        ProvenanceClass::Degraded => "DEGRADED",
        ProvenanceClass::Approximate => "APPROXIMATE",
        ProvenanceClass::MetadataOnly => "METADATA_ONLY",
    }
}

fn provenance_of(s: &str) -> ProvenanceClass {
    match s {
        "DEGRADED" => ProvenanceClass::Degraded,
        "APPROXIMATE" => ProvenanceClass::Approximate,
        "METADATA_ONLY" => ProvenanceClass::MetadataOnly,
        // Anything else, including 'EXACT'. A CHECK constraint guards the
        // column, so an unknown value here is a schema change we have not seen;
        // claiming EXACT for it would over-state provenance, but the column
        // cannot hold anything else, so this arm is unreachable in practice.
        _ => ProvenanceClass::Exact,
    }
}

fn origin_of(s: &str) -> Origin {
    // Invariant #13 fails safe: anything that is not literally 'USER' is
    // treated as self-written, i.e. barred from supporting a claim.
    if s == "USER" {
        Origin::User
    } else {
        Origin::SelfWritten
    }
}

/// The same JSON shape `ir_nodes.source_span` holds, so a hit's span is
/// byte-identical to the canonical one.
fn span_json(span: &SourceSpan) -> String {
    // `SourceSpan` is a plain enum of owned data, so serialization cannot fail
    // for any value it can hold. If it somehow did, `Whole` is the honest
    // fallback: "somewhere in this file" is a weaker citation, not a wrong one,
    // and `SourceSpan::is_precise` already tells callers that.
    serde_json::to_string(span).unwrap_or_else(|e| {
        tracing::error!(error = %e, "source span would not serialize; storing Whole");
        r#"{"kind":"whole"}"#.to_string()
    })
}

fn parse_span(json: &str) -> SourceSpan {
    serde_json::from_str(json).unwrap_or_else(|e| {
        // Invariant #1 fails safe rather than loudly: a span we cannot decode
        // must not become a *confident wrong* location.
        tracing::warn!(error = %e, "unreadable source span in the text index");
        SourceSpan::Whole
    })
}

/// Strip the snippet markers from text before it is indexed, so a marker in a
/// returned snippet can only have come from FTS5.
fn sanitize(s: &str) -> String {
    if s.contains(MARK_OPEN) || s.contains(MARK_CLOSE) {
        s.replace([MARK_OPEN, MARK_CLOSE], " ")
    } else {
        s.to_string()
    }
}

// --------------------------------------------------------------- expression

/// Split untrusted query text into tokens the way `unicode61` would.
///
/// Everything that is not alphanumeric is a separator — which is exactly
/// `unicode61`'s default classification, so the terms we emit are the terms the
/// index actually holds. Quotes, `*`, `NOT`, `NEAR/5`, backslashes and
/// unbalanced anything all fall out as either ordinary tokens or nothing.
fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            if cur.chars().count() < MAX_TERM_CHARS {
                cur.push(ch);
            }
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Quote one token as an FTS5 string literal.
///
/// FTS5 string syntax is `"..."` with `""` for an embedded quote. Our tokens
/// are alphanumeric by construction so no escape is ever needed — the doubling
/// is here anyway, because a "cannot happen" that is one refactor away from
/// happening is not a safety argument.
fn quote(token: &str) -> String {
    let mut s = String::with_capacity(token.len() + 2);
    s.push('"');
    for ch in token.chars() {
        if ch == '"' {
            s.push('"');
        }
        s.push(ch);
    }
    s.push('"');
    s
}

/// Build the FTS5 `MATCH` expression for a query.
///
/// The result contains only quoted strings and operators this function wrote.
/// User input reaches it exclusively as the contents of a quoted string.
pub fn match_expression(q: &TextQuery) -> Result<String> {
    let tokens = tokenize(&q.text);
    if tokens.is_empty() {
        // §108 has no query-input class. CFG_INVALID is the closest and says
        // the right thing: what this build was handed is not usable input.
        return Err(Error::new(
            Code::CfgInvalid,
            // Deliberately names no flag and no tool. This is a library, and
            // three surfaces show this message — a CLI flag, an MCP tool and a
            // desktop control. Each names its own; a message that names one of
            // them is wrong on the other two, which is a suggestion that leads
            // nowhere.
            "A search needs at least one letter or digit. Type a word to search for, or run \
             an exact scan to match punctuation.",
        )
        .with_context(format!("query had {} characters, 0 terms", q.text.len())));
    }
    if tokens.len() > MAX_QUERY_TERMS {
        return Err(Error::new(
            Code::CfgInvalid,
            "That search has too many words to run as a lexical query. Shorten it to the terms \
             that matter, or use `--literal` to scan for the text exactly.",
        )
        .with_context(format!("{} terms, limit {MAX_QUERY_TERMS}", tokens.len())));
    }

    let body = match q.mode {
        MatchMode::Phrase => {
            // One quoted string containing every token: FTS5 reads a
            // multi-token string as an ordered adjacent phrase.
            quote(&tokens.join(" "))
        }
        MatchMode::Terms => tokens
            .iter()
            .map(|t| quote(t))
            .collect::<Vec<_>>()
            .join(" AND "),
        MatchMode::Any => tokens
            .iter()
            .map(|t| quote(t))
            .collect::<Vec<_>>()
            .join(" OR "),
        MatchMode::Prefix => {
            let last = tokens.len() - 1;
            tokens
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    if i == last {
                        format!("{}*", quote(t))
                    } else {
                        quote(t)
                    }
                })
                .collect::<Vec<_>>()
                .join(" AND ")
        }
    };

    let fields = q.effective_fields();
    if fields.len() == TextField::ALL.len() {
        Ok(format!("({body})"))
    } else {
        // `{col ...} : (expr)`. Column names are fixed identifiers from
        // `TextField`, never user input.
        let cols = fields
            .iter()
            .map(|f| f.column())
            .collect::<Vec<_>>()
            .join(" ");
        Ok(format!("{{{cols}}} : ({body})"))
    }
}

// ------------------------------------------------------------------- writes

/// Insert or replace `docs`. **Call this inside the same transaction as the
/// canonical rows the docs derive from** (D3).
pub fn upsert_docs(conn: &Connection, docs: &[TextDoc]) -> Result<()> {
    if docs.is_empty() {
        return Ok(());
    }
    // Replace rather than update: the FTS5 row and the doc row must move
    // together, and the delete trigger already knows how to retire a doc.
    let mut del = conn
        .prepare_cached(&format!("DELETE FROM {DOCS_TABLE} WHERE chunk_id = ?1"))
        .map_err(index_missing("Could not prepare the text index update."))?;
    let mut ins_doc = conn
        .prepare_cached(&format!(
            "INSERT INTO {DOCS_TABLE}
               (chunk_id, file_id, version_id, workspace_id, path, extension,
                source_span, provenance_class, origin, modified_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"
        ))
        .map_err(index_missing("Could not prepare the text index update."))?;
    let mut ins_fts = conn
        .prepare_cached(&format!(
            "INSERT INTO {FTS_TABLE} (rowid, path, title, body) VALUES (?1, ?2, ?3, ?4)"
        ))
        .map_err(index_missing("Could not prepare the text index update."))?;

    for d in docs {
        let write = "Could not write a document to the text index.";
        del.execute([d.chunk_id.to_string()])
            .map_err(|e| map_sqlite(e, write).with_context(format!("chunk {}", d.chunk_id)))?;
        ins_doc
            .execute(params![
                d.chunk_id.to_string(),
                d.file_id.to_string(),
                d.version_id.to_string(),
                d.workspace_id.to_string(),
                d.path,
                d.extension(),
                span_json(&d.span),
                provenance_sql(d.provenance),
                marrow_store::read::origin_sql(d.origin),
                d.modified.as_millis(),
            ])
            .map_err(|e| map_sqlite(e, write).with_context(format!("chunk {}", d.chunk_id)))?;
        let doc_id = conn.last_insert_rowid();
        ins_fts
            .execute(params![
                doc_id,
                sanitize(&d.path),
                sanitize(&d.title),
                sanitize(&d.body),
            ])
            .map_err(|e| map_sqlite(e, write).with_context(format!("chunk {}", d.chunk_id)))?;
    }
    tracing::trace!(docs = docs.len(), "text index upsert");
    Ok(())
}

/// Remove documents by chunk id. Unknown ids are not an error.
pub fn delete_docs(conn: &Connection, ids: &[ChunkId]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut removed = 0usize;
    for batch in ids.chunks(DELETE_CHUNK) {
        let holes = std::iter::repeat_n("?", batch.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!("DELETE FROM {DOCS_TABLE} WHERE chunk_id IN ({holes})");
        let args: Vec<String> = batch.iter().map(|i| i.to_string()).collect();
        let refs: Vec<&dyn ToSql> = args.iter().map(|s| s as &dyn ToSql).collect();
        removed += conn.execute(&sql, refs.as_slice()).map_err(index_missing(
            "Could not remove documents from the text index.",
        ))?;
    }
    tracing::debug!(asked = ids.len(), removed, "text index delete");
    Ok(())
}

/// Drop every document, then re-derive from `src`. Runs in the caller's
/// transaction, so a failure leaves the previous index untouched.
pub fn rebuild(conn: &Connection, src: &dyn ChunkSource) -> Result<()> {
    clear(conn)?;
    let mut n = 0u64;
    src.for_each_chunk(&mut |doc| {
        upsert_docs(conn, std::slice::from_ref(&doc))?;
        n += 1;
        Ok(())
    })?;
    tracing::info!(docs = n, "text index rebuilt from canonical state");
    Ok(())
}

/// Empty the index without touching canonical state.
pub fn clear(conn: &Connection) -> Result<()> {
    conn.execute(&format!("DELETE FROM {DOCS_TABLE}"), [])
        .map_err(index_missing("Could not clear the text index."))?;
    // The trigger removes matching FTS5 rows; this catches any that predate it
    // or were written by a build with a different sync strategy.
    conn.execute(&format!("DELETE FROM {FTS_TABLE}"), [])
        .map_err(index_missing("Could not clear the text index."))?;
    Ok(())
}

/// How many documents are indexed.
pub fn doc_count(conn: &Connection) -> Result<u64> {
    let n: i64 = conn
        .query_row(&format!("SELECT count(*) FROM {DOCS_TABLE}"), [], |r| {
            r.get(0)
        })
        .map_err(index_missing(
            "Could not count the documents in the text index.",
        ))?;
    Ok(n.max(0) as u64)
}

/// Map "no such table" onto a rebuild instruction rather than an internal error.
fn index_missing(message: &'static str) -> impl Fn(rusqlite::Error) -> Error {
    move |e| {
        let text = e.to_string();
        if text.contains("no such table") {
            Error::new(
                Code::IdxRebuildRequired,
                "The text index is missing and has to be rebuilt before search will work. Run \
                 `marrow reindex`, or delete the index directory to rebuild from your files.",
            )
            .with_context(text)
        } else {
            map_sqlite(e, message)
        }
    }
}

// ------------------------------------------------------------------ searching

/// Run a lexical query against `conn`.
pub fn search(conn: &Connection, q: &TextQuery) -> Result<Vec<TextHit>> {
    let expr = match_expression(q)?;
    let fields = q.effective_fields();
    // `snippet()` takes -1 for "pick the best column". Pin it when the caller
    // scoped the query to exactly one field, so the snippet is from the field
    // they asked about.
    let snippet_col = match q.snippet.column {
        // An explicit choice always wins: the caller knows whether the snippet
        // is for a human to skim or for a model to read.
        Some(f) => f.column_index(),
        None if fields.len() == 1 => fields[0].column_index(),
        // FTS5 picks the best-matching column, which is what a result row
        // wants and is a trap for anything else. See `SnippetOptions::column`.
        None => -1,
    };
    let tokens = q.snippet.tokens.clamp(1, MAX_SNIPPET_TOKENS) as i64;
    // `limit = 0` means zero results, which is a coherent request; the upper
    // bound is here because a caller asking for a million candidates has made a
    // mistake and should get a slow answer rather than an unbounded one.
    let limit = q.limit.min(10_000) as i64;

    // Parameters ?1..?9 are numbered because the weights and snippet arguments
    // are positional and must not shift when a filter is appended; the filter
    // parameters below are bare `?`, which SQLite numbers from 10 upwards in
    // the order they appear — the same order they are pushed onto `args`.
    let mut sql = format!(
        "SELECT d.chunk_id, d.file_id, d.version_id, d.workspace_id, d.path, d.source_span,
                d.provenance_class, d.origin, d.modified_ms,
                bm25({FTS_TABLE}, ?1, ?2, ?3) AS bm25_score,
                snippet({FTS_TABLE}, ?4, ?5, ?6, ?7, ?8),
                {FTS_TABLE}.title
           FROM {FTS_TABLE}
           JOIN {DOCS_TABLE} d ON d.doc_id = {FTS_TABLE}.rowid
          WHERE {FTS_TABLE} MATCH ?9"
    );

    let mut args: Vec<Box<dyn ToSql>> = vec![
        Box::new(q.weights.path),
        Box::new(q.weights.title),
        Box::new(q.weights.body),
        Box::new(snippet_col as i64),
        Box::new(MARK_OPEN.to_string()),
        Box::new(MARK_CLOSE.to_string()),
        Box::new(ELLIPSIS.to_string()),
        Box::new(tokens),
        Box::new(expr),
    ];
    push_filters(&mut sql, &mut args, &q.filters);
    // FTS5 bm25 is negative with more-negative meaning better, so ascending is
    // best-first.
    sql.push_str(" ORDER BY bm25_score ASC LIMIT ?");
    args.push(Box::new(limit));

    let mut stmt = conn
        .prepare_cached(&sql)
        .map_err(index_missing("Could not prepare the lexical search."))?;
    let max_chars = q.snippet.max_chars;
    let refs: Vec<&dyn ToSql> = args.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(refs.as_slice(), |row| hit_from_row(row, max_chars))
        .map_err(search_failed)?;

    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(search_failed)?);
    }
    tracing::debug!(hits = out.len(), "lexical search");
    Ok(out)
}

fn search_failed(e: rusqlite::Error) -> Error {
    let text = e.to_string();
    if text.contains("no such table") {
        return Error::new(
            Code::IdxRebuildRequired,
            "The text index is missing and has to be rebuilt before search will work. Run \
             `marrow reindex`, or delete the index directory to rebuild from your files.",
        )
        .with_context(text);
    }
    if text.contains("fts5: syntax error") || text.contains("fts5:") {
        // The expression is built entirely by `match_expression`, so FTS5
        // rejecting it means this build emitted something wrong — a defect
        // here, never the user's input.
        return Error::new(
            Code::IntInvariantViolated,
            "The search could not be run because this build produced an invalid index query. \
             Report this with the detail below; searching for the same words with `--literal` \
             works in the meantime.",
        )
        .with_context(text);
    }
    map_sqlite(e, "The lexical search could not be run.")
}

fn push_filters(sql: &mut String, args: &mut Vec<Box<dyn ToSql>>, f: &Filters) {
    if let Some(ws) = f.workspace {
        sql.push_str(" AND d.workspace_id = ?");
        args.push(Box::new(ws.to_string()));
    }
    if let Some(glob) = &f.path_glob {
        // Bound as a parameter: a glob is data, and GLOB is a SQLite operator,
        // so nothing here can become SQL.
        sql.push_str(" AND d.path GLOB ?");
        args.push(Box::new(glob.clone()));
    }
    if !f.extensions.is_empty() {
        let holes = std::iter::repeat_n("?", f.extensions.len())
            .collect::<Vec<_>>()
            .join(",");
        sql.push_str(&format!(" AND d.extension IN ({holes})"));
        for e in &f.extensions {
            args.push(Box::new(e.trim_start_matches('.').to_ascii_lowercase()));
        }
    }
    if let Some(after) = f.modified_after {
        sql.push_str(" AND d.modified_ms >= ?");
        args.push(Box::new(after.as_millis()));
    }
    if let Some(before) = f.modified_before {
        sql.push_str(" AND d.modified_ms <= ?");
        args.push(Box::new(before.as_millis()));
    }
}

fn hit_from_row(row: &Row<'_>, max_chars: usize) -> rusqlite::Result<TextHit> {
    let chunk_id: String = row.get(0)?;
    let file_id: String = row.get(1)?;
    let version_id: String = row.get(2)?;
    let workspace_id: String = row.get(3)?;
    let path: String = row.get(4)?;
    let span_json: String = row.get(5)?;
    let provenance: String = row.get(6)?;
    let origin: String = row.get(7)?;
    let modified: i64 = row.get(8)?;
    let bm25: f64 = row.get(9)?;
    let raw_snippet: String = row.get(10)?;
    let raw_title: String = row.get(11)?;

    Ok(TextHit {
        chunk_id: parse_id(&chunk_id, "text_index_docs.chunk_id")?,
        file_id: parse_id::<FileId>(&file_id, "text_index_docs.file_id")?,
        version_id: parse_id::<VersionId>(&version_id, "text_index_docs.version_id")?,
        workspace_id: parse_id::<WorkspaceId>(&workspace_id, "text_index_docs.workspace_id")?,
        path,
        title: strip_markers(&raw_title),
        // FTS5's bm25 is negative, better = more negative. Flip it so "higher
        // is better" holds for every consumer.
        score: (-bm25) as f32,
        span: parse_span(&span_json),
        snippet: parse_snippet(&raw_snippet, max_chars),
        provenance: provenance_of(&provenance),
        origin: origin_of(&origin),
        modified: Timestamp::from_millis(modified),
    })
}

fn parse_id<T: std::str::FromStr>(s: &str, column: &str) -> rusqlite::Result<T> {
    s.parse::<T>().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{column}: unexpected value {s:?}"),
            )),
        )
    })
}

/// Split FTS5's marked-up snippet into plain text plus match offsets.
///
/// Offsets are byte offsets into the returned text, ascending and
/// non-overlapping. An unbalanced marker (which FTS5 does not produce) is
/// dropped rather than allowed to produce a bogus range.
fn parse_snippet(raw: &str, max_chars: usize) -> Snippet {
    let mut text = String::with_capacity(raw.len());
    let mut matches: Vec<MatchRange> = Vec::new();
    let mut open: Option<usize> = None;
    let mut chars = 0usize;
    for ch in raw.chars() {
        match ch {
            MARK_OPEN => open = Some(text.len()),
            MARK_CLOSE => {
                if let Some(start) = open.take() {
                    if start < text.len() {
                        matches.push(MatchRange {
                            start,
                            end: text.len(),
                        });
                    }
                }
            }
            _ => {
                if chars >= max_chars {
                    // Bounded: FTS5's token window is approximate, this is not.
                    if let Some(start) = open.take() {
                        if start < text.len() {
                            matches.push(MatchRange {
                                start,
                                end: text.len(),
                            });
                        }
                    }
                    break;
                }
                text.push(ch);
                chars += 1;
            }
        }
    }
    Snippet { text, matches }
}

/// Belt and braces on the breadcrumb: [`sanitize`] already removes the markers
/// at index time, so this only fires for rows written by a build that did not.
fn strip_markers(raw: &str) -> String {
    if raw.contains(MARK_OPEN) || raw.contains(MARK_CLOSE) {
        raw.replace([MARK_OPEN, MARK_CLOSE], "")
    } else {
        raw.to_string()
    }
}

// ------------------------------------------------------------- chunk source

/// Canonical chunks, as the index should hold them.
///
/// The definition of "searchable" is one place and it is here: an `ACTIVE`
/// chunk of the `CURRENT` version of an `ACTIVE` file. `rebuild_from` and the
/// ingest path must agree on it or a rebuild silently changes the result set.
pub const CHUNK_SOURCE_SQL: &str = "
    SELECT c.chunk_id, f.file_id, fv.version_id, f.workspace_id,
           COALESCE(f.current_path, fv.path_at_observation),
           COALESCE(c.context_prefix, ''), c.text,
           n.source_span, c.provenance_class, f.origin, fv.mtime_ms
      FROM chunks c
      JOIN file_versions fv ON fv.version_id = c.version_id
      JOIN files f          ON f.file_id     = fv.file_id
      LEFT JOIN ir_nodes n  ON n.node_id     = c.root_node_id
     WHERE c.status = 'ACTIVE'
       AND fv.status = 'CURRENT'
       AND f.status = 'ACTIVE'
     ORDER BY c.chunk_id";

/// A [`ChunkSource`] over the canonical tables of one connection.
#[derive(Debug)]
pub struct StoreChunkSource<'a> {
    conn: &'a Connection,
}

impl<'a> StoreChunkSource<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}

impl ChunkSource for StoreChunkSource<'_> {
    fn for_each_chunk(&self, sink: &mut dyn FnMut(TextDoc) -> Result<()>) -> Result<()> {
        let mut stmt = self
            .conn
            .prepare(CHUNK_SOURCE_SQL)
            .map_err(|e| map_sqlite(e, "Could not read chunks to rebuild the text index."))?;
        let mut rows = stmt
            .query([])
            .map_err(|e| map_sqlite(e, "Could not read chunks to rebuild the text index."))?;
        loop {
            let row = rows
                .next()
                .map_err(|e| map_sqlite(e, "Could not read a chunk while rebuilding the index."))?;
            let Some(row) = row else { break };
            let doc = doc_from_canonical_row(row)
                .map_err(|e| map_sqlite(e, "A canonical chunk row could not be decoded."))?;
            sink(doc)?;
        }
        Ok(())
    }
}

fn doc_from_canonical_row(row: &Row<'_>) -> rusqlite::Result<TextDoc> {
    let chunk_id: String = row.get(0)?;
    let file_id: String = row.get(1)?;
    let version_id: String = row.get(2)?;
    let workspace_id: String = row.get(3)?;
    let path: String = row.get(4)?;
    let title: String = row.get(5)?;
    let body: String = row.get(6)?;
    let span: Option<String> = row.get(7)?;
    let provenance: String = row.get(8)?;
    let origin: String = row.get(9)?;
    let modified: i64 = row.get(10)?;
    Ok(TextDoc {
        chunk_id: parse_id(&chunk_id, "chunks.chunk_id")?,
        file_id: parse_id(&file_id, "files.file_id")?,
        version_id: parse_id(&version_id, "file_versions.version_id")?,
        workspace_id: parse_id(&workspace_id, "files.workspace_id")?,
        path,
        title,
        body,
        // A chunk with no IR root node has no location finer than the file. That
        // is honest (`Whole` is not "precise", and `SourceSpan::is_precise`
        // says so) rather than a fabricated byte range.
        span: span.map(|s| parse_span(&s)).unwrap_or(SourceSpan::Whole),
        provenance: provenance_of(&provenance),
        origin: origin_of(&origin),
        modified: Timestamp::from_millis(modified),
    })
}

// ------------------------------------------------------------------- adapter

/// The [`TextIndex`] implementation over a [`Store`].
///
/// Writes go through the store's single-writer actor; reads go through one
/// `query_only` connection. **The ingest path should not use `upsert`/`delete`
/// here** — it should call [`upsert_docs`] inside the same writer closure as
/// the canonical write, so index and canonical state commit together (D3).
/// These methods exist for the index-only operations: rebuild, maintenance,
/// and the `query` crate's read path.
#[derive(Debug)]
pub struct Fts5Index {
    writer: Writer,
    // `ReadConn` is Send but not Sync (it wraps a raw SQLite connection), and
    // the port is `Send + Sync`. One mutex-guarded reader rather than a pool:
    // M1 has one search at a time, and a pool is the kind of thing that gets
    // added because it sounds right rather than because a benchmark asked.
    reader: Mutex<ReadConn>,
}

impl Fts5Index {
    /// Open the index over `store`, installing its tables if needed.
    pub fn open(store: &Store) -> Result<Self> {
        // `send` + `flush` rather than `submit`: the migration must be
        // committed before the reader connection is opened, and waiting for the
        // writer's batch interval to expire on its own would make opening the
        // index take as long as that interval.
        let pending = store.writer().send(ensure_installed)?;
        store.flush()?;
        pending.wait()?;
        Ok(Self {
            writer: store.writer().clone(),
            reader: Mutex::new(store.reader()?),
        })
    }

    fn reader(&self) -> Result<std::sync::MutexGuard<'_, ReadConn>> {
        self.reader.lock().map_err(|_| {
            Error::new(
                Code::IntInvariantViolated,
                "The text index read connection was left in a broken state by a panic. \
                 Restart Marrow.",
            )
        })
    }
}

impl TextIndex for Fts5Index {
    fn upsert(&self, docs: &[TextDoc]) -> Result<()> {
        if docs.is_empty() {
            return Ok(());
        }
        let docs = docs.to_vec();
        self.writer.submit(move |conn| upsert_docs(conn, &docs))
    }

    fn delete(&self, ids: &[ChunkId]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let ids = ids.to_vec();
        self.writer.submit(move |conn| delete_docs(conn, &ids))
    }

    fn search(&self, q: &TextQuery) -> Result<Vec<TextHit>> {
        let reader = self.reader()?;
        search(&reader, q)
    }

    /// Rebuild from canonical state.
    ///
    /// The clear and each batch of documents are separate writer batches, so
    /// this is **not** atomic: a crash halfway leaves a partial index. That is
    /// the correct trade — holding one transaction open would mean materializing
    /// every document in memory first, and a partial derived index is fixed by
    /// running the rebuild again, which is what this method is.
    fn rebuild_from(&self, src: &dyn ChunkSource) -> Result<()> {
        self.writer.submit(clear)?;
        let mut batch: Vec<TextDoc> = Vec::with_capacity(REBUILD_BATCH);
        let mut total = 0u64;
        let flush = |batch: &mut Vec<TextDoc>| -> Result<()> {
            if batch.is_empty() {
                return Ok(());
            }
            let docs = std::mem::take(batch);
            self.writer.submit(move |conn| upsert_docs(conn, &docs))
        };
        src.for_each_chunk(&mut |doc| {
            batch.push(doc);
            total += 1;
            if batch.len() >= REBUILD_BATCH {
                flush(&mut batch)?;
            }
            Ok(())
        })?;
        flush(&mut batch)?;
        tracing::info!(docs = total, "text index rebuilt");
        Ok(())
    }

    fn doc_count(&self) -> Result<u64> {
        let reader = self.reader()?;
        doc_count(&reader)
    }
}

/// Rebuild straight from the canonical tables of the store this index sits on.
///
/// The convenience form of `rebuild_from(&StoreChunkSource::new(conn))`, run
/// inside a single writer transaction — read and write on the same connection,
/// different tables.
pub fn rebuild_from_store(store: &Store) -> Result<()> {
    store.writer().submit(|conn| {
        let src = StoreChunkSource::new(conn);
        rebuild(conn, &src)
    })?;
    store.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(text: &str) -> TextQuery {
        TextQuery::new(text)
    }

    #[test]
    fn a_natural_language_question_becomes_a_disjunction_not_a_conjunction() {
        // The failure this mode exists for. "When does the lease renew?" as a
        // conjunctive query needs a document containing *when*, *does*, *the*,
        // *lease* and *renew*; the lease says "renews", so every other mode
        // matches nothing — and nothing is the one answer a retrieval layer
        // must not give when the document is right there.
        let e = match_expression(&q("when does the lease renew").mode(MatchMode::Any)).unwrap();
        assert_eq!(e, r#"("when" OR "does" OR "the" OR "lease" OR "renew")"#);
        // Precision is what it trades, not ordering: BM25 sums per-term
        // contributions, so a document matching more terms still ranks higher.
        let conjunctive = match_expression(&q("when does the lease renew")).unwrap();
        assert!(conjunctive.contains(" AND "), "the default is unchanged");
    }

    #[test]
    fn any_mode_quotes_its_terms_like_every_other_mode() {
        // The disjunction must not become a way to smuggle FTS5 syntax in.
        let e = match_expression(&q(r#"a OR b NEAR/2 c"#).mode(MatchMode::Any)).unwrap();
        assert!(!e.contains('/'), "{e}");
        assert_eq!(e.matches(" OR ").count(), e.matches('"').count() / 2 - 1);
    }

    #[test]
    fn expressions_contain_only_operators_we_wrote() {
        let e = match_expression(&q("auth refresh")).unwrap();
        assert_eq!(e, r#"("auth" AND "refresh")"#);
        let e = match_expression(&q("auth refresh").phrase()).unwrap();
        assert_eq!(e, r#"("auth refresh")"#);
        let e = match_expression(&q("auth ref").prefix()).unwrap();
        assert_eq!(e, r#"("auth" AND "ref"*)"#);
    }

    #[test]
    fn hostile_input_becomes_quoted_terms() {
        for hostile in [
            r#"" OR "a" NEAR/5 "b"#,
            "NOT",
            "a OR b",
            r#"x" ) ; DROP TABLE files; --"#,
            "*",
            "^caret",
        ] {
            let e = match_expression(&q(hostile)).unwrap_or_else(|_| String::new());
            // Every quote in the expression is one we emitted around a token,
            // and there is no bare operator character left anywhere.
            assert!(
                !e.contains(';') && !e.contains('*') && !e.contains('^') && !e.contains('/'),
                "{hostile:?} leaked syntax: {e}"
            );
        }
    }

    #[test]
    fn field_scoping_uses_fixed_column_identifiers() {
        let e =
            match_expression(&q("auth").in_fields([TextField::Title, TextField::Path])).unwrap();
        assert_eq!(e, r#"{path title} : ("auth")"#);
    }

    #[test]
    fn empty_and_over_long_queries_are_clean_errors() {
        for bad in ["", "   ", "\t\n", "***", "!!!"] {
            let e = match_expression(&q(bad)).unwrap_err();
            assert_eq!(e.code(), Code::CfgInvalid, "{bad:?}");
            assert!(e.message().len() > 30, "message must explain, not label");
        }
        let many = (0..MAX_QUERY_TERMS + 1)
            .map(|i| format!("t{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let e = match_expression(&q(&many)).unwrap_err();
        assert_eq!(e.code(), Code::CfgInvalid);
    }

    #[test]
    fn snippet_markers_become_offsets_and_leave_no_residue() {
        let raw = format!("the {MARK_OPEN}refresh{MARK_CLOSE} {MARK_OPEN}token{MARK_CLOSE} rots");
        let s = parse_snippet(&raw, 1000);
        assert_eq!(s.text, "the refresh token rots");
        assert!(!s.text.contains(MARK_OPEN) && !s.text.contains(MARK_CLOSE));
        assert_eq!(s.matched_text(), vec!["refresh", "token"]);
    }

    #[test]
    fn snippet_length_is_bounded_and_matches_stay_in_range() {
        let raw = format!("{MARK_OPEN}abc{MARK_CLOSE}{}", "x".repeat(500));
        let s = parse_snippet(&raw, 20);
        assert_eq!(s.text.chars().count(), 20);
        for m in &s.matches {
            assert!(m.end <= s.text.len());
        }
    }

    #[test]
    fn an_unbalanced_marker_is_dropped_rather_than_producing_a_bogus_range() {
        let s = parse_snippet(&format!("a{MARK_OPEN}bc"), 100);
        assert_eq!(s.text, "abc");
        assert!(s.matches.is_empty());
    }

    #[test]
    fn document_text_cannot_smuggle_snippet_markers() {
        let sneaky = format!("evil{MARK_OPEN}here{MARK_CLOSE}now");
        assert!(!sanitize(&sneaky).contains(MARK_OPEN));
        assert!(!sanitize(&sneaky).contains(MARK_CLOSE));
    }

    #[test]
    fn unknown_origin_values_fail_closed_to_self_written() {
        // Invariant #13: if we cannot prove content is the user's, it must not
        // be allowed to support a claim.
        assert_eq!(origin_of("USER"), Origin::User);
        assert_eq!(origin_of("SELF"), Origin::SelfWritten);
        assert_eq!(origin_of("gibberish"), Origin::SelfWritten);
        assert!(!origin_of("").can_support_a_claim());
    }
}
