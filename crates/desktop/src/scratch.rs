//! The scratch workspace — where files dropped on the window land.
//!
//! **The app could not index a *file*.** It could be given a folder, through a
//! native picker, and nothing else. Someone with a PDF on their desktop and a
//! question about it had to move the file into an indexed folder first, which
//! is the product asking the user to do the filing before it will do the
//! reading.
//!
//! Four decisions shape everything below, and each of them had a plausible
//! alternative that is wrong for a specific, checkable reason.
//!
//! ── 1. Copy, not reference ────────────────────────────────────────────────
//!
//! A dropped file is **copied** into a directory Marrow owns. The alternative —
//! record the file where it lies — fails three ways at once:
//!
//!   * There is nothing to record it *under*. Every containment check in this
//!     system is anchored to an [`AuthorizedRoot`], which is a directory
//!     ([`AuthorizedRoot::open`] refuses a file). Referencing `~/Desktop/x.pdf`
//!     means granting `~/Desktop`, which is a grant the user never made — one
//!     dropped file would hand over the whole folder.
//!   * Nothing would ever correct it. The index is kept true by reconciliation,
//!     and reconciliation walks *roots*. A referenced file in an unwatched,
//!     unwalked directory is never revisited, so when the user moves or deletes
//!     it the row stays `ACTIVE` with a `current_path` pointing at nothing.
//!     Every citation to it then resolves to a file that is not there, and
//!     `read_region` and `open_path` both refuse a file Marrow claims to hold.
//!     Path is never identity — the index is keyed on a file
//!     id with path history precisely so a *move* is survivable, and that
//!     machinery only works for files something is watching.
//!   * With a copy, none of it is a special case. The copy lives under a root
//!     that is granted, canonicalized, watched and swept like any other; it
//!     only disappears when Marrow deletes it, and when it does, the same
//!     reconciliation that retires any deleted file retires it.
//!
//! The cost is honest and stated: it duplicates the bytes. That is what the
//! caps below are for, and why the Settings card shows the size.
//!
//! ── 2. Lifetime: until it is emptied, with a ceiling ──────────────────────
//!
//! Not per session. Conversations persist, and a conversation is the one thing
//! in this database that cannot be re-derived from the user's files — so
//! deleting the evidence a saved answer cites, on quit, would silently rot
//! every thread that was ever answered from a dropped file. "Temporary" here
//! means *the user can throw it away*, not *it throws itself away*.
//!
//! Bounded, because "until emptied" without a ceiling is a directory that grows
//! for ever. [`MAX_TOTAL_BYTES`] caps the whole workspace and
//! [`MAX_FILE_BYTES`] caps one file. When a new drop would exceed the total,
//! the **oldest copies** are evicted first and the report names every one of
//! them — an eviction the user is not told about is the same silent rot in a
//! different costume.
//!
//! ── 3. Dropped files are the user's, not Marrow's ─────────────────────────
//!
//! Nothing here touches `origin`. A copied file is ingested exactly as any
//! other file is, so it is `USER` and citable. `origin = SELF` (the `origin = SELF` rule)
//! is for content *this system generated*, and it bars that content from
//! supporting a claim. Marking a dropped file `SELF` because Marrow was the
//! process that wrote the bytes would get the rule exactly backwards and make
//! every dropped file silently uncitable — the file would be found, and no
//! answer would ever be allowed to use it.
//!
//! ── 4. The path is never one the window chose ─────────────────────────────
//!
//! No function here is reachable from the WebView with a source path in it. A
//! drop arrives as an OS event that Tauri hands to Rust with the paths the
//! window server actually delivered ([`crate::commands::handle_drop`]); a pick
//! arrives from a native panel that also runs in Rust. The window's own copy of
//! those paths (Tauri also forwards `tauri://drag-drop` to the WebView) is used
//! for the hover overlay and is never sent back — there is no command that
//! would accept it.
//!
//! The destination is checked as well as the source. [`destination`] derives
//! the name from the source's own final component, refuses anything that is not
//! a single ordinary component, and proves containment **component-wise** — a
//! string prefix would accept `/data/dropped-evil` under `/data/dropped`. After
//! the copy the result is resolved and re-verified against the root, at
//! operation time, which is what catches a symlink planted between the check
//! and the write (never hydrate a placeholder).

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use marrow_core::{Code, Error, Result, RootId, WorkspaceId};
use marrow_scan::AuthorizedRoot;

use crate::state::Core;

/// The directory Marrow creates inside its own data directory.
///
/// A sibling of `models/` and of the database, never a child of either. Roots
/// may not overlap ([`Core::grant`] refuses it), so this also means granting
/// the data directory itself is refused once scratch exists — which is the
/// correct answer, not an accident.
pub const DIR_NAME: &str = "dropped";

/// What the sidebar calls it. See [`Core::grant_named`] for why the directory
/// name and the display name differ.
pub const WORKSPACE_NAME: &str = "Dropped files";

/// The largest single file that will be copied in.
///
/// Above this the honest move is to refuse and say the size, because the
/// alternative is silently duplicating a gigabyte to answer one question.
/// "Add a folder" indexes a large file where it already lives.
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// The ceiling on the whole scratch workspace. Oldest copies are evicted to
/// make room, and every eviction is reported.
pub const MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;

/// The scratch root, once it exists.
#[derive(Debug, Clone)]
pub struct Handle {
    pub root: AuthorizedRoot,
    pub root_id: RootId,
    pub workspace_id: WorkspaceId,
}

/// One file that did not make it in, and why — in words, with a code the UI can
/// branch on.
///
/// Every refusal is reported per file rather than failing the whole drop: three
/// files and one cloud placeholder should index three files and explain one,
/// not refuse four.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Skipped {
    pub name: String,
    pub code: String,
    pub reason: String,
}

/// What a drop actually did. Nothing about it is silent.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DropReport {
    /// Workspace-relative names now indexed and searchable.
    pub added: Vec<String>,
    /// Already present with identical contents. Not a failure — dedup is a
    /// feature — but the user asked for something and must be told what
    /// happened.
    pub already_there: Vec<String>,
    pub skipped: Vec<Skipped>,
    /// Older copies removed to stay under [`MAX_TOTAL_BYTES`].
    pub evicted: Vec<String>,
    pub bytes_added: u64,
    /// Where they landed, so the notice can name a workspace the user can go
    /// and look at.
    pub workspace: String,
}

impl DropReport {
    /// True when nothing at all happened, which reads differently from a
    /// partial success and must not be reported as one.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.already_there.is_empty() && self.evicted.is_empty()
    }
}

/// What is in the scratch workspace right now, for the card that offers to
/// empty it.
///
/// Counted from the **directory**, not from the index: the question the card
/// answers is "how much disk is this costing me", and a file that is on disk
/// but not yet swept is still costing it.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScratchStatus {
    /// False before the first drop. The directory is created on demand so that
    /// a fresh install genuinely has no workspaces — which is what the
    /// first-run flow keys off.
    pub exists: bool,
    /// `null` until it exists; the window never prints a guess at a path.
    pub path: Option<String>,
    pub workspace: String,
    pub files: u64,
    pub bytes: u64,
    pub max_bytes: u64,
    pub max_file_bytes: u64,
}

/// What emptying it removed.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearReport {
    pub removed: Vec<String>,
    pub bytes: u64,
}

/// Where the scratch directory would be, whether or not it exists yet.
pub fn dir_in(data_dir: &Path) -> PathBuf {
    data_dir.join(DIR_NAME)
}

/// The scratch root if it has already been granted, without creating anything.
pub fn existing(core: &Core) -> Result<Option<Handle>> {
    let dir = dir_in(core.data_dir());
    // Canonicalized so the comparison below is against the same form
    // `Core::grant` stored. A data directory reached through a symlink — which
    // `~/.local/share` routinely is — would otherwise never match.
    let Ok(canonical) = std::fs::canonicalize(&dir) else {
        return Ok(None);
    };
    let conn = core.store().reader()?;
    let row = conn
        .query_row(
            "SELECT r.root_id, r.workspace_id
               FROM workspace_roots r
               JOIN workspaces w ON w.workspace_id = r.workspace_id
              WHERE r.canonical_path = ?1 AND w.status = 'ACTIVE'
              LIMIT 1",
            [canonical.to_string_lossy().as_ref()],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .map(Some)
        .or_else(|e| match e {
            marrow_store::rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(marrow_store::map_sqlite(
                other,
                "looking up the scratch root",
            )),
        })?;

    let Some((root_id, workspace_id)) = row else {
        return Ok(None);
    };
    let (Ok(root_id), Ok(workspace_id)) = (root_id.parse(), workspace_id.parse()) else {
        return Ok(None);
    };
    Ok(Some(Handle {
        root: AuthorizedRoot::open(&canonical)?,
        root_id,
        workspace_id,
    }))
}

/// The scratch root, creating and granting it if this is the first drop.
///
/// Created on demand rather than at startup for a reason the first-run flow
/// depends on: a fresh install must have *no* workspaces, so that "has this
/// user got anywhere yet?" can be answered from real state rather than from a
/// flag. A scratch workspace conjured at launch would answer "yes" for someone
/// who has never done anything.
///
/// Watching it is the same call a granted folder makes, so the copy that lands
/// here is reconciled by the same machinery as everything else.
pub fn ensure(
    core: &Arc<Core>,
    watchers: Option<&Arc<crate::watching::Watchers>>,
) -> Result<Handle> {
    if let Some(h) = existing(core)? {
        return Ok(h);
    }
    let dir = dir_in(core.data_dir());
    std::fs::create_dir_all(&dir).map_err(|e| {
        Error::new(
            Code::CfgInvalid,
            format!(
                "Could not create the folder Marrow keeps dropped files in: {e}. \
                 Check that Marrow's data directory is writable, then drop the file again."
            ),
        )
        .with_context(dir.display().to_string())
    })?;

    let root_id = core.grant_named(&dir, Some(WORKSPACE_NAME))?;
    if let Some(w) = watchers {
        // Before anything is copied in, because the sweep a watcher runs on
        // start *is* the initial index — the same one code path a folder the
        // user picked goes through.
        w.watch_also(Arc::clone(core), root_id)?;
    }
    existing(core)?
        .ok_or_else(|| Error::invariant("the scratch root was granted but is not in the store"))
}

/// Copy files into the scratch workspace and index them, now.
///
/// `sources` came from the operating system — a drop event or a native panel —
/// never from the window. Each one is canonicalized, checked for a cloud
/// placeholder before anything reads it, and refused individually rather than
/// failing the batch.
///
/// The ingest is [`marrow_ingest::apply_hints`] rather than a wait for the
/// watcher, because "drop a file and ask about it" has to work in the next
/// breath: a watcher event is a hint that arrives up to a poll interval later,
/// and a sweep of the folder would re-hash everything already in it. Same
/// idempotent, resumable pipeline either way (jobs are idempotent and resumable).
pub fn accept(
    core: &Arc<Core>,
    watchers: Option<&Arc<crate::watching::Watchers>>,
    sources: &[PathBuf],
) -> Result<DropReport> {
    let handle = ensure(core, watchers)?;
    let dir = handle.root.path().to_path_buf();
    let mut report = DropReport {
        workspace: WORKSPACE_NAME.to_string(),
        ..DropReport::default()
    };
    // Everything the ingest has to look at afterwards: the copies, and the
    // evictions. Both in one pass, because a deleted path is how `apply_hints`
    // is told to retire a row.
    let mut touched: BTreeSet<PathBuf> = BTreeSet::new();
    let granted = granted_roots(core)?;

    for source in sources {
        match one(&handle, &dir, source, &granted, &mut touched, &mut report) {
            Ok(()) => {}
            Err(e) => report.skipped.push(Skipped {
                name: display_name(source),
                code: e.code().as_str().to_string(),
                reason: e.message().to_string(),
            }),
        }
    }

    if !touched.is_empty() {
        marrow_ingest::apply_hints(
            core.store(),
            handle.workspace_id,
            handle.root_id,
            &handle.root,
            &marrow_ingest::IngestPolicy::default(),
            &touched,
            &Arc::new(marrow_ingest::Progress::new()),
            &marrow_ingest::Cancel::new(),
            Some(core.index()),
        )?;
    }
    Ok(report)
}

/// One source file, from canonicalization through to a copy the index has been
/// told about. Every refusal is an `Err` the caller turns into a `Skipped` row.
fn one(
    handle: &Handle,
    dir: &Path,
    source: &Path,
    granted: &[(String, AuthorizedRoot)],
    touched: &mut BTreeSet<PathBuf>,
    report: &mut DropReport,
) -> Result<()> {
    // Symlink escape is re-checked at operation time. `canonicalize` resolves both `..` and
    // symlinks, so what follows is about the real file and not about the name
    // the window server handed over.
    let canonical = std::fs::canonicalize(source)
        .map_err(|e| Error::from(e).with_context(source.display().to_string()))?;

    let meta = std::fs::symlink_metadata(&canonical)
        .map_err(|e| Error::from(e).with_context(canonical.display().to_string()))?;
    if meta.is_dir() {
        return Err(Error::new(
            Code::CfgInvalid,
            "That is a folder. Marrow indexes a folder where it already is rather \
             than copying it — use “Add a folder” to grant it.",
        ));
    }
    if !meta.is_file() {
        return Err(Error::new(
            Code::CfgInvalid,
            "That is not an ordinary file, so there is nothing to read from it.",
        ));
    }

    // **Never hydrate a placeholder, before anything opens the file.** A dropped iCloud
    // placeholder is a name with no bytes behind it, and copying it is exactly
    // the read that downloads it. Refused with the reason rather than silently
    // pulling a gigabyte over the network.
    marrow_scan::ensure_safe_to_read(&canonical, marrow_scan::tier_of(&canonical)?)?;

    let size = meta.len();
    if size > MAX_FILE_BYTES {
        return Err(Error::new(
            Code::CfgInvalid,
            format!(
                "That file is {} and dropped files are capped at {}. Add its folder \
                 as a workspace instead — a folder is indexed where it is, not copied.",
                human_bytes(size),
                human_bytes(MAX_FILE_BYTES)
            ),
        ));
    }

    // Already inside a folder the user granted — including the scratch root
    // itself, which is how a re-drop of something already here is caught.
    // Component-wise containment, never a string prefix (never hydrate a placeholder).
    if let Some((workspace, _)) = granted.iter().find(|(_, r)| r.contains(&canonical)) {
        return Err(Error::new(
            Code::ActAlreadyExists,
            format!(
                "That file is already indexed in “{workspace}”, so copying it here \
                 would store the same contents twice under two identities."
            ),
        ));
    }

    let name = canonical.file_name().ok_or_else(|| {
        Error::new(
            Code::ActNameRejected,
            "That file has no name Marrow can copy it under.",
        )
    })?;

    // Same name already here with the same contents: nothing to do. Two files
    // with one content hash is expected — dedup is a feature — and copying it
    // again would spend the disk to say the same thing twice.
    let taken = dir.join(name);
    if std::fs::symlink_metadata(&taken).is_ok() && same_contents(&taken, &canonical) {
        report.already_there.push(display_name(&taken));
        return Ok(());
    }

    make_room(dir, size, touched, report)?;

    // The destination is proved to be inside the root *before* the write, from
    // a name that has already been reduced to a single ordinary component.
    let dest = destination(&handle.root, name)?;
    std::fs::copy(&canonical, &dest).map_err(|e| {
        Error::new(
            Code::FsLocked,
            format!(
                "Could not copy “{}” into Marrow's dropped-files folder: {e}. \
                 It may be locked by another application.",
                display_name(&canonical)
            ),
        )
        .with_context(dest.display().to_string())
    })?;

    // And proved again against the live filesystem, now that a file exists
    // there to resolve. This is the check that catches a component replaced by
    // a symlink between the decision and the write — the rule says *at
    // operation time*, and this is the operation.
    handle
        .root
        .resolve(&dest)
        .and_then(|safe| safe.reverify(&handle.root))?;

    report.bytes_added += size;
    report.added.push(display_name(&dest));
    touched.insert(dest);
    Ok(())
}

/// A destination inside `root`, from a source file's own final component.
///
/// Two separate refusals, and both matter:
///
///   * The name must be one ordinary component. `file_name()` already
///     guarantees that for a path the OS produced, but this function is the
///     boundary and a boundary that trusts its caller is not one. `..`, an
///     absolute path, an embedded separator and an empty name are all rejected
///     by name rather than by hoping `join` does the right thing.
///   * Containment is checked with [`AuthorizedRoot::contains`], which compares
///     components. A string prefix test would put `/data/dropped-evil` inside
///     `/data/dropped`, and NFD/NFC spellings of the same folder name would
///     fail to match at all.
///
/// The returned path does not exist yet, which is why this cannot simply call
/// [`AuthorizedRoot::resolve`] — that canonicalizes, and canonicalizing a path
/// with no file at it fails. The caller resolves and re-verifies immediately
/// after the write.
fn destination(root: &AuthorizedRoot, name: &OsStr) -> Result<PathBuf> {
    let reject = |why: &str| {
        Error::new(
            Code::ActNameRejected,
            format!(
                "Marrow will not copy a file in under that name: {why}. Rename it \
                 and drop it again."
            ),
        )
    };

    let as_path = Path::new(name);
    if name.is_empty() {
        return Err(reject("it is empty"));
    }
    let mut components = as_path.components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => {}
        (Some(Component::ParentDir), _) => return Err(reject("it climbs out of the folder")),
        (Some(Component::RootDir), _) | (Some(Component::Prefix(_)), _) => {
            return Err(reject("it is an absolute path, not a file name"))
        }
        (Some(Component::CurDir), _) => return Err(reject("it names a directory, not a file")),
        (None, _) => return Err(reject("it is empty")),
        (Some(_), Some(_)) => return Err(reject("it contains a path separator")),
    }

    let dest = root.path().join(as_path);
    if !root.contains(&dest) {
        // Unreachable given the component check above, and kept anyway: this is
        // the assertion that actually states the property the caller relies on,
        // and it is the one that would catch a future edit loosening the check.
        return Err(Error::new(
            Code::FsPathEscapeBlocked,
            "Refused to copy a file to a path outside Marrow's dropped-files folder.",
        )
        .with_context(dest.display().to_string()));
    }
    Ok(dest)
}

/// Evict oldest-first until `incoming` more bytes fit under [`MAX_TOTAL_BYTES`].
///
/// Oldest by modification time, which for a copy is when it was dropped. The
/// evicted paths join the ingest batch so their rows are retired in the same
/// pass — a file deleted from disk whose index row survives is the "index lying
/// about the disk" failure this whole module exists to avoid.
fn make_room(
    dir: &Path,
    incoming: u64,
    touched: &mut BTreeSet<PathBuf>,
    report: &mut DropReport,
) -> Result<()> {
    let mut entries = contents(dir)?;
    let mut total: u64 = entries.iter().map(|(_, size, _)| *size).sum();
    if total + incoming <= MAX_TOTAL_BYTES {
        return Ok(());
    }
    entries.sort_by_key(|(_, _, modified)| *modified);

    for (path, size, _) in entries {
        if total + incoming <= MAX_TOTAL_BYTES {
            break;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => {
                total = total.saturating_sub(size);
                report.evicted.push(display_name(&path));
                touched.insert(path);
            }
            // A file that will not delete is not a reason to refuse the drop;
            // the cap is a budget, not an invariant. It is logged so a folder
            // stuck over its cap is discoverable.
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "could not evict a dropped file")
            }
        }
    }
    Ok(())
}

/// Everything in the scratch workspace, and how much disk it is costing.
pub fn status(core: &Core) -> Result<ScratchStatus> {
    let dir = dir_in(core.data_dir());
    let entries = if dir.is_dir() {
        contents(&dir)?
    } else {
        Vec::new()
    };
    Ok(ScratchStatus {
        exists: dir.is_dir(),
        path: dir.is_dir().then(|| dir.to_string_lossy().into_owned()),
        workspace: WORKSPACE_NAME.to_string(),
        files: entries.len() as u64,
        bytes: entries.iter().map(|(_, size, _)| *size).sum(),
        max_bytes: MAX_TOTAL_BYTES,
        max_file_bytes: MAX_FILE_BYTES,
    })
}

/// Throw away everything in the scratch workspace.
///
/// The **copies** are deleted from disk, which is Marrow deleting its own
/// bytes. The index rows are not deleted: `apply_hints` finds each path gone
/// and moves `status` to `DELETED`, which is a soft delete through the path the
/// rest of the system already uses. Physical removal from the database happens
/// only through the forget path, and nothing here is a shortcut around it.
///
/// The root itself stays granted. Re-granting it on the next drop would be a
/// second workspace row for the same directory in every listing that ran in
/// between, and an empty workspace is a truthful thing to show.
pub fn clear(core: &Arc<Core>) -> Result<ClearReport> {
    let Some(handle) = existing(core)? else {
        return Ok(ClearReport::default());
    };
    let dir = handle.root.path().to_path_buf();
    let mut report = ClearReport::default();
    let mut touched: BTreeSet<PathBuf> = BTreeSet::new();

    for (path, size, _) in contents(&dir)? {
        match std::fs::remove_file(&path) {
            Ok(()) => {
                report.bytes += size;
                report.removed.push(display_name(&path));
                touched.insert(path);
            }
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "could not remove a dropped file")
            }
        }
    }

    if !touched.is_empty() {
        marrow_ingest::apply_hints(
            core.store(),
            handle.workspace_id,
            handle.root_id,
            &handle.root,
            &marrow_ingest::IngestPolicy::default(),
            &touched,
            &Arc::new(marrow_ingest::Progress::new()),
            &marrow_ingest::Cancel::new(),
            Some(core.index()),
        )?;
    }
    Ok(report)
}

/// Regular files directly in the scratch directory, with size and mtime.
///
/// Top level only, and symlinks are ignored rather than followed: nothing here
/// ever creates a subdirectory or a link, so anything that is not a plain file
/// was put there by something else and is not Marrow's to size or delete.
fn contents(dir: &Path) -> Result<Vec<(PathBuf, u64, std::time::SystemTime)>> {
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::from(e).with_context(dir.display().to_string())),
    };
    let mut out = Vec::new();
    for entry in read.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        out.push((entry.path(), meta.len(), modified));
    }
    Ok(out)
}

/// Every root the user has granted, paired with the workspace that names it.
///
/// Built once per drop rather than per file: opening an `AuthorizedRoot`
/// canonicalizes, and a batch of forty dropped files would otherwise re-stat
/// every root forty times.
fn granted_roots(core: &Core) -> Result<Vec<(String, AuthorizedRoot)>> {
    let conn = core.store().reader()?;
    let mut stmt = conn
        .prepare(
            "SELECT w.name, r.canonical_path
               FROM workspace_roots r
               JOIN workspaces w ON w.workspace_id = r.workspace_id
              WHERE w.status = 'ACTIVE'",
        )
        .map_err(|e| marrow_store::map_sqlite(e, "listing granted roots"))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .and_then(|it| it.collect::<std::result::Result<Vec<_>, _>>())
        .map_err(|e| marrow_store::map_sqlite(e, "listing granted roots"))?;

    Ok(rows
        .into_iter()
        // A root on a volume that is no longer mounted cannot be opened, and it
        // is not a reason to refuse a drop into a folder that is fine.
        .filter_map(|(name, path)| AuthorizedRoot::open(&path).ok().map(|r| (name, r)))
        .collect())
}

/// Whether two files hold the same bytes.
///
/// By content hash, not by size and date: the whole point is to recognise the
/// same document dropped twice, and a copy made by another program has a
/// different timestamp. A file that cannot be hashed — a cloud placeholder, a
/// permission failure — is not "the same", so the caller goes on to make a
/// distinct copy rather than silently treating them as one file.
fn same_contents(a: &Path, b: &Path) -> bool {
    match (marrow_scan::hash_file(a), marrow_scan::hash_file(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Sizes in a refusal the user reads. Deliberately coarse — the decision is
/// "too big", not "how big".
fn human_bytes(n: u64) -> String {
    const MB: u64 = 1024 * 1024;
    if n >= MB {
        format!("{} MB", n / MB)
    } else {
        format!("{n} bytes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> (tempfile::TempDir, AuthorizedRoot) {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = AuthorizedRoot::open(dir.path()).expect("authorize");
        (dir, root)
    }

    #[test]
    fn a_name_that_climbs_out_of_the_scratch_root_is_refused() {
        // The check the drop path depends on. `file_name()` on an OS-supplied
        // path cannot produce these, which is exactly why the boundary must
        // refuse them itself rather than trusting that it never sees one.
        let (_dir, root) = root();
        for bad in ["..", "../escape", "/etc/passwd", "a/b", "", "."] {
            let e = destination(&root, OsStr::new(bad))
                .expect_err(&format!("`{bad}` must not be a destination"));
            assert!(
                matches!(e.code(), Code::ActNameRejected | Code::FsPathEscapeBlocked),
                "`{bad}` was refused with {} instead",
                e.code()
            );
        }
    }

    #[test]
    fn an_ordinary_name_lands_directly_inside_the_root() {
        let (_dir, root) = root();
        let dest = destination(&root, OsStr::new("lease.pdf")).expect("an ordinary name");
        assert_eq!(dest.parent(), Some(root.path()));
        assert!(root.contains(&dest));
    }

    #[test]
    fn containment_is_component_wise_not_a_string_prefix() {
        // `/tmp/x/dropped-evil` starts with `/tmp/x/dropped` as a string and is
        // not inside it as a path. This is the failure mode the operation-time symlink check names
        // explicitly, and the reason `destination` calls `contains` rather than
        // comparing text.
        let parent = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir(parent.path().join("dropped")).expect("dropped");
        std::fs::create_dir(parent.path().join("dropped-evil")).expect("sibling");
        let root = AuthorizedRoot::open(parent.path().join("dropped")).expect("authorize");
        assert!(!root.contains(&parent.path().join("dropped-evil").join("x.md")));
    }

    #[test]
    fn a_symlink_planted_at_the_destination_cannot_redirect_a_copy() {
        // The attack the post-write re-verification exists for: a link inside
        // the scratch folder pointing at a file outside it. `resolve` +
        // `reverify` resolve the link and find themselves outside the root.
        let (_dir, root) = root();
        let outside = tempfile::tempdir().expect("outside");
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, "untouched").expect("write");

        let link = root.path().join("note.md");
        std::os::unix::fs::symlink(&secret, &link).expect("symlink");

        // `destination` is lexical and cannot see the link — which is the point
        // of checking again once the filesystem has been consulted.
        let dest = destination(&root, OsStr::new("note.md")).expect("a lexically fine name");
        let escaped = root
            .resolve(&dest)
            .and_then(|safe| safe.reverify(&root))
            .expect_err("a link out of the root must be refused");
        assert_eq!(escaped.code(), Code::FsPathEscapeBlocked);
        assert_eq!(
            std::fs::read_to_string(&secret).expect("still there"),
            "untouched"
        );
    }

    #[test]
    fn evicting_keeps_the_folder_under_its_ceiling_and_says_what_went() {
        let dir = tempfile::tempdir().expect("temp dir");
        // Three files whose sizes are known, written oldest-first. `make_room`
        // is asked for more than the remaining headroom, so the oldest has to
        // go — and has to be reported, because a file that vanishes without a
        // word is the same silent rot as a per-session wipe.
        for name in ["old.bin", "mid.bin", "new.bin"] {
            std::fs::write(dir.path().join(name), vec![0u8; 1024]).expect("write");
            // `filetime` is not a dependency here; a real gap in mtime is what
            // the sort needs and a short sleep is the cheapest way to get one.
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut touched = BTreeSet::new();
        let mut report = DropReport::default();
        make_room(
            dir.path(),
            MAX_TOTAL_BYTES - 2048,
            &mut touched,
            &mut report,
        )
        .expect("eviction");

        assert_eq!(report.evicted, vec!["old.bin".to_string()]);
        assert!(!dir.path().join("old.bin").exists());
        assert!(dir.path().join("new.bin").exists());
        assert!(
            touched.contains(&dir.path().join("old.bin")),
            "an evicted file must reach the ingest so its row is retired"
        );
    }

    #[test]
    fn nothing_is_evicted_while_there_is_room() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("a.md"), "small").expect("write");
        let mut touched = BTreeSet::new();
        let mut report = DropReport::default();
        make_room(dir.path(), 1024, &mut touched, &mut report).expect("no eviction needed");
        assert!(report.evicted.is_empty());
        assert!(touched.is_empty());
    }

    #[test]
    fn the_same_file_dropped_twice_is_recognised_by_its_contents() {
        let dir = tempfile::tempdir().expect("temp dir");
        let a = dir.path().join("a.md");
        let b = dir.path().join("b.md");
        std::fs::write(&a, "the lease renews on 31 December").expect("write");
        std::fs::write(&b, "the lease renews on 31 December").expect("write");
        assert!(same_contents(&a, &b));
        std::fs::write(&b, "something else entirely").expect("write");
        assert!(!same_contents(&a, &b));
    }
}
