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

/// The largest single file copied by default: 64 MB.
///
/// Above this, a replacement is not undoable and says so. Chosen against what
/// this program indexes — M0 measured 70.6% of files under 64 KB and nothing at
/// all above 500 MB — so it is far above ordinary documents and well below the
/// point where one write costs a meaningful part of a disk.
pub const DEFAULT_MAX_CAPTURE_BYTES: u64 = 64 * 1024 * 1024;
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
    /// Rebuild a handle from the digest a write reported.
    ///
    /// The handle *is* the digest, so this is a rename rather than a lookup —
    /// it does not assert that the content is present. [`Snapshots::read`] is
    /// where a handle for content that was never captured, or has since been
    /// removed, produces an error a caller can act on.
    pub fn from_digest(digest: ContentHash) -> Self {
        Self(digest)
    }

    pub fn digest(&self) -> &ContentHash {
        &self.0
    }
}

impl std::fmt::Display for SnapshotId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// When a snapshot may be deleted.
///
/// **Pruning a snapshot is itself destructive**, and it destroys the one thing
/// standing between a user and a write they did not want. So the two limits
/// are not symmetric: the age floor wins, and the size cap is allowed to be
/// exceeded rather than allowed to delete something recent.
///
/// A store that is over its cap entirely because of young snapshots reports
/// that fact ([`Pruned::kept_despite_cap`]) instead of deleting its way out of
/// it. Filling a disk is recoverable by hand; deleting the only copy of
/// somebody's afternoon is not, and that asymmetry is the whole reason this
/// type exists rather than a single `max_bytes`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retention {
    /// Below this age, a snapshot is never deleted whatever the cap says.
    pub keep_for: std::time::Duration,
    /// The size the store tries to stay under, by deleting oldest-first among
    /// the snapshots that are already past `keep_for`.
    pub max_bytes: u64,
}

impl Default for Retention {
    /// Fourteen days and two gigabytes.
    ///
    /// Fourteen because "I asked it to rewrite that file last week and I want
    /// it back" is a real sentence and "last month" mostly is not. Two
    /// gigabytes because the index beside it is already larger than that, so a
    /// store this size is not what fills the disk — and because the cap only
    /// ever applies to snapshots already past the floor.
    fn default() -> Self {
        Self {
            keep_for: std::time::Duration::from_secs(14 * 24 * 60 * 60),
            max_bytes: 2 * 1024 * 1024 * 1024,
        }
    }
}

/// What a prune did, and what it declined to do.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Pruned {
    pub removed: usize,
    pub freed_bytes: u64,
    /// Still over the cap after pruning, because everything left is younger
    /// than [`Retention::keep_for`]. Reported rather than resolved.
    pub kept_despite_cap: bool,
    pub bytes_after: u64,
}

/// Where overwritten content is kept.
#[derive(Clone, Debug)]
pub struct Snapshots {
    dir: PathBuf,
    /// Above this, a single file is not copied at all. See
    /// [`Snapshots::with_max_capture_bytes`].
    max_capture_bytes: u64,
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
        Ok(Self {
            dir,
            max_capture_bytes: DEFAULT_MAX_CAPTURE_BYTES,
        })
    }

    /// The largest single file this store will copy.
    ///
    /// A replacement of a 4 GB file would otherwise put 4 GB here, silently,
    /// on the way to doing something the user asked for. Above the limit the
    /// write still happens and simply is not undoable — which
    /// [`crate::Written::is_undoable`] and the MCP response both say, so the
    /// caller learns it at the time rather than when the undo fails.
    pub fn with_max_capture_bytes(mut self, bytes: u64) -> Self {
        self.max_capture_bytes = bytes;
        self
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

    /// [`Snapshots::capture`], but `None` when the content is too large to keep.
    ///
    /// Not an error: nothing has gone wrong, and the write it belongs to is
    /// still perfectly valid. It is simply final, and the caller is told so.
    pub fn try_capture(&self, bytes: &[u8]) -> Result<Option<SnapshotId>> {
        if bytes.len() as u64 > self.max_capture_bytes {
            tracing::info!(
                bytes = bytes.len(),
                limit = self.max_capture_bytes,
                "too large to keep a copy of; this write will not be undoable"
            );
            return Ok(None);
        }
        self.capture(bytes).map(Some)
    }

    /// Delete what is both old enough and surplus to the cap.
    ///
    /// Oldest first, by modification time — which for a content-addressed store
    /// is the time it was last *captured*, and therefore a decent proxy for how
    /// recently it mattered.
    ///
    /// A snapshot younger than [`Retention::keep_for`] is never deleted, even
    /// when that leaves the store over its cap. See the note on [`Retention`].
    pub fn prune(&self, retention: Retention) -> Result<Pruned> {
        let now = std::time::SystemTime::now();
        let mut entries: Vec<(std::time::Duration, u64, PathBuf)> = Vec::new();
        let mut total: u64 = 0;

        for entry in fs::read_dir(&self.dir)
            .map_err(|e| Error::from(e).with_context(self.dir.display().to_string()))?
            .flatten()
        {
            if entry.file_name().to_string_lossy().starts_with(".partial-") {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            total += meta.len();
            // An unreadable or future mtime counts as brand new, so the
            // uncertain case keeps the file rather than deleting it.
            let age = meta
                .modified()
                .ok()
                .and_then(|m| now.duration_since(m).ok())
                .unwrap_or_default();
            entries.push((age, meta.len(), entry.path()));
        }

        let mut out = Pruned {
            bytes_after: total,
            ..Pruned::default()
        };
        if total <= retention.max_bytes {
            return Ok(out);
        }

        // Oldest first: the ones least likely to be wanted go first.
        entries.sort_by_key(|(age, _, _)| std::cmp::Reverse(*age));
        for (age, len, path) in entries {
            if out.bytes_after <= retention.max_bytes {
                break;
            }
            if age < retention.keep_for {
                // Everything from here on is younger still, because the list is
                // sorted oldest first.
                out.kept_despite_cap = true;
                break;
            }
            if fs::remove_file(&path).is_ok() {
                out.removed += 1;
                out.freed_bytes += len;
                out.bytes_after = out.bytes_after.saturating_sub(len);
            }
        }

        if out.kept_despite_cap {
            tracing::warn!(
                bytes = out.bytes_after,
                cap = retention.max_bytes,
                "the snapshot store is over its cap and everything left is recent; \
                 keeping it rather than deleting an undo somebody may want"
            );
        }
        Ok(out)
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

    use std::time::Duration;

    /// Backdate a stored snapshot so age-based rules are testable without
    /// waiting fourteen days.
    fn age(s: &Snapshots, id: &SnapshotId, by: Duration) {
        let path = s.dir().join(id.to_string());
        let when = std::time::SystemTime::now() - by;
        filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(when))
            .expect("backdate");
    }

    #[test]
    fn a_store_under_its_cap_is_left_alone() {
        let (_t, s) = store();
        s.capture(b"kept").unwrap();
        let p = s
            .prune(Retention {
                keep_for: Duration::ZERO,
                max_bytes: 1024,
            })
            .unwrap();
        assert_eq!(p.removed, 0);
        assert!(!p.kept_despite_cap);
    }

    #[test]
    fn over_the_cap_the_oldest_go_first() {
        let (_t, s) = store();
        let old = s.capture(b"oldest content here").unwrap();
        let mid = s.capture(b"middle content here").unwrap();
        let new = s.capture(b"newest content here").unwrap();
        age(&s, &old, Duration::from_secs(60 * 60 * 24 * 30));
        age(&s, &mid, Duration::from_secs(60 * 60 * 24 * 20));
        age(&s, &new, Duration::from_secs(60 * 60 * 24 * 15));

        // Room for two of the three 19-byte entries.
        let p = s
            .prune(Retention {
                keep_for: Duration::from_secs(60 * 60 * 24 * 14),
                max_bytes: 40,
            })
            .unwrap();

        assert_eq!(p.removed, 1);
        assert!(!s.contains(&old), "the oldest went");
        assert!(s.contains(&mid) && s.contains(&new));
    }

    /// The asymmetry that makes this type worth having. Filling a disk is
    /// recoverable by hand; deleting the only copy of somebody's afternoon is
    /// not.
    #[test]
    fn a_recent_snapshot_is_kept_even_when_that_leaves_the_store_over_its_cap() {
        let (_t, s) = store();
        s.capture(b"written moments ago and irreplaceable").unwrap();

        let p = s
            .prune(Retention {
                keep_for: Duration::from_secs(60 * 60 * 24 * 14),
                max_bytes: 1,
            })
            .unwrap();

        assert_eq!(p.removed, 0, "nothing recent may be deleted for space");
        assert!(p.kept_despite_cap, "and the caller is told why");
        assert!(p.bytes_after > 1);
        assert_eq!(s.usage().unwrap().1, 1);
    }

    #[test]
    fn pruning_never_touches_a_capture_in_flight() {
        let (_t, s) = store();
        let old = s.capture(b"old enough to go").unwrap();
        age(&s, &old, Duration::from_secs(60 * 60 * 24 * 30));
        fs::write(s.dir().join(".partial-abc"), b"being written right now").unwrap();

        s.prune(Retention {
            keep_for: Duration::from_secs(60 * 60 * 24 * 14),
            max_bytes: 0,
        })
        .unwrap();

        assert!(!s.contains(&old));
        assert!(
            s.dir().join(".partial-abc").is_file(),
            "a capture in flight is not the store's to delete"
        );
    }

    /// A 4 GB replacement must not put 4 GB here on the way to doing what was
    /// asked. The write stays valid and stops being undoable, which the caller
    /// is told at the time.
    #[test]
    fn content_too_large_to_keep_is_declined_rather_than_stored_or_refused() {
        let (t, _s) = store();
        let s = Snapshots::open(t.path().join("small"))
            .unwrap()
            .with_max_capture_bytes(8);

        assert_eq!(s.try_capture(b"tiny").unwrap().map(|_| ()), Some(()));
        assert_eq!(
            s.try_capture(b"far too long to keep").unwrap(),
            None,
            "declined, and not an error"
        );
        assert_eq!(s.usage().unwrap().1, 1);
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
