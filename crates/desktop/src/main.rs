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

mod commands;
mod state;

use std::sync::Arc;

use state::Core;

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

    tauri::Builder::default()
        .manage(core)
        .invoke_handler(tauri::generate_handler![
            commands::search,
            commands::list_workspaces,
            commands::index_health,
            commands::file_detail,
            commands::read_region,
            commands::open_path,
            commands::reveal_path,
            commands::list_files,
        ])
        .run(tauri::generate_context!())
        .expect("failed to start the Marrow window");
}
