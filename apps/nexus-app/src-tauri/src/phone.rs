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
pub async fn phone_start(
    app: tauri::AppHandle,
    phone: State<'_, Phone>,
    options: Options,
    viewport: Option<crate::workspace::Viewport>,
) -> Result<(), String> {
    if let Some(viewport) = &viewport {
        viewport.validate()?;
    }
    phone.start(options).await.map_err(|e| e.to_string())?;
    if let Some(viewport) = viewport
        && let Err(error) = layout(&app, &phone, viewport).await
    {
        let _ = phone.stop().await;
        return Err(error);
    }
    Ok(())
}

pub async fn layout(
    app: &tauri::AppHandle,
    phone: &Phone,
    viewport: crate::workspace::Viewport,
) -> Result<(), String> {
    #[cfg(windows)]
    {
        use tauri::Manager;
        let parent = app
            .get_webview_window("main")
            .ok_or("main window unavailable")?
            .hwnd()
            .map_err(|e| e.to_string())?
            .0 as isize;
        phone
            .layout(
                parent,
                nexus_phone::WindowBounds {
                    x: viewport.x,
                    y: viewport.y,
                    width: viewport.width,
                    height: viewport.height,
                    visible: viewport.visible,
                    detached: viewport.detached,
                },
            )
            .await
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(windows))]
    let _ = (app, phone, viewport);
    Ok(())
}

#[tauri::command]
pub async fn phone_session(phone: State<'_, Phone>) -> Result<Session, String> {
    Ok(phone.session().await)
}

#[tauri::command]
pub async fn phone_stop(phone: State<'_, Phone>) -> Result<(), String> {
    phone.stop().await.map_err(|e| e.to_string())
}
