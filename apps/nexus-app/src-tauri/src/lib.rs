//! linktunnel desktop app: the dashboard window and tray, talking to the local agent over IPC.
//!
//! Desktop sessions go through the local agent; Android control uses local adb/scrcpy.
//! Pixels never pass through the web layer: both viewers are native processes.

mod alert;
mod commands;
mod dto;
mod link;
mod phone;
mod tray;
mod viewer;

use std::sync::Arc;

use tauri::{Manager, WindowEvent};

pub fn run() {
    let result = tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            let link = link::AgentLink::new(link::agent_paths());
            app.manage(Arc::clone(&link));
            app.manage(nexus_phone::Phone::default());
            let handle = app.handle().clone();
            *link.tray_ok.lock() = tray::build(&handle, &link);
            alert::register_panic_hotkey(&handle);
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
            commands::disconnect_sessions,
            commands::panic,
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
            phone::phone_status,
            phone::phone_devices,
            phone::phone_pair,
            phone::phone_connect,
            phone::phone_disconnect,
            phone::phone_start,
            phone::phone_session,
            phone::phone_stop,
            phone::phone_calls,
        ])
        .build(tauri::generate_context!());
    match result {
        Ok(app) => app.run(|handle, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                // The GUI exit callback runs outside the async runtime. Cancel an
                // in-flight start as well as reaping an already running viewer.
                let phone = handle.state::<nexus_phone::Phone>();
                let _ = tauri::async_runtime::block_on(phone.stop());
            }
        }),
        Err(e) => {
            eprintln!("nexus-app: {e}");
            std::process::exit(1);
        }
    }
}
