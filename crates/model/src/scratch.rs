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
//! output re-indexed and cited back — invariant #13, and the reason `Origin`
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
    pub fn resolve(&self, relative: &str) -> Result<PathBuf> {
        let candidate = self.path.join(relative);
        // Lexical, because the file usually does not exist yet, so
        // `canonicalize` would fail before it could refuse.
        let normalised = normalise(&candidate);
        if !normalised.starts_with(&self.path) {
            return Err(Error::new(
                Code::FsPathEscapeBlocked,
                "The model tried to write outside its working directory. \
                 The request was stopped.",
            )
            .with_context(relative.to_string()));
        }
        Ok(normalised)
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

fn within(child: &Path, ancestor: &Path) -> bool {
    normalise(child).starts_with(normalise(ancestor))
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
        // SUP-011 / invariant #13. By the time a warning is read, the model's
        // output is already in the index.
        let t = temp();
        let indexed = t.path().join("Documents");
        fs::create_dir_all(&indexed).unwrap();
        let e =
            ModelWorkspace::open(indexed.join("marrow-models"), std::slice::from_ref(&indexed)).unwrap_err();
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
