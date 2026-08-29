//! Cloud-placeholder detection. **Invariant #5 — the highest-severity item in
//! this crate.**
//!
//! Reading a dehydrated file makes the sync client download it. On
//! `~/Library/CloudStorage` or `~/Library/Mobile Documents` that is hundreds of
//! gigabytes of someone's bandwidth, spent silently, by a background indexer.
//! So the rule is absolute: **the tier is decided from metadata, the file is
//! never opened to find out.**
//!
//! Two macOS mechanisms, both visible from a single `lstat` (TIER-003):
//!
//! - **`SF_DATALESS`** in `st_flags` — set by the kernel on a file whose data
//!   has been evicted but whose name and size remain. This is what iCloud
//!   Drive's "Optimise Mac Storage" produces today.
//! - **`.icloud` stub files** — a real, tiny plist named `.<original>.icloud`
//!   standing in for an evicted file. Older sync clients and non-APFS volumes
//!   produce these, and the original name is *gone* from the directory, so the
//!   flag check alone never sees it.
//!
//! Cost matters: M0 found `find ~ -flags dataless` timed out at two minutes,
//! which is a signal that these paths are slow to touch. Everything here reads
//! an `fs::Metadata` the caller already had, and stats nothing itself.

use std::ffi::OsStr;
use std::fs::Metadata;
use std::path::Path;

use marrow_core::{Error, Result, TierState};

/// `SF_DATALESS` from `<sys/stat.h>`: "file is dataless object".
///
/// Superuser-settable flag, so it also cannot be cleared by us, only observed.
#[cfg(target_os = "macos")]
pub const SF_DATALESS: u32 = 0x4000_0000;

/// Suffix of an iCloud stub file. The full name is `.<original name>.icloud`.
pub const ICLOUD_STUB_SUFFIX: &str = ".icloud";

/// Whether an `st_flags` value marks the file as dehydrated.
///
/// Split out from [`tier_from_metadata`] so the bit test is testable without
/// root: setting `SF_DATALESS` requires the superuser and a filesystem that
/// supports it, so no test can create a genuinely dataless file.
#[cfg(target_os = "macos")]
pub const fn flags_are_dataless(st_flags: u32) -> bool {
    st_flags & SF_DATALESS != 0
}

/// Whether a filename is an iCloud placeholder stub.
///
/// The canonical form is dot-prefixed (`.Report.pdf.icloud`). A bare
/// `Report.pdf.icloud` is accepted too: it is not a name any real document
/// carries, and treating an ambiguous name as a placeholder costs one skipped
/// file, whereas treating it as resident costs a download.
pub fn is_icloud_stub_name(name: &OsStr) -> bool {
    match name.to_str() {
        Some(s) => s.len() > ICLOUD_STUB_SUFFIX.len() && s.ends_with(ICLOUD_STUB_SUFFIX),
        None => false,
    }
}

/// The original filename an iCloud stub stands in for, if this is a stub.
///
/// `.Report.pdf.icloud` → `Report.pdf`. Useful for reporting "cloud-only, not
/// indexed" (TIER-008) under the name the user recognises.
pub fn icloud_stub_original_name(name: &OsStr) -> Option<String> {
    let s = name.to_str()?;
    if !is_icloud_stub_name(name) {
        return None;
    }
    let inner = &s[..s.len() - ICLOUD_STUB_SUFFIX.len()];
    Some(inner.strip_prefix('.').unwrap_or(inner).to_string())
}

/// Decide the tier from metadata that has **already been obtained**, without
/// opening the file.
///
/// `meta` must come from `lstat` (`fs::symlink_metadata`, or an `ignore` walk
/// with `follow_links(false)`). Following the link first would both stat twice
/// and let a symlink decide the tier of a file outside the root.
///
/// Never returns [`TierState::Unavailable`] on macOS: a volume that is gone
/// fails at the `lstat`, which is [`tier_of`]'s problem, not this function's.
#[cfg(target_os = "macos")]
pub fn tier_from_metadata(path: &Path, meta: &Metadata) -> TierState {
    use std::os::macos::fs::MetadataExt as _;

    if flags_are_dataless(meta.st_flags()) {
        tracing::trace!(path = %path.display(), "dataless: SF_DATALESS set");
        return TierState::Placeholder;
    }
    if path.file_name().map(is_icloud_stub_name).unwrap_or(false) {
        tracing::trace!(path = %path.display(), "dataless: .icloud stub");
        return TierState::Placeholder;
    }
    TierState::Resident
}

/// Fail-closed stub for every platform this build does not implement.
///
/// Returning [`TierState::Unavailable`] means nothing is read and nothing is
/// hashed — the safe direction. TIER-002 (Windows `FILE_ATTRIBUTE_RECALL_*`,
/// `..._OFFLINE`) and TIER-004 (Linux sync-client mount points by config) are
/// the work a port has to do here. Marrow targets macOS only today (D5).
#[cfg(not(target_os = "macos"))]
pub fn tier_from_metadata(path: &Path, _meta: &Metadata) -> TierState {
    tracing::error!(
        path = %path.display(),
        "no cloud-placeholder detection on this platform: refusing to treat any \
         file as resident (see TIER-002/TIER-004)"
    );
    TierState::Unavailable
}

/// `lstat` the path and decide its tier.
///
/// Prefer [`tier_from_metadata`] wherever the caller already holds metadata —
/// the walk does, and a second stat per file is exactly the cost M0 warned
/// about.
pub fn tier_of(path: &Path) -> Result<TierState> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => Ok(tier_from_metadata(path, &meta)),
        Err(e) => {
            let err = Error::from(e).with_context(path.display().to_string());
            // A detached volume surfaces as ENOENT/EIO on the parent, not on
            // this file, so it is not distinguishable here. The caller records
            // the error; it does not get to call the file resident.
            Err(err)
        }
    }
}

/// Guard every read path must pass. **Invariant #5.**
///
/// Returns [`marrow_core::Code::FsPlaceholderSkipped`] for anything that is not
/// [`TierState::Resident`]. Default policy is TIER-005: index metadata only,
/// never hydrate. Hydration is a separate, explicit, opt-in path (TIER-006) and
/// does not exist yet.
pub fn ensure_safe_to_read(path: &Path, tier: TierState) -> Result<()> {
    if tier.safe_to_read() {
        return Ok(());
    }
    Err(Error::new(
        marrow_core::Code::FsPlaceholderSkipped,
        "File is stored in the cloud and not on this disk. It is indexed by \
         metadata only; reading it would download it. Enable hydration for this \
         workspace if you want its contents.",
    )
    .with_context(format!("{} — tier {tier:?}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn dataless_flag_is_recognised() {
        assert!(flags_are_dataless(SF_DATALESS));
        // Coexisting with other flags, e.g. UF_HIDDEN | SF_DATALESS.
        assert!(flags_are_dataless(SF_DATALESS | 0x0000_8000));
        assert!(!flags_are_dataless(0));
        assert!(!flags_are_dataless(0x0000_0002)); // UF_IMMUTABLE
    }

    #[test]
    fn icloud_stub_names_are_recognised() {
        for name in [".Report.pdf.icloud", ".x.icloud", "Report.pdf.icloud"] {
            assert!(
                is_icloud_stub_name(&OsString::from(name)),
                "{name} must be treated as a placeholder"
            );
        }
        for name in ["report.pdf", "icloud", ".icloud", "a.icloudy"] {
            assert!(
                !is_icloud_stub_name(&OsString::from(name)),
                "{name} is not a stub"
            );
        }
    }

    #[test]
    fn stub_reports_the_original_name() {
        assert_eq!(
            icloud_stub_original_name(&OsString::from(".Q3 Report.pdf.icloud")).as_deref(),
            Some("Q3 Report.pdf")
        );
        assert_eq!(
            icloud_stub_original_name(&OsString::from("report.pdf")),
            None
        );
    }

    #[test]
    fn an_icloud_stub_on_disk_is_a_placeholder() {
        let td = tempfile::tempdir().unwrap();
        let stub = td.path().join(".Budget.xlsx.icloud");
        std::fs::write(&stub, b"\x00\x06\x00plist-ish").unwrap();
        assert_eq!(tier_of(&stub).unwrap(), TierState::Placeholder);

        let real = td.path().join("Budget.xlsx");
        std::fs::write(&real, b"data").unwrap();
        assert_eq!(tier_of(&real).unwrap(), TierState::Resident);
    }

    #[test]
    fn non_resident_tiers_are_refused_before_any_read() {
        let p = Path::new("/tmp/whatever");
        assert!(ensure_safe_to_read(p, TierState::Resident).is_ok());
        for t in [
            TierState::Placeholder,
            TierState::Hydrating,
            TierState::Unavailable,
        ] {
            let err = ensure_safe_to_read(p, t).unwrap_err();
            assert_eq!(err.code(), marrow_core::Code::FsPlaceholderSkipped);
        }
    }

    #[test]
    fn a_missing_file_is_an_error_not_a_resident_file() {
        let err = tier_of(Path::new("/definitely/not/here")).unwrap_err();
        assert_eq!(err.code(), marrow_core::Code::FsNotFound);
    }
}
