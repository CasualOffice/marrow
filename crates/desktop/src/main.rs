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

/// Stop, and make sure a person finds out.
///
/// **`eprintln!` is not "loudly" for an app somebody double-clicked.** Both
/// startup failures below used to print to stderr and exit, and stderr from a
/// Finder-launched `.app` goes nowhere at all: the user sees one bounce in the
/// Dock and no dialog, which is indistinguishable from the invalid-signature
/// failure that killed v0.0.1 and v0.0.2 and gives them nothing to act on or
/// report. Reported from real use — "it bounced and didn't open" — on a machine
/// that was not the one that built it.
///
/// So both: the line on stderr for anybody running this from a terminal, and a
/// dialog for everybody else. `osascript` rather than `NSAlert` because this
/// runs *before* Tauri has started an `NSApplication`, and a separate process
/// has no opinion about that.
fn fatal(code: i32, title: &str, detail: &str) -> ! {
    eprintln!("{title}: {detail}");

    // AppleScript string literals take double quotes and backslashes as
    // escapes, and a path or a SQLite message can contain both.
    fn escape(s: &str) -> String {
        s.replace('\\', "\\\\").replace('"', "\\\"")
    }
    let script = format!(
        r#"display alert "{}" message "{}" as critical"#,
        escape(title),
        escape(detail)
    );
    // Best effort. If this cannot run we are no worse off than before it
    // existed, and the exit code and the stderr line still stand.
    let _ = std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .status();
    std::process::exit(code);
}

fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();

    let dir = data_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        fatal(
            1,
            "Marrow cannot start",
            &format!(
                "It could not create the folder it keeps its index in.\n\n{}\n\n{e}",
                dir.display()
            ),
        );
    }

    let core = match Core::open(dir.join(marrow_store::DB_FILE_NAME)) {
        Ok(c) => Arc::new(c),
        Err(e) => {
            // Failing loudly beats a window that opens and answers nothing —
            // and a dialog is what "loudly" means to somebody who launched this
            // from the Dock. The message already names a cause and an action;
            // the code goes with it so a report can be acted on.
            fatal(
                4,
                "Marrow cannot start",
                &format!("{}\n\n[{}]", e.message(), e.code()),
            );
        }
    };

    // Probes the machine, detects any local runtime and starts the supervisor
    // thread. Nothing is loaded — that is the point of LLM-047.
    //
    // The indexed roots are passed so the model directory can refuse to sit
    // inside one: a model writing into an indexed folder would have its own
    // output re-indexed and cited back (SUP-011, and the `origin = SELF` rule).
    let indexed_roots: Vec<std::path::PathBuf> = core
        .workspaces()
        .map(|ws| {
            ws.iter()
                .map(|w| std::path::PathBuf::from(&w.path))
                .collect()
        })
        .unwrap_or_default();
    let hub = Arc::new(models::Hub::start(dir.join("models"), &indexed_roots));

    // **A stale index is worse than no index.** Until this existed the index
    // only moved when somebody ran `marrow index` in a terminal, so how fresh
    // an answer was depended on when the author last remembered. Not fatal if
    // it cannot start: the app still searches what it has, and `index_health`
    // reports that nothing is watching rather than implying everything is fine.
    let watchers = match marrow_desktop::Watchers::start(Arc::clone(&core), dir.clone()) {
        Ok(w) => Some(Arc::new(w)),
        Err(e) => {
            tracing::warn!(error = %e, "could not start the folder watchers");
            None
        }
    };

    tauri::Builder::default()
        .manage(core)
        .manage(hub)
        .manage(watchers)
        // **The paths a drop delivers arrive here, in Rust, from the OS.**
        // Tauri forwards the same list to the WebView so it can draw a hover
        // overlay, but nothing sends it back: no command accepts a source path.
        // The window cannot ask Marrow to read a file it invented, because
        // there is nothing to ask.
        .on_window_event(|window, event| {
            use tauri::Manager;
            if let tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Drop { paths, .. }) = event {
                commands::handle_drop(window.app_handle(), paths.clone());
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::search,
            commands::list_workspaces,
            commands::list_projects,
            commands::add_workspace,
            commands::add_files,
            commands::scratch_status,
            commands::clear_scratch,
            commands::index_health,
            commands::reindex,
            commands::file_detail,
            commands::read_region,
            commands::open_path,
            commands::reveal_path,
            commands::list_files,
            commands::models_overview,
            commands::refresh_model_detection,
            commands::set_ai_profile,
            commands::set_generator_model,
            commands::download_model,
            commands::delete_model,
            commands::user_name,
            commands::set_user_name,
            commands::cancel_model_download,
            commands::dismiss_model_download,
            commands::install_runtime,
            commands::cancel_runtime_install,
            commands::dismiss_runtime_install,
            commands::provider_settings,
            commands::set_cloud_provider,
            commands::clear_cloud_provider,
            commands::ask,
            commands::cancel_ask,
            commands::release_model,
            commands::forget_conversation,
            commands::start_semantic_backfill,
            commands::stop_semantic_backfill,
            commands::list_conversations,
            commands::search_conversations,
            commands::load_conversation,
            commands::save_turn,
            commands::rename_conversation,
            commands::delete_conversation,
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
