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
pub(crate) fn to_hit(rank: usize, h: &marrow_index::TextHit, roots: &[String]) -> SearchHit {
    let line = match &h.span {
        SourceSpan::Lines { start, .. } => Some(*start),
        _ => None,
    };
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
        excerpt: h.snippet.text.clone(),
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
];

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(COMMAND_NAMES.len(), 7);
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
