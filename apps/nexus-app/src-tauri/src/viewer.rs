//! Own the native viewer process, including pending consent, layout and teardown.
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

enum ViewerEvent {
    Ended(String),
    Stats(String),
    Fullscreen,
    Geometry(u32, u32),
}

pub async fn launch_managed(
    app: tauri::AppHandle,
    id: String,
    program: PathBuf,
    args: Vec<String>,
    parent: Option<isize>,
    viewport: Option<crate::workspace::Viewport>,
    controls: tokio::sync::mpsc::Receiver<crate::workspace::Control>,
) -> Result<(), String> {
    use tauri::Emitter as _;
    use tauri::Manager as _;
    supervise(
        program,
        args,
        parent,
        viewport,
        controls,
        Duration::from_secs(90),
        move |event| {
            let event = match event {
                ViewerEvent::Fullscreen => {
                    if let Some(window) = app.get_webview_window("main") {
                        let result = window
                            .is_fullscreen()
                            .and_then(|fullscreen| window.set_fullscreen(!fullscreen));
                        if let Err(error) = result {
                            tracing::warn!(%error, "cannot toggle workspace fullscreen");
                        }
                    }
                    return;
                }
                ViewerEvent::Geometry(width, height) => crate::dto::AppEvent::ViewerGeometry {
                    id: id.clone(),
                    width,
                    height,
                },
                ViewerEvent::Ended(reason) => crate::dto::AppEvent::ViewerEnded {
                    id: id.clone(),
                    reason,
                },
                ViewerEvent::Stats(text) => crate::dto::AppEvent::ViewerStats {
                    id: id.clone(),
                    text,
                },
            };
            let _ = app.emit(crate::link::EVENT, event);
        },
    )
    .await
}

/// Keep the native child alive, respond to layout/close even while consent is pending, and
/// acknowledge Connect only after the first decoded picture.
async fn supervise(
    program: PathBuf,
    args: Vec<String>,
    parent: Option<isize>,
    viewport: Option<crate::workspace::Viewport>,
    mut controls: tokio::sync::mpsc::Receiver<crate::workspace::Control>,
    startup_timeout: Duration,
    notify: impl Fn(ViewerEvent) + Send + 'static,
) -> Result<(), String> {
    use crate::workspace::Control;
    use tokio::io::AsyncWriteExt as _;
    let mut command = Command::new(program);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    command
        .args(args)
        .env("NEXUS_VIEWER_STATUS", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(parent) = parent {
        command.env("NEXUS_VIEWER_PARENT", parent.to_string());
    }
    if let Some(viewport) = viewport {
        command.env(
            "NEXUS_VIEWER_VIEWPORT",
            serde_json::to_string(&viewport).map_err(|e| e.to_string())?,
        );
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("cannot start desktop viewer: {e}"))?;
    let mut input = child.stdin.take().ok_or("viewer input unavailable")?;
    let mut output =
        BufReader::new(child.stdout.take().ok_or("viewer output unavailable")?).lines();
    let stderr = child.stderr.take().ok_or("viewer errors unavailable")?;
    let errors = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut tail = Vec::new();
        let mut buffer = [0u8; 1024];
        while let Ok(n) = reader.read(&mut buffer).await {
            if n == 0 {
                break;
            }
            tail.extend_from_slice(&buffer[..n]);
            if tail.len() > 8192 {
                tail.drain(..tail.len() - 8192);
            }
        }
        String::from_utf8_lossy(&tail).trim().to_owned()
    });
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let mut ready = Some(ready_tx);
        let timeout = tokio::time::sleep(startup_timeout);
        tokio::pin!(timeout);
        let mut close_ack = None;
        let reason = loop {
            tokio::select! {
                command = controls.recv() => match command {
                    Some(Control::Layout(viewport)) => {
                        match serde_json::to_vec(&viewport) {
                            Ok(mut bytes) => { bytes.push(b'\n'); if let Err(e) = input.write_all(&bytes).await { break format!("viewer layout failed: {e}"); } }
                            Err(e) => break e.to_string(),
                        }
                    }
                    Some(Control::Close(ack)) => { close_ack = Some(ack); break "closed".into(); }
                    None => break "closed".into(),
                },
                line = output.next_line() => match line {
                    Ok(Some(line)) => {
                        if let Ok(status) = serde_json::from_str::<serde_json::Value>(&line) {
                            match status["nexusViewer"].as_str() {
                                Some("ready") => { if let Some(tx) = ready.take() { let _ = tx.send(Ok(())); } }
                                Some("stats") => { if let Some(text) = status["text"].as_str() { notify(ViewerEvent::Stats(text.to_owned())); } }
                                Some("geometry") => {
                                    if let (Some(width), Some(height)) = (status["width"].as_u64(), status["height"].as_u64())
                                        && (1..=32768).contains(&width) && (1..=32768).contains(&height) {
                                        notify(ViewerEvent::Geometry(width as u32, height as u32));
                                    }
                                }
                                Some("fullscreen") => notify(ViewerEvent::Fullscreen),
                                Some("ended") => break status["reason"].as_str().unwrap_or("desktop ended").to_owned(),
                                _ => {},
                            }
                        }
                    }
                    Ok(None) => break "viewer exited".into(),
                    Err(e) => break e.to_string(),
                },
                _ = &mut timeout, if ready.is_some() => break "timed out waiting for the remote desktop".into(),
            }
        };
        let _ = child.kill().await;
        let _ = child.wait().await;
        let detail = errors.await.unwrap_or_default();
        let reason = if reason == "closed" || detail.is_empty() {
            reason
        } else {
            format!("{reason}: {detail}")
        };
        if let Some(tx) = ready {
            let _ = tx.send(Err(reason.clone()));
        }
        notify(ViewerEvent::Ended(reason));
        if let Some(ack) = close_ack {
            let _ = ack.send(());
        }
    });
    ready_rx
        .await
        .map_err(|_| "viewer supervisor stopped".to_owned())?
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::workspace::{Control, Scale, Viewport};
    use tokio::sync::{mpsc, oneshot};

    fn args(script: &str) -> Vec<String> {
        ["-NoProfile", "-NonInteractive", "-Command", script]
            .map(str::to_owned)
            .to_vec()
    }
    #[tokio::test]
    async fn close_interrupts_pending_startup_and_reaps_before_acknowledging() {
        let (tx, rx) = mpsc::channel(4);
        let start = tokio::spawn(supervise(
            "powershell.exe".into(),
            args("Start-Sleep -Seconds 30"),
            None,
            None,
            rx,
            Duration::from_secs(10),
            |_| {},
        ));
        let (ack, closed) = oneshot::channel();
        tx.send(Control::Close(ack)).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), closed)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(start.await.unwrap().unwrap_err(), "closed");
    }
    #[tokio::test]
    async fn needs_a_picture_and_times_out_a_hung_start() {
        let (_tx, rx) = mpsc::channel(4);
        let error = supervise(
            "powershell.exe".into(),
            args("[Console]::Error.WriteLine('capture failed'); exit 1"),
            None,
            None,
            rx,
            Duration::from_secs(10),
            |_| {},
        )
        .await
        .unwrap_err();
        assert!(error.contains("capture failed"));
        let (_tx, rx) = mpsc::channel(4);
        let error = supervise(
            "powershell.exe".into(),
            args("Start-Sleep -Seconds 30"),
            None,
            None,
            rx,
            Duration::from_millis(50),
            |_| {},
        )
        .await
        .unwrap_err();
        assert!(error.contains("timed out"));
    }
    #[tokio::test]
    async fn gui_child_has_no_console_and_receives_layout_after_ready() {
        let script = r#"
Add-Type 'using System; using System.Runtime.InteropServices; public static class ViewerConsole { [DllImport("kernel32.dll")] public static extern IntPtr GetConsoleWindow(); }'
if ([ViewerConsole]::GetConsoleWindow() -ne [IntPtr]::Zero) { Write-Error 'unexpected console'; exit 1 }
Write-Output '{"nexusViewer":"ready"}'
$layout = [Console]::ReadLine() | ConvertFrom-Json
if ($layout.width -eq 777 -and $layout.scale -eq 'fill') { Write-Output '{"nexusViewer":"ended","reason":"layout received"}' }
"#;
        let (tx, rx) = mpsc::channel(4);
        let (ended, mut result) = mpsc::unbounded_channel();
        supervise(
            "powershell.exe".into(),
            args(script),
            Some(1),
            None,
            rx,
            Duration::from_secs(20),
            move |event| {
                if let ViewerEvent::Ended(reason) = event {
                    let _ = ended.send(reason);
                }
            },
        )
        .await
        .unwrap();
        tx.send(Control::Layout(Viewport {
            x: 20,
            y: 80,
            width: 777,
            height: 500,
            visible: true,
            scale: Scale::Fill,
            detached: false,
        }))
        .await
        .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), result.recv())
                .await
                .unwrap()
                .unwrap(),
            "layout received"
        );
    }
}
