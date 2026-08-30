//! Download one catalogue model.
//!
//! `cargo run -p marrow-model --example get -- embeddinggemma-300m-mlx-q4`

use std::path::PathBuf;

fn main() {
    let id = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: get <model-id>");
        for e in marrow_model::catalogue::builtin() {
            eprintln!("  {}", e.id);
        }
        std::process::exit(2);
    });
    let entry = marrow_model::catalogue::builtin()
        .into_iter()
        .find(|e| e.id == id)
        .unwrap_or_else(|| {
            eprintln!("no model called {id}");
            std::process::exit(2);
        });

    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    let ws = marrow_model::ModelWorkspace::open(home.join(".local/share/marrow/models"), &[])
        .expect("model workspace");

    let mut last = 0u64;
    let dir = marrow_model::download::download(
        &entry,
        &ws,
        &marrow_model::Https,
        &marrow_model::Cancel::new(),
        &mut |p| {
            if p.bytes_done >= last + 20_000_000 {
                last = p.bytes_done;
                eprintln!("  {:>5.1}%  {:?}", p.fraction() * 100.0, p.stage);
            }
        },
    )
    .expect("download");
    println!("{}", dir.display());
}
