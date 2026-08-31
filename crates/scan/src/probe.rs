//! Cheap per-file facts, obtained without reading a single content byte.
//!
//! Everything here comes out of one `lstat`. Nothing opens the file, so a
//! cloud placeholder can be probed safely — which is the point: TIER-005 says
//! placeholders are indexed by *metadata only*, and this is that metadata.

use std::fs::Metadata;
use std::path::Path;

use marrow_core::{Error, Result, TierState, Timestamp};

use crate::tier;

/// Filesystem-level identity: device plus inode.
///
/// **Not a file identity.** Path is never identity, and neither is an inode:
/// they are reused after
/// deletion, differ across volumes, and change when a file is copied rather
/// than moved. This is a *hint* used to notice that a path that changed is the
/// same on-disk object (rename detection, hard-link detection). The durable
/// identity is `marrow_core::FileId`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FsIdentity {
    pub dev: u64,
    pub ino: u64,
}

/// A MIME type guessed from the filename extension. **FS-014.**
///
/// The type exists so the guess cannot be mistaken for a fact. The spec is
/// explicit that extension is not the classifier: a `.txt` holding a ZIP, a
/// `.dat` holding SQLite, and 97 extension-less files in the M0 corpus are all
/// normal. Content sniffing belongs to the parse layer, which reads bytes; this
/// crate does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MimeHint(&'static str);

impl MimeHint {
    pub const fn as_str(self) -> &'static str {
        self.0
    }

    /// Always `false`. Kept as a method so call sites read as a check rather
    /// than as a comment someone can ignore.
    pub const fn is_authoritative(self) -> bool {
        false
    }
}

impl std::fmt::Display for MimeHint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}?", self.0) // the `?` is not decoration: it is the hint
    }
}

/// Extension → MIME guess.
///
/// Deliberately small: these are the extensions M0 actually found, in the order
/// it found them. Adding an entry for a format no file on disk uses is exactly
/// the speculative work the scope rules forbid.
const EXTENSION_HINTS: &[(&str, &str)] = &[
    // code (M0 priority 1: ~1,300 files)
    ("rs", "text/rust"),
    ("ts", "text/typescript"),
    ("tsx", "text/typescript"),
    ("js", "text/javascript"),
    ("mjs", "text/javascript"),
    ("cjs", "text/javascript"),
    ("py", "text/x-python"),
    ("sql", "application/sql"),
    ("sh", "application/x-sh"),
    ("css", "text/css"),
    ("html", "text/html"),
    ("htm", "text/html"),
    // text and config
    ("txt", "text/plain"),
    ("md", "text/markdown"),
    ("toml", "application/toml"),
    ("json", "application/json"),
    ("yaml", "application/yaml"),
    ("yml", "application/yaml"),
    ("csv", "text/csv"),
    ("xml", "text/xml"),
    // documents (66 files total; low priority per M0 F5)
    ("pdf", "application/pdf"),
    (
        "docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
    (
        "xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
    (
        "pptx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ),
    // images: 35% of the corpus by count, metadata-only (M0 F6)
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("png", "image/png"),
    ("gif", "image/gif"),
    ("heic", "image/heic"),
    ("webp", "image/webp"),
    ("svg", "image/svg+xml"),
    // app-internal noise, named so it can be filtered by type rather than by
    // extension string at the parse layer (M0 §6: ~2,700 files, no value)
    ("plist", "application/x-plist"),
    ("ttf", "font/ttf"),
    ("otf", "font/otf"),
    ("woff", "font/woff"),
    ("woff2", "font/woff2"),
];

/// Guess a MIME type from the extension. A hint, never a classification.
pub fn mime_hint(path: &Path) -> Option<MimeHint> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    EXTENSION_HINTS
        .iter()
        .find(|(e, _)| *e == ext)
        .map(|(_, m)| MimeHint(m))
}

/// Everything one `lstat` can tell us about a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileFacts {
    /// Apparent size in bytes. For a placeholder this is the *logical* size of
    /// the evicted file, not the bytes on disk — which is why a placeholder can
    /// be listed with a useful size without being downloaded.
    pub size: u64,
    /// Last modification time, UTC milliseconds.
    pub mtime: Timestamp,
    pub identity: FsIdentity,
    /// True if the path *itself* is a symlink (this comes from `lstat`, so the
    /// link is never followed to answer it).
    pub is_symlink: bool,
    pub is_dir: bool,
    /// **Never hydrate a placeholder.** Decided from this same metadata; see
    /// [`crate::tier`].
    pub tier: TierState,
    /// Extension guess only. See [`MimeHint`] and FS-014.
    pub mime_hint: Option<MimeHint>,
}

impl FileFacts {
    /// Whether content may be read. The one question every read path asks.
    pub fn readable(&self) -> bool {
        !self.is_dir && !self.is_symlink && self.tier.safe_to_read()
    }
}

/// Derive facts from metadata the caller already has.
///
/// `meta` must be from `lstat`. Passing `stat` metadata for a symlink would
/// describe the target while `is_symlink` claimed otherwise.
pub fn facts_from_metadata(path: &Path, meta: &Metadata) -> FileFacts {
    use std::os::unix::fs::MetadataExt as _;

    FileFacts {
        size: meta.len(),
        // `modified()` fails only on platforms without mtime; clamp rather than
        // fail the whole entry over a timestamp.
        mtime: meta
            .modified()
            .map(Timestamp::from_system_time)
            .unwrap_or(Timestamp::EPOCH),
        identity: FsIdentity {
            dev: meta.dev(),
            ino: meta.ino(),
        },
        is_symlink: meta.file_type().is_symlink(),
        is_dir: meta.is_dir(),
        tier: tier::tier_from_metadata(path, meta),
        mime_hint: mime_hint(path),
    }
}

/// `lstat` the path and derive its facts.
pub fn probe(path: &Path) -> Result<FileFacts> {
    let meta = std::fs::symlink_metadata(path)
        .map_err(|e| Error::from(e).with_context(path.display().to_string()))?;
    Ok(facts_from_metadata(path, &meta))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_reports_size_mtime_and_identity_without_reading() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("notes.md");
        std::fs::write(&p, b"# hello").unwrap();

        let f = probe(&p).unwrap();
        assert_eq!(f.size, 7);
        assert!(!f.is_dir && !f.is_symlink);
        assert_eq!(f.tier, TierState::Resident);
        assert!(f.mtime.as_millis() > 1_700_000_000_000);
        assert!(f.identity.ino != 0);
        assert_eq!(f.mime_hint.map(|m| m.as_str()), Some("text/markdown"));
        assert!(f.readable());
    }

    #[test]
    fn two_paths_to_one_inode_share_an_fs_identity() {
        let td = tempfile::tempdir().unwrap();
        let a = td.path().join("a.txt");
        let b = td.path().join("b.txt");
        std::fs::write(&a, b"x").unwrap();
        std::fs::hard_link(&a, &b).unwrap();
        assert_eq!(probe(&a).unwrap().identity, probe(&b).unwrap().identity);
    }

    #[test]
    fn a_symlink_is_reported_as_a_symlink_and_is_not_readable() {
        let td = tempfile::tempdir().unwrap();
        let target = td.path().join("target.txt");
        let link = td.path().join("link.txt");
        std::fs::write(&target, b"0123456789").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let f = probe(&link).unwrap();
        assert!(f.is_symlink, "lstat must not follow the link");
        assert_ne!(f.size, 10, "size must be the link's, not the target's");
        assert!(!f.readable(), "resolve through path::AuthorizedRoot first");
    }

    #[test]
    fn the_mime_guess_is_only_ever_a_hint() {
        // FS-014: a `.txt` full of ZIP bytes still hints `text/plain`. The type
        // is what stops that guess from being recorded as a fact.
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("archive.txt");
        std::fs::write(&p, b"PK\x03\x04").unwrap();
        let hint = probe(&p).unwrap().mime_hint.unwrap();
        assert_eq!(hint.as_str(), "text/plain");
        assert!(!hint.is_authoritative());
        assert_eq!(hint.to_string(), "text/plain?");
    }

    #[test]
    fn unknown_and_absent_extensions_give_no_hint() {
        assert!(mime_hint(Path::new("/x/README")).is_none());
        assert!(mime_hint(Path::new("/x/thing.qqq")).is_none());
        // Case is not significant on APFS and must not be here either.
        assert_eq!(
            mime_hint(Path::new("/x/PHOTO.JPEG")).map(|m| m.as_str()),
            Some("image/jpeg")
        );
    }

    #[test]
    fn an_icloud_stub_probes_as_a_placeholder_and_is_not_readable() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join(".Deck.pptx.icloud");
        std::fs::write(&p, b"stub").unwrap();
        let f = probe(&p).unwrap();
        assert_eq!(f.tier, TierState::Placeholder);
        assert!(!f.readable());
    }
}
