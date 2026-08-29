//! Prints what `marrow-hw` says about the machine it runs on.
//!
//! `cargo run -p marrow-hw --example what_can_this_machine_run`

use std::time::Duration;

use marrow_hw::{assess, ModelShape, Probe, Quantization, Sampler};

fn main() {
    let machine = Probe::run();
    println!("{}", machine.summary());
    println!("{}\n", machine.tier.headline());

    let sampler = Sampler::new(machine.cpu_cores, Duration::from_secs(2));
    sampler.tick();
    let c = sampler.conditions(Duration::from_secs(10));
    println!(
        "free now: {:.1} GB · load {:.2} · thermal {:?}\n",
        c.latest.available_memory_bytes as f64 / 1e9,
        c.latest.cpu_load,
        c.latest.thermal
    );

    for (label, params) in [
        ("3B", 3.0),
        ("4B", 4.0),
        ("7B", 7.0),
        ("14B", 14.0),
        ("70B", 70.0),
    ] {
        let shape = ModelShape::new(params, Quantization::Q4, 8192);
        let v = assess(&machine, &shape, c.min_available_bytes);
        println!("{label:>4} Q4  {:?}  {}", v.fit, v.reason);
        println!("        {}", v.breakdown);
    }
}
