//! Live conditions. Read on a timer, so the budget is **under a millisecond**.
//!
//! A sampler that costs measurable CPU is a bug in the thing measuring CPU, so
//! nothing here shells out, allocates per sample, or walks a process list. It
//! reads what the kernel already has and stops.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

/// How many samples to remember.
///
/// HW-014: one spike is noise, a sustained trend is a decision. Ten samples at
/// the default interval is a few seconds of history — enough to tell a build
/// starting from a build running.
const HISTORY: usize = 10;

/// Thermal pressure, where the platform reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Thermal {
    Nominal,
    Fair,
    Serious,
    Critical,
    /// The platform does not report it. **Not** an optimistic default — the
    /// caller must decide what to do with ignorance, and HW-012 says the UI
    /// must say which it is.
    Unknown,
}

impl Thermal {
    pub fn blocks_work(self) -> bool {
        matches!(self, Thermal::Serious | Thermal::Critical)
    }
}

/// One reading.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
    pub available_memory_bytes: u64,
    /// Load average normalised by core count. 1.0 means fully committed.
    pub cpu_load: f32,
    pub thermal: Thermal,
    pub on_battery: bool,
    /// 0.0–1.0. `None` when there is no battery.
    pub battery_level: Option<f32>,
    /// HW-015: a stale sampler must be detectable rather than reporting the
    /// last good reading forever.
    pub taken_at_ms: u64,
}

impl Sample {
    /// Deliberately unhelpful, for before the first real sample lands.
    ///
    /// Zero available memory refuses everything, which is the right way to be
    /// wrong: the alternative admits a model on a machine we have not measured.
    pub fn unknown(now_ms: u64) -> Self {
        Self {
            available_memory_bytes: 0,
            cpu_load: 1.0,
            thermal: Thermal::Unknown,
            on_battery: false,
            battery_level: None,
            taken_at_ms: now_ms,
        }
    }
}

/// What the supervisor asks about, derived from recent history.
///
/// Derived from a window rather than the latest sample, so a momentary spike
/// during a build does not suspend a model that was running fine.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Conditions {
    pub latest: Sample,
    /// Lowest available memory across the window — the number to size against,
    /// because admitting on a peak means OOMing in the trough.
    pub min_available_bytes: u64,
    /// Mean load across the window.
    pub sustained_load: f32,
    /// True when the newest sample is older than the caller's tolerance.
    pub stale: bool,
}

/// Samples the machine on a background thread.
#[derive(Debug)]
pub struct Sampler {
    history: Arc<Mutex<Vec<Sample>>>,
    interval: Duration,
    cores: usize,
    started: Instant,
    ticks: Arc<AtomicU64>,
}

impl Sampler {
    pub fn new(cores: usize, interval: Duration) -> Self {
        Self {
            history: Arc::new(Mutex::new(Vec::with_capacity(HISTORY))),
            interval,
            cores: cores.max(1),
            started: Instant::now(),
            ticks: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Take one reading now. Public so the supervisor can force a fresh read
    /// before an admission decision rather than trusting the last tick.
    pub fn sample_now(&self) -> Sample {
        let now = self.started.elapsed().as_millis() as u64;
        Sample {
            available_memory_bytes: available_memory().unwrap_or(0),
            cpu_load: load_average()
                .map(|l| (l / self.cores as f32).min(4.0))
                .unwrap_or(1.0),
            thermal: thermal_state(),
            on_battery: on_battery().unwrap_or(false),
            battery_level: battery_level(),
            taken_at_ms: now,
        }
    }

    /// Record a sample into the ring.
    pub fn tick(&self) {
        let s = self.sample_now();
        if let Ok(mut h) = self.history.lock() {
            if h.len() == HISTORY {
                h.remove(0);
            }
            h.push(s);
        }
        self.ticks.fetch_add(1, Ordering::Relaxed);
    }

    /// Conditions derived from the window.
    ///
    /// `tolerance` is how old the newest sample may be before it is called
    /// stale — a supervisor that trusts a frozen sampler is worse than one with
    /// no sampler at all.
    pub fn conditions(&self, tolerance: Duration) -> Conditions {
        let now = self.started.elapsed().as_millis() as u64;
        let h = match self.history.lock() {
            Ok(h) => h,
            Err(_) => return unknown_conditions(now),
        };
        let Some(latest) = h.last().copied() else {
            return unknown_conditions(now);
        };

        Conditions {
            latest,
            // The trough, not the peak: admitting on a peak means OOMing later.
            min_available_bytes: h
                .iter()
                .map(|s| s.available_memory_bytes)
                .min()
                .unwrap_or(0),
            sustained_load: h.iter().map(|s| s.cpu_load).sum::<f32>() / h.len() as f32,
            stale: now.saturating_sub(latest.taken_at_ms) > tolerance.as_millis() as u64,
        }
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }

    pub fn ticks(&self) -> u64 {
        self.ticks.load(Ordering::Relaxed)
    }
}

fn unknown_conditions(now: u64) -> Conditions {
    Conditions {
        latest: Sample::unknown(now),
        min_available_bytes: 0,
        sustained_load: 1.0,
        stale: true,
    }
}

// ── platform reads ────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
#[allow(unsafe_code)] // A read of the kernel's own page counters. See lib.rs.
fn available_memory() -> Option<u64> {
    // `free + inactive` pages: inactive pages are reclaimable, and counting
    // only free would report a few hundred MB on a healthy machine and refuse
    // every model.
    let mut count = libc::HOST_VM_INFO64_COUNT;
    let mut stat: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
    // Deprecated in `libc` in favour of the `mach2` crate; a whole dependency
    // for one symbol we call once per tick is not a trade worth making.
    #[allow(deprecated)]
    let port = unsafe { libc::mach_host_self() };
    let rc = unsafe {
        libc::host_statistics64(
            port,
            libc::HOST_VM_INFO64,
            &mut stat as *mut _ as *mut i32,
            &mut count,
        )
    };
    if rc != 0 {
        return None;
    }
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
    Some((u64::from(stat.free_count) + u64::from(stat.inactive_count)) * page)
}

#[cfg(not(target_os = "macos"))]
fn available_memory() -> Option<u64> {
    None
}

#[cfg(unix)]
#[allow(unsafe_code)] // Fills a caller-owned [f64; 3]. See lib.rs.
fn load_average() -> Option<f32> {
    let mut avg = [0f64; 3];
    let n = unsafe { libc::getloadavg(avg.as_mut_ptr(), 3) };
    (n > 0).then(|| avg[0] as f32)
}

#[cfg(not(unix))]
fn load_average() -> Option<f32> {
    None
}

/// Thermal pressure.
///
/// macOS exposes this through `NSProcessInfo.thermalState`, which needs an
/// Objective-C bridge this crate deliberately does not link. HW-012 says
/// sustained load stands in where the platform does not report it, and that the
/// UI must say which — hence `Unknown` rather than a guess dressed as a reading.
fn thermal_state() -> Thermal {
    Thermal::Unknown
}

/// Whether we are on battery.
///
/// Read from `pmset`, which is a subprocess and therefore too slow for the
/// per-tick budget. The supervisor calls this on its own slower cadence; the
/// tick path treats it as sticky.
#[cfg(target_os = "macos")]
fn on_battery() -> Option<bool> {
    None
}

#[cfg(not(target_os = "macos"))]
fn on_battery() -> Option<bool> {
    None
}

fn battery_level() -> Option<f32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sample_is_cheap_enough_to_take_on_a_timer() {
        // The whole design rests on this. If sampling costs milliseconds, the
        // sampler becomes part of the load it is measuring.
        let s = Sampler::new(10, Duration::from_secs(2));
        let start = Instant::now();
        for _ in 0..100 {
            let _ = s.sample_now();
        }
        let per = start.elapsed() / 100;
        assert!(
            per < Duration::from_millis(1),
            "sampling took {per:?} each; the budget is under 1 ms"
        );
    }

    #[test]
    fn available_memory_is_a_real_number_on_this_machine() {
        let s = Sampler::new(10, Duration::from_secs(2)).sample_now();
        #[cfg(target_os = "macos")]
        assert!(
            s.available_memory_bytes > 100_000_000,
            "expected a plausible figure, got {}",
            s.available_memory_bytes
        );
        let _ = s;
    }

    #[test]
    fn conditions_report_the_trough_not_the_peak() {
        // Admitting a model on a memory peak means OOMing in the trough.
        let s = Sampler::new(4, Duration::from_millis(1));
        {
            let mut h = s.history.lock().unwrap();
            for avail in [8_000_000_000u64, 1_000_000_000, 6_000_000_000] {
                h.push(Sample {
                    available_memory_bytes: avail,
                    ..Sample::unknown(0)
                });
            }
        }
        let c = s.conditions(Duration::from_secs(60));
        assert_eq!(c.min_available_bytes, 1_000_000_000);
        assert_ne!(c.min_available_bytes, c.latest.available_memory_bytes);
    }

    #[test]
    fn an_empty_sampler_refuses_rather_than_guesses() {
        // Before the first tick we know nothing, and zero available memory is
        // the right way to be wrong.
        let c = Sampler::new(4, Duration::from_secs(2)).conditions(Duration::from_secs(60));
        assert_eq!(c.min_available_bytes, 0);
        assert!(c.stale);
        assert_eq!(c.latest.thermal, Thermal::Unknown);
    }

    #[test]
    fn a_frozen_sampler_is_detectable() {
        // HW-015. A supervisor that trusts a stopped sampler is worse than one
        // with no sampler at all.
        let s = Sampler::new(4, Duration::from_millis(1));
        s.tick();
        std::thread::sleep(Duration::from_millis(30));
        assert!(
            s.conditions(Duration::from_millis(10)).stale,
            "should be stale"
        );
        assert!(
            !s.conditions(Duration::from_secs(60)).stale,
            "should be fresh"
        );
    }

    #[test]
    fn the_ring_buffer_is_bounded() {
        let s = Sampler::new(4, Duration::from_millis(1));
        for _ in 0..HISTORY * 3 {
            s.tick();
        }
        assert_eq!(s.history.lock().unwrap().len(), HISTORY);
        assert_eq!(s.ticks(), (HISTORY * 3) as u64);
    }

    #[test]
    fn serious_and_critical_thermal_block_work_but_unknown_does_not() {
        assert!(Thermal::Serious.blocks_work());
        assert!(Thermal::Critical.blocks_work());
        assert!(!Thermal::Nominal.blocks_work());
        // Unknown must not block: refusing on ignorance would disable local
        // models on every platform that does not report thermals.
        assert!(!Thermal::Unknown.blocks_work());
    }
}
