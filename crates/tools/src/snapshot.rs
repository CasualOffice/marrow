//! What a replacement destroyed, kept so it can be put back.
//!
//! # Why this exists
//!
//! [`Expect::Replacing`](crate::Expect) has always been reachable from an MCP
//! tool call — `CreateFile::expect` is deserialised straight off the wire —
//! and until this module a replacement was final. [`Written::replaced`] handed
//! back the *digest* of what had been there, which names the loss precisely and
//! does nothing about it.
//!
//! Hard rule 8 draws the line this sits on: **derived indexes are rebuildable;
//! corrections are not.** A file the user wrote is on the second side of that
//! line, and a model that replaces one over MCP has destroyed the only copy.
//! The write path was otherwise careful — containment re-proved at operation
//! time, a stale check immediately before the rename, one `rename` for the
//! whole crate — and every one of those refuses a *wrong* write. None of them
//! helps once a correct-looking write turns out to be unwanted.
//!
//! # Shape
//!
//! Content-addressed, and deliberately **outside the workspace**. A snapshot
//! written beside its original would be indexed, cited, and eventually
//! snapshotted itself; it also has to survive the workspace being deleted,
//! which is one of the things people undo. So the store lives with the index —
//! `~/.local/share/marrow/snapshots/` in practice — and the workspace is handed
//! one rather than making its own.
//!
//! ```text
//!   snapshots/<blake3-of-content>
//! ```
//!
//! Addressing by content rather than by write means replacing the same file
//! ten times costs one copy of each distinct prior state, and an undo of an
//! undo finds the bytes already there.
//!
//! # What undo is not
//!
//! Not a transaction log, not a stack, and not multi-step. [`Undo`] restores
//! one file to one prior state, and it goes through the same guarded path as
//! every other write — including a stale check, so an undo that would discard
//! an edit the user made *after* the tool ran is refused rather than being the
//! second destruction in a row. The transaction and step tables the milestone
//! also asks for are a different piece of work; this one is the part that stops
//! the bleeding, and it is useful on its own.

use std::fs;
use std::path::{Path, PathBuf};

use marrow_core::{Code, ContentHash, Error, Result};
use serde::{Deserialize, Serialize};

/// A handle to bytes that were overwritten.
///
/// The digest *is* the address, so this is a content hash with a name that says
/// what it is for. Kept as its own type because a `ContentHash` in a signature
/// could be the new content, the old content or the expectation, and those have
/// been confused in worse codebases than this one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotId(ContentHash);

impl SnapshotId {
    pub fn digest(&self) -> &ContentHash {
        &self.0
    }
}

impl std::fmt::Display for SnapshotId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Where overwritten content is kept.
#[derive(Clone, Debug)]
pub struct Snapshots {
    dir: PathBuf,
}

impl Snapshots {
    /// Open, creating the directory if it is not there.
    ///
    /// Takes the store's own directory rather than the data directory, so a
    /// test can point it at a temporary path without the layout being decided
    /// in two places.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)
            .map_err(|e| Error::from(e).with_context(dir.display().to_string()))?;
        Ok(Self { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path_for(&self, id: &SnapshotId) -> PathBuf {
        self.dir.join(id.0.to_string())
    }

    /// Copy `bytes` in, and return the handle that gets them back.
    ///
    /// Idempotent by construction: the address is the content, so capturing the
    /// same bytes twice is one file. The write is temp-then-rename for the same
    /// reason the main write path is — a snapshot half-written is a snapshot
    /// that restores corruption, and it would do it during someone's attempt to
    /// recover from the last problem.
    pub fn capture(&self, bytes: &[u8]) -> Result<SnapshotId> {
        let id = SnapshotId(ContentHash::of(bytes));
        let final_path = self.path_for(&id);
        if final_path.is_file() {
            return Ok(id);
        }

        let temp = self.dir.join(format!(".partial-{}", id.0));
        fs::write(&temp, bytes)
            .map_err(|e| Error::from(e).with_context(temp.display().to_string()))?;
        match fs::rename(&temp, &final_path) {
            Ok(()) => {}
            Err(e) => {
                let _ = fs::remove_file(&temp);
                // Someone else captured the same content in between. The
                // destination is content-addressed, so if it is there it holds
                // exactly these bytes.
                if !final_path.is_file() {
                    return Err(Error::from(e).with_context(final_path.display().to_string()));
                }
            }
        }
        Ok(id)
    }

    /// Read back what was captured.
    ///
    /// **Verified against its own address before it is returned.** A snapshot
    /// store is the last copy of something, and handing back bytes that no
    /// longer hash to their name would restore corruption over a file that was
    /// merely unwanted.
    pub fn read(&self, id: &SnapshotId) -> Result<Vec<u8>> {
        let path = self.path_for(id);
        let bytes = fs::read(&path).map_err(|e| {
            Error::from(e)
                .with_context(format!("snapshot {id}"))
                .with_context(path.display().to_string())
        })?;
        let actual = ContentHash::of(&bytes);
        if actual != id.0 {
            return Err(Error::new(
                Code::ModIntegrityFailed,
                "The saved copy of this file no longer matches its checksum, so it was not \
                 restored. Restoring it would overwrite a file that is merely unwanted with \
                 one that is damaged.",
            )
            .with_context(format!("snapshot {id}, found {actual}")));
        }
        Ok(bytes)
    }

    pub fn contains(&self, id: &SnapshotId) -> bool {
        self.path_for(id).is_file()
    }

    /// How many bytes the store is holding, and across how many entries.
    ///
    /// For the Status page to be able to say so. A store that grows without
    /// anybody being able to see it grow is the next surprise.
    pub fn usage(&self) -> Result<(u64, usize)> {
        let mut bytes = 0;
        let mut count = 0;
        for entry in fs::read_dir(&self.dir)
            .map_err(|e| Error::from(e).with_context(self.dir.display().to_string()))?
            .flatten()
        {
            let name = entry.file_name();
            // Partials belong to a capture in flight, not to the store.
            if name.to_string_lossy().starts_with(".partial-") {
                continue;
            }
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    bytes += meta.len();
                    count += 1;
                }
            }
        }
        Ok((bytes, count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Snapshots) {
        let t = tempfile::tempdir().unwrap();
        let s = Snapshots::open(t.path().join("snapshots")).unwrap();
        (t, s)
    }

    #[test]
    fn what_went_in_comes_back_out() {
        let (_t, s) = store();
        let id = s.capture(b"the paragraph the user typed").unwrap();
        assert_eq!(s.read(&id).unwrap(), b"the paragraph the user typed");
    }

    #[test]
    fn the_same_content_captured_twice_is_stored_once() {
        let (_t, s) = store();
        let a = s.capture(b"same").unwrap();
        let b = s.capture(b"same").unwrap();
        assert_eq!(a, b);
        assert_eq!(s.usage().unwrap(), (4, 1));
    }

    #[test]
    fn different_content_gets_different_handles() {
        let (_t, s) = store();
        let a = s.capture(b"one").unwrap();
        let b = s.capture(b"two").unwrap();
        assert_ne!(a, b);
        assert_eq!(s.read(&a).unwrap(), b"one");
        assert_eq!(s.read(&b).unwrap(), b"two");
    }

    /// The store is the last copy of something. Handing back damaged bytes
    /// would overwrite a file that was merely unwanted with one that is broken —
    /// the second destruction, during the recovery from the first.
    #[test]
    fn a_snapshot_that_no_longer_matches_its_address_is_refused_rather_than_returned() {
        let (_t, s) = store();
        let id = s.capture(b"original").unwrap();
        fs::write(s.dir().join(id.to_string()), b"tampered").unwrap();

        let e = s.read(&id).expect_err("must refuse");
        assert_eq!(e.code(), Code::ModIntegrityFailed);
    }

    #[test]
    fn a_handle_for_content_never_captured_is_not_present() {
        let (_t, s) = store();
        let id = SnapshotId(ContentHash::of(b"never stored"));
        assert!(!s.contains(&id));
        assert!(s.read(&id).is_err());
    }

    #[test]
    fn a_partial_capture_is_not_counted_as_stored_content() {
        let (_t, s) = store();
        s.capture(b"real").unwrap();
        fs::write(s.dir().join(".partial-deadbeef"), b"half written").unwrap();
        assert_eq!(
            s.usage().unwrap(),
            (4, 1),
            "a capture in flight is not part of the store"
        );
    }

    #[test]
    fn an_empty_file_is_snapshottable_like_any_other() {
        // The obvious off-by-one: "no bytes" and "no snapshot" are different
        // facts, and truncating a file to nothing is exactly the edit somebody
        // wants back.
        let (_t, s) = store();
        let id = s.capture(b"").unwrap();
        assert!(s.contains(&id));
        assert_eq!(s.read(&id).unwrap(), b"");
    }
}
