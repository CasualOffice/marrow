//! Index-independent literal search (CAP-005).
//!
//! `marrow search --literal` is the escape hatch: **grep semantics, exact and
//! immediate, regardless of index freshness.** It reads files directly and
//! never consults the index, which is the whole point — the index can be
//! stale, mid-rebuild or missing, and this still answers correctly. It is also
//! the answer to "why did the index not find `FOO_BAR`?", because `unicode61`
//! splits on `_` (see the `fts5` module note) and this does not split at all.
//!
//! Three properties make it usable rather than merely correct:
//!
//! - **Stated scope.** The caller supplies the file list. Nothing here walks a
//!   directory, so there is no hidden reach: the scope is exactly what was
//!   passed in, and [`LiteralOutcome`] reports what inside that scope was
//!   skipped and why.
//! - **Time bound.** [`LiteralQuery::time_budget`] is checked between files and
//!   inside the per-file match loop. Part 6 §116 budgets 10k files in under 3 s
//!   cold; when the budget runs out the partial result is returned with
//!   [`StopReason::TimeBudget`] rather than a truncated lie.
//! - **Cancellable.** A caller-supplied `&AtomicBool` stops it. It is a
//!   parameter, not a global: a global cancel flag is one that two callers
//!   eventually share by accident.
//!
//! # Invariant #5 — placeholders are never hydrated
//!
//! Reading a cloud placeholder silently downloads it. This module **refuses to
//! open anything whose [`TierState`] is not `Resident`**, checked before the
//! `open`, and counts the refusal in
//! [`LiteralOutcome::files_skipped_not_resident`] so the CLI can say "412
//! cloud-only files may contain this" (UX §4's zero-results diagnosis).
//!
//! The tier is a **caller-supplied parameter**. This crate does not depend on
//! `marrow-scan`, so it cannot probe the tier itself; the caller must have
//! established it — from `files.tier_state`, or from a fresh `marrow_scan`
//! probe if the answer has to be current. Passing a stale `Resident` for a file
//! that has since been evicted is the caller's defect, and the only one this
//! module cannot defend against.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use marrow_core::{Code, Error, FileId, Result, SourceSpan, TierState};

use crate::port::{MatchRange, Snippet};

/// How the pattern is interpreted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PatternKind {
    /// The pattern is text to find exactly. Regex metacharacters are literal.
    #[default]
    Literal,
    /// The pattern is a regular expression (Rust `regex` syntax: no
    /// backtracking, so a hostile pattern cannot make this exponential).
    Regex,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaseMode {
    #[default]
    Sensitive,
    Insensitive,
}

/// Why the scan stopped. Always reported, so a partial answer is never
/// presented as a complete one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// Every in-scope file was considered.
    Completed,
    /// [`LiteralQuery::time_budget`] elapsed.
    TimeBudget,
    /// The caller's cancel token was set.
    Cancelled,
    /// [`LiteralQuery::max_total_matches`] was reached.
    MatchLimit,
}

impl StopReason {
    /// Whether the result covers the whole requested scope.
    pub fn is_complete(self) -> bool {
        matches!(self, StopReason::Completed)
    }
}

/// One file to consider.
#[derive(Clone, Debug)]
pub struct LiteralTarget {
    pub file_id: FileId,
    pub path: PathBuf,
    /// **Caller-supplied.** Anything but `Resident` is skipped unread.
    pub tier: TierState,
}

impl LiteralTarget {
    pub fn new(file_id: FileId, path: impl Into<PathBuf>, tier: TierState) -> Self {
        Self {
            file_id,
            path: path.into(),
            tier,
        }
    }
}

/// A literal / regex scan.
#[derive(Clone, Debug)]
pub struct LiteralQuery {
    pub pattern: String,
    pub kind: PatternKind,
    pub case: CaseMode,
    /// Require word boundaries around the match (`grep -w`).
    pub whole_word: bool,
    pub max_matches_per_file: usize,
    pub max_total_matches: usize,
    /// Files larger than this are skipped and counted. Bounds the single
    /// uninterruptible unit of work: one regex pass over one buffer.
    pub max_file_bytes: u64,
    pub time_budget: Duration,
    /// Characters of context each side of the match in the snippet.
    pub context_chars: usize,
}

impl LiteralQuery {
    pub fn new(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            kind: PatternKind::Literal,
            case: CaseMode::Sensitive,
            whole_word: false,
            max_matches_per_file: 50,
            max_total_matches: 1_000,
            // 8 MiB. M0 F7: 70.6% of the corpus is under 64 KB and nothing is
            // over 500 MB, so this excludes almost nothing that is text.
            max_file_bytes: 8 * 1024 * 1024,
            // §116's budget is 10k files in under 3 s cold; 5 s leaves headroom
            // for a larger scope before the answer becomes partial.
            time_budget: Duration::from_secs(5),
            context_chars: 80,
        }
    }

    pub fn regex(mut self) -> Self {
        self.kind = PatternKind::Regex;
        self
    }

    pub fn ignore_case(mut self) -> Self {
        self.case = CaseMode::Insensitive;
        self
    }

    pub fn whole_word(mut self, yes: bool) -> Self {
        self.whole_word = yes;
        self
    }

    pub fn time_budget(mut self, budget: Duration) -> Self {
        self.time_budget = budget;
        self
    }

    pub fn max_total_matches(mut self, n: usize) -> Self {
        self.max_total_matches = n;
        self
    }
}

/// One literal match.
#[derive(Clone, Debug)]
pub struct LiteralHit {
    pub file_id: FileId,
    pub path: PathBuf,
    /// 1-based line number, as an editor counts them.
    pub line: u32,
    /// Byte range of the match within the file. Invariant #1: a literal hit is
    /// as precise as provenance gets.
    pub span: SourceSpan,
    /// The line the match is on, as `Lines { start, end }`.
    pub line_span: SourceSpan,
    /// The line (or a window of it), with the match offsets inside it.
    pub snippet: Snippet,
}

/// The result of one scan, including everything it did not look at.
#[derive(Clone, Debug)]
pub struct LiteralOutcome {
    pub hits: Vec<LiteralHit>,
    pub files_scanned: usize,
    /// Invariant #5: skipped without being opened.
    pub files_skipped_not_resident: usize,
    pub files_skipped_binary: usize,
    pub files_skipped_too_large: usize,
    /// Unreadable for some other reason — permissions, a lock, a race with a
    /// delete. Isolated per file (FS-011), never fatal to the scan.
    pub files_failed: usize,
    /// Files that had more matches than [`LiteralQuery::max_matches_per_file`]
    /// allowed. The file was read; not all of its matches are here.
    pub files_truncated: usize,
    pub elapsed: Duration,
    pub stopped: StopReason,
}

impl LiteralOutcome {
    /// Whether anything in scope was not actually looked at.
    pub fn has_gaps(&self) -> bool {
        !self.stopped.is_complete()
            || self.files_skipped_not_resident > 0
            || self.files_skipped_binary > 0
            || self.files_skipped_too_large > 0
            || self.files_failed > 0
            || self.files_truncated > 0
    }
}

/// Compiled form of the pattern.
#[derive(Debug)]
enum Matcher {
    /// Case-sensitive plain substring: SIMD memmem, no regex engine.
    Literal(memchr::memmem::Finder<'static>),
    Regex(regex::bytes::Regex),
}

impl Matcher {
    fn compile(q: &LiteralQuery) -> Result<Self> {
        if q.pattern.is_empty() {
            return Err(Error::new(
                Code::CfgInvalid,
                "A literal search needs something to look for. Give it the exact text, or use \
                 `--regex` for a pattern.",
            ));
        }
        let plain =
            q.kind == PatternKind::Literal && q.case == CaseMode::Sensitive && !q.whole_word;
        if plain {
            return Ok(Matcher::Literal(
                memchr::memmem::Finder::new(q.pattern.as_bytes()).into_owned(),
            ));
        }
        let mut pattern = match q.kind {
            PatternKind::Literal => regex::escape(&q.pattern),
            PatternKind::Regex => q.pattern.clone(),
        };
        if q.whole_word {
            pattern = format!(r"\b(?:{pattern})\b");
        }
        let re = regex::bytes::RegexBuilder::new(&pattern)
            .case_insensitive(q.case == CaseMode::Insensitive)
            // Bounds compilation of a hostile pattern. `regex` has no
            // backtracking, so match time is already linear; this bounds memory.
            .size_limit(4 << 20)
            .dfa_size_limit(4 << 20)
            // Content is bytes, not necessarily UTF-8, so `.` must not be
            // required to match a whole codepoint.
            .unicode(q.kind == PatternKind::Regex)
            .build()
            .map_err(|e| {
                Error::new(
                    Code::CfgInvalid,
                    "That search pattern is not a valid regular expression. Fix the pattern, or \
                     drop `--regex` to search for the text exactly.",
                )
                .with_context(e.to_string())
            })?;
        Ok(Matcher::Regex(re))
    }

    /// Byte range of the next match at or after `from`.
    fn find_at(&self, hay: &[u8], from: usize) -> Option<(usize, usize)> {
        match self {
            Matcher::Literal(f) => f
                .find(&hay[from..])
                .map(|i| (from + i, from + i + f.needle().len())),
            Matcher::Regex(re) => re.find_at(hay, from).map(|m| (m.start(), m.end())),
        }
    }
}

/// How often the inner loop re-checks the cancel token and the clock.
const CHECK_EVERY_MATCHES: usize = 64;

/// Scan `targets` for `q`, stopping on `cancel`, the time budget or the match
/// limit — whichever comes first.
///
/// Never reads a file whose tier is not [`TierState::Resident`].
pub fn literal_search(
    targets: &[LiteralTarget],
    q: &LiteralQuery,
    cancel: &AtomicBool,
) -> Result<LiteralOutcome> {
    let matcher = Matcher::compile(q)?;
    let started = Instant::now();
    let mut out = LiteralOutcome {
        hits: Vec::new(),
        files_scanned: 0,
        files_skipped_not_resident: 0,
        files_skipped_binary: 0,
        files_skipped_too_large: 0,
        files_failed: 0,
        files_truncated: 0,
        elapsed: Duration::ZERO,
        stopped: StopReason::Completed,
    };

    for target in targets {
        if let Some(reason) = stop_now(cancel, started, q.time_budget) {
            out.stopped = reason;
            break;
        }
        if out.hits.len() >= q.max_total_matches {
            out.stopped = StopReason::MatchLimit;
            break;
        }

        // Invariant #5. Before the open, not after: opening is what starts a
        // hydration on some providers.
        if !target.tier.safe_to_read() {
            out.files_skipped_not_resident += 1;
            tracing::debug!(
                path = %target.path.display(),
                tier = ?target.tier,
                "literal scan skipped a non-resident file"
            );
            continue;
        }

        let bytes = match read_bounded(&target.path, q.max_file_bytes) {
            Ok(Read::Content(b)) => b,
            Ok(Read::TooLarge) => {
                out.files_skipped_too_large += 1;
                continue;
            }
            Err(e) => {
                // FS-011: one unreadable file must not end the scan.
                out.files_failed += 1;
                tracing::debug!(path = %target.path.display(), error = %e, "literal scan skipped a file");
                continue;
            }
        };
        if looks_binary(&bytes) {
            out.files_skipped_binary += 1;
            continue;
        }
        out.files_scanned += 1;

        let remaining = q.max_total_matches.saturating_sub(out.hits.len());
        let budget = q.max_matches_per_file.min(remaining);
        match scan_buffer(
            &bytes,
            &matcher,
            q,
            budget,
            target,
            cancel,
            started,
            &mut out.hits,
        ) {
            Scanned::Stopped(reason) => {
                out.stopped = reason;
                break;
            }
            // The budget cut this file short. Which budget it was decides
            // whether the whole scan is over or only this file is incomplete.
            Scanned::Truncated => {
                if out.hits.len() >= q.max_total_matches {
                    out.stopped = StopReason::MatchLimit;
                    break;
                }
                out.files_truncated += 1;
            }
            Scanned::Done => {}
        }
    }

    out.elapsed = started.elapsed();
    tracing::debug!(
        hits = out.hits.len(),
        scanned = out.files_scanned,
        skipped_not_resident = out.files_skipped_not_resident,
        elapsed_ms = out.elapsed.as_millis(),
        stopped = ?out.stopped,
        "literal scan"
    );
    Ok(out)
}

fn stop_now(cancel: &AtomicBool, started: Instant, budget: Duration) -> Option<StopReason> {
    if cancel.load(Ordering::Relaxed) {
        return Some(StopReason::Cancelled);
    }
    if started.elapsed() >= budget {
        return Some(StopReason::TimeBudget);
    }
    None
}

/// How one file's scan ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scanned {
    /// Every match in the file is in the result.
    Done,
    /// A budget was reached and the file has more matches than were reported.
    Truncated,
    /// The whole scan must stop.
    Stopped(StopReason),
}

/// Match inside one file's bytes.
#[allow(clippy::too_many_arguments)]
fn scan_buffer(
    bytes: &[u8],
    matcher: &Matcher,
    q: &LiteralQuery,
    budget: usize,
    target: &LiteralTarget,
    cancel: &AtomicBool,
    started: Instant,
    hits: &mut Vec<LiteralHit>,
) -> Scanned {
    // Line starts are walked once, forwards, alongside the matches — which
    // arrive in ascending order — so this stays O(file), not O(matches × file).
    let mut lines = LineIndex::new(bytes);
    let mut at = 0usize;
    let mut found = 0usize;
    while found < budget {
        if found > 0 && found % CHECK_EVERY_MATCHES == 0 {
            if let Some(reason) = stop_now(cancel, started, q.time_budget) {
                return Scanned::Stopped(reason);
            }
        }
        let Some((start, end)) = matcher.find_at(bytes, at) else {
            return Scanned::Done;
        };
        let (line_no, line_start, line_end) = lines.locate(start);
        hits.push(LiteralHit {
            file_id: target.file_id,
            path: target.path.clone(),
            line: line_no,
            span: SourceSpan::Bytes {
                start: start as u64,
                end: end as u64,
            },
            line_span: SourceSpan::Lines {
                start: line_no,
                end: line_no,
            },
            snippet: line_snippet(bytes, line_start, line_end, start, end, q.context_chars),
        });
        found += 1;
        // Zero-width regex matches (`^`, `\b`) would otherwise spin here.
        at = if end > start { end } else { start + 1 };
        if at >= bytes.len() {
            return Scanned::Done;
        }
    }
    // The budget ran out. Say whether anything was actually left behind, rather
    // than reporting an incomplete answer for a file that happened to end here.
    if matcher.find_at(bytes, at).is_some() {
        Scanned::Truncated
    } else {
        Scanned::Done
    }
}

/// Walks newline positions forwards. `locate` must be called with ascending
/// offsets, which is how matches arrive.
struct LineIndex<'a> {
    bytes: &'a [u8],
    line: u32,
    line_start: usize,
}

impl<'a> LineIndex<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            line: 1,
            line_start: 0,
        }
    }

    /// `(1-based line, line start offset, line end offset exclusive of `\n`)`.
    fn locate(&mut self, offset: usize) -> (u32, usize, usize) {
        while let Some(nl) = memchr::memchr(b'\n', &self.bytes[self.line_start..]) {
            let abs = self.line_start + nl;
            if abs >= offset {
                break;
            }
            self.line_start = abs + 1;
            self.line = self.line.saturating_add(1);
        }
        let end = memchr::memchr(b'\n', &self.bytes[self.line_start..])
            .map(|i| self.line_start + i)
            .unwrap_or(self.bytes.len());
        (self.line, self.line_start, end)
    }
}

/// The matched line, trimmed to a window around the match, with the match
/// offsets relative to the returned text.
fn line_snippet(
    bytes: &[u8],
    line_start: usize,
    line_end: usize,
    match_start: usize,
    match_end: usize,
    context: usize,
) -> Snippet {
    let win_start = line_start.max(match_start.saturating_sub(context));
    let win_end = line_end
        .min(match_end.saturating_add(context))
        .max(win_start);
    // Lossy on purpose: a file that is mostly UTF-8 with one bad byte must
    // still produce a readable line rather than no result at all.
    let head = String::from_utf8_lossy(&bytes[win_start..match_start.clamp(win_start, win_end)]);
    let mid = String::from_utf8_lossy(
        &bytes[match_start.clamp(win_start, win_end)..match_end.clamp(win_start, win_end)],
    );
    let tail = String::from_utf8_lossy(&bytes[match_end.clamp(win_start, win_end)..win_end]);
    let mut text = String::with_capacity(head.len() + mid.len() + tail.len());
    text.push_str(head.trim_start_matches(['\r']));
    let start = text.len();
    text.push_str(&mid);
    let end = text.len();
    text.push_str(tail.trim_end_matches(['\r']));
    Snippet {
        text,
        matches: if end > start {
            vec![MatchRange { start, end }]
        } else {
            Vec::new()
        },
    }
}

enum Read {
    Content(Vec<u8>),
    TooLarge,
}

fn read_bounded(path: &Path, max: u64) -> Result<Read> {
    let meta = std::fs::metadata(path)?;
    if !meta.is_file() {
        return Err(Error::new(
            Code::FsNotFound,
            "That path is not a regular file, so there was nothing to search in it.",
        )
        .with_context(path.display().to_string()));
    }
    if meta.len() > max {
        tracing::debug!(path = %path.display(), size = meta.len(), "literal scan skipped a large file");
        return Ok(Read::TooLarge);
    }
    Ok(Read::Content(std::fs::read(path)?))
}

/// A NUL byte in the first 8 KB. The same heuristic `grep` uses, and the same
/// one that keeps a search from dumping a binary onto a terminal.
fn looks_binary(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(8192)];
    memchr::memchr(0, head).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn never() -> AtomicBool {
        AtomicBool::new(false)
    }

    fn target(dir: &Path, name: &str, body: &str, tier: TierState) -> LiteralTarget {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        LiteralTarget::new(FileId::new(), p, tier)
    }

    #[test]
    fn line_numbers_are_one_based_and_ascending() {
        let dir = tempfile::tempdir().unwrap();
        let t = target(
            dir.path(),
            "a.txt",
            "alpha\nbeta\ngamma needle\ndelta\nneedle again\n",
            TierState::Resident,
        );
        let out = literal_search(&[t], &LiteralQuery::new("needle"), &never()).unwrap();
        let lines: Vec<u32> = out.hits.iter().map(|h| h.line).collect();
        assert_eq!(lines, vec![3, 5]);
        assert!(out.stopped.is_complete());
    }

    #[test]
    fn snippet_offsets_point_at_the_matched_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let t = target(
            dir.path(),
            "a.txt",
            "the refresh token rotates\n",
            TierState::Resident,
        );
        let out = literal_search(&[t], &LiteralQuery::new("refresh"), &never()).unwrap();
        assert_eq!(out.hits.len(), 1);
        assert_eq!(out.hits[0].snippet.matched_text(), vec!["refresh"]);
        assert_eq!(
            out.hits[0].span,
            SourceSpan::Bytes { start: 4, end: 11 },
            "byte span must be into the file, not the snippet"
        );
    }

    #[test]
    fn case_and_word_options_behave_like_grep() {
        let dir = tempfile::tempdir().unwrap();
        let body = "Token tokenize token\n";
        let t = |tier| target(dir.path(), "a.txt", body, tier);

        let sensitive = literal_search(
            &[t(TierState::Resident)],
            &LiteralQuery::new("Token"),
            &never(),
        )
        .unwrap();
        assert_eq!(sensitive.hits.len(), 1);

        let insensitive = literal_search(
            &[t(TierState::Resident)],
            &LiteralQuery::new("token").ignore_case(),
            &never(),
        )
        .unwrap();
        assert_eq!(insensitive.hits.len(), 3);

        let words = literal_search(
            &[t(TierState::Resident)],
            &LiteralQuery::new("token").ignore_case().whole_word(true),
            &never(),
        )
        .unwrap();
        assert_eq!(words.hits.len(), 2, "`tokenize` is not the word `token`");
    }

    #[test]
    fn literal_mode_does_not_interpret_regex_metacharacters() {
        let dir = tempfile::tempdir().unwrap();
        let t = target(dir.path(), "a.txt", "a.c and abc\n", TierState::Resident);
        let out = literal_search(&[t], &LiteralQuery::new("a.c"), &never()).unwrap();
        assert_eq!(out.hits.len(), 1, "`.` must be a full stop, not any-char");
        assert_eq!(out.hits[0].snippet.matched_text(), vec!["a.c"]);
    }

    #[test]
    fn a_bad_regex_is_a_clean_error() {
        let e = Matcher::compile(&LiteralQuery::new("(unclosed").regex()).unwrap_err();
        assert_eq!(e.code(), Code::CfgInvalid);
        assert!(e.message().len() > 30);
        let e = Matcher::compile(&LiteralQuery::new("")).unwrap_err();
        assert_eq!(e.code(), Code::CfgInvalid);
    }

    #[test]
    fn binary_and_oversized_files_are_skipped_and_counted() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("b.bin");
        std::fs::write(&bin, b"needle\0\0\0needle").unwrap();
        let big = dir.path().join("big.txt");
        std::fs::write(&big, "needle".repeat(1000)).unwrap();

        let q = LiteralQuery {
            max_file_bytes: 100,
            ..LiteralQuery::new("needle")
        };
        let out = literal_search(
            &[
                LiteralTarget::new(FileId::new(), bin, TierState::Resident),
                LiteralTarget::new(FileId::new(), big, TierState::Resident),
            ],
            &q,
            &never(),
        )
        .unwrap();
        assert!(out.hits.is_empty());
        assert_eq!(out.files_skipped_binary, 1);
        assert_eq!(out.files_skipped_too_large, 1);
        assert!(out.has_gaps(), "a scan with skips must say so");
    }

    #[test]
    fn a_missing_file_does_not_end_the_scan() {
        let dir = tempfile::tempdir().unwrap();
        let good = target(dir.path(), "a.txt", "needle\n", TierState::Resident);
        let gone = LiteralTarget::new(FileId::new(), dir.path().join("nope"), TierState::Resident);
        let out = literal_search(&[gone, good], &LiteralQuery::new("needle"), &never()).unwrap();
        assert_eq!(
            out.hits.len(),
            1,
            "FS-011: one bad file, the rest still run"
        );
        assert_eq!(out.files_failed, 1);
    }

    #[test]
    fn the_match_limit_stops_the_scan_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let t = target(
            dir.path(),
            "a.txt",
            &"needle\n".repeat(50),
            TierState::Resident,
        );
        let out = literal_search(
            &[t],
            &LiteralQuery::new("needle").max_total_matches(5),
            &never(),
        )
        .unwrap();
        assert_eq!(out.hits.len(), 5);
        assert!(!out.stopped.is_complete());
    }

    #[test]
    fn a_zero_width_regex_match_terminates() {
        // `\b` matches empty. Without the `start + 1` step this loops forever.
        let dir = tempfile::tempdir().unwrap();
        let t = target(dir.path(), "a.txt", "one two three\n", TierState::Resident);
        let out = literal_search(&[t], &LiteralQuery::new(r"\b").regex(), &never()).unwrap();
        assert!(!out.hits.is_empty());
    }

    #[test]
    fn non_utf8_content_is_still_searchable() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("latin1.txt");
        // 0xE9 is `é` in Latin-1 and invalid UTF-8.
        std::fs::write(&p, b"caf\xe9 needle here\n").unwrap();
        let out = literal_search(
            &[LiteralTarget::new(FileId::new(), p, TierState::Resident)],
            &LiteralQuery::new("needle"),
            &never(),
        )
        .unwrap();
        assert_eq!(out.hits.len(), 1);
        assert_eq!(out.hits[0].snippet.matched_text(), vec!["needle"]);
    }
}
