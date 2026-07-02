//! Tauri GUI for naque.
//!
//! Assembly is shared with the terminal binary via `naque::setup`; this crate
//! is a second frontend over the same headless `naque::App` engine. A single
//! background pump task owns the `App` (mirroring the TUI's event loop) and
//! streams state + agent events to the webview; the frontend drives it through
//! `tauri::command` IPC.

mod cli;
mod commands;
mod ipc;
mod pump;

use clap::Parser;

pub fn run() {
    let launch = cli::Args::parse().launch_config();
    tauri::Builder::default()
        .manage(ipc::AppState::new(launch))
        .invoke_handler(tauri::generate_handler![
            commands::submit,
            commands::cancel_turn,
            commands::respond_approval,
            commands::respond_path_approval,
            commands::list_profiles,
            commands::list_environments,
            commands::switch_profile,
            commands::get_state,
            commands::auto_connect,
            commands::connect,
            commands::launcher_profiles,
            commands::launcher_environments,
            commands::llm_status,
            commands::set_llm_credentials,
            commands::clear_llm_credentials,
        ])
        .run(tauri::generate_context!())
        .expect("error while running naque-gui");
}
