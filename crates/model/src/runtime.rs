//! Installing the MLX runtime, so that having one is not a thing the user is
//! assumed to have already done.
//!
//! # Why this exists
//!
//! Every release up to v0.0.4 shipped `mlx_worker.py` in the bundle and left
//! the interpreter as a hand-made venv in the author's data directory.
//! [`Runtime::discover`](crate::worker::Runtime::discover) reads
//! `<data_dir>/runtime/mlx/bin/python`, which is **not in the app bundle**, so
//! every check that verified a release — "the MLX worker is in
//! `Contents/Resources`" — passed on the build machine and could not fail for
//! the missing half. On any other Mac the app installed, opened, indexed, and
//! could not answer a question.
//!
//! The old fallback was a printed hint that began `python3.11 -m venv`. macOS
//! ships no `python3.11`, so the first line was `command not found` and the
//! instruction dead-ended one step after the step that was missing.
//!
//! # What it installs
//!
//! One archive, pinned by digest, holding a **relocatable** CPython and the
//! three pinned packages. Not a venv: a venv records the interpreter that made
//! it — an absolute `home` in `pyvenv.cfg`, `bin/python` symlinked out to it —
//! so a venv built anywhere works only there. See `worker/build-runtime.sh`.
//!
//! # The rules it inherits
//!
//! The same three as [`crate::download`], for the same reasons:
//!
//! 1. **A partial install is never discoverable.** Bytes land in `partial/`,
//!    the tree is built in a staging directory, and one rename publishes it.
//!    There is no window in which `runtime/mlx` is half a runtime.
//! 2. **Verified against its digest before anything is extracted.** An archive
//!    that fails is deleted rather than left to be resumed into a corrupt whole.
//! 3. **Progress is real bytes** — an indeterminate bar on a 400 MB transfer is
//!    a lie with a spinner on it.
//!
//! And one this file adds, because extracting an archive is the one thing here
//! that writes attacker-shaped paths: **every entry is re-checked against the
//! staging root at extraction time**, not by string prefix. Symlink targets
//! too. The archive is ours and digest-pinned, so this is defence in depth
//! rather than the primary control — but "the input is trusted" is how path
//! escapes ship.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use marrow_core::{Code, Error, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::download::Fetcher;
use crate::queue::Cancel;
use crate::worker::Runtime;

/// Read buffer, matched to [`crate::download`] for the same reason: large
/// enough that hashing dominates syscalls, small enough that cancellation is
/// felt inside the UX §10 budget of 500 ms on a slow link.
const CHUNK: usize = 256 * 1024;

/// Headroom over the unpacked size before extraction starts.
///
/// The archive is on disk at this point and the tree is written beside it, so
/// the peak is both at once. Refusing early with a real number beats filling
/// the disk and failing halfway through with `ENOSPC`.
const DISK_HEADROOM: u64 = 512 * 1024 * 1024;

/// The pinned runtime archive.
///
/// Pinned by digest for the reason the model catalogue is: a download that
/// cannot be checked cannot be told apart from a corrupted or substituted one.
/// `version` moves independently of the app version — the runtime changes when
/// a package pin changes, which is rarely, and an app release that changes
/// nothing here must not invalidate an install that already works.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Archive {
    pub version: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    /// Compressed bytes, for the progress bar.
    pub size: u64,
    /// Unpacked bytes, for the disk check before extraction.
    pub unpacked_bytes: u64,
}

impl Archive {
    /// Whether this build has a real pin.
    ///
    /// A digest is filled in by hand from `build-runtime.sh` once the archive
    /// is published. Until then [`install`] refuses with a sentence that says
    /// so, rather than downloading something it cannot check — and rather than
    /// carrying a plausible-looking digest that was never a real file, which is
    /// the drift this repository keeps logging.
    pub fn is_pinned(&self) -> bool {
        !self.sha256.is_empty() && self.sha256 != PIN_PENDING
    }

    fn file_name(&self) -> String {
        format!("marrow-runtime-{}-macos-arm64.tar.gz", self.version)
    }
}

/// The marker a build with no published archive carries.
pub const PIN_PENDING: &str = "PENDING";

/// The archive this build installs.
pub const ARCHIVE: Archive = Archive {
    version: "1",
    url: "https://github.com/CasualOffice/marrow/releases/download/runtime-v1/marrow-runtime-1-macos-arm64.tar.gz",
    // Built by `worker/build-runtime.sh` and pinned by hand from what it
    // printed, the same way the model catalogue is pinned. Never typed from
    // memory and never guessed: an unpinned or invented digest is a download
    // that cannot be told apart from a substituted one.
    sha256: "8b0ed7452b442dcdf7c1e542403be579a8c1df35e3e325a6c922d1bf70c49014",
    size: 202_574_441,
    unpacked_bytes: 656_277_504,
};

/// Where an install is, in the words SKEL-006 requires: a stage with a subject.
/// "Installing" for ninety seconds with no noun is indistinguishable from hung.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "stage"
)]
pub enum Stage {
    Downloading,
    Verifying,
    /// Files unpacked so far, so a 700 MB tree does not look stalled.
    Extracting {
        files: u64,
    },
    /// Starting the interpreter and importing both halves. LLM-036: a runtime
    /// is available when it has answered, not when a file exists.
    Proving,
    Ready,
    Cancelled,
    Failed {
        code: String,
        reason: String,
    },
}

/// What the progress bar renders from.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Install {
    pub stage: Stage,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub bytes_per_sec: u64,
    /// `None` until there is enough of a rate to divide by. An ETA invented
    /// from one chunk is worse than no ETA.
    pub eta_secs: Option<u64>,
}

impl Install {
    pub fn fraction(&self) -> f64 {
        if self.bytes_total == 0 {
            0.0
        } else {
            (self.bytes_done as f64 / self.bytes_total as f64).clamp(0.0, 1.0)
        }
    }

    pub fn is_settled(&self) -> bool {
        matches!(
            self.stage,
            Stage::Ready | Stage::Cancelled | Stage::Failed { .. }
        )
    }
}

/// Somewhere to send progress. A closure rather than a channel, so the caller
/// decides whether to buffer, throttle or drop.
pub type Report<'a> = &'a mut dyn FnMut(Install);

/// Install the runtime, or return the one already there.
///
/// Idempotent: an existing `runtime/mlx` is returned untouched. That matters
/// for the author's hand-made venv, which predates this code and must keep
/// working — an installer that deletes a working runtime to install its own is
/// a worse bug than the one it fixes.
pub fn install(
    data_dir: &Path,
    script: PathBuf,
    archive: &Archive,
    fetcher: &dyn Fetcher,
    cancel: &Cancel,
    report: Report<'_>,
) -> Result<Runtime> {
    let started = Instant::now();

    // Already installed — including by hand, including by an older version of
    // this code. Discovery is the authority on what counts, so there is one
    // definition of "there is a runtime" rather than two that can disagree.
    if let Some(existing) = Runtime::discover(data_dir, script.clone()) {
        report(at(Stage::Ready, archive.size, archive.size, started));
        return Ok(existing);
    }

    if !archive.is_pinned() {
        return Err(Error::new(
            Code::ModIntegrityFailed,
            "This build has no published runtime archive to install, so the \
             model runtime has to be set up by hand. The Models page shows the \
             commands.",
        )
        .with_context(format!("runtime version {}", archive.version)));
    }

    let runtime_dir = data_dir.join("runtime");
    let partial = runtime_dir.join("partial");
    fs::create_dir_all(&partial)?;

    let tarball = partial.join(archive.file_name());
    fetch(archive, &tarball, fetcher, cancel, report, started)?;
    if cancel.is_cancelled() {
        return cancelled(archive, started, report);
    }

    // Disk, before extraction rather than during it. `ENOSPC` halfway through
    // unpacking leaves a staging tree, a full disk and an error about neither.
    let need = archive.unpacked_bytes + DISK_HEADROOM;
    if let Some(free) = free_space(&runtime_dir) {
        if free < need {
            return Err(Error::new(
                Code::FsVolumeUnavailable,
                format!(
                    "The model runtime needs about {} GB free to unpack and \
                     there is {} GB. Free some space and start it again — what \
                     was downloaded is kept.",
                    need / 1_000_000_000,
                    free / 1_000_000_000,
                ),
            ));
        }
    }

    // Built under a name discovery does not look at, so a crash mid-extraction
    // leaves something obviously incomplete rather than a `runtime/mlx` that is
    // missing half its files.
    let staging = runtime_dir.join(format!(".staging-{}", archive.version));
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging)?;

    let unpacked = unpack(&tarball, &staging, archive, cancel, report, started);
    if let Err(e) = unpacked {
        let _ = fs::remove_dir_all(&staging);
        return Err(e);
    }
    if cancel.is_cancelled() {
        let _ = fs::remove_dir_all(&staging);
        return cancelled(archive, started, report);
    }

    // The archive's single top-level directory. Named rather than discovered,
    // so an archive with an unexpected shape fails here with a sentence about
    // the archive instead of somewhere later with a sentence about Python.
    let built = staging.join("mlx");
    if !built.join("bin/python").is_file() {
        let _ = fs::remove_dir_all(&staging);
        return Err(Error::new(
            Code::ModIntegrityFailed,
            "The model runtime archive unpacked without an interpreter in it. \
             Nothing was installed.",
        )
        .with_context(built.display().to_string()));
    }

    let final_dir = runtime_dir.join("mlx");
    if let Err(e) = fs::rename(&built, &final_dir) {
        // Something else finished between the check at the top and now — two
        // windows, or a second install. The destination is digest-pinned, so if
        // it exists it holds the same verified bytes.
        let _ = fs::remove_dir_all(&staging);
        if final_dir.is_dir() {
            return finish(data_dir, script, archive, started, report);
        }
        return Err(Error::new(
            Code::ModIntegrityFailed,
            "The model runtime was downloaded but could not be moved into \
             place. It will resume from where it stopped.",
        )
        .with_source(e));
    }
    let _ = fs::remove_dir_all(&staging);
    // The archive is 400 MB and is never needed again once the tree is built.
    let _ = fs::remove_file(&tarball);

    finish(data_dir, script, archive, started, report)
}

/// Discovery and the proving import, as one step.
fn finish(
    data_dir: &Path,
    script: PathBuf,
    archive: &Archive,
    started: Instant,
    report: Report<'_>,
) -> Result<Runtime> {
    report(at(Stage::Proving, archive.size, archive.size, started));
    let runtime = Runtime::discover(data_dir, script).ok_or_else(|| {
        Error::new(
            Code::ModIntegrityFailed,
            "The model runtime installed but cannot be found where it was put. \
             Nothing here can answer a question until that is resolved.",
        )
    })?;
    report(at(Stage::Ready, archive.size, archive.size, started));
    Ok(runtime)
}

/// Fetch the archive, resuming if some of it is already here.
fn fetch(
    archive: &Archive,
    target: &Path,
    fetcher: &dyn Fetcher,
    cancel: &Cancel,
    report: Report<'_>,
    started: Instant,
) -> Result<()> {
    let have = fs::metadata(target).map(|m| m.len()).unwrap_or(0);

    if have == archive.size {
        // Already complete from an earlier run. Verify rather than assume:
        // resuming is exactly when a corrupt file would slip through.
        report(at(Stage::Verifying, have, archive.size, started));
        if verify(target, archive.sha256)? {
            return Ok(());
        }
        fs::remove_file(target)?;
    }

    let resuming = have > 0 && have < archive.size;
    let (mut reader, ranged) = fetcher.open(archive.url, if resuming { have } else { 0 })?;

    // A server that ignored the range restarts the transfer. Appending its
    // bytes to ours makes a file of the right length and the wrong content —
    // caught by the digest, but only after the whole 400 MB.
    let start_at = if resuming && ranged { have } else { 0 };

    let mut out = if start_at > 0 {
        let mut f = fs::OpenOptions::new().write(true).open(target)?;
        f.seek(SeekFrom::Start(start_at))?;
        f
    } else {
        File::create(target)?
    };

    // The digest covers the whole file, so a resumed transfer hashes what is
    // already on disk before it hashes anything new.
    let mut hasher = Sha256::new();
    if start_at > 0 {
        let mut existing = File::open(target)?;
        let mut left = start_at;
        let mut buf = vec![0u8; CHUNK];
        while left > 0 {
            let n = existing.read(&mut buf[..CHUNK.min(left as usize)])?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            left -= n as u64;
        }
    }

    let mut written = start_at;
    let mut buf = vec![0u8; CHUNK];
    loop {
        if cancel.is_cancelled() {
            out.flush()?;
            return Ok(());
        }
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        hasher.update(&buf[..n]);
        written += n as u64;
        report(at(Stage::Downloading, written, archive.size, started));
    }
    out.flush()?;
    drop(out);

    if cancel.is_cancelled() {
        return Ok(());
    }

    report(at(Stage::Verifying, written, archive.size, started));
    let got = hex(&hasher.finalize());
    if got != archive.sha256 {
        // Delete it. Keeping it lets the next run "resume" into the same
        // corruption forever, which is why `MOD_INTEGRITY_FAILED` is
        // deliberately not retryable.
        let _ = fs::remove_file(target);
        return Err(Error::new(
            Code::ModIntegrityFailed,
            "The model runtime did not match its published checksum and was \
             discarded. Nothing was installed.",
        )
        .with_context(format!("expected {}, got {got}", archive.sha256)));
    }
    Ok(())
}

/// Unpack the archive into `staging`.
///
/// Every entry's destination is resolved against `staging` and re-checked
/// there — hard rule 5, and the one place in this file that writes a path the
/// process did not compose itself. A `..` component, an absolute path, or a
/// symlink pointing outside the tree refuses the whole install rather than
/// skipping the entry: an archive containing one is not an archive with a bad
/// file in it, it is not our archive.
fn unpack(
    tarball: &Path,
    staging: &Path,
    archive: &Archive,
    cancel: &Cancel,
    report: Report<'_>,
    started: Instant,
) -> Result<()> {
    let file = File::open(tarball)?;
    let decoder = flate2::read::GzDecoder::new(std::io::BufReader::new(file));
    let mut tar = tar::Archive::new(decoder);
    tar.set_preserve_permissions(true);
    tar.set_unpack_xattrs(false);

    let mut files: u64 = 0;
    for entry in tar.entries().map_err(archive_unreadable)? {
        if cancel.is_cancelled() {
            return Ok(());
        }
        let mut entry = entry.map_err(archive_unreadable)?;
        let path = entry.path().map_err(archive_unreadable)?.into_owned();
        let dest = safe_join(staging, &path)?;

        // A symlink is the one entry kind whose *target* can escape after the
        // path has been checked — `bin/python -> ../../../etc/passwd` writes
        // inside the tree and reads outside it. The runtime's own links are all
        // relative and within `bin/`, so anything else is refused.
        if entry.header().entry_type().is_symlink() {
            let link = entry
                .link_name()
                .map_err(archive_unreadable)?
                .ok_or_else(|| refused(&path, "a symlink with no target"))?
                .into_owned();
            let resolved = dest
                .parent()
                .ok_or_else(|| refused(&path, "a symlink at the archive root"))?
                .join(&link);
            safe_join(staging, resolved.strip_prefix(staging).unwrap_or(&resolved))?;
        }

        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        entry.unpack(&dest).map_err(archive_unreadable)?;

        files += 1;
        // Every 256 files rather than every file: at ~9,000 entries a report
        // per entry is nine thousand paints to say one thing.
        if files % 256 == 0 {
            report(at(
                Stage::Extracting { files },
                archive.size,
                archive.size,
                started,
            ));
        }
    }
    report(at(
        Stage::Extracting { files },
        archive.size,
        archive.size,
        started,
    ));
    Ok(())
}

/// Resolve `path` inside `root`, refusing anything that leaves it.
///
/// Component-wise, not a string prefix check: `runtime/../../etc` has the right
/// prefix and the wrong destination, which is the whole reason hard rule 5 is
/// worded the way it is.
fn safe_join(root: &Path, path: &Path) -> Result<PathBuf> {
    let mut out = root.to_path_buf();
    for c in path.components() {
        match c {
            Component::Normal(part) => out.push(part),
            // A leading `/` or `C:\`. Never legitimate in an archive we made.
            Component::RootDir | Component::Prefix(_) => {
                return Err(refused(path, "an absolute path"))
            }
            Component::ParentDir => return Err(refused(path, "a parent-directory component")),
            Component::CurDir => {}
        }
    }
    if !out.starts_with(root) {
        return Err(refused(path, "a destination outside the runtime directory"));
    }
    Ok(out)
}

fn refused(path: &Path, why: &str) -> Error {
    Error::new(
        Code::FsPathEscapeBlocked,
        "The model runtime archive contains a file that would be written \
         outside the runtime directory. Nothing was installed.",
    )
    .with_context(format!("{}: {why}", path.display()))
}

fn archive_unreadable(e: std::io::Error) -> Error {
    Error::new(
        Code::ModIntegrityFailed,
        "The model runtime archive could not be read all the way through. \
         Nothing was installed; starting again re-downloads it.",
    )
    .with_source(e)
}

fn cancelled(archive: &Archive, started: Instant, report: Report<'_>) -> Result<Runtime> {
    report(at(Stage::Cancelled, 0, archive.size, started));
    Err(Error::new(
        Code::ModCancelled,
        "Setting up the model runtime was cancelled. What was downloaded is \
         kept, so starting again resumes rather than restarts.",
    ))
}

fn verify(path: &Path, expected: &str) -> Result<bool> {
    let mut f = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()) == expected)
}

fn at(stage: Stage, done: u64, total: u64, started: Instant) -> Install {
    let elapsed = started.elapsed().as_secs_f64();
    let rate = if elapsed > 0.5 {
        (done as f64 / elapsed) as u64
    } else {
        0
    };
    Install {
        stage,
        bytes_done: done.min(total),
        bytes_total: total,
        bytes_per_sec: rate,
        eta_secs: (rate > 0 && done < total).then(|| (total - done) / rate.max(1)),
    }
}

/// Free bytes on the volume holding `path`, or `None` if it cannot be asked.
///
/// `None` means the check is skipped rather than the install refused: a
/// `statfs` that fails must not be the reason a working machine cannot set up
/// its runtime.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)] // One documented read of a kernel-owned struct, the same
                      // shape as `worker::resident_bytes` next door. There is
                      // no free-space call in std to use instead.
fn free_space(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;

    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `buf` is the size the call expects and is only read after a zero
    // return, which is `statfs`'s documented success.
    let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statfs(c.as_ptr(), &mut buf) };
    (rc == 0).then(|| buf.f_bavail * u64::from(buf.f_bsize))
}

#[cfg(not(target_os = "macos"))]
fn free_space(_path: &Path) -> Option<u64> {
    None
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// How long a fresh install takes to talk about. Not used for control flow —
/// only so the UI can say "a few minutes" rather than nothing.
pub const TYPICAL_INSTALL: Duration = Duration::from_secs(180);

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::Mutex;

    /// Serves one archive from memory, so extraction, verification, resume and
    /// path refusal are all testable without a network or 400 MB.
    struct Fake {
        bytes: Vec<u8>,
        ranges: Mutex<Vec<u64>>,
        honour_range: bool,
    }

    impl Fake {
        fn new(bytes: Vec<u8>) -> Self {
            Self {
                bytes,
                ranges: Mutex::new(Vec::new()),
                honour_range: true,
            }
        }
    }

    impl Fetcher for Fake {
        fn open(&self, _url: &str, from: u64) -> Result<(Box<dyn Read + Send>, bool)> {
            self.ranges.lock().unwrap().push(from);
            if self.honour_range && from > 0 {
                let rest = self.bytes[from as usize..].to_vec();
                return Ok((Box::new(Cursor::new(rest)), true));
            }
            Ok((Box::new(Cursor::new(self.bytes.clone())), false))
        }
    }

    fn sha(b: &[u8]) -> String {
        hex(&Sha256::digest(b))
    }

    /// A tar.gz shaped like the real one: `mlx/bin/python` and a file or two.
    fn tarball(entries: &[(&str, &[u8])], links: &[(&str, &str)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (path, body) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, path, *body).unwrap();
        }
        for (path, target) in links {
            let mut header = tar::Header::new_gnu();
            header.set_size(0);
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_mode(0o777);
            builder.append_link(&mut header, path, target).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn real_shaped() -> Vec<u8> {
        tarball(
            &[
                ("mlx/bin/python3.11", b"#!/bin/sh\n"),
                ("mlx/lib/python3.11/site-packages/mlx/__init__.py", b""),
            ],
            &[("mlx/bin/python", "python3.11")],
        )
    }

    fn archive_for(bytes: &[u8]) -> Archive {
        Archive {
            version: "test",
            url: "https://example.invalid/runtime.tar.gz",
            sha256: Box::leak(sha(bytes).into_boxed_str()),
            size: bytes.len() as u64,
            unpacked_bytes: 4096,
        }
    }

    fn script(dir: &Path) -> PathBuf {
        let p = dir.join("mlx_worker.py");
        fs::write(&p, "# worker\n").unwrap();
        p
    }

    #[test]
    fn an_install_lands_verified_and_discoverable() {
        let t = tempfile::tempdir().unwrap();
        let bytes = real_shaped();
        let a = archive_for(&bytes);
        let mut seen = Vec::new();

        let runtime = install(
            t.path(),
            script(t.path()),
            &a,
            &Fake::new(bytes),
            &Cancel::new(),
            &mut |p| seen.push(p.stage.clone()),
        )
        .expect("installs");

        assert_eq!(runtime.python, t.path().join("runtime/mlx/bin/python"));
        assert!(runtime.python.exists(), "the interpreter is really there");
        assert!(seen.contains(&Stage::Ready));
        assert!(
            seen.iter().any(|s| matches!(s, Stage::Extracting { .. })),
            "extraction is reported, or a 700 MB unpack looks hung: {seen:?}"
        );
    }

    /// The bug this module exists for: a check that passes because the runtime
    /// was already there cannot tell you the runtime ships.
    #[test]
    fn an_existing_runtime_is_returned_untouched_and_nothing_is_downloaded() {
        let t = tempfile::tempdir().unwrap();
        let bin = t.path().join("runtime/mlx/bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("python"), "#!/bin/sh\n").unwrap();
        let marker = t.path().join("runtime/mlx/hand-made");
        fs::write(&marker, "the author's venv").unwrap();

        let fake = Fake::new(Vec::new());
        let runtime = install(
            t.path(),
            script(t.path()),
            &archive_for(b"unused"),
            &fake,
            &Cancel::new(),
            &mut |_| {},
        )
        .expect("returns what is there");

        assert_eq!(runtime.python, bin.join("python"));
        assert!(marker.exists(), "a working runtime is never replaced");
        assert!(
            fake.ranges.lock().unwrap().is_empty(),
            "nothing was fetched"
        );
    }

    /// A source build made before the archive was published, or one whose pin
    /// was deliberately cleared. It must say so rather than fetch something it
    /// has no way to check.
    #[test]
    fn a_build_with_no_published_archive_says_so_rather_than_fetching() {
        let t = tempfile::tempdir().unwrap();
        let pending = Archive {
            sha256: PIN_PENDING,
            ..ARCHIVE
        };
        let err = install(
            t.path(),
            script(t.path()),
            &pending,
            &Fake::new(Vec::new()),
            &Cancel::new(),
            &mut |_| {},
        )
        .expect_err("must refuse");
        assert_eq!(err.code(), Code::ModIntegrityFailed);
        assert!(
            err.message().contains("by hand"),
            "must point somewhere: {}",
            err.message()
        );
    }

    #[test]
    fn an_archive_whose_checksum_is_wrong_installs_nothing_and_keeps_nothing() {
        let t = tempfile::tempdir().unwrap();
        let bytes = real_shaped();
        let mut a = archive_for(&bytes);
        a.sha256 = "0000000000000000000000000000000000000000000000000000000000000000";

        let err = install(
            t.path(),
            script(t.path()),
            &a,
            &Fake::new(bytes),
            &Cancel::new(),
            &mut |_| {},
        )
        .expect_err("must refuse");

        assert_eq!(err.code(), Code::ModIntegrityFailed);
        assert!(!t.path().join("runtime/mlx").exists());
        assert!(
            !t.path()
                .join("runtime/partial")
                .join(a.file_name())
                .exists(),
            "a corrupt archive is deleted, or the next run resumes into it forever"
        );
    }

    #[test]
    fn a_partial_install_is_never_where_discovery_looks() {
        let t = tempfile::tempdir().unwrap();
        let bytes = real_shaped();
        let a = archive_for(&bytes);
        let cancel = Cancel::new();

        // Cancelled the moment the first progress lands, which is mid-transfer.
        let err = install(
            t.path(),
            script(t.path()),
            &a,
            &Fake::new(bytes),
            &cancel,
            &mut |_| cancel.cancel(),
        )
        .expect_err("cancelled");

        assert_eq!(err.code(), Code::ModCancelled);
        assert!(
            Runtime::discover(t.path(), script(t.path())).is_none(),
            "nothing half-installed is discoverable"
        );
        assert!(
            !t.path().join("runtime/.staging-test").exists(),
            "staging is cleaned up"
        );
    }

    #[test]
    fn cancelling_keeps_what_was_fetched_so_the_next_attempt_resumes() {
        let t = tempfile::tempdir().unwrap();
        let bytes = real_shaped();
        let a = archive_for(&bytes);
        let cancel = Cancel::new();
        let mut ticks = 0;

        let _ = install(
            t.path(),
            script(t.path()),
            &a,
            &Fake::new(bytes.clone()),
            &cancel,
            &mut |_| {
                ticks += 1;
                if ticks == 1 {
                    cancel.cancel();
                }
            },
        );

        let partial = t.path().join("runtime/partial").join(a.file_name());
        assert!(partial.exists(), "the transfer is kept");

        // Second attempt, uncancelled, resumes and completes.
        let fake = Fake::new(bytes);
        let runtime = install(
            t.path(),
            script(t.path()),
            &a,
            &fake,
            &Cancel::new(),
            &mut |_| {},
        )
        .expect("resumes");
        assert!(runtime.python.exists());
    }

    #[test]
    fn a_server_that_ignores_the_range_restarts_rather_than_appending() {
        let t = tempfile::tempdir().unwrap();
        let bytes = real_shaped();
        let a = archive_for(&bytes);

        // Half a file already on disk, from an interrupted run.
        let partial = t.path().join("runtime/partial");
        fs::create_dir_all(&partial).unwrap();
        fs::write(partial.join(a.file_name()), &bytes[..bytes.len() / 2]).unwrap();

        let mut fake = Fake::new(bytes);
        fake.honour_range = false;

        let runtime = install(
            t.path(),
            script(t.path()),
            &a,
            &fake,
            &Cancel::new(),
            &mut |_| {},
        )
        .expect("restarts and completes");
        assert!(runtime.python.exists(), "the digest still matched");
    }

    /// A tarball with a `..` in an entry name.
    ///
    /// The `tar` *builder* refuses to write one — `append_data` errors with
    /// "paths in archives must not have `..`" — so the name goes into the
    /// header field directly. That is not the test cheating: it is the only
    /// way to produce the archive a hostile mirror would serve, and the point
    /// is that the **reader** refuses it. A fixture that cannot be built by
    /// the happy path is exactly the fixture worth having.
    fn tarball_with_raw_name(name: &str, body: &[u8], also: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (path, b) in also {
            let mut header = tar::Header::new_gnu();
            header.set_size(b.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, path, *b).unwrap();
        }

        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        {
            let old = header.as_old_mut();
            let raw = name.as_bytes();
            assert!(raw.len() < old.name.len(), "fixture name fits the field");
            old.name[..raw.len()].copy_from_slice(raw);
        }
        header.set_cksum();
        builder.append(&header, body).unwrap();

        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn an_archive_that_would_write_outside_the_runtime_directory_is_refused() {
        let t = tempfile::tempdir().unwrap();
        let bytes = tarball_with_raw_name(
            "mlx/../../escaped.py",
            b"pwned",
            &[("mlx/bin/python", b"#!/bin/sh\n")],
        );
        let a = archive_for(&bytes);

        let err = install(
            t.path(),
            script(t.path()),
            &a,
            &Fake::new(bytes),
            &Cancel::new(),
            &mut |_| {},
        )
        .expect_err("must refuse");

        assert_eq!(err.code(), Code::FsPathEscapeBlocked);
        assert!(!t.path().join("runtime/mlx").exists(), "nothing installed");
        assert!(!t.path().parent().unwrap().join("escaped.py").exists());
    }

    #[test]
    fn a_symlink_pointing_out_of_the_tree_is_refused_even_though_its_path_is_inside() {
        let t = tempfile::tempdir().unwrap();
        // The path `mlx/bin/python` is perfectly inside the tree. The *target*
        // is not, which is the half a path check alone does not see.
        let bytes = tarball(
            &[("mlx/bin/python3.11", b"#!/bin/sh\n")],
            &[("mlx/bin/python", "../../../../../../etc/passwd")],
        );
        let a = archive_for(&bytes);

        let err = install(
            t.path(),
            script(t.path()),
            &a,
            &Fake::new(bytes),
            &Cancel::new(),
            &mut |_| {},
        )
        .expect_err("must refuse");

        assert_eq!(err.code(), Code::FsPathEscapeBlocked);
        assert!(!t.path().join("runtime/mlx").exists());
    }

    #[test]
    fn an_archive_with_no_interpreter_in_it_refuses_rather_than_publishing_a_tree() {
        let t = tempfile::tempdir().unwrap();
        let bytes = tarball(&[("mlx/lib/nothing.py", b"")], &[]);
        let a = archive_for(&bytes);

        let err = install(
            t.path(),
            script(t.path()),
            &a,
            &Fake::new(bytes),
            &Cancel::new(),
            &mut |_| {},
        )
        .expect_err("must refuse");

        assert_eq!(err.code(), Code::ModIntegrityFailed);
        assert!(err.message().contains("interpreter"), "{}", err.message());
        assert!(!t.path().join("runtime/mlx").exists());
    }

    #[test]
    fn progress_reports_real_bytes_and_never_exceeds_the_total() {
        let t = tempfile::tempdir().unwrap();
        let bytes = real_shaped();
        let a = archive_for(&bytes);
        let mut seen: Vec<Install> = Vec::new();

        install(
            t.path(),
            script(t.path()),
            &a,
            &Fake::new(bytes),
            &Cancel::new(),
            &mut |p| seen.push(p),
        )
        .unwrap();

        assert!(!seen.is_empty());
        for p in &seen {
            assert!(p.bytes_done <= p.bytes_total, "{p:?}");
            assert!((0.0..=1.0).contains(&p.fraction()), "{p:?}");
        }
        assert!(seen.last().unwrap().is_settled());
    }

    #[test]
    fn the_pin_marker_is_not_mistaken_for_a_digest() {
        let unpinned = Archive {
            sha256: "",
            ..ARCHIVE
        };
        assert!(!unpinned.is_pinned());
        assert!(!Archive {
            sha256: PIN_PENDING,
            ..ARCHIVE
        }
        .is_pinned());
    }

    /// The shipped constant, checked as a constant.
    ///
    /// `release.yml` fetches this URL and compares this digest before it will
    /// tag an app, and it reads both by grepping this file. A malformed pin
    /// would make that grep come back empty — which the workflow treats as a
    /// hard error, but only after a build. Failing here is faster and says
    /// which half is wrong.
    #[test]
    fn the_shipped_pin_is_well_formed() {
        assert!(ARCHIVE.is_pinned(), "an unpinned release cannot install");
        assert_eq!(ARCHIVE.sha256.len(), 64, "sha256 is 64 hex characters");
        assert!(
            ARCHIVE.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
            "sha256 is hex: {}",
            ARCHIVE.sha256
        );
        // A const block, because it can be: this fails at compile time rather
        // than at test time, and clippy is right that an assertion over
        // constants is otherwise just a comment that runs.
        const { assert!(ARCHIVE.size > 0) };
        const { assert!(ARCHIVE.unpacked_bytes > ARCHIVE.size) };
        assert!(
            ARCHIVE.url.starts_with("https://"),
            "NET-001: HTTPS only, and this one is not going through marrow-net"
        );
    }

    /// The URL's last segment and the name the download lands under must be
    /// the same string.
    ///
    /// They are composed separately — one written by hand into the constant,
    /// one built from `version` — and if they drift, every resume writes to a
    /// file the next attempt does not look for. The symptom is a download that
    /// restarts from zero forever, with nothing saying why.
    #[test]
    fn the_pinned_url_and_the_file_it_lands_under_agree() {
        let last = ARCHIVE.url.rsplit('/').next().expect("a URL with a path");
        assert_eq!(last, ARCHIVE.file_name());
    }
}
