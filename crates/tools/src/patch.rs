//! Change part of a file instead of all of it.
//!
//! # Why the whole-file write was not enough
//!
//! Until now every write in this crate replaced a file entirely. Fixing one
//! line of a long document meant a caller reproducing the whole thing from
//! memory, and a model that reproduces a 900-line file to change one of them
//! will quietly drop something on line 400. The damage is invisible: the write
//! succeeds, the digest matches what was sent, and the loss is somewhere in the
//! middle of a file nobody re-reads.
//!
//! # The shape, and why not the other shapes
//!
//! An anchored replacement. The caller supplies `find`, which must appear in
//! the file **exactly once**, and `replace`, which goes in its place.
//!
//! - **Not line numbers.** A caller working from a summary, a search result or
//!   its own recollection is confidently wrong about line numbers, and being
//!   wrong lands the edit somewhere plausible instead of failing.
//! - **Not a fuzzy or context diff.** Fuzz is a tolerance for being slightly
//!   wrong about a file you are about to overwrite, which is the wrong thing to
//!   be tolerant of. Exact bytes or nothing.
//! - **Not "replace the first occurrence".** First-match is the rule that turns
//!   an ambiguous instruction into a silent edit in the wrong place. Two matches
//!   is a refusal that names the count, so the caller can extend the anchor
//!   until it is unique.
//!
//! # The read-before-edit guard
//!
//! Two halves, and both are needed:
//!
//! 1. **`expect` is mandatory.** There is no patch that creates a file, so
//!    [`Expect::Replacing`] is the only precondition this accepts —
//!    [`Expect::New`] is refused rather than silently treated as "anything".
//!    A caller that has not read the file cannot know its digest.
//! 2. **The anchor has to be there.** The digest proves the file is the one that
//!    was read; the anchor proves the caller read the *part* it is editing.
//!
//! # This is the first read in this crate
//!
//! Everything before it wrote bytes it was handed. A patch has to open the file
//! it is changing, which brings the never-hydrate-a-placeholder rule inside
//! these walls for the first time: `ensure_safe_to_read` is checked before the
//! open, so patching a cloud stub refuses rather than pulling it down.

use marrow_core::{Code, ContentHash, Error, Result};
use serde::{Deserialize, Serialize};

use crate::guard::{Expect, Workspace, Written};

/// The largest file this will read in to patch.
///
/// A patch holds the whole file in memory twice — once read, once rewritten —
/// and a caller that wants to anchor-edit a 500 MB log is asking for the wrong
/// tool. M0 measured 70.6% of files under 64 KB, so this is far above anything
/// the corpus actually contains.
pub const MAX_PATCH_BYTES: u64 = 16 * 1024 * 1024;

/// One anchored replacement.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Patch {
    /// Workspace-relative path.
    pub path: String,
    /// Exact text to find. Must occur exactly once.
    pub find: String,
    /// What replaces it. Empty deletes the anchor.
    pub replace: String,
    /// What the caller read. **Required** — see the module docs.
    pub expect: Expect,
}

/// Apply an anchored replacement.
pub fn patch(ws: &Workspace, req: &Patch) -> Result<Written> {
    // A patch that creates a file is not a patch. Refused explicitly rather
    // than left to fail later on a missing anchor, because the message a
    // caller gets should name the thing they got wrong.
    let Expect::Replacing(expected) = &req.expect else {
        return Err(Error::new(
            Code::CfgInvalid,
            "A patch edits a file that exists, so it needs the digest of what you read. \
             Read the file first and pass it as `expect`.",
        )
        .with_context(req.path.clone()));
    };

    if req.find.is_empty() {
        return Err(Error::new(
            Code::CfgInvalid,
            "`find` is empty, which matches everywhere and therefore nowhere. Give the \
             exact text to replace.",
        )
        .with_context(req.path.clone()));
    }

    let current = read_for_patch(ws, &req.path, expected)?;

    // Count every occurrence, not just enough to know there are two. The
    // refusal says how many there are, and "found 7" tells the caller how much
    // more anchor they need in a way "more than one" does not.
    let matches = current.matches(req.find.as_str()).count();
    match matches {
        1 => {}
        0 => {
            return Err(Error::new(
                Code::ActStaleVersion,
                "That text is not in the file, so there is nothing to replace. The file may \
                 use different whitespace or wording than you remember — read it again and \
                 anchor on what is actually there.",
            )
            .with_context(req.path.clone()))
        }
        n => {
            return Err(Error::new(
                Code::CfgInvalid,
                format!(
                    "That text appears {n} times, so replacing it would be a guess about \
                     which one you meant. Extend it until it is unique."
                ),
            )
            .with_context(req.path.clone()))
        }
    }

    let updated = current.replacen(req.find.as_str(), &req.replace, 1);

    // A patch that changes nothing is not an error and is not a write. Going
    // through the write path would burn a snapshot and mint a new version of a
    // file that did not move — the same defect the ingest resume had, where
    // needing to redo our own work minted a version of an unchanged file.
    if updated == current {
        return Err(Error::new(
            Code::CfgInvalid,
            "`find` and `replace` are the same, so this patch would rewrite the file \
             without changing it. Nothing was written.",
        )
        .with_context(req.path.clone()));
    }

    // **Validity may not be lost.** Checked here rather than in
    // `Workspace::write` for a reason worth stating: `write` does not read the
    // old content unless it happens to be snapshotting, so validating there
    // would either add a read to every write or make a safety check depend on
    // whether a snapshot store is configured. A check that is on or off
    // according to unrelated configuration is worse than one that is honestly
    // scoped, and a patch is the operation that has both versions in hand
    // anyway.
    let target = ws.resolve_existing(&req.path)?;
    crate::validate::check_no_regression(&target, &current, &updated)?;

    // Through the one guarded path, with the caller's own precondition. Every
    // rule the whole-file write has — containment re-proved at operation time,
    // the stale check immediately before the rename, the snapshot for undo,
    // `origin = SELF` — applies here because this does not write on its own
    // account.
    ws.write(&req.path, updated.as_bytes(), &req.expect)
}

/// Read the file a patch is about to change.
///
/// Three refusals before any content is used, in the order that costs least:
/// tier, then size, then encoding. The digest check comes last because it is
/// the only one that has to read the whole file — and it is re-asked inside
/// [`Workspace::write`] anyway, immediately before the rename, which is the
/// check that actually protects the write. This one exists so the anchor is
/// searched in the file the caller believes it read.
fn read_for_patch(ws: &Workspace, relative: &str, expected: &ContentHash) -> Result<String> {
    let target = ws.resolve_existing(relative)?;

    // **Never hydrate a placeholder**, checked before the open rather than
    // after. This is the first read in this crate, so it is the first place the
    // rule has had anything to say here.
    let tier = marrow_scan::tier_of(&target)?;
    marrow_scan::ensure_safe_to_read(&target, tier)?;

    let meta = std::fs::metadata(&target)
        .map_err(|e| Error::from(e).with_context(target.display().to_string()))?;
    if meta.len() > MAX_PATCH_BYTES {
        return Err(Error::new(
            Code::ParBudgetExceeded,
            format!(
                "That file is larger than the {} MB a patch will read. Rewrite the part you \
                 need as a new file, or edit it outside this system.",
                MAX_PATCH_BYTES / (1024 * 1024)
            ),
        )
        .with_context(format!("{} bytes", meta.len())));
    }

    let bytes = std::fs::read(&target)
        .map_err(|e| Error::from(e).with_context(target.display().to_string()))?;

    let actual = ContentHash::of(&bytes);
    if actual != *expected {
        return Err(Error::new(
            Code::ActStaleVersion,
            "The file changed since it was read, so this patch would be anchored in text \
             that is no longer there. Re-read it and try again.",
        )
        .with_context(format!(
            "{} — expected {expected}, found {actual}",
            target.display()
        )));
    }

    // Text only. Anchoring inside a binary would match bytes that are not
    // characters, and `replacen` on lossy text would write back a file with the
    // unrepresentable parts replaced by U+FFFD — a silent corruption of
    // everything the anchor did not touch.
    String::from_utf8(bytes).map_err(|_| {
        Error::new(
            Code::ParUnsupported,
            "That file is not text, so there is no way to anchor an edit in it without \
             corrupting the parts this patch does not touch.",
        )
        .with_context(target.display().to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _tmp: tempfile::TempDir,
        ws: Workspace,
        root: std::path::PathBuf,
    }

    fn fixture(name: &str, body: &str) -> (Fixture, ContentHash) {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        std::fs::write(root.join(name), body).unwrap();
        let store = crate::Snapshots::open(root.join(".snapshots")).unwrap();
        let ws = Workspace::open(&root)
            .unwrap()
            .with_snapshots(store)
            // The store lives under the root only because a test needs it
            // somewhere; excluding it keeps it out of the workspace's own view.
            .protect(root.join(".snapshots"));
        let digest = ContentHash::of(body.as_bytes());
        (
            Fixture {
                _tmp: tmp,
                ws,
                root,
            },
            digest,
        )
    }

    fn req(path: &str, find: &str, replace: &str, expect: Expect) -> Patch {
        Patch {
            path: path.into(),
            find: find.into(),
            replace: replace.into(),
            expect,
        }
    }

    #[test]
    fn a_unique_anchor_is_replaced_and_the_rest_of_the_file_is_untouched() {
        let body = "# Notes\n\nThe meeting is on Tuesday.\n\nOther text stays.\n";
        let (f, digest) = fixture("notes.md", body);

        let w = patch(
            &f.ws,
            &req(
                "notes.md",
                "on Tuesday",
                "on Thursday",
                Expect::Replacing(digest),
            ),
        )
        .expect("patches");

        let after = std::fs::read_to_string(f.root.join("notes.md")).unwrap();
        assert_eq!(
            after,
            "# Notes\n\nThe meeting is on Thursday.\n\nOther text stays.\n"
        );
        assert_eq!(w.digest(), ContentHash::of(after.as_bytes()));
        // And it went through the guarded path, so it is undoable.
        assert!(w.snapshot().is_some());
    }

    /// First-match is the rule that turns an ambiguous instruction into a
    /// silent edit in the wrong place.
    #[test]
    fn an_anchor_that_appears_twice_is_refused_and_the_count_is_in_the_message() {
        let body = "alpha\nbeta\nalpha\n";
        let (f, digest) = fixture("f.txt", body);

        let e = patch(
            &f.ws,
            &req("f.txt", "alpha", "gamma", Expect::Replacing(digest)),
        )
        .expect_err("must refuse");

        assert!(e.message().contains('2'), "{}", e.message());
        assert_eq!(
            std::fs::read_to_string(f.root.join("f.txt")).unwrap(),
            body,
            "and nothing was written"
        );
    }

    #[test]
    fn an_anchor_that_is_not_there_says_so_rather_than_writing() {
        let body = "the file as it is\n";
        let (f, digest) = fixture("f.txt", body);

        let e = patch(
            &f.ws,
            &req(
                "f.txt",
                "the file as I remember it",
                "x",
                Expect::Replacing(digest),
            ),
        )
        .expect_err("must refuse");

        assert_eq!(e.code(), Code::ActStaleVersion);
        assert_eq!(std::fs::read_to_string(f.root.join("f.txt")).unwrap(), body);
    }

    /// The read-before-edit guard. A caller with no digest has not read the
    /// file, and a patch is an edit to something you are supposed to have read.
    #[test]
    fn a_patch_with_no_digest_is_refused_rather_than_treated_as_overwrite() {
        let (f, _) = fixture("f.txt", "content\n");
        let e =
            patch(&f.ws, &req("f.txt", "content", "other", Expect::New)).expect_err("must refuse");
        assert_eq!(e.code(), Code::CfgInvalid);
        assert!(e.message().contains("digest"), "{}", e.message());
    }

    #[test]
    fn a_patch_against_a_stale_digest_is_refused() {
        let (f, _) = fixture("f.txt", "current content\n");
        let stale = ContentHash::of(b"what it used to say\n");

        let e = patch(
            &f.ws,
            &req("f.txt", "current", "new", Expect::Replacing(stale)),
        )
        .expect_err("must refuse");
        assert_eq!(e.code(), Code::ActStaleVersion);
    }

    #[test]
    fn an_empty_anchor_matches_nothing_rather_than_everything() {
        let (f, digest) = fixture("f.txt", "content\n");
        let e = patch(&f.ws, &req("f.txt", "", "x", Expect::Replacing(digest)))
            .expect_err("must refuse");
        assert_eq!(e.code(), Code::CfgInvalid);
    }

    #[test]
    fn a_patch_that_would_change_nothing_is_refused_rather_than_minting_a_version() {
        let (f, digest) = fixture("f.txt", "same\n");
        let e = patch(
            &f.ws,
            &req("f.txt", "same", "same", Expect::Replacing(digest)),
        )
        .expect_err("must refuse");
        assert_eq!(e.code(), Code::CfgInvalid);
    }

    #[test]
    fn an_empty_replacement_deletes_the_anchor() {
        let body = "keep\nDELETE ME\nkeep\n";
        let (f, digest) = fixture("f.txt", body);
        patch(
            &f.ws,
            &req("f.txt", "DELETE ME\n", "", Expect::Replacing(digest)),
        )
        .expect("patches");
        assert_eq!(
            std::fs::read_to_string(f.root.join("f.txt")).unwrap(),
            "keep\nkeep\n"
        );
    }

    /// Anchoring in bytes that are not characters would corrupt everything the
    /// anchor did not touch, because the write-back goes through `String`.
    #[test]
    fn a_file_that_is_not_text_is_refused_rather_than_lossily_rewritten() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let bytes = [0x00u8, 0xff, 0xfe, b'a', b'b', 0x80];
        std::fs::write(root.join("blob.bin"), bytes).unwrap();
        let ws = Workspace::open(&root).unwrap();

        let e = patch(
            &ws,
            &req(
                "blob.bin",
                "ab",
                "cd",
                Expect::Replacing(ContentHash::of(&bytes)),
            ),
        )
        .expect_err("must refuse");
        assert_eq!(e.code(), Code::ParUnsupported);
        assert_eq!(std::fs::read(root.join("blob.bin")).unwrap(), bytes);
    }

    #[test]
    fn a_patch_cannot_reach_outside_the_workspace() {
        let (f, digest) = fixture("f.txt", "content\n");
        let e = patch(
            &f.ws,
            &req("../escaped.txt", "content", "x", Expect::Replacing(digest)),
        )
        .expect_err("must refuse");
        assert!(
            matches!(e.code(), Code::FsPathEscapeBlocked | Code::ActNameRejected),
            "unexpected {:?}",
            e.code()
        );
    }

    #[test]
    fn a_multi_line_anchor_works_and_keeps_the_surrounding_lines() {
        let body = "one\ntwo\nthree\nfour\n";
        let (f, digest) = fixture("f.txt", body);
        patch(
            &f.ws,
            &req(
                "f.txt",
                "two\nthree\n",
                "TWO\nTHREE\n",
                Expect::Replacing(digest),
            ),
        )
        .expect("patches");
        assert_eq!(
            std::fs::read_to_string(f.root.join("f.txt")).unwrap(),
            "one\nTWO\nTHREE\nfour\n"
        );
    }
}
