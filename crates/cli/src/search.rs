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
use marrow_store::Store;

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

/// Render results, or a diagnosis when there are none.
#[allow(clippy::too_many_arguments)]
pub fn run(
    store: &Store,
    index: &dyn TextIndex,
    semantic: Option<&Semantic>,
    query: &str,
    limit: usize,
    roots: &[String],
    json: bool,
    style: Style,
    out: &mut impl Write,
) -> Result<()> {
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
    let req = SearchRequest::new(query)
        .mode(MatchMode::Terms)
        .limit(limit);
    let branch = semantic.map(|s| (&s.vectors as &dyn VectorIndex, &s.embedding));
    let results = marrow_query::search::search_hybrid(store, index, branch, &req)?;
    let hits = results.hits;

    // IDX-001: a file must be findable by its name. Content search alone cannot
    // do that — a file with no parseable content has no chunks and therefore no
    // index document, so `marrow index` could truthfully report "still findable
    // by name" about a file that was not findable at all.
    //
    // This branch belongs in `marrow-query` alongside the fusion machinery; it
    // lives here until the CLI is moved onto that crate.
    let by_name = path_matches(store, query, limit, &hits)?;
    // Everything the user waited for, model load included.
    let setup = semantic.map(|s| s.prepared_in.as_millis()).unwrap_or(0);
    let elapsed = started.elapsed().as_millis() + setup;

    if json {
        return render_json(
            &hits,
            &by_name,
            &results.branches,
            setup,
            query,
            elapsed,
            out,
        );
    }
    if hits.is_empty() && by_name.is_empty() {
        // Cheap: a count on the vector table, no model started. The
        // suggestion is only worth printing if there is something to search.
        let semantic_available = SqliteVectorIndex::open(store)
            .and_then(|v| v.doc_count())
            .map(|n| n > 0)
            .unwrap_or(false);
        return render_nothing(
            query,
            &results.branches,
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
        roots,
        elapsed,
        style,
        out,
    )
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

/// Files whose path contains the query, excluding ones already found by content.
fn path_matches(store: &Store, query: &str, limit: usize, already: &[Hit]) -> Result<Vec<NameHit>> {
    let needle = query.trim();
    if needle.is_empty() {
        return Ok(Vec::new());
    }
    let seen: std::collections::HashSet<&str> = already.iter().map(|h| h.path.as_str()).collect();

    let conn = store.reader()?;
    let mut stmt = conn
        .prepare(
            "SELECT f.current_path, COALESCE(v.mtime_ms, 0),
                    (SELECT count(*) FROM chunks c WHERE c.version_id = v.version_id)
               FROM files f
          LEFT JOIN file_versions v
                 ON v.file_id = f.file_id AND v.status = 'CURRENT'
              WHERE f.status = 'ACTIVE'
                AND f.current_path IS NOT NULL
                AND lower(f.current_path) LIKE '%' || lower(?1) || '%'
              ORDER BY length(f.current_path)
              LIMIT ?2",
        )
        .map_err(|e| marrow_store::map_sqlite(e, "searching by name"))?;

    let rows = stmt
        .query_map(
            marrow_store::rusqlite::params![needle, limit as i64 * 2],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            },
        )
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
    roots: &[String],
    elapsed: u128,
    style: Style,
    out: &mut impl Write,
) -> Result<()> {
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
            "{} result{} · {} · {}{}{}",
            total,
            if total == 1 { "" } else { "s" },
            render::duration(elapsed),
            branch_line(branches),
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

/// Zero results is a diagnosis, not a shrug ([UX §4]).
fn render_nothing(
    query: &str,
    branches: &[&'static str],
    semantic_available: bool,
    elapsed: u128,
    style: Style,
    out: &mut impl Write,
) -> Result<()> {
    writeln!(out)?;
    writeln!(out, "No matches for {}", style.bold(query))?;
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
    writeln!(out)?;
    writeln!(out, "  {}", style.dim("try"))?;
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
    query: &str,
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
            "query": query,
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

    #[test]
    fn the_zero_results_hint_survives_being_pasted_into_a_shell() {
        // The whole point of `--literal` is patterns the index cannot express,
        // which are exactly the ones a shell would expand or swallow.
        let mut buf = Vec::new();
        render_nothing("});", &[LEXICAL], false, 3, Style::plain(), &mut buf).expect("rendering");
        let text = String::from_utf8(buf).expect("output is utf-8");
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
            &[LEXICAL, SEMANTIC],
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
    fn blank_snippet_lines_are_dropped() {
        let s = Snippet {
            text: "\n\n  \nreal content\n".into(),
            matches: vec![],
        };
        assert_eq!(snippet_lines(&s, 80), vec!["real content"]);
    }
}
