//! Whether a recorded file is *still there*, and what that means for citing it.
//!
//! **The index is a memory of the last sweep, not a view of the disk.**
//! `files.status = 'ACTIVE'` means the most recent reconciliation saw the file,
//! not that it exists now, and nothing marks a file deleted between sweeps. A
//! surface that reports what the index remembers — `citable: true`,
//! `tier_state: resident` — for a file deleted hours ago has answered the one
//! question it was asked, wrong.
//!
//! MCP's `file_info` learned that and grew a disk check. The desktop's
//! `file_detail`, which is the same question through the product's main
//! surface, did not: it answered from the index alone, so the app would offer
//! to cite a file that is gone while MCP correctly called it missing. Two
//! answers to one question, decided by which surface asked.
//!
//! So the rule lives here once, as a pure function over a path and what the
//! index recorded, and both surfaces apply it.

use marrow_core::Origin;

/// What is true *now* about a file the index remembers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Presence {
    /// Whether a directory entry exists at that path at this moment.
    pub on_disk: bool,
    /// `missing` when it is gone, otherwise what the last scan recorded.
    pub tier_state: String,
    /// What the last scan recorded, kept alongside so the two stay
    /// distinguishable: `missing` is a fact about now, `resident` was a fact
    /// about then, and collapsing them loses which is which.
    pub recorded_tier_state: String,
    /// **Both gated on the file still existing.** Chunks of a file that is gone
    /// are still in the index and search may return them until the next sweep,
    /// but they cannot be verified against the source — and citable means
    /// exactly that they can.
    pub citable: bool,
    pub indexed_for_search: bool,
    /// Set only when the file is gone: what the remaining figures describe, and
    /// what to do about it.
    pub note: Option<&'static str>,
}

/// The message a caller shows when the path is no longer there.
///
/// Reported rather than refused, deliberately. A refusal is indistinguishable
/// from "no such path in the index", which is a different fact and the wrong
/// next move — and it throws away the file id, the content hash and the path
/// history, which are what say the content was *renamed* rather than
/// destroyed.
const GONE: &str = "This path is in the index but is not on the disk now, so nothing here can \
                    be read or cited. The size, hash, version and chunk counts describe the \
                    copy last seen, not a file that exists. Check the file's previous paths \
                    first — a rename the last scan has not caught up with looks exactly like \
                    this — then run `marrow index` to reconcile.";

/// Stat the path and decide what the index's record still supports.
///
/// `symlink_metadata`, not `metadata`: it stats the path itself and opens
/// nothing, so it cannot follow a link out of the workspace and **never
/// hydrates a cloud placeholder**. A placeholder is a real directory entry, so
/// it still reports present — which is right, and `tier_state` is what says it
/// cannot be read.
pub fn check(path: &str, recorded_tier: &str, origin: Origin, chunks: i64) -> Presence {
    let on_disk = std::fs::symlink_metadata(path).is_ok();
    let recorded = recorded_tier.to_lowercase();
    Presence {
        on_disk,
        tier_state: if on_disk {
            recorded.clone()
        } else {
            "missing".to_string()
        },
        recorded_tier_state: recorded,
        citable: on_disk && origin == Origin::User,
        indexed_for_search: on_disk && chunks > 0,
        note: (!on_disk).then_some(GONE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_that_is_gone_is_not_citable_however_the_index_remembers_it() {
        // The bug, in one line: the index says RESIDENT and USER, and the file
        // was deleted an hour ago.
        let p = check("/nowhere/at/all.md", "RESIDENT", Origin::User, 12);
        assert!(!p.on_disk);
        assert!(!p.citable, "a file that is gone cannot be cited");
        assert!(!p.indexed_for_search, "nor verified against its source");
        assert_eq!(p.tier_state, "missing");
        // And what the index remembered is kept, because it is how a caller
        // tells a deletion from a rename.
        assert_eq!(p.recorded_tier_state, "resident");
        assert!(p.note.is_some(), "must say what the numbers describe");
    }

    #[test]
    fn a_file_that_is_there_keeps_what_the_scan_recorded() {
        let f = tempfile::NamedTempFile::new().expect("temp");
        let path = f.path().to_string_lossy().to_string();
        let p = check(&path, "RESIDENT", Origin::User, 3);
        assert!(p.on_disk);
        assert!(p.citable);
        assert!(p.indexed_for_search);
        assert_eq!(p.tier_state, "resident");
        assert_eq!(p.note, None);
    }

    #[test]
    fn a_file_this_system_wrote_is_present_and_still_not_citable() {
        // **The `origin = SELF` rule.** Existing is necessary and not
        // sufficient: the content is findable and may never support a claim.
        let f = tempfile::NamedTempFile::new().expect("temp");
        let path = f.path().to_string_lossy().to_string();
        let p = check(&path, "RESIDENT", Origin::SelfWritten, 3);
        assert!(p.on_disk);
        assert!(!p.citable);
        // Still searchable, though — findable and uncitable are different.
        assert!(p.indexed_for_search);
    }

    #[test]
    fn a_file_with_no_chunks_is_not_searchable_but_may_still_be_cited() {
        // A photo: recorded, findable by name, with no text in the index. It
        // exists and is the user's, so a claim about it can point at it.
        let f = tempfile::NamedTempFile::new().expect("temp");
        let path = f.path().to_string_lossy().to_string();
        let p = check(&path, "RESIDENT", Origin::User, 0);
        assert!(!p.indexed_for_search);
        assert!(p.citable);
    }
}
