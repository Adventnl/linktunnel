//! Tauri commands: everything the UI can ask for. Each is a thin call to the local agent.

use std::sync::Arc;

use base64::Engine as _;
use nexus_proto::v1::{OpenSession, OpenTerminal, PowerActionKind};
use tauri::{AppHandle, State};

use crate::dto::{self, Snapshot};
use crate::link::{self, AgentLink};

type Link<'a> = State<'a, Arc<AgentLink>>;
type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[tauri::command]
pub async fn snapshot(link: Link<'_>) -> CmdResult<Snapshot> {
    Ok(link.snapshot().await)
}

/// Opens the native viewer window by running `nexus remote view` (video never passes through the
/// web layer; the viewer is its own wgpu/winit window).
#[tauri::command]
pub async fn connect_screen(link: Link<'_>, id: String) -> CmdResult<()> {
    let name = device_name(&link, &id).await?;
    spawn_viewer(&name)
}

pub fn spawn_viewer(name: &str) -> CmdResult<()> {
    let mut cmd = tokio::process::Command::new(link::nexus_cli());
    cmd.args(["remote", "view", name, "--audio"])
        .stdin(std::process::Stdio::null());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start the viewer (is `nexus` installed?): {e}"))?;
    // Reap it when it ends so it does not linger as a zombie.
    tauri::async_runtime::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(())
}

async fn device_name(link: &Link<'_>, id: &str) -> CmdResult<String> {
    let want = dto::unhex(id)?;
    let list = link.client()?.list_devices().await.map_err(err)?;
    list.devices
        .iter()
        .find(|d| d.device_id.as_ref() == want.as_slice())
        .map(|d| d.name.clone())
        .ok_or_else(|| "no such device".to_owned())
}

#[tauri::command]
pub async fn wake(link: Link<'_>, id: String) -> CmdResult<()> {
    link.client()?
        .wake_device(dto::unhex(&id)?, Vec::new())
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn power(link: Link<'_>, id: String, action: String) -> CmdResult<()> {
    let kind = match action.as_str() {
        "lock" => PowerActionKind::Lock,
        "sleep" => PowerActionKind::Sleep,
        "restart" => PowerActionKind::Restart,
        "shutdown" => PowerActionKind::Shutdown,
        other => return Err(format!("unknown power action `{other}`")),
    };
    let client = link.client()?;
    let session = client
        .open_session(OpenSession {
            device_id: dto::unhex(&id)?.into(),
            want_video: false,
            ..Default::default()
        })
        .await
        .map_err(err)?;
    let result = client.power(session.session_id, kind).await.map_err(err);
    let _ = client.close_session(session.session_id).await;
    result
}

#[tauri::command]
pub async fn set_unattended(link: Link<'_>, id: String, on: bool) -> CmdResult<()> {
    link.client()?
        .set_unattended(dto::unhex(&id)?, on)
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn set_lockdown(app: AppHandle, link: Link<'_>, on: bool) -> CmdResult<()> {
    link.client()?.set_lockdown(on).await.map_err(err)?;
    // The agent pushes no event for this; refresh what the UI and tray show.
    let snap = link.snapshot().await;
    crate::tray::refresh(&app, &link);
    tauri::Emitter::emit(
        &app,
        link::EVENT,
        dto::AppEvent::Snapshot { snapshot: snap },
    )
    .map_err(err)
}

/// Ends the live incoming sessions of one device, or of everyone when `id` is empty.
#[tauri::command]
pub async fn disconnect_sessions(link: Link<'_>, id: Option<String>) -> CmdResult<()> {
    let device = match id {
        Some(id) if !id.is_empty() => dto::unhex(&id)?,
        _ => Vec::new(),
    };
    link.client()?.disconnect_sessions(device).await.map_err(err)
}

/// Lockdown on and everyone disconnected.
#[tauri::command]
pub async fn panic(app: AppHandle, link: Link<'_>) -> CmdResult<()> {
    crate::alert::panic_now(&app, link.inner()).await
}

#[tauri::command]
pub async fn unpair(link: Link<'_>, id: String) -> CmdResult<()> {
    link.client()?.unpair(dto::unhex(&id)?).await.map_err(err)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Offer {
    pairing_id: u64,
    code: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Joined {
    pairing_id: u64,
}

#[tauri::command]
pub async fn pair_offer(link: Link<'_>) -> CmdResult<Offer> {
    // Defaults match `nexus pair offer`: the other device may use this one with every grant.
    let started = link
        .client()?
        .pairing_offer(nexus_core_grants())
        .await
        .map_err(err)?;
    Ok(Offer {
        pairing_id: started.pairing_id,
        code: started.code,
    })
}

#[tauri::command]
pub async fn pair_join(link: Link<'_>, code: String) -> CmdResult<Joined> {
    let started = link
        .client()?
        .pairing_join(&code, nexus_core_grants(), vec!["screen".to_owned()])
        .await
        .map_err(err)?;
    Ok(Joined {
        pairing_id: started.pairing_id,
    })
}

/// The grants `nexus pair` hands out by default (kept in step with `nexus_trust::grants::all`).
fn nexus_core_grants() -> Vec<String> {
    [
        "screen",
        "input",
        "clipboard",
        "files",
        "terminal",
        "power",
        "wake",
        "audio",
    ]
    .iter()
    .map(ToString::to_string)
    .collect()
}

#[tauri::command]
pub async fn pair_confirm(link: Link<'_>, pairing_id: u64, words_match: bool) -> CmdResult<()> {
    link.client()?
        .pairing_confirm(pairing_id, words_match)
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn pair_cancel(link: Link<'_>, pairing_id: u64) -> CmdResult<()> {
    link.client()?.pairing_cancel(pairing_id).await.map_err(err)
}

#[tauri::command]
pub async fn consent_decide(
    link: Link<'_>,
    request_id: u64,
    allow: bool,
    remember: bool,
) -> CmdResult<()> {
    link.client()?
        .consent_decision(request_id, allow, remember)
        .await
        .map_err(err)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Opened {
    terminal_id: u64,
}

#[tauri::command]
pub async fn terminal_open(
    link: Link<'_>,
    id: String,
    cols: u32,
    rows: u32,
    name: Option<String>,
) -> CmdResult<Opened> {
    let client = link.client()?;
    let session = client
        .open_session(OpenSession {
            device_id: dto::unhex(&id)?.into(),
            want_video: false,
            ..Default::default()
        })
        .await
        .map_err(err)?;
    let started = client
        .open_terminal(OpenTerminal {
            session_id: session.session_id,
            cols: cols.clamp(1, 500),
            rows: rows.clamp(1, 300),
            name: name.unwrap_or_default(),
            ..Default::default()
        })
        .await;
    match started {
        Ok(t) => {
            link.terminals
                .lock()
                .insert(t.terminal_id, session.session_id);
            Ok(Opened {
                terminal_id: t.terminal_id,
            })
        }
        Err(e) => {
            let _ = client.close_session(session.session_id).await;
            Err(err(e))
        }
    }
}

#[tauri::command]
pub async fn terminal_input(link: Link<'_>, terminal_id: u64, data_b64: String) -> CmdResult<()> {
    let data = base64::engine::general_purpose::STANDARD
        .decode(data_b64)
        .map_err(err)?;
    // A paste is the largest legitimate input; anything bigger is a bug or abuse.
    if data.len() > 1 << 20 {
        return Err("input too large".into());
    }
    link.client()?
        .terminal_input(terminal_id, data)
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn terminal_resize(
    link: Link<'_>,
    terminal_id: u64,
    cols: u32,
    rows: u32,
) -> CmdResult<()> {
    link.client()?
        .terminal_resize(terminal_id, cols.clamp(1, 500), rows.clamp(1, 300))
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn terminal_close(link: Link<'_>, terminal_id: u64, kill: bool) -> CmdResult<()> {
    let client = link.client()?;
    let session = link.terminals.lock().remove(&terminal_id);
    let r = client.close_terminal(terminal_id, kill).await.map_err(err);
    if let Some(s) = session {
        let _ = client.close_session(s).await;
    }
    r
}

#[tauri::command]
pub fn quit(app: AppHandle) {
    app.exit(0);
}
