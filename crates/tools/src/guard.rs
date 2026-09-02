//! The one guarded write path. Everything this crate creates goes through
//! [`Workspace::write`] — there is no second way to put bytes on disk.
//!
//! The order below is the design. Each step exists because the step before it
//! is not sufficient on its own, and each has a test named after the rule it
//! enforces.
//!
//! ```text
//!  1. name rules            crate::name — refuse the string before any syscall
//!  2. containment           canonicalize every level, compare components
//!  3. protected + excluded  .git, the model directory, config exclusions
//!  4. collision             a name differing only by case or NFC/NFD
//!  ── the caller's world can change at any point from here ──
//!  5. temp file             O_EXCL in the destination directory
//!  6. re-verify             canonicalize the temp file: is it *really* inside?
//!  7. stale check           digest the file we are replacing, now, not earlier
//!  8. rename                one atomic move; a crash leaves old or new
//!  9. Origin::SelfWritten   in the result, so it can never be cited back
//! ```
//!
//! **Why the checks repeat.** Steps 2–4 prove things about the filesystem as it
//! was. A `mv` in another terminal, a sync client, or a poisoned repo's
//! post-checkout hook can invalidate all of them between the proof and the
//! write. The symlink-escape rule says *at operation time*: steps 6 and 7 are the same
//! questions asked again with nothing in between them and the `rename`.
//!
//! **Why the stale check is last.** The stale-version rule is "immediately
//! before any write". Digesting the file first and then spending 20 ms building content
//! would make the check a formality. The bytes are prepared into a temp file
//! first, so the last three syscalls are: digest, rename, done.

use std::collections::BTreeSet;
use std::fmt;
use std::fs::{self, File};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use marrow_core::{Code, ContentHash, Error, Origin, Result, Timestamp};
use marrow_scan::{path_key, AuthorizedRoot, DEFAULT_NOISE_DIRS};
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::name;

/// What the caller believes it is replacing. **The stale-version rule.**
///
/// There is deliberately no "just overwrite" variant. A write with no
/// precondition is the one that silently discards the paragraph the user typed
/// into their editor thirty seconds ago, and an escape hatch on a rule this
/// cheap would be taken every time.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Expect {
    /// Nothing is there. Anything present — file, directory or symlink — is a
    /// refusal, not an overwrite.
    #[default]
    New,
    /// Replacing content the caller has read, identified by its digest.
    Replacing(ContentHash),
}

/// What a completed write did. **`origin = SELF`** is carried in the type: there
/// is no constructor that produces anything but [`Origin::SelfWritten`].
#[derive(Clone, Debug, Serialize)]
pub struct Written {
    path: PathBuf,
    digest: ContentHash,
    bytes: u64,
    origin: Origin,
    replaced: Option<ContentHash>,
    /// The bytes this write displaced, if it displaced any and the workspace
    /// was given somewhere to keep them.
    snapshot: Option<crate::snapshot::SnapshotId>,
    written_at: Timestamp,
}

impl Written {
    /// Absolute, canonical path of the file that now exists.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// BLAKE3 of the bytes written. The caller's precondition for the *next*
    /// write to this file.
    pub fn digest(&self) -> ContentHash {
        self.digest
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Always [`Origin::SelfWritten`].
    pub fn origin(&self) -> Origin {
        self.origin
    }

    /// Digest of the content this write replaced, if it replaced anything.
    /// The handle that undoes this write.
    ///
    /// `None` for a creation — there is nothing to restore, and undoing one
    /// means removing the file, which [`Workspace::undo`] does from the digest
    /// rather than from a snapshot. Also `None` when the workspace has no
    /// store, which is how a caller finds out that this replacement is final.
    pub fn snapshot(&self) -> Option<&crate::snapshot::SnapshotId> {
        self.snapshot.as_ref()
    }

    /// Whether [`Workspace::undo`] can put this back.
    ///
    /// A creation is always undoable. A replacement is undoable only if
    /// something caught what it displaced.
    pub fn is_undoable(&self) -> bool {
        self.replaced.is_none() || self.snapshot.is_some()
    }

    pub fn replaced(&self) -> Option<ContentHash> {
        self.replaced
    }

    pub fn written_at(&self) -> Timestamp {
        self.written_at
    }

    /// Always false. Kept as a method rather than a comment because the caller
    /// that indexes this file has to ask the question, and a method is harder
    /// to forget than a paragraph.
    pub fn can_support_a_claim(&self) -> bool {
        self.origin.can_support_a_claim()
    }
}

/// Fired after validation and before anything is written, so a test can do
/// what a racing process would. Production code never sets one; there is no
/// public constructor.
pub(crate) type RaceHook = Box<dyn Fn(&Path) + Send + Sync>;

/// A directory tree this crate may create files in.
pub struct Workspace {
    root: AuthorizedRoot,
    /// Directory names refused at any depth, folded for comparison.
    excluded: BTreeSet<String>,
    /// Subtrees refused outright — the model directory, the index, anything
    /// else whose contents this crate must never author.
    protected: Vec<PathBuf>,
    /// Where overwritten content is kept, when the caller supplied a store.
    ///
    /// `Option`, and the absence is load-bearing rather than lazy: a workspace
    /// with no store can still *create* files, and only a replacement needs
    /// somewhere to put what it displaced. Making it mandatory would mean every
    /// caller that only ever creates has to invent a directory.
    snapshots: Option<crate::snapshot::Snapshots>,
    race: Option<RaceHook>,
}

impl fmt::Debug for Workspace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Workspace")
            .field("root", &self.root.path())
            .field("excluded", &self.excluded)
            .field("protected", &self.protected)
            .field("snapshots", &self.snapshots.as_ref().map(|s| s.dir()))
            .finish()
    }
}

impl Workspace {
    /// Adopt `root` as the only tree this workspace can write into.
    ///
    /// The root is canonicalised now and must exist. Exclusions start at
    /// [`DEFAULT_NOISE_DIRS`], which already contains `.git`, `node_modules`
    /// and `target` — the directories where a written file is either someone
    /// else's state or noise that should never have been indexed.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            root: AuthorizedRoot::open(root)?,
            excluded: DEFAULT_NOISE_DIRS.iter().map(|d| fold(d)).collect(),
            protected: Vec::new(),
            snapshots: None,
            race: None,
        })
    }

    /// Refuse a directory name wherever it appears below the root.
    pub fn exclude_dir(mut self, name: &str) -> Self {
        self.excluded.insert(fold(name));
        self
    }

    /// Stop refusing a directory name that is excluded by default.
    pub fn allow_dir(mut self, name: &str) -> Self {
        self.excluded.remove(&fold(name));
        self
    }

    /// Refuse an entire subtree. Absolute, or relative to the root.
    ///
    /// The model directory is the case this exists for. It normally lives
    /// outside every workspace root (SUP-011), in which case containment
    /// already refuses it — but "normally" is doing too much work in a sentence
    /// about somebody's `~/.ssh`, and a misconfigured root must not be the only
    /// thing between an agent and the weights it is running on.
    pub fn protect(mut self, path: impl AsRef<Path>) -> Self {
        let p = path.as_ref();
        let joined = if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.root.path().join(p)
        };
        // Canonicalise if it exists; a protected directory that has not been
        // created yet is still protected.
        self.protected
            .push(fs::canonicalize(&joined).unwrap_or(joined));
        self
    }

    /// Keep what replacements displace, so they can be undone.
    ///
    /// The store belongs beside the index rather than in the workspace: a
    /// snapshot written next to its original would be indexed, cited, and
    /// eventually snapshotted itself — and it has to outlive the workspace
    /// being deleted, which is one of the things people want undone.
    ///
    /// Without one, [`Workspace::write`] still refuses every *wrong* write and
    /// still cannot undo a right-looking one. It says so through
    /// [`Written::snapshot`] returning `None` rather than by failing, because a
    /// caller that only creates files has nothing to lose.
    pub fn with_snapshots(mut self, snapshots: crate::snapshot::Snapshots) -> Self {
        self.snapshots = Some(snapshots);
        self
    }

    pub fn root(&self) -> &Path {
        self.root.path()
    }

    /// Install the race seam. Crate-private, and used only by this crate's
    /// tests and by the corpus runner.
    pub(crate) fn with_race(mut self, hook: RaceHook) -> Self {
        self.race = Some(hook);
        self
    }

    /// **The write path.** Everything else in this crate is content generation
    /// in front of this function.
    ///
    /// `relative` is workspace-relative; missing parent directories are
    /// created. `expect` is the precondition, checked immediately before the
    /// rename.
    pub fn write(&self, relative: &str, bytes: &[u8], expect: &Expect) -> Result<Written> {
        let plan = self.plan(relative)?;

        // Everything above proved facts about the filesystem as it was a
        // moment ago. This is where a racing process gets its chance, and
        // every check from here down is re-asked against live inodes.
        if let Some(hook) = &self.race {
            hook(&plan.target);
        }

        // The destination directory, re-resolved. If it became a symlink out of
        // the workspace since `plan` ran, this is where that shows up.
        let parent_now = self.canonical_within(&plan.parent)?;
        if parent_now != plan.parent {
            return Err(escape_error(
                &parent_now,
                "the destination directory changed identity between the check and the write",
            ));
        }

        let temp = TempFile::create(&parent_now)?;
        // Canonicalising the file we just created proves containment against
        // the inode that exists, not against the path we hoped it would take.
        // A parent swapped for a symlink is caught here even if it happened
        // microseconds ago.
        let temp_canonical = self.canonical_within(temp.path())?;
        if temp_canonical != *temp.path() {
            return Err(escape_error(
                &temp_canonical,
                "the file created for this write did not land where it was created",
            ));
        }
        temp.fill(bytes)?;

        let replaced = self.check_precondition(&plan.target, expect)?;

        // **After the precondition, before the rename.** Before it, and the
        // bytes captured might not be the ones this write was authorised to
        // replace; after it, and there is nothing left to capture.
        let snapshot = self.capture_replaced(&plan.target, replaced.as_ref())?;

        temp.commit(&plan.target)?;
        sync_dir(&parent_now);

        let digest = ContentHash::of(bytes);
        tracing::info!(
            path = %plan.target.display(),
            digest = %digest,
            bytes = bytes.len(),
            origin = "SELF",
            "wrote file"
        );
        Ok(Written {
            path: plan.target,
            digest,
            bytes: bytes.len() as u64,
            // `origin = SELF`. Not a parameter, so no caller can weaken it.
            origin: Origin::SelfWritten,
            replaced,
            snapshot,
            written_at: Timestamp::now(),
        })
    }

    /// Keep what a replacement is about to destroy.
    ///
    /// `None` when nothing was there, and `None` when no store was configured —
    /// the two cases are different and neither is an error. A workspace that
    /// only creates files has nothing to lose, and saying so through
    /// [`Written::snapshot`] beats refusing writes to demand a directory the
    /// caller has no use for.
    ///
    /// **The read is verified against the digest the precondition just
    /// established.** `check_precondition` hashed the file; this opens it
    /// again, and between the two a sync client or an editor can land. Trusting
    /// the second read would file the wrong bytes under the right digest, so an
    /// undo would restore content that was never there — a corruption invented
    /// by the recovery path, which is the worst place to invent one.
    fn capture_replaced(
        &self,
        target: &Path,
        replaced: Option<&ContentHash>,
    ) -> Result<Option<crate::snapshot::SnapshotId>> {
        let (Some(expected), Some(store)) = (replaced, &self.snapshots) else {
            return Ok(None);
        };

        // Safe to read: `check_precondition` reached `hash_file`, which refuses
        // a cloud placeholder before opening it, so anything still here is
        // resident. Never hydrate a placeholder is upheld upstream rather than
        // re-asked, because re-asking would mean a second `stat` and a third
        // window.
        let bytes = fs::read(target)
            .map_err(|e| Error::from(e).with_context(target.display().to_string()))?;
        let actual = ContentHash::of(&bytes);
        if actual != *expected {
            return Err(Error::new(
                Code::ActStaleVersion,
                "The file changed while this write was being prepared, so the copy kept for \
                 undo would not have matched what was replaced. Nothing was written.",
            )
            .with_context(format!(
                "{} — expected {expected}, found {actual}",
                target.display()
            )));
        }
        // `try_capture`, not `capture`: a file too large to keep leaves the
        // write valid and simply not undoable, which `Written::is_undoable`
        // reports. Failing the write instead would mean a size limit on a
        // convenience could stop somebody doing what they asked for.
        store.try_capture(&bytes)
    }

    /// Put back what a write displaced.
    ///
    /// Two shapes, because a write has two:
    ///
    /// - **A replacement** is undone by restoring the snapshot, through
    ///   [`Workspace::write`] and therefore through every containment and
    ///   symlink check the original write passed. Nothing here writes on its
    ///   own account; one `rename` for the crate is the point of the crate.
    /// - **A creation** is undone by removing the file. There is no earlier
    ///   content to restore, and leaving it would make "undo" mean "nearly".
    ///
    /// **Both are stale-checked against what this write left behind.** If the
    /// file no longer hashes to [`Written::digest`], somebody edited it after
    /// the tool ran, and undoing would be the second destruction rather than
    /// the repair of the first. That is refused, with the digests in the
    /// message, and it is the case worth having a test for: an undo that
    /// discards a human edit is worse than the write it was undoing.
    ///
    /// # Origin
    ///
    /// [`Undone::origin`] is [`Origin::User`], not `SelfWritten`, and the
    /// difference is not cosmetic. Restoring somebody's file gives them their
    /// own words back; continuing to mark them as this system's output would
    /// keep them out of evidence under the `origin = SELF` rule, so an undo
    /// would silently cost the user a citable source. The caller owns writing
    /// that back to `files.origin`, exactly as it owns writing `SELF` after a
    /// normal write.
    pub fn undo(&self, written: &Written) -> Result<Undone> {
        self.undo_inner(written)
    }

    /// [`Workspace::undo`], addressed by value rather than by object.
    ///
    /// A caller that crossed a process boundary — MCP, the CLI — has three
    /// strings and no `Written`. Rather than have it reconstruct one, or have
    /// this crate grow a transaction table before the milestone asks for one,
    /// undo takes exactly what the write reported: where it wrote, the digest
    /// it left, and the handle for what it displaced.
    ///
    /// **This grants no authority the caller did not already have.** Restoring
    /// snapshot *X* over path *Y* writes bytes the user once had to a path the
    /// caller chose — and a caller who can reach this can reach
    /// [`crate::create_file`], which writes arbitrary bytes to the same place
    /// through the same guard. Binding a snapshot to its original path would be
    /// state for its own sake. What actually constrains this is unchanged:
    /// containment, the protected subtrees, and `wrote` having to match what is
    /// on disk right now.
    pub fn undo_write(&self, path: &Path, wrote: ContentHash, action: Undo) -> Result<Undone> {
        let written = match &action {
            Undo::Restore(id) => Written {
                path: path.to_path_buf(),
                digest: wrote,
                bytes: 0,
                origin: Origin::SelfWritten,
                replaced: Some(*id.digest()),
                snapshot: Some(id.clone()),
                written_at: Timestamp::now(),
            },
            Undo::RemoveCreated => Written {
                path: path.to_path_buf(),
                digest: wrote,
                bytes: 0,
                origin: Origin::SelfWritten,
                replaced: None,
                snapshot: None,
                written_at: Timestamp::now(),
            },
        };
        self.undo_inner(&written)
    }

    fn undo_inner(&self, written: &Written) -> Result<Undone> {
        let relative = written
            .path()
            .strip_prefix(self.root.path())
            .map_err(|_| Error::invariant("undoing a write that is not inside this workspace"))?
            .to_str()
            .ok_or_else(|| {
                Error::new(
                    Code::FsNotUtf8Path,
                    "That file's path is not valid UTF-8, so it cannot be undone by name.",
                )
            })?
            .to_string();

        match (&written.replaced, &written.snapshot) {
            // A replacement whose displaced bytes were kept.
            (Some(_), Some(id)) => {
                let store = self.snapshots.as_ref().ok_or_else(|| {
                    Error::new(
                        Code::CfgInvalid,
                        "This workspace has nowhere to read saved copies from, so that write \
                         cannot be undone here.",
                    )
                })?;
                let bytes = store.read(id)?;
                // Through the front door: the same containment, the same
                // symlink re-check, the same atomic rename. `Replacing` is what
                // makes this refuse if the file moved on since.
                let restored =
                    self.write(&relative, &bytes, &Expect::Replacing(written.digest()))?;
                Ok(Undone {
                    path: restored.path().to_path_buf(),
                    digest: restored.digest(),
                    bytes: restored.bytes(),
                    removed: false,
                    origin: Origin::User,
                })
            }
            // A replacement whose displaced bytes were not kept. Nothing to do
            // but say so plainly — silently doing nothing, or removing the file
            // as if it had been a creation, are both worse.
            (Some(_), None) => Err(Error::new(
                Code::ActNotReversible,
                "That write replaced a file and no copy of the earlier content was kept, so \
                 it cannot be undone. Configure a snapshot store before writes that replace.",
            )
            .with_context(written.path().display().to_string())),
            // A creation. Undoing it is removing it.
            (None, _) => {
                let target = self.canonical_within(written.path())?;
                let actual = marrow_scan::hash_file(&target)?;
                if actual != written.digest() {
                    return Err(Error::new(
                        Code::ActStaleVersion,
                        "That file changed after it was created, so it was left alone. \
                         Removing it now would discard an edit somebody made since.",
                    )
                    .with_context(format!(
                        "{} — expected {}, found {actual}",
                        target.display(),
                        written.digest()
                    )));
                }
                fs::remove_file(&target)
                    .map_err(|e| Error::from(e).with_context(target.display().to_string()))?;
                if let Some(parent) = target.parent() {
                    sync_dir(parent);
                }
                tracing::info!(path = %target.display(), "undid a creation by removing it");
                Ok(Undone {
                    path: target,
                    digest: written.digest(),
                    bytes: 0,
                    removed: true,
                    origin: Origin::User,
                })
            }
        }
    }

    /// Resolve an existing workspace-relative file, proven inside the root.
    ///
    /// For readers. [`Workspace::write`] cannot be reused for this: it creates
    /// missing parent directories on the way, which is right for a write and
    /// wrong for looking something up. The rules that matter are the same ones
    /// — the name is validated, excluded and protected subtrees are refused,
    /// and containment is proved by canonicalising the path that exists rather
    /// than the one that was asked for, so a symlink out of the tree is caught
    /// here exactly as it would be on the way in.
    pub fn resolve_existing(&self, relative: &str) -> Result<PathBuf> {
        let components = name::validate(relative)?;
        for c in &components {
            if self.excluded.contains(&fold(c)) {
                return Err(Error::new(
                    Code::PolDenied,
                    format!("`{c}` holds state this system did not author."),
                )
                .with_context(relative.to_string()));
            }
        }
        let intended = self.root.path().join(relative);
        self.refuse_if_protected(&intended, relative)?;
        let resolved = self.canonical_within(&intended)?;
        self.refuse_if_protected(&resolved, relative)?;
        if !resolved.is_file() {
            return Err(Error::new(
                Code::FsNotFound,
                "There is no file at that path in this workspace.",
            )
            .with_context(relative.to_string()));
        }
        Ok(resolved)
    }

    /// Steps 1–4: everything decidable before the destination directory is
    /// allowed to change under us.
    fn plan(&self, relative: &str) -> Result<Plan> {
        let components = name::validate(relative)?;
        let (file_name, dirs) = components
            .split_last()
            .ok_or_else(|| Error::invariant("name::validate returned no components"))?;

        for c in &components {
            if self.excluded.contains(&fold(c)) {
                return Err(Error::new(
                    Code::PolDenied,
                    format!(
                        "`{c}` holds state this system did not author — writing into it \
                         would corrupt someone else's tool. Choose a directory outside it."
                    ),
                )
                .with_context(relative.to_string()));
            }
        }

        // Lexical pre-check against the protected subtrees, *before* any
        // directory is created. `name::validate` has already refused `..`, so
        // joining is sound here; the same check runs again after
        // canonicalisation, where a symlink cannot hide behind it.
        let intended = self.root.path().join(relative);
        self.refuse_if_protected(&intended, relative)?;

        // Create and prove one level at a time. Creating the whole chain first
        // and checking afterwards would mean a symlinked ancestor had already
        // been followed by `create_dir_all`.
        let mut parent = self.root.path().to_path_buf();
        for d in dirs {
            let next = parent.join(d);
            if let Err(e) = fs::create_dir(&next) {
                if e.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(Error::from(e).with_context(next.display().to_string()));
                }
            }
            parent = self.canonical_within(&next)?;
        }

        let target = parent.join(file_name);
        self.refuse_if_protected(&target, relative)?;

        match fs::symlink_metadata(&target) {
            Ok(meta) if meta.file_type().is_symlink() => {
                // Resolve it so the message can say whether it left the
                // workspace, then refuse either way: `rename` replaces the link
                // rather than following it, so "writing" here would quietly
                // destroy a link the user made, and following it would be worse.
                let resolved = fs::canonicalize(&target).unwrap_or_else(|_| target.clone());
                return Err(escape_error(
                    &resolved,
                    "the target is a symbolic link, which a write must never follow or replace",
                ));
            }
            Ok(meta) if meta.is_dir() => {
                return Err(Error::new(
                    Code::PolDenied,
                    "A directory already exists at that path, so there is nothing to write. \
                     Name a file inside it, or choose another path.",
                )
                .with_context(target.display().to_string()));
            }
            _ => {}
        }

        self.refuse_collision(&parent, file_name)?;

        Ok(Plan { parent, target })
    }

    /// Canonicalise and prove the result is inside the root. This is the only
    /// place containment is decided, so there is one implementation to be
    /// wrong.
    fn canonical_within(&self, path: &Path) -> Result<PathBuf> {
        let canonical = fs::canonicalize(path)
            .map_err(|e| Error::from(e).with_context(path.display().to_string()))?;
        if !self.root.contains(&canonical) {
            tracing::warn!(
                root = %self.root.path().display(),
                resolved = %canonical.display(),
                "write path escape blocked"
            );
            return Err(escape_error(
                &canonical,
                "it resolves outside the workspace root (symlink or traversal)",
            ));
        }
        Ok(canonical)
    }

    fn refuse_if_protected(&self, target: &Path, relative: &str) -> Result<()> {
        for p in &self.protected {
            if under(target, p) {
                return Err(Error::new(
                    Code::PolDenied,
                    "That directory is protected: files written there would be read back \
                     by this system as if a person had written them. Write outside it.",
                )
                .with_context(format!("{relative} → inside {}", p.display())));
            }
        }
        Ok(())
    }

    /// **The Unicode NFC/NFD rule** (§126 #14). A name that differs from an
    /// existing one only by
    /// capitalisation or Unicode normalisation is the same file on APFS and a
    /// different file on a case-sensitive volume. Either way the caller did not
    /// mean to address the file that is already there, so refuse rather than
    /// guess — and refuse on every filesystem, so the rule does not depend on
    /// which volume the workspace happens to live on.
    fn refuse_collision(&self, parent: &Path, file_name: &str) -> Result<()> {
        let folded = fold(file_name);
        let Ok(entries) = fs::read_dir(parent) else {
            return Ok(());
        };
        let names: Vec<String> = entries
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(str::to_owned))
            .collect();
        if names.iter().any(|n| n == file_name) {
            // The exact name exists. Whether it may be replaced is the stale
            // check's question, not this one's — and it is checked first,
            // because on a case-sensitive volume both spellings can be present
            // and directory order must not decide the answer.
            return Ok(());
        }
        for existing in &names {
            if fold(existing) == folded {
                return Err(Error::new(
                    Code::ActAlreadyExists,
                    format!(
                        "`{existing}` already exists and differs from the requested name only \
                         by capitalisation or Unicode normalisation — on this disk they are one \
                         file. Use the existing spelling, or choose a different name."
                    ),
                )
                .with_context(format!("requested `{file_name}`")));
            }
        }
        Ok(())
    }

    /// **The stale-version check**, run with nothing between it and the rename.
    ///
    /// Returns the digest of what is being replaced, so the caller can record
    /// what it overwrote.
    fn check_precondition(&self, target: &Path, expect: &Expect) -> Result<Option<ContentHash>> {
        let present = fs::symlink_metadata(target);

        // A symlink here means one appeared after `plan` refused symlinks.
        // That is a race, not a naming mistake, and it is the case the
        // canonicalize-and-check-symlink-escape-at-operation-time rule exists
        // for.
        if let Ok(meta) = &present {
            if meta.file_type().is_symlink() {
                return Err(escape_error(
                    target,
                    "the target became a symbolic link after it was checked",
                ));
            }
        }

        match expect {
            Expect::New => {
                if present.is_ok() {
                    return Err(Error::new(
                        Code::ActAlreadyExists,
                        "A file already exists at that path and this write expected to create \
                         a new one. Read it and pass its digest to replace it, or choose \
                         another name.",
                    )
                    .with_context(target.display().to_string()));
                }
                Ok(None)
            }
            Expect::Replacing(expected) => {
                if present.is_err() {
                    return Err(Error::new(
                        Code::FsNotFound,
                        "The file this write meant to replace is no longer there. Re-read the \
                         directory and try again.",
                    )
                    .with_context(target.display().to_string()));
                }
                // `hash_file` refuses a cloud placeholder before opening it (the
                // never-hydrate rule) — reading one here to compare digests would
                // download it, which is the failure TIER-001 is about.
                let actual = marrow_scan::hash_file(target)?;
                if actual != *expected {
                    return Err(Error::new(
                        Code::ActStaleVersion,
                        "The file changed since it was read, so this write would discard \
                         someone else's edit. Re-read it and try again.",
                    )
                    .with_context(format!(
                        "{} — expected {expected}, found {actual}",
                        target.display()
                    )));
                }
                Ok(Some(actual))
            }
        }
    }
}

/// Which undo is being asked for.
///
/// **Stated, never inferred, and that is a bug fix rather than a preference.**
/// The first version of `undo_write` worked out whether a write had replaced
/// something by asking whether a snapshot handle was supplied. A replacement
/// whose snapshot had not been kept therefore looked exactly like a creation —
/// so undoing an *un-undoable* replacement deleted the user's file instead of
/// refusing, which is the opposite of what the word undo promises. Across a
/// wire the same shape is worse: a caller that simply forgets the handle
/// deletes a file it meant to restore.
///
/// So the caller says which, and "neither" is a refusal rather than a default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Undo {
    /// Put these bytes back. The handle came from [`Written::snapshot`].
    Restore(crate::snapshot::SnapshotId),
    /// Remove the file, because the write created it and displaced nothing.
    RemoveCreated,
}

/// What an undo did.
///
/// A separate type from [`Written`] rather than a reuse, because the two differ
/// on the field that matters most: a write produces
/// [`Origin::SelfWritten`] content and an undo hands the user their own back.
/// Sharing the type would mean the `origin = SELF` rule depended on a caller
/// reading a boolean correctly.
#[derive(Clone, Debug, Serialize)]
pub struct Undone {
    path: PathBuf,
    digest: ContentHash,
    bytes: u64,
    removed: bool,
    origin: Origin,
}

impl Undone {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The digest of what is there now. For a removal, the digest of what was
    /// removed — the file is gone, and naming what went is more useful than a
    /// hash of nothing.
    pub fn digest(&self) -> ContentHash {
        self.digest
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// True when undoing meant deleting, because the write had created it.
    pub fn removed(&self) -> bool {
        self.removed
    }

    /// Always [`Origin::User`]. See [`Workspace::undo`].
    pub fn origin(&self) -> Origin {
        self.origin
    }
}

#[derive(Debug)]
struct Plan {
    /// Canonical destination directory, proven inside the root.
    parent: PathBuf,
    /// Canonical directory plus the validated file name.
    target: PathBuf,
}

/// A file being written, removed on drop unless it was committed.
///
/// `Drop` rather than cleanup at each `return`: there are seven ways out of
/// [`Workspace::write`] after the temp file exists, and the eighth one added
/// next year would leak.
#[derive(Debug)]
struct TempFile {
    path: PathBuf,
    committed: bool,
}

impl TempFile {
    /// In the destination directory, because `rename` is only atomic within one
    /// filesystem — a temp file in `/tmp` would be a copy, and a copy has a
    /// window where the destination is half-written.
    ///
    /// Dot-prefixed so the indexer's `skip_hidden` walk never sees it, and
    /// `create_new` so it can never land on an existing file.
    fn create(dir: &Path) -> Result<Self> {
        let path = dir.join(format!(".marrow-write-{}.tmp", ulid::Ulid::new()));
        File::create_new(&path)
            .map_err(|e| Error::from(e).with_context(path.display().to_string()))?;
        Ok(Self {
            path,
            committed: false,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn fill(&self, bytes: &[u8]) -> Result<()> {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .open(&self.path)
            .map_err(|e| Error::from(e).with_context(self.path.display().to_string()))?;
        f.write_all(bytes)?;
        // Before the rename, not after: a rename that lands before the data
        // reaches the platter is how a crash produces a zero-length file with
        // the right name.
        f.sync_all()?;
        Ok(())
    }

    fn commit(mut self, target: &Path) -> Result<()> {
        fs::rename(&self.path, target).map_err(|e| {
            Error::from(e).with_context(format!("{} → {}", self.path.display(), target.display()))
        })?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if !self.committed {
            // Best effort by necessity: this runs during unwinding too, and a
            // failure to tidy up must not become the error the caller sees.
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Durability of the rename itself. Best effort and never fatal: the file is
/// already correct, this only decides whether a power cut in the next second
/// can lose the directory entry.
fn sync_dir(dir: &Path) {
    match File::open(dir) {
        Ok(f) => {
            if let Err(e) = f.sync_all() {
                tracing::debug!(dir = %dir.display(), error = %e, "directory fsync failed");
            }
        }
        Err(e) => tracing::debug!(dir = %dir.display(), error = %e, "directory fsync failed"),
    }
}

/// Comparison form for a file or directory name: NFC so the two spellings of
/// `café` are one name (the Unicode NFC/NFD rule, §126 #14), lowercased so
/// `Notes` and `notes` are
/// one name on the case-insensitive volume this runs on by default.
fn fold(s: &str) -> String {
    s.nfc().collect::<String>().to_lowercase()
}

/// Whether `path` is `ancestor` or lies below it.
///
/// Compares NFC path strings with an explicit separator appended, so
/// `/root-evil` is not "inside" `/root` — the string-prefix bug the
/// symlink-escape rule names by hand.
fn under(path: &Path, ancestor: &Path) -> bool {
    let (Ok(p), Ok(a)) = (path_key(path), path_key(ancestor)) else {
        return false;
    };
    let (p, a) = (p.as_str().to_string(), a.as_str().to_string());
    p == a || p.starts_with(&format!("{}/", a.trim_end_matches('/')))
}

fn escape_error(path: &Path, why: &str) -> Error {
    Error::new(
        Code::FsPathEscapeBlocked,
        "Refused to write outside the workspace. If writing there is intended, add \
         that directory as its own workspace root.",
    )
    .with_context(format!("{} — {why}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    struct Sandbox {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        outside: PathBuf,
    }

    /// A workspace with a sibling directory outside it — the shape every
    /// escape test needs.
    fn sandbox() -> Sandbox {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = fs::canonicalize(tmp.path()).expect("canonicalize");
        let root = base.join("workspace");
        let outside = base.join("outside");
        fs::create_dir(&root).expect("root");
        fs::create_dir(&outside).expect("outside");
        Sandbox {
            _tmp: tmp,
            root,
            outside,
        }
    }

    fn ws(s: &Sandbox) -> Workspace {
        Workspace::open(&s.root).expect("open workspace")
    }

    /// A workspace that keeps what it replaces. The store sits beside the
    /// workspace rather than inside it — a snapshot under the root would be
    /// indexed, and would eventually be snapshotted itself.
    fn ws_undoable(s: &Sandbox) -> Workspace {
        let store = crate::snapshot::Snapshots::open(s.outside.join("snapshots"))
            .expect("open snapshot store");
        Workspace::open(&s.root)
            .expect("open workspace")
            .with_snapshots(store)
    }

    #[test]
    fn a_replacement_can_be_undone_and_gives_the_user_their_own_words_back() {
        let s = sandbox();
        let w = ws_undoable(&s);
        let original = b"the paragraph the user typed";

        let first = w.write("notes.md", original, &Expect::New).unwrap();
        let replaced = w
            .write(
                "notes.md",
                b"what the model wrote",
                &Expect::Replacing(first.digest()),
            )
            .unwrap();

        assert!(
            replaced.snapshot().is_some(),
            "the displaced bytes were kept"
        );
        assert!(replaced.is_undoable());
        assert_eq!(
            fs::read(s.root.join("notes.md")).unwrap(),
            b"what the model wrote"
        );

        let undone = w.undo(&replaced).expect("undo");
        assert!(!undone.removed());
        assert_eq!(fs::read(s.root.join("notes.md")).unwrap(), original);
        // The `origin = SELF` rule, from the other side: giving somebody their
        // file back and still calling it this system's output would quietly
        // cost them a citable source.
        assert_eq!(undone.origin(), Origin::User);
    }

    /// The case worth having a test for. An undo that discards a human edit is
    /// worse than the write it was undoing.
    #[test]
    fn an_undo_that_would_discard_a_later_human_edit_is_refused() {
        let s = sandbox();
        let w = ws_undoable(&s);

        let first = w.write("notes.md", b"original", &Expect::New).unwrap();
        let replaced = w
            .write(
                "notes.md",
                b"model output",
                &Expect::Replacing(first.digest()),
            )
            .unwrap();

        // The user opens it and types.
        fs::write(s.root.join("notes.md"), b"model output, then my edit").unwrap();

        let e = w.undo(&replaced).expect_err("must refuse");
        assert_eq!(e.code(), Code::ActStaleVersion);
        assert_eq!(
            fs::read(s.root.join("notes.md")).unwrap(),
            b"model output, then my edit",
            "the edit survives the refusal"
        );
    }

    #[test]
    fn undoing_a_creation_removes_the_file() {
        let s = sandbox();
        let w = ws_undoable(&s);
        let made = w
            .write("scratch/new.md", b"invented", &Expect::New)
            .unwrap();
        assert!(made.snapshot().is_none(), "nothing was displaced");
        assert!(made.is_undoable(), "a creation is always undoable");

        let undone = w.undo(&made).expect("undo");
        assert!(undone.removed());
        assert!(!s.root.join("scratch/new.md").exists());
    }

    #[test]
    fn undoing_a_creation_that_was_edited_since_leaves_it_alone() {
        let s = sandbox();
        let w = ws_undoable(&s);
        let made = w.write("draft.md", b"invented", &Expect::New).unwrap();
        fs::write(s.root.join("draft.md"), b"invented, then improved").unwrap();

        let e = w.undo(&made).expect_err("must refuse");
        assert_eq!(e.code(), Code::ActStaleVersion);
        assert!(s.root.join("draft.md").exists(), "and it is still there");
    }

    /// A workspace with no store can still replace, and must say that the
    /// replacement is final rather than pretending otherwise.
    #[test]
    fn a_replacement_with_nowhere_to_keep_the_old_bytes_says_it_cannot_be_undone() {
        let s = sandbox();
        let w = ws(&s);
        let first = w.write("notes.md", b"original", &Expect::New).unwrap();
        let replaced = w
            .write(
                "notes.md",
                b"replacement",
                &Expect::Replacing(first.digest()),
            )
            .unwrap();

        assert!(replaced.snapshot().is_none());
        assert!(!replaced.is_undoable(), "and it says so before being asked");

        let e = w.undo(&replaced).expect_err("must refuse");
        assert_eq!(e.code(), Code::ActNotReversible);
    }

    /// The bug the explicit [`Undo`] exists to prevent, pinned.
    ///
    /// When `undo_write` worked out "was this a replacement?" from whether a
    /// snapshot handle was supplied, a replacement whose bytes had not been
    /// kept was indistinguishable from a creation — so undoing it **deleted the
    /// user's file** instead of refusing. Over a wire, a caller that merely
    /// forgot the handle would do the same.
    #[test]
    fn an_undo_asked_for_by_value_never_deletes_a_file_it_was_asked_to_restore() {
        let s = sandbox();
        let w = ws_undoable(&s);
        let first = w
            .write("notes.md", b"the user's work", &Expect::New)
            .unwrap();
        let replaced = w
            .write(
                "notes.md",
                b"model output",
                &Expect::Replacing(first.digest()),
            )
            .unwrap();
        let handle = replaced.snapshot().cloned().expect("kept");

        // The restore that was asked for.
        let undone = w
            .undo_write(replaced.path(), replaced.digest(), Undo::Restore(handle))
            .expect("restores");
        assert!(!undone.removed());
        assert_eq!(
            fs::read(s.root.join("notes.md")).unwrap(),
            b"the user's work"
        );
    }

    /// And the other half: removal happens only when it is what was asked for.
    #[test]
    fn removal_is_only_ever_what_the_caller_explicitly_asked_for() {
        let s = sandbox();
        let w = ws_undoable(&s);
        let first = w
            .write("notes.md", b"the user's work", &Expect::New)
            .unwrap();
        let replaced = w
            .write(
                "notes.md",
                b"model output",
                &Expect::Replacing(first.digest()),
            )
            .unwrap();

        // A caller that asks to remove a file this write did not create gets
        // exactly what it asked for — which is why asking has to be deliberate,
        // and why `Undo` has no default.
        w.undo_write(replaced.path(), replaced.digest(), Undo::RemoveCreated)
            .expect("does what was asked");
        assert!(!s.root.join("notes.md").exists());
    }

    #[test]
    fn replacing_the_same_file_twice_keeps_both_earlier_states() {
        let s = sandbox();
        let w = ws_undoable(&s);
        let a = w.write("n.md", b"first", &Expect::New).unwrap();
        let b = w
            .write("n.md", b"second", &Expect::Replacing(a.digest()))
            .unwrap();
        let c = w
            .write("n.md", b"third", &Expect::Replacing(b.digest()))
            .unwrap();

        // Undo the last write, then the one before it: two steps back, each
        // through its own snapshot rather than through a stack.
        w.undo(&c).expect("undo the third");
        assert_eq!(fs::read(s.root.join("n.md")).unwrap(), b"second");
        w.undo(&b).expect("undo the second");
        assert_eq!(fs::read(s.root.join("n.md")).unwrap(), b"first");
    }

    /// An undo is a write, and gets no exemption from containment.
    #[test]
    fn an_undo_goes_through_the_same_guarded_path_as_the_write() {
        let s = sandbox();
        let w = ws_undoable(&s);
        let first = w.write("notes.md", b"original", &Expect::New).unwrap();
        let replaced = w
            .write(
                "notes.md",
                b"model output",
                &Expect::Replacing(first.digest()),
            )
            .unwrap();

        // The file becomes a symlink pointing out of the workspace between the
        // write and the undo — the escape the whole crate is shaped around.
        fs::remove_file(s.root.join("notes.md")).unwrap();
        symlink(s.outside.join("target.md"), s.root.join("notes.md")).unwrap();
        fs::write(s.outside.join("target.md"), b"outside").unwrap();

        let e = w.undo(&replaced).expect_err("must refuse");
        assert!(
            matches!(e.code(), Code::FsPathEscapeBlocked | Code::ActStaleVersion),
            "unexpected code {:?}",
            e.code()
        );
        assert_eq!(
            fs::read(s.outside.join("target.md")).unwrap(),
            b"outside",
            "nothing outside the workspace was touched"
        );
    }

    fn digest_of(p: &Path) -> ContentHash {
        ContentHash::of(&fs::read(p).expect("read"))
    }

    #[test]
    fn a_plain_write_lands_where_it_was_asked_to() {
        // The guard has to be usable. A gate that refuses everything is not a
        // gate, it is an outage.
        let s = sandbox();
        let w = ws(&s)
            .write("notes/summary.md", b"hello", &Expect::New)
            .unwrap();
        assert_eq!(w.path(), s.root.join("notes/summary.md"));
        assert_eq!(fs::read(w.path()).unwrap(), b"hello");
        assert_eq!(w.digest(), ContentHash::of(b"hello"));
        assert_eq!(w.bytes(), 5);
        assert_eq!(w.replaced(), None);
    }

    #[test]
    fn a_symlink_pointing_out_of_the_workspace_is_refused_not_followed() {
        // The invariant-#5 case verbatim: a symlink in a cloned repo pointing
        // at somewhere it has no business writing. A string prefix check on
        // `root/link/x.md` passes — that is the whole point.
        let s = sandbox();
        symlink(&s.outside, s.root.join("link")).unwrap();
        let e = ws(&s)
            .write("link/stolen.md", b"x", &Expect::New)
            .unwrap_err();
        assert_eq!(e.code(), Code::FsPathEscapeBlocked);
        assert!(
            !s.outside.join("stolen.md").exists(),
            "nothing may be written outside the workspace"
        );
    }

    #[test]
    fn a_symlink_that_stays_inside_the_workspace_is_still_refused_for_writing() {
        // Reading follows an internal symlink happily (`marrow_scan`), but
        // `rename` replaces the link instead of the file it points at, so a
        // "write" here destroys the user's link and leaves the real file stale.
        let s = sandbox();
        fs::write(s.root.join("real.md"), b"real").unwrap();
        symlink(s.root.join("real.md"), s.root.join("alias.md")).unwrap();
        let e = ws(&s).write("alias.md", b"x", &Expect::New).unwrap_err();
        assert_eq!(e.code(), Code::FsPathEscapeBlocked);
        assert_eq!(fs::read(s.root.join("real.md")).unwrap(), b"real");
    }

    #[test]
    fn a_symlink_created_after_validation_is_still_refused() {
        // TOCTOU. Validation saw a real directory; by the time the bytes are
        // written it is a symlink out of the workspace. Nothing before step 6
        // can catch this, which is why step 6 exists.
        let s = sandbox();
        fs::create_dir(s.root.join("notes")).unwrap();
        let victim = s.root.join("notes");
        let target_dir = s.outside.clone();
        let w = ws(&s).with_race(Box::new(move |_target| {
            fs::remove_dir_all(&victim).expect("remove the real directory");
            symlink(&target_dir, &victim).expect("swap in a symlink");
        }));

        let e = w
            .write("notes/summary.md", b"secret", &Expect::New)
            .unwrap_err();
        assert_eq!(e.code(), Code::FsPathEscapeBlocked);
        assert!(
            fs::read_dir(&s.outside).unwrap().next().is_none(),
            "not one byte, not even a temp file, may appear outside the workspace"
        );
    }

    #[test]
    fn a_target_that_becomes_a_symlink_after_validation_is_refused() {
        // The other half of the race: the *file*, not its directory, is swapped
        // between the check and the rename.
        let s = sandbox();
        fs::write(s.root.join("notes.md"), b"original").unwrap();
        fs::write(s.outside.join("secrets"), b"secrets").unwrap();
        let victim = s.root.join("notes.md");
        let elsewhere = s.outside.join("secrets");
        let w = ws(&s).with_race(Box::new(move |_| {
            fs::remove_file(&victim).unwrap();
            symlink(&elsewhere, &victim).unwrap();
        }));

        let e = w
            .write(
                "notes.md",
                b"x",
                &Expect::Replacing(ContentHash::of(b"original")),
            )
            .unwrap_err();
        assert_eq!(e.code(), Code::FsPathEscapeBlocked);
        assert_eq!(fs::read(s.outside.join("secrets")).unwrap(), b"secrets");
    }

    #[test]
    fn writing_into_an_excluded_directory_is_refused() {
        // `.git` is someone else's state machine. A file written into it is at
        // best noise and at worst a hook that runs on the next commit.
        let s = sandbox();
        fs::create_dir_all(s.root.join(".git/hooks")).unwrap();
        for bad in [".git/hooks/post-commit", ".git/config", "node_modules/x.md"] {
            let e = ws(&s).write(bad, b"x", &Expect::New).unwrap_err();
            assert_eq!(e.code(), Code::PolDenied, "`{bad}`");
        }
        assert!(!s.root.join(".git/hooks/post-commit").exists());
    }

    #[test]
    fn writing_into_the_model_directory_is_refused() {
        // The model area normally sits outside every root, where containment
        // already refuses it. This is the misconfigured case — and an agent
        // that can edit the weights it is running on is a different product.
        let s = sandbox();
        fs::create_dir(s.root.join("models")).unwrap();
        let w = ws(&s).protect("models");
        let e = w
            .write("models/weights/note.md", b"x", &Expect::New)
            .unwrap_err();
        assert_eq!(e.code(), Code::PolDenied);
        assert!(
            !s.root.join("models/weights").exists(),
            "a refused write must not leave directories behind inside a protected tree"
        );
    }

    #[test]
    fn a_protected_directory_does_not_protect_its_prefix_sibling() {
        // `models-notes` is not inside `models`. The string-prefix bug, from
        // the other direction: over-refusing is also a defect.
        let s = sandbox();
        fs::create_dir(s.root.join("models")).unwrap();
        let w = ws(&s).protect("models");
        assert!(w.write("models-notes/x.md", b"x", &Expect::New).is_ok());
    }

    #[test]
    fn replacing_a_file_that_changed_since_it_was_read_is_refused() {
        // The stale-version rule. The user has the file open in their editor.
        let s = sandbox();
        let p = s.root.join("notes.md");
        fs::write(&p, b"as read").unwrap();
        let stale = ContentHash::of(b"what the caller thinks is there");
        let e = ws(&s)
            .write("notes.md", b"new", &Expect::Replacing(stale))
            .unwrap_err();
        assert_eq!(e.code(), Code::ActStaleVersion);
        assert!(e.message().contains("changed since"), "{}", e.message());
        assert_eq!(
            fs::read(&p).unwrap(),
            b"as read",
            "the file must be untouched"
        );
    }

    #[test]
    fn the_stale_check_runs_at_commit_time_not_at_validation_time() {
        // The reason step 7 is last. The caller's digest was correct when it
        // was read; the file changed while the content was being prepared.
        let s = sandbox();
        let p = s.root.join("notes.md");
        fs::write(&p, b"as read").unwrap();
        let victim = p.clone();
        let w = ws(&s).with_race(Box::new(move |_| {
            fs::write(&victim, b"edited in another window").unwrap();
        }));

        let e = w
            .write(
                "notes.md",
                b"new",
                &Expect::Replacing(ContentHash::of(b"as read")),
            )
            .unwrap_err();
        assert_eq!(e.code(), Code::ActStaleVersion);
        assert_eq!(fs::read(&p).unwrap(), b"edited in another window");
    }

    #[test]
    fn replacing_a_file_with_the_digest_it_actually_has_succeeds() {
        let s = sandbox();
        let p = s.root.join("notes.md");
        fs::write(&p, b"as read").unwrap();
        let w = ws(&s)
            .write("notes.md", b"new", &Expect::Replacing(digest_of(&p)))
            .unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"new");
        assert_eq!(w.replaced(), Some(ContentHash::of(b"as read")));
    }

    #[test]
    fn creating_a_file_that_already_exists_is_refused_rather_than_overwritten() {
        // `Expect::New` is a claim about the world, not a formality.
        let s = sandbox();
        fs::write(s.root.join("notes.md"), b"mine").unwrap();
        let e = ws(&s)
            .write("notes.md", b"theirs", &Expect::New)
            .unwrap_err();
        assert_eq!(e.code(), Code::ActAlreadyExists);
        assert_eq!(fs::read(s.root.join("notes.md")).unwrap(), b"mine");
    }

    #[test]
    fn replacing_a_file_that_is_not_there_is_not_silently_a_create() {
        let s = sandbox();
        let e = ws(&s)
            .write("gone.md", b"x", &Expect::Replacing(ContentHash::of(b"x")))
            .unwrap_err();
        assert_eq!(e.code(), Code::FsNotFound);
    }

    #[test]
    fn a_name_differing_only_by_normalisation_is_refused_not_merged() {
        // The Unicode NFC/NFD rule (§126 #14). macOS stores NFD; everything
        // else hands you NFC. Without
        // this, one file quietly acquires two identities — or one write
        // clobbers a file the caller did not name.
        let s = sandbox();
        fs::write(s.root.join("cafe\u{301}.md"), b"nfd on disk").unwrap();
        let e = ws(&s)
            .write("caf\u{e9}.md", b"nfc from the caller", &Expect::New)
            .unwrap_err();
        assert_eq!(e.code(), Code::ActAlreadyExists);
        assert!(e.message().contains("normalisation"), "{}", e.message());
    }

    #[test]
    fn a_name_differing_only_by_case_is_refused_not_merged() {
        // APFS is case-insensitive by default: `NOTES.md` and `notes.md` are
        // one file here and two on a case-sensitive volume. Neither answer is
        // one a caller should get by accident.
        let s = sandbox();
        fs::write(s.root.join("notes.md"), b"mine").unwrap();
        let e = ws(&s)
            .write("NOTES.md", b"theirs", &Expect::New)
            .unwrap_err();
        assert_eq!(e.code(), Code::ActAlreadyExists);
        assert_eq!(fs::read(s.root.join("notes.md")).unwrap(), b"mine");
    }

    #[test]
    fn a_cloud_placeholder_is_never_read_to_satisfy_the_stale_check() {
        // Never hydrate a placeholder. Digesting the file being replaced is a *read*, and on a
        // dehydrated file a read is a download. The stub name is the form
        // iCloud leaves behind and the one that can be created in a test.
        let s = sandbox();
        let stub = s.root.join(".report.md.icloud");
        fs::write(&stub, b"stub").unwrap();
        let e = ws(&s)
            .write(
                ".report.md.icloud",
                b"x",
                &Expect::Replacing(ContentHash::of(b"stub")),
            )
            .unwrap_err();
        assert_eq!(e.code(), Code::FsPlaceholderSkipped);
    }

    #[test]
    fn a_refused_write_leaves_no_temp_file_behind() {
        // Every early return after the temp file exists is covered by `Drop`;
        // a leaked `.tmp` in a watched folder becomes an indexed document.
        let s = sandbox();
        fs::write(s.root.join("notes.md"), b"as read").unwrap();
        let _ = ws(&s).write(
            "notes.md",
            b"new",
            &Expect::Replacing(ContentHash::of(b"wrong")),
        );
        let leftovers: Vec<_> = fs::read_dir(&s.root)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
    }

    #[test]
    fn the_write_is_atomic_so_a_crash_cannot_leave_half_a_file() {
        // Proved structurally: the destination is only ever touched by
        // `rename`, and the bytes are fsynced before it. What is observable
        // from a test is that the temp file lives in the destination directory
        // — a rename across filesystems is a copy, and a copy has a window.
        let s = sandbox();
        let dir = s.root.join("notes");
        fs::create_dir(&dir).unwrap();
        let temp = TempFile::create(&dir).unwrap();
        assert_eq!(temp.path().parent(), Some(dir.as_path()));
        assert!(
            temp.path()
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.')),
            "the temp file must be hidden from the indexer's walk"
        );
        let p = temp.path().to_path_buf();
        drop(temp);
        assert!(!p.exists(), "an uncommitted temp file must not survive");
    }

    #[test]
    fn everything_written_is_marked_self_written_and_cannot_be_cited() {
        // `origin = SELF`. If the agent's own summary can support a claim, the
        // system cites itself back as independent corroboration.
        let s = sandbox();
        let w = ws(&s)
            .write("summary.md", b"the answer is 42", &Expect::New)
            .unwrap();
        assert_eq!(w.origin(), Origin::SelfWritten);
        assert!(!w.can_support_a_claim());
    }

    #[test]
    fn a_workspace_root_that_is_not_a_directory_is_refused() {
        let s = sandbox();
        let f = s.root.join("f.md");
        fs::write(&f, b"x").unwrap();
        assert_eq!(Workspace::open(&f).unwrap_err().code(), Code::CfgInvalid);
    }

    #[test]
    fn under_compares_components_not_string_prefixes() {
        // The bug this function exists to not have.
        assert!(under(Path::new("/a/root/x"), Path::new("/a/root")));
        assert!(under(Path::new("/a/root"), Path::new("/a/root")));
        assert!(!under(Path::new("/a/root-evil/x"), Path::new("/a/root")));
        assert!(!under(Path::new("/a"), Path::new("/a/root")));
    }
}
