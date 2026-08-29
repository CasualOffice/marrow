//! `marrow search` — the primary surface.
//!
//! Rendering follows [UX §4]. The anatomy is deliberate: `path:line` on its own
//! line and first, because the eye scans a left-aligned column of paths far
//! faster than paths embedded in prose, and because that format is the one
//! editors and terminals linkify.
//!
//! [UX §4]: ../../../docs/UX.md

use std::io::Write;

use marrow_core::{Origin, ProvenanceClass, Result, SourceSpan, Timestamp};
use marrow_index::{MatchMode, Snippet, TextHit, TextIndex, TextQuery};

use crate::render::{self, Style};

/// Render results, or a diagnosis when there are none.
pub fn run(
    index: &dyn TextIndex,
    query: &str,
    limit: usize,
    roots: &[String],
    json: bool,
    style: Style,
    out: &mut impl Write,
) -> Result<()> {
    let started = std::time::Instant::now();
    let q = TextQuery::new(query).mode(MatchMode::Terms).limit(limit);
    let hits = index.search(&q)?;
    let elapsed = started.elapsed().as_millis();

    if json {
        return render_json(&hits, query, elapsed, out);
    }
    if hits.is_empty() {
        return render_nothing(query, elapsed, style, out);
    }
    render_hits(&hits, roots, elapsed, style, out)
}

fn render_hits(
    hits: &[TextHit],
    roots: &[String],
    elapsed: u128,
    style: Style,
    out: &mut impl Write,
) -> Result<()> {
    writeln!(out)?;
    for h in hits {
        let loc = location(&relative_to(&h.path, roots), &h.span);
        let reason = match_reason(h);
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

    writeln!(
        out,
        "{}",
        style.dim(&format!(
            "{} result{} · {}",
            hits.len(),
            if hits.len() == 1 { "" } else { "s" },
            render::duration(elapsed)
        ))
    )?;
    Ok(())
}

/// Zero results is a diagnosis, not a shrug ([UX §4]).
fn render_nothing(query: &str, elapsed: u128, style: Style, out: &mut impl Write) -> Result<()> {
    writeln!(out)?;
    writeln!(out, "No matches for {}", style.bold(query))?;
    writeln!(
        out,
        "  {}",
        style.dim(&format!("searched in {}", render::duration(elapsed)))
    )?;
    writeln!(out)?;
    writeln!(out, "  {}", style.dim("try"))?;
    writeln!(
        out,
        "    marrow search --literal {query}   exact scan, ignores the index"
    )?;
    writeln!(
        out,
        "    marrow status                     what is and is not indexed"
    )?;
    Ok(())
}

fn render_json(hits: &[TextHit], query: &str, elapsed: u128, out: &mut impl Write) -> Result<()> {
    // Same data as the human view — a second renderer, not a parallel path.
    let results: Vec<_> = hits
        .iter()
        .enumerate()
        .map(|(i, h)| {
            serde_json::json!({
                "rank": i + 1,
                "score": h.score,
                "reasons": [match_reason(h)],
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
    writeln!(
        out,
        "{}",
        serde_json::json!({
            "schema": "marrow.search/1",
            "query": query,
            "elapsed_ms": elapsed,
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

/// Why this matched ([UX §2] principle 3).
///
/// M1 has only the lexical branch, so this is honest rather than interesting;
/// it becomes meaningful when the vector branch lands in M4.
fn match_reason(_h: &TextHit) -> String {
    "exact".to_string()
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

    #[test]
    fn blank_snippet_lines_are_dropped() {
        let s = Snippet {
            text: "\n\n  \nreal content\n".into(),
            matches: vec![],
        };
        assert_eq!(snippet_lines(&s, 80), vec!["real content"]);
    }
}
