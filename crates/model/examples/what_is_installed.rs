//! `cargo run -p marrow-model --example what_is_installed`

use marrow_hw::{assess, KvPrecision, Probe, Sampler};
use marrow_model::{catalogue, detect, registry::Registry};
use std::time::Duration;

fn main() {
    let machine = Probe::run();
    println!("{}\n{}\n", machine.summary(), machine.tier.headline());

    let sampler = Sampler::new(machine.cpu_cores, Duration::from_secs(2));
    sampler.tick();
    let free = sampler
        .conditions(Duration::from_secs(10))
        .min_available_bytes;
    println!("free now: {:.1} GB\n", free as f64 / 1e9);

    let scan = detect::scan();
    if scan.detected.is_empty() {
        println!("DETECTED\n  nothing running locally\n");
    } else {
        println!("DETECTED");
        for d in &scan.detected {
            println!(
                "  {} on :{} — {} models",
                d.runtime.label(),
                d.port,
                d.model_count
            );
        }
        println!();
    }
    for p in &scan.problems {
        println!("  ! {p}");
    }

    let mut registry = Registry::new();
    for e in catalogue::builtin() {
        registry.insert(e);
    }
    for e in scan.entries {
        registry.insert(e);
    }

    for e in registry.iter() {
        let v = assess(&machine, &e.shape(8192, KvPrecision::F16), free);
        let mark = if e.installed {
            "●"
        } else if v.offerable() {
            "○"
        } else {
            "⊘"
        };
        println!(
            "{mark} {:<26} {:>5.1}B {:<3}  {:?}",
            e.display_name,
            e.params_b,
            e.quantization.label(),
            v.fit
        );
        println!("    {}", v.reason);
    }
}
