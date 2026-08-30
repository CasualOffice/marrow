//! `marrow search --literal` — an exact scan that ignores the index.
//!
//! The index is the fast path and it is a *lexical* one: it tokenizes, so
//! `refresh_token` is found by `refresh` and `token`, and a search for
//! `});` or `TODO(sachin)` finds nothing at all. This is the escape hatch for
//! exactly that, and the zero-results screen has been suggesting it since M1
//! while the flag did not exist — a suggestion that leads nowhere is worse than
//! no suggestion.
//!
//! # The scope is walked, not queried (R10-A)
//!
//! This used to build its target list from `SELECT ... FROM files` — the index
//! it exists to bypass. Add a folder and search it before the first `marrow
//! index` and the answer was **"0 matches in 0 of 0 files"**, `Completed`, no
//! warning: a complete search of a folder nothing had ever opened. The same
//! held for every file created since the last sweep, which is precisely the
//! case this command is recommended for.
//!
//! So the scope now comes from walking the authorised roots. The only thing
//! read from the database is *which folders the user granted*, which is consent
//! and not an index — nothing here believes anything the index says about what
//! is in them, what state it is in, or whether it still exists.
//!
//! # Invariants #5 and #7
//!
//! It reads files, so neither is optional here.
//!
//! - **#5, placeholders.** The tier comes from the `lstat` the walk just did
//!   (`marrow_scan::tier`), not from the `tier_state` column some earlier sweep
//!   wrote. A placeholder is skipped **unread** — never hydrated — and counted.
//! - **#7, containment.** Each root is re-canonicalised through
//!   [`AuthorizedRoot::open`] at the moment of the search, the walk does not
//!   follow symlinks out of it, and `marrow_index`'s reader stats with
//!   `symlink_metadata` and refuses a link before the open.
//!
//! # And it says what it did not cover
//!
//! A scan with a time budget over tens of thousands of files can give up long
//! before the end, so "no matches" is routinely a partial answer. Every count
//! the human view has, `--json` has too, in a `coverage` block shaped like
//! MCP's — a script that sees `"matches": 0` and cannot tell a searched corpus
//! from an abandoned one is the most misleading thing this command can offer.
//!
//! Two things make the partial case reproducible rather than a coin toss: the
//! target list is sorted before it is scanned, so two identical invocations
//! cover the same prefix of the same corpus, and the stop reason with the
//! unreached count is on every surface.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use marrow_core::{Error, Result};
use marrow_index::{
    literal_search, CaseMode, LiteralOutcome, LiteralQuery, LiteralTarget, PatternKind, StopReason,
};
use marrow_scan::{walk, AuthorizedRoot, ScanEvent, WalkPolicy, DEFAULT_NOISE_DIRS};
use marrow_store::Store;

use crate::render::{self, Style};

/// Seconds the scan may spend before it reports a partial result.
///
/// The library default is 5 s, sized by Part 6 §116 against a 10k-file scope.
/// The author's is 35,366 walked files, and 5 s reached between 6,000 and
/// 14,000 of them depending on what the OS page cache happened to hold — which
/// is how F5 came to answer "no matches" cold and "5 matches" warm for the same
/// string on the same disk. Measured here: 1.2 s warm, 10.0 s from a cache
/// evicted by streaming 17 GB. 30 s is three times the cold figure.
///
/// It is not the *fix* for F5 — no fixed number can be, and the fix is that a
/// partial answer now says so, in both renderers, and `--time-limit` can raise
/// it. This is the default that makes the ordinary case actually finish.
const DEFAULT_TIME_LIMIT: Duration = Duration::from_secs(30);

/// One `--literal` invocation, as the command line parsed it.
pub struct Request<'a> {
    pub pattern: &'a str,
    pub regex: bool,
    pub ignore_case: bool,
    pub whole_word: bool,
    pub limit: usize,
    /// `--workspace`: restrict the walk to one granted folder.
    pub workspace: Option<&'a str>,
    /// `--path`: keep only files whose path contains this, ASCII-case-blind.
    /// Same semantics as MCP's `path_contains`.
    pub path_contains: Option<&'a str>,
    /// `--time-limit`. `Some(ZERO)` means no limit at all.
    pub time_limit: Option<Duration>,
}

/// Scan and print.
pub fn run(
    store: &Store,
    req: &Request<'_>,
    json: bool,
    style: Style,
    out: &mut dyn Write,
    cancel: &AtomicBool,
) -> Result<()> {
    let scope = walk_scope(store, req)?;
    let budget = match req.time_limit {
        // `--time-limit 0`: run to the end. `Duration::MAX` rather than a
        // separate "unbounded" flag, because the cancel token still stops it
        // and Ctrl-C is what an over-long scan actually needs.
        Some(d) if d.is_zero() => Duration::MAX,
        Some(d) => d,
        None => DEFAULT_TIME_LIMIT,
    };
    let q = LiteralQuery {
        pattern: req.pattern.to_string(),
        kind: if req.regex {
            PatternKind::Regex
        } else {
            PatternKind::Literal
        },
        case: if req.ignore_case {
            CaseMode::Insensitive
        } else {
            CaseMode::Sensitive
        },
        whole_word: req.whole_word,
        max_total_matches: req.limit,
        time_budget: budget,
        ..LiteralQuery::new(req.pattern)
    };

    let started = std::time::Instant::now();
    let outcome = literal_search(&scope.targets, &q, cancel)?;
    let elapsed = started.elapsed();

    if json {
        return render_json(req, &scope, &outcome, budget, elapsed, out);
    }
    render_human(&scope, &outcome, budget, elapsed, style, out)
}

// ---------------------------------------------------------------------------
// Scope
// ---------------------------------------------------------------------------

/// What the walk established, including what it could not reach.
struct Scope {
    targets: Vec<LiteralTarget>,
    /// The roots actually walked, canonical.
    roots: Vec<PathBuf>,
    /// Roots that could not be opened at all — unmounted volume, folder
    /// deleted, permission revoked. One gone root must not end the search, but
    /// it also must not be invisible: everything under it is missing from the
    /// scope.
    unreachable_roots: Vec<String>,
    /// Directories the walk could not read. FS-011 keeps the walk going; this
    /// keeps the gap on the report.
    unreadable_dirs: usize,
    /// Files the walk found and `--path` excluded. Named so that a `--path`
    /// that matched nothing is distinguishable from a folder that is empty.
    excluded_by_filter: usize,
}

impl Scope {
    /// Whether establishing the scope itself left holes, independently of how
    /// the scan over it went.
    fn is_partial(&self) -> bool {
        !self.unreachable_roots.is_empty() || self.unreadable_dirs > 0
    }
}

/// Walk the authorised roots and describe every file found, with a tier from
/// the `lstat` that found it.
///
/// The one thing read from the database is the list of granted folders. That is
/// the consent record, not the index — the index is what this command exists to
/// bypass, and a scope taken from it is a scope that cannot see anything the
/// last sweep missed.
fn walk_scope(store: &Store, req: &Request<'_>) -> Result<Scope> {
    let started = std::time::Instant::now();
    let granted = granted_roots(store, req.workspace)?;
    let filter = req.path_contains.map(|s| s.to_lowercase());

    let mut scope = Scope {
        targets: Vec::new(),
        roots: Vec::with_capacity(granted.len()),
        unreachable_roots: Vec::new(),
        unreadable_dirs: 0,
        excluded_by_filter: 0,
    };

    // Gitignore stays off, matching `marrow index`'s default (D47): a file that
    // git ignores is still a file on the disk, and this is the command whose
    // whole promise is that it looks at the disk. Noise directories and hidden
    // files are pruned, which is a real limit on the scope — so it is stated in
    // the report rather than left for the user to discover.
    let policy = WalkPolicy::default();

    for (name, path) in granted {
        // Invariant #7 at operation time: canonicalised now, not trusted from
        // the string the workspace row holds. A root that has since become a
        // symlink to somewhere else resolves here, before anything is read.
        let root = match AuthorizedRoot::open(&path) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(workspace = %name, path = %path, error = %e, "root unavailable; it is not in the scope");
                scope.unreachable_roots.push(name);
                continue;
            }
        };
        scope.roots.push(root.path().to_path_buf());

        for event in walk(&root, &policy) {
            let entry = match event {
                ScanEvent::Entry(e) => e,
                ScanEvent::Failed(e) => {
                    tracing::debug!(error = %e, "literal scan could not read a directory");
                    scope.unreadable_dirs += 1;
                    continue;
                }
            };
            // Directories, and symlinks, which the walk yields but does not
            // descend through (WS-005).
            if !entry.is_file() {
                continue;
            }
            if let Some(f) = &filter {
                if !path_matches(&entry.path, f) {
                    scope.excluded_by_filter += 1;
                    continue;
                }
            }
            // Invariant #5: the tier is this walk's `lstat`, decided by
            // `marrow_scan::tier` from metadata, with nothing opened. A
            // placeholder arrives at `literal_search` labelled as one and is
            // skipped unread.
            //
            // The identity is a fresh `FileId` because this scope is the disk,
            // not the index: most of these files have no row, and inventing a
            // path→id lookup to fill the field in would be keying on a path
            // (invariant #2) for a value nothing here displays or stores.
            scope.targets.push(LiteralTarget::new(
                marrow_core::FileId::new(),
                entry.path,
                entry.facts.tier,
            ));
        }
    }

    // Stable order, so two identical invocations scan the same files in the
    // same sequence. With a time budget the tail is routinely never reached,
    // and an unordered walk makes *which* tail differ per run — F5's "0 matches
    // cold, 5 matches warm" is that, plus the budget.
    scope.targets.sort_by(|a, b| a.path.cmp(&b.path));
    tracing::debug!(
        files = scope.targets.len(),
        roots = scope.roots.len(),
        elapsed_ms = started.elapsed().as_millis(),
        "literal scope walked"
    );
    Ok(scope)
}

/// `--path`: does this path contain the fragment?
///
/// A substring of the whole path, ASCII-case-blind, which is what MCP's
/// `path_contains` means through SQLite `LIKE`. `--path crates/model` and
/// `--path .rs` both work, and neither needs the user to know whether the
/// answer is a directory or a suffix. `filter` is already lowercased.
fn path_matches(path: &std::path::Path, filter: &str) -> bool {
    path.as_os_str()
        .to_string_lossy()
        .to_lowercase()
        .contains(filter)
}

/// The folders the user granted, as `(workspace name, canonical path)`.
fn granted_roots(store: &Store, only: Option<&str>) -> Result<Vec<(String, String)>> {
    let conn = store.reader()?;
    let mut stmt = conn
        .prepare(
            "SELECT w.name, r.canonical_path
               FROM workspaces w
               JOIN workspace_roots r ON r.workspace_id = w.workspace_id
              WHERE w.status = 'ACTIVE'
              ORDER BY w.name",
        )
        .map_err(|e| marrow_store::map_sqlite(e, "listing the folders to scan"))?;
    let all: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .and_then(|it| it.collect::<std::result::Result<Vec<_>, _>>())
        .map_err(|e| marrow_store::map_sqlite(e, "listing the folders to scan"))?;

    if all.is_empty() {
        return Err(Error::new(
            marrow_core::Code::FsNotFound,
            "No folder has been granted yet, so there is nothing on disk to scan. \
             Run `marrow workspace add <path>` first.",
        ));
    }
    let Some(name) = only else { return Ok(all) };

    let picked: Vec<_> = all.iter().filter(|(n, _)| n == name).cloned().collect();
    if picked.is_empty() {
        let known: Vec<&str> = all.iter().map(|(n, _)| n.as_str()).collect();
        return Err(Error::new(
            marrow_core::Code::FsNotFound,
            format!(
                "No workspace named '{name}'. Drop --workspace to scan everything, \
                 or name one of: {}.",
                known.join(", ")
            ),
        ));
    }
    Ok(picked)
}

// ---------------------------------------------------------------------------
// Reporting
// ---------------------------------------------------------------------------

/// The wire form of [`StopReason`], snake_case like every other CLI payload.
fn stop_reason(r: StopReason) -> &'static str {
    match r {
        StopReason::Completed => "completed",
        StopReason::TimeBudget => "time_budget",
        StopReason::Cancelled => "cancelled",
        StopReason::MatchLimit => "match_limit",
    }
}

/// The cause, and something the user can actually do about it.
///
/// The old text said "Narrow it with a workspace or a path, or raise the
/// limit". It had been copied from the MCP tool, which has `workspace` and
/// `path_contains`; the CLI had neither, and no time-limit flag either, so the
/// one line whose job is to tell the user what to do next named three things
/// that did not exist. The flags exist now, and this names them.
fn incompleteness(o: &LiteralOutcome, budget: Duration) -> Option<(String, String)> {
    match o.stopped {
        StopReason::Completed => None,
        StopReason::TimeBudget => Some((
            format!(
                "the {} time limit ran out with {} of {} files never looked at",
                render::duration(budget.as_millis()),
                render::count(o.files_unreached() as u64),
                render::count(o.files_in_scope as u64),
            ),
            format!(
                "Raise it with `--time-limit {}` (or `--time-limit 0` for no limit), or \
                 shrink the scan with `--path <part of a path>` or `--workspace <name>`.",
                budget.as_secs().saturating_mul(3).max(60),
            ),
        )),
        StopReason::Cancelled => Some((
            "you stopped it".to_string(),
            "Nothing was missed that had been scanned.".to_string(),
        )),
        StopReason::MatchLimit => Some((
            format!(
                "the match limit was reached after {} of {} files",
                render::count(o.files_considered as u64),
                render::count(o.files_in_scope as u64),
            ),
            "There may be more; raise -n to see them.".to_string(),
        )),
    }
}

/// What the walk deliberately left out. Stated whenever the answer is "no
/// matches", because that is when "it is not on this disk" is the wrong
/// conclusion to let the user reach on their own.
fn scope_caveat(scope: &Scope) -> String {
    let where_ = match scope.roots.len() {
        1 => scope.roots[0].display().to_string(),
        n => format!("{n} granted folders"),
    };
    format!(
        "Scope: files under {where_}, walked now rather than read from the index. \
         Hidden files and build directories ({}, …) are not in it.",
        DEFAULT_NOISE_DIRS[..3].join(", "),
    )
}

fn render_human(
    scope: &Scope,
    outcome: &LiteralOutcome,
    budget: Duration,
    elapsed: Duration,
    style: Style,
    out: &mut dyn Write,
) -> Result<()> {
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
            "{} {} in {} of {} files, scanned in {}",
            render::count(outcome.hits.len() as u64),
            if outcome.hits.len() == 1 {
                "match"
            } else {
                "matches"
            },
            render::count(outcome.files_scanned as u64),
            render::count(outcome.files_in_scope as u64),
            render::duration(elapsed.as_millis())
        ))
    )
    .map_err(io)?;

    // **The important line.** Without it, "0 matches in 8,427 files" reads as
    // "we looked everywhere" when the scan gave up after five seconds — which
    // is the most misleading thing this command can say.
    match incompleteness(outcome, budget) {
        Some((what, fix)) => writeln!(
            out,
            "  {}",
            style.warn(&format!("Incomplete: {what}. {fix}"))
        )
        .map_err(io)?,
        // *Reached*, not "read": the skip counts below say how many of them
        // were then left unopened, and claiming more than reaching them is the
        // completeness this scan has not earned.
        // The count is on the line above; repeating it here reads as
        // "1 matches in 1 of 1 files / every one of the 1 files".
        None => writeln!(out, "  {}", style.dim("Every file in scope was reached.")).map_err(io)?,
    }

    // A root that has gone away is a hole in the scope itself, not in the scan
    // over it, so it is reported even when the scan ran to the end.
    if !scope.unreachable_roots.is_empty() {
        writeln!(
            out,
            "  {}",
            style.warn(&format!(
                "Not searched: {} — the folder could not be opened. \
                 Reconnect the volume, or remove the workspace.",
                scope.unreachable_roots.join(", ")
            ))
        )
        .map_err(io)?;
    }
    if scope.unreadable_dirs > 0 {
        writeln!(
            out,
            "  {}",
            style.warn(&format!(
                "{} directories could not be read, so their contents were not in scope. \
                 Grant access in System Settings › Privacy & Security.",
                render::count(scope.unreadable_dirs as u64)
            ))
        )
        .map_err(io)?;
    }

    for (n, what) in [
        (outcome.files_skipped_not_resident, "cloud-only, not read"),
        (outcome.files_skipped_binary, "not text"),
        (outcome.files_skipped_too_large, "over the size limit"),
        (outcome.files_failed, "could not be read"),
        (outcome.files_truncated, "had more matches than were shown"),
        (scope.excluded_by_filter, "excluded by --path"),
    ] {
        if n > 0 {
            writeln!(
                out,
                "  {}",
                style.dim(&format!("{} skipped: {what}", render::count(n as u64)))
            )
            .map_err(io)?;
        }
    }

    if outcome.hits.is_empty() {
        writeln!(out, "  {}", style.dim(&scope_caveat(scope))).map_err(io)?;
    }
    Ok(())
}

/// `--json` carries everything the human view carries.
///
/// It used to carry strictly less: `{"matches":0,"filesScanned":12204,
/// "stopReason":"TimeBudget"}` with no scope and no completeness flag, while
/// the human view at least printed "12,204 of 79,179". A script had no way to
/// tell a searched corpus from an abandoned one. The `coverage` block is shaped
/// like MCP `search_literal`'s, which got this right.
fn render_json(
    req: &Request<'_>,
    scope: &Scope,
    outcome: &LiteralOutcome,
    budget: Duration,
    elapsed: Duration,
    out: &mut dyn Write,
) -> Result<()> {
    // Four branches, because `complete: false` next to "every file was read"
    // is its own small lie. Reaching a file, reading it, and the scope being
    // whole are three different claims.
    let advice = match incompleteness(outcome, budget) {
        Some((what, fix)) => format!("This scan did not cover everything in scope: {what}. {fix}"),
        None if scope.is_partial() => "Every file in scope was reached, but the scope itself \
             has holes — see roots_unreachable and directories_unreadable. Reconnect the \
             folder, or grant access to it, and scan again."
            .to_string(),
        None if outcome.has_gaps() => "Every file in scope was reached, but some were not \
             read — see the files_skipped counts. No match here is not proof the pattern is \
             absent from those."
            .to_string(),
        None => "Every file in scope was read.".to_string(),
    };
    let payload = serde_json::json!({
        "schema": "marrow.search.literal/1",
        "pattern": req.pattern,
        "matches": outcome.hits.len(),
        "elapsed_ms": elapsed.as_millis() as u64,
        "results": outcome.hits.iter().map(|h| serde_json::json!({
            "path": h.path.display().to_string(),
            "location": format!("{}:{}", h.path.display(), h.line),
            "line": h.line,
            "span": h.span,
            "excerpt": h.snippet.text,
        })).collect::<Vec<_>>(),
        // `complete` is the field that matters: without it "matches: 0" reads
        // as "not on this disk" when the scan gave up a twelfth of the way in.
        // It is false whenever the *scope* has holes too, not only the scan.
        "coverage": {
            "complete": !outcome.has_gaps() && !scope.is_partial(),
            "stopped_because": stop_reason(outcome.stopped),
            // Walked, not queried: the number is what is on the disk now, not
            // what the last sweep recorded. R10-A.
            "scope_from": "walk",
            "roots_walked": scope.roots.len(),
            "roots_unreachable": scope.unreachable_roots,
            "directories_unreadable": scope.unreadable_dirs,
            "files_in_scope": outcome.files_in_scope,
            "files_considered": outcome.files_considered,
            "files_never_reached": outcome.files_unreached(),
            "files_scanned": outcome.files_scanned,
            // Invariant #5: skipped without being opened.
            "files_skipped_cloud_only": outcome.files_skipped_not_resident,
            "files_skipped_binary": outcome.files_skipped_binary,
            "files_skipped_too_large": outcome.files_skipped_too_large,
            "files_unreadable": outcome.files_failed,
            "files_with_more_matches": outcome.files_truncated,
            "files_excluded_by_path_filter": scope.excluded_by_filter,
            // The walk's own limits, which no stop reason describes.
            "excluded_directory_names": DEFAULT_NOISE_DIRS,
            "hidden_files_excluded": true,
            "advice": advice,
        },
    });
    writeln!(out, "{payload}").map_err(io)?;
    Ok(())
}

fn io(e: std::io::Error) -> marrow_core::Error {
    marrow_core::Error::from(e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use marrow_core::TierState;

    /// A scratch directory that removes itself.
    ///
    /// Hand-rolled rather than `tempfile`, which this crate does not depend on
    /// and which is not worth a new dependency for one test module. The name
    /// carries the pid and a counter, so two tests in the same run — and two
    /// runs at once — never collide.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "marrow-literal-{tag}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch dir");
            Self(dir)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }

        fn write(&self, name: &str, body: &str) -> PathBuf {
            let p = self.0.join(name);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).expect("scratch subdir");
            }
            std::fs::write(&p, body).expect("scratch file");
            p
        }

        /// Every regular file the scope builder would take from this directory,
        /// in the order it would scan them.
        fn targets(&self) -> Vec<LiteralTarget> {
            let root = AuthorizedRoot::open(self.path()).expect("root");
            let mut t: Vec<_> = walk(&root, &WalkPolicy::default())
                .filter_map(ScanEvent::entry)
                .filter(|e| e.is_file())
                .map(|e| LiteralTarget::new(marrow_core::FileId::new(), e.path, e.facts.tier))
                .collect();
            t.sort_by(|a, b| a.path.cmp(&b.path));
            t
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scope_of(targets: Vec<LiteralTarget>) -> Scope {
        Scope {
            targets,
            roots: vec![PathBuf::from("/tmp/root")],
            unreachable_roots: Vec::new(),
            unreadable_dirs: 0,
            excluded_by_filter: 0,
        }
    }

    fn outcome(in_scope: usize, considered: usize, stopped: StopReason) -> LiteralOutcome {
        LiteralOutcome {
            hits: Vec::new(),
            files_in_scope: in_scope,
            files_considered: considered,
            files_scanned: considered,
            files_skipped_not_resident: 0,
            files_skipped_binary: 0,
            files_skipped_too_large: 0,
            files_failed: 0,
            files_truncated: 0,
            elapsed: Duration::ZERO,
            stopped,
        }
    }

    fn request<'a>(pattern: &'a str) -> Request<'a> {
        Request {
            pattern,
            regex: false,
            ignore_case: false,
            whole_word: false,
            limit: 20,
            workspace: None,
            path_contains: None,
            time_limit: None,
        }
    }

    /// **R10-A.** The scope used to come from `SELECT ... FROM files`, so a
    /// folder that had never been indexed produced zero targets, the loop body
    /// never ran, and the user was told "0 matches in 0 of 0 files" with
    /// `Completed` and no warning. The walk sees the file the index has never
    /// heard of.
    #[test]
    fn a_file_no_index_run_has_ever_seen_is_in_scope() {
        let s = Scratch::new("unindexed");
        s.write("brand_new.txt", "the needle is here\n");

        let targets = s.targets();
        assert_eq!(targets.len(), 1, "the walk found it with no index at all");
        assert_eq!(targets[0].tier, TierState::Resident);

        let out = literal_search(
            &targets,
            &LiteralQuery::new("needle"),
            &AtomicBool::new(false),
        )
        .expect("scan");
        assert_eq!(out.hits.len(), 1);
        assert_eq!(out.files_in_scope, 1, "and the denominator is not zero");
        assert!(out.stopped.is_complete());
    }

    /// Invariant #5. The tier travels with the target, decided by
    /// `marrow_scan` from the walk's own `lstat` — the point of probing per
    /// file is that the answer is current, rather than whatever the last sweep
    /// wrote into `files.tier_state`.
    #[test]
    fn the_walk_supplies_a_tier_and_only_resident_is_readable() {
        let s = Scratch::new("tier");
        s.write("plain.txt", "needle\n");
        assert_eq!(s.targets()[0].tier, TierState::Resident);

        for tier in [
            TierState::Placeholder,
            TierState::Hydrating,
            TierState::Unavailable,
        ] {
            assert!(!tier.safe_to_read(), "{tier:?} must never be opened");
            // A placeholder reaching the scan is skipped unread and counted,
            // not hydrated.
            let target =
                LiteralTarget::new(marrow_core::FileId::new(), s.path().join("plain.txt"), tier);
            let out = literal_search(
                &[target],
                &LiteralQuery::new("needle"),
                &AtomicBool::new(false),
            )
            .expect("scan");
            assert!(out.hits.is_empty());
            assert_eq!(out.files_skipped_not_resident, 1);
            assert_eq!(out.files_scanned, 0);
        }
    }

    /// **F5.** The scope is sorted, so two identical invocations scan the same
    /// files in the same order. Without it, which part of the corpus a
    /// budget-limited scan covers depends on directory order and the page
    /// cache — which is how "no matches" and "5 matches" came from the same
    /// command on the same disk.
    #[test]
    fn the_scan_order_is_stable_across_runs() {
        let s = Scratch::new("order");
        for n in ["c.txt", "a.txt", "b.txt", "nested/d.txt"] {
            s.write(n, "x\n");
        }
        let first: Vec<_> = s.targets().into_iter().map(|t| t.path).collect();
        let again: Vec<_> = s.targets().into_iter().map(|t| t.path).collect();
        assert_eq!(first, again);
        assert!(first[0].ends_with("a.txt"), "sorted, not readdir order");
    }

    /// A cut-short scan over a real directory reports the scope it did not
    /// finish, rather than the number of files it happened to reach.
    #[test]
    fn a_scan_that_stops_early_still_reports_the_whole_scope() {
        let s = Scratch::new("partial");
        for i in 0..6 {
            s.write(&format!("f{i}.txt"), "needle\n");
        }
        let out = literal_search(
            &s.targets(),
            &LiteralQuery::new("needle").max_total_matches(2),
            &AtomicBool::new(false),
        )
        .expect("scan");
        assert_eq!(out.files_in_scope, 6);
        assert!(out.files_unreached() > 0);
        assert!(!out.stopped.is_complete());
    }

    /// `--path` narrows the walk, and what it removed is counted — so a filter
    /// that matched nothing is distinguishable from a folder that is empty.
    #[test]
    fn the_path_filter_is_a_case_blind_substring_of_the_whole_path() {
        let p = std::path::Path::new("/Users/x/melp/crates/Model/src/supervisor.rs");
        assert!(path_matches(p, "crates/model"));
        assert!(path_matches(p, ".rs"));
        assert!(path_matches(p, "supervisor"));
        assert!(!path_matches(p, "crates/index"));

        let s = Scratch::new("filter");
        s.write("keep/a.txt", "needle\n");
        s.write("drop.txt", "needle\n");
        let kept = s
            .targets()
            .into_iter()
            .filter(|t| path_matches(&t.path, "keep"));
        assert_eq!(kept.count(), 1);
    }

    /// **F5's advice.** The old text said "Narrow it with a workspace or a
    /// path, or raise the limit" — copied from the MCP tool, which has those
    /// parameters. The CLI had none of them, so the one line whose job is to
    /// say what to do next named three things that did not exist. Whatever it
    /// names now has to be a flag `marrow search` actually parses.
    #[test]
    fn the_advice_names_only_flags_that_exist() {
        // Kept in step with `Cmd::Search` by
        // `main::literal_flags_named_in_advice_are_flags_the_parser_accepts`,
        // which runs the real clap parser over each of these.
        let real = ["--time-limit", "--path", "--workspace", "-n"];
        for stop in [
            StopReason::TimeBudget,
            StopReason::Cancelled,
            StopReason::MatchLimit,
        ] {
            let (what, fix) = incompleteness(&outcome(100, 8, stop), Duration::from_secs(30))
                .expect("not complete");
            assert!(!what.is_empty() && !fix.is_empty(), "{stop:?}");
            for word in fix.split_whitespace() {
                if let Some(flag) = word.strip_prefix("`") {
                    let flag = flag.split_whitespace().next().unwrap_or(flag);
                    if flag.starts_with('-') {
                        assert!(real.contains(&flag), "{flag} is not a flag this CLI has");
                    }
                }
            }
        }
        assert!(
            incompleteness(&outcome(100, 100, StopReason::Completed), Duration::ZERO).is_none(),
            "a complete scan has nothing to warn about"
        );
    }

    /// **F5's other half.** `--json` gave `{"matches":0,"filesScanned":12204,
    /// "stopReason":"TimeBudget"}` — no scope, no completeness flag — while the
    /// human view at least printed "12,204 of 79,179".
    #[test]
    fn json_carries_the_scope_and_the_completeness_flag() {
        let mut buf = Vec::new();
        render_json(
            &request("mark_loaded"),
            &scope_of(Vec::new()),
            &outcome(79_179, 12_204, StopReason::TimeBudget),
            Duration::from_secs(30),
            Duration::from_secs(30),
            &mut buf,
        )
        .expect("render");
        let v: serde_json::Value = serde_json::from_slice(&buf).expect("json");
        let c = &v["coverage"];
        assert_eq!(c["complete"], false);
        assert_eq!(c["stopped_because"], "time_budget");
        assert_eq!(c["files_in_scope"], 79_179);
        assert_eq!(c["files_scanned"], 12_204);
        assert_eq!(c["files_never_reached"], 66_975);
        assert_eq!(c["scope_from"], "walk");
        assert!(c["advice"]
            .as_str()
            .expect("advice")
            .contains("--time-limit"));
    }

    /// Completeness is claimed only when it was earned. A root that could not
    /// be opened is a hole in the scope, however well the scan over the rest of
    /// it went.
    #[test]
    fn a_scope_with_a_hole_in_it_is_never_reported_complete() {
        let mut scope = scope_of(Vec::new());
        scope.unreachable_roots.push("archive".to_string());
        assert!(scope.is_partial());

        let mut buf = Vec::new();
        render_json(
            &request("needle"),
            &scope,
            &outcome(10, 10, StopReason::Completed),
            DEFAULT_TIME_LIMIT,
            Duration::from_millis(4),
            &mut buf,
        )
        .expect("render");
        let v: serde_json::Value = serde_json::from_slice(&buf).expect("json");
        assert_eq!(v["coverage"]["complete"], false, "a gone root is a gap");
        assert_eq!(v["coverage"]["roots_unreachable"][0], "archive");
    }

    /// A clean, complete scan may say so — and still states what the walk
    /// itself excludes, so "complete" is not read as "every byte on this disk".
    #[test]
    fn a_complete_scan_says_so_and_still_states_what_the_walk_left_out() {
        let mut buf = Vec::new();
        render_json(
            &request("needle"),
            &scope_of(Vec::new()),
            &outcome(10, 10, StopReason::Completed),
            DEFAULT_TIME_LIMIT,
            Duration::from_millis(4),
            &mut buf,
        )
        .expect("render");
        let v: serde_json::Value = serde_json::from_slice(&buf).expect("json");
        assert_eq!(v["coverage"]["complete"], true);
        assert_eq!(v["coverage"]["hidden_files_excluded"], true);
        assert!(v["coverage"]["excluded_directory_names"]
            .as_array()
            .expect("names")
            .iter()
            .any(|n| n == "node_modules"));
        assert!(scope_caveat(&scope_of(Vec::new())).contains("node_modules"));
    }

    /// `complete: false` beside "every file in scope was read" is its own
    /// small lie. Reaching a file, reading it, and the scope being whole are
    /// three separate claims and the advice has to match the flag.
    #[test]
    fn the_advice_never_contradicts_the_completeness_flag() {
        let mut reached_but_unread = outcome(10, 10, StopReason::Completed);
        reached_but_unread.files_skipped_binary = 4;
        reached_but_unread.files_skipped_not_resident = 1;

        let mut buf = Vec::new();
        render_json(
            &request("needle"),
            &scope_of(Vec::new()),
            &reached_but_unread,
            DEFAULT_TIME_LIMIT,
            Duration::from_millis(9),
            &mut buf,
        )
        .expect("render");
        let v: serde_json::Value = serde_json::from_slice(&buf).expect("json");
        let advice = v["coverage"]["advice"].as_str().expect("advice");
        assert_eq!(v["coverage"]["complete"], false);
        assert!(advice.contains("not read"), "{advice}");
        assert_eq!(v["coverage"]["files_skipped_cloud_only"], 1);
    }

    /// The human view and `--json` are two renderers over one outcome, so the
    /// numbers that matter have to appear in both.
    #[test]
    fn the_human_view_names_the_scope_and_warns_when_it_is_partial() {
        let mut buf = Vec::new();
        render_human(
            &scope_of(Vec::new()),
            &outcome(79_179, 12_204, StopReason::TimeBudget),
            Duration::from_secs(30),
            Duration::from_secs(30),
            Style::plain(),
            &mut buf,
        )
        .expect("render");
        let text = String::from_utf8(buf).expect("utf8");
        assert!(
            text.contains("0 matches in 12,204 of 79,179 files"),
            "{text}"
        );
        assert!(text.contains("Incomplete:"), "{text}");
        assert!(text.contains("--time-limit"), "{text}");
        // Zero matches is exactly when "it is not on this disk" is the wrong
        // conclusion to let the reader reach unaided.
        assert!(
            text.contains("walked now rather than read from the index"),
            "{text}"
        );
    }
}
