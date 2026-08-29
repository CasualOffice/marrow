//! Path safety and path identity. **Invariants #7 and #8.**
//!
//! Two separate jobs live here and they must not be confused:
//!
//! 1. **Containment** ([`AuthorizedRoot`]): a path is only touchable if, *after
//!    canonicalisation*, it is inside a root the user consented to. String
//!    prefix comparison is not sufficient — `/safe/root-evil` starts with
//!    `/safe/root` and is a different tree. Comparison is per **component**.
//! 2. **Identity of a path string** ([`PathKey`]): macOS stores filenames in
//!    NFD, but an app, an archive or a network share can hand you the NFC form
//!    of the same name. Without normalising, the same file gets two identities.
//!    Invariant #8 calls this a correctness bug, not a locale feature.
//!
//! [`PathKey`] is **not** a file identity. Invariant #2 — path is never
//! identity — still holds: `PathKey` exists to compare and de-duplicate *paths*
//! (was this the same path we saw before?), never to key derived data. That is
//! what `marrow_core::FileId` is for.
//!
//! Canonicalisation resolves symlinks, so symlink escape falls out of the
//! containment check for free. The check must be re-run **at operation time**
//! ([`SafePath::reverify`]), not only at index time: the path that was inside
//! the root when it was discovered can be a symlink to `~/.ssh` by the time it
//! is read.

use std::ffi::OsStr;
use std::fmt;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use marrow_core::{Code, Error, Result};
use unicode_normalization::UnicodeNormalization;

/// NFC-normalise a string. Idempotent; leaves ASCII untouched and unallocated
/// in the common case (`nfc()` is lazy, the `collect` is the only cost).
fn nfc(s: &str) -> String {
    s.nfc().collect()
}

/// Comparison key for one path component.
///
/// Valid UTF-8 is NFC-normalised so the NFD form macOS stores on disk and the
/// NFC form everything else produces compare equal. Invalid UTF-8 (rare on
/// APFS, which enforces UTF-8, but possible on an attached FAT/exFAT volume)
/// falls back to the raw bytes rather than being lost to lossy conversion —
/// two different invalid names must not collapse into the same component.
fn component_key(c: &OsStr) -> Vec<u8> {
    match c.to_str() {
        Some(s) => nfc(s).into_bytes(),
        None => c.as_bytes().to_vec(),
    }
}

/// A path string reduced to a single normalised form.
///
/// Equal keys mean "the same path spelled differently", nothing more. See the
/// module note on why this is not a file identity.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PathKey(String);

impl PathKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PathKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for PathKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PathKey({})", self.0)
    }
}

/// NFC-normalised identity of a path string. **Invariant #8.**
///
/// Deliberately *not* case-folded. APFS is case-insensitive by default, so
/// `Foo.txt` and `foo.txt` are usually one file — but the same volume can be
/// created case-sensitive, and an attached volume very often is. Folding case
/// here would merge two genuinely distinct files on those volumes, which is the
/// worse failure. Case is resolved by `canonicalize`, which returns the name as
/// stored, so a canonical path already has one spelling.
pub fn path_key(p: &Path) -> Result<PathKey> {
    let s = p.to_str().ok_or_else(|| {
        Error::new(
            Code::FsNotUtf8Path,
            "Path is not valid UTF-8, so it cannot be given a stable identity. \
             Exclude this file, or move it to a volume that stores UTF-8 names.",
        )
        .with_context(p.to_string_lossy().into_owned())
    })?;
    Ok(PathKey(nfc(s)))
}

/// A directory tree the user has consented to. Every filesystem operation is
/// resolved against one of these.
#[derive(Clone, Debug)]
pub struct AuthorizedRoot {
    canonical: PathBuf,
    /// Precomputed so containment does not renormalise the root per candidate;
    /// a scan resolves this against tens of thousands of paths.
    key_components: Vec<Vec<u8>>,
}

impl AuthorizedRoot {
    /// Canonicalise `path` and adopt it as a root.
    ///
    /// Fails if the path does not exist or is not a directory — an unauthorised
    /// root is better than a root that silently means the wrong tree.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let canonical = std::fs::canonicalize(path)
            .map_err(|e| Error::from(e).with_context(format!("root {}", path.display())))?;
        // `canonicalize` follows symlinks, so this is the real directory.
        let meta = std::fs::metadata(&canonical)?;
        if !meta.is_dir() {
            return Err(Error::new(
                Code::CfgInvalid,
                "A workspace root must be a directory. Point the root at the \
                 containing folder instead of the file.",
            )
            .with_context(canonical.display().to_string()));
        }
        let key_components = canonical
            .components()
            .map(|c| component_key(c.as_os_str()))
            .collect();
        Ok(Self {
            canonical,
            key_components,
        })
    }

    /// The canonical root directory.
    pub fn path(&self) -> &Path {
        &self.canonical
    }

    /// Whether an **already canonical** path lies inside this root.
    ///
    /// Component-wise, never a string prefix: `/safe/root-evil` is not inside
    /// `/safe/root`. Each component is NFC-normalised first (invariant #8) so
    /// the NFD form of a root name still matches.
    ///
    /// A path equal to the root counts as inside.
    pub fn contains(&self, canonical_candidate: &Path) -> bool {
        let mut have = self.key_components.iter();
        let mut want = canonical_candidate.components();
        loop {
            match (have.next(), want.next()) {
                // Root exhausted: everything left is below it.
                (None, _) => return true,
                // Candidate ran out before the root did: it is an ancestor.
                (Some(_), None) => return false,
                (Some(a), Some(b)) => {
                    if a.as_slice() != component_key(b.as_os_str()).as_slice() {
                        return false;
                    }
                }
            }
        }
    }

    /// Resolve a path for an operation and prove it is inside this root.
    ///
    /// `candidate` may be absolute, or relative to the root. Returns
    /// [`Code::FsPathEscapeBlocked`] if it resolves outside — which covers both
    /// `..` traversal and a symlink pointing out of the tree, because
    /// `canonicalize` resolves both.
    pub fn resolve(&self, candidate: impl AsRef<Path>) -> Result<SafePath> {
        let candidate = candidate.as_ref();

        // Lexical pre-check. `canonicalize` below is the real defence, but a
        // `..` in a caller-supplied relative path is always a bug or an attack:
        // nothing in this system has a legitimate reason to address a file by
        // walking upwards, and rejecting it before touching the disk keeps the
        // failure cheap and unambiguous.
        if candidate
            .components()
            .any(|c| matches!(c, Component::ParentDir))
        {
            return Err(escape_error(
                candidate,
                "the path contains a `..` component",
            ));
        }

        let joined = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            self.canonical.join(candidate)
        };

        let canonical = std::fs::canonicalize(&joined)
            .map_err(|e| Error::from(e).with_context(joined.display().to_string()))?;

        if !self.contains(&canonical) {
            tracing::warn!(
                root = %self.canonical.display(),
                resolved = %canonical.display(),
                "path escape blocked"
            );
            return Err(escape_error(
                &canonical,
                "it resolves outside the authorised root (symlink or traversal)",
            ));
        }

        Ok(SafePath { canonical })
    }
}

fn escape_error(path: &Path, why: &str) -> Error {
    Error::new(
        Code::FsPathEscapeBlocked,
        "Refused to operate on a path outside the authorised root. Add the \
         target directory as its own root if access to it is intended.",
    )
    .with_context(format!("{} — {why}", path.display()))
}

/// A canonical path proven to be inside an [`AuthorizedRoot`] at the moment it
/// was resolved.
///
/// The proof is a snapshot, not a guarantee: between resolution and the read,
/// a component can be replaced by a symlink. Call [`SafePath::reverify`]
/// immediately before the operation — invariant #7 says *at operation time*,
/// not at index time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafePath {
    canonical: PathBuf,
}

impl SafePath {
    pub fn as_path(&self) -> &Path {
        &self.canonical
    }

    pub fn into_path_buf(self) -> PathBuf {
        self.canonical
    }

    /// NFC identity of this path. See [`path_key`].
    pub fn key(&self) -> Result<PathKey> {
        path_key(&self.canonical)
    }

    /// Re-run the containment proof against the live filesystem.
    ///
    /// Cheap (one `realpath`) and mandatory before any read or write.
    pub fn reverify(&self, root: &AuthorizedRoot) -> Result<()> {
        let now = std::fs::canonicalize(&self.canonical)
            .map_err(|e| Error::from(e).with_context(self.canonical.display().to_string()))?;
        if now != self.canonical || !root.contains(&now) {
            tracing::warn!(
                was = %self.canonical.display(),
                now = %now.display(),
                "path changed identity between resolution and use"
            );
            return Err(escape_error(
                &now,
                "the path no longer resolves to where it did when it was checked",
            ));
        }
        Ok(())
    }
}

impl AsRef<Path> for SafePath {
    fn as_ref(&self) -> &Path {
        &self.canonical
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// "café" with a precomposed é (NFC) and with e + combining acute (NFD).
    const NFC_NAME: &str = "caf\u{e9}.txt";
    const NFD_NAME: &str = "cafe\u{301}.txt";

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn sibling_directory_with_a_shared_prefix_is_not_inside() {
        // The invariant-#7 example, verbatim: /safe/root vs /safe/root-evil.
        let td = tmp();
        let base = fs::canonicalize(td.path()).unwrap();
        fs::create_dir(base.join("root")).unwrap();
        fs::create_dir(base.join("root-evil")).unwrap();
        fs::write(base.join("root-evil/loot.txt"), b"x").unwrap();

        let root = AuthorizedRoot::open(base.join("root")).unwrap();

        assert!(
            !root.contains(&base.join("root-evil")),
            "string-prefix containment would wrongly accept this"
        );
        assert!(!root.contains(&base.join("root-evil/loot.txt")));
        assert!(root.contains(root.path()));

        let err = root.resolve(base.join("root-evil/loot.txt")).unwrap_err();
        assert_eq!(err.code(), Code::FsPathEscapeBlocked);
    }

    #[test]
    fn parent_traversal_in_a_relative_path_is_blocked() {
        let td = tmp();
        let base = fs::canonicalize(td.path()).unwrap();
        fs::create_dir(base.join("root")).unwrap();
        fs::create_dir(base.join("root/sub")).unwrap();
        fs::write(base.join("outside.txt"), b"x").unwrap();

        let root = AuthorizedRoot::open(base.join("root")).unwrap();

        for attempt in ["../outside.txt", "sub/../../outside.txt", "sub/.."] {
            let err = root.resolve(attempt).unwrap_err();
            assert_eq!(
                err.code(),
                Code::FsPathEscapeBlocked,
                "`{attempt}` must be refused"
            );
        }

        // A plain relative path inside the root still works.
        fs::write(base.join("root/sub/ok.txt"), b"x").unwrap();
        assert!(root.resolve("sub/ok.txt").is_ok());
    }

    #[test]
    fn symlink_escape_blocked() {
        let td = tmp();
        let base = fs::canonicalize(td.path()).unwrap();
        fs::create_dir(base.join("root")).unwrap();
        fs::create_dir(base.join("secrets")).unwrap();
        fs::write(base.join("secrets/id_rsa"), b"PRIVATE KEY").unwrap();

        // The "cloned repo with a symlink to ~/.ssh" case from invariant #7.
        std::os::unix::fs::symlink(base.join("secrets"), base.join("root/ssh")).unwrap();
        std::os::unix::fs::symlink(base.join("secrets/id_rsa"), base.join("root/key.txt")).unwrap();

        let root = AuthorizedRoot::open(base.join("root")).unwrap();

        for attempt in ["ssh/id_rsa", "key.txt", "ssh"] {
            let err = root.resolve(attempt).unwrap_err();
            assert_eq!(
                err.code(),
                Code::FsPathEscapeBlocked,
                "`{attempt}` escapes the root through a symlink"
            );
        }
    }

    #[test]
    fn a_symlink_that_stays_inside_the_root_is_allowed() {
        // The check is containment, not "symlinks are forbidden".
        let td = tmp();
        let base = fs::canonicalize(td.path()).unwrap();
        fs::create_dir(base.join("root")).unwrap();
        fs::write(base.join("root/real.txt"), b"x").unwrap();
        std::os::unix::fs::symlink(base.join("root/real.txt"), base.join("root/link.txt")).unwrap();

        let root = AuthorizedRoot::open(base.join("root")).unwrap();
        let resolved = root.resolve("link.txt").unwrap();
        assert_eq!(resolved.as_path(), base.join("root/real.txt"));
    }

    #[test]
    fn nfc_nfd_single_identity() {
        assert_ne!(NFC_NAME, NFD_NAME, "the two spellings must differ as bytes");

        let td = tmp();
        let base = fs::canonicalize(td.path()).unwrap();

        // Written in the decomposed form, the way macOS itself stores it.
        fs::write(base.join(NFD_NAME), b"content").unwrap();

        let by_nfd = path_key(&base.join(NFD_NAME)).unwrap();
        let by_nfc = path_key(&base.join(NFC_NAME)).unwrap();
        assert_eq!(by_nfd, by_nfc, "one file must not have two identities");

        // And through the real filesystem: APFS is normalisation-insensitive,
        // so both spellings open the same file and canonicalise to one path.
        let root = AuthorizedRoot::open(&base).unwrap();
        let a = root.resolve(NFD_NAME).unwrap().key().unwrap();
        let b = root.resolve(NFC_NAME).unwrap().key().unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn a_root_named_in_nfd_contains_a_path_spelled_in_nfc() {
        let td = tmp();
        let base = fs::canonicalize(td.path()).unwrap();
        let dir = base.join("caf\u{65}\u{301}"); // NFD directory name
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("f.txt"), b"x").unwrap();

        let root = AuthorizedRoot::open(&dir).unwrap();
        // Same directory, composed spelling.
        assert!(root.contains(&base.join("caf\u{e9}").join("f.txt")));
    }

    #[test]
    fn reverify_rejects_a_path_that_became_a_symlink_out() {
        let td = tmp();
        let base = fs::canonicalize(td.path()).unwrap();
        fs::create_dir(base.join("root")).unwrap();
        fs::write(base.join("root/doc.txt"), b"safe").unwrap();
        fs::write(base.join("evil.txt"), b"not safe").unwrap();

        let root = AuthorizedRoot::open(base.join("root")).unwrap();
        let safe = root.resolve("doc.txt").unwrap();
        assert!(safe.reverify(&root).is_ok());

        // Swap the file for a symlink pointing outside, as a racing process
        // could between discovery and read.
        fs::remove_file(base.join("root/doc.txt")).unwrap();
        std::os::unix::fs::symlink(base.join("evil.txt"), base.join("root/doc.txt")).unwrap();

        let err = safe.reverify(&root).unwrap_err();
        assert_eq!(err.code(), Code::FsPathEscapeBlocked);
    }

    #[test]
    fn a_file_is_not_a_valid_root() {
        let td = tmp();
        let f = td.path().join("f.txt");
        fs::write(&f, b"x").unwrap();
        assert_eq!(
            AuthorizedRoot::open(&f).unwrap_err().code(),
            Code::CfgInvalid
        );
    }

    #[test]
    fn a_missing_path_resolves_to_not_found_not_to_escape() {
        let td = tmp();
        let root = AuthorizedRoot::open(td.path()).unwrap();
        assert_eq!(
            root.resolve("nope.txt").unwrap_err().code(),
            Code::FsNotFound
        );
    }
}
