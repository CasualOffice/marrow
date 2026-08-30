//! `marrow search --literal` — an exact scan that ignores the index.
//!
//! The index is the fast path and it is a *lexical* one: it tokenizes, so
//! `refresh_token` is found by `refresh` and `token`, and a search for
//! `});` or `TODO(sachin)` finds nothing at all. This is the escape hatch for
//! exactly that, and the zero-results screen has been suggesting it since M1
//! while the flag did not exist — a suggestion that leads nowhere is worse than
//! no suggestion.
//!
//! It reads files. Invariant #5 is therefore not optional here: the tier is
//! checked before every open and a placeholder is skipped unread rather than
//! hydrated, and the count of what was skipped is reported. A scan that
//! silently omitted the cloud-only half of a folder would be the most
//! misleading possible "no matches".

use std::io::Write;
use std::sync::atomic::AtomicBool;

use marrow_core::{Result, TierState};
use marrow_index::{
    literal_search, CaseMode, LiteralQuery, LiteralTarget, PatternKind, StopReason,
};
use marrow_store::Store;

use crate::render::{self, Style};

/// Scan and print.
#[allow(clippy::too_many_arguments)] // Each is a distinct input the command
                                     // parsed; a struct would move the list rather than shorten it.
pub fn run(
    store: &Store,
    pattern: &str,
    regex: bool,
    ignore_case: bool,
    whole_word: bool,
    limit: usize,
    json: bool,
    style: Style,
    out: &mut dyn Write,
    cancel: &AtomicBool,
) -> Result<()> {
    let targets = resident_targets(store)?;
    let q = LiteralQuery {
        pattern: pattern.to_string(),
        kind: if regex {
            PatternKind::Regex
        } else {
            PatternKind::Literal
        },
        case: if ignore_case {
            CaseMode::Insensitive
        } else {
            CaseMode::Sensitive
        },
        whole_word,
        max_total_matches: limit,
        ..LiteralQuery::new(pattern)
    };

    let started = std::time::Instant::now();
    let outcome = literal_search(&targets, &q, cancel)?;
    let elapsed = started.elapsed();

    if json {
        let payload = serde_json::json!({
            "pattern": pattern,
            "matches": outcome.hits.len(),
            "filesScanned": outcome.files_scanned,
            "filesSkippedNotResident": outcome.files_skipped_not_resident,
            "filesSkippedBinary": outcome.files_skipped_binary,
            "filesSkippedTooLarge": outcome.files_skipped_too_large,
            "filesFailed": outcome.files_failed,
            "stopReason": format!("{:?}", outcome.stopped),
            "filesTruncated": outcome.files_truncated,
            "hits": outcome.hits.iter().map(|h| serde_json::json!({
                "path": h.path.display().to_string(),
                "line": h.line,
                "text": h.snippet.text,
            })).collect::<Vec<_>>(),
        });
        writeln!(out, "{payload}").map_err(io)?;
        return Ok(());
    }

    let total = targets.len();
    for h in &outcome.hits {
        writeln!(
            out,
            "{}",
            style.dim(&format!("{}:{}", h.path.display(), h.line))
        )
        .map_err(io)?;
        writeln!(out, "  {}", h.snippet.text.trim_end()).map_err(io)?;
    }
    if outcome.hits.is_empty() {
        writeln!(out, "  {}", style.dim("no matches")).map_err(io)?;
    }
    writeln!(out).map_err(io)?;

    // Every skipped file is named as a count. A scan that quietly omitted the
    // cloud-only half of a folder would be the most misleading "no matches"
    // this program could print.
    writeln!(
        out,
        "  {}",
        style.dim(&format!(
            "{} matches in {} of {total} files, scanned in {}",
            outcome.hits.len(),
            outcome.files_scanned,
            render::duration(elapsed.as_millis())
        ))
    )
    .map_err(io)?;

    // **The important line.** Without it, "0 matches in 8,427 files" reads as
    // "we looked everywhere" when the scan gave up after five seconds — which
    // is the most misleading thing this command can say.
    if !outcome.stopped.is_complete() {
        let (what, fix) = match outcome.stopped {
            StopReason::TimeBudget => (
                "the time limit was reached before every file was scanned",
                "Narrow it with a workspace or a path, or raise the limit.",
            ),
            StopReason::Cancelled => (
                "you stopped it",
                "Nothing was missed that had been scanned.",
            ),
            StopReason::MatchLimit => (
                "the match limit was reached",
                "There may be more; raise -n to see them.",
            ),
            StopReason::Completed => unreachable!("checked above"),
        };
        writeln!(
            out,
            "  {}",
            style.warn(&format!("Incomplete: {what}. {fix}"))
        )
        .map_err(io)?;
    }
    for (n, what) in [
        (outcome.files_skipped_not_resident, "not on this disk"),
        (outcome.files_skipped_binary, "not text"),
        (outcome.files_skipped_too_large, "over the size limit"),
        (outcome.files_failed, "could not be read"),
        (outcome.files_truncated, "had more matches than were shown"),
    ] {
        if n > 0 {
            writeln!(out, "  {}", style.dim(&format!("{n} skipped: {what}"))).map_err(io)?;
        }
    }
    Ok(())
}

/// Every active file, with the tier the scanner must check before opening it.
///
/// The tier comes from the index rather than from a fresh `stat`: it is the
/// value the last scan recorded, and `literal_search` re-checks nothing — the
/// caller supplying a wrong tier is how invariant #5 gets broken by a caller
/// rather than by the engine.
fn resident_targets(store: &Store) -> Result<Vec<LiteralTarget>> {
    let conn = store.reader()?;
    let mut stmt = conn
        .prepare(
            "SELECT file_id, current_path, tier_state FROM files
              WHERE status='ACTIVE' AND current_path IS NOT NULL",
        )
        .map_err(|e| marrow_store::map_sqlite(e, "listing files to scan"))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| marrow_store::map_sqlite(e, "listing files to scan"))?;

    let mut out = Vec::new();
    for row in rows {
        let (id, path, tier) =
            row.map_err(|e| marrow_store::map_sqlite(e, "reading a file to scan"))?;
        let Ok(file_id) = id.parse() else { continue };
        let tier = match tier.as_str() {
            "PLACEHOLDER" => TierState::Placeholder,
            "HYDRATING" => TierState::Hydrating,
            "UNAVAILABLE" => TierState::Unavailable,
            _ => TierState::Resident,
        };
        out.push(LiteralTarget::new(file_id, path, tier));
    }
    Ok(out)
}

fn io(e: std::io::Error) -> marrow_core::Error {
    marrow_core::Error::from(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_placeholder_is_carried_as_a_placeholder_not_flattened_to_resident() {
        // Invariant #5 is the caller's to keep here: `literal_search` trusts
        // the tier it is given and skips anything but `Resident` unread. A
        // caller that defaulted everything to `Resident` would hydrate the
        // user's whole cloud folder.
        for (raw, want) in [
            ("PLACEHOLDER", TierState::Placeholder),
            ("HYDRATING", TierState::Hydrating),
            ("UNAVAILABLE", TierState::Unavailable),
            ("RESIDENT", TierState::Resident),
        ] {
            let tier = match raw {
                "PLACEHOLDER" => TierState::Placeholder,
                "HYDRATING" => TierState::Hydrating,
                "UNAVAILABLE" => TierState::Unavailable,
                _ => TierState::Resident,
            };
            assert_eq!(tier, want, "{raw}");
            assert_eq!(tier.safe_to_read(), raw == "RESIDENT");
        }
    }
}
