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
    /// Keep the two clipboards in sync.
    pub clipboard: bool,
    /// Swap ⌘ and Ctrl between a Mac and a PC so shortcuts keep working.
    pub key_remap: bool,
    /// Play the target's sound on this machine.
    pub audio: bool,
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
        /// How many monitors the device has.
        monitors: u32,
    },
    /// The device switched to streaming another monitor.
    MonitorChanged {
        /// Monitor index.
        monitor: u32,
        /// The monitor's native size.
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
    /// The link was lost; the viewer will try again in `in_secs` seconds. Keep the last picture.
    Reconnecting {
        /// Attempt number, starting at 1.
        attempt: u32,
        /// Why the session ended.
        reason: String,
        /// Seconds until the next attempt.
        in_secs: u64,
    },
    /// The session ended (or could not start). The window should close.
    Ended(String),
}

/// What the window tells the network thread.
#[derive(Debug)]
pub enum Command {
    /// Send this input.
    Input(InputEvent),
    /// Show another monitor.
    SwitchMonitor(u32),
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

/// How long a viewer keeps trying to get a lost session back before giving up.
const RESUME_WINDOW: Duration = Duration::from_secs(300);
/// Longest wait between reconnection attempts.
const MAX_RETRY_DELAY: Duration = Duration::from_secs(10);

/// How one attempt at a session ended.
enum Outcome {
    /// Finished for good (the user left, the target said goodbye, access was refused).
    Done(String),
    /// The link died or the target could not be reached yet: try again. `established` is true if
    /// the session had actually come up first (so the loss starts a fresh streak).
    Retry { reason: String, established: bool },
}

/// Delay before reconnection attempt number `attempt` (1-based): 1 s, 2 s, 4 s, 8 s, then 10 s.
fn retry_delay(attempt: u32) -> Duration {
    Duration::from_secs(1u64 << attempt.saturating_sub(1).min(3)).min(MAX_RETRY_DELAY)
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
    let mut first_loss: Option<Instant> = None;
    let mut attempt = 0u32;
    loop {
        match run_session(
            &options,
            &client,
            &mut events,
            &mut commands,
            notify,
            attempt > 0,
        )
        .await
        {
            Outcome::Done(reason) => return reason,
            Outcome::Retry {
                reason,
                established,
            } => {
                // A session that had been up resets the clock; only a streak of failures counts
                // against the window.
                if established {
                    first_loss = None;
                    attempt = 0;
                }
                let since = *first_loss.get_or_insert_with(Instant::now);
                if since.elapsed() > RESUME_WINDOW {
                    return format!("could not reconnect to {}: {reason}", options.device_name);
                }
                attempt += 1;
                let delay = retry_delay(attempt);
                notify(UiEvent::Reconnecting {
                    attempt,
                    reason: reason.clone(),
                    in_secs: delay.as_secs(),
                });
                // Wait, staying responsive to the user closing the window.
                let deadline = tokio::time::Instant::now() + delay;
                loop {
                    tokio::select! {
                        () = tokio::time::sleep_until(deadline) => break,
                        cmd = commands.recv() => match cmd {
                            None | Some(Command::Quit) => return "closed".to_owned(),
                            Some(_) => {} // input while disconnected goes nowhere
                        },
                        // Keep the agent's event queue drained while we wait.
                        ev = events.next() => if ev.is_none() {
                            return "the agent closed the connection".to_owned();
                        },
                    }
                }
            }
        }
    }
}

/// Opens one session and streams it until it ends.
async fn run_session(
    options: &ViewerOptions,
    client: &IpcClient,
    events: &mut nexus_ipc::EventReceiver,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    notify: &(impl Fn(UiEvent) + Send),
    resuming: bool,
) -> Outcome {
    let opened = match client
        .open_session(OpenSession {
            device_id: options.device_id.clone().into(),
            monitor: options.monitor,
            max_fps: options.max_fps,
            bitrate_bps: options.bitrate_bps,
            want_video: true,
            clipboard: options.clipboard,
            audio: options.audio,
            key_remap: if options.key_remap {
                "auto".into()
            } else {
                String::new()
            },
        })
        .await
    {
        Ok(o) => o,
        Err(e) => {
            let text = format!("could not connect to {}: {e}", options.device_name);
            // Refusals are final; everything else (unreachable, timeout, network change) is
            // worth another try once we are already resuming.
            return match &e {
                nexus_ipc::IpcError::Agent {
                    code:
                        nexus_proto::v1::IpcErrorCode::PermissionDenied
                        | nexus_proto::v1::IpcErrorCode::NotFound
                        | nexus_proto::v1::IpcErrorCode::InvalidRequest,
                    ..
                } => Outcome::Done(text),
                nexus_ipc::IpcError::Unreachable(_) | nexus_ipc::IpcError::Closed => {
                    Outcome::Done(text)
                }
                _ if resuming => Outcome::Retry {
                    reason: text,
                    established: false,
                },
                _ => Outcome::Done(text),
            };
        }
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
            monitors: u32::try_from(opened.monitors.len()).unwrap_or(1),
        });
    }

    let mut decoder = VideoDecoder::new();
    let mut hud = HudCounter::default();
    let mut stats_tick = tokio::time::interval(Duration::from_millis(500));
    let mut last_latency_us = 0u64;
    let mut input_buf: Vec<InputEvent> = Vec::new();
    let outcome = loop {
        tokio::select! {
            ev = events.next() => {
                let Some(ev) = ev else { break Outcome::Done("the agent closed the connection".to_owned()) };
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
                    Some(Ev::SessionMonitorChanged(m)) if m.session_id == id => {
                        notify(UiEvent::MonitorChanged { monitor: m.monitor, native: (m.native_width, m.native_height) });
                    }
                    Some(Ev::SessionEnded(e)) if e.session_id == id => {
                        break if e.connection_lost { Outcome::Retry { reason: e.reason, established: true } } else { Outcome::Done(e.reason) };
                    }
                    _ => {}
                }
            }
            cmd = commands.recv() => {
                let Some(cmd) = cmd else { break Outcome::Done("closed".to_owned()) };
                match cmd {
                    Command::Quit => break Outcome::Done("closed".to_owned()),
                    Command::SwitchMonitor(n) => {
                        if let Err(e) = client.session_monitor(id, n).await {
                            warn!(error = %e, "switching monitor failed");
                        }
                    }
                    Command::Input(first) => {
                        if options.view_only { continue; }
                        input_buf.push(first);
                        while input_buf.len() < MAX_EVENTS {
                            match commands.try_recv() {
                                Ok(Command::Input(e)) => input_buf.push(e),
                                Ok(Command::SwitchMonitor(n)) => { let _ = client.session_monitor(id, n).await; }
                                Ok(Command::Quit) => { let _ = client.close_session(id).await; return Outcome::Done("closed".to_owned()); }
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
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delays_grow_then_cap() {
        let secs: Vec<u64> = (1..=7).map(|a| retry_delay(a).as_secs()).collect();
        assert_eq!(secs, [1, 2, 4, 8, 8, 8, 8]);
        assert!(retry_delay(1000) <= MAX_RETRY_DELAY);
    }

    #[test]
    fn malformed_blobs_are_rejected() {
        assert!(parse_frame(&[]).is_none());
        assert!(parse_frame(&[0u8; FIXED_LEN]).is_none());
        assert!(parse_frame(&[1u8; 100]).is_none());
    }

    /// A fake agent speaking the real IPC protocol: the first session dies of a lost link, the
    /// viewer must announce a retry and reconnect on its own; the second ends on purpose.
    #[tokio::test]
    async fn a_lost_link_is_retried_and_a_deliberate_end_is_not() {
        use std::sync::Mutex;

        use nexus_ipc::{IpcPaths, IpcServer};
        use nexus_proto::v1::{
            IpcEvent, IpcFrame, IpcResponse, IpcWelcome, SessionEnded, SessionOpened, VideoStarted,
            ipc_frame::Body as F, ipc_request::Body as Req, ipc_response::Body as Resp,
        };

        let dir = tempfile::tempdir().unwrap();
        let paths = IpcPaths::in_dir(dir.path());
        let mut server = IpcServer::bind(paths.clone()).await.unwrap();
        let opens = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let agent = {
            let opens = std::sync::Arc::clone(&opens);
            tokio::spawn(async move {
                let pending = server.accept().await.unwrap();
                let (conn, _name) = pending.authenticate(IpcWelcome::default()).await.unwrap();
                let (mut reader, mut writer) = conn.split();
                while let Ok(Some(IpcFrame {
                    body: Some(F::Request(req)),
                })) = reader.recv().await
                {
                    match req.body {
                        Some(Req::OpenSession(_)) => {
                            let n = opens.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                            let opened = SessionOpened {
                                session_id: u64::from(n),
                                video: Some(VideoStarted {
                                    width: 640,
                                    height: 480,
                                    ..Default::default()
                                }),
                                ..Default::default()
                            };
                            let reply = IpcResponse {
                                in_reply_to: req.id,
                                body: Some(Resp::SessionOpened(opened)),
                            };
                            writer
                                .send(&IpcFrame {
                                    body: Some(F::Response(reply)),
                                })
                                .await
                                .unwrap();
                            // Session 1 loses its link; session 2 is ended by the target on purpose.
                            let ended = SessionEnded {
                                session_id: u64::from(n),
                                reason: format!("session {n} over"),
                                connection_lost: n == 1,
                            };
                            let ev = IpcEvent {
                                body: Some(nexus_proto::v1::ipc_event::Body::SessionEnded(ended)),
                            };
                            writer
                                .send(&IpcFrame {
                                    body: Some(F::Event(ev)),
                                })
                                .await
                                .unwrap();
                        }
                        _ => {
                            let reply = IpcResponse {
                                in_reply_to: req.id,
                                body: Some(Resp::Ack(nexus_proto::v1::IpcAck {})),
                            };
                            let _ = writer
                                .send(&IpcFrame {
                                    body: Some(F::Response(reply)),
                                })
                                .await;
                        }
                    }
                }
            })
        };

        let seen: std::sync::Arc<Mutex<Vec<String>>> = std::sync::Arc::default();
        let log = std::sync::Arc::clone(&seen);
        let notify = move |ev: UiEvent| {
            let line = match ev {
                UiEvent::Opened { width, .. } => format!("opened {width}"),
                UiEvent::Reconnecting {
                    attempt, in_secs, ..
                } => format!("reconnecting {attempt} in {in_secs}s"),
                UiEvent::Ended(r) => format!("ended: {r}"),
                _ => return,
            };
            log.lock().unwrap().push(line);
        };
        let (_tx, rx) = mpsc::unbounded_channel();
        let options = ViewerOptions {
            ipc: paths,
            device_id: vec![1; 16],
            device_name: "Server".into(),
            monitor: 0,
            max_fps: 0,
            bitrate_bps: 0,
            view_only: false,
            clipboard: false,
            key_remap: false,
            audio: false,
        };
        let reason = tokio::time::timeout(Duration::from_secs(20), run(options, rx, &notify))
            .await
            .expect("the viewer must finish");
        agent.abort();
        assert_eq!(reason, "session 2 over");
        assert_eq!(
            opens.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "exactly one reconnect"
        );
        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen,
            ["opened 640", "reconnecting 1 in 1s", "opened 640"],
            "announce the retry, then come back with the same picture"
        );
    }
}
