//! The sync loop.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{debug, warn};

use crate::backend::ClipboardBackend;
use crate::content::{ClipContent, MAX_CLIPBOARD_BYTES};
use crate::error::ClipError;
use crate::io::ClipIo;

/// Which directions are on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardPolicy {
    /// Push local copies to the peer.
    pub send: bool,
    /// Apply the peer's copies locally.
    pub receive: bool,
    /// Also sync images (text only otherwise).
    pub images: bool,
    /// How often the local clipboard is checked.
    pub poll: Duration,
}

impl Default for ClipboardPolicy {
    fn default() -> Self {
        Self {
            send: true,
            receive: true,
            images: true,
            poll: Duration::from_millis(400),
        }
    }
}

/// Runs clipboard sync until the stream ends. Returns why it stopped.
pub async fn run<B: ClipboardBackend, I: ClipIo>(
    backend: B,
    mut io: I,
    policy: ClipboardPolicy,
) -> Result<(), ClipError> {
    let backend = Arc::new(Mutex::new(backend));
    let mut tick = interval(policy.poll);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // What the local clipboard held when we last looked or wrote: an unchanged digest is not news.
    let mut seen: [u8; 32] = {
        let b = Arc::clone(&backend);
        // Whatever is on the clipboard at start is not "copied during the session".
        let content = tokio::task::spawn_blocking(move || b.lock().read(policy.images))
            .await
            .map_err(|e| ClipError::Backend(e.to_string()))??;
        content.digest()
    };
    let mut seq = 0u64;
    loop {
        tokio::select! {
            _ = tick.tick(), if policy.send => {
                let b = Arc::clone(&backend);
                let read = tokio::task::spawn_blocking(move || b.lock().read(policy.images))
                    .await
                    .map_err(|e| ClipError::Backend(e.to_string()))?;
                let content = match read {
                    Ok(c) => c,
                    Err(e) => { debug!(error = %e, "clipboard read failed"); continue; }
                };
                let digest = content.digest();
                if digest == seen { continue; }
                seen = digest;
                if content.is_empty() { continue; }
                if content.len() > MAX_CLIPBOARD_BYTES {
                    warn!(bytes = content.len(), "clipboard too large to sync; skipped");
                    continue;
                }
                seq += 1;
                io.send(content.to_update(seq)).await?;
            }
            update = io.recv() => {
                let Some(update) = update? else { return Ok(()) };
                if !policy.receive { continue; }
                let mut content = match ClipContent::from_update(&update) {
                    Ok(c) => c,
                    Err(e) => { warn!(error = %e, "ignored a clipboard update"); continue; }
                };
                if !policy.images { content.image_png = None; }
                if content.is_empty() { continue; }
                let digest = content.digest();
                if digest == seen { continue; }
                // Record before writing so the next poll does not send it straight back.
                seen = digest;
                let b = Arc::clone(&backend);
                let written = tokio::task::spawn_blocking(move || b.lock().write(&content))
                    .await
                    .map_err(|e| ClipError::Backend(e.to_string()))?;
                if let Err(e) = written { warn!(error = %e, "could not update the clipboard"); }
            }
        }
    }
}
