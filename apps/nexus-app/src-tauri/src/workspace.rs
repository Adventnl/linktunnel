//! Native viewer ownership for the desktop workspace. Geometry is physical client pixels.
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{Manager, State};
use tokio::sync::{Mutex, mpsc, oneshot};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Viewport {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub visible: bool,
    pub scale: Scale,
    /// Lift the native picture into a decorated, aspect-correct window.
    #[serde(default)]
    pub detached: bool,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scale {
    Fit,
    Fill,
    Pixel,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    Auto,
    Sharp,
    Smooth,
}
#[derive(Debug, Deserialize)]
pub struct ScreenOptions {
    pub viewport: Viewport,
    pub quality: Quality,
}
impl Viewport {
    pub fn validate(&self) -> Result<(), String> {
        if self.x < 0
            || self.y < 0
            || self.x > 32768
            || self.y > 32768
            || !(1..=32768).contains(&self.width)
            || !(1..=32768).contains(&self.height)
        {
            return Err("invalid desktop viewport".into());
        }
        Ok(())
    }
}
pub enum Control {
    Layout(Viewport),
    Close(oneshot::Sender<()>),
}
#[derive(Default)]
pub struct Workspace {
    pub control: Mutex<Option<mpsc::Sender<Control>>>,
}
impl Workspace {
    pub async fn close(&self) -> Result<(), String> {
        let control = self.control.lock().await.take();
        if let Some(control) = control {
            let (tx, rx) = oneshot::channel();
            if control.send(Control::Close(tx)).await.is_ok() {
                tokio::time::timeout(std::time::Duration::from_secs(5), rx)
                    .await
                    .map_err(|_| "viewer did not stop")?
                    .map_err(|_| "viewer closed without acknowledgement")?;
            }
        }
        Ok(())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopInfo {
    platform: &'static str,
    embedded: bool,
    account: Option<String>,
}

fn account_name(status: &serde_json::Value) -> Option<String> {
    let user_id = status["Self"]["UserID"].as_u64()?.to_string();
    let profile = &status["User"][user_id];
    ["DisplayName", "LoginName"].iter().find_map(|key| {
        profile[*key]
            .as_str()
            .filter(|name| !name.trim().is_empty())
            .map(str::to_owned)
    })
}

#[tauri::command]
pub async fn desktop_info() -> DesktopInfo {
    let mut candidates = vec![std::path::PathBuf::from("tailscale")];
    #[cfg(target_os = "macos")]
    candidates.extend(
        [
            "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
            "/usr/local/bin/tailscale",
            "/opt/homebrew/bin/tailscale",
        ]
        .map(std::path::PathBuf::from),
    );
    #[cfg(windows)]
    if let Some(program_files) = std::env::var_os("ProgramFiles") {
        candidates.push(std::path::PathBuf::from(program_files).join("Tailscale/tailscale.exe"));
    }
    let account = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        for binary in candidates {
            let mut command = tokio::process::Command::new(binary);
            #[cfg(windows)]
            command.creation_flags(0x0800_0000);
            let output = command
                .args(["status", "--json"])
                .kill_on_drop(true)
                .output()
                .await;
            if let Ok(output) = output
                && output.status.success()
                && let Ok(status) = serde_json::from_slice::<serde_json::Value>(&output.stdout)
                && let Some(account) = account_name(&status)
            {
                return Some(account);
            }
        }
        None
    })
    .await
    .ok()
    .flatten();
    DesktopInfo {
        platform: std::env::consts::OS,
        embedded: cfg!(windows),
        account,
    }
}

#[tauri::command]
pub async fn viewer_layout(
    app: tauri::AppHandle,
    phone: State<'_, nexus_phone::Phone>,
    workspace: State<'_, Arc<Workspace>>,
    viewport: Viewport,
) -> Result<(), String> {
    viewport.validate()?;
    crate::phone::layout(&app, &phone, viewport.clone()).await?;
    let control = workspace.control.lock().await.clone();
    if let Some(control) = control {
        let _ = control.send(Control::Layout(viewport)).await;
    }
    Ok(())
}
#[tauri::command]
pub async fn viewer_close(workspace: State<'_, Arc<Workspace>>) -> Result<(), String> {
    workspace.close().await
}

#[tauri::command]
pub async fn window_action(window: tauri::WebviewWindow, action: String) -> Result<(), String> {
    let result = match action.as_str() {
        "minimize" => window.minimize(),
        "maximize" => {
            if window.is_maximized().map_err(|e| e.to_string())? {
                window.unmaximize()
            } else {
                window.maximize()
            }
        }
        "close" => window.close(),
        "fullscreen" => window.set_fullscreen(!window.is_fullscreen().map_err(|e| e.to_string())?),
        "drag" => window.start_dragging(),
        _ => return Err("unknown window action".into()),
    };
    result.map_err(|e| e.to_string())
}

pub async fn connect(
    app: tauri::AppHandle,
    id: String,
    options: Option<ScreenOptions>,
) -> Result<(), String> {
    let workspace = app.state::<Arc<Workspace>>();
    workspace.close().await?;
    let mut args = Vec::new();
    if crate::link::agent_scope() == nexus_core::Scope::System {
        args.push("--system".into());
    }
    args.extend(["remote", "view", &id, "--audio"].map(str::to_owned));
    if let Some(options) = &options {
        options.viewport.validate()?;
        match options.quality {
            Quality::Auto => {}
            Quality::Sharp => args.extend(["--fps", "30", "--mbps", "20"].map(str::to_owned)),
            Quality::Smooth => args.extend(["--fps", "60", "--mbps", "30"].map(str::to_owned)),
        }
    }
    #[cfg(windows)]
    let parent = Some(
        app.get_webview_window("main")
            .ok_or("main window unavailable")?
            .hwnd()
            .map_err(|e| e.to_string())?
            .0 as isize,
    );
    #[cfg(not(windows))]
    let parent: Option<isize> = None;
    let (control, receiver) = mpsc::channel(16);
    *workspace.control.lock().await = Some(control);
    crate::viewer::launch_managed(
        app,
        id,
        crate::link::nexus_cli(),
        args,
        parent,
        options.map(|o| o.viewport),
        receiver,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn account_uses_the_local_user_not_a_peer_profile() {
        let status = serde_json::json!({"Self": {"UserID": 42}, "User": {
            "7": {"DisplayName": "Other account"},
            "42": {"DisplayName": "Local account", "LoginName": "local@example.test"}
        }});
        assert_eq!(account_name(&status).as_deref(), Some("Local account"));
        assert_eq!(
            account_name(&serde_json::json!({"Self": {"UserID": 42}})),
            None
        );
    }

    #[test]
    fn rejects_unbounded_or_empty_viewports() {
        let mut v = Viewport {
            x: 0,
            y: 56,
            width: 1920,
            height: 1080,
            visible: true,
            scale: Scale::Fit,
            detached: false,
        };
        assert!(v.validate().is_ok());
        v.width = 0;
        assert!(v.validate().is_err());
        v.width = u32::MAX;
        assert!(v.validate().is_err());
        v.width = 1920;
        v.x = -1;
        assert!(v.validate().is_err());
    }
}
