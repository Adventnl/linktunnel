//! Native Windows cellular calling; no external calling application is launched.
use nexus_phone::calls::{Action, Calls, Snapshot};
use tauri::State;

#[tauri::command]
pub async fn phone_calls_status(calls: State<'_, Calls>) -> Result<Snapshot, String> {
    calls.status().await
}

#[tauri::command]
pub async fn phone_calls_connect(
    calls: State<'_, Calls>,
    device_id: String,
) -> Result<Snapshot, String> {
    calls.connect(device_id).await
}

#[tauri::command]
pub async fn phone_calls_disconnect(calls: State<'_, Calls>) -> Result<Snapshot, String> {
    calls.disconnect().await
}

#[tauri::command]
pub async fn phone_calls_action(
    calls: State<'_, Calls>,
    action: Action,
) -> Result<Snapshot, String> {
    calls.action(action).await
}

/// Only fixed OS settings pages; never accepts a caller-provided URI.
#[tauri::command]
pub async fn phone_calls_settings(audio: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        let uri = if audio {
            "ms-settings:sound"
        } else {
            "ms-settings:bluetooth"
        };
        let status = tokio::process::Command::new("explorer.exe")
            .arg(uri)
            .creation_flags(0x0800_0000)
            .status()
            .await
            .map_err(|e| e.to_string())?;
        if status.success() || status.code() == Some(1) {
            Ok(())
        } else {
            Err(format!("Cannot open Windows settings: {status}"))
        }
    }
    #[cfg(not(windows))]
    {
        let _ = audio;
        Err("Native cellular calling settings are currently available on Windows only.".into())
    }
}
