//! Supervise viewer startup: a spawned process is not yet a remote desktop.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

pub async fn launch(program: PathBuf, args: Vec<String>) -> Result<(), String> {
    launch_with_timeout(program, args, Duration::from_secs(90)).await
}

async fn launch_with_timeout(
    program: PathBuf,
    args: Vec<String>,
    timeout: Duration,
) -> Result<(), String> {
    let mut child = Command::new(program)
        .args(args)
        .env("NEXUS_VIEWER_STATUS", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("cannot start the viewer (is `nexus` installed?): {e}"))?;
    let stdout = child.stdout.take().ok_or("viewer output unavailable")?;
    let stderr = child.stderr.take().ok_or("viewer errors unavailable")?;
    // Drain stderr for the whole viewer lifetime, retaining only a bounded tail.
    let errors = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut tail = Vec::new();
        let mut buf = [0u8; 1024];
        while let Ok(n) = reader.read(&mut buf).await {
            if n == 0 {
                break;
            }
            tail.extend_from_slice(&buf[..n]);
            if tail.len() > 8192 {
                tail.drain(..tail.len() - 8192);
            }
        }
        String::from_utf8_lossy(&tail).trim().to_owned()
    });
    let mut lines = BufReader::new(stdout).lines();
    let startup = tokio::time::timeout(timeout, async {
        while let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? {
            let Ok(status) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            match status["nexusViewer"].as_str() {
                Some("ready") => return Ok(()),
                Some("ended") => {
                    return Err(status["reason"]
                        .as_str()
                        .unwrap_or("viewer closed before receiving the desktop")
                        .to_owned());
                }
                _ => {}
            }
        }
        Err("viewer exited before receiving the desktop".to_owned())
    })
    .await;
    match startup {
        Ok(Ok(())) => {
            // Keep draining stdout and reap the process after the viewer closes.
            tauri::async_runtime::spawn(async move {
                while matches!(lines.next_line().await, Ok(Some(_))) {}
                let _ = child.wait().await;
                let _ = errors.await;
            });
            Ok(())
        }
        result => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            let detail = errors.await.unwrap_or_default();
            let reason = match result {
                Ok(Err(reason)) => reason,
                _ => "timed out waiting for the remote desktop; check the PC's agent and network, then try Connect again".to_owned(),
            };
            if detail.is_empty() {
                Err(reason)
            } else {
                Err(format!("{reason}: {detail}"))
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    async fn shell(script: &str, timeout: Duration) -> Result<(), String> {
        launch_with_timeout("/bin/sh".into(), vec!["-c".into(), script.into()], timeout).await
    }

    #[tokio::test]
    async fn requires_a_remote_picture_not_just_a_process() {
        assert!(shell("exit 0", Duration::from_secs(2)).await.is_err());
        assert!(
            shell(
                "printf '%s\\n' '{\"nexusViewer\":\"ready\"}'",
                Duration::from_secs(2)
            )
            .await
            .is_ok()
        );
    }

    #[tokio::test]
    async fn reports_startup_failures_and_times_out_hung_viewers() {
        let error = shell(
            "echo 'session setup failed' >&2; exit 1",
            Duration::from_secs(2),
        )
        .await
        .unwrap_err();
        assert!(error.contains("session setup failed"));
        let error = shell(
            "printf '%s\\n' '{\"nexusViewer\":\"ended\",\"reason\":\"PC refused access\"}'",
            Duration::from_secs(2),
        )
        .await
        .unwrap_err();
        assert!(error.contains("PC refused access"));
        assert!(
            shell("exec sleep 10", Duration::from_millis(50))
                .await
                .unwrap_err()
                .contains("timed out")
        );
    }
}
