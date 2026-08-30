//! `marrow search` — the primary surface.
//!
//! Rendering follows [UX §4]. The anatomy is deliberate: `path:line` on its own
//! line and first, because the eye scans a left-aligned column of paths far
//! faster than paths embedded in prose, and because that format is the one
//! editors and terminals linkify.
//!
//! [UX §4]: ../../../docs/UX.md

use std::io::Write;
use std::path::Path;

use marrow_core::{Origin, ProvenanceClass, Result, SourceSpan, Timestamp};
use marrow_index::{Embedding, MatchMode, Snippet, SqliteVectorIndex, TextIndex, VectorIndex};
use marrow_query::search::{BranchRank, Hit, SearchRequest, LEXICAL, SEMANTIC};
use marrow_store::rusqlite::types::ToSql;
use marrow_store::Store;

use crate::filters::Filters;
use crate::render::{self, Style};

/// The semantic branch's inputs, when this machine has them.
///
/// `None` is the ordinary state and never an error: no embedding model, no MLX
/// runtime, or a backfill that has not run all mean the same thing here, and
/// hard rule 10 says search answers with none of them. Nothing is printed —
/// a warning on every search on a machine that deliberately has no model is
/// noise, and the branches this search actually ran are reported in the result
/// either way.
///
/// The vector count is checked **before** the model is loaded. Starting a
/// worker takes seconds; spending them to embed a query that will be compared
/// against nothing would make every search on a machine that has never run
/// `marrow embed` slower for no result.
pub fn semantic_branch(store: &Store, data_dir: &Path, query: &str) -> Option<Semantic> {
    let started = std::time::Instant::now();
    let vectors = SqliteVectorIndex::open(store)
        .map_err(|e| tracing::debug!(error = %e, "no vector index; answering lexically"))
        .ok()?;
    if vectors.doc_count().ok()? == 0 {
        return None;
    }
    let model = vectors.model_id().ok()?;
    let embedder = crate::embed::try_open_embedder(store, data_dir, model.as_deref())?;
    let embedding = embedder
        .embed_one(query)
        .map_err(
            |e| tracing::debug!(error = %e, "the query could not be embedded; answering lexically"),
        )
        .ok()?;
    Some(Semantic {
        vectors,
        embedding,
        prepared_in: started.elapsed(),
    })
}

/// The semantic branch, and what it cost to get it.
///
/// The cost travels with it because the fused search is milliseconds and
/// loading the model is seconds: timing only the search would print `1.2 s` at
/// the end of a command the user waited eight seconds for, which is the same
/// class of untruth as counts with no freshness.
pub struct Semantic {
    vectors: SqliteVectorIndex,
    embedding: Embedding,
    prepared_in: std::time::Duration,
}

/// One indexed search, as the command line asked for it.
///
/// A struct rather than ten positional parameters: filters made the argument
/// list long enough that `run(store, index, sem, q, 20, &roots, false, true, …)`
/// stopped being readable at the call site, and two adjacent `bool`s that mean
/// `--json` and `--explain` are a swap waiting to happen.
pub struct Request<'a> {
    pub query: &'a str,
    pub limit: usize,
    /// Narrowing, already resolved. Applied *inside* the query, never to its
    /// results — see [`crate::filters`].
    pub filters: &'a Filters,
    /// Workspace roots, for rendering paths relative to them.
    pub roots: &'a [String],
    pub json: bool,
    pub explain: bool,
}

/// Render results, or a diagnosis when there are none.
pub fn run(
    store: &Store,
    index: &dyn TextIndex,
    semantic: Option<&Semantic>,
    req: &Request<'_>,
    style: Style,
    out: &mut impl Write,
) -> Result<()> {
    let query = req.query;
    // The surface names its own escape hatch. The index rejects a query that
    // tokenizes to nothing, but its message cannot know whether the caller has
    // a flag, a tool or a button — so it names none and each surface names its.
    if !query.chars().any(char::is_alphanumeric) {
        return Err(marrow_core::Error::new(
            marrow_core::Code::CfgInvalid,
            format!(
                // Single-quoted, because the patterns that land here are
                // exactly the ones a shell would eat: `});`, `$foo`, `*`.
                "The index searches words, so `{query}` cannot be expressed as one. \
                 Run `marrow search --literal '{query}'` — it reads the files themselves \
                 and matches punctuation exactly."
            ),
        ));
    }

    let started = std::time::Instant::now();
    // `marrow-query` rather than the index directly: fusion, the §113.3
    // multipliers and the hydration a semantic-only hit needs all live there
    // and are tested there. Calling the index alone is what made `marrow embed`
    // change nothing that this surface showed.
    //
    // The filters ride on the request, so the index applies them *before*
    // `limit`. Narrowing the returned page instead would let a `--type html`
    // search report nothing while HTML matches sat at rank 101.
    let search = SearchRequest::new(query)
        .mode(MatchMode::Terms)
        .limit(req.limit)
        .filters(req.filters.search.clone());
    let branch = semantic.map(|s| (&s.vectors as &dyn VectorIndex, &s.embedding));
    let results = marrow_query::search::search_hybrid(store, index, branch, &search)?;
    // See `Filters::admits`: the semantic branch is filtered by workspace and
    // nothing else, so a chunk only it found has passed no extension, path or
    // date test. This removes those and only those.
    let ran = results.branches.clone();
    let hits: Vec<Hit> = results
        .hits
        .into_iter()
        .filter(|h| req.filters.admits(&h.path, h.modified))
        .collect();

    // **Why these results, in the ranking's own terms.** `explain` has existed
    // in `marrow-query`, tested, with no way to reach it — a ranking nobody can
    // interrogate is one you can only trust or distrust wholesale, and every
    // retrieval bug this week was found by someone asking "why did it return
    // *that*". Rendered from the same hits the answer used, so it explains the
    // search that ran rather than a second one that might disagree.
    if req.explain {
        let ex = marrow_query::explain::explain(&search, &ran, &hits);
        if req.json {
            return render_explanation_json(&ex, req.filters, out);
        }
        return render_explanation(&ex, req.filters, req.roots, style, out);
    }

    // IDX-001: a file must be findable by its name. Content search alone cannot
    // do that — a file with no parseable content has no chunks and therefore no
    // index document, so `marrow index` could truthfully report "still findable
    // by name" about a file that was not findable at all.
    //
    // Filtered by the same filters as the content search. A `--type html`
    // search that answered with a `.pdf` because its *name* matched would be
    // reporting a result the reader had explicitly excluded.
    //
    // This branch belongs in `marrow-query` alongside the fusion machinery; it
    // lives here until the CLI is moved onto that crate.
    let by_name = path_matches(store, query, req.limit, &hits, req.filters)?;
    // Everything the user waited for, model load included.
    let setup = semantic.map(|s| s.prepared_in.as_millis()).unwrap_or(0);
    let elapsed = started.elapsed().as_millis() + setup;

    if req.json {
        return render_json(&hits, &by_name, &results.branches, setup, req, elapsed, out);
    }
    if hits.is_empty() && by_name.is_empty() {
        // Cheap: a count on the vector table, no model started. The
        // suggestion is only worth printing if there is something to search.
        let semantic_available = SqliteVectorIndex::open(store)
            .and_then(|v| v.doc_count())
            .map(|n| n > 0)
            .unwrap_or(false);
        // Only asked when filters were applied, because that is the one case
        // where the number changes the diagnosis. A failure is not worth
        // failing the command over — the screen is correct without the extra
        // line — so it degrades to "unknown" with a note in the log.
        let outside = if req.filters.is_empty() {
            None
        } else {
            count_without_filters(store, index, semantic, query)
                .map_err(|e| tracing::debug!(error = %e, "could not count outside the filters"))
                .ok()
        };
        return render_nothing(
            query,
            req.filters,
            &results.branches,
            outside,
            semantic_available,
            elapsed,
            style,
            out,
        );
    }
    render_hits(
        &hits,
        &by_name,
        &results.branches,
        setup,
        req,
        elapsed,
        style,
        out,
    )
}

/// What the same query finds with every filter taken off.
///
/// `count` is **not** always a total, and the difference is the whole reason
/// this type exists rather than a bare `usize`. Retrieval is bounded — the
/// index takes a `LIMIT` and the port exposes no count-of-matches — so a number
/// read off a result set is a fact about what was returned, and printing it as
/// "11 matches" when eleven is merely as far as the probe looked is the exact
/// class of untruth this whole change is about. `capped` says which one it is,
/// and the renderer says "at least" when it must.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Outside {
    count: usize,
    /// The probe reached its own ceiling, so `count` is a floor.
    capped: bool,
}

/// The same query with every filter taken off, and how much it finds.
///
/// Only ever run when a filtered search returned nothing, because that is the
/// one moment the number changes what the answer *means*: "nothing matched"
/// and "nothing matched inside these filters, though eleven things matched
/// outside them" send the reader to two different places, and the screen this
/// replaces said the first while meaning the second.
///
/// Probed at [`CANDIDATE_DEPTH`] rather than at the user's `-n`. Two reasons,
/// and the second is the important one: it is the depth both branches retrieve
/// at anyway, so asking for it costs nothing extra — and probing at `-n` would
/// have made the answer to "how much did my filter exclude?" depend on how many
/// results the reader happened to ask to see, which is not a fact about their
/// corpus at all.
///
/// The semantic branch is reused rather than re-prepared — the model is already
/// loaded and the query already embedded — so this asks the identical question
/// of the identical branches and its count is comparable rather than merely
/// suggestive.
fn count_without_filters(
    store: &Store,
    index: &dyn TextIndex,
    semantic: Option<&Semantic>,
    query: &str,
) -> Result<Outside> {
    let probe = marrow_query::search::CANDIDATE_DEPTH;
    let search = SearchRequest::new(query)
        .mode(MatchMode::Terms)
        .limit(probe);
    let branch = semantic.map(|s| (&s.vectors as &dyn VectorIndex, &s.embedding));
    let results = marrow_query::search::search_hybrid(store, index, branch, &search)?;
    let by_name = path_matches(store, query, probe, &results.hits, &Filters::default())?;
    Ok(Outside {
        count: results.hits.len() + by_name.len(),
        // Either half reaching the ceiling means the corpus had more to give.
        capped: results.hits.len() >= probe || by_name.len() >= probe,
    })
}

/// The branches that ran, as one word each.
///
/// Printed on every search, not only when both ran. A user who has spent two
/// hours on `marrow embed` has no other way to tell whether it is being used,
/// and the answer "it is not, because no model is installed on this machine"
/// is one they can only act on if it is said.
fn branch_line(branches: &[&'static str]) -> String {
    branches.join(" + ")
}

/// A file matched by its path rather than its contents.
#[derive(Debug)]
pub struct NameHit {
    pub path: String,
    pub modified: Timestamp,
    /// True when the file has no searchable contents at all — worth saying,
    /// because it explains why only the name matched.
    pub metadata_only: bool,
}

/// Files whose path contains the query, excluding ones already found by
/// content, and narrowed by the same filters the content search used.
///
/// The filter clauses are appended to the SQL rather than applied to the rows
/// that come back, for the reason the whole feature exists: `LIMIT` runs first,
/// so filtering the result would throw away most of a page and call what
/// remained the answer.
fn path_matches(
    store: &Store,
    query: &str,
    limit: usize,
    already: &[Hit],
    filters: &Filters,
) -> Result<Vec<NameHit>> {
    let needle = query.trim();
    if needle.is_empty() {
        return Ok(Vec::new());
    }
    let seen: std::collections::HashSet<&str> = already.iter().map(|h| h.path.as_str()).collect();

    // Every value is bound; nothing the user typed becomes SQL. The numbered
    // `?1` is the query, and the filter clauses and the limit take the
    // anonymous holes after it in push order — the same shape `fts5.rs` uses
    // for the identical job.
    let mut sql = String::from(
        "SELECT f.current_path, COALESCE(v.mtime_ms, 0),
                (SELECT count(*) FROM chunks c WHERE c.version_id = v.version_id)
           FROM files f
      LEFT JOIN file_versions v
             ON v.file_id = f.file_id AND v.status = 'CURRENT'
          WHERE f.status = 'ACTIVE'
            AND f.current_path IS NOT NULL
            AND lower(f.current_path) LIKE '%' || lower(?1) || '%'",
    );
    let mut args: Vec<Box<dyn ToSql>> = vec![Box::new(needle.to_string())];

    if let Some(ext) = &filters.search.extension {
        // `'%.' || ext` rather than a match anywhere in the name, so `--type ml`
        // does not claim every `.html` file on the disk.
        sql.push_str(" AND lower(f.current_path) LIKE '%.' || ?");
        args.push(Box::new(ext.trim_start_matches('.').to_ascii_lowercase()));
    }
    if let Some(sub) = filters.path_substring() {
        sql.push_str(" AND lower(f.current_path) LIKE '%' || lower(?) || '%'");
        args.push(Box::new(sub.to_string()));
    }
    if let Some(name) = &filters.search.workspace {
        // Resolved through `marrow-query` so a name that matches nothing raises
        // the same error here as it does for the content search — and so the
        // case-insensitive second pass its resolver does applies to both halves
        // of one search rather than to one of them.
        sql.push_str(" AND f.workspace_id = ?");
        args.push(Box::new(
            marrow_query::search::workspace_id_for(store, name)?.to_string(),
        ));
    }
    if let Some(after) = filters.search.modified_after {
        sql.push_str(" AND COALESCE(v.mtime_ms, 0) >= ?");
        args.push(Box::new(after.as_millis()));
    }
    if let Some(before) = filters.search.modified_before {
        sql.push_str(" AND COALESCE(v.mtime_ms, 0) <= ?");
        args.push(Box::new(before.as_millis()));
    }
    sql.push_str(" ORDER BY length(f.current_path) LIMIT ?");
    args.push(Box::new(limit as i64 * 2));

    let conn = store.reader()?;
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| marrow_store::map_sqlite(e, "searching by name"))?;

    let refs: Vec<&dyn ToSql> = args.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(refs.as_slice(), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .and_then(|it| it.collect::<std::result::Result<Vec<_>, _>>())
        .map_err(|e| marrow_store::map_sqlite(e, "searching by name"))?;

    Ok(rows
        .into_iter()
        .filter(|(p, _, _)| !seen.contains(p.as_str()))
        .map(|(path, mtime, chunks)| NameHit {
            path,
            modified: Timestamp::from_millis(mtime),
            metadata_only: chunks == 0,
        })
        .take(limit)
        .collect())
}

#[allow(clippy::too_many_arguments)]
fn render_hits(
    hits: &[Hit],
    by_name: &[NameHit],
    branches: &[&'static str],
    model_load_ms: u128,
    req: &Request<'_>,
    elapsed: u128,
    style: Style,
    out: &mut impl Write,
) -> Result<()> {
    let roots = req.roots;
    writeln!(out)?;
    for h in hits {
        let loc = location(&relative_to(&h.path, roots), &h.span);
        let reason = match_reason(&h.branch_ranks);
        let age = age(h.modified);

        // Path first and alone. Right-aligned metadata stays out of the way of
        // the column the eye is scanning.
        let left = render::elide(
            &loc,
            style.width.saturating_sub(reason.len() + age.len() + 6),
        );
        let pad = style
            .width
            .saturating_sub(left.chars().count() + reason.len() + age.len() + 3);
        writeln!(
            out,
            "{}{}{} {}",
            style.bold(&left),
            " ".repeat(pad.max(1)),
            style.dim(&reason),
            style.dim(&age)
        )?;

        for line in snippet_lines(&h.snippet, style.width.saturating_sub(4)) {
            writeln!(out, "  {line}")?;
        }
        if !h.title.is_empty() {
            writeln!(
                out,
                "  {}",
                style.dim(&render::elide(&h.title, style.width.saturating_sub(4)))
            )?;
        }
        // Anything not `Exact` is badged. Silent precision loss is the one
        // thing that would destroy the product's premise (UX §2 principle 6).
        if h.provenance != ProvenanceClass::Exact {
            writeln!(out, "  {}", style.warn("~approx"))?;
        }
        // Invariant #13, made visible: agent-written content is findable but
        // cannot be cited.
        if h.origin == Origin::SelfWritten {
            writeln!(
                out,
                "  {}",
                style.warn("[self] written by Marrow — not evidence")
            )?;
        }
        writeln!(out)?;
    }

    for n in by_name {
        let rel = relative_to(&n.path, roots);
        writeln!(
            out,
            "{}{}{} {}",
            style.bold(&render::elide(&rel, style.width.saturating_sub(18))),
            " ".repeat(style.width.saturating_sub(rel.chars().count() + 14).max(1)),
            style.dim("name"),
            style.dim(&age(n.modified))
        )?;
        if n.metadata_only {
            // Says why only the name matched, rather than leaving the user to
            // wonder why there is no excerpt.
            writeln!(out, "  {}", style.dim("contents not indexed"))?;
        }
        writeln!(out)?;
    }

    let total = hits.len() + by_name.len();
    writeln!(
        out,
        "{}",
        style.dim(&format!(
            "{} result{} · {} · {}{}{}{}",
            total,
            if total == 1 { "" } else { "s" },
            render::duration(elapsed),
            branch_line(branches),
            // What was excluded, on the same line as what was found. A count
            // is not interpretable without it: three results out of a filtered
            // corpus and three out of the whole index are different findings,
            // and only the second is a reason to stop looking.
            if req.filters.is_empty() {
                String::new()
            } else {
                format!(" · {}", req.filters.summary())
            },
            // Named separately because it is nearly all of the wall time and
            // none of the search: without it the same query looks ten times
            // slower than it is, and nobody could tell which half to blame.
            if model_load_ms > 0 {
                format!(" ({} loading the model)", render::duration(model_load_ms))
            } else {
                String::new()
            },
            if by_name.is_empty() {
                String::new()
            } else {
                format!(" · {} by name", by_name.len())
            }
        ))
    )?;
    Ok(())
}

/// The explanation as JSON, with the filters the query ran under.
///
/// `Explanation` is `marrow-query`'s type and has no filter field — filters are
/// resolved on this side of the boundary, so the crate that ranks has never
/// heard of them. Adding the key here keeps the payload complete without
/// inventing a shape in the library for one caller's benefit.
fn render_explanation_json(
    ex: &marrow_query::explain::Explanation,
    filters: &Filters,
    out: &mut impl Write,
) -> Result<()> {
    let mut value = serde_json::to_value(ex).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(obj) = value.as_object_mut() {
        obj.insert("filters".into(), filters.json());
    }
    writeln!(out, "{value}")?;
    Ok(())
}

/// Zero results is a diagnosis, not a shrug ([UX §4]).
/// The ranking, in words, for someone asking why a result is where it is.
fn render_explanation(
    ex: &marrow_query::explain::Explanation,
    filters: &Filters,
    roots: &[String],
    style: Style,
    out: &mut impl Write,
) -> Result<()> {
    writeln!(out)?;
    writeln!(
        out,
        "{} {}",
        style.bold(&ex.query),
        style.dim(&format!("· {} · rrf k={}", ex.mode, ex.rrf_k))
    )?;

    // Before the branches, because a filter changes what the branches were
    // ranking *over*. An explanation that describes the ranking but not its
    // candidate set explains the wrong half of the outcome.
    if !filters.is_empty() {
        writeln!(out)?;
        writeln!(out, "  {}", style.dim("filters"))?;
        writeln!(out, "    {}", filters.summary())?;
    }

    writeln!(out)?;
    writeln!(out, "  {}", style.dim("branches"))?;
    for b in &ex.branches {
        writeln!(
            out,
            "    {:<10} weight {:<5} {:>3} of these hits   {}",
            b.name,
            b.weight,
            b.contributed,
            style.dim(b.ran_because)
        )?;
    }

    writeln!(out)?;
    for h in &ex.hits {
        let where_ = crate::render::elide(&relative_to(&h.path, roots), 64);
        writeln!(out, "  {:>2}. {}", h.rank, style.bold(&where_))?;
        let ranks: Vec<String> = h
            .branch_ranks
            .iter()
            .map(|r| format!("{}#{}", r.branch, r.rank))
            .collect();
        writeln!(
            out,
            "      {}",
            style.dim(&format!(
                "{} → base {:.4}{} → {:.4}",
                ranks.join(" + "),
                h.base_score,
                h.multipliers
                    .iter()
                    .map(|m| format!(" × {:.2} ({})", m.factor, m.reason))
                    .collect::<String>(),
                h.final_score
            ))
        )?;
        // Invariant #13 is the one line here that changes what you may *do*
        // with a result, so it is stated rather than implied by its absence.
        if !h.can_support_a_claim {
            writeln!(
                out,
                "      {}",
                style.warn("written by Marrow itself — cannot support a claim")
            )?;
        }
    }

    // Never omitted: an explanation that hides its own limits invites more
    // confidence than it has earned.
    if !ex.caveats.is_empty() {
        writeln!(out)?;
        writeln!(out, "  {}", style.dim("this cannot tell you"))?;
        for c in &ex.caveats {
            writeln!(out, "    {}", style.dim(c))?;
        }
    }
    Ok(())
}

/// Zero results, diagnosed rather than shrugged at ([UX §4]).
///
/// The filtered case is a *different* finding and gets a different screen. A
/// search narrowed to `--type html` that returned nothing while eleven matches
/// sat outside the filter is not "nothing matched" — it is "your filter is what
/// removed them", and it sends the reader to their command line instead of to
/// `marrow status` wondering what failed to index. The old screen said the
/// second thing while meaning the first.
///
/// `outside` is what the same search finds with the filters taken off, or
/// `None` when there were no filters or the second search failed. It is passed
/// in rather than computed here so this stays what the module says it is — a
/// pure function of data — and so both screens can be tested without a store.
#[allow(clippy::too_many_arguments)]
fn render_nothing(
    query: &str,
    filters: &Filters,
    branches: &[&'static str],
    outside: Option<Outside>,
    semantic_available: bool,
    elapsed: u128,
    style: Style,
    out: &mut impl Write,
) -> Result<()> {
    writeln!(out)?;
    if filters.is_empty() {
        writeln!(out, "No matches for {}", style.bold(query))?;
    } else {
        writeln!(
            out,
            "No matches for {} {}",
            style.bold(query),
            style.dim(&format!("with {}", filters.summary()))
        )?;
    }
    // Which branches looked, not just how long it took. "Nothing matched" from
    // one branch and from two are different findings, and only the second is a
    // reason to stop looking.
    writeln!(
        out,
        "  {}",
        style.dim(&format!(
            "searched {} in {}",
            branch_line(branches),
            render::duration(elapsed)
        ))
    )?;
    match outside {
        Some(o) if o.count > 0 => {
            // "at least" whenever the probe stopped at its own ceiling rather
            // than at the end of the matches. Without that word the number
            // reads as a total, which is a claim about the corpus that a
            // bounded retrieval never made.
            writeln!(
                out,
                "  {}",
                style.warn(&format!(
                    "{}{} match{} without the filters — they are what excluded them",
                    if o.capped { "at least " } else { "" },
                    o.count,
                    if o.count == 1 { "" } else { "es" }
                ))
            )?;
        }
        // Said out loud rather than left as an absence. Without this line, a
        // reader who filtered and found nothing cannot tell whether the filter
        // or the corpus is the reason, which is the entire question they have.
        Some(_) => {
            writeln!(
                out,
                "  {}",
                style.dim("nothing matches without the filters either, so they are not the reason")
            )?;
        }
        None => {}
    }
    writeln!(out)?;
    writeln!(out, "  {}", style.dim("try"))?;
    // The first thing to try when a filter is what emptied the page is the same
    // search without it, and it is offered before the escape hatches because it
    // is the one that most often works.
    if outside.is_some_and(|o| o.count > 0) {
        writeln!(
            out,
            "    marrow search '{query}'   {}",
            style.dim("the same search with no filters")
        )?;
    }
    // Single-quoted for the same reason the error above is: this is the one
    // message whose entire purpose is patterns a shell eats — `});`, `$foo`,
    // `*` — and an unquoted hint is a command that fails when pasted.
    writeln!(
        out,
        "    marrow search --literal '{query}'   exact scan, ignores the index"
    )?;
    // Only when the semantic branch did not already run, and only when there is
    // something for it to search. A suggestion that leads nowhere is the bug
    // this codebase keeps reproducing, so this one is gated on vectors actually
    // existing rather than on the feature existing.
    if !branches.contains(&"semantic") && semantic_available {
        writeln!(
            out,
            "    marrow search --semantic '{query}'   also match on meaning, not words"
        )?;
    }
    writeln!(
        out,
        "    marrow status                     what is and is not indexed"
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn render_json(
    hits: &[Hit],
    by_name: &[NameHit],
    branches: &[&'static str],
    model_load_ms: u128,
    req: &Request<'_>,
    elapsed: u128,
    out: &mut impl Write,
) -> Result<()> {
    // Same data as the human view — a second renderer, not a parallel path.
    let results: Vec<_> = hits
        .iter()
        .enumerate()
        .map(|(i, h)| {
            serde_json::json!({
                "rank": i + 1,
                // The lexical branch's own score, unchanged: a semantic-only
                // hit never had one, and `fused_score` beside it is the number
                // the ranking was actually done on.
                "score": h.hit.score,
                "fused_score": h.fused_score,
                "reasons": match_reasons(&h.branch_ranks),
                "file_id": h.file_id.to_string(),
                "chunk_id": h.chunk_id.to_string(),
                "path": h.path,
                "span": h.span,
                "provenance": format!("{:?}", h.provenance).to_lowercase(),
                "origin": format!("{:?}", h.origin).to_lowercase(),
                "breadcrumb": h.title,
                "preview": h.snippet.text,
                "modified_ms": h.modified.as_millis(),
            })
        })
        .collect();
    let named: Vec<_> = by_name
        .iter()
        .map(|n| {
            serde_json::json!({
                "path": n.path,
                "reasons": ["name"],
                "metadata_only": n.metadata_only,
                "modified_ms": n.modified.as_millis(),
            })
        })
        .collect();
    writeln!(
        out,
        "{}",
        serde_json::json!({
            "schema": "marrow.search/1",
            "by_name": named,
            // What actually ran. A script that pipes this has no other way to
            // tell a machine with a built semantic index from one without.
            "branches": branches,
            "query": req.query,
            // What was excluded, alongside what came back. A result set is not
            // interpretable without it: a consumer counting three results has
            // no way to tell a three-document corpus from a filter that
            // removed the other forty, and an omitted key reads as "no
            // filters" rather than as "unknown".
            "filters": req.filters.json(),
            // The whole command, and the part of it that was the model
            // starting up. Zero whenever the semantic branch did not run, so
            // the difference is the search itself either way.
            "elapsed_ms": elapsed,
            "model_load_ms": model_load_ms,
            "total": hits.len(),
            "results": results,
        })
    )?;
    Ok(())
}

/// Strip the workspace root, so results read `src/auth/token.rs` rather than
/// `/Users/…/Desktop/melp/services/vault/src/auth/token.rs`.
///
/// An absolute path eats the width the snippet needs and buries the part that
/// distinguishes one result from another.
fn relative_to(path: &str, roots: &[String]) -> String {
    roots
        .iter()
        // Longest first: nested roots would otherwise strip the shorter one and
        // leave a misleading prefix.
        .filter(|r| path.starts_with(r.as_str()))
        .max_by_key(|r| r.len())
        .and_then(|r| path.strip_prefix(r.as_str()))
        .map(|s| s.trim_start_matches('/').to_string())
        .unwrap_or_else(|| path.to_string())
}

/// `path:line` where the span knows a line, plain path otherwise.
///
/// The format is the one editors linkify; a citation you cannot open is a
/// screenshot of an answer.
fn location(path: &str, span: &SourceSpan) -> String {
    match span {
        SourceSpan::Lines { start, .. } => format!("{path}:{start}"),
        SourceSpan::Page { page, .. } => format!("{path}:p{page}"),
        SourceSpan::Cells { sheet, range } => format!("{path}:{sheet}!{range}"),
        _ => path.to_string(),
    }
}

/// Why this matched ([UX §2] principle 3), from the branches that returned it.
///
/// Read off the fusion rather than assumed: a hit only the vector branch found
/// is not an exact match and saying it is would be the precision loss this
/// product exists to avoid. `exact` rather than `lexical` because it describes
/// the match to the reader — the word is on the page — where the branch names
/// describe the machinery.
fn match_reasons(branches: &[BranchRank]) -> Vec<&'static str> {
    let mut out = Vec::with_capacity(2);
    if branches.iter().any(|b| b.branch == LEXICAL) {
        out.push("exact");
    }
    if branches.iter().any(|b| b.branch == SEMANTIC) {
        out.push("semantic");
    }
    // A hit that fused from no branch cannot happen; saying "exact" about one
    // would be a claim, and this is the one column that must never overstate.
    if out.is_empty() {
        out.push("unknown");
    }
    out
}

/// The same thing as one column of text.
fn match_reason(branches: &[BranchRank]) -> String {
    match_reasons(branches).join("+")
}

/// Recency as a human judges it. `2026-06-14` makes you do arithmetic.
fn age(modified: Timestamp) -> String {
    let now = Timestamp::now().as_millis();
    let delta = (now - modified.as_millis()).max(0);
    let mins = delta / 60_000;
    match mins {
        _ if mins < 60 => format!("{mins}m"),
        _ if mins < 60 * 24 => format!("{}h", mins / 60),
        _ if mins < 60 * 24 * 28 => format!("{}d", mins / (60 * 24)),
        _ if mins < 60 * 24 * 365 => format!("{}w", mins / (60 * 24 * 7)),
        _ => format!("{}y", mins / (60 * 24 * 365)),
    }
}

/// Snippet text as at most two lines, collapsed and trimmed.
///
/// Two lines is enough to decide; more is a pager, not a search result.
fn snippet_lines(s: &Snippet, width: usize) -> Vec<String> {
    s.text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(2)
        .map(|l| render::elide(l, width))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_span_renders_as_an_openable_location() {
        assert_eq!(
            location(
                "src/auth/token.rs",
                &SourceSpan::Lines {
                    start: 142,
                    end: 144
                }
            ),
            "src/auth/token.rs:142"
        );
    }

    #[test]
    fn a_cell_span_names_the_sheet_and_range() {
        assert_eq!(
            location(
                "q2.xlsx",
                &SourceSpan::Cells {
                    sheet: "Q2".into(),
                    range: "B4:B18".into()
                }
            ),
            "q2.xlsx:Q2!B4:B18"
        );
    }

    #[test]
    fn a_byte_span_falls_back_to_the_plain_path() {
        // Byte offsets are not something a human or an editor can jump to.
        assert_eq!(
            location("notes.md", &SourceSpan::Bytes { start: 10, end: 20 }),
            "notes.md"
        );
    }

    #[test]
    fn paths_render_relative_to_their_workspace_root() {
        let roots = vec!["/Users/x/proj".to_string()];
        assert_eq!(
            relative_to("/Users/x/proj/src/main.rs", &roots),
            "src/main.rs"
        );
    }

    #[test]
    fn the_longest_matching_root_wins() {
        // Nested roots: stripping the shorter one leaves a misleading prefix.
        let roots = vec!["/Users/x/proj".to_string(), "/Users/x/proj/sub".to_string()];
        assert_eq!(relative_to("/Users/x/proj/sub/a.rs", &roots), "a.rs");
    }

    #[test]
    fn a_path_outside_every_root_is_left_alone() {
        let roots = vec!["/Users/x/proj".to_string()];
        assert_eq!(relative_to("/elsewhere/a.rs", &roots), "/elsewhere/a.rs");
    }

    #[test]
    fn age_uses_the_unit_a_human_would() {
        let now = Timestamp::now().as_millis();
        assert!(age(Timestamp::from_millis(now - 30 * 60_000)).ends_with('m'));
        assert!(age(Timestamp::from_millis(now - 5 * 3_600_000)).ends_with('h'));
        assert!(age(Timestamp::from_millis(now - 3 * 86_400_000)).ends_with('d'));
        assert!(age(Timestamp::from_millis(now - 60 * 86_400_000)).ends_with('w'));
    }

    #[test]
    fn snippets_are_capped_at_two_lines() {
        let s = Snippet {
            text: "one\ntwo\nthree\nfour".into(),
            matches: vec![],
        };
        assert_eq!(snippet_lines(&s, 80).len(), 2);
    }

    fn ranks(branches: &[&'static str]) -> Vec<BranchRank> {
        branches
            .iter()
            .enumerate()
            .map(|(i, b)| BranchRank {
                branch: b,
                rank: i + 1,
            })
            .collect()
    }

    #[test]
    fn a_hit_only_the_vector_branch_found_is_not_called_exact() {
        // The badge is a claim about the evidence. Calling a semantic
        // neighbour an exact match is the precision loss this product exists
        // to avoid.
        assert_eq!(match_reasons(&ranks(&[SEMANTIC])), vec!["semantic"]);
        assert_eq!(match_reason(&ranks(&[SEMANTIC])), "semantic");
    }

    #[test]
    fn a_hit_both_branches_found_says_so() {
        assert_eq!(
            match_reasons(&ranks(&[LEXICAL, SEMANTIC])),
            vec!["exact", "semantic"]
        );
        assert_eq!(match_reason(&ranks(&[LEXICAL, SEMANTIC])), "exact+semantic");
    }

    #[test]
    fn a_lexical_hit_reads_the_way_it_always_did() {
        assert_eq!(match_reasons(&ranks(&[LEXICAL])), vec!["exact"]);
    }

    #[test]
    fn the_branches_that_ran_are_named_even_when_there_is_one() {
        assert_eq!(branch_line(&[LEXICAL]), "lexical");
        assert_eq!(branch_line(&[LEXICAL, SEMANTIC]), "lexical + semantic");
    }

    fn nothing(query: &str, filters: &Filters, outside: Option<Outside>) -> String {
        let mut buf = Vec::new();
        render_nothing(
            query,
            filters,
            &[LEXICAL],
            outside,
            false,
            3,
            Style::plain(),
            &mut buf,
        )
        .expect("rendering");
        String::from_utf8(buf).expect("output is utf-8")
    }

    fn typed(args: crate::filters::Args<'_>) -> Filters {
        crate::filters::resolve(args, Timestamp::now()).expect("the fixture's flags resolve")
    }

    #[test]
    fn the_zero_results_hint_survives_being_pasted_into_a_shell() {
        // The whole point of `--literal` is patterns the index cannot express,
        // which are exactly the ones a shell would expand or swallow.
        let text = nothing("});", &Filters::default(), None);
        assert!(
            text.contains("marrow search --literal '});'"),
            "the hint must quote the query: {text}"
        );
    }

    #[test]
    fn zero_results_says_which_branches_looked() {
        let mut buf = Vec::new();
        render_nothing(
            "lease",
            &Filters::default(),
            &[LEXICAL, SEMANTIC],
            None,
            true,
            3,
            Style::plain(),
            &mut buf,
        )
        .expect("rendering");
        let text = String::from_utf8(buf).expect("output is utf-8");
        assert!(text.contains("searched lexical + semantic"), "{text}");
    }

    #[test]
    fn zero_results_from_a_filter_is_a_different_finding_from_an_empty_index() {
        // The bug this fixes: the old screen said "no matches" — a claim about
        // the corpus — when the truthful finding was "no matches inside the
        // filters you gave, and eleven outside them".
        let f = typed(crate::filters::Args {
            extension: Some("html"),
            ..Default::default()
        });
        let text = nothing(
            "lease",
            &f,
            Some(Outside {
                count: 11,
                capped: false,
            }),
        );
        assert!(text.contains("type=html"), "the filter is named: {text}");
        assert!(
            text.contains("11 matches without the filters"),
            "the count outside the filters is stated: {text}"
        );
        assert!(
            text.contains("marrow search 'lease'"),
            "and the search without them is offered: {text}"
        );
    }

    #[test]
    fn a_count_that_only_reached_the_probes_ceiling_is_reported_as_a_floor() {
        // The number comes off a bounded retrieval, so printing it bare would
        // state a total the search never established. "at least" is the whole
        // difference between a measurement and a claim.
        let f = typed(crate::filters::Args {
            extension: Some("html"),
            ..Default::default()
        });
        let text = nothing(
            "lease",
            &f,
            Some(Outside {
                count: 100,
                capped: true,
            }),
        );
        assert!(
            text.contains("at least 100 matches without the filters"),
            "{text}"
        );

        // And the unbounded case must *not* hedge, or the word stops meaning
        // anything where it matters.
        let exact = nothing(
            "lease",
            &f,
            Some(Outside {
                count: 4,
                capped: false,
            }),
        );
        assert!(exact.contains("4 matches without"), "{exact}");
        assert!(!exact.contains("at least"), "{exact}");
    }

    #[test]
    fn a_filter_that_excluded_nothing_says_so_rather_than_staying_silent() {
        // Otherwise a reader cannot tell whether the filter or the corpus is
        // the reason, which is the only question they have at this point.
        let f = typed(crate::filters::Args {
            path: Some("docs"),
            ..Default::default()
        });
        let text = nothing(
            "lease",
            &f,
            Some(Outside {
                count: 0,
                capped: false,
            }),
        );
        assert!(
            text.contains("nothing matches without the filters either"),
            "{text}"
        );
        assert!(
            !text.contains("marrow search 'lease'   "),
            "no point offering an unfiltered search that also finds nothing: {text}"
        );
    }

    #[test]
    fn an_unfiltered_zero_result_screen_is_unchanged() {
        let text = nothing("lease", &Filters::default(), None);
        assert!(text.contains("No matches for lease"), "{text}");
        assert!(!text.contains("without the filters"), "{text}");
    }

    #[test]
    fn blank_snippet_lines_are_dropped() {
        let s = Snippet {
            text: "\n\n  \nreal content\n".into(),
            matches: vec![],
        };
        assert_eq!(snippet_lines(&s, 80), vec!["real content"]);
    }
}
