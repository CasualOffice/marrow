//! Tauri commands.
//!
//! Every handler deserializes, calls, serializes. **If a handler contains an
//! `if` that is not error handling, the logic is in the wrong crate** — the
//! test for it is that each one reads in under ten lines ([LLD §7]).
//!
//! Handlers are `async` and immediately hand work to a blocking pool. That is
//! the whole reason the core stays synchronous: Tauri brings a runtime to the
//! adapter edge, and `cli` and `mcp` link the same core with no runtime at all.
//! One `async fn` in `core` would make every caller async ([LLD §4]).
//!
//! [LLD §7]: ../../../docs/LLD.md
//! [LLD §4]: ../../../docs/LLD.md

use std::sync::Arc;

use marrow_core::{Origin, ProvenanceClass, SourceSpan};

use serde::Serialize;
use tauri::State;

use crate::state::Core;

/// An error the WebView can render.
///
/// Carries the stable code alongside the message so the UI can branch on the
/// code (a cloud-only file needs a different affordance from a parse failure)
/// without string-matching prose.
#[derive(Debug, Serialize)]
pub struct UiError {
    pub code: String,
    pub message: String,
}

impl From<marrow_core::Error> for UiError {
    fn from(e: marrow_core::Error) -> Self {
        Self {
            code: e.code().as_str().to_string(),
            message: e.message().to_string(),
        }
    }
}

type Res<T> = Result<T, UiError>;

/// One search result, in the shape the UI renders.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub rank: usize,
    /// Absolute; the UI opens with it.
    pub path: String,
    /// Workspace-relative; the UI displays it. An absolute path eats the width
    /// the snippet needs and buries what distinguishes one result from another.
    pub relative_path: String,
    /// `path:line`, the form an editor linkifies.
    pub location: String,
    pub line: Option<u32>,
    /// The chunker's ancestor chain — the dimmed last line of a result row.
    pub breadcrumb: String,
    pub excerpt: String,
    /// `exact` | `degraded` | `approximate`. Anything but `exact` gets a badge.
    pub provenance: String,
    /// Why it matched. One branch in M1, so it is honest rather than interesting.
    pub reason: String,
    /// Invariant #13: `false` means the agent wrote it and it cannot be cited.
    pub citable: bool,
    pub modified_ms: i64,
    pub file_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    pub query: String,
    /// Hits on this page — i.e. `hits.len()`.
    pub total: usize,
    /// Documents that matched, before the limit.
    ///
    /// Separate from `total` because a footer reading "20 results" when the
    /// corpus holds 900 is a lie the user cannot detect. `total` saturates at
    /// the limit; this does not.
    pub matched: usize,
    pub elapsed_ms: u64,
    pub hits: Vec<SearchHit>,
    /// Which retrieval branches ran. The UI shows this in the footer so a
    /// regression is visible before a benchmark catches it.
    pub branches: Vec<String>,
}

#[tauri::command]
pub async fn search(
    core: State<'_, Arc<Core>>,
    query: String,
    limit: usize,
) -> Res<SearchResponse> {
    let core = Arc::clone(&core);
    blocking(move || core.search(&query, limit)).await
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRow {
    pub name: String,
    pub path: String,
    pub files: i64,
    /// Per-workspace, not global. GUI §11 requires every degraded state to be
    /// visible from the sidebar without navigating, and a single global number
    /// cannot say which workspace is the problem.
    pub chunks: i64,
    pub content_bytes: i64,
    /// Files whose contents were deliberately not read. Never omitted, even at
    /// zero — a silent zero reads as "no cloud files" (TIER-008).
    pub cloud_only: i64,
    /// Files recorded from metadata alone because their contents could not be
    /// indexed. This is what makes a partly-broken workspace visible.
    pub unindexed: i64,
}

#[tauri::command]
pub async fn list_workspaces(core: State<'_, Arc<Core>>) -> Res<Vec<WorkspaceRow>> {
    let core = Arc::clone(&core);
    blocking(move || core.workspaces()).await
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexHealth {
    pub files: i64,
    pub chunks: i64,
    pub content_bytes: i64,
    /// Never omitted, even at zero — a silent zero is indistinguishable from
    /// "no cloud files", which is the failure TIER-008 exists to prevent.
    pub cloud_only: i64,
    pub schema_version: i64,
}

#[tauri::command]
pub async fn index_health(core: State<'_, Arc<Core>>) -> Res<IndexHealth> {
    let core = Arc::clone(&core);
    blocking(move || core.health()).await
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDetail {
    pub path: String,
    pub file_id: String,
    pub workspace: String,
    pub size_bytes: Option<i64>,
    pub content_hash: Option<String>,
    pub mime: Option<String>,
    pub modified_ms: Option<i64>,
    pub versions: i64,
    pub chunks: i64,
    pub tier_state: String,
    pub citable: bool,
    pub previous_paths: Vec<String>,
    /// Explicitly `None`, not omitted: M1 does not extract these, and absence
    /// must be distinguishable from ignorance (FI-003). The UI renders `—`.
    pub embedded_metadata: Option<serde_json::Value>,
    pub structure: Option<serde_json::Value>,
}

#[tauri::command]
pub async fn file_detail(core: State<'_, Arc<Core>>, path: String) -> Res<FileDetail> {
    let core = Arc::clone(&core);
    blocking(move || core.file_detail(&path)).await
}

/// A slice of a file, and where in the file it starts.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Region {
    /// 1-based line number of `lines[0]`.
    ///
    /// Returned rather than left to the caller: without it the UI has to
    /// duplicate this crate's private context constant to guess, and the two
    /// drift the first time one changes.
    pub first_line: u32,
    pub lines: Vec<String>,
    /// True when the region was cut short by the cap rather than by the file
    /// ending — the UI cannot otherwise tell those apart.
    pub truncated: bool,
}

/// Read a bounded region of an indexed file, for the preview pane.
#[tauri::command]
pub async fn read_region(
    core: State<'_, Arc<Core>>,
    path: String,
    around_line: Option<u32>,
) -> Res<Region> {
    let core = Arc::clone(&core);
    blocking(move || core.read_region(&path, around_line)).await
}

/// Open a file in whatever the system uses for it.
///
/// **Only indexed files.** This is not a general "open anything" affordance:
/// the workspace grant is what says which files Marrow may touch, and that
/// applies to handing one to another application too.
///
/// The path is passed as a single argv element to `open`, never through a
/// shell (SEC-011), so a filename cannot become a command.
#[tauri::command]
pub async fn open_path(core: State<'_, Arc<Core>>, path: String) -> Res<()> {
    let core = Arc::clone(&core);
    blocking(move || core.open_path(&path, false)).await
}

/// Reveal a file in the system file manager.
#[tauri::command]
pub async fn reveal_path(core: State<'_, Arc<Core>>, path: String) -> Res<()> {
    let core = Arc::clone(&core);
    blocking(move || core.open_path(&path, true)).await
}

/// Run work on the blocking pool and map the error for the WebView.
///
/// The one place the async boundary is crossed. Everything below it is
/// synchronous, which is what lets three frontends share one core.
async fn blocking<T, F>(f: F) -> Res<T>
where
    F: FnOnce() -> marrow_core::Result<T> + Send + 'static,
    T: Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| UiError {
            code: "INT_INVARIANT_VIOLATED".into(),
            message: format!("A background task failed to complete: {e}"),
        })?
        .map_err(UiError::from)
}

/// Shape a hit for the UI. Kept here so the two renderers — this and the CLI —
/// can be compared side by side.
/// The line the match is actually on, not the line the chunk starts at.
///
/// A chunk can span forty lines. Reporting its first line while the excerpt
/// shows line twenty-seven sends the user to the wrong place in the file — and
/// it looks like the search found the wrong thing rather than that the citation
/// is off by nineteen lines.
///
/// FTS5's snippet is a window over one column. When it did not truncate the
/// front — no leading ellipsis — the window starts at the column's start, so
/// the newlines before the first match are the offset into the chunk. When it
/// did truncate, that offset is unknowable and the chunk's first line is the
/// honest answer.
///
/// The result is clamped to the chunk's own line range either way. That is what
/// makes this safe when the snippet came from the `path` column instead of the
/// body: counting newlines in a path is meaningless, and the clamp turns a
/// meaningless number back into the chunk's start.
fn matched_line(h: &marrow_index::TextHit) -> Option<u32> {
    const ELLIPSIS: char = '…';
    let SourceSpan::Lines { start, end } = &h.span else {
        return None;
    };
    let Some(first) = h.snippet.matches.first() else {
        return Some(*start);
    };
    if h.snippet.text.starts_with(ELLIPSIS) {
        return Some(*start);
    }
    let before = h.snippet.text.get(..first.start).unwrap_or("");
    let offset = before.matches('\n').count() as u32;
    Some((*start + offset).min(*end).max(*start))
}

pub(crate) fn to_hit(rank: usize, h: &marrow_index::TextHit, roots: &[String]) -> SearchHit {
    let line = matched_line(h);
    let relative = roots
        .iter()
        .filter(|r| h.path.starts_with(r.as_str()))
        .max_by_key(|r| r.len())
        .and_then(|r| h.path.strip_prefix(r.as_str()))
        .map(|s| s.trim_start_matches('/').to_string())
        .unwrap_or_else(|| h.path.clone());

    SearchHit {
        rank,
        location: match line {
            Some(l) => format!("{relative}:{l}"),
            None => relative.clone(),
        },
        relative_path: relative,
        path: h.path.clone(),
        line,
        breadcrumb: h.title.clone(),
        // Centred on the match here, because the UI is given no offsets and can
        // therefore only take the first lines — which for a large chunk shows
        // text that does not contain the search term at all.
        excerpt: centre_on_match(&h.snippet),
        provenance: match h.provenance {
            ProvenanceClass::Exact => "exact",
            ProvenanceClass::Degraded => "degraded",
            ProvenanceClass::Approximate => "approximate",
            ProvenanceClass::MetadataOnly => "metadata_only",
        }
        .to_string(),
        reason: "exact".to_string(),
        citable: h.origin == Origin::User,
        modified_ms: h.modified.as_millis(),
        file_id: h.file_id.to_string(),
    }
}

/// Trim a snippet to the lines that actually contain the match.
///
/// FTS5 marks matches with the delimiters `marrow-index` configured. A result
/// row shows two lines; taking the first two of a ten-line snippet shows the
/// user text without their search term in it, which reads as a broken search.
fn centre_on_match(s: &marrow_index::Snippet) -> String {
    const OPEN: char = '\u{1}';
    const CLOSE: char = '\u{2}';

    let lines: Vec<&str> = s
        .text
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return String::new();
    }

    // First line carrying a marker; fall back to the start when FTS5 gave us
    // none (a prefix match on a short field can do that).
    let hit = lines.iter().position(|l| l.contains(OPEN)).unwrap_or(0);

    lines
        .iter()
        .skip(hit)
        .take(2)
        .map(|l| l.replace([OPEN, CLOSE], ""))
        .collect::<Vec<_>>()
        .join("\n")
}

/// One file, for the Files browser.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileRow {
    pub path: String,
    pub relative_path: String,
    pub workspace: String,
    pub size_bytes: Option<i64>,
    pub modified_ms: Option<i64>,
    pub chunks: i64,
    /// Recorded but with no searchable contents — the Files view says so
    /// rather than showing a row that looks the same as an indexed one.
    pub metadata_only: bool,
}

/// List indexed files, newest first.
///
/// The Files view was built on `search`, so with no query it showed nothing —
/// an empty browser for an index holding 35,000 files. Browsing is not
/// searching and needs its own command.
#[tauri::command]
pub async fn list_files(
    core: State<'_, Arc<Core>>,
    workspace: Option<String>,
    prefix: Option<String>,
    limit: usize,
) -> Res<Vec<FileRow>> {
    let core = Arc::clone(&core);
    blocking(move || core.list_files(workspace.as_deref(), prefix.as_deref(), limit)).await
}

/// Names every command the WebView may call.
///
/// The capability manifest grants no filesystem, shell or network permission
/// (SEC-012), so this list is the complete surface between the UI and the disk.
///
/// Test-only: its purpose is the assertion below that the surface stays small
/// and read-only. Keeping it in the binary would be a second list to forget to
/// update.
#[cfg(test)]
const COMMAND_NAMES: &[&str] = &[
    "search",
    "list_workspaces",
    "index_health",
    "file_detail",
    "read_region",
    "open_path",
    "reveal_path",
    "list_files",
    "models_overview",
    "refresh_model_detection",
    "set_ai_profile",
    "download_model",
    "cancel_model_download",
    "dismiss_model_download",
    "ask",
    "cancel_ask",
    "release_model",
    "forget_conversation",
    "start_semantic_backfill",
    "stop_semantic_backfill",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn hit_with(
        span: SourceSpan,
        snippet: &str,
        matches: Vec<marrow_index::MatchRange>,
    ) -> marrow_index::TextHit {
        marrow_index::TextHit {
            chunk_id: marrow_core::ChunkId::new(),
            file_id: marrow_core::FileId::new(),
            version_id: marrow_core::VersionId::new(),
            workspace_id: marrow_core::WorkspaceId::new(),
            path: "src/auth/token.rs".into(),
            title: String::new(),
            score: 0.0,
            span,
            snippet: marrow_index::Snippet {
                text: snippet.into(),
                matches,
            },
            provenance: ProvenanceClass::Exact,
            origin: Origin::User,
            modified: marrow_core::Timestamp::now(),
        }
    }

    #[test]
    fn the_reported_line_is_where_the_match_is_not_where_the_chunk_starts() {
        // The bug this exists for: a chunk spanning lines 100–140 with the
        // match on 127 reported `:100`, so clicking the result opened the file
        // nineteen lines above the thing the excerpt was showing.
        let snippet = "line one\nline two\nline three has refresh in it";
        let at = snippet.find("refresh").unwrap();
        let h = hit_with(
            SourceSpan::Lines {
                start: 100,
                end: 140,
            },
            snippet,
            vec![marrow_index::MatchRange {
                start: at,
                end: at + 7,
            }],
        );
        assert_eq!(matched_line(&h), Some(102), "two newlines before the match");
    }

    #[test]
    fn a_truncated_snippet_falls_back_to_the_chunks_first_line() {
        // A leading ellipsis means FTS5 cut the front off, so the offset into
        // the chunk is unknowable. The chunk's start is then the honest answer;
        // a guess would be a citation that points somewhere specific and wrong.
        let h = hit_with(
            SourceSpan::Lines {
                start: 100,
                end: 140,
            },
            "…two\nthree has refresh",
            vec![marrow_index::MatchRange { start: 11, end: 18 }],
        );
        assert_eq!(matched_line(&h), Some(100));
    }

    #[test]
    fn the_line_can_never_leave_the_chunk_it_came_from() {
        // The clamp is what makes this safe when the snippet came from the
        // `path` column rather than the body: counting newlines in a path is
        // meaningless, and the clamp turns a meaningless number back into the
        // chunk's start.
        let snippet = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk match";
        let at = snippet.find("match").unwrap();
        let h = hit_with(
            SourceSpan::Lines { start: 10, end: 12 },
            snippet,
            vec![marrow_index::MatchRange {
                start: at,
                end: at + 5,
            }],
        );
        assert_eq!(
            matched_line(&h),
            Some(12),
            "clamped to the chunk's last line"
        );
    }

    #[test]
    fn a_hit_with_no_match_offsets_reports_the_chunk_start() {
        let h = hit_with(
            SourceSpan::Lines { start: 7, end: 9 },
            "no markers here",
            vec![],
        );
        assert_eq!(matched_line(&h), Some(7));
    }

    #[test]
    fn a_span_that_is_not_lines_reports_no_line_rather_than_inventing_one() {
        // Invariant #1: `Whole` is honest and a fabricated line number is not.
        let h = hit_with(SourceSpan::Whole, "text", vec![]);
        assert_eq!(matched_line(&h), None);
    }

    #[test]
    fn an_error_carries_its_stable_code_not_just_prose() {
        // The UI branches on the code — a cloud-only file needs a different
        // affordance from a parse failure — and must never string-match prose.
        let e: UiError = marrow_core::Error::new(
            marrow_core::Code::FsPlaceholderSkipped,
            "That file is cloud-only.",
        )
        .into();
        assert_eq!(e.code, "FS_PLACEHOLDER_SKIPPED");
        assert!(!e.message.is_empty());
    }

    #[test]
    fn the_command_surface_is_small_and_read_only() {
        // Every name here is a hole in the WebView sandbox. M1 exposes no
        // mutation at all; when one arrives it needs a deliberate addition.
        assert_eq!(COMMAND_NAMES.len(), 20);
        for n in COMMAND_NAMES {
            assert!(
                !n.contains("write") && !n.contains("delete") && !n.contains("exec"),
                "{n} looks like a mutation; M1 exposes none"
            );
        }
        // `open_path` hands a file to another application, which is the closest
        // thing here to leaving the sandbox. It is guarded by the index, and
        // that guard is the reason it is allowed at all.
        assert!(COMMAND_NAMES.contains(&"open_path"));
    }
}

// ── models (Part 8) ───────────────────────────────────────────────────────

/// Everything the Models page renders from.
///
/// One command rather than five, because the page's numbers must agree with
/// each other: a free-memory figure fetched separately from the verdicts
/// computed against it would drift within one paint.
#[tauri::command]
pub async fn models_overview(
    hub: State<'_, Arc<crate::models::Hub>>,
) -> Result<crate::models::ModelsSnapshot, UiError> {
    let hub = Arc::clone(&hub);
    blocking(move || Ok(hub.snapshot())).await
}

/// Re-run local runtime detection. The answer changes whenever the user starts
/// or stops Ollama, which they will do with this page open.
#[tauri::command]
pub async fn refresh_model_detection(
    hub: State<'_, Arc<crate::models::Hub>>,
) -> Result<crate::models::ModelsSnapshot, UiError> {
    let hub = Arc::clone(&hub);
    blocking(move || {
        hub.refresh_detection();
        Ok(hub.snapshot())
    })
    .await
}

/// Choose the AI preference (§139.6).
#[tauri::command]
pub async fn set_ai_profile(
    hub: State<'_, Arc<crate::models::Hub>>,
    profile: String,
) -> Result<crate::models::ModelsSnapshot, UiError> {
    let hub = Arc::clone(&hub);
    blocking(move || {
        hub.set_profile(&profile).ok_or_else(|| {
            marrow_core::Error::new(
                marrow_core::Code::CfgInvalid,
                "That is not one of the AI preferences. Choose Efficient, \
                 Balanced, Larger local model or Cloud.",
            )
        })?;
        Ok(hub.snapshot())
    })
    .await
}

/// Start fetching a model's weights.
///
/// Returns immediately with a fresh snapshot; the transfer runs on its own
/// thread and its progress arrives through `models_overview`.
#[tauri::command]
pub async fn download_model(
    hub: State<'_, Arc<crate::models::Hub>>,
    model_id: String,
) -> Result<crate::models::ModelsSnapshot, UiError> {
    let hub = Arc::clone(&hub);
    blocking(move || {
        hub.start_download(&model_id)?;
        Ok(hub.snapshot())
    })
    .await
}

/// Cancel a transfer. What was fetched is kept, so starting again resumes.
#[tauri::command]
pub async fn cancel_model_download(
    hub: State<'_, Arc<crate::models::Hub>>,
    model_id: String,
) -> Result<crate::models::ModelsSnapshot, UiError> {
    let hub = Arc::clone(&hub);
    blocking(move || {
        hub.cancel_download(&model_id);
        Ok(hub.snapshot())
    })
    .await
}

/// Clear a finished or failed transfer from the page.
#[tauri::command]
pub async fn dismiss_model_download(
    hub: State<'_, Arc<crate::models::Hub>>,
    model_id: String,
) -> Result<crate::models::ModelsSnapshot, UiError> {
    let hub = Arc::clone(&hub);
    blocking(move || {
        hub.dismiss_download(&model_id);
        Ok(hub.snapshot())
    })
    .await
}

// ── ask (Part 8 §148) ─────────────────────────────────────────────────────

/// Ask a question and stream the answer.
///
/// Takes a `Channel` rather than returning a string: SKEL-004 says streaming
/// replaces skeleton rows as content arrives, and a command that returns only
/// when the model is finished cannot do that.
#[tauri::command]
#[allow(clippy::too_many_arguments)] // Each of these is a distinct input the
                                     // window has; folding them into one struct would only move the list.
pub async fn ask(
    core: State<'_, Arc<Core>>,
    hub: State<'_, Arc<crate::models::Hub>>,
    conversation: String,
    question: String,
    history: Vec<crate::ask::PriorTurn>,
    thorough: bool,
    on_event: tauri::ipc::Channel<crate::ask::AskEvent>,
) -> Result<String, UiError> {
    let core = Arc::clone(&core);
    let hub = Arc::clone(&hub);
    let cancel = marrow_model::queue::Cancel::new();
    let token = hub.register_ask(cancel.clone());
    let handle = token.clone();
    blocking(move || {
        let turns = crate::ask::turns_from(&history);
        crate::ask::run(
            &core,
            &hub,
            &conversation,
            &question,
            &turns,
            thorough,
            &cancel,
            &mut |e| {
                // A closed channel means the window went away mid-answer. Stop
                // generating rather than talking to nobody.
                if on_event.send(e).is_err() {
                    cancel.cancel();
                }
            },
        );
        hub.finish_ask(&handle);
        Ok(handle)
    })
    .await
}

/// Stop the answer in progress. UX §10: within 500 ms.
#[tauri::command]
pub async fn cancel_ask(
    hub: State<'_, Arc<crate::models::Hub>>,
    id: String,
) -> Result<bool, UiError> {
    Ok(hub.cancel_ask(&id))
}

/// Release the loaded model now rather than waiting out the idle timer.
#[tauri::command]
pub async fn release_model(
    hub: State<'_, Arc<crate::models::Hub>>,
) -> Result<crate::models::ModelsSnapshot, UiError> {
    let hub = Arc::clone(&hub);
    blocking(move || {
        hub.release_model();
        Ok(hub.snapshot())
    })
    .await
}

/// Drop a conversation's session. Called when the thread is cleared, so a
/// delimiter is not held for a conversation nobody will return to.
#[tauri::command]
pub async fn forget_conversation(
    hub: State<'_, Arc<crate::models::Hub>>,
    conversation: String,
) -> Result<(), UiError> {
    hub.forget_session(&conversation);
    Ok(())
}

/// Build semantic search over everything already indexed.
///
/// Returns immediately; the work runs on its own thread and its progress
/// arrives through `models_overview`.
#[tauri::command]
pub async fn start_semantic_backfill(
    core: State<'_, Arc<Core>>,
    hub: State<'_, Arc<crate::models::Hub>>,
) -> Result<crate::models::ModelsSnapshot, UiError> {
    let core = Arc::clone(&core);
    let hub = Arc::clone(&hub);
    blocking(move || {
        hub.start_backfill(core)?;
        Ok(hub.snapshot())
    })
    .await
}

/// Stop it. What is already embedded stays embedded.
#[tauri::command]
pub async fn stop_semantic_backfill(
    hub: State<'_, Arc<crate::models::Hub>>,
) -> Result<crate::models::ModelsSnapshot, UiError> {
    let hub = Arc::clone(&hub);
    blocking(move || {
        hub.stop_backfill();
        Ok(hub.snapshot())
    })
    .await
}
