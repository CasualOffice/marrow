//! Marrow desktop — a Tauri shell over the same core the CLI and MCP use.
//!
//! Three frontends, one core ([GUI §1]). This binary is an adapter: it opens
//! the store, registers commands, and shows a window. Anything it computes here
//! is something MCP and the CLI cannot reach, which would put the boundary in
//! the wrong place.
//!
//! [GUI §1]: ../../../docs/GUI.md

// Release builds must not open a console window behind the app.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![forbid(unsafe_code)]

use std::sync::Arc;

use marrow_desktop::{commands, models, Core};

/// Per-user, never machine-wide (MULTI-002). Shared with the CLI, so indexing
/// from a terminal and searching from the app see the same database.
fn data_dir() -> std::path::PathBuf {
    std::env::var_os("MARROW_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").unwrap_or_default();
            std::path::PathBuf::from(home).join(".local/share/marrow")
        })
}

fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();

    let dir = data_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("cannot create {}: {e}", dir.display());
        std::process::exit(1);
    }

    let core = match Core::open(dir.join(marrow_store::DB_FILE_NAME)) {
        Ok(c) => Arc::new(c),
        Err(e) => {
            // Failing loudly beats a window that opens and answers nothing.
            eprintln!("[{}] {}", e.code(), e.message());
            std::process::exit(4);
        }
    };

    // Probes the machine, detects any local runtime and starts the supervisor
    // thread. Nothing is loaded — that is the point of LLM-047.
    //
    // The indexed roots are passed so the model directory can refuse to sit
    // inside one: a model writing into an indexed folder would have its own
    // output re-indexed and cited back (SUP-011, invariant #13).
    let indexed_roots: Vec<std::path::PathBuf> = core
        .workspaces()
        .map(|ws| {
            ws.iter()
                .map(|w| std::path::PathBuf::from(&w.path))
                .collect()
        })
        .unwrap_or_default();
    let hub = Arc::new(models::Hub::start(dir.join("models"), &indexed_roots));

    tauri::Builder::default()
        .manage(core)
        .manage(hub)
        .invoke_handler(tauri::generate_handler![
            commands::search,
            commands::list_workspaces,
            commands::index_health,
            commands::file_detail,
            commands::read_region,
            commands::open_path,
            commands::reveal_path,
            commands::list_files,
            commands::models_overview,
            commands::refresh_model_detection,
            commands::set_ai_profile,
            commands::download_model,
            commands::cancel_model_download,
            commands::dismiss_model_download,
            commands::ask,
            commands::cancel_ask,
            commands::release_model,
            commands::forget_conversation,
            commands::start_semantic_backfill,
            commands::stop_semantic_backfill,
        ])
        .build(tauri::generate_context!())
        .expect("failed to start the Marrow window")
        .run(|app, event| {
            // Stop the supervisor thread on the way out. A relaunch that
            // leaves the previous one sampling is how "Marrow uses 3% CPU
            // doing nothing" happens.
            if matches!(event, tauri::RunEvent::Exit) {
                use tauri::Manager;
                app.state::<Arc<models::Hub>>().shutdown();
            }
        });
}
