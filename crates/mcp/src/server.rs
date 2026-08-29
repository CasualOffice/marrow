//! The stdio loop and tool dispatch.

use std::io::{BufRead, Write};

use marrow_core::{Result, TierState};
use marrow_index::{Fts5Index, TextIndex};
use marrow_store::Store;
use serde_json::{json, Value};
use tracing::{debug, warn};

use crate::protocol::{err, ok, tool_error, tool_ok, Request, RpcError, ServerDescriptor};
use crate::tools;

/// Hard cap on results, whatever a client asks for.
///
/// A model that asks for 1,000 hits gets a context window full of excerpts and
/// a worse answer than one that asks for 20.
const MAX_LIMIT: usize = 100;
const DEFAULT_LIMIT: usize = 20;

/// Cap on a single `read_file` response. An unbounded read is how a tool call
/// turns into a context-window incident.
const MAX_READ_BYTES: usize = 256 * 1024;

pub struct Server {
    store: Store,
    index: Fts5Index,
}

impl Server {
    pub fn new(store: Store) -> Result<Self> {
        let index = Fts5Index::open(&store)?;
        Ok(Self { store, index })
    }

    /// Handle one request. Returns `None` for notifications.
    pub fn handle(&self, req: &Request) -> Option<Value> {
        let id = req.id.clone().unwrap_or(Value::Null);
        if req.is_notification() {
            debug!(method = %req.method, "notification");
            return None;
        }

        Some(match req.method.as_str() {
            // Both spellings resolve to the same descriptor — see the note in
            // `protocol`. A client speaking either one gets a working server.
            "initialize" | "server/discover" => ok(
                id,
                serde_json::to_value(ServerDescriptor::default()).unwrap_or_else(|_| json!({})),
            ),
            "ping" => ok(id, json!({})),
            "tools/list" => ok(id, tools::list()),
            "tools/call" => self.call(id, &req.params),
            other => err(
                id,
                RpcError::MethodNotFound,
                &format!("Marrow does not implement `{other}`."),
            ),
        })
    }

    fn call(&self, id: Value, params: &Value) -> Value {
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return err(id, RpcError::InvalidParams, "`name` is required.");
        };
        if tools::find(name).is_none() {
            return err(
                id,
                RpcError::InvalidParams,
                &format!("No tool named `{name}`. Call tools/list for what exists."),
            );
        }
        let args = params.get("arguments").cloned().unwrap_or(json!({}));

        // A tool failure is a *successful* response carrying isError, so the
        // model sees the reason and can act on it. Only a protocol violation is
        // a JSON-RPC error.
        let outcome = match name {
            "search" => self.search(&args),
            "read_file" => self.read_file(&args),
            "file_info" => self.file_info(&args),
            "list_workspaces" => self.list_workspaces(),
            "index_status" => self.index_status(),
            _ => unreachable!("checked above"),
        };

        match outcome {
            Ok(v) => tool_ok(id, &v),
            Err(e) => {
                warn!(tool = name, error = %e, "tool failed");
                // The user-facing message names a cause and an action; that is
                // exactly what a model needs too.
                tool_error(id, e.message())
            }
        }
    }

    fn search(&self, args: &Value) -> Result<Value> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if query.is_empty() {
            return Err(marrow_core::Error::new(
                marrow_core::Code::CfgInvalid,
                "`query` is required and must not be empty.",
            ));
        }
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| (n as usize).clamp(1, MAX_LIMIT))
            .unwrap_or(DEFAULT_LIMIT);

        // Filters go INTO the query, not onto its results. Filtering afterwards
        // means the index applies `limit` first and the filter then discards
        // most of what came back — so a filter can report zero matches while
        // matching documents sit just past the cut. The port takes `Filters`
        // for exactly this reason.
        let mut filters = marrow_index::Filters::default();
        if let Some(ext) = args.get("extension").and_then(Value::as_str) {
            filters.extensions = vec![ext.trim_start_matches('.').to_lowercase()];
        }
        if let Some(sub) = args.get("path_contains").and_then(Value::as_str) {
            // GLOB, so the substring has to be wrapped rather than passed raw.
            filters.path_glob = Some(format!("*{sub}*"));
        }
        if let Some(name) = args.get("workspace").and_then(Value::as_str) {
            filters.workspace = Some(self.workspace_by_name(name)?);
        }

        let q = marrow_index::TextQuery::new(query)
            .limit(limit)
            .with_filters(filters);
        let hits = self.index.search(&q)?;
        let roots = self.roots()?;

        let results: Vec<Value> = hits
            .iter()
            .enumerate()
            .map(|(i, h)| {
                json!({
                    "rank": i + 1,
                    "path": h.path,
                    "relative_path": relative(&h.path, &roots),
                    "location": location(&h.path, &h.span),
                    "span": h.span,
                    "breadcrumb": h.title,
                    "excerpt": h.snippet.text,
                    "provenance": lower(&h.provenance),
                    // Invariant #13, surfaced in the payload rather than left
                    // for the caller to infer.
                    "origin": lower(&h.origin),
                    "citable": h.origin == marrow_core::Origin::User,
                    "file_id": h.file_id.to_string(),
                    "modified_ms": h.modified.as_millis(),
                })
            })
            .collect();

        Ok(json!({
            "query": query,
            "total": results.len(),
            "results": results,
        }))
    }

    fn read_file(&self, args: &Value) -> Result<Value> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("`path` is required."))?;

        // Only files Marrow has indexed. Reading an arbitrary path would make
        // this a general filesystem tool, which is not what it is and not what
        // the workspace grants authorised.
        let conn = self.store.reader()?;
        let known: i64 = conn
            .query_row(
                "SELECT count(*) FROM files WHERE current_path = ?1 AND status = 'ACTIVE'",
                [path],
                |r| r.get(0),
            )
            .map_err(|e| marrow_store::map_sqlite(e, "looking up a file"))?;
        if known == 0 {
            return Err(bad(
                "That file is not indexed. Call list_workspaces to see which folders \
                 Marrow has been granted.",
            ));
        }

        let tier: String = conn
            .query_row(
                "SELECT tier_state FROM files WHERE current_path = ?1 LIMIT 1",
                [path],
                |r| r.get(0),
            )
            .map_err(|e| marrow_store::map_sqlite(e, "reading tier state"))?;
        if tier != "RESIDENT" {
            // **Invariant #5.** Reading it is what triggers the download.
            return Err(bad(
                "That file is cloud-only. Its contents are not on this machine, and \
                 reading it would download them.",
            ));
        }

        let body = std::fs::read_to_string(path)?;
        let start = args
            .get("start_line")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .max(1) as usize;
        let end = args
            .get("end_line")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(usize::MAX);

        let mut text = String::new();
        let mut truncated = false;
        let mut last = start;
        for (n, line) in body.lines().enumerate() {
            let n = n + 1;
            if n < start {
                continue;
            }
            if n > end {
                break;
            }
            if text.len() + line.len() > MAX_READ_BYTES {
                truncated = true;
                break;
            }
            text.push_str(line);
            text.push('\n');
            last = n;
        }

        Ok(json!({
            "path": path,
            "start_line": start,
            "end_line": last,
            "truncated": truncated,
            "content": text,
        }))
    }

    fn file_info(&self, args: &Value) -> Result<Value> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("`path` is required."))?;
        let conn = self.store.reader()?;

        let row = conn
            .query_row(
                "SELECT f.file_id, f.tier_state, f.origin, w.name,
                        v.size_bytes, v.content_hash, v.mime, v.mtime_ms,
                        (SELECT count(*) FROM file_versions x WHERE x.file_id = f.file_id),
                        (SELECT count(*) FROM chunks c WHERE c.version_id = v.version_id)
                   FROM files f
                   JOIN workspaces w ON w.workspace_id = f.workspace_id
              LEFT JOIN file_versions v
                     ON v.file_id = f.file_id AND v.status = 'CURRENT'
                  WHERE f.current_path = ?1 AND f.status = 'ACTIVE'
                  LIMIT 1",
                [path],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<i64>>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, Option<String>>(6)?,
                        r.get::<_, Option<i64>>(7)?,
                        r.get::<_, i64>(8)?,
                        r.get::<_, i64>(9)?,
                    ))
                },
            )
            .map_err(|_| bad("That file is not indexed."))?;

        let (file_id, tier, origin, workspace, size, hash, mime, mtime, versions, chunks) = row;

        // Path history is the point of a stable file id: it is how a rename
        // stays the same file (invariant #2).
        let mut stmt = conn
            .prepare("SELECT path FROM file_paths WHERE file_id = ?1 ORDER BY observed_from")
            .map_err(|e| marrow_store::map_sqlite(e, "reading path history"))?;
        let history: Vec<String> = stmt
            .query_map([&file_id], |r| r.get(0))
            .and_then(|it| it.collect())
            .map_err(|e| marrow_store::map_sqlite(e, "reading path history"))?;

        Ok(json!({
            "path": path,
            "file_id": file_id,
            "workspace": workspace,
            "size_bytes": size,
            "content_hash": hash,
            "mime": mime,
            "modified_ms": mtime,
            "versions": versions,
            "chunks": chunks,
            "indexed_for_search": chunks > 0,
            "tier_state": tier.to_lowercase(),
            "origin": origin.to_lowercase(),
            "citable": origin == "USER",
            "previous_paths": history.iter().filter(|p| p.as_str() != path).collect::<Vec<_>>(),
            // Explicitly null rather than omitted: M1 does not extract these,
            // and absence must be distinguishable from ignorance (FI-003).
            "embedded_metadata": Value::Null,
            "structure": Value::Null,
            "entities": Value::Null,
        }))
    }

    fn list_workspaces(&self) -> Result<Value> {
        let conn = self.store.reader()?;
        let mut stmt = conn
            .prepare(
                "SELECT w.name, COALESCE(r.canonical_path,''),
                        (SELECT count(*) FROM files f
                          WHERE f.workspace_id = w.workspace_id AND f.status='ACTIVE')
                   FROM workspaces w
              LEFT JOIN workspace_roots r ON r.workspace_id = w.workspace_id
                  WHERE w.status = 'ACTIVE' ORDER BY w.name",
            )
            .map_err(|e| marrow_store::map_sqlite(e, "listing workspaces"))?;
        let rows: Vec<Value> = stmt
            .query_map([], |r| {
                Ok(json!({
                    "name": r.get::<_, String>(0)?,
                    "path": r.get::<_, String>(1)?,
                    "files": r.get::<_, i64>(2)?,
                }))
            })
            .and_then(|it| it.collect())
            .map_err(|e| marrow_store::map_sqlite(e, "listing workspaces"))?;
        Ok(json!({ "workspaces": rows }))
    }

    fn index_status(&self) -> Result<Value> {
        let conn = self.store.reader()?;
        let (files, bytes, cloud_only): (i64, i64, i64) = conn
            .query_row(
                "SELECT (SELECT count(*) FROM files WHERE status='ACTIVE'),
                        (SELECT COALESCE(sum(size_bytes),0) FROM file_versions WHERE status='CURRENT'),
                        (SELECT count(*) FROM files WHERE tier_state != 'RESIDENT')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(|e| marrow_store::map_sqlite(e, "reading index health"))?;
        let chunks = marrow_store::read::chunk_count(&conn)?;

        Ok(json!({
            "files_indexed": files,
            "content_bytes": bytes,
            "searchable_chunks": chunks,
            // Never a silent zero: a count of files deliberately not read is
            // the thing that explains a search which should have matched.
            "cloud_only_not_read": cloud_only,
            "schema_version": self.store.schema_version(),
        }))
    }

    /// Resolve a workspace name, refusing an unknown one.
    ///
    /// Silently returning no results for a typo'd name is the worst outcome:
    /// it is indistinguishable from "nothing matched", and a model will believe
    /// the second answer.
    fn workspace_by_name(&self, name: &str) -> Result<marrow_core::WorkspaceId> {
        let conn = self.store.reader()?;
        let id: Option<String> = conn
            .query_row(
                "SELECT workspace_id FROM workspaces WHERE name = ?1 AND status='ACTIVE'",
                [name],
                |r| r.get(0),
            )
            .ok();
        match id {
            Some(s) => s
                .parse()
                .map_err(|_| marrow_core::Error::invariant("bad workspace id in database")),
            None => Err(bad(&format!(
                "No workspace named `{name}`. Call list_workspaces to see what exists."
            ))),
        }
    }

    fn roots(&self) -> Result<Vec<String>> {
        let conn = self.store.reader()?;
        let mut stmt = conn
            .prepare("SELECT canonical_path FROM workspace_roots")
            .map_err(|e| marrow_store::map_sqlite(e, "reading roots"))?;
        let v = stmt
            .query_map([], |r| r.get(0))
            .and_then(|it| it.collect())
            .map_err(|e| marrow_store::map_sqlite(e, "reading roots"))?;
        Ok(v)
    }
}

fn bad(msg: &str) -> marrow_core::Error {
    marrow_core::Error::new(marrow_core::Code::CfgInvalid, msg)
}

fn lower<T: std::fmt::Debug>(v: &T) -> String {
    // Enum names are the wire form; snake_case them so a JSON consumer sees
    // `self_written` rather than `SelfWritten`.
    let s = format!("{v:?}");
    let mut out = String::with_capacity(s.len() + 4);
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push('_');
        }
        out.extend(c.to_lowercase());
    }
    out
}

fn relative(path: &str, roots: &[String]) -> String {
    roots
        .iter()
        .filter(|r| path.starts_with(r.as_str()))
        .max_by_key(|r| r.len())
        .and_then(|r| path.strip_prefix(r.as_str()))
        .map(|s| s.trim_start_matches('/').to_string())
        .unwrap_or_else(|| path.to_string())
}

fn location(path: &str, span: &marrow_core::SourceSpan) -> String {
    match span {
        marrow_core::SourceSpan::Lines { start, .. } => format!("{path}:{start}"),
        marrow_core::SourceSpan::Page { page, .. } => format!("{path}:p{page}"),
        marrow_core::SourceSpan::Cells { sheet, range } => format!("{path}:{sheet}!{range}"),
        _ => path.to_string(),
    }
}

/// Run the stdio loop until stdin closes.
///
/// **stdout is protocol traffic only.** Narration goes to stderr; a stray write
/// here corrupts the stream in a way that looks like a client bug.
pub fn serve(server: &Server, input: impl BufRead, mut output: impl Write) -> std::io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(req) => server.handle(&req),
            Err(e) => {
                warn!(error = %e, "unparseable request");
                Some(err(
                    Value::Null,
                    RpcError::ParseError,
                    "Request was not valid JSON-RPC.",
                ))
            }
        };
        if let Some(r) = response {
            writeln!(output, "{r}")?;
            output.flush()?;
        }
    }
    Ok(())
}

/// Only ever `Resident` files are read. Kept as a named check so the invariant
/// is greppable rather than an inline string comparison.
#[allow(dead_code)]
fn readable(tier: TierState) -> bool {
    tier.safe_to_read()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_names_become_snake_case_on_the_wire() {
        assert_eq!(lower(&marrow_core::Origin::SelfWritten), "self_written");
        assert_eq!(lower(&marrow_core::Origin::User), "user");
        assert_eq!(
            lower(&marrow_core::ProvenanceClass::MetadataOnly),
            "metadata_only"
        );
    }

    #[test]
    fn a_line_span_becomes_an_openable_location() {
        assert_eq!(
            location(
                "a/b.rs",
                &marrow_core::SourceSpan::Lines { start: 12, end: 14 }
            ),
            "a/b.rs:12"
        );
    }

    #[test]
    fn the_longest_matching_root_wins() {
        let roots = vec!["/a".to_string(), "/a/b".to_string()];
        assert_eq!(relative("/a/b/c.rs", &roots), "c.rs");
    }

    #[test]
    fn only_resident_files_are_readable() {
        assert!(readable(TierState::Resident));
        for t in [
            TierState::Placeholder,
            TierState::Hydrating,
            TierState::Unavailable,
        ] {
            assert!(!readable(t));
        }
    }
}
