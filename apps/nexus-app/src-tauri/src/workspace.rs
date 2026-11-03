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

#[tauri::command]
pub async fn viewer_layout(
    workspace: State<'_, Arc<Workspace>>,
    viewport: Viewport,
) -> Result<(), String> {
    viewport.validate()?;
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
    fn rejects_unbounded_or_empty_viewports() {
        let mut v = Viewport {
            x: 0,
            y: 56,
            width: 1920,
            height: 1080,
            visible: true,
            scale: Scale::Fit,
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
