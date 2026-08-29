//! What this machine is. Read once, cached, re-read on hardware change.

use serde::Serialize;

/// Capability tier (Part 5 §95.3), trimmed to what a single-machine build needs.
///
/// Part 7 §127 dropped the low tiers as degradation branches — this machine is
/// the target. They survive here only as a label for the Models page, so
/// "comfortable up to ~8B at 4-bit" can be phrased without arithmetic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Minimal,
    Low,
    Mid,
    High,
    Max,
}

impl Tier {
    /// The sentence the Models page shows under "This machine".
    pub fn headline(self) -> &'static str {
        match self {
            Tier::Minimal => "Too little memory for a local model. Search still works.",
            Tier::Low => "Comfortable up to about 3B at 4-bit.",
            Tier::Mid => "Comfortable up to about 8B at 4-bit.",
            Tier::High => "Comfortable up to about 14B at 4-bit.",
            Tier::Max => "Comfortable with 30B and above at 4-bit.",
        }
    }

    /// Tier a hypothetical machine. Test-only, so the tiering rule can be
    /// exercised without fabricating a whole `Machine`.
    #[cfg(test)]
    pub(crate) fn for_test(bytes: u64, unified: bool) -> Self {
        Self::from_memory(bytes, unified)
    }

    fn from_memory(bytes: u64, unified: bool) -> Self {
        // Unified memory is shared with the OS and the display, so the same
        // number buys less than it would as dedicated VRAM.
        let gb = bytes as f64 / 1_000_000_000.0;
        let effective = if unified { gb * 0.75 } else { gb };
        match effective {
            g if g < 6.0 => Tier::Minimal,
            g if g < 10.0 => Tier::Low,
            g if g < 20.0 => Tier::Mid,
            g if g < 40.0 => Tier::High,
            _ => Tier::Max,
        }
    }
}

/// An accelerator we actually managed to talk to.
///
/// HW-003: every claim is verified by initialising it once, not by reading a
/// device list. A GPU that is present but whose driver will not load is not an
/// accelerator, it is a support ticket.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Accelerator {
    pub name: String,
    pub verified: bool,
}

/// The static shape of this machine.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Machine {
    pub cpu_cores: usize,
    pub total_memory_bytes: u64,
    /// Apple Silicon and similar: CPU and GPU share one pool, so the sizing
    /// rules differ (see `sizing::Requirement::estimate`).
    pub unified_memory: bool,
    pub model_identifier: String,
    pub os_version: String,
    pub accelerators: Vec<Accelerator>,
    pub tier: Tier,
}

impl Machine {
    /// A deliberately pessimistic machine, for when the probe fails.
    ///
    /// HW-010: probe failure degrades to a conservative profile and never
    /// blocks launch. Claiming capability we could not measure is how a model
    /// gets offered on a machine that cannot run it.
    pub fn unknown() -> Self {
        Self {
            cpu_cores: 1,
            total_memory_bytes: 0,
            unified_memory: false,
            model_identifier: "unknown".into(),
            os_version: "unknown".into(),
            accelerators: Vec::new(),
            tier: Tier::Minimal,
        }
    }

    /// What the Models page prints under "This machine".
    pub fn summary(&self) -> String {
        let gb = self.total_memory_bytes as f64 / 1_000_000_000.0;
        format!(
            "{:.0} GB {} · {} cores · {}",
            gb,
            if self.unified_memory {
                "unified"
            } else {
                "RAM"
            },
            self.cpu_cores,
            self.model_identifier
        )
    }
}

/// Reads the machine's static shape.
#[derive(Debug, Default)]
pub struct Probe;

impl Probe {
    /// Read it. Cheap enough to call at startup; cached by the caller.
    pub fn run() -> Machine {
        let cpu_cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let total_memory_bytes = total_memory().unwrap_or(0);
        let model_identifier = sysctl_string("hw.model").unwrap_or_else(|| "unknown".into());
        let unified_memory = is_unified(&model_identifier);
        let os_version = os_version().unwrap_or_else(|| "unknown".into());

        Machine {
            tier: Tier::from_memory(total_memory_bytes, unified_memory),
            cpu_cores,
            total_memory_bytes,
            unified_memory,
            model_identifier,
            os_version,
            // Populated by the runtime that actually loads a model — this crate
            // does not link an inference engine, and a list read from a device
            // enumeration would be exactly the unverified claim HW-003 forbids.
            accelerators: Vec::new(),
        }
    }
}

/// Apple Silicon shares one memory pool. Intel Macs do not.
fn is_unified(model_identifier: &str) -> bool {
    // `Mac16,12`, `MacBookAir10,1` … Intel models are `MacBookPro16,1`-style
    // too, so the identifier alone is ambiguous. The architecture is not.
    cfg!(target_arch = "aarch64") && model_identifier.starts_with("Mac")
}

#[cfg(target_os = "macos")]
fn total_memory() -> Option<u64> {
    sysctl_u64("hw.memsize")
}

#[cfg(not(target_os = "macos"))]
fn total_memory() -> Option<u64> {
    None
}

#[cfg(target_os = "macos")]
fn sysctl_u64(name: &str) -> Option<u64> {
    let out = std::process::Command::new("/usr/sbin/sysctl")
        .arg("-n")
        .arg(name)
        .output()
        .ok()?;
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}

#[cfg(target_os = "macos")]
fn sysctl_string(name: &str) -> Option<String> {
    let out = std::process::Command::new("/usr/sbin/sysctl")
        .arg("-n")
        .arg(name)
        .output()
        .ok()?;
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

#[cfg(not(target_os = "macos"))]
fn sysctl_string(_name: &str) -> Option<String> {
    None
}

#[cfg(target_os = "macos")]
fn os_version() -> Option<String> {
    let out = std::process::Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
        .ok()?;
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

#[cfg(not(target_os = "macos"))]
fn os_version() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_probe_reads_this_machine() {
        let m = Probe::run();
        assert!(m.cpu_cores >= 1);
        #[cfg(target_os = "macos")]
        {
            assert!(
                m.total_memory_bytes > 1_000_000_000,
                "should read hw.memsize"
            );
            assert_ne!(m.model_identifier, "unknown");
        }
    }

    #[test]
    fn an_unknown_machine_is_pessimistic_not_optimistic() {
        // HW-010. Claiming capability we could not measure is how a model gets
        // offered on a machine that cannot run it.
        let m = Machine::unknown();
        assert_eq!(m.tier, Tier::Minimal);
        assert_eq!(m.total_memory_bytes, 0);
    }

    #[test]
    fn unified_memory_is_derated_before_tiering() {
        // 16 GB unified buys less than 16 GB of dedicated VRAM, because the OS
        // and the display are in the same pool.
        let unified = Tier::from_memory(17_179_869_184, true);
        let discrete = Tier::from_memory(17_179_869_184, false);
        assert!(unified <= discrete, "unified must not tier higher");
        assert_eq!(unified, Tier::Mid);
    }

    #[test]
    fn every_tier_has_a_sentence_rather_than_a_number() {
        for t in [Tier::Minimal, Tier::Low, Tier::Mid, Tier::High, Tier::Max] {
            let h = t.headline();
            assert!(!h.is_empty());
            assert!(
                h.contains("Comfortable") || h.contains("Search still works"),
                "{t:?}: {h}"
            );
        }
    }

    #[test]
    fn the_minimal_tier_says_search_still_works() {
        // A machine that cannot run a model is not a machine that cannot use
        // Marrow (ADR-010).
        assert!(Tier::Minimal.headline().contains("Search still works"));
    }

    #[test]
    fn accelerators_start_empty_rather_than_assumed() {
        // HW-003: a claim must be verified by loading, and this crate links no
        // inference engine to verify with.
        assert!(Probe::run().accelerators.is_empty());
    }
}
