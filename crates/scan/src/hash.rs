//! BLAKE3 content hashing.
//!
//! The only code in this crate that opens a file, which makes it the choke
//! point for **invariant #5**: there is no public way to reach the read loop
//! without a [`TierState`] having been checked first. A placeholder is refused
//! with [`Code::FsPlaceholderSkipped`] before the `File::open`, because the
//! `open` is what triggers hydration.
//!
//! Streaming with a fixed 256 KB buffer rather than `mmap`: M0 measured
//! 417 MB/s that way over the real corpus, memory stays flat regardless of file
//! size, and a file truncated by another process mid-read produces a short read
//! instead of a SIGBUS. M0 F7 also found nothing on this disk at or above
//! 500 MB, so the exotic large-file paths have no corpus to justify them.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use marrow_core::{Code, ContentHash, Error, Result, TierState};

use crate::tier;

/// Read buffer size. 256 KB is what M0 measured at 417 MB/s; larger buffers did
/// not help and cost resident memory per worker.
pub const HASH_BUFFER_BYTES: usize = 256 * 1024;

/// Hash a file whose tier the caller already determined.
///
/// Use this from a walk: the walk's `lstat` already produced the tier, and
/// re-statting per file is the cost M0 warned about after
/// `find -flags dataless` timed out at two minutes.
///
/// Refuses anything that is not [`TierState::Resident`].
pub fn hash_file_with_tier(path: &Path, tier: TierState) -> Result<ContentHash> {
    // Invariant #5, checked before the open, not after.
    tier::ensure_safe_to_read(path, tier)?;
    stream(path)
}

/// Hash a file, determining its tier first.
///
/// One extra `lstat` compared with [`hash_file_with_tier`]. Correct by
/// construction, so it is the right entry point when there is no walk in front.
pub fn hash_file(path: &Path) -> Result<ContentHash> {
    let meta = std::fs::symlink_metadata(path)
        .map_err(|e| Error::from(e).with_context(path.display().to_string()))?;

    // A symlink's own tier says nothing about its target, and the target may be
    // outside the authorised root. Resolving that is `path::AuthorizedRoot`'s
    // job; refusing here keeps this module from becoming a second, weaker
    // containment check. `FsPathEscapeBlocked` is the closest code in the
    // taxonomy — there is no `FS_NOT_A_REGULAR_FILE`.
    if meta.file_type().is_symlink() {
        return Err(Error::new(
            Code::FsPathEscapeBlocked,
            "Refused to hash a symbolic link. Resolve it against an authorised \
             root first, then hash the resolved path.",
        )
        .with_context(path.display().to_string()));
    }
    if meta.is_dir() {
        return Err(Error::new(
            Code::FsNotFound,
            "Refused to hash a directory. Hash the files inside it instead.",
        )
        .with_context(path.display().to_string()));
    }

    hash_file_with_tier(path, tier::tier_from_metadata(path, &meta))
}

/// The read loop. Private: reaching it requires passing a tier check.
fn stream(path: &Path) -> Result<ContentHash> {
    let mut file =
        File::open(path).map_err(|e| Error::from(e).with_context(path.display().to_string()))?;

    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; HASH_BUFFER_BYTES];
    let mut total: u64 = 0;

    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                hasher.update(&buf[..n]);
                total += n as u64;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                // FS-011: one unreadable file does not stop the workspace.
                return Err(Error::from(e)
                    .with_context(format!("{} after {total} bytes", path.display())));
            }
        }
    }

    let hash = ContentHash::from_bytes(*hasher.finalize().as_bytes());
    tracing::trace!(path = %path.display(), bytes = total, hash = ?hash, "hashed");
    Ok(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashing_matches_the_one_shot_hash() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("f.bin");
        // Deliberately spans several buffers so the streaming path is exercised.
        let data: Vec<u8> = (0..(HASH_BUFFER_BYTES * 2 + 7))
            .map(|i| (i % 251) as u8)
            .collect();
        std::fs::write(&p, &data).unwrap();

        assert_eq!(hash_file(&p).unwrap(), ContentHash::of(&data));
    }

    #[test]
    fn an_empty_file_hashes_to_the_empty_digest() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("empty");
        std::fs::write(&p, b"").unwrap();
        assert_eq!(hash_file(&p).unwrap(), ContentHash::of(b""));
    }

    #[test]
    fn placeholder_never_hydrated() {
        // Invariant #5. Every non-resident tier is refused, and the refusal
        // happens before the file is opened.
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("real.txt");
        std::fs::write(&p, b"content").unwrap();

        for t in [
            TierState::Placeholder,
            TierState::Hydrating,
            TierState::Unavailable,
        ] {
            let err = hash_file_with_tier(&p, t).unwrap_err();
            assert_eq!(
                err.code(),
                Code::FsPlaceholderSkipped,
                "{t:?} must not be hashed"
            );
            assert!(!err.retryable(), "retrying must not defeat the skip");
        }
        assert!(hash_file_with_tier(&p, TierState::Resident).is_ok());
    }

    #[test]
    fn an_icloud_stub_is_refused_by_the_self_checking_entry_point() {
        let td = tempfile::tempdir().unwrap();
        let stub = td.path().join(".Budget.xlsx.icloud");
        std::fs::write(&stub, b"stub bytes").unwrap();
        assert_eq!(
            hash_file(&stub).unwrap_err().code(),
            Code::FsPlaceholderSkipped
        );
    }

    #[test]
    fn a_missing_file_reports_not_found() {
        let td = tempfile::tempdir().unwrap();
        assert_eq!(
            hash_file(&td.path().join("nope")).unwrap_err().code(),
            Code::FsNotFound
        );
    }

    #[test]
    fn an_unreadable_file_fails_without_panicking() {
        use std::os::unix::fs::PermissionsExt as _;
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("secret");
        std::fs::write(&p, b"x").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();

        let got = hash_file(&p);

        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        // Running as root would make this readable; skip rather than lie.
        if let Err(e) = got {
            assert_eq!(e.code(), Code::FsPermissionDenied);
        }
    }

    #[test]
    fn a_symlink_is_refused_rather_than_followed() {
        let td = tempfile::tempdir().unwrap();
        let target = td.path().join("t.txt");
        let link = td.path().join("l.txt");
        std::fs::write(&target, b"x").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(
            hash_file(&link).unwrap_err().code(),
            Code::FsPathEscapeBlocked
        );
    }

    #[test]
    fn a_directory_is_refused() {
        let td = tempfile::tempdir().unwrap();
        assert!(hash_file(td.path()).is_err());
    }
}
