//! Bounded child output, deadlines, and frame-based viewer startup supervision.
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::sync::watch;

use crate::Error;

const TAIL_LIMIT: usize = 8192;
const OUTPUT_LIMIT: u64 = 1 << 20;

pub(super) fn command(path: &Path) -> Command {
    let mut cmd = Command::new(path);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: the SDL viewer still creates its GUI.
    cmd
}

pub(super) fn spawn(cmd: &mut Command) -> Result<Child, Error> {
    cmd.spawn().map_err(|e| Error::Tool(format!("Cannot start phone tool. Install adb and scrcpy 3.3+, then restart linktunnel (or set NEXUS_ADB / NEXUS_SCRCPY to absolute paths): {e}")))
}

pub(super) async fn output(cmd: &mut Command, timeout: Duration) -> Result<String, Error> {
    collect(spawn(cmd)?, timeout).await
}

async fn read_bounded(reader: impl AsyncRead + Unpin) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    reader
        .take(OUTPUT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| Error::Tool(e.to_string()))?;
    if bytes.len() as u64 > OUTPUT_LIMIT {
        return Err(Error::Invalid("Phone tool output exceeded its limit."));
    }
    Ok(bytes)
}

pub(super) async fn collect(mut child: Child, timeout: Duration) -> Result<String, Error> {
    let stdout = child
        .stdout
        .take()
        .ok_or(Error::Invalid("Phone tool output unavailable."))?;
    let stderr = child
        .stderr
        .take()
        .ok_or(Error::Invalid("Phone tool errors unavailable."))?;
    let result = tokio::time::timeout(timeout, async {
        let (out, err, status) =
            tokio::try_join!(read_bounded(stdout), read_bounded(stderr), async {
                child.wait().await.map_err(|e| Error::Tool(e.to_string()))
            },)?;
        if !status.success() {
            let detail = if err.is_empty() { &out } else { &err };
            return Err(Error::Tool(format!(
                "Phone tool exited ({status}): {}",
                String::from_utf8_lossy(detail).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out).trim().to_owned())
    })
    .await;
    match result {
        Ok(value) => value,
        Err(_) => {
            let _ = child.kill().await;
            Err(Error::Tool(
                "Phone tool timed out. Check the phone and its debugging authorisation.".into(),
            ))
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct Logs(Arc<Mutex<Vec<u8>>>);

impl Logs {
    pub(super) fn tail(&self) -> String {
        self.0
            .lock()
            .map(|v| String::from_utf8_lossy(&v).trim().to_owned())
            .unwrap_or_default()
    }
}

async fn drain(mut reader: impl AsyncRead + Unpin, logs: Logs, ready: watch::Sender<bool>) {
    let mut buf = [0u8; 1024];
    let mut line = Vec::new();
    while let Ok(n) = reader.read(&mut buf).await {
        if n == 0 {
            break;
        }
        if let Ok(mut tail) = logs.0.lock() {
            tail.extend_from_slice(&buf[..n]);
            if tail.len() > TAIL_LIMIT {
                let count = tail.len() - TAIL_LIMIT;
                tail.drain(..count);
            }
        }
        for &b in &buf[..n] {
            if b == b'\n' {
                // 3.x logs Texture after decoding. 4.x uses interop textures;
                // its FPS counter is the shared indication of rendered frames.
                if frame_received(&line) {
                    let _ = ready.send(true);
                }
                line.clear();
            } else if line.len() < TAIL_LIMIT {
                line.push(b);
            }
        }
    }
}

pub(super) fn frame_received(line: &[u8]) -> bool {
    if line.starts_with(b"INFO: Texture:") {
        return true;
    }
    let Ok(text) = std::str::from_utf8(line) else {
        return false;
    };
    let Some(text) = text.strip_prefix("INFO: ") else {
        return false;
    };
    let mut words = text.split_whitespace();
    words
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .is_some_and(|count| count > 0)
        && words.next() == Some("fps")
}

pub(super) async fn viewer(cmd: &mut Command, timeout: Duration) -> Result<(Child, Logs), Error> {
    let mut child = spawn(cmd)?;
    let stdout = child
        .stdout
        .take()
        .ok_or(Error::Invalid("Phone viewer output unavailable."))?;
    let stderr = child
        .stderr
        .take()
        .ok_or(Error::Invalid("Phone viewer errors unavailable."))?;
    let logs = Logs::default();
    let (tx, mut rx) = watch::channel(false);
    tokio::spawn(drain(stdout, logs.clone(), tx.clone()));
    tokio::spawn(drain(stderr, logs.clone(), tx));
    let result = tokio::time::timeout(timeout, async {
        loop {
            tokio::select! {
                changed = rx.changed() => {
                    if changed.is_err() { return Err("Phone viewer closed before receiving a picture."); }
                    if *rx.borrow() { return Ok(()); }
                }
                _ = child.wait() => return Err("Phone viewer exited before receiving a picture."),
            }
        }
    }).await;
    match result {
        Ok(Ok(())) => Ok((child, logs)),
        other => {
            let _ = child.kill().await;
            let reason = match other {
                Ok(Err(s)) => s,
                _ => "Timed out waiting for a phone picture.",
            };
            Err(Error::Tool(format!("{reason}\n{}", logs.tail())))
        }
    }
}
