//! `marrow` — the command line.
//!
//! Contains no logic ([LLD §7]): it parses arguments, calls into the crates
//! that do the work, and renders. Anything it computes here is something MCP
//! and the desktop app cannot reach, which would put the boundary in the wrong
//! place.
//!
//! [LLD §7]: ../../../docs/LLD.md

#![forbid(unsafe_code)]

mod embed;
mod literal;
mod render;
mod search;
mod waiting;
mod watching;

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use marrow_core::{Error, Result, Timestamp};
use marrow_ingest::{IngestPolicy, Progress, Stage};
use marrow_scan::{AuthorizedRoot, WalkPolicy};
use marrow_store::read::{NewRoot, NewWorkspace, StorageKind};
use marrow_store::Store;
use render::Style;

/// Exit codes ([UX §8]). Zero results is success, not an error.
pub(crate) const EXIT_INTERRUPTED: i32 = 5;

mod exit {
    pub const OK: i32 = 0;
    pub const USAGE: i32 = 1;
    pub const NOT_FOUND: i32 = 2;
    pub const INDEX_UNAVAILABLE: i32 = 4;
    /// The model could not run: not installed, suspended, or out of memory.
    /// Distinct from `INDEX_UNAVAILABLE` because search still works — a script
    /// that falls back to `marrow search` needs to tell the two apart.
    pub const MODEL_UNAVAILABLE: i32 = 5;
    /// The network could not be reached, or refused.
    pub const NETWORK_UNAVAILABLE: i32 = 6;
    pub const INTERRUPTED: i32 = super::EXIT_INTERRUPTED;
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
        /// Scan the files themselves instead of the index
        ///
        /// The index tokenizes, so `}});` and `TODO(name)` are unfindable
        /// through it. This reads the files. It is slower, it only sees files
        /// that are on this disk, and it says how many it skipped.
        #[arg(long)]
        literal: bool,
        /// Treat the pattern as a regular expression (with --literal)
        #[arg(long, requires = "literal")]
        regex: bool,
        /// Ignore case (with --literal)
        #[arg(short = 'i', long, requires = "literal")]
        ignore_case: bool,
        /// Match whole words only (with --literal)
        #[arg(short = 'w', long, requires = "literal")]
        whole_word: bool,
    },
    /// Build semantic search over what is already indexed
    ///
    /// Needs the embedding model on disk. Keyword search never does — it works
    /// with no model, no GPU and no network. This adds the meaning-based half
    /// on top, and it is resumable: interrupt it and run it again.
    Embed,
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
        // A refused write is the user's to resolve — re-read the file, pick
        // another name — so it exits like a usage error rather than a fault.
        Action => exit::USAGE,
        Model => exit::MODEL_UNAVAILABLE,
        // Reaching outward failed. Distinct from the index being unavailable:
        // a script that retries on one should not retry on the other.
        Network => exit::NETWORK_UNAVAILABLE,
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
        // A redirected log full of escape codes is worse than no colour.
        // `tracing_subscriber` colours unconditionally unless told otherwise.
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
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
        marrow_index::MIGRATIONS,
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
        Cmd::Search {
            query,
            limit,
            literal,
            regex,
            ignore_case,
            whole_word,
        } => {
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
            if *literal {
                let cancel = waiting::install_interrupt_handler();
                return literal::run(
                    &store,
                    &q,
                    *regex,
                    *ignore_case,
                    *whole_word,
                    *limit,
                    cli.json,
                    style,
                    out,
                    cancel.as_flag(),
                );
            }
            let index = marrow_index::Fts5Index::open(&store)?;
            search::run(&store, &index, &q, *limit, &roots, cli.json, style, out)
        }
        Cmd::Embed => {
            let store = open_store()?;
            embed::run(&store, &data_dir()?, style, out)
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

    // First Ctrl-C asks every stage to stop at its next boundary, which is what
    // leaves the index consistent. A second exits immediately.
    let cancel = waiting::install_interrupt_handler();

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

        // UX §10: nothing is drawn until the work has run past ~500 ms, and it
        // goes to stderr so stdout stays a clean pipe.
        let meter = waiting::Meter::new();
        meter.set_label(name.clone());
        let mut spinner = waiting::Spinner::start(Arc::clone(&meter), "files");
        let pump = {
            let p = Arc::clone(&progress);
            let m = Arc::clone(&meter);
            let c = cancel.clone();
            std::thread::spawn(move || {
                while !c.is_cancelled() {
                    m.set(p.get(Stage::Discovered));
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    if p.get(Stage::Discovered) == u64::MAX {
                        break;
                    }
                }
            })
        };

        let outcome = marrow_ingest::ingest_root_with_index(
            &store,
            ws_id,
            root_id,
            &root,
            &policy,
            &progress,
            &cancel,
            Some(&text_index),
        );
        spinner.finish();
        // The pump is a display detail; it must never hold up the result.
        drop(pump);
        let outcome = outcome?;

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
        totals.merge_failures_from(&outcome);
        // Record that this root now agrees with the disk. `UNAVAILABLE`
        // because a one-shot index leaves nothing watching: the index is
        // current at this instant and will drift from the next change on, and
        // saying so is what stops a reader treating the counts as durable.
        if !outcome.cancelled {
            let _ = store.mark_reconciled(
                root_id,
                marrow_store::read::WatcherHealth::Unavailable,
                marrow_core::Timestamp::now(),
            );
        }
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
                "failures": totals.failures.iter().map(|(code, g)| serde_json::json!({
                    "code": code,
                    "count": g.count,
                    "message": g.message,
                    "examples": g.examples,
                })).collect::<Vec<_>>(),
                "cancelled": totals.cancelled,
            })
        )?;
    } else {
        writeln!(
            out,
            "\n{}",
            style.dim(&format!(
                "{} files · {} in · {} parsed · {} chunks · {}",
                render::count(totals.discovered),
                render::count(totals.stored),
                render::count(totals.parsed),
                render::count(totals.chunks),
                render::duration(elapsed),
            ))
        )?;
        render_failures(&totals, style, out)?;
    }

    if totals.cancelled {
        std::process::exit(exit::INTERRUPTED);
    }
    Ok(())
}

/// Failures, grouped and actionable.
///
/// A dim `· 156 failed` at the end of a summary line is a number people learn
/// to ignore. What makes it actionable is the code, the cause, and a path you
/// can go and look at — so that is what gets rendered, and it gets its own
/// block rather than a suffix.
fn render_failures(
    o: &marrow_ingest::IngestOutcome,
    style: Style,
    out: &mut impl Write,
) -> Result<()> {
    if o.failures.is_empty() {
        return Ok(());
    }
    writeln!(out)?;
    writeln!(
        out,
        "{} {} file{} could not be indexed",
        style.warn("⚠"),
        render::count(o.failed),
        if o.failed == 1 { "" } else { "s" }
    )?;
    for (code, g) in &o.failures {
        writeln!(
            out,
            "  {:>6} × {}",
            render::count(g.count),
            style.bold(code)
        )?;
        writeln!(out, "         {}", style.dim(&g.message))?;
        for ex in g.examples.iter().filter(|e| !e.is_empty()) {
            writeln!(
                out,
                "         {}",
                style.dim(&render::elide(ex, style.width.saturating_sub(10)))
            )?;
        }
    }
    // Every failure isolates to one file (FS-011); saying so prevents the
    // reasonable but wrong conclusion that the whole index is suspect.
    writeln!(
        out,
        "\n  {}",
        style.dim("These files are still findable by name. Only their contents are unindexed.")
    )?;
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
    let rows: Vec<_> = list_workspaces(&conn)?
        .into_iter()
        .filter(|(n, _, _)| only.is_none_or(|o| o == n))
        .collect();

    let mut targets = Vec::with_capacity(rows.len());
    for (name, path, _) in rows {
        let (workspace_id, root_id) = ids_for(&conn, &name)?;
        targets.push(watching::Target {
            name,
            path,
            workspace_id,
            root_id,
        });
    }
    drop(conn);

    if targets.is_empty() {
        return Err(Error::new(
            marrow_core::Code::FsNotFound,
            match only {
                Some(n) => format!("No workspace named '{n}'. Run `marrow workspace list`."),
                None => "No workspace to watch. Run `marrow workspace add <path>` first.".into(),
            },
        ));
    }

    let cancel = waiting::install_interrupt_handler();
    writeln!(out, "{}", style.dim("Ctrl-C to stop"))?;
    watching::run(&store, targets, &cancel, style, out)
}
