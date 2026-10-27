//! Local phone commands, independent of the desktop agent's trust and IPC model.
use nexus_phone::{Device, Options, Phone, Session, Status};
use tauri::State;

#[tauri::command]
pub async fn phone_status(phone: State<'_, Phone>) -> Result<Status, String> {
    Ok(phone.status().await)
}

#[tauri::command]
pub async fn phone_devices(phone: State<'_, Phone>) -> Result<Vec<Device>, String> {
    phone.devices().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn phone_pair(
    phone: State<'_, Phone>,
    address: String,
    code: String,
) -> Result<(), String> {
    phone.pair(&address, &code).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn phone_connect(phone: State<'_, Phone>, address: String) -> Result<(), String> {
    phone.connect(&address).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn phone_disconnect(phone: State<'_, Phone>, address: String) -> Result<(), String> {
    phone.disconnect(&address).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn phone_start(phone: State<'_, Phone>, options: Options) -> Result<(), String> {
    phone.start(options).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn phone_session(phone: State<'_, Phone>) -> Result<Session, String> {
    Ok(phone.session().await)
}

#[tauri::command]
pub async fn phone_stop(phone: State<'_, Phone>) -> Result<(), String> {
    phone.stop().await.map_err(|e| e.to_string())
}

/// Opens a fixed Microsoft-documented URI. Pairing/routing remain in Phone Link.
#[tauri::command]
pub async fn phone_calls() -> Result<(), String> {
    #[cfg(windows)]
    {
        let status = tokio::process::Command::new("explorer.exe")
            .arg("ms-settings:mobile-devices-addphone-direct")
            .status()
            .await
            .map_err(|e| e.to_string())?;
        // Explorer may return 1 after successfully dispatching a URI.
        if status.success() || status.code() == Some(1) {
            Ok(())
        } else {
            Err(format!("Cannot open Phone Link: {status}"))
        }
    }
    #[cfg(not(windows))]
    Err("Cellular-call audio handoff is not implemented on this platform.".into())
}
