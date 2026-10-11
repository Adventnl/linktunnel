//! Controller-side sessions on behalf of local viewers (CLI, app).
//!
//! The agent owns the network session; the viewer owns decoding and presentation. This module
//! connects them: it opens the session, forwards *encoded* video, cursor and shapes to the
//! IPC connection that asked for them, and turns the viewer's input back into session input.
//! The agent never decodes what the remote machine sends.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use bytes::BytesMut;
use nexus_media::controller::{ControllerOptions, ControllerSession, MediaEvent, Streams};
use nexus_proto::v1::{
    IpcEvent, MonitorInfo, OpenSession, OsFamily, SessionCursorPos, SessionCursorShape,
    SessionEnded, SessionMonitor, SessionMonitorChanged, SessionOpened, SessionQuality,
    SessionVideo, VideoStarted, ipc_event::Body as Ev,
};
use nexus_proto::wire::cursor::CursorUpdate;
use nexus_proto::wire::input::InputPacket;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::error::AgentError;
use crate::sessions;
use crate::state::Shared;
use crate::sysinfo;

/// How long a target gets to start streaming.
const START_TIMEOUT: Duration = Duration::from_secs(15);
/// Largest input packet accepted from a viewer.
const MAX_INPUT_PACKET: usize = 128 * 1024;

/// One IPC connection, as far as session ownership is concerned.
pub(crate) struct ConnCtx {
    /// Where this connection's session events go.
    pub events: mpsc::Sender<IpcEvent>,
    /// Sessions this connection opened (closed when it disconnects).
    pub owned: Mutex<HashSet<u64>>,
}

impl ConnCtx {
    pub(crate) fn new(events: mpsc::Sender<IpcEvent>) -> Arc<Self> {
        Arc::new(Self {
            events,
            owned: Mutex::new(HashSet::new()),
        })
    }
}

/// A live viewer session.
pub(crate) struct ViewerEntry {
    controller: ControllerSession,
    owner: Arc<ConnCtx>,
    pump: tokio::task::JoinHandle<()>,
}

pub(crate) async fn open(
    shared: &Arc<Shared>,
    ctx: &Arc<ConnCtx>,
    req: OpenSession,
) -> Result<SessionOpened, AgentError> {
    if req.monitor > 255 {
        return Err(AgentError::Invalid("monitor index out of range".into()));
    }
    let peer = shared
        .find_peer(&req.device_id)
        .ok_or_else(|| AgentError::NotFound("no paired device with that id".into()))?;
    let id = shared.next_viewer_id.fetch_add(1, Ordering::Relaxed);
    let (session, established) = sessions::dial(shared, &req.device_id).await?;
    let path = session.path().kind;
    let key_remap = if req.key_remap == "auto" {
        let controller_os = sysinfo::os_info().family;
        let target_os = established.peer_hello.os.as_ref().map_or(0, |o| o.family);
        nexus_input::remap::KeyRemap::auto(
            OsFamily::try_from(controller_os).unwrap_or(OsFamily::Unspecified),
            OsFamily::try_from(target_os).unwrap_or(OsFamily::Unspecified),
        )
    } else {
        nexus_input::remap::KeyRemap::identity()
    };
    let options = ControllerOptions {
        key_remap,
        monitor: req.monitor,
        max_fps: req.max_fps,
        bitrate_bps: req.bitrate_bps,
        want_video: req.want_video,
        clipboard: if req.clipboard {
            shared.handler.clipboard()
        } else {
            None
        },
        ..Default::default()
    };
    let (controller, mut streams) = ControllerSession::start(session, established.control, options)
        .await
        .map_err(|e| AgentError::Failed(e.to_string()))?;

    let mut monitors: Vec<MonitorInfo> = Vec::new();
    let mut video: Option<VideoStarted> = None;
    if req.want_video {
        let deadline = tokio::time::Instant::now() + START_TIMEOUT;
        loop {
            let ev = match tokio::time::timeout_at(deadline, streams.events.recv()).await {
                Ok(Some(ev)) => ev,
                Ok(None) => {
                    return Err(AgentError::Failed(
                        "the session ended before video started".into(),
                    ));
                }
                Err(_) => {
                    controller.close().await;
                    return Err(AgentError::Failed(
                        "the device did not start streaming in time".into(),
                    ));
                }
            };
            match ev {
                MediaEvent::Monitors(m) => monitors = m,
                MediaEvent::Started(v) => {
                    video = Some(v);
                    break;
                }
                MediaEvent::Refused { code, message } => {
                    controller.close().await;
                    return Err(
                        if code == nexus_proto::v1::ErrorCode::PermissionDenied as i32 {
                            AgentError::Denied(format!("the device refused: {message}"))
                        } else {
                            AgentError::Failed(format!("the device refused: {message}"))
                        },
                    );
                }
                MediaEvent::Closed(reason) => {
                    return Err(AgentError::Failed(format!("session closed: {reason}")));
                }
                // A shape sent before the stream started: forward it now so it is not lost.
                MediaEvent::CursorShape(shape) => {
                    let _ = ctx.events.try_send(IpcEvent {
                        body: Some(Ev::SessionCursorShape(SessionCursorShape {
                            session_id: id,
                            shape: Some(shape),
                        })),
                    });
                }
                MediaEvent::Frame(_) => {}
            }
        }
    }

    let pump = tokio::spawn(pump_events(
        Arc::clone(shared),
        Arc::clone(ctx),
        id,
        controller.clone(),
        streams,
    ));
    shared.viewers.lock().insert(
        id,
        ViewerEntry {
            controller,
            owner: Arc::clone(ctx),
            pump,
        },
    );
    ctx.owned.lock().insert(id);
    info!(session = id, peer = %peer.name, "viewer session opened");
    Ok(SessionOpened {
        session_id: id,
        video,
        monitors,
        path: path as i32,
        peer_name: peer.name,
    })
}

async fn pump_events(
    shared: Arc<Shared>,
    ctx: Arc<ConnCtx>,
    id: u64,
    controller: ControllerSession,
    mut streams: Streams,
) {
    let stats = Arc::clone(controller.stats());
    let mut monitors: Vec<MonitorInfo> = Vec::new();
    let reason = loop {
        tokio::select! {
            ev = streams.events.recv() => {
                let Some(ev) = ev else { break "session ended".to_owned() };
                let out = match ev {
                    MediaEvent::Frame(f) => {
                        let mut buf = BytesMut::with_capacity(f.header.wire_len() + f.payload.len());
                        if f.header.encode(&mut buf).is_err() { continue; }
                        buf.extend_from_slice(&f.payload);
                        let offset = stats.clock_offset();
                        #[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
                        let upstream = (f.recv_us as i64 - (f.header.capture_us as i64 - offset)).max(0) as u64;
                        Ev::SessionVideo(SessionVideo { session_id: id, frame: buf.freeze(), upstream_us: upstream })
                    }
                    MediaEvent::CursorShape(shape) => Ev::SessionCursorShape(SessionCursorShape { session_id: id, shape: Some(shape) }),
                    MediaEvent::Closed(reason) => break reason,
                    MediaEvent::Refused { message, .. } => { debug!(%message, "target refused a request"); continue; }
                    MediaEvent::Monitors(list) => { monitors = list; continue }
                    MediaEvent::Started(v) => {
                        // Sent again after a monitor switch: the viewer needs the new geometry.
                        let native = monitors.iter().find(|m| m.index == v.monitor).map_or((v.width, v.height), |m| (m.width, m.height));
                        Ev::SessionMonitorChanged(SessionMonitorChanged {
                            session_id: id,
                            monitor: v.monitor,
                            width: v.width,
                            height: v.height,
                            native_width: native.0,
                            native_height: native.1,
                        })
                    }
                };
                // Blocking send is the back-pressure path: a slow viewer slows this task, the
                // controller's event queue fills, and it drops frames and asks for a keyframe.
                if ctx.events.send(IpcEvent { body: Some(out) }).await.is_err() {
                    break "viewer disconnected".to_owned();
                }
            }
            changed = streams.cursor.changed() => {
                if changed.is_err() { break "session ended".to_owned(); }
                let update: Option<CursorUpdate> = *streams.cursor.borrow_and_update();
                if let Some(u) = update {
                    let mut buf = BytesMut::with_capacity(nexus_proto::wire::cursor::LEN);
                    u.encode(&mut buf);
                    // Positions are disposable: if the viewer is behind, skip this one.
                    let _ = ctx.events.try_send(IpcEvent { body: Some(Ev::SessionCursorPos(SessionCursorPos { session_id: id, update: buf.freeze() })) });
                }
            }
        }
    };
    let _ = ctx
        .events
        .send(IpcEvent {
            body: Some(Ev::SessionEnded(SessionEnded {
                session_id: id,
                reason,
            })),
        })
        .await;
    ctx.owned.lock().remove(&id);
    shared.viewers.lock().remove(&id);
}

pub(crate) fn controller_for(
    shared: &Shared,
    ctx: &Arc<ConnCtx>,
    id: u64,
) -> Result<ControllerSession, AgentError> {
    let viewers = shared.viewers.lock();
    let entry = viewers
        .get(&id)
        .ok_or_else(|| AgentError::NotFound("no such session".into()))?;
    if !Arc::ptr_eq(&entry.owner, ctx) {
        return Err(AgentError::Denied(
            "that session belongs to another client".into(),
        ));
    }
    Ok(entry.controller.clone())
}

pub(crate) fn input(
    shared: &Shared,
    ctx: &Arc<ConnCtx>,
    id: u64,
    packet: &[u8],
) -> Result<(), AgentError> {
    if packet.len() > MAX_INPUT_PACKET {
        return Err(AgentError::Invalid("input packet too large".into()));
    }
    let controller = controller_for(shared, ctx, id)?;
    let packet = InputPacket::decode(packet)
        .map_err(|e| AgentError::Invalid(format!("input packet: {e}")))?;
    controller.send_input(packet.events.into_iter().map(|t| t.event));
    Ok(())
}

pub(crate) fn switch_monitor(
    shared: &Shared,
    ctx: &Arc<ConnCtx>,
    req: &SessionMonitor,
) -> Result<(), AgentError> {
    if req.monitor > 255 {
        return Err(AgentError::Invalid("monitor index out of range".into()));
    }
    controller_for(shared, ctx, req.session_id)?.switch_monitor(req.monitor);
    Ok(())
}

pub(crate) fn keyframe(shared: &Shared, ctx: &Arc<ConnCtx>, id: u64) -> Result<(), AgentError> {
    controller_for(shared, ctx, id)?.request_keyframe();
    Ok(())
}

pub(crate) fn quality(
    shared: &Shared,
    ctx: &Arc<ConnCtx>,
    q: &SessionQuality,
) -> Result<(), AgentError> {
    controller_for(shared, ctx, q.session_id)?.set_quality(q.bitrate_bps, q.fps, q.scale_percent);
    Ok(())
}

pub(crate) async fn close(shared: &Shared, ctx: &Arc<ConnCtx>, id: u64) -> Result<(), AgentError> {
    let controller = controller_for(shared, ctx, id)?;
    controller.close().await;
    Ok(())
}

/// Ends every session an IPC connection opened (it disconnected).
pub(crate) async fn close_all(shared: &Shared, ctx: &Arc<ConnCtx>) {
    let ids: Vec<u64> = ctx.owned.lock().drain().collect();
    for id in ids {
        let entry = shared.viewers.lock().remove(&id);
        if let Some(entry) = entry {
            entry.pump.abort();
            entry.controller.close().await;
        }
    }
}
