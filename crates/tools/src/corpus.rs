//! The adversarial corpus: the gate every write tool has to pass.
//!
//! CLAUDE.md: *"the adversarial corpus must be green before any write tool
//! ships, and only ever grows."* The cases live in `corpus/adversarial/*.toml`
//! as **data**, not code, so a case found in the wild can be added by writing
//! six lines of TOML — no recompilation of the suite, no new test function to
//! forget to call. `every_adversarial_case_produces_its_expected_refusal` runs
//! all of them.
//!
//! Each case declares what it attacks, what it does, and the **exact error
//! code** it must produce. "Should fail" is not an expectation: a traversal
//! that is refused as `POL_DENIED` because the filename happened to be too
//! long has not tested containment at all.
//!
//! ```toml
//! [[case]]
//! id      = "traversal-parent"
//! attacks = "path traversal"
//! why     = "`..` arrives as data, from a model that read it in a document."
//! input   = { op = "create_file", path = "../escape.md", body = "x" }
//! expect  = { outcome = "refused", code = "FS_PATH_ESCAPE_BLOCKED" }
//! ```
//!
//! The sandbox each case runs in:
//!
//! ```text
//! <tmp>/
//! ├── workspace/   the workspace root — the only place a write may land
//! └── outside/     everything a case is trying to reach
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use marrow_core::{Code, ContentHash, Error, Origin, Result};
use serde::Deserialize;

use crate::create::{self, CreateDiagram, CreateFile, CreatePage};
use crate::guard::{Expect, Workspace};

/// One case. Every field is prose a human reviews, or data the runner executes.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    /// Stable, unique, kebab-case. Appears in failure output.
    pub id: String,
    /// The class of attack, from the vocabulary the coverage test asserts on.
    pub attacks: String,
    /// Why this must be refused — the sentence that survives when someone is
    /// tempted to relax the rule.
    pub why: String,
    /// The filesystem this case needs before it runs.
    #[serde(default)]
    pub setup: Vec<Setup>,
    /// Workspace-relative subtrees to protect, e.g. the model directory.
    #[serde(default)]
    pub protect: Vec<String>,
    /// What a racing process does after validation and before the write.
    #[serde(default)]
    pub race: Option<Race>,
    pub input: Input,
    pub expect: Expectation,
}

/// A piece of the world a case needs. `path` is workspace-relative except for
/// the `outside_*` kinds, which are relative to the sandbox's `outside/`.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Setup {
    Dir {
        path: String,
    },
    File {
        path: String,
        contents: String,
    },
    /// A symbolic link inside the workspace. `to` is relative to the sandbox,
    /// so `outside/secrets` points out of the workspace and `workspace/real.md`
    /// points within it.
    Symlink {
        path: String,
        to: String,
    },
    OutsideDir {
        path: String,
    },
    OutsideFile {
        path: String,
        contents: String,
    },
}

/// What the caller claims is at the target, expressed without knowing the
/// digest — the runner computes it when the case runs.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Precondition {
    /// "Nothing is there."
    #[default]
    New,
    /// "I read the file, and this is what it said" — digest taken from disk as
    /// the case starts, which is what an honest caller would have.
    Current,
    /// "I read the file and it said *this*" — for the caller holding a digest
    /// that is already out of date.
    DigestOf(String),
}

/// What a racing process does between validation and the write. The one thing
/// a corpus case cannot express as a static filesystem.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Race {
    /// The destination directory is replaced by a symlink pointing out of the
    /// workspace.
    ParentBecomesSymlinkOut,
    /// The target file is replaced by a symlink pointing out of the workspace.
    TargetBecomesSymlinkOut,
    /// Someone edits the target file — the editor the user has open.
    TargetContentChanges,
}

/// The tool call under test.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Input {
    CreateFile {
        path: String,
        #[serde(default)]
        body: String,
        #[serde(default)]
        precondition: Precondition,
    },
    CreateDiagram {
        path: String,
        mermaid: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        precondition: Precondition,
    },
    CreatePage {
        path: String,
        title: String,
        body: String,
        #[serde(default)]
        precondition: Precondition,
    },
}

/// What must happen. A refusal names its code; there is no "it errors somehow".
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum Expectation {
    Refused {
        /// Wire form from `marrow_core::Code`, e.g. `FS_PATH_ESCAPE_BLOCKED`.
        code: String,
        /// Distinguishes two refusals that share a code. Required wherever the
        /// code alone would let a wrong-but-refusing implementation pass.
        #[serde(default)]
        message_contains: Option<String>,
    },
    /// The write is allowed — and is still marked `origin = SELF`, which the
    /// runner asserts on every one of these.
    Written {
        /// Workspace-relative path the bytes must land at.
        at: String,
        /// Text the written file must contain.
        #[serde(default)]
        contains: Option<String>,
    },
}

/// A case that did not do what it said it would.
#[derive(Clone, Debug)]
pub struct Mismatch {
    pub case_id: String,
    pub detail: String,
}

impl std::fmt::Display for Mismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.case_id, self.detail)
    }
}

/// The file `Race::TargetBecomesSymlinkOut` points its symlink at.
const DECOY: &str = "swapped-in.txt";

/// Where the corpus lives, relative to this crate.
pub fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/adversarial")
}

/// Load every `*.toml` case file in `dir`.
///
/// A malformed case file is an error, never a skip: a corpus that silently
/// drops the file it cannot parse is a corpus that reports green while testing
/// nothing.
pub fn load_dir(dir: &Path) -> Result<Vec<Case>> {
    #[derive(Deserialize)]
    struct CaseFile {
        #[serde(default)]
        case: Vec<Case>,
    }

    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| {
            Error::from(e).with_context(format!("adversarial corpus at {}", dir.display()))
        })?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .collect();
    files.sort();

    let mut cases = Vec::new();
    for path in files {
        let text = fs::read_to_string(&path)
            .map_err(|e| Error::from(e).with_context(path.display().to_string()))?;
        let parsed: CaseFile = toml::from_str(&text).map_err(|e| {
            Error::new(
                Code::CfgInvalid,
                "An adversarial corpus file could not be parsed, so the cases in it would \
                 not run. Fix the TOML — a corpus that skips what it cannot read reports \
                 green while testing nothing.",
            )
            .with_context(format!("{}: {e}", path.display()))
        })?;
        cases.extend(parsed.case);
    }
    Ok(cases)
}

/// Run one case in `sandbox`, which must be an empty directory of its own.
pub fn run_case(case: &Case, sandbox: &Path) -> std::result::Result<(), Mismatch> {
    execute(case, sandbox).map_err(|detail| Mismatch {
        case_id: case.id.clone(),
        detail,
    })
}

fn execute(case: &Case, sandbox: &Path) -> std::result::Result<(), String> {
    mkdir(&sandbox.join("workspace"))?;
    mkdir(&sandbox.join("outside"))?;
    // Canonical from here on: on macOS a temp directory is reached through
    // `/var`, which is a symlink to `/private/var`, and the guard reports the
    // resolved path. Comparing the two spellings would fail every `written`
    // case for a reason that has nothing to do with what is being tested.
    let sandbox =
        &fs::canonicalize(sandbox).map_err(|e| format!("canonicalising the sandbox: {e}"))?;
    let root = sandbox.join("workspace");
    let outside = sandbox.join("outside");

    for step in &case.setup {
        apply(step, &root, &outside, sandbox)?;
    }
    if matches!(case.race, Some(Race::TargetBecomesSymlinkOut)) {
        // Staged before the snapshot below, so the decoy the race points at is
        // not mistaken for something the write leaked.
        write(&outside.join(DECOY), "a file the write must never reach")?;
    }
    // What is outside the workspace before the tool runs. Anything that
    // appears after it is a leak, whatever the refusal said.
    let before = walk_names(&outside);

    let mut ws = Workspace::open(&root).map_err(|e| format!("opening the workspace: {e}"))?;
    for p in &case.protect {
        ws = ws.protect(p);
    }
    if let Some(race) = case.race {
        ws = ws.with_race(race_hook(race, outside.clone()));
    }

    let expect = precondition(case.input.precondition(), &root, case.input.path())?;
    let outcome = match &case.input {
        Input::CreateFile { path, body, .. } => create::create_file(
            &ws,
            &CreateFile {
                path: path.clone(),
                body: body.clone(),
                expect,
            },
        ),
        Input::CreateDiagram {
            path,
            mermaid,
            title,
            ..
        } => create::create_diagram(
            &ws,
            &CreateDiagram {
                path: path.clone(),
                mermaid: mermaid.clone(),
                title: title.clone(),
                expect,
            },
        ),
        Input::CreatePage {
            path, title, body, ..
        } => create::create_page(
            &ws,
            &CreatePage {
                path: path.clone(),
                title: title.clone(),
                body: body.clone(),
                expect,
            },
        ),
    };

    match (&case.expect, outcome) {
        (Expectation::Refused { code, .. }, Ok(w)) => Err(format!(
            "expected refusal {code}, but the write succeeded at {}",
            w.path().display()
        )),
        (
            Expectation::Refused {
                code,
                message_contains,
            },
            Err(e),
        ) => {
            let expected = Code::from_wire(code).ok_or_else(|| {
                format!("`{code}` is not a code in marrow_core::Code — fix the case")
            })?;
            if e.code() != expected {
                return Err(format!("expected {expected}, got {e}"));
            }
            if let Some(needle) = message_contains {
                let haystack = format!("{} {}", e.message(), e.context().unwrap_or(""));
                if !haystack.contains(needle.as_str()) {
                    return Err(format!(
                        "refused with the right code but the wrong reason: \
                         expected a message containing {needle:?}, got {e}"
                    ));
                }
            }
            // Nothing may reach the outside directory, whatever the refusal
            // said. This is the assertion that catches "refused, but only
            // after writing the file".
            let leaked: Vec<String> = walk_names(&outside)
                .into_iter()
                .filter(|n| !before.contains(n))
                .collect();
            if !leaked.is_empty() {
                return Err(format!(
                    "refused correctly, but these appeared outside the workspace: {leaked:?}"
                ));
            }
            Ok(())
        }
        (Expectation::Written { at, .. }, Err(e)) => {
            Err(format!("expected a write at `{at}`, got refusal {e}"))
        }
        (Expectation::Written { at, contains }, Ok(w)) => {
            let expected_path = root.join(at);
            if w.path() != expected_path {
                return Err(format!(
                    "written to {} but the case says {}",
                    w.path().display(),
                    expected_path.display()
                ));
            }
            let text = fs::read_to_string(w.path())
                .map_err(|e| format!("reading back {}: {e}", w.path().display()))?;
            if ContentHash::of(text.as_bytes()) != w.digest() {
                return Err("the reported digest does not match the bytes on disk".into());
            }
            if let Some(needle) = contains {
                if !text.contains(needle.as_str()) {
                    return Err(format!("written file does not contain {needle:?}"));
                }
            }
            // **`origin = SELF`, asserted on every allowed write in the corpus.**
            // A write tool that forgets this turns the agent's own notes into
            // corroborating evidence for the agent's own claims.
            if w.origin() != Origin::SelfWritten || w.can_support_a_claim() {
                return Err(format!(
                    "written with origin {:?} — everything this crate writes must be \
                     SELF and uncitable",
                    w.origin()
                ));
            }
            Ok(())
        }
    }
}

impl Input {
    fn path(&self) -> &str {
        match self {
            Input::CreateFile { path, .. }
            | Input::CreateDiagram { path, .. }
            | Input::CreatePage { path, .. } => path,
        }
    }

    fn precondition(&self) -> &Precondition {
        match self {
            Input::CreateFile { precondition, .. }
            | Input::CreateDiagram { precondition, .. }
            | Input::CreatePage { precondition, .. } => precondition,
        }
    }
}

/// Turn a case's symbolic precondition into the digest the tool takes.
fn precondition(
    p: &Precondition,
    root: &Path,
    relative: &str,
) -> std::result::Result<Expect, String> {
    Ok(match p {
        Precondition::New => Expect::New,
        Precondition::Current => {
            let target = root.join(relative);
            let bytes = fs::read(&target).map_err(|e| {
                format!(
                    "`precondition = \"current\"` needs {} to exist and be readable: {e}",
                    target.display()
                )
            })?;
            Expect::Replacing(ContentHash::of(&bytes))
        }
        Precondition::DigestOf(text) => Expect::Replacing(ContentHash::of(text.as_bytes())),
    })
}

/// The attacks a static filesystem cannot express: what another process does
/// while the tool is between its checks and its `rename`.
fn race_hook(race: Race, outside: PathBuf) -> crate::guard::RaceHook {
    Box::new(move |target: &Path| {
        // A failure here means the case cannot run at all, and it is a bug in
        // the case rather than in the guard. Traced, not panicked: the
        // assertion that follows reports it as a mismatch with context.
        let result = match race {
            Race::ParentBecomesSymlinkOut => target.parent().map_or(Ok(()), |parent| {
                fs::remove_dir_all(parent)
                    .and_then(|()| std::os::unix::fs::symlink(&outside, parent))
            }),
            Race::TargetBecomesSymlinkOut => {
                // The decoy is staged by the runner before the call, so that
                // its appearance is not read as something the write leaked.
                let decoy = outside.join(DECOY);
                match fs::remove_file(target) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                    _ => Ok(()),
                }
                .and_then(|()| std::os::unix::fs::symlink(&decoy, target))
            }
            Race::TargetContentChanges => fs::write(
                target,
                b"edited in another window while the tool was working",
            ),
        };
        if let Err(e) = result {
            tracing::error!(race = ?race, error = %e, "corpus race could not be staged");
        }
    })
}

fn apply(
    step: &Setup,
    root: &Path,
    outside: &Path,
    sandbox: &Path,
) -> std::result::Result<(), String> {
    match step {
        Setup::Dir { path } => mkdir(&root.join(path)),
        Setup::OutsideDir { path } => mkdir(&outside.join(path)),
        Setup::File { path, contents } => write(&root.join(path), contents),
        Setup::OutsideFile { path, contents } => write(&outside.join(path), contents),
        Setup::Symlink { path, to } => {
            let link = root.join(path);
            if let Some(parent) = link.parent() {
                mkdir(parent)?;
            }
            std::os::unix::fs::symlink(sandbox.join(to), &link)
                .map_err(|e| format!("setup: symlink {}: {e}", link.display()))
        }
    }
}

fn mkdir(p: &Path) -> std::result::Result<(), String> {
    fs::create_dir_all(p).map_err(|e| format!("setup: mkdir {}: {e}", p.display()))
}

fn write(p: &Path, contents: &str) -> std::result::Result<(), String> {
    if let Some(parent) = p.parent() {
        mkdir(parent)?;
    }
    fs::write(p, contents).map_err(|e| format!("setup: write {}: {e}", p.display()))
}

/// Every entry below `dir`, as display strings. Used to prove nothing leaked.
fn walk_names(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        if path.is_dir() {
            out.extend(walk_names(&path));
        } else {
            out.push(path.display().to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn corpus() -> Vec<Case> {
        load_dir(&corpus_dir()).expect("the corpus must load")
    }

    #[test]
    fn every_adversarial_case_produces_its_expected_refusal() {
        // The gate. CLAUDE.md: green before any write tool ships.
        let cases = corpus();
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut failures = Vec::new();
        for case in &cases {
            let sandbox = tmp.path().join(&case.id);
            std::fs::create_dir_all(&sandbox).expect("sandbox");
            if let Err(m) = run_case(case, &sandbox) {
                failures.push(format!("{m}\n    (case: {})", case.why));
            }
        }
        assert!(
            failures.is_empty(),
            "{} of {} adversarial cases failed:\n  - {}",
            failures.len(),
            cases.len(),
            failures.join("\n  - ")
        );
    }

    #[test]
    fn the_corpus_only_ever_grows() {
        // CLAUDE.md, verbatim. Raise this floor when you add cases; a change
        // that lowers it is deleting a defence someone found the hard way.
        let cases = corpus();
        assert!(
            cases.len() >= 59,
            "the corpus has shrunk to {} cases",
            cases.len()
        );
    }

    #[test]
    fn every_case_has_a_unique_id_and_explains_itself() {
        // Ids appear in failure output and in commit messages; two cases with
        // one id makes a failure unattributable.
        let cases = corpus();
        let ids: BTreeSet<&str> = cases.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids.len(), cases.len(), "duplicate case id");
        for c in &cases {
            assert!(
                c.why.len() > 30,
                "case `{}` does not say why the rule exists",
                c.id
            );
            assert!(
                c.id.chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-'),
                "case id `{}` is not kebab-case",
                c.id
            );
        }
    }

    #[test]
    fn the_corpus_covers_every_attack_class_the_write_path_claims_to_survive() {
        // A defence with no case is a comment. This list is the contract in
        // CLAUDE.md and Part 7 §126 turned into an assertion.
        let cases = corpus();
        let present: BTreeSet<&str> = cases.iter().map(|c| c.attacks.as_str()).collect();
        for required in [
            "path traversal",
            "symlink escape",
            "toctou",
            "unicode normalisation",
            "case collision",
            "name mangling",
            "protected directory",
            "stale write",
            "cloud placeholder",
            "self-poisoning",
            "injection",
        ] {
            assert!(
                present.contains(required),
                "no corpus case attacks `{required}`"
            );
        }
    }

    #[test]
    fn a_case_expecting_the_wrong_code_fails_rather_than_passing_quietly() {
        // The runner is the thing every other assertion depends on. If a
        // mismatch could be reported as success, the whole gate is decorative.
        let tmp = tempfile::tempdir().expect("tempdir");
        let case = Case {
            id: "self-check".into(),
            attacks: "path traversal".into(),
            why: "the runner must not report a mismatch as a pass".into(),
            setup: Vec::new(),
            protect: Vec::new(),
            race: None,
            input: Input::CreateFile {
                path: "../escape.md".into(),
                body: "x".into(),
                precondition: Precondition::New,
            },
            expect: Expectation::Refused {
                code: "DB_BUSY".into(),
                message_contains: None,
            },
        };
        let m = run_case(&case, tmp.path()).expect_err("must not pass");
        assert!(m.detail.contains("DB_BUSY"), "{m}");
    }

    #[test]
    fn a_case_naming_a_code_that_does_not_exist_is_an_error_not_a_pass() {
        // Codes drift; a typo in a case file must be loud.
        let tmp = tempfile::tempdir().expect("tempdir");
        let case = Case {
            id: "self-check-2".into(),
            attacks: "path traversal".into(),
            why: "a typo in a case file must fail the suite, not skip the case".into(),
            setup: Vec::new(),
            protect: Vec::new(),
            race: None,
            input: Input::CreateFile {
                path: "../escape.md".into(),
                body: "x".into(),
                precondition: Precondition::New,
            },
            expect: Expectation::Refused {
                code: "FS_PATH_ESCAPE".into(),
                message_contains: None,
            },
        };
        let m = run_case(&case, tmp.path()).expect_err("must not pass");
        assert!(m.detail.contains("not a code"), "{m}");
    }

    #[test]
    fn a_malformed_case_file_fails_the_load_rather_than_being_skipped() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("broken.toml"), "[[case]]\nid = ").expect("write");
        let e = load_dir(tmp.path()).expect_err("must not load");
        assert_eq!(e.code(), Code::CfgInvalid);
    }
}
