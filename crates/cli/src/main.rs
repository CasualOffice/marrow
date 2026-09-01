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
mod filters;
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
///
/// [UX §8]: ../../../docs/UX.md
pub(crate) const EXIT_INTERRUPTED: i32 = 5;

mod exit {
    pub const OK: i32 = 0;
    pub const USAGE: i32 = 1;
    pub const NOT_FOUND: i32 = 2;
    pub const INDEX_UNAVAILABLE: i32 = 4;
    /// The user pressed Ctrl-C. 5 because that is the number UX §8's table
    /// gives it, and a documented exit code is a contract with whatever script
    /// is reading it.
    pub const INTERRUPTED: i32 = super::EXIT_INTERRUPTED;
    /// The network could not be reached, or refused. Beyond the §8 table:
    /// reaching outward failed, and a script that retries on a missing index
    /// should not retry on this.
    pub const NETWORK_UNAVAILABLE: i32 = 6;
    /// The model could not run: not installed, suspended, or out of memory.
    /// Distinct from `INDEX_UNAVAILABLE` because search still works — a script
    /// that falls back to `marrow search` needs to tell the two apart.
    ///
    /// **7, not 5.** It shared 5 with `INTERRUPTED`, which made the one
    /// distinction it exists for impossible: a fallback script could not tell a
    /// machine with no model from a user who had just pressed Ctrl-C, and would
    /// have retried the run they stopped. This is the code that moved because
    /// it is the one §8 never assigned.
    pub const MODEL_UNAVAILABLE: i32 = 7;
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
        /// Only files whose path contains this
        ///
        /// A substring of the whole path, case-blind: `--path crates/model`,
        /// `--path .rs`. The counterpart of MCP's `path_contains`, which the
        /// incomplete-scan advice used to name on a CLI that had no such flag.
        #[arg(long, value_name = "SUBSTRING")]
        path: Option<String>,
        /// Only this workspace
        ///
        /// A name from `marrow workspace list`. A name that matches nothing is
        /// an error rather than an empty result — zero hits for a typo reads as
        /// "nothing is indexed" and sends you to debug the wrong thing.
        #[arg(long, value_name = "NAME")]
        workspace: Option<String>,
        /// Only files with this extension
        ///
        /// With or without the dot: `--type md`, `--type .html`. One
        /// extension; the index's filter takes a list, but no query yet has
        /// wanted two and a flag nobody uses is a flag nobody maintains.
        #[arg(long = "type", value_name = "EXT", conflicts_with = "literal")]
        file_type: Option<String>,
        /// Only files modified on or after this
        ///
        /// A calendar date, `2026-01-31`, or a span counted back from now:
        /// `7d`, `3w`, `6m`, `2y`. Dates are read as UTC, which is what the
        /// index stores.
        #[arg(long, value_name = "WHEN", conflicts_with = "literal")]
        since: Option<String>,
        /// Only files modified on or before this
        ///
        /// Same spellings as `--since`. A bare date covers the whole of that
        /// day, so `--until 2026-01-31` includes a file saved that evening.
        #[arg(long, value_name = "WHEN", conflicts_with = "literal")]
        until: Option<String>,
        /// Seconds to scan before reporting a partial result; 0 for no limit
        ///
        /// A literal scan reads files, so a large corpus does not finish
        /// quickly. When the limit runs out the result says so and says how
        /// much of the scope it never reached — it is never presented as
        /// "not found".
        #[arg(long, value_name = "SECONDS", requires = "literal")]
        time_limit: Option<u64>,
        /// Show why each result ranked where it did, instead of the results
        ///
        /// Prints the branches that ran, their weights, each hit's rank inside
        /// each branch, the multipliers applied, and what the explanation
        /// cannot tell you.
        #[arg(long, conflicts_with = "literal")]
        explain: bool,
        /// Also search by meaning, not only by words
        ///
        /// Off by default because it starts an embedding model, which costs
        /// several seconds on every invocation, and a search you type is a
        /// search you expect to answer immediately. When words are the wrong
        /// tool — you remember what a document *said* but not what it called
        /// it — this is what finds it.
        #[arg(long, conflicts_with = "literal")]
        semantic: bool,
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
    /// Add up a range of spreadsheet cells
    ///
    /// Computed here, not read out by a model. A model handed forty numbers as
    /// text will usually add them correctly and cannot tell you when it did
    /// not; this uses the typed values the parser recorded and says which
    /// cells it skipped and why.
    ///
    ///   marrow table sum ~/q2.xlsx 'Q2!B4:B18'
    Table {
        /// sum · mean · min · max · count
        op: String,
        /// The spreadsheet. Must already be indexed.
        path: String,
        /// The range, as the spreadsheet writes it: `B4:B18`, or `Q2!B4:B18`
        /// to name the sheet. Required when the workbook has more than one.
        range: String,
        /// Only rows where a column matches: `--where 'A=Rent'`.
        ///
        /// The column is a letter and the match is on the cell's text,
        /// trimmed and ignoring case — a filter is a question about what the
        /// sheet says, so `$1,200` is matched by what you can read in it.
        #[arg(long = "where", value_name = "COL=VALUE")]
        filter: Option<String>,
        /// One result per distinct value of a column: `--by A`.
        ///
        /// "What did each category come to" is the question a single total
        /// cannot answer. Blank cells are not a group.
        #[arg(long = "by", value_name = "COL", conflicts_with = "filter")]
        by: Option<String>,
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
            path,
            workspace,
            file_type,
            since,
            until,
            time_limit,
            semantic,
            explain,
        } => {
            let q = query.join(" ");
            if q.trim().is_empty() {
                return Err(Error::new(
                    marrow_core::Code::CfgInvalid,
                    "Nothing to search for. Pass a query: marrow search \"auth refresh token\"",
                ));
            }
            let store = open_store()?;
            if *literal {
                let cancel = waiting::install_interrupt_handler();
                let req = literal::Request {
                    pattern: &q,
                    regex: *regex,
                    ignore_case: *ignore_case,
                    whole_word: *whole_word,
                    limit: *limit,
                    workspace: workspace.as_deref(),
                    path_contains: path.as_deref(),
                    time_limit: time_limit.map(std::time::Duration::from_secs),
                };
                // The literal scan establishes its own scope by walking the
                // granted folders, so it does not want the index's idea of the
                // roots — it wants the folders themselves.
                return literal::run(&store, &req, cli.json, style, out, cancel.as_flag());
            }
            // Resolved before anything is opened or loaded, so a mistyped date
            // costs nothing and a mistyped workspace name is caught by the
            // search that follows rather than answered with zero results.
            let filters = filters::resolve(
                filters::Args {
                    extension: file_type.as_deref(),
                    path: path.as_deref(),
                    workspace: workspace.as_deref(),
                    since: since.as_deref(),
                    until: until.as_deref(),
                },
                Timestamp::now(),
            )?;
            let conn = store.reader()?;
            let roots: Vec<String> = list_workspaces(&conn)?
                .into_iter()
                .map(|(_, p, _)| p)
                .collect();
            drop(conn);
            let index = marrow_index::Fts5Index::open(&store)?;
            // **Opt-in, because it costs seconds.** Starting the embedder took
            // a 40 ms search to 4.7 s on this machine, and a search you type is
            // one you expect to answer before you have finished reading the
            // prompt. Measured against what it buys: 239 of 79,186 files have
            // vectors, so on this corpus the default would be paying five
            // seconds for a branch that can speak about 0.3% of the index.
            //
            // Absent regardless unless there is an embedding model, an MLX
            // runtime and vectors from a backfill — hard rule 10 says search
            // answers with none of those, so it stays silent when it cannot
            // happen rather than failing.
            let semantic = if *semantic {
                search::semantic_branch(&store, &data_dir()?, &q)
            } else {
                None
            };
            search::run(
                &store,
                &index,
                semantic.as_ref(),
                &search::Request {
                    query: &q,
                    limit: *limit,
                    filters: &filters,
                    roots: &roots,
                    json: cli.json,
                    explain: *explain,
                },
                style,
                out,
            )
        }
        Cmd::Embed => {
            let store = open_store()?;
            embed::run(&store, &data_dir()?, cli.json, style, out)
        }
        Cmd::Status => status(cli.json, style, out),
        Cmd::Table {
            op,
            path,
            range,
            filter,
            by,
        } => table(
            op,
            path,
            range,
            filter.as_deref(),
            by.as_deref(),
            cli.json,
            style,
            out,
        ),
        Cmd::Watch { name } => watch(name.as_deref(), cli.json, style, out),
        Cmd::Mcp => {
            let store = open_store()?;
            // The data directory is where the network allowlist lives. Without
            // it `fetch_url` can never confirm a host, which is how it came to
            // refuse every URL forever while advertising a confirmation step.
            let server = marrow_mcp::Server::new(store)?.with_data_dir(data_dir()?);
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

/// `marrow table sum <path> '<range>'` — arithmetic, not a reading of it.
///
/// **The whole value is that no model is involved.** "Deterministic before
/// probabilistic" is the first line of the design and a sum is its clearest
/// case: a model given forty numbers as text will usually add them correctly
/// and has no way to tell you when it did not.
///
/// So the output leads with the number and then says what it is a number *of*.
/// A total over eighteen cells where three held text is not a total over
/// eighteen cells, and this repository keeps finding the same defect — a
/// figure accurate about what the code did, shown where a reader takes it as a
/// fact about their data.
#[allow(clippy::too_many_arguments)]
fn table(
    op: &str,
    path: &str,
    range: &str,
    filter: Option<&str>,
    by: Option<&str>,
    json: bool,
    style: Style,
    out: &mut impl Write,
) -> Result<()> {
    let op = marrow_query::table::Op::parse(op).ok_or_else(|| {
        Error::new(
            marrow_core::Code::CfgInvalid,
            format!("`{op}` is not something this can compute. Use sum, mean, min, max or count."),
        )
    })?;

    let store = open_store()?;
    let conn = store.reader()?;

    // Indexed files only, and by the same rule every other read obeys: this is
    // not a general spreadsheet tool, and a path that is not in the index is a
    // file nobody granted.
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    let version: Option<String> = conn
        .query_row(
            "SELECT f.current_version_id FROM files f
              WHERE f.current_path = ?1 AND f.status = 'ACTIVE' LIMIT 1",
            [canonical.to_string_lossy().as_ref()],
            |r| r.get(0),
        )
        .ok();
    let Some(version) = version.and_then(|v| v.parse().ok()) else {
        return Err(Error::new(
            marrow_core::Code::FsNotFound,
            format!(
                "`{path}` is not indexed, so there are no cells to add up.                  Run `marrow index` first, or check the path."
            ),
        ));
    };

    // Parsed before the store is touched: `--where B` with no `=` is a typo,
    // and finding that out after a scan of the sheet wastes the user's time
    // and tells them nothing they could not have been told at once.
    let filter = match filter {
        Some(raw) => Some(marrow_query::table::Where::parse(raw).ok_or_else(|| {
            Error::new(
                marrow_core::Code::CfgInvalid,
                format!(
                    "`{raw}` is not a filter. Write it as a column letter, `=`, and the \
                     text to match — for example `A=Rent`."
                ),
            )
        })?),
        None => None,
    };

    if let Some(by) = by {
        // Same column arithmetic as a filter's, so `--by A` and `--where A=…`
        // cannot disagree about which column `A` is.
        let column = marrow_core::a1::parse_cell_ref(&format!("{}1", by.trim()))
            .map(|(_, c)| c)
            .ok_or_else(|| {
                Error::new(
                    marrow_core::Code::CfgInvalid,
                    format!("`{by}` is not a column. Use a letter, like `--by A`."),
                )
            })?;
        let groups = marrow_query::table::compute_by(&conn, version, op, range, column)?;
        return render_groups(&groups, op, range, column, json, style, out);
    }

    let computed = marrow_query::table::compute(&conn, version, op, range, filter.as_ref())?;

    if json {
        writeln!(
            out,
            "{}",
            serde_json::json!({
                "op": computed.op.as_str(),
                "value": computed.value,
                "sheet": computed.sheet,
                "range": computed.range,
                "contributing_cells": computed.contributing,
                "filtered_by": computed.filtered_by.as_ref().map(|w| serde_json::json!({
                    "column": marrow_core::a1::column_name(w.column),
                    "equals": w.equals,
                })),
                "skipped": computed.skipped.iter().map(|s| serde_json::json!({
                    "cell": s.reference,
                    "text": s.raw_text,
                    "reason": s.reason,
                })).collect::<Vec<_>>(),
            })
        )?;
        return Ok(());
    }

    match computed.value {
        Some(v) => writeln!(out, "{}", style.bold(&fmt_number(v)))?,
        // `min` of nothing has no answer, and printing 0 would be one.
        None => writeln!(out, "{}", style.dim("no cells in that range held a number"))?,
    }
    writeln!(
        out,
        "{}",
        style.dim(&format!(
            "{} over {} cell{} in {}{}",
            computed.op.as_str(),
            computed.contributing,
            if computed.contributing == 1 { "" } else { "s" },
            // A Markdown table has no sheet, and a bare `!` reads as a typo.
            if computed.sheet.is_empty() {
                String::new()
            } else {
                format!("{}!", computed.sheet)
            },
            computed.range,
        ))
    )?;
    // A total of four cells out of a range of forty is a different claim from
    // a total of four cells, and only this line separates them.
    if let Some(w) = &computed.filtered_by {
        writeln!(
            out,
            "{}",
            style.dim(&format!(
                "  where {} is {}",
                marrow_core::a1::column_name(w.column),
                w.equals
            ))
        )?;
    }

    // Never a bare count. "3 skipped" sends a reader looking; naming them is
    // the difference between a caveat and an errand.
    if computed.is_partial() {
        writeln!(out)?;
        writeln!(
            out,
            "{}",
            style.warn(&format!(
                "{} cell{} in that range did not count:",
                computed.skipped.len(),
                if computed.skipped.len() == 1 { "" } else { "s" },
            ))
        )?;
        for s in computed.skipped.iter().take(10) {
            // Collapsed and clipped. A cell holding a paragraph — or a
            // newline, which spreadsheets allow — otherwise breaks one row of
            // this list across four lines and the list stops being scannable.
            let shown = if s.raw_text.trim().is_empty() {
                String::new()
            } else {
                let flat = s.raw_text.split_whitespace().collect::<Vec<_>>().join(" ");
                let clipped = if flat.chars().count() > 60 {
                    format!("{}…", flat.chars().take(59).collect::<String>())
                } else {
                    flat
                };
                format!(" — {clipped}")
            };
            writeln!(out, "  {}  {}{}", s.reference, s.reason, shown)?;
        }
        if computed.skipped.len() > 10 {
            writeln!(
                out,
                "  {}",
                style.dim(&format!("… and {} more", computed.skipped.len() - 10))
            )?;
        }
    }
    Ok(())
}

/// One row per group, widest key first so the numbers line up.
///
/// Each group carries its own skipped cells. They are summarised rather than
/// listed here — twelve groups of three would bury the totals the breakdown was
/// asked for — but the count is always shown, because a group whose total came
/// from two of its five rows is not a total of that group.
fn render_groups(
    groups: &[marrow_query::table::Group],
    op: marrow_query::table::Op,
    range: &str,
    column: u32,
    json: bool,
    style: Style,
    out: &mut impl Write,
) -> Result<()> {
    if json {
        writeln!(
            out,
            "{}",
            serde_json::json!({
                "op": op.as_str(),
                "range": range,
                "grouped_by": marrow_core::a1::column_name(column),
                "groups": groups.iter().map(|g| serde_json::json!({
                    "key": g.key,
                    "value": g.computed.value,
                    "contributing_cells": g.computed.contributing,
                    "skipped_cells": g.computed.skipped.len(),
                })).collect::<Vec<_>>(),
            })
        )?;
        return Ok(());
    }

    let width = groups
        .iter()
        .map(|g| g.key.chars().count())
        .max()
        .unwrap_or(0);
    for g in groups {
        let value = match g.computed.value {
            Some(v) => fmt_number(v),
            None => "—".into(),
        };
        write!(out, "  {:<width$}  {}", g.key, style.bold(&value))?;
        if g.computed.is_partial() {
            write!(
                out,
                "{}",
                style.dim(&format!(
                    "   ({} cell{} did not count)",
                    g.computed.skipped.len(),
                    if g.computed.skipped.len() == 1 {
                        ""
                    } else {
                        "s"
                    },
                ))
            )?;
        }
        writeln!(out)?;
    }
    writeln!(
        out,
        "{}",
        style.dim(&format!(
            "{} of {}, grouped by {}",
            op.as_str(),
            range,
            marrow_core::a1::column_name(column)
        ))
    )?;
    Ok(())
}

/// A number a person reads, not a float's debug form.
fn fmt_number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v:.2}")
    }
}

fn status(json: bool, style: Style, out: &mut impl Write) -> Result<()> {
    let store = open_store()?;
    let conn = store.reader()?;
    let rows = list_workspaces(&conn)?;

    // The same read model MCP and the desktop report from, rather than a
    // fourth hand-rolled count query. It is the only one of the four that
    // carries freshness, and the counts are worth nothing without it.
    let stats = marrow_query::catalog::index_stats(&conn)?;
    let files = stats.files;
    let bytes = stats.content_bytes;
    let placeholders = stats.cloud_only;

    if json {
        writeln!(
            out,
            "{}",
            serde_json::json!({
                "schema": "marrow.status/1",
                "workspaces": stats.workspaces,
                "files": files,
                "content_bytes": bytes,
                "cloud_only": placeholders,
                "schema_version": stats.schema_version,
                // Counts without freshness are how a stale index answers
                // confidently: `35,134 files` reads as the disk now, and a
                // script has no way to see that nothing has looked at it since
                // this morning. Same three fields as MCP's `index_status`.
                "last_indexed_ms": stats.last_reconciled_ms,
                "watcher": stats.watcher_health,
                "may_be_stale": stats.may_be_stale(),
                "freshness": freshness(&stats),
                "embedded_chunks": stats.embedded_chunks,
                "semantic_coverage_pct": stats.semantic_coverage(),
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
            stats.workspaces,
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
    // Warned rather than dimmed when it may be stale: the counts above are the
    // part people read, and a sentence saying they might describe a disk that
    // has moved on has to compete with them.
    let line = freshness(&stats);
    writeln!(
        out,
        "  {}",
        if stats.may_be_stale() {
            style.warn(&line)
        } else {
            style.dim(&line)
        }
    )?;

    // **What `--semantic` can actually speak for.** Only chunks with a vector
    // are visible to that branch, and a partial backfill looks exactly like a
    // complete one from the outside — the answers are simply drawn from a
    // fraction of the corpus, with nothing saying which fraction. Silent when
    // nothing is embedded at all, because "0%" and "you are not using this"
    // are different things to say and only one needs saying.
    if let Some(pct) = stats.semantic_coverage() {
        writeln!(
            out,
            "  {}",
            style.dim(&format!(
                "`--semantic` covers {pct:.0}% of the index ({} of {} chunks). \
                 `marrow embed` finishes the rest.",
                render::count(stats.embedded_chunks as u64),
                render::count(stats.chunks as u64)
            ))
        )?;
    }
    Ok(())
}

/// One sentence about whether these counts can be trusted as current.
///
/// Three states, not two. "Never scanned" and "scanned an hour ago but nothing
/// is watching" both mean the index may lag the disk, but they call for
/// different actions, and collapsing them would tell the user to re-run a scan
/// that just ran. MCP says the same three things in its own vocabulary; here
/// the action is a command they can paste.
fn freshness(st: &marrow_query::catalog::IndexStats) -> String {
    let Some(at) = st.last_reconciled_ms else {
        return "These folders have never been scanned, so nothing here reflects what is \
                on the disk. Run `marrow index` before relying on a result."
            .to_string();
    };
    if !st.may_be_stale() {
        return "A watcher is running, so the index follows the disk as it changes.".to_string();
    }
    let ago = Timestamp::now().as_millis().saturating_sub(at);
    let when = match ago / 1000 {
        s if s < 90 => "less than two minutes ago".to_string(),
        s if s < 5_400 => format!("{} minutes ago", s / 60),
        s if s < 172_800 => format!("{} hours ago", s / 3600),
        s => format!("{} days ago", s / 86_400),
    };
    format!(
        "Last scanned {when}, and nothing is watching now — anything added, changed or \
         deleted since then is not in the index, and a search cannot mention what it does \
         not know about. Run `marrow index` to catch up, or `marrow watch` to follow \
         changes as they happen."
    )
}

fn watch(only: Option<&str>, json: bool, style: Style, out: &mut impl Write) -> Result<()> {
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
    // Not in JSON: stdout is one object per event there, and a line of prose
    // in the middle of it is what breaks the consumer this flag exists for.
    if !json {
        writeln!(out, "{}", style.dim("Ctrl-C to stop"))?;
    }
    watching::run(&store, targets, &cancel, json, style, out, &data_dir()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use marrow_query::catalog::IndexStats;

    #[test]
    fn every_exit_code_means_one_thing() {
        // A script that falls back to `marrow search` when the model is gone
        // reads this number; two meanings on one number make that impossible.
        let codes = [
            exit::OK,
            exit::USAGE,
            exit::NOT_FOUND,
            exit::INDEX_UNAVAILABLE,
            exit::MODEL_UNAVAILABLE,
            exit::NETWORK_UNAVAILABLE,
            exit::INTERRUPTED,
            exit::INTERNAL,
        ];
        for (i, a) in codes.iter().enumerate() {
            for b in &codes[i + 1..] {
                assert_ne!(a, b, "two exit codes share the value {a}");
            }
        }
        // The documented ones keep the numbers UX §8 gave them; the model
        // code, which that table never assigned, is the one that moved.
        assert_eq!(exit::INTERRUPTED, 5);
        assert_eq!(exit::MODEL_UNAVAILABLE, 7);
    }

    #[test]
    fn an_index_nobody_has_scanned_says_so_rather_than_reporting_counts_alone() {
        let st = IndexStats {
            files: 35_134,
            last_reconciled_ms: None,
            watcher_health: "unavailable".into(),
            ..IndexStats::default()
        };
        assert!(st.may_be_stale());
        let line = freshness(&st);
        assert!(line.contains("never been scanned"), "{line}");
        assert!(line.contains("marrow index"), "{line}");
    }

    #[test]
    fn a_watched_index_is_not_reported_as_stale() {
        let st = IndexStats {
            last_reconciled_ms: Some(Timestamp::now().as_millis()),
            watcher_health: "live".into(),
            ..IndexStats::default()
        };
        assert!(!st.may_be_stale());
        assert!(freshness(&st).contains("watcher is running"));
    }

    #[test]
    fn an_unwatched_index_says_how_old_it_is_and_what_to_run() {
        // The state the CLI leaves behind: `marrow index` marks the root
        // UNAVAILABLE because a one-shot run leaves nothing watching.
        let three_hours = 3 * 3_600_000;
        let st = IndexStats {
            last_reconciled_ms: Some(Timestamp::now().as_millis() - three_hours),
            watcher_health: "unavailable".into(),
            ..IndexStats::default()
        };
        let line = freshness(&st);
        assert!(line.contains("3 hours ago"), "{line}");
        assert!(line.contains("marrow index"), "{line}");
        assert!(line.contains("marrow watch"), "{line}");
    }
    /// The incomplete-scan advice names flags. This is the test that keeps
    /// those names true: F5 was, in part, a message that told the user to
    /// "narrow it with a workspace or a path" on a command line that parsed
    /// neither. An action the user cannot take is not an action.
    #[test]
    fn literal_flags_named_in_the_advice_are_flags_the_parser_accepts() {
        for args in [
            vec!["marrow", "search", "--literal", "x", "--time-limit", "90"],
            vec!["marrow", "search", "--literal", "x", "--time-limit", "0"],
            vec![
                "marrow",
                "search",
                "--literal",
                "x",
                "--path",
                "crates/model",
            ],
            vec!["marrow", "search", "--literal", "x", "--workspace", "melp"],
            vec!["marrow", "search", "--literal", "x", "-n", "5"],
        ] {
            let cli = Cli::try_parse_from(&args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
            let Cmd::Search { literal, .. } = cli.cmd else {
                panic!("{args:?} did not parse as a search")
            };
            assert!(literal, "{args:?}");
        }
    }

    /// Flags that only mean something to the literal scan have to be refused
    /// rather than accepted and ignored — "a parameter declared but ignored is
    /// the worst kind of bug".
    ///
    /// `--path` and `--workspace` used to be on this list and are not any more:
    /// they narrow the indexed search too now, which is what the tracker's
    /// "the port supports them, the CLI does not expose them" was about.
    #[test]
    fn the_literal_only_flags_are_refused_without_literal() {
        for flag in [
            vec!["--time-limit", "10"],
            vec!["--regex"],
            vec!["--ignore-case"],
            vec!["--whole-word"],
        ] {
            let mut args = vec!["marrow", "search", "x"];
            args.extend(flag.iter().copied());
            assert!(
                Cli::try_parse_from(&args).is_err(),
                "{args:?} should not parse without --literal"
            );
        }
    }

    /// And the mirror of it. The literal scan reads the filesystem and knows
    /// nothing about extensions or the index's mtimes, so accepting these
    /// beside `--literal` would silently answer a narrower question than the
    /// one that was asked.
    #[test]
    fn the_index_only_filters_are_refused_with_literal() {
        for flag in [
            vec!["--type", "html"],
            vec!["--since", "7d"],
            vec!["--until", "2026-01-01"],
        ] {
            let mut args = vec!["marrow", "search", "--literal", "x"];
            args.extend(flag.iter().copied());
            assert!(
                Cli::try_parse_from(&args).is_err(),
                "{args:?} should not parse with --literal"
            );
        }
    }

    /// The filters the tracker named, on the surface that lacked them.
    #[test]
    fn the_indexed_search_accepts_every_filter_the_port_supports() {
        let cli = Cli::try_parse_from([
            "marrow",
            "search",
            "lease",
            "--type",
            "html",
            "--path",
            "docs",
            "--workspace",
            "system",
            "--since",
            "7d",
            "--until",
            "2026-01-31",
            "--semantic",
            "--explain",
        ])
        .expect("every filter parses together, and beside --semantic and --explain");
        let Cmd::Search {
            file_type,
            path,
            workspace,
            since,
            until,
            semantic,
            explain,
            ..
        } = cli.cmd
        else {
            panic!("did not parse as a search")
        };
        assert_eq!(file_type.as_deref(), Some("html"));
        assert_eq!(path.as_deref(), Some("docs"));
        assert_eq!(workspace.as_deref(), Some("system"));
        assert_eq!(since.as_deref(), Some("7d"));
        assert_eq!(until.as_deref(), Some("2026-01-31"));
        assert!(semantic && explain);
    }
}
