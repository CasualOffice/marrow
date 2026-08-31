//! The model workspace (Part 8 §144).
//!
//! ```text
//! <AppData>/marrow/models/
//! ├── catalogue.json          built-in, read-only
//! ├── weights/<sha256>/       content-addressed; the SHA is the name
//! ├── partial/<sha256>/       resumable downloads; never loadable
//! └── scratch/<request_id>/   per-request working directory
//! ```
//!
//! The rule that decides the layout: **scratch is outside every workspace
//! root** (SUP-011). A model writing into an indexed folder would have its own
//! output re-indexed and cited back — the `origin = SELF` rule, and the reason `Origin`
//! exists in `marrow-core`.

use std::fs;
use std::path::{Path, PathBuf};

use marrow_core::{Code, Error, RequestId, Result};

/// Default per-request scratch cap. Exceeding it fails the request rather than
/// filling the disk (SUP-012).
pub const DEFAULT_SCRATCH_CAP: u64 = 256 * 1024 * 1024;

/// The model area on disk.
#[derive(Clone, Debug)]
pub struct ModelWorkspace {
    root: PathBuf,
    scratch_cap: u64,
}

impl ModelWorkspace {
    /// Open (creating if needed) the model area under `root`.
    ///
    /// `indexed_roots` are the workspace roots. Placing the model area inside
    /// one is refused outright rather than warned about: by the time a warning
    /// is read, the model's scratch output is already in the index.
    pub fn open(root: impl Into<PathBuf>, indexed_roots: &[PathBuf]) -> Result<Self> {
        let root = root.into();
        for r in indexed_roots {
            if within(&root, r) {
                return Err(Error::new(
                    Code::CfgInvalid,
                    "The model workspace cannot live inside an indexed folder: \
                     the model's own output would be indexed and cited back. \
                     Move it outside every workspace root.",
                )
                .with_context(format!(
                    "{} is inside {}",
                    root.display(),
                    r.display()
                )));
            }
        }
        for sub in ["weights", "partial", "scratch"] {
            fs::create_dir_all(root.join(sub))?;
        }
        Ok(Self {
            root,
            scratch_cap: DEFAULT_SCRATCH_CAP,
        })
    }

    pub fn with_scratch_cap(mut self, bytes: u64) -> Self {
        self.scratch_cap = bytes;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where verified weights live. The digest is the directory name, so a
    /// corrupt download cannot masquerade as a good one and two models never
    /// collide (SUP-014).
    pub fn weights_dir(&self, sha256: &str) -> PathBuf {
        self.root.join("weights").join(sha256)
    }

    /// Where a download in progress lives. Separate from `weights/`, so a
    /// partial file is **never loadable** (LLM-027) — not "loadable and then
    /// checked", which is a race with a 3 GB window.
    pub fn partial_dir(&self, sha256: &str) -> PathBuf {
        self.root.join("partial").join(sha256)
    }

    pub fn is_installed(&self, sha256: &str) -> bool {
        self.weights_dir(sha256).is_dir()
    }

    /// A per-request working directory, removed when the guard drops —
    /// including on cancel and on panic (SUP-010).
    pub fn scratch(&self, id: RequestId) -> Result<Scratch> {
        let path = self.root.join("scratch").join(id.to_string());
        fs::create_dir_all(&path)?;
        Ok(Scratch {
            path,
            cap: self.scratch_cap,
        })
    }

    /// Remove scratch left behind by a previous crash (SUP-015).
    ///
    /// Runs at startup, before anything can create new scratch — otherwise it
    /// would race the directory it just handed out.
    pub fn clean_orphaned_scratch(&self) -> Result<usize> {
        let dir = self.root.join("scratch");
        let mut removed = 0;
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                fs::remove_dir_all(entry.path())?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Remove a partial download. Called when its digest stops matching the
    /// registry entry — a resumed download must never append to bytes fetched
    /// for a different file (LLM-037).
    pub fn discard_partial(&self, sha256: &str) -> Result<()> {
        let p = self.partial_dir(sha256);
        if p.exists() {
            fs::remove_dir_all(p)?;
        }
        Ok(())
    }

    /// Delete an installed model's weights, and any partial download beside
    /// them. Returns how many bytes went.
    ///
    /// **There was no way to remove a model at all.** The Models page could
    /// start a 3.1 GB download and nothing could undo it from inside the app,
    /// so the only remedy was to find the directory by hand — on a machine
    /// where a full disk had already stopped SQLite writing once.
    ///
    /// The digest is the directory name, so this can only ever remove a path
    /// this type composed itself: the id never reaches the filesystem, and a
    /// caller cannot walk out of the weights directory with one. Removing a
    /// model that is not installed is not an error — the end state the caller
    /// asked for is the end state they get, and a second click on a button
    /// that already worked should not raise.
    pub fn delete_weights(&self, sha256: &str) -> Result<u64> {
        let dir = self.weights_dir(sha256);
        let freed = dir_size(&dir);
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
        // A half-finished download of the same model is the same model.
        // Leaving it would make "delete" free nothing on a cancelled fetch.
        self.discard_partial(sha256)?;
        Ok(freed)
    }
}

/// A request's working directory. Removed on drop.
#[derive(Debug)]
pub struct Scratch {
    path: PathBuf,
    cap: u64,
}

impl Scratch {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Bytes currently used.
    pub fn used(&self) -> u64 {
        dir_size(&self.path)
    }

    /// Check the cap. Called before a write, and it **fails the request** —
    /// filling the disk is not an acceptable way to discover a runaway model
    /// (SUP-012).
    pub fn check_cap(&self) -> Result<()> {
        let used = self.used();
        if used > self.cap {
            return Err(Error::new(
                Code::ModScratchExceeded,
                format!(
                    "The model wrote {} MB to its working directory, over the {} MB limit. \
                     The request was stopped.",
                    used / 1_000_000,
                    self.cap / 1_000_000
                ),
            ));
        }
        Ok(())
    }

    /// Resolve a path the model asked for, refusing anything outside scratch.
    ///
    /// SUP-013: a worker gets scratch and its weights, and nothing else. A
    /// relative `../../` is the whole attack, and it arrives as data.
    ///
    /// **Canonicalized, not folded.** An earlier version of this resolved `..`
    /// as a string and compared prefixes, which the operation-time symlink check says outright is
    /// not sufficient — and it is not: a worker that creates a symlink *inside
    /// its own scratch* pointing at `~/.ssh` and then resolves a path through
    /// it escapes cleanly, because lexical folding says `link/../x` is `x`
    /// while the kernel says it is `x` relative to the symlink's target.
    ///
    /// The file usually does not exist yet, so it is the **parent** that is
    /// canonicalized. Every component up to it must already resolve inside
    /// scratch, and the final component is checked for being a link itself.
    pub fn resolve(&self, relative: &str) -> Result<PathBuf> {
        let escaped = |why: &str| {
            Err(Error::new(
                Code::FsPathEscapeBlocked,
                "The model tried to write outside its working directory. \
                 The request was stopped.",
            )
            .with_context(format!("{relative}: {why}")))
        };

        let candidate = self.path.join(relative);
        let Some(parent) = candidate.parent() else {
            return escaped("no parent");
        };
        let Some(name) = candidate.file_name() else {
            return escaped("no file name");
        };

        // The deepest existing ancestor, resolved by the kernel. Components
        // that do not exist yet cannot be a symlink, so resolving the ones
        // that do is sufficient — and it is the only check that sees through
        // a link.
        let mut existing = parent.to_path_buf();
        let mut tail: Vec<std::ffi::OsString> = Vec::new();
        let real = loop {
            match existing.canonicalize() {
                Ok(p) => break p,
                Err(_) => match (existing.file_name(), existing.parent()) {
                    (Some(n), Some(up)) => {
                        tail.push(n.to_os_string());
                        existing = up.to_path_buf();
                    }
                    _ => return escaped("no resolvable ancestor"),
                },
            }
        };

        let root = self.canonical_root()?;
        if !real.starts_with(&root) {
            return escaped("resolves outside the working directory");
        }
        // A component that does not exist may still be named `..`.
        if tail.iter().any(|c| c == ".." || c == ".") || relative.contains("..") {
            return escaped("contains a relative component");
        }

        let mut out = real;
        for c in tail.iter().rev() {
            out.push(c);
        }
        out.push(name);

        // The final component may itself already be a symlink pointing out.
        if let Ok(meta) = std::fs::symlink_metadata(&out) {
            if meta.file_type().is_symlink() {
                match out.canonicalize() {
                    Ok(t) if t.starts_with(&root) => {}
                    _ => return escaped("the target is a symlink leaving the directory"),
                }
            }
        }
        Ok(out)
    }

    /// The scratch root as the kernel sees it.
    ///
    /// Resolved every time rather than cached: symlink escape is re-checked at operation time, so the check
    /// happens at operation time, and a root that was replaced by a symlink
    /// since construction is exactly the case a cached value would miss.
    fn canonical_root(&self) -> Result<PathBuf> {
        self.path.canonicalize().map_err(|e| {
            Error::new(
                Code::FsPathEscapeBlocked,
                "The working directory could not be resolved. The request was stopped.",
            )
            .with_source(e)
        })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Best effort by necessity: a failure here must not panic during
        // unwinding. The startup sweep is the backstop (SUP-015).
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Lexical `..` and `.` resolution. Deliberately does not touch the disk.
fn normalise(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// Whether `child` is inside `ancestor`, as the kernel sees it.
///
/// Canonicalized, not folded, for the same reason `Scratch::resolve` is:
/// a model area that reaches an indexed root *through a symlink* passes a
/// string comparison and fails the rule (symlink escape at operation time, SUP-011).
///
/// Neither path need exist yet, so each is resolved as deeply as it can be and
/// the unresolved tail is appended. That is conservative in the right
/// direction: an unresolvable component cannot be a link.
fn within(child: &Path, ancestor: &Path) -> bool {
    match (deepest_real(child), deepest_real(ancestor)) {
        (Some(c), Some(a)) => c.starts_with(a),
        // Nothing on either path resolves, so there is no link to see through
        // and the lexical answer is the only one available.
        _ => normalise(child).starts_with(normalise(ancestor)),
    }
}

/// `path` with its deepest existing prefix resolved by the kernel.
fn deepest_real(path: &Path) -> Option<PathBuf> {
    let mut existing = normalise(path);
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(real) = existing.canonicalize() {
            let mut out = real;
            for c in tail.iter().rev() {
                out.push(c);
            }
            return Some(out);
        }
        match (existing.file_name(), existing.parent()) {
            (Some(n), Some(up)) => {
                tail.push(n.to_os_string());
                existing = up.to_path_buf();
            }
            _ => return None,
        }
    }
}

fn dir_size(p: &Path) -> u64 {
    let Ok(rd) = fs::read_dir(p) else { return 0 };
    rd.flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(_) => e.metadata().map(|m| m.len()).unwrap_or(0),
            Err(_) => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn scratch_is_removed_when_the_request_ends() {
        // SUP-010, including on cancel and on panic — `Drop` covers all three.
        let t = temp();
        let ws = ModelWorkspace::open(t.path(), &[]).unwrap();
        let path;
        {
            let s = ws.scratch(RequestId::new()).unwrap();
            path = s.path().to_path_buf();
            fs::write(path.join("work.txt"), "x").unwrap();
            assert!(path.exists());
        }
        assert!(!path.exists(), "scratch must not survive the request");
    }

    #[test]
    fn the_model_area_refuses_to_live_inside_an_indexed_folder() {
        // SUP-011 and the `origin = SELF` rule. By the time a warning is read, the model's
        // output is already in the index.
        let t = temp();
        let indexed = t.path().join("Documents");
        fs::create_dir_all(&indexed).unwrap();
        let e = ModelWorkspace::open(
            indexed.join("marrow-models"),
            std::slice::from_ref(&indexed),
        )
        .unwrap_err();
        assert_eq!(e.code(), Code::CfgInvalid);
        assert!(e.message().contains("cited back"), "{}", e.message());
    }

    #[test]
    fn a_model_area_that_reaches_an_indexed_folder_through_a_symlink_is_refused() {
        // SUP-011 with the same lesson as `resolve`: a string comparison sees
        // a path beside the workspace, and the kernel sees a path inside it.
        let t = temp();
        let indexed = t.path().join("Documents");
        fs::create_dir_all(indexed.join("nested")).unwrap();
        let link = t.path().join("models-link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(indexed.join("nested"), &link).unwrap();

        let e = ModelWorkspace::open(&link, std::slice::from_ref(&indexed)).unwrap_err();
        assert_eq!(e.code(), Code::CfgInvalid);
        assert!(e.message().contains("cited back"), "{}", e.message());
    }

    #[test]
    fn a_model_area_beside_an_indexed_folder_is_fine() {
        let t = temp();
        let indexed = t.path().join("Documents");
        fs::create_dir_all(&indexed).unwrap();
        assert!(ModelWorkspace::open(t.path().join("models"), &[indexed]).is_ok());
    }

    #[test]
    fn a_path_that_climbs_out_of_scratch_is_refused() {
        // SUP-013. `../../` arrives as data, from a model that read it in a
        // document.
        let t = temp();
        let ws = ModelWorkspace::open(t.path(), &[]).unwrap();
        let s = ws.scratch(RequestId::new()).unwrap();
        for bad in ["../escape", "a/../../escape", "../../../../etc/passwd"] {
            let e = s.resolve(bad).unwrap_err();
            assert_eq!(
                e.code(),
                Code::FsPathEscapeBlocked,
                "{bad} should be refused"
            );
        }
        assert!(s.resolve("out/result.json").is_ok());
        assert!(s.resolve("./a/./b").is_ok());
    }

    #[test]
    fn a_symlink_inside_scratch_cannot_be_used_to_climb_out() {
        // Symlink escape at operation time: a string prefix check is not sufficient, and this is
        // the case that proves it. Lexical folding says `link/../x` is `x`;
        // the kernel says it is `x` beside whatever `link` points at. The
        // worker owns its scratch directory, so it can create the link.
        let t = temp();
        let ws = ModelWorkspace::open(t.path(), &[]).unwrap();
        let s = ws.scratch(RequestId::new()).unwrap();

        let outside = t.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, s.path().join("link")).unwrap();

        let e = s.resolve("link/stolen.txt").unwrap_err();
        assert_eq!(e.code(), Code::FsPathEscapeBlocked);

        // And through it with a `..`, which is the spelling lexical folding
        // silently rewrites into something harmless-looking.
        assert!(s.resolve("link/../../stolen.txt").is_err());
    }

    #[test]
    fn a_symlinked_target_file_is_refused_even_when_its_parent_is_inside() {
        // The last component can be the link. Checking only the parent would
        // let a worker overwrite whatever it points at.
        let t = temp();
        let ws = ModelWorkspace::open(t.path(), &[]).unwrap();
        let s = ws.scratch(RequestId::new()).unwrap();
        let outside = t.path().join("secret.txt");
        fs::write(&outside, "x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, s.path().join("innocent.txt")).unwrap();
        assert_eq!(
            s.resolve("innocent.txt").unwrap_err().code(),
            Code::FsPathEscapeBlocked
        );
    }

    #[test]
    fn an_ordinary_nested_path_that_does_not_exist_yet_still_resolves() {
        // The check must not be so strict that it refuses the normal case:
        // scratch is written to before its subdirectories exist.
        let t = temp();
        let ws = ModelWorkspace::open(t.path(), &[]).unwrap();
        let s = ws.scratch(RequestId::new()).unwrap();
        let p = s.resolve("out/deep/result.json").unwrap();
        assert!(p.starts_with(s.path().canonicalize().unwrap()));
        assert!(p.ends_with("out/deep/result.json"));
    }

    #[test]
    fn exceeding_the_scratch_cap_fails_the_request_rather_than_the_disk() {
        // SUP-012.
        let t = temp();
        let ws = ModelWorkspace::open(t.path(), &[])
            .unwrap()
            .with_scratch_cap(1024);
        let s = ws.scratch(RequestId::new()).unwrap();
        fs::write(s.path().join("big"), vec![0u8; 4096]).unwrap();
        let e = s.check_cap().unwrap_err();
        assert_eq!(e.code(), Code::ModScratchExceeded);
        assert!(
            e.message().contains("MB"),
            "must name the numbers: {}",
            e.message()
        );
    }

    #[test]
    fn the_cap_counts_nested_files_too() {
        // A model that writes into a subdirectory is still writing to disk.
        let t = temp();
        let ws = ModelWorkspace::open(t.path(), &[])
            .unwrap()
            .with_scratch_cap(1024);
        let s = ws.scratch(RequestId::new()).unwrap();
        fs::create_dir_all(s.path().join("deep/deeper")).unwrap();
        fs::write(s.path().join("deep/deeper/big"), vec![0u8; 4096]).unwrap();
        assert!(s.check_cap().is_err());
    }

    #[test]
    fn orphaned_scratch_from_a_crash_is_cleaned_at_startup() {
        // SUP-015. A process that was killed had no chance to run `Drop`.
        let t = temp();
        let ws = ModelWorkspace::open(t.path(), &[]).unwrap();
        for name in ["01ABC", "01DEF"] {
            let p = t.path().join("scratch").join(name);
            fs::create_dir_all(&p).unwrap();
            fs::write(p.join("leftover"), "x").unwrap();
        }
        assert_eq!(ws.clean_orphaned_scratch().unwrap(), 2);
        assert_eq!(
            ws.clean_orphaned_scratch().unwrap(),
            0,
            "must be idempotent"
        );
    }

    #[test]
    fn a_partial_download_is_never_in_the_loadable_directory() {
        // LLM-027. "Loadable and then checked" is a race with a 3 GB window.
        let t = temp();
        let ws = ModelWorkspace::open(t.path(), &[]).unwrap();
        let sha = "a".repeat(64);
        assert_ne!(ws.partial_dir(&sha), ws.weights_dir(&sha));
        fs::create_dir_all(ws.partial_dir(&sha)).unwrap();
        assert!(
            !ws.is_installed(&sha),
            "a partial download must not read as installed"
        );
        fs::create_dir_all(ws.weights_dir(&sha)).unwrap();
        assert!(ws.is_installed(&sha));
    }

    #[test]
    fn weights_are_addressed_by_digest_so_two_models_cannot_collide() {
        // SUP-014.
        let t = temp();
        let ws = ModelWorkspace::open(t.path(), &[]).unwrap();
        assert_ne!(
            ws.weights_dir(&"a".repeat(64)),
            ws.weights_dir(&"b".repeat(64))
        );
        assert!(ws.weights_dir("abc").ends_with("abc"));
    }

    #[test]
    fn discarding_a_partial_is_idempotent() {
        // It runs when a digest changes, which may be after it already ran.
        let t = temp();
        let ws = ModelWorkspace::open(t.path(), &[]).unwrap();
        let sha = "c".repeat(64);
        fs::create_dir_all(ws.partial_dir(&sha)).unwrap();
        ws.discard_partial(&sha).unwrap();
        ws.discard_partial(&sha).unwrap();
        assert!(!ws.partial_dir(&sha).exists());
    }
}
