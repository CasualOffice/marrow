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
//! write. Invariant #5 says *at operation time*: steps 6 and 7 are the same
//! questions asked again with nothing in between them and the `rename`.
//!
//! **Why the stale check is last.** Invariant #6 is "immediately before any
//! write". Digesting the file first and then spending 20 ms building content
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

/// What the caller believes it is replacing. **Invariant #6.**
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

/// What a completed write did. **Invariant #9** is carried in the type: there
/// is no constructor that produces anything but [`Origin::SelfWritten`].
#[derive(Clone, Debug, Serialize)]
pub struct Written {
    path: PathBuf,
    digest: ContentHash,
    bytes: u64,
    origin: Origin,
    replaced: Option<ContentHash>,
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
    race: Option<RaceHook>,
}

impl fmt::Debug for Workspace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Workspace")
            .field("root", &self.root.path())
            .field("excluded", &self.excluded)
            .field("protected", &self.protected)
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
            // Invariant #9. Not a parameter, so no caller can weaken it.
            origin: Origin::SelfWritten,
            replaced,
            written_at: Timestamp::now(),
        })
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

    /// **§126 #14.** A name that differs from an existing one only by
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

    /// **Invariant #6**, run with nothing between it and the rename.
    ///
    /// Returns the digest of what is being replaced, so the caller can record
    /// what it overwrote.
    fn check_precondition(&self, target: &Path, expect: &Expect) -> Result<Option<ContentHash>> {
        let present = fs::symlink_metadata(target);

        // A symlink here means one appeared after `plan` refused symlinks.
        // That is a race, not a naming mistake, and it is the case invariant #5
        // exists for.
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
                // `hash_file` refuses a cloud placeholder before opening it
                // (invariant #3) — reading one here to compare digests would
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
/// `café` are one name (invariant #8), lowercased so `Notes` and `notes` are
/// one name on the case-insensitive volume this runs on by default.
fn fold(s: &str) -> String {
    s.nfc().collect::<String>().to_lowercase()
}

/// Whether `path` is `ancestor` or lies below it.
///
/// Compares NFC path strings with an explicit separator appended, so
/// `/root-evil` is not "inside" `/root` — the string-prefix bug invariant #5
/// names by hand.
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
        // Invariant #6. The user has the file open in their editor.
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
        // §126 #14. macOS stores NFD; everything else hands you NFC. Without
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
        // Invariant #3. Digesting the file being replaced is a *read*, and on a
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
        // Invariant #9. If the agent's own summary can support a claim, the
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
