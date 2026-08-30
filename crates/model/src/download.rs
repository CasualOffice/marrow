//! Fetching and verifying model weights (Part 8 §144, LLM-027).
//!
//! Three rules decide the whole design:
//!
//! 1. **A partial download is never loadable.** Not "loadable and then
//!    checked" — that is a race with a 3 GB window. Bytes land in
//!    `partial/<manifest>/` and are promoted by a single rename.
//! 2. **Every file is verified against its own digest** as it lands, not
//!    afterwards from disk. A file that fails is deleted, not left to be
//!    resumed into a corrupt whole.
//! 3. **Progress is real bytes and a real ETA** (SKEL-005). An indeterminate
//!    bar on a 3 GB transfer is a lie with a spinner on it.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use marrow_core::{Code, Error, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::queue::Cancel;
use crate::registry::{Artifact, Entry};
use crate::scratch::ModelWorkspace;

/// Read buffer. Large enough that hashing dominates rather than syscalls,
/// small enough that cancellation is felt within the UX §10 budget of 500 ms
/// even on a slow link.
const CHUNK: usize = 256 * 1024;

/// Where a download is, in the words SKEL-006 requires.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "stage")]
pub enum Stage {
    Downloading {
        file: String,
        index: usize,
        of: usize,
    },
    Verifying {
        file: String,
    },
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
pub struct Progress {
    pub model_id: String,
    pub stage: Stage,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// Measured over the transfer so far. Zero until the first chunk lands.
    pub bytes_per_sec: u64,
    /// `None` while there is not yet enough of a rate to divide by. An ETA
    /// invented from one chunk is worse than no ETA.
    pub eta_secs: Option<u64>,
}

impl Progress {
    pub fn fraction(&self) -> f64 {
        if self.bytes_total == 0 {
            0.0
        } else {
            (self.bytes_done as f64 / self.bytes_total as f64).clamp(0.0, 1.0)
        }
    }
}

/// Somewhere to send progress. A closure rather than a channel so the caller
/// decides whether to buffer, throttle or drop.
pub type Report<'a> = &'a mut dyn FnMut(Progress);

/// Anything that can fetch a byte range over HTTPS.
///
/// A seam, so the whole downloader — resume, verification, promotion,
/// cancellation — is testable without a network. The real implementation is
/// [`Https`]; the tests use a fake that serves bytes from memory.
pub trait Fetcher: Send + Sync {
    /// Open `url`, optionally starting at `from` bytes in.
    ///
    /// Returns the reader and whether the server honoured the range. A server
    /// that ignores `Range` and restarts must not have its bytes appended to
    /// what we already have — that is how a resumed download silently
    /// corrupts.
    fn open(&self, url: &str, from: u64) -> Result<(Box<dyn Read + Send>, bool)>;
}

/// The real one.
#[derive(Debug, Default)]
pub struct Https;

impl Fetcher for Https {
    fn open(&self, url: &str, from: u64) -> Result<(Box<dyn Read + Send>, bool)> {
        let mut req = ureq::get(url);
        if from > 0 {
            req = req.header("Range", &format!("bytes={from}-"));
        }
        let resp = req.call().map_err(|e| {
            Error::new(
                Code::FsVolumeUnavailable,
                "Could not reach the model host. Check the network and try again.",
            )
            .with_context(format!("{url}: {e}"))
        })?;
        let status = resp.status().as_u16();
        // 206 means the range was honoured. 200 with a range asked means it
        // was not, and the caller must restart rather than append.
        let ranged = status == 206;
        if status != 200 && status != 206 {
            return Err(Error::new(
                Code::FsVolumeUnavailable,
                format!("The model host answered {status}. The file may have moved."),
            )
            .with_context(url.to_string()));
        }
        Ok((Box::new(resp.into_body().into_reader()), ranged))
    }
}

/// Fetch every file in `entry`'s manifest, verify each, and promote the whole
/// directory atomically.
///
/// Returns the directory the weights now live in.
pub fn download(
    entry: &Entry,
    workspace: &ModelWorkspace,
    fetcher: &dyn Fetcher,
    cancel: &Cancel,
    report: Report<'_>,
) -> Result<PathBuf> {
    if !entry.downloadable() {
        return Err(Error::new(
            Code::ModIntegrityFailed,
            format!(
                "{} has no verified manifest, so it cannot be downloaded. \
                 A download that cannot be checked cannot be told apart from a \
                 corrupted or substituted one.",
                entry.display_name
            ),
        ));
    }
    let digest = entry.manifest_digest.as_deref().expect("checked above");
    let final_dir = workspace.weights_dir(digest);
    if final_dir.is_dir() {
        return Ok(final_dir);
    }

    let staging = workspace.partial_dir(digest);
    fs::create_dir_all(&staging)?;

    let total: u64 = entry.download_bytes();
    let started = Instant::now();
    let mut done: u64 = 0;

    for (i, file) in entry.files.iter().enumerate() {
        // Re-checked here and not only in `downloadable()`: this is the last
        // point before a path from a manifest becomes a path on disk.
        if !file.is_safe() {
            return Err(Error::new(
                Code::ModIntegrityFailed,
                "The model manifest contains a file path that would write \
                 outside the model directory. Nothing was downloaded.",
            )
            .with_context(file.path.clone()));
        }
        let target = staging.join(&file.path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }

        let have = fs::metadata(&target).map(|m| m.len()).unwrap_or(0);
        if have == file.size {
            // Already here from an earlier run. Verify rather than assume:
            // resuming is exactly when a corrupt file would slip through.
            let mut r = |p: Progress| report(p);
            r(progress(
                entry,
                Stage::Verifying {
                    file: file.path.clone(),
                },
                done,
                total,
                started,
            ));
            if verify(&target, &file.sha256)? {
                done += file.size;
                continue;
            }
            fs::remove_file(&target)?;
        }

        done += fetch_one(
            entry, file, &target, i, fetcher, cancel, report, done, total, started,
        )?;
    }

    if cancel.is_cancelled() {
        return cancelled(entry, done, total, started, report);
    }

    // One rename, so a reader either sees no directory or sees a complete,
    // verified one. There is no window in which a half-written model is
    // loadable.
    if let Some(parent) = final_dir.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(&staging, &final_dir).map_err(|e| {
        Error::new(
            Code::ModIntegrityFailed,
            "The download finished but could not be moved into place. \
             It will be retried from where it stopped.",
        )
        .with_source(e)
    })?;

    report(progress(entry, Stage::Ready, total, total, started));
    Ok(final_dir)
}

#[allow(clippy::too_many_arguments)] // Every one of these is a distinct fact
                                     // the loop needs; bundling them would only move the list.
fn fetch_one(
    entry: &Entry,
    file: &Artifact,
    target: &std::path::Path,
    index: usize,
    fetcher: &dyn Fetcher,
    cancel: &Cancel,
    report: Report<'_>,
    already: u64,
    total: u64,
    started: Instant,
) -> Result<u64> {
    let url = entry
        .file_url(file)
        .ok_or_else(|| Error::invariant("a downloadable entry with no URL"))?;

    let have = fs::metadata(target).map(|m| m.len()).unwrap_or(0);
    let resuming = have > 0 && have < file.size;
    let (mut reader, ranged) = fetcher.open(&url, if resuming { have } else { 0 })?;

    // A server that ignored the range restarts the file. Appending its bytes
    // to ours would produce a file of the right length and the wrong content —
    // caught by the digest, but only after the whole transfer.
    let start_at = if resuming && ranged { have } else { 0 };

    let mut out = if start_at > 0 {
        let mut f = fs::OpenOptions::new().write(true).open(target)?;
        f.seek(SeekFrom::Start(start_at))?;
        f
    } else {
        File::create(target)?
    };

    // The digest covers the whole file, so a resumed transfer has to hash the
    // bytes already on disk before it can hash the new ones.
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
            return Ok(written.saturating_sub(start_at));
        }
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        hasher.update(&buf[..n]);
        written += n as u64;
        report(progress(
            entry,
            Stage::Downloading {
                file: file.path.clone(),
                index: index + 1,
                of: entry.files.len(),
            },
            already + written.saturating_sub(start_at) + start_at.min(file.size),
            total,
            started,
        ));
    }
    out.flush()?;
    drop(out);

    let got = hex(&hasher.finalize());
    if got != file.sha256 {
        // Delete it. Leaving it would let the next run "resume" into the same
        // corruption forever, and `MOD_INTEGRITY_FAILED` is deliberately not
        // retryable for the same reason.
        let _ = fs::remove_file(target);
        return Err(Error::new(
            Code::ModIntegrityFailed,
            format!(
                "{} did not match its published checksum and was discarded. \
                 The download was not completed.",
                file.path
            ),
        )
        .with_context(format!("expected {}, got {got}", file.sha256)));
    }
    Ok(file.size)
}

fn cancelled(
    entry: &Entry,
    done: u64,
    total: u64,
    started: Instant,
    report: Report<'_>,
) -> Result<PathBuf> {
    report(progress(entry, Stage::Cancelled, done, total, started));
    Err(Error::new(
        Code::ModCancelled,
        "The download was cancelled. What was fetched is kept, so starting \
         again resumes rather than restarts.",
    ))
}

fn verify(path: &std::path::Path, expected: &str) -> Result<bool> {
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

fn progress(entry: &Entry, stage: Stage, done: u64, total: u64, started: Instant) -> Progress {
    let elapsed = started.elapsed().as_secs_f64();
    let rate = if elapsed > 0.5 && done > 0 {
        (done as f64 / elapsed) as u64
    } else {
        0
    };
    Progress {
        model_id: entry.id.clone(),
        stage,
        bytes_done: done,
        bytes_total: total,
        bytes_per_sec: rate,
        // An ETA invented from one chunk is worse than no ETA, so it appears
        // only once there is a rate worth dividing by.
        eta_secs: (rate > 0 && total > done).then(|| (total - done) / rate.max(1)),
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// How long to wait between retries of a transfer that failed mid-stream.
/// Exposed so the supervisor and the tests agree on it.
pub const RETRY_BACKOFF: Duration = Duration::from_secs(2);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{Capabilities, Format, Licence, Source};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// Serves bytes from memory, so resume, verification, promotion and
    /// cancellation are all testable without a network.
    struct Fake {
        bodies: HashMap<String, Vec<u8>>,
        /// When true, `Range` is ignored and the whole body is served — the
        /// behaviour that silently corrupts a naive resume.
        ignore_range: bool,
        opens: AtomicUsize,
        ranges: Mutex<Vec<u64>>,
    }

    impl Fake {
        fn new(files: &[(&str, &[u8])]) -> Self {
            Self {
                bodies: files
                    .iter()
                    .map(|(p, b)| ((*p).to_string(), b.to_vec()))
                    .collect(),
                ignore_range: false,
                opens: AtomicUsize::new(0),
                ranges: Mutex::new(Vec::new()),
            }
        }
    }

    impl Fetcher for Fake {
        fn open(&self, url: &str, from: u64) -> Result<(Box<dyn Read + Send>, bool)> {
            self.opens.fetch_add(1, Ordering::SeqCst);
            self.ranges.lock().unwrap().push(from);
            let name = url.rsplit('/').next().unwrap();
            let body = self
                .bodies
                .get(name)
                .ok_or_else(|| Error::new(Code::FsNotFound, "no such file in the fake"))?;
            if from > 0 && !self.ignore_range {
                return Ok((
                    Box::new(std::io::Cursor::new(body[from as usize..].to_vec())),
                    true,
                ));
            }
            Ok((Box::new(std::io::Cursor::new(body.clone())), false))
        }
    }

    fn sha(b: &[u8]) -> String {
        hex(&Sha256::digest(b))
    }

    fn entry_for(files: &[(&str, &[u8])]) -> Entry {
        let artifacts: Vec<Artifact> = files
            .iter()
            .map(|(p, b)| Artifact {
                path: (*p).to_string(),
                sha256: sha(b),
                size: b.len() as u64,
            })
            .collect();
        Entry {
            id: "test-model".into(),
            display_name: "Test Model".into(),
            family: "test".into(),
            params_b: 4.0,
            quantization: marrow_hw::Quantization::Q4,
            format: Format::Mlx,
            context_limit: 8192,
            default_context: 8192,
            kv_bytes_per_token: Some(1024),
            weights_bytes: Some(artifacts.iter().map(|a| a.size).sum()),
            capabilities: Capabilities::default(),
            licence: Licence {
                spdx_or_name: "test".into(),
                url: None,
                commercial_use: None,
            },
            role: "test".into(),
            source: Source::Catalogue,
            repo: Some("acme/test".into()),
            revision: Some("0".repeat(40)),
            manifest_digest: Some(Entry::compute_manifest_digest(&artifacts)),
            files: artifacts,
            installed: false,
            breaker: Default::default(),
        }
    }

    fn workspace() -> (tempfile::TempDir, ModelWorkspace) {
        let t = tempfile::tempdir().unwrap();
        let w = ModelWorkspace::open(t.path(), &[]).unwrap();
        (t, w)
    }

    const A: &[u8] = b"the quick brown fox jumps over the lazy dog, repeatedly and at length";
    const B: &[u8] = b"{\"model_type\":\"test\"}";

    #[test]
    fn a_download_lands_verified_and_promoted_in_one_move() {
        let (_t, w) = workspace();
        let e = entry_for(&[("model.safetensors", A), ("config.json", B)]);
        let mut seen = Vec::new();
        let dir = download(
            &e,
            &w,
            &Fake::new(&[("model.safetensors", A), ("config.json", B)]),
            &Cancel::new(),
            &mut |p| seen.push(p),
        )
        .unwrap();

        assert_eq!(dir, w.weights_dir(e.manifest_digest.as_deref().unwrap()));
        assert_eq!(fs::read(dir.join("model.safetensors")).unwrap(), A);
        assert!(w.is_installed(e.manifest_digest.as_deref().unwrap()));
        assert!(!w
            .partial_dir(e.manifest_digest.as_deref().unwrap())
            .exists());
        assert_eq!(seen.last().unwrap().stage, Stage::Ready);
    }

    #[test]
    fn a_partial_download_is_never_in_the_loadable_directory() {
        // LLM-027, and the reason promotion is a single rename.
        let (_t, w) = workspace();
        let e = entry_for(&[("model.safetensors", A)]);
        let digest = e.manifest_digest.clone().unwrap();
        let cancel = Cancel::new();
        cancel.cancel();
        let err = download(
            &e,
            &w,
            &Fake::new(&[("model.safetensors", A)]),
            &cancel,
            &mut |_| {},
        )
        .unwrap_err();
        assert_eq!(err.code(), Code::ModCancelled);
        assert!(
            !w.is_installed(&digest),
            "a cancelled download must not read as installed"
        );
    }

    #[test]
    fn a_file_whose_checksum_is_wrong_is_discarded_rather_than_kept() {
        // Keeping it would let the next run "resume" into the same corruption
        // forever, which is why MOD_INTEGRITY_FAILED is not retryable.
        let (_t, w) = workspace();
        let e = entry_for(&[("model.safetensors", A)]);
        // The server serves different bytes than the manifest promises.
        let err = download(
            &e,
            &w,
            &Fake::new(&[("model.safetensors", b"tampered")]),
            &Cancel::new(),
            &mut |_| {},
        )
        .unwrap_err();
        assert_eq!(err.code(), Code::ModIntegrityFailed);
        assert!(
            !err.retryable(),
            "the bytes are wrong; retrying changes nothing"
        );
        assert!(err.message().contains("checksum"), "{}", err.message());
        let staged = w.partial_dir(e.manifest_digest.as_deref().unwrap());
        assert!(
            !staged.join("model.safetensors").exists(),
            "the bad file must be gone"
        );
    }

    #[test]
    fn a_resumed_download_asks_for_the_range_it_is_missing() {
        let (_t, w) = workspace();
        let e = entry_for(&[("model.safetensors", A)]);
        let digest = e.manifest_digest.clone().unwrap();
        let staged = w.partial_dir(&digest);
        fs::create_dir_all(&staged).unwrap();
        fs::write(staged.join("model.safetensors"), &A[..20]).unwrap();

        let fake = Fake::new(&[("model.safetensors", A)]);
        download(&e, &w, &fake, &Cancel::new(), &mut |_| {}).unwrap();

        assert_eq!(
            fake.ranges.lock().unwrap().as_slice(),
            &[20],
            "must resume, not restart"
        );
        assert_eq!(
            fs::read(w.weights_dir(&digest).join("model.safetensors")).unwrap(),
            A
        );
    }

    #[test]
    fn a_server_that_ignores_the_range_restarts_the_file_rather_than_appending() {
        // Appending would produce a file of the right length and the wrong
        // content. The digest would catch it — after the whole transfer.
        let (_t, w) = workspace();
        let e = entry_for(&[("model.safetensors", A)]);
        let digest = e.manifest_digest.clone().unwrap();
        let staged = w.partial_dir(&digest);
        fs::create_dir_all(&staged).unwrap();
        fs::write(staged.join("model.safetensors"), &A[..20]).unwrap();

        let mut fake = Fake::new(&[("model.safetensors", A)]);
        fake.ignore_range = true;
        download(&e, &w, &fake, &Cancel::new(), &mut |_| {}).unwrap();
        assert_eq!(
            fs::read(w.weights_dir(&digest).join("model.safetensors")).unwrap(),
            A
        );
    }

    #[test]
    fn an_already_complete_file_is_verified_rather_than_trusted() {
        // Resuming is exactly when a corrupt file would slip through.
        let (_t, w) = workspace();
        let e = entry_for(&[("model.safetensors", A)]);
        let staged = w.partial_dir(e.manifest_digest.as_deref().unwrap());
        fs::create_dir_all(&staged).unwrap();
        // Right length, wrong bytes.
        fs::write(staged.join("model.safetensors"), vec![b'x'; A.len()]).unwrap();

        let fake = Fake::new(&[("model.safetensors", A)]);
        download(&e, &w, &fake, &Cancel::new(), &mut |_| {}).unwrap();
        assert_eq!(
            fake.opens.load(Ordering::SeqCst),
            1,
            "must re-fetch, not trust the length"
        );
    }

    #[test]
    fn an_installed_model_is_not_downloaded_again() {
        let (_t, w) = workspace();
        let e = entry_for(&[("model.safetensors", A)]);
        let digest = e.manifest_digest.clone().unwrap();
        fs::create_dir_all(w.weights_dir(&digest)).unwrap();
        let fake = Fake::new(&[]);
        download(&e, &w, &fake, &Cancel::new(), &mut |_| {}).unwrap();
        assert_eq!(fake.opens.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_manifest_path_that_escapes_the_directory_is_refused_before_any_write() {
        // The last point before a path from a server becomes a path on disk.
        let (_t, w) = workspace();
        let mut e = entry_for(&[("model.safetensors", A)]);
        e.files.push(Artifact {
            path: "../../escaped".into(),
            sha256: sha(b"x"),
            size: 1,
        });
        e.manifest_digest = Some(Entry::compute_manifest_digest(&e.files));
        let err = download(
            &e,
            &w,
            &Fake::new(&[("model.safetensors", A)]),
            &Cancel::new(),
            &mut |_| {},
        )
        .unwrap_err();
        assert_eq!(err.code(), Code::ModIntegrityFailed);
    }

    #[test]
    fn a_model_with_no_manifest_refuses_with_a_reason() {
        let (_t, w) = workspace();
        let mut e = entry_for(&[("model.safetensors", A)]);
        e.manifest_digest = None;
        let err = download(&e, &w, &Fake::new(&[]), &Cancel::new(), &mut |_| {}).unwrap_err();
        assert_eq!(err.code(), Code::ModIntegrityFailed);
        assert!(
            err.message().contains("cannot be told apart"),
            "{}",
            err.message()
        );
    }

    #[test]
    fn progress_reports_real_bytes_and_never_exceeds_the_total() {
        // SKEL-005: real bytes and a real ETA, never an indeterminate bar.
        let (_t, w) = workspace();
        let e = entry_for(&[("model.safetensors", A), ("config.json", B)]);
        let total = e.download_bytes();
        let mut seen: Vec<Progress> = Vec::new();
        download(
            &e,
            &w,
            &Fake::new(&[("model.safetensors", A), ("config.json", B)]),
            &Cancel::new(),
            &mut |p| seen.push(p),
        )
        .unwrap();

        assert!(!seen.is_empty(), "a transfer must report progress");
        for p in &seen {
            assert_eq!(p.bytes_total, total);
            assert!(
                p.bytes_done <= p.bytes_total,
                "{} > {}",
                p.bytes_done,
                p.bytes_total
            );
            assert!((0.0..=1.0).contains(&p.fraction()));
        }
        // Monotonic: a bar that goes backwards reads as a bug even when it is
        // not.
        for pair in seen.windows(2) {
            assert!(
                pair[1].bytes_done >= pair[0].bytes_done,
                "progress went backwards"
            );
        }
        assert_eq!(seen.last().unwrap().bytes_done, total);
    }

    #[test]
    fn the_stage_names_the_file_so_a_stall_is_attributable() {
        // SKEL-006: downloading -> verifying -> ready, each with its subject.
        let (_t, w) = workspace();
        let e = entry_for(&[("model.safetensors", A)]);
        let mut stages = Vec::new();
        download(
            &e,
            &w,
            &Fake::new(&[("model.safetensors", A)]),
            &Cancel::new(),
            &mut |p| stages.push(p.stage),
        )
        .unwrap();
        assert!(stages.iter().any(|s| matches!(
            s,
            Stage::Downloading { file, of: 1, .. } if file == "model.safetensors"
        )));
        assert_eq!(stages.last(), Some(&Stage::Ready));
    }

    #[test]
    fn cancelling_keeps_what_was_fetched_so_the_next_attempt_resumes() {
        let (_t, w) = workspace();
        let e = entry_for(&[("model.safetensors", A), ("config.json", B)]);
        let digest = e.manifest_digest.clone().unwrap();
        let cancel = Cancel::new();
        // Cancel after the first file lands.
        let err = download(
            &e,
            &w,
            &Fake::new(&[("model.safetensors", A), ("config.json", B)]),
            &cancel,
            &mut |p| {
                if p.bytes_done > 0 {
                    cancel.cancel();
                }
            },
        )
        .unwrap_err();
        assert_eq!(err.code(), Code::ModCancelled);
        assert!(err.message().contains("resumes"), "{}", err.message());
        assert!(w.partial_dir(&digest).exists(), "partial work must be kept");
        assert!(!w.is_installed(&digest));
    }
}

/// Against the real network. `#[ignore]` by default: `cargo test` must stay
/// runnable on a plane.
///
/// `cargo test -p marrow-model -- --ignored --nocapture`
#[cfg(test)]
mod network {
    use super::*;
    use crate::catalogue;

    #[test]
    #[ignore = "downloads ~212 MB from huggingface.co"]
    fn the_smallest_pinned_model_really_downloads_and_verifies() {
        // The proof that the pinned catalogue is not merely well-formed: every
        // URL resolves, every published digest matches the bytes behind it,
        // and the whole directory promotes.
        let t = tempfile::tempdir().unwrap();
        let w = ModelWorkspace::open(t.path(), &[]).unwrap();
        let e = catalogue::builtin()
            .into_iter()
            .find(|e| e.id == "embeddinggemma-300m-mlx-q4")
            .unwrap();

        let mut last = 0u64;
        let dir = download(&e, &w, &Https, &Cancel::new(), &mut |p| {
            if p.bytes_done >= last + 20_000_000 {
                last = p.bytes_done;
                eprintln!(
                    "  {:>5.1}%  {:.0} MB/s  {:?}",
                    p.fraction() * 100.0,
                    p.bytes_per_sec as f64 / 1e6,
                    p.stage
                );
            }
        })
        .expect("the pinned manifest must download and verify");

        for f in &e.files {
            let path = dir.join(&f.path);
            assert!(path.exists(), "{} is missing", f.path);
            assert_eq!(
                fs::metadata(&path).unwrap().len(),
                f.size,
                "{} is the wrong length",
                f.path
            );
        }
        assert!(w.is_installed(e.manifest_digest.as_deref().unwrap()));
        eprintln!("verified {} files at {}", e.files.len(), dir.display());
    }
}
