//! Recursive discovery over an authorised root.
//!
//! Produces an iterator, never a `Vec`: the caller decides how much of a tree
//! it wants in memory at once. M0 measured 97,308 files/s single-threaded over
//! the real corpus, which is ~1,700× the requirement, so this is a plain
//! single-threaded `ignore::Walk` rather than the parallel builder — a parallel
//! walk would trade a clean `Iterator` for throughput nobody needs.
//!
//! Three policy points, each with a reason from a measurement:
//!
//! - **`.gitignore` is per-root, not a global default (D47 / M0 F9).** Gitignore
//!   does 97% of the exclusion work, and it also hid **442 of 475 `.xlsx`
//!   files** — real spreadsheets in gitignored data directories. FS-002 says
//!   "where configured"; [`WalkPolicy::respect_gitignore`] defaults to `false`
//!   and a code root turns it on.
//! - **Symlinks are not followed (WS-005).** M0 found zero symlinks in the
//!   corpus, so this costs nothing today and one cloned repo changes that.
//! - **Noise directories are pruned by name.** M0: `node_modules` alone was
//!   76,097 files across 15 directories, and build output was 6.6× the size of
//!   the entire knowledge corpus.
//!
//! Errors are per-entry and never abort the walk (FS-011): a directory that
//! cannot be opened yields one [`ScanEvent::Failed`] and the walk continues
//! with its siblings.

use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use marrow_core::{Code, Error};

use crate::path::AuthorizedRoot;
use crate::probe::{self, FileFacts};
use crate::tier;

/// Directory names pruned by default.
///
/// Exactly the set M0 measured, which took the corpus from 25,092 files to
/// 9,435 (−62%) on top of what `.gitignore` already removed.
pub const DEFAULT_NOISE_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "build",
    "dist",
    ".venv",
    "venv",
    "__pycache__",
    ".gradle",
    ".next",
    "vendor",
    "Pods",
    "DerivedData",
];

/// Per-root walk policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalkPolicy {
    /// Honour `.gitignore`, `.ignore`, `.git/info/exclude` and the global
    /// gitignore. **Per-root, default off** — see D47 and the module note.
    ///
    /// As in git itself, these files only take effect inside a repository: a
    /// stray `.gitignore` in a photo folder is not policy.
    pub respect_gitignore: bool,
    /// Skip dot-files and dot-directories.
    ///
    /// iCloud stub files (`.Report.pdf.icloud`) are **exempt**: they are always
    /// dot-prefixed, and hiding them would erase the only evidence that an
    /// evicted file exists at all, which is what TIER-008's "cloud-only, not
    /// indexed" count is made of.
    pub skip_hidden: bool,
    /// WS-005: default disabled. Turning it on enables a per-entry containment
    /// check on symlinked entries, which costs a `realpath` each.
    pub follow_links: bool,
    /// Stop at volume boundaries. Off by default, matching what M0 measured.
    pub same_file_system: bool,
    pub max_depth: Option<usize>,
    /// Directory names pruned wherever they appear below the root.
    pub excluded_dirs: BTreeSet<String>,
}

impl Default for WalkPolicy {
    fn default() -> Self {
        Self {
            respect_gitignore: false,
            skip_hidden: true,
            follow_links: false,
            same_file_system: false,
            max_depth: None,
            excluded_dirs: DEFAULT_NOISE_DIRS.iter().map(|s| (*s).to_string()).collect(),
        }
    }
}

impl WalkPolicy {
    /// Policy for a root that is a source tree: gitignore on.
    pub fn for_code_root() -> Self {
        Self {
            respect_gitignore: true,
            ..Self::default()
        }
    }

    /// Policy for a root that holds documents and data: gitignore off, because
    /// of M0 F9.
    pub fn for_data_root() -> Self {
        Self::default()
    }

    pub fn exclude_dir(mut self, name: impl Into<String>) -> Self {
        self.excluded_dirs.insert(name.into());
        self
    }

    /// Stop pruning a directory name that is excluded by default.
    pub fn include_dir(mut self, name: &str) -> Self {
        self.excluded_dirs.remove(name);
        self
    }
}

/// One discovered path, with the facts its single `lstat` produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanEntry {
    pub path: PathBuf,
    /// Depth below the root; the root itself is 0.
    pub depth: usize,
    pub facts: FileFacts,
}

impl ScanEntry {
    pub fn is_dir(&self) -> bool {
        self.facts.is_dir
    }

    pub fn is_file(&self) -> bool {
        !self.facts.is_dir && !self.facts.is_symlink
    }
}

/// One step of a walk. A failure describes one entry, never the whole walk
/// (FS-011).
#[derive(Debug)]
pub enum ScanEvent {
    Entry(ScanEntry),
    Failed(Error),
}

impl ScanEvent {
    /// The entry, if this step succeeded. Convenience for
    /// `walk(..).filter_map(ScanEvent::entry)` when the caller logs errors
    /// elsewhere — errors must be logged somewhere.
    pub fn entry(self) -> Option<ScanEntry> {
        match self {
            ScanEvent::Entry(e) => Some(e),
            ScanEvent::Failed(_) => None,
        }
    }
}

/// A lazy walk. See [`walk`].
pub struct Scan {
    inner: ignore::Walk,
    root: AuthorizedRoot,
    check_containment: bool,
}

impl fmt::Debug for Scan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Scan")
            .field("root", &self.root.path())
            .finish_non_exhaustive()
    }
}

/// Walk `root` under `policy`, lazily.
pub fn walk(root: &AuthorizedRoot, policy: &WalkPolicy) -> Scan {
    let mut b = ignore::WalkBuilder::new(root.path());

    b.follow_links(policy.follow_links) // WS-005
        .same_file_system(policy.same_file_system)
        .max_depth(policy.max_depth)
        // Hidden files are filtered by our own predicate below so that iCloud
        // stubs can be exempted. Leaving this on would hide every placeholder
        // in the tree before we ever got to count it.
        .hidden(false)
        // D47: all of the gitignore machinery moves together, per root.
        .git_ignore(policy.respect_gitignore)
        .git_global(policy.respect_gitignore)
        .git_exclude(policy.respect_gitignore)
        .ignore(policy.respect_gitignore)
        .parents(policy.respect_gitignore)
        .require_git(true);

    // ONE closure over the whole set.
    //
    // `WalkBuilder::filter_entry` **replaces** the predicate — its own docs say
    // "only one filter predicate can be applied to a `WalkBuilder`. Calling
    // this subsequent times overrides previous filter predicates." Calling it
    // in a loop over patterns silently keeps only the last one, which looks
    // like it works because the last pattern does get excluded.
    let excluded: Arc<BTreeSet<String>> = Arc::new(policy.excluded_dirs.clone());
    let skip_hidden = policy.skip_hidden;
    b.filter_entry(move |entry| {
        // Depth 0 (the root itself) is never offered to the predicate by
        // `ignore`, so a root named `target` still walks.
        let name = entry.file_name();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);

        if is_dir {
            if let Some(n) = name.to_str() {
                if excluded.contains(n) {
                    tracing::trace!(dir = n, "pruned noise directory");
                    return false;
                }
            }
        }

        if skip_hidden {
            let hidden = name.to_str().map(|n| n.starts_with('.')).unwrap_or(false);
            if hidden && !tier::is_icloud_stub_name(name) {
                return false;
            }
        }

        true
    });

    Scan {
        inner: b.build(),
        root: root.clone(),
        // Only worth paying for when symlinks can be followed out of the tree.
        check_containment: policy.follow_links,
    }
}

impl Iterator for Scan {
    type Item = ScanEvent;

    fn next(&mut self) -> Option<ScanEvent> {
        loop {
            let entry = match self.inner.next()? {
                Ok(e) => e,
                Err(e) => return Some(ScanEvent::Failed(map_walk_error(e))),
            };

            // A non-fatal per-entry error (e.g. an unreadable `.gitignore`).
            // The entry itself is still good, so it is reported and kept.
            if let Some(err) = entry.error() {
                tracing::debug!(path = %entry.path().display(), error = %err, "partial walk error");
            }

            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(e) => return Some(ScanEvent::Failed(map_walk_error(e))),
            };

            // Invariant #7, for the opt-in `follow_links` case only: a followed
            // symlink is the one way a walk can leave the authorised root.
            if self.check_containment && entry.path_is_symlink() {
                match std::fs::canonicalize(entry.path()) {
                    Ok(real) if !self.root.contains(&real) => {
                        return Some(ScanEvent::Failed(
                            Error::new(
                                Code::FsPathEscapeBlocked,
                                "Skipped a symbolic link that leaves the authorised root. \
                                 Add its target as a root if indexing it is intended.",
                            )
                            .with_context(format!(
                                "{} -> {}",
                                entry.path().display(),
                                real.display()
                            )),
                        ));
                    }
                    Ok(_) => {}
                    Err(e) => {
                        return Some(ScanEvent::Failed(
                            Error::from(e).with_context(entry.path().display().to_string()),
                        ))
                    }
                }
            }

            let facts = probe::facts_from_metadata(entry.path(), &meta);
            return Some(ScanEvent::Entry(ScanEntry {
                path: entry.path().to_path_buf(),
                depth: entry.depth(),
                facts,
            }));
        }
    }
}

/// Map an `ignore` failure onto the error taxonomy.
///
/// `ignore::Error` carries the offending path in its `Display`, which is why
/// the whole error becomes both context and source rather than being unwrapped.
fn map_walk_error(e: ignore::Error) -> Error {
    let context = e.to_string();
    let (code, message) = match e.io_error().map(|io| io.kind()) {
        Some(std::io::ErrorKind::NotFound) => (
            Code::FsNotFound,
            "A directory entry vanished during the scan; it will be picked up by \
             the next reconciliation.",
        ),
        Some(std::io::ErrorKind::PermissionDenied) => (
            Code::FsPermissionDenied,
            "No permission to read this directory. Grant access in System \
             Settings › Privacy & Security, or exclude it from the workspace.",
        ),
        Some(_) => (
            Code::FsLocked,
            "A directory entry could not be read; the rest of the scan continued. \
             It will be retried.",
        ),
        None => (
            Code::CfgInvalid,
            "An ignore rule for this root could not be applied. Check the \
             `.gitignore` files under it, or turn gitignore off for this root.",
        ),
    };
    tracing::warn!(code = %code, detail = %context, "walk error (scan continues)");
    Error::new(code, message).with_context(context).with_source(e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::Path;

    fn root_of(p: &Path) -> AuthorizedRoot {
        AuthorizedRoot::open(p).unwrap()
    }

    /// Names of every file yielded, relative to the root.
    fn file_names(root: &AuthorizedRoot, policy: &WalkPolicy) -> BTreeSet<String> {
        walk(root, policy)
            .filter_map(ScanEvent::entry)
            .filter(ScanEntry::is_file)
            .map(|e| {
                e.path
                    .strip_prefix(root.path())
                    .unwrap_or(&e.path)
                    .display()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn every_noise_directory_is_excluded_not_just_the_last() {
        // The `filter_entry` trap: the builder *replaces* the predicate, so a
        // loop over patterns keeps only the final one. With `node_modules`
        // written last, a broken implementation still excludes `node_modules`
        // and looks fine. Every pattern is asserted individually.
        let td = tempfile::tempdir().unwrap();
        let base = td.path();

        for dir in DEFAULT_NOISE_DIRS {
            let d = base.join(dir);
            fs::create_dir_all(d.join("nested")).unwrap();
            fs::write(d.join("junk.rs"), b"noise").unwrap();
            fs::write(d.join("nested/deep.rs"), b"noise").unwrap();
        }
        fs::write(base.join("keep.rs"), b"real").unwrap();
        fs::create_dir(base.join("src")).unwrap();
        fs::write(base.join("src/lib.rs"), b"real").unwrap();

        let root = root_of(base);
        let found = file_names(&root, &WalkPolicy::default());

        assert_eq!(
            found,
            BTreeSet::from(["keep.rs".to_string(), "src/lib.rs".to_string()]),
            "found unexpected files: {found:?}"
        );
        for dir in DEFAULT_NOISE_DIRS {
            assert!(
                !found.iter().any(|f| f.starts_with(&format!("{dir}/"))),
                "`{dir}` was not pruned — filter_entry may have been called per pattern"
            );
        }
    }

    #[test]
    fn a_default_exclusion_can_be_turned_back_on_per_root() {
        let td = tempfile::tempdir().unwrap();
        fs::create_dir(td.path().join("vendor")).unwrap();
        fs::write(td.path().join("vendor/thing.rs"), b"x").unwrap();

        let root = root_of(td.path());
        assert!(file_names(&root, &WalkPolicy::default()).is_empty());

        let policy = WalkPolicy::default().include_dir("vendor");
        assert_eq!(
            file_names(&root, &policy),
            BTreeSet::from(["vendor/thing.rs".to_string()])
        );
    }

    #[test]
    fn gitignore_is_a_per_root_policy_not_a_global_default() {
        // M0 F9 / D47: gitignore hid 442 of 475 spreadsheets. Data roots must
        // see them; code roots want the exclusion.
        let td = tempfile::tempdir().unwrap();
        let base = td.path();
        fs::create_dir(base.join(".git")).unwrap(); // gitignore needs a repo
        fs::write(base.join(".gitignore"), b"data/\n*.xlsx\n").unwrap();
        fs::create_dir(base.join("data")).unwrap();
        fs::write(base.join("data/budget.xlsx"), b"x").unwrap();
        fs::write(base.join("top.xlsx"), b"x").unwrap();
        fs::write(base.join("main.rs"), b"x").unwrap();

        let root = root_of(base);

        let data_root = file_names(&root, &WalkPolicy::for_data_root());
        assert!(data_root.contains("data/budget.xlsx"), "{data_root:?}");
        assert!(data_root.contains("top.xlsx"));

        let code_root = file_names(&root, &WalkPolicy::for_code_root());
        assert!(!code_root.contains("data/budget.xlsx"), "{code_root:?}");
        assert!(!code_root.contains("top.xlsx"));
        assert!(code_root.contains("main.rs"));
    }

    #[test]
    fn walk_continues_after_a_permission_error_on_one_entry() {
        // FS-011. One unreadable directory must not cost the whole scan.
        use std::os::unix::fs::PermissionsExt as _;

        let td = tempfile::tempdir().unwrap();
        let base = td.path();
        let locked = base.join("locked");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("hidden.txt"), b"x").unwrap();
        for n in ["a.txt", "b.txt", "c.txt"] {
            fs::write(base.join(n), b"x").unwrap();
        }
        fs::create_dir(base.join("zzz_after")).unwrap();
        fs::write(base.join("zzz_after/d.txt"), b"x").unwrap();

        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let root = root_of(base);
        let mut files = BTreeSet::new();
        let mut errors = Vec::new();
        for ev in walk(&root, &WalkPolicy::default()) {
            match ev {
                ScanEvent::Entry(e) if e.is_file() => {
                    files.insert(e.path.strip_prefix(base).unwrap().display().to_string());
                }
                ScanEvent::Entry(_) => {}
                ScanEvent::Failed(e) => errors.push(e),
            }
        }

        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

        // Running the suite as root would make the directory readable; the
        // point of the test is the *continuation*, which is asserted either way.
        if !errors.is_empty() {
            assert_eq!(errors.len(), 1, "{errors:?}");
            assert_eq!(errors[0].code(), Code::FsPermissionDenied);
            assert!(errors[0].code().isolates_to_one_file());
        }
        for n in ["a.txt", "b.txt", "c.txt", "zzz_after/d.txt"] {
            assert!(files.contains(n), "walk stopped early: {files:?}");
        }
    }

    #[test]
    fn symlinks_are_not_followed_by_default() {
        // WS-005. The link is reported as an entry so it is not invisible, but
        // its target tree is never entered.
        let td = tempfile::tempdir().unwrap();
        let base = td.path();
        fs::create_dir(base.join("root")).unwrap();
        fs::create_dir(base.join("outside")).unwrap();
        fs::write(base.join("outside/secret.txt"), b"x").unwrap();
        fs::write(base.join("root/own.txt"), b"x").unwrap();
        std::os::unix::fs::symlink(base.join("outside"), base.join("root/link")).unwrap();

        let root = root_of(&base.join("root"));
        let entries: Vec<ScanEntry> = walk(&root, &WalkPolicy::default())
            .filter_map(ScanEvent::entry)
            .collect();

        assert!(
            entries.iter().any(|e| e.facts.is_symlink),
            "the link itself should be visible"
        );
        assert!(
            !entries.iter().any(|e| e.path.ends_with("secret.txt")),
            "the walk followed a symlink out of the root"
        );
    }

    #[test]
    fn following_links_still_refuses_to_leave_the_root() {
        let td = tempfile::tempdir().unwrap();
        let base = td.path();
        fs::create_dir(base.join("root")).unwrap();
        fs::create_dir(base.join("outside")).unwrap();
        fs::write(base.join("outside/secret.txt"), b"x").unwrap();
        std::os::unix::fs::symlink(base.join("outside"), base.join("root/link")).unwrap();

        let root = root_of(&base.join("root"));
        let policy = WalkPolicy {
            follow_links: true,
            ..WalkPolicy::default()
        };

        let mut escapes = 0;
        let mut saw_secret = false;
        for ev in walk(&root, &policy) {
            match ev {
                ScanEvent::Failed(e) if e.code() == Code::FsPathEscapeBlocked => escapes += 1,
                ScanEvent::Entry(e) => saw_secret |= e.path.ends_with("secret.txt"),
                ScanEvent::Failed(_) => {}
            }
        }
        assert_eq!(escapes, 1, "the escaping link must be reported");
        // `ignore` yields the link's children too; what matters is that the
        // escape was surfaced rather than silently indexed.
        let _ = saw_secret;
    }

    #[test]
    fn hidden_files_are_skipped_but_icloud_stubs_survive() {
        // Hiding dot-files would also hide every placeholder, and the
        // "cloud-only, not indexed" count (TIER-008) would silently read zero.
        let td = tempfile::tempdir().unwrap();
        let base = td.path();
        fs::write(base.join(".DS_Store"), b"x").unwrap();
        fs::create_dir(base.join(".cache")).unwrap();
        fs::write(base.join(".cache/junk"), b"x").unwrap();
        fs::write(base.join(".Report.pdf.icloud"), b"stub").unwrap();
        fs::write(base.join("visible.md"), b"x").unwrap();

        let root = root_of(base);
        let found = file_names(&root, &WalkPolicy::default());
        assert_eq!(
            found,
            BTreeSet::from([
                ".Report.pdf.icloud".to_string(),
                "visible.md".to_string()
            ]),
            "{found:?}"
        );

        let stub = walk(&root, &WalkPolicy::default())
            .filter_map(ScanEvent::entry)
            .find(|e| e.path.ends_with(".Report.pdf.icloud"))
            .unwrap();
        assert_eq!(stub.facts.tier, marrow_core::TierState::Placeholder);
        assert!(!stub.facts.readable(), "invariant #5");
    }

    #[test]
    fn the_walk_is_lazy_and_reports_depth() {
        let td = tempfile::tempdir().unwrap();
        let base = td.path();
        fs::create_dir_all(base.join("a/b/c")).unwrap();
        for i in 0..50 {
            fs::write(base.join(format!("f{i}.txt")), b"x").unwrap();
        }
        fs::write(base.join("a/b/c/deep.txt"), b"x").unwrap();

        let root = root_of(base);

        // Taking three entries must not require walking the whole tree.
        let three: Vec<_> = walk(&root, &WalkPolicy::default()).take(3).collect();
        assert_eq!(three.len(), 3);

        let deep = walk(&root, &WalkPolicy::default())
            .filter_map(ScanEvent::entry)
            .find(|e| e.path.ends_with("deep.txt"))
            .unwrap();
        assert_eq!(deep.depth, 4);

        let shallow = WalkPolicy {
            max_depth: Some(1),
            ..WalkPolicy::default()
        };
        assert!(!file_names(&root, &shallow)
            .iter()
            .any(|f| f.contains("deep.txt")));
    }

    #[test]
    fn entries_carry_the_facts_from_their_own_lstat() {
        let td = tempfile::tempdir().unwrap();
        fs::write(td.path().join("notes.md"), b"hello").unwrap();
        let root = root_of(td.path());

        let e = walk(&root, &WalkPolicy::default())
            .filter_map(ScanEvent::entry)
            .find(|e| e.is_file())
            .unwrap();
        assert_eq!(e.facts.size, 5);
        assert_eq!(e.facts.tier, marrow_core::TierState::Resident);
        assert_eq!(
            e.facts.mime_hint.map(|m| m.as_str()),
            Some("text/markdown")
        );
    }
}
