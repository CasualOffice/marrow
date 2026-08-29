//! `marrow` — the command line.
//!
//! Contains no logic ([LLD §7]): it parses arguments, calls into the crates
//! that do the work, and renders. Anything it computes here is something MCP
//! and the desktop app cannot reach, which would put the boundary in the wrong
//! place.
//!
//! [LLD §7]: ../../../docs/LLD.md

#![forbid(unsafe_code)]

mod render;
mod search;

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use marrow_core::{Error, Result, Timestamp};
use marrow_ingest::{Cancel, IngestPolicy, Progress, Stage};
use marrow_scan::{AuthorizedRoot, WalkPolicy};
use marrow_store::read::{NewRoot, NewWorkspace, StorageKind};
use marrow_store::Store;
use render::Style;

/// Exit codes ([UX §8]). Zero results is success, not an error.
mod exit {
    pub const OK: i32 = 0;
    pub const USAGE: i32 = 1;
    pub const NOT_FOUND: i32 = 2;
    pub const INDEX_UNAVAILABLE: i32 = 4;
    pub const INTERRUPTED: i32 = 5;
    pub const INTERNAL: i32 = 70;
}

#[derive(Parser, Debug)]
#[command(
    name = "marrow",
    about = "A local knowledge runtime",
    version,
    disable_help_subcommand = true
)]
struct Cli {
    /// Machine-readable output. Same data as the human view.
    #[arg(long, global = true)]
    json: bool,
    /// Never emit colour. `NO_COLOR` is honoured too.
    #[arg(long, global = true)]
    no_color: bool,
    /// Narration on stderr. Repeat for more.
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Manage the folders Marrow may read
    #[command(subcommand)]
    Workspace(WorkspaceCmd),
    /// Scan a workspace and record what changed
    Index {
        /// Workspace name. Omit to index every workspace.
        name: Option<String>,
        /// Honour .gitignore in this run (D47: per-root, off by default)
        #[arg(long)]
        gitignore: bool,
    },
    /// Find things
    Search {
        /// What to look for
        query: Vec<String>,
        /// Maximum results
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
    },
    /// Index health
    Status,
    /// Watch workspaces and index changes as they happen
    ///
    /// Runs until interrupted. Watcher health is reported on start and whenever
    /// it changes — a degraded watcher is never silent (WATCH-009).
    Watch {
        /// Workspace name. Omit to watch every workspace.
        name: Option<String>,
    },
    /// Serve the index over MCP on stdio
    ///
    /// Point an agent front-end at this. Protocol traffic uses stdout, so
    /// nothing else may write there.
    Mcp,
}

#[derive(Subcommand, Debug)]
enum WorkspaceCmd {
    /// Grant Marrow a folder
    Add {
        path: PathBuf,
        /// Name for it. Defaults to the folder name.
        #[arg(long)]
        name: Option<String>,
    },
    /// List granted folders
    List,
}

fn main() {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    let style = if cli.json {
        Style::plain()
    } else {
        Style::detect(cli.no_color)
    };
    let code = match run(&cli, style) {
        Ok(()) => exit::OK,
        Err(e) => {
            let mut err = std::io::stderr();
            let _ = render::error(&e, &mut err, style);
            exit_code_for(&e)
        }
    };
    std::process::exit(code);
}

fn exit_code_for(e: &Error) -> i32 {
    use marrow_core::Class::*;
    match e.code().class() {
        Config => exit::USAGE,
        Filesystem => exit::NOT_FOUND,
        Storage | Index => exit::INDEX_UNAVAILABLE,
        Policy => 3,
        Internal => exit::INTERNAL,
        Parse => exit::OK,
    }
}

fn init_tracing(verbosity: u8) {
    // Narration goes to stderr so stdout stays a clean pipe.
    let level = match verbosity {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level)),
        )
        .with_target(false)
        .try_init();
}

/// Where state lives. Per-user, never machine-wide (MULTI-002).
fn data_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| {
        Error::new(
            marrow_core::Code::CfgInvalid,
            "HOME is not set, so Marrow cannot find its data directory. \
             Set HOME, or pass MARROW_DATA_DIR.",
        )
    })?;
    let dir = std::env::var_os("MARROW_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(home).join(".local/share/marrow"));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// The composition root.
///
/// This is the only place that knows every adapter in play, so it is where the
/// migration chain is assembled. `marrow-index` adds FTS5 tables to the same
/// database (D3), but it depends on `marrow-store` — so store cannot reference
/// it back without a cycle. The binary composes them instead, which keeps store
/// unaware of index and index a swappable implementation of a port.
fn open_store() -> Result<Store> {
    Store::open_with_migrations(
        data_dir()?.join(marrow_store::DB_FILE_NAME),
        &[marrow_index::fts5::MIGRATION],
    )
}

fn run(cli: &Cli, style: Style) -> Result<()> {
    let out = &mut std::io::stdout();
    match &cli.cmd {
        Cmd::Workspace(WorkspaceCmd::Add { path, name }) => {
            workspace_add(path, name.as_deref(), cli.json, style, out)
        }
        Cmd::Workspace(WorkspaceCmd::List) => workspace_list(cli.json, style, out),
        Cmd::Index { name, gitignore } => index(name.as_deref(), *gitignore, cli.json, style, out),
        Cmd::Search { query, limit } => {
            let q = query.join(" ");
            if q.trim().is_empty() {
                return Err(Error::new(
                    marrow_core::Code::CfgInvalid,
                    "Nothing to search for. Pass a query: marrow search \"auth refresh token\"",
                ));
            }
            let store = open_store()?;
            let conn = store.reader()?;
            let roots: Vec<String> = list_workspaces(&conn)?
                .into_iter()
                .map(|(_, p, _)| p)
                .collect();
            drop(conn);
            let index = marrow_index::Fts5Index::open(&store)?;
            search::run(&index, &q, *limit, &roots, cli.json, style, out)
        }
        Cmd::Status => status(cli.json, style, out),
        Cmd::Watch { name } => watch(name.as_deref(), style, out),
        Cmd::Mcp => {
            let store = open_store()?;
            let server = marrow_mcp::Server::new(store)?;
            let stdin = std::io::stdin().lock();
            let stdout = std::io::stdout().lock();
            marrow_mcp::serve(&server, stdin, stdout)?;
            Ok(())
        }
    }
}

fn workspace_add(
    path: &std::path::Path,
    name: Option<&str>,
    json: bool,
    style: Style,
    out: &mut impl Write,
) -> Result<()> {
    // Canonicalize before storing: an authorized root that is not canonical
    // defeats the containment check that depends on it.
    let root = AuthorizedRoot::open(path)?;
    let canonical = root.path().to_path_buf();
    let name = name
        .map(str::to_string)
        .or_else(|| {
            canonical
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "workspace".to_string());

    let store = open_store()?;
    let now = Timestamp::now();
    let ws = store.upsert_workspace(NewWorkspace {
        workspace_id: marrow_core::WorkspaceId::new(),
        name: name.clone(),
        at: now,
    })?;
    store.upsert_root(NewRoot {
        root_id: marrow_core::RootId::new(),
        workspace_id: ws,
        canonical_path: canonical.to_string_lossy().into_owned(),
        volume_identity: None,
        grant_token: None,
        storage_kind: StorageKind::Local,
        cloud_provider: None,
        at: now,
    })?;
    store.flush()?;

    if json {
        writeln!(
            out,
            "{}",
            serde_json::json!({
                "schema": "marrow.workspace.add/1",
                "workspace": name,
                "path": canonical.to_string_lossy(),
            })
        )?;
    } else {
        writeln!(
            out,
            "{} {}  {}",
            style.ok("added"),
            style.bold(&name),
            style.dim(&canonical.to_string_lossy())
        )?;
        writeln!(out, "  {}", style.dim("marrow index    to scan it"))?;
    }
    Ok(())
}

fn workspace_list(json: bool, style: Style, out: &mut impl Write) -> Result<()> {
    let store = open_store()?;
    let conn = store.reader()?;
    let rows = list_workspaces(&conn)?;

    if json {
        let items: Vec<_> = rows
            .iter()
            .map(|(n, p, files)| serde_json::json!({ "name": n, "path": p, "files": files }))
            .collect();
        writeln!(
            out,
            "{}",
            serde_json::json!({ "schema": "marrow.workspace.list/1", "workspaces": items })
        )?;
        return Ok(());
    }

    if rows.is_empty() {
        writeln!(out, "No workspaces yet.")?;
        writeln!(out, "  {}", style.dim("marrow workspace add ~/some/folder"))?;
        return Ok(());
    }
    for (name, path, files) in &rows {
        writeln!(
            out,
            "{:<16} {:>9}  {}",
            style.bold(name),
            render::count(*files as u64),
            style.dim(&render::elide(path, style.width.saturating_sub(30)))
        )?;
    }
    Ok(())
}

type WorkspaceRow = (String, String, i64);

fn list_workspaces(conn: &marrow_store::ReadConn) -> Result<Vec<WorkspaceRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT w.name,
                    COALESCE(r.canonical_path, ''),
                    (SELECT count(*) FROM files f
                      WHERE f.workspace_id = w.workspace_id AND f.status = 'ACTIVE')
             FROM workspaces w
             LEFT JOIN workspace_roots r ON r.workspace_id = w.workspace_id
             WHERE w.status = 'ACTIVE'
             ORDER BY w.name",
        )
        .map_err(|e| marrow_store::map_sqlite(e, "listing workspaces"))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .and_then(|it| it.collect::<std::result::Result<Vec<_>, _>>())
        .map_err(|e| marrow_store::map_sqlite(e, "listing workspaces"))?;
    Ok(rows)
}

fn index(
    only: Option<&str>,
    gitignore: bool,
    json: bool,
    style: Style,
    out: &mut impl Write,
) -> Result<()> {
    let store = open_store()?;
    let conn = store.reader()?;
    let targets: Vec<_> = list_workspaces(&conn)?
        .into_iter()
        .filter(|(n, _, _)| only.is_none_or(|o| o == n))
        .collect();
    drop(conn);

    if targets.is_empty() {
        return Err(Error::new(
            marrow_core::Code::FsNotFound,
            match only {
                Some(n) => format!(
                    "No workspace named '{n}'. Run `marrow workspace list` to see what exists."
                ),
                None => "No workspaces yet. Run `marrow workspace add <path>` first.".into(),
            },
        ));
    }

    // Ctrl-C sets the flag; every stage checks it at its loop boundary.
    let cancel = Cancel::new();
    {
        let c = cancel.clone();
        let _ = ctrl_c(move || c.cancel());
    }

    let started = std::time::Instant::now();
    let mut totals = marrow_ingest::IngestOutcome::default();

    for (name, path, _) in &targets {
        let conn = store.reader()?;
        let (ws_id, root_id) = ids_for(&conn, name)?;
        drop(conn);

        let root = AuthorizedRoot::open(path)?;
        let policy = IngestPolicy {
            walk: WalkPolicy {
                respect_gitignore: gitignore,
                ..Default::default()
            },
            ..Default::default()
        };
        let progress = Arc::new(Progress::new());
        let text_index = marrow_index::Fts5Index::open(&store)?;
        let outcome = marrow_ingest::ingest_root_with_index(
            &store,
            ws_id,
            root_id,
            &root,
            &policy,
            &progress,
            &cancel,
            Some(&text_index),
        )?;

        if !json {
            writeln!(
                out,
                "{:<16} {:>9} files   {:>7} new   {:>7} unchanged{}",
                style.bold(name),
                render::count(outcome.discovered),
                render::count(outcome.stored),
                render::count(outcome.unchanged),
                if outcome.skipped_placeholder > 0 {
                    style.warn(&format!(
                        "   {} cloud-only, not read",
                        render::count(outcome.skipped_placeholder)
                    ))
                } else {
                    String::new()
                }
            )?;
        }

        totals.discovered += outcome.discovered;
        totals.stored += outcome.stored;
        totals.unchanged += outcome.unchanged;
        totals.skipped_placeholder += outcome.skipped_placeholder;
        totals.failed += outcome.failed;
        totals.parsed += outcome.parsed;
        totals.chunks += outcome.chunks;
        totals.cancelled |= outcome.cancelled;
        let _ = progress.get(Stage::Hashed);
    }

    let elapsed = started.elapsed().as_millis();

    if json {
        writeln!(
            out,
            "{}",
            serde_json::json!({
                "schema": "marrow.index/1",
                "elapsed_ms": elapsed,
                "discovered": totals.discovered,
                "stored": totals.stored,
                "unchanged": totals.unchanged,
                "skipped_placeholder": totals.skipped_placeholder,
                "parsed": totals.parsed,
                "chunks": totals.chunks,
                "failed": totals.failed,
                "cancelled": totals.cancelled,
            })
        )?;
    } else {
        writeln!(
            out,
            "\n{}",
            style.dim(&format!(
                "{} files · {} in · {} parsed · {} chunks · {}{}",
                render::count(totals.discovered),
                render::count(totals.stored),
                render::count(totals.parsed),
                render::count(totals.chunks),
                render::duration(elapsed),
                if totals.failed > 0 {
                    format!(" · {} failed", totals.failed)
                } else {
                    String::new()
                }
            ))
        )?;
    }

    if totals.cancelled {
        std::process::exit(exit::INTERRUPTED);
    }
    Ok(())
}

fn ids_for(
    conn: &marrow_store::ReadConn,
    name: &str,
) -> Result<(marrow_core::WorkspaceId, marrow_core::RootId)> {
    let (ws, root): (String, String) = conn
        .query_row(
            "SELECT w.workspace_id, r.root_id
               FROM workspaces w JOIN workspace_roots r ON r.workspace_id = w.workspace_id
              WHERE w.name = ?1 LIMIT 1",
            [name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| marrow_store::map_sqlite(e, "resolving workspace"))?;
    Ok((
        ws.parse()
            .map_err(|_| Error::invariant("bad workspace id in database"))?,
        root.parse()
            .map_err(|_| Error::invariant("bad root id in database"))?,
    ))
}

fn status(json: bool, style: Style, out: &mut impl Write) -> Result<()> {
    let store = open_store()?;
    let conn = store.reader()?;
    let rows = list_workspaces(&conn)?;

    let (files, bytes, placeholders): (i64, i64, i64) = conn
        .query_row(
            "SELECT (SELECT count(*) FROM files WHERE status='ACTIVE'),
                    (SELECT COALESCE(sum(size_bytes),0) FROM file_versions WHERE status='CURRENT'),
                    (SELECT count(*) FROM files WHERE tier_state != 'RESIDENT')",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|e| marrow_store::map_sqlite(e, "reading index health"))?;

    if json {
        writeln!(
            out,
            "{}",
            serde_json::json!({
                "schema": "marrow.status/1",
                "workspaces": rows.len(),
                "files": files,
                "content_bytes": bytes,
                "cloud_only": placeholders,
                "schema_version": store.schema_version(),
            })
        )?;
        return Ok(());
    }

    if rows.is_empty() {
        writeln!(out, "No workspaces yet.")?;
        writeln!(out, "  {}", style.dim("marrow workspace add ~/some/folder"))?;
        return Ok(());
    }

    for (name, path, n) in &rows {
        writeln!(
            out,
            "{:<16} {}",
            style.bold(name),
            style.dim(&render::elide(path, style.width.saturating_sub(20)))
        )?;
        writeln!(out, "  {} files", render::count(*n as u64))?;
    }
    writeln!(out)?;
    writeln!(
        out,
        "  {}",
        style.dim(&format!(
            "{} workspaces · {} files · {}",
            rows.len(),
            render::count(files as u64),
            render::bytes(bytes as u64)
        ))
    )?;
    // TIER-008: a silent zero here is indistinguishable from "no cloud files",
    // which is the whole failure this count exists to prevent.
    if placeholders > 0 {
        writeln!(
            out,
            "  {}",
            style.warn(&format!(
                "{} files are cloud-only and were not read",
                render::count(placeholders as u64)
            ))
        )?;
    }
    Ok(())
}

fn watch(only: Option<&str>, style: Style, out: &mut impl Write) -> Result<()> {
    let store = open_store()?;
    let conn = store.reader()?;
    let targets: Vec<_> = list_workspaces(&conn)?
        .into_iter()
        .filter(|(n, _, _)| only.is_none_or(|o| o == n))
        .collect();
    drop(conn);

    let Some((name, path, _)) = targets.into_iter().next() else {
        return Err(Error::new(
            marrow_core::Code::FsNotFound,
            "No workspace to watch. Run `marrow workspace add <path>` first.",
        ));
    };

    // One root for now. Watching several needs a thread per watcher and a
    // shared cancel; that is worth doing when a second root exists.
    let root = AuthorizedRoot::open(&path)?;
    let conn = store.reader()?;
    let (ws_id, root_id) = ids_for(&conn, &name)?;
    drop(conn);

    let mut watcher = marrow_scan::Watcher::open(&root)?;
    let index = marrow_index::Fts5Index::open(&store)?;
    let policy = IngestPolicy::default();
    let cancel = Cancel::new();

    writeln!(
        out,
        "{} {}  {}",
        style.bold(&name),
        style.dim(&path),
        match watcher.health() {
            marrow_scan::Health::Live => style.ok("live"),
            h => style.warn(h.label()),
        }
    )?;
    if let Some(reason) = watcher.health().reason() {
        writeln!(out, "  {}", style.warn(reason))?;
    }
    writeln!(
        out,
        "  {}",
        style.dim(&format!(
            "sweeping every {}",
            render::duration(marrow_scan::reconcile_interval(watcher.health()).as_millis())
        ))
    )?;
    writeln!(out, "  {}", style.dim("Ctrl-C to stop"))?;

    let mut last_health = watcher.health().clone();
    while let Some(hints) = watcher.next_batch(std::time::Duration::from_secs(2)) {
        // WATCH-009: a change in health is reported the moment it happens.
        if *watcher.health() != last_health {
            last_health = watcher.health().clone();
            writeln!(
                out,
                "{} {}",
                style.warn("⚠"),
                style.bold(last_health.label())
            )?;
            if let Some(r) = last_health.reason() {
                writeln!(out, "  {}", style.warn(r))?;
            }
        }
        if hints.is_empty() {
            continue;
        }
        if hints.rescan_required {
            writeln!(out, "  {}", style.dim("events were lost — sweeping"))?;
            let progress = Arc::new(Progress::new());
            let o = marrow_ingest::ingest_root_with_index(
                &store,
                ws_id,
                root_id,
                &root,
                &policy,
                &progress,
                &cancel,
                Some(&index),
            )?;
            writeln!(
                out,
                "  {} {} changed",
                style.dim("sweep:"),
                render::count(o.stored)
            )?;
            continue;
        }

        let progress = Arc::new(Progress::new());
        let o = marrow_ingest::apply_hints(
            &store,
            ws_id,
            root_id,
            &root,
            &policy,
            &hints.touched,
            &progress,
            &cancel,
            Some(&index),
        )?;
        if o.stored > 0 {
            writeln!(
                out,
                "  {} {} · {} chunks",
                style.dim(&render::count(o.stored)),
                style.dim("changed"),
                render::count(o.chunks)
            )?;
        }
    }
    Ok(())
}

/// Minimal Ctrl-C handling without pulling in a signal crate: spawn a thread
/// that waits on the default handler being replaced is not possible in std, so
/// we rely on the process default for now and expose the hook for later.
fn ctrl_c(_f: impl FnOnce() + Send + 'static) -> Result<()> {
    // TODO(M2): wire a real SIGINT handler. Until then Ctrl-C terminates the
    // process; the store's WAL and the job queue make that safe to resume, so
    // this is a UX gap rather than a correctness one.
    Ok(())
}
