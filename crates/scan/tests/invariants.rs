//! End-to-end checks against the public API only.
//!
//! The unit tests inside each module check the pieces. These check that the
//! pieces still hold the invariants once composed the way a caller composes
//! them: walk a root, probe what comes out, hash what is safe to hash.

use std::collections::BTreeMap;
use std::fs;

use marrow_core::{Code, TierState};
use marrow_scan::{hash, path::AuthorizedRoot, walk, ScanEvent, WalkPolicy};

/// A tree with one of everything this crate has to survive.
fn fixture() -> (tempfile::TempDir, AuthorizedRoot) {
    let td = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(td.path()).unwrap();

    fs::create_dir(base.join("root")).unwrap();
    let root = base.join("root");

    fs::write(root.join("notes.md"), b"# real content").unwrap();
    fs::create_dir(root.join("src")).unwrap();
    fs::write(root.join("src/lib.rs"), b"fn main() {}").unwrap();

    // Cloud placeholder: an iCloud stub standing in for an evicted file.
    fs::write(root.join(".Quarterly.xlsx.icloud"), b"stub plist").unwrap();

    // Noise that must be pruned.
    fs::create_dir_all(root.join("node_modules/left-pad")).unwrap();
    fs::write(root.join("node_modules/left-pad/index.js"), b"x").unwrap();
    fs::create_dir_all(root.join("target/debug")).unwrap();
    fs::write(root.join("target/debug/blob"), b"x").unwrap();

    // A symlink out of the root, as a cloned repo pointing at ~/.ssh would be.
    fs::create_dir(base.join("secrets")).unwrap();
    fs::write(base.join("secrets/id_rsa"), b"PRIVATE").unwrap();
    std::os::unix::fs::symlink(base.join("secrets"), root.join("ssh")).unwrap();

    // The same name in both Unicode forms.
    fs::write(root.join("cafe\u{301}.txt"), b"latte").unwrap();

    let authorized = AuthorizedRoot::open(&root).unwrap();
    (td, authorized)
}

/// Walk, then hash everything the scan says is readable — the exact loop an
/// indexer runs. Nothing outside the root, and no placeholder, may be opened.
#[test]
fn a_full_scan_hashes_only_resident_files_inside_the_root() {
    let (_td, root) = fixture();

    let mut hashed = BTreeMap::new();
    let mut skipped = Vec::new();

    for event in walk::walk(&root, &WalkPolicy::default()) {
        let entry = match event {
            ScanEvent::Entry(e) => e,
            ScanEvent::Failed(e) => {
                skipped.push(e.code());
                continue;
            }
        };
        if !entry.facts.readable() {
            skipped.push(Code::FsPlaceholderSkipped);
            continue;
        }
        // Invariant #7 at operation time, not at index time.
        let safe = root
            .resolve(&entry.path)
            .expect("entry must resolve inside");
        safe.reverify(&root)
            .expect("still inside at operation time");

        let digest = hash::hash_file_with_tier(safe.as_path(), entry.facts.tier).unwrap();
        hashed.insert(
            entry
                .path
                .strip_prefix(root.path())
                .unwrap()
                .display()
                .to_string(),
            digest,
        );
    }

    let names: Vec<&str> = hashed.keys().map(String::as_str).collect();
    // Note the *decomposed* spelling: the filesystem hands back the form it
    // stores, which on macOS is NFD even though the literal below was written
    // as NFD deliberately. Normalising is `path_key`'s job, not the walk's —
    // raw paths keep whatever spelling the volume uses.
    assert_eq!(
        names,
        vec!["cafe\u{301}.txt", "notes.md", "src/lib.rs"],
        "unexpected set of hashed files"
    );
    assert!(
        !names.iter().any(|n| n.contains("icloud")),
        "a cloud placeholder was hashed"
    );
    assert!(
        !names.iter().any(|n| n.contains("id_rsa")),
        "content outside the root was hashed"
    );
    assert!(
        !names
            .iter()
            .any(|n| n.starts_with("node_modules") || n.starts_with("target")),
        "noise directories were not pruned"
    );
}

#[test]
fn placeholder_never_hydrated() {
    let (_td, root) = fixture();

    let stub = walk::walk(&root, &WalkPolicy::default())
        .filter_map(ScanEvent::entry)
        .find(|e| e.path.ends_with(".Quarterly.xlsx.icloud"))
        .expect("the stub must be discoverable — it is the only evidence the file exists");

    assert_eq!(stub.facts.tier, TierState::Placeholder);
    assert!(!stub.facts.readable());
    // Metadata is still available for a placeholder: TIER-005 indexes it, and
    // TIER-008 counts it.
    assert!(stub.facts.size > 0);

    let err = hash::hash_file(&stub.path).unwrap_err();
    assert_eq!(err.code(), Code::FsPlaceholderSkipped);
    assert!(!err.retryable());
}

#[test]
fn symlink_escape_blocked() {
    let (_td, root) = fixture();
    let err = root.resolve("ssh/id_rsa").unwrap_err();
    assert_eq!(err.code(), Code::FsPathEscapeBlocked);
}

#[test]
fn nfc_nfd_single_identity() {
    let (_td, root) = fixture();

    let nfd = root.resolve("cafe\u{301}.txt").unwrap();
    let nfc = root.resolve("caf\u{e9}.txt").unwrap();
    assert_eq!(nfd.key().unwrap(), nfc.key().unwrap());

    // And the walk agrees with both.
    let discovered = walk::walk(&root, &WalkPolicy::default())
        .filter_map(ScanEvent::entry)
        .find(|e| e.path.extension().is_some_and(|x| x == "txt"))
        .unwrap();
    assert_eq!(
        marrow_scan::path_key(&discovered.path).unwrap(),
        nfc.key().unwrap()
    );
}
