//! The viewer's network side: a background thread with its own async runtime that talks to the
//! local agent, decodes video, and forwards input. The UI thread only sees [`UiEvent`]s.

use std::time::{Duration, Instant};

use nexus_core::clock::now_us;
use nexus_ipc::{IpcClient, IpcPaths};
use nexus_media::controller::VideoFrame;
use nexus_media::viewer::{HudCounter, Picture, VideoDecoder};
use nexus_proto::v1::{CursorShape, OpenSession, ipc_event::Body as Ev};
use nexus_proto::wire::cursor::CursorUpdate;
use nexus_proto::wire::input::{InputEvent, InputPacket, MAX_EVENTS, TimedInput, coalesce};
use nexus_proto::wire::video::{FIXED_LEN, VideoFrameHeader};
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// What to view.
#[derive(Debug, Clone)]
pub struct ViewerOptions {
    /// Where the local agent listens.
    pub ipc: IpcPaths,
    /// The paired device to connect to.
    pub device_id: Vec<u8>,
    /// Its name, for the title bar.
    pub device_name: String,
    /// Monitor to show.
    pub monitor: u32,
    /// Frame-rate cap (0 = target default).
    pub max_fps: u32,
    /// Starting bitrate (0 = target default).
    pub bitrate_bps: u32,
    /// Do not send input.
    pub view_only: bool,
}

/// Messages from the network thread to the window.
#[derive(Debug)]
pub enum UiEvent {
    /// The session is up.
    Opened {
        /// Remote picture size.
        width: u32,
        /// Remote picture size.
        height: u32,
        /// Encoder in use on the target.
        encoder: String,
        /// The monitor's native size: pointer positions are in these pixels even when the video
        /// has been scaled down by the quality ladder.
        native: (u32, u32),
    },
    /// A new picture to show.
    Picture(Box<Picture>),
    /// Cursor position update.
    Cursor(CursorUpdate),
    /// A new cursor bitmap.
    Shape(CursorShape),
    /// One-line statistics for the title bar.
    Stats(String),
    /// The session ended (or could not start). The window should close.
    Ended(String),
}

/// What the window tells the network thread.
#[derive(Debug)]
pub enum Command {
    /// Send this input.
    Input(InputEvent),
    /// Close the session.
    Quit,
}

/// Channels for the window.
pub struct SessionHandle {
    /// Commands to the network thread.
    pub commands: mpsc::UnboundedSender<Command>,
}

/// Starts the network thread. `notify` is called (from that thread) for each [`UiEvent`].
pub fn spawn(options: ViewerOptions, notify: impl Fn(UiEvent) + Send + 'static) -> SessionHandle {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name("viewer-net".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(r) => r,
                Err(e) => {
                    notify(UiEvent::Ended(format!(
                        "cannot start the viewer runtime: {e}"
                    )));
                    return;
                }
            };
            let reason = runtime.block_on(run(options, rx, &notify));
            notify(UiEvent::Ended(reason));
        })
        .ok();
    SessionHandle { commands: tx }
}

/// Splits an IPC video blob into header and payload.
#[must_use]
pub fn parse_frame(blob: &[u8]) -> Option<VideoFrame> {
    let fixed: [u8; FIXED_LEN] = blob.get(..FIXED_LEN)?.try_into().ok()?;
    let head_len = FIXED_LEN + VideoFrameHeader::extra_len(&fixed).ok()?;
    let header = VideoFrameHeader::decode(blob.get(..head_len)?).ok()?;
    let payload = blob.get(head_len..)?;
    (payload.len() == header.payload_len as usize).then(|| VideoFrame {
        header,
        payload: bytes::Bytes::copy_from_slice(payload),
        recv_us: now_us(),
    })
}

async fn run(
    options: ViewerOptions,
    mut commands: mpsc::UnboundedReceiver<Command>,
    notify: &(impl Fn(UiEvent) + Send),
) -> String {
    let (client, mut events, _welcome) =
        match IpcClient::connect(&options.ipc, "nexus-viewer").await {
            Ok(c) => c,
            Err(e) => return format!("cannot reach the Nexus agent: {e}"),
        };
    let opened = match client
        .open_session(OpenSession {
            device_id: options.device_id.clone().into(),
            monitor: options.monitor,
            max_fps: options.max_fps,
            bitrate_bps: options.bitrate_bps,
            want_video: true,
        })
        .await
    {
        Ok(o) => o,
        Err(e) => return format!("could not connect to {}: {e}", options.device_name),
    };
    let id = opened.session_id;
    let path = format!("{:?}", opened.path());
    if let Some(v) = &opened.video {
        let native = opened
            .monitors
            .iter()
            .find(|m| m.index == options.monitor)
            .map_or((v.width, v.height), |m| (m.width, m.height));
        notify(UiEvent::Opened {
            width: v.width,
            height: v.height,
            encoder: v.encoder.clone(),
            native,
        });
    }

    let mut decoder = VideoDecoder::new();
    let mut hud = HudCounter::default();
    let mut stats_tick = tokio::time::interval(Duration::from_millis(500));
    let mut last_latency_us = 0u64;
    let mut input_buf: Vec<InputEvent> = Vec::new();
    let reason = loop {
        tokio::select! {
            ev = events.next() => {
                let Some(ev) = ev else { break "the agent closed the connection".to_owned() };
                match ev.body {
                    Some(Ev::SessionVideo(v)) if v.session_id == id => {
                        let arrived = Instant::now();
                        let Some(frame) = parse_frame(&v.frame) else { continue };
                        hud.frame(frame.payload.len(), arrived);
                        match decoder.decode(&frame) {
                            Ok(Some(pic)) => {
                                // capture → agent (offset-corrected) + agent → decoded here.
                                last_latency_us = v.upstream_us + u64::try_from(arrived.elapsed().as_micros()).unwrap_or(0);
                                notify(UiEvent::Picture(Box::new(pic)));
                            }
                            Ok(None) => {}
                            Err(e) => {
                                debug!(error = %e, "decode failed; asking for a keyframe");
                                let _ = client.session_keyframe(id).await;
                            }
                        }
                    }
                    Some(Ev::SessionCursorShape(s)) if s.session_id == id => {
                        if let Some(shape) = s.shape { notify(UiEvent::Shape(shape)); }
                    }
                    Some(Ev::SessionCursorPos(c)) if c.session_id == id => {
                        if let Ok(u) = CursorUpdate::decode(&c.update) { notify(UiEvent::Cursor(u)); }
                    }
                    Some(Ev::SessionEnded(e)) if e.session_id == id => break e.reason,
                    _ => {}
                }
            }
            cmd = commands.recv() => {
                let Some(cmd) = cmd else { break "closed".to_owned() };
                match cmd {
                    Command::Quit => break "closed".to_owned(),
                    Command::Input(first) => {
                        if options.view_only { continue; }
                        input_buf.push(first);
                        while input_buf.len() < MAX_EVENTS {
                            match commands.try_recv() {
                                Ok(Command::Input(e)) => input_buf.push(e),
                                Ok(Command::Quit) => { let _ = client.close_session(id).await; return "closed".to_owned(); }
                                Err(_) => break,
                            }
                        }
                        let events = std::mem::take(&mut input_buf);
                        let timed: Vec<TimedInput> = coalesce(events.into_iter().map(|event| TimedInput { dt_us: 0, event }).collect());
                        let packet = InputPacket { seq: 0, base_us: now_us(), events: timed, skipped: 0 };
                        let mut buf = bytes::BytesMut::new();
                        if packet.encode(&mut buf).is_ok() && let Err(e) = client.session_input(id, buf.to_vec()).await {
                            warn!(error = %e, "sending input failed");
                        }
                    }
                }
            }
            _ = stats_tick.tick() => {
                let h = hud.hud();
                notify(UiEvent::Stats(format!(
                    "{} — {:.0} fps · {:.1} Mbit/s · {:.0} ms · {path}",
                    options.device_name, h.fps, h.mbps, last_latency_us as f64 / 1000.0
                )));
            }
        }
    };
    let _ = client.close_session(id).await;
    reason
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_blobs_are_rejected() {
        assert!(parse_frame(&[]).is_none());
        assert!(parse_frame(&[0u8; FIXED_LEN]).is_none());
        assert!(parse_frame(&[1u8; 100]).is_none());
    }
}
