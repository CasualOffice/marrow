//! The scratch workspace, end to end: drop a file in, find it, throw it away.
//!
//! These run against a real store, a real index and the real ingest pipeline
//! over a temporary directory — no model, no network, no window. That is
//! deliberate: the promise being tested is "a dropped file is searchable and
//! answerable without the user doing anything else", and the searchable half is
//! the half that must work with no LLM, no GPU and no network (hard rule 10).

use std::path::PathBuf;
use std::sync::Arc;

use marrow_desktop::{scratch, Core};

/// A core whose data directory is a temporary one, so the scratch workspace it
/// creates is temporary too.
///
/// The directory is returned rather than leaked: every test here has to be able
/// to look at the scratch folder on disk, which is most of what is under test.
fn core() -> (tempfile::TempDir, Arc<Core>) {
    let data = tempfile::tempdir().expect("data dir");
    let core = Core::open(data.path().join("marrow.db")).expect("core");
    (data, Arc::new(core))
}

/// A file somewhere else entirely — the desktop, in the story this exists for.
fn loose_file(name: &str, body: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("elsewhere");
    let path = dir.path().join(name);
    std::fs::write(&path, body).expect("write");
    (dir, path)
}

#[test]
fn a_dropped_file_is_searchable_immediately_and_citable() {
    // The whole point of the feature. Nothing is asked of the user between the
    // drop and the search — no index run, no waiting for a watcher tick.
    let (_data, core) = core();
    let (_elsewhere, source) = loose_file(
        "lease.md",
        "# Unit 7B\n\nThe agreement renews on 31 December 2031.\n",
    );

    let report = scratch::accept(&core, None, std::slice::from_ref(&source)).expect("accept");
    assert_eq!(report.added, vec!["lease.md".to_string()]);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);

    let found = core.search("renews", 10).expect("search");
    assert_eq!(found.hits.len(), 1, "the dropped file is not searchable");

    // **Invariant #9, applied the right way round.** A copied file is the
    // *user's* file; `origin = SELF` is for content this system generated and
    // it bars that content from supporting a claim. Marking a dropped file SELF
    // because Marrow wrote the bytes would make every dropped file silently
    // uncitable — found by search and never usable in an answer.
    assert!(
        found.hits[0].citable,
        "a dropped file is the user's and must be able to support a claim"
    );
}

#[test]
fn the_scratch_workspace_is_an_ordinary_workspace_with_its_own_counts() {
    // Registered as a root like any other, so nothing downstream needs a
    // special case for "temporary" content. The one thing that is special is
    // the flag, and it exists so the *window* can tell them apart — not so the
    // index can.
    let (_data, core) = core();
    let (_elsewhere, source) = loose_file("note.md", "a quarterly renewal clause\n");
    scratch::accept(&core, None, std::slice::from_ref(&source)).expect("accept");

    let rows = core.workspaces().expect("workspaces");
    assert_eq!(rows.len(), 1);
    assert!(rows[0].scratch, "the scratch workspace is flagged as one");
    assert_eq!(rows[0].name, scratch::WORKSPACE_NAME);
    assert_eq!(rows[0].files, 1);
    assert!(rows[0].chunks > 0, "it was parsed, not only recorded");
}

#[test]
fn nothing_exists_until_the_first_drop() {
    // The first-run flow decides from real state — "has this user got anywhere
    // yet?" — rather than from a flag. A scratch workspace conjured at startup
    // would answer "yes" for someone who has never done anything.
    let (data, core) = core();
    assert!(core.workspaces().expect("workspaces").is_empty());

    let status = scratch::status(&core).expect("status");
    assert!(!status.exists);
    assert_eq!(
        status.path, None,
        "the window is never shown a guessed path"
    );
    assert!(!scratch::dir_in(data.path()).exists());
}

#[test]
fn emptying_it_removes_the_copies_and_the_search_stops_finding_them() {
    // "Temporary" here means the user can throw it away, not that it throws
    // itself away. This is the throwing away.
    let (data, core) = core();
    let (_elsewhere, source) = loose_file("receipt.md", "paid 2,417 on the first working day\n");
    scratch::accept(&core, None, std::slice::from_ref(&source)).expect("accept");
    assert_eq!(core.search("2,417", 10).expect("search").hits.len(), 1);

    let cleared = scratch::clear(&core).expect("clear");
    assert_eq!(cleared.removed, vec!["receipt.md".to_string()]);
    assert!(cleared.bytes > 0);

    assert!(
        core.search("2,417", 10).expect("search").hits.is_empty(),
        "an index that still answers from a file that is gone is the failure \
         this whole module exists to avoid"
    );
    assert!(!scratch::dir_in(data.path()).join("receipt.md").exists());

    // The root stays granted and the workspace stays listed, empty. Re-granting
    // on the next drop would put a second row for one directory in every
    // listing that ran in between, and an empty workspace is truthful.
    let rows = core.workspaces().expect("workspaces");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].files, 0);

    // And it can be used again straight afterwards.
    let again = scratch::accept(&core, None, std::slice::from_ref(&source)).expect("accept again");
    assert_eq!(again.added, vec!["receipt.md".to_string()]);
}

#[test]
fn emptying_an_untouched_install_is_not_an_error() {
    // The button is on the Settings card whether or not anything has been
    // dropped. A control that throws when there is nothing to do is a control
    // people learn to avoid.
    let (_data, core) = core();
    let cleared = scratch::clear(&core).expect("clear");
    assert!(cleared.removed.is_empty());
    assert_eq!(cleared.bytes, 0);
}

#[test]
fn the_same_file_dropped_twice_is_not_copied_twice() {
    // Two files with one content hash is expected — dedup is a feature — and
    // spending the disk to say the same thing twice is not.
    let (_data, core) = core();
    let (_elsewhere, source) = loose_file("dup.md", "the same words either way\n");

    scratch::accept(&core, None, std::slice::from_ref(&source)).expect("first");
    let second = scratch::accept(&core, None, std::slice::from_ref(&source)).expect("second");

    assert!(second.added.is_empty());
    assert_eq!(second.already_there, vec!["dup.md".to_string()]);
    assert_eq!(scratch::status(&core).expect("status").files, 1);
}

#[test]
fn a_file_that_is_already_indexed_somewhere_is_not_copied_in() {
    // Copying it would store one document twice under two identities, which is
    // the same reason `Core::grant` refuses overlapping roots (invariant #2).
    // The containment test is component-wise, not a string prefix.
    let (_data, core) = core();
    let granted = tempfile::tempdir().expect("granted");
    let inside = granted.path().join("already.md");
    std::fs::write(&inside, "indexed where it lives\n").expect("write");
    core.grant(granted.path()).expect("grant");

    let report = scratch::accept(&core, None, std::slice::from_ref(&inside)).expect("accept");
    assert!(report.added.is_empty());
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].code, "ACT_ALREADY_EXISTS");
    assert!(
        report.skipped[0].reason.contains(
            granted
                .path()
                .file_name()
                .expect("name")
                .to_str()
                .expect("utf-8")
        ),
        "the refusal must name the workspace it is already in: {}",
        report.skipped[0].reason
    );
}

#[test]
fn a_dropped_folder_is_refused_and_points_at_the_grant_that_does_work() {
    // Copying a whole tree into scratch would duplicate it. A folder is
    // indexed where it already is, and the refusal has to say so — "that did
    // not work" with no way forward is the shape of failure this app keeps
    // being audited for.
    let (_data, core) = core();
    let folder = tempfile::tempdir().expect("a folder");
    let report =
        scratch::accept(&core, None, &[folder.path().to_path_buf()]).expect("accept the batch");

    assert!(report.added.is_empty());
    assert_eq!(report.skipped.len(), 1);
    assert!(
        report.skipped[0].reason.contains("Add a folder"),
        "{}",
        report.skipped[0].reason
    );
}

#[test]
fn one_bad_file_does_not_cost_the_rest_of_the_drop() {
    // Three files and one folder should index three files and explain one, not
    // refuse four.
    let (_data, core) = core();
    let elsewhere = tempfile::tempdir().expect("elsewhere");
    let mut sources = Vec::new();
    for name in ["one.md", "two.md", "three.md"] {
        let p = elsewhere.path().join(name);
        std::fs::write(&p, format!("contents of {name}\n")).expect("write");
        sources.push(p);
    }
    let folder = elsewhere.path().join("a-folder");
    std::fs::create_dir(&folder).expect("mkdir");
    sources.push(folder);

    let report = scratch::accept(&core, None, &sources).expect("accept");
    assert_eq!(report.added.len(), 3, "{:?}", report);
    assert_eq!(report.skipped.len(), 1);
}

#[test]
fn a_file_over_the_per_file_cap_is_refused_with_its_size() {
    // The cap is the price of copying rather than referencing, and a refusal
    // that does not say the number cannot be argued with.
    let (_data, core) = core();
    let elsewhere = tempfile::tempdir().expect("elsewhere");
    let big = elsewhere.path().join("huge.bin");
    std::fs::write(&big, vec![0u8; (scratch::MAX_FILE_BYTES + 1) as usize]).expect("write");

    let report = scratch::accept(&core, None, &[big]).expect("accept");
    assert!(report.added.is_empty());
    assert_eq!(report.skipped.len(), 1);
    assert!(
        report.skipped[0].reason.contains("64 MB"),
        "{}",
        report.skipped[0].reason
    );
}
