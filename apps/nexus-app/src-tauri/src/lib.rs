//! Nexus Remote desktop app: the dashboard window and tray, talking to the local agent over IPC.
//!
//! The app never touches the network or any key material itself. Pixels never pass through the
//! web layer either: "Connect" launches the native viewer (`nexus remote view`).

mod commands;
mod dto;
mod link;
mod tray;

use std::sync::Arc;

use tauri::{Manager, WindowEvent};

pub fn run() {
    let result = tauri::Builder::default()
        .setup(|app| {
            let link = link::AgentLink::new(link::agent_paths());
            app.manage(Arc::clone(&link));
            let handle = app.handle().clone();
            *link.tray_ok.lock() = tray::build(&handle, &link);
            link.start(handle);
            Ok(())
        })
        .on_window_event(|window, event| {
            // With a tray icon the window hides instead of quitting; without one it would be lost.
            if let WindowEvent::CloseRequested { api, .. } = event
                && let Some(link) = window.app_handle().try_state::<Arc<link::AgentLink>>()
                && *link.tray_ok.lock()
            {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::snapshot,
            commands::connect_screen,
            commands::wake,
            commands::power,
            commands::set_unattended,
            commands::set_lockdown,
            commands::unpair,
            commands::pair_offer,
            commands::pair_join,
            commands::pair_confirm,
            commands::pair_cancel,
            commands::consent_decide,
            commands::terminal_open,
            commands::terminal_input,
            commands::terminal_resize,
            commands::terminal_close,
            commands::quit,
        ])
        .run(tauri::generate_context!());
    if let Err(e) = result {
        eprintln!("nexus-app: {e}");
        std::process::exit(1);
    }
}
