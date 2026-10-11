//! Serves the local IPC: authentication, request dispatch, event fan-out.

use std::sync::Arc;
use std::time::Duration;

use nexus_core::telemetry::LatencySummary;
use nexus_ipc::{IpcServer, PendingConnection};
use nexus_net::diag;
use nexus_proto::v1::{
    DeviceList, IpcAck, IpcFrame, IpcRequest, IpcResponse, IpcWelcome, LatencyView, PairingStarted,
    PingReport, ipc_event::Body as Ev, ipc_frame::Body as F, ipc_request::Body as Req,
    ipc_response::Body as Resp,
};
use nexus_proto::version::PROTOCOL_VERSION;
use nexus_trust::revocation;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn};

use crate::consent;
use crate::error::AgentError;
use crate::files_bridge;
use crate::pairing;
use crate::power_bridge;
use crate::sessions;
use crate::state::Shared;
use crate::terminal_bridge;
use crate::viewer_bridge::{self, ConnCtx};

/// Limits on what a client may ask the diagnostics to do.
const MAX_PINGS: u32 = 10_000;
const MAX_BULK_BYTES: u64 = 1 << 30;

pub(crate) async fn serve(shared: Arc<Shared>, mut server: IpcServer) {
    let mut shutdown = shared.shutdown.subscribe();
    loop {
        let pending = tokio::select! {
            accepted = server.accept() => match accepted {
                Ok(p) => p,
                Err(e) => {
                    warn!(error = %e, "accepting an IPC connection failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
            _ = shutdown.changed() => return,
        };
        let shared = Arc::clone(&shared);
        tokio::spawn(async move { client(shared, pending).await });
    }
}

async fn client(shared: Arc<Shared>, pending: PendingConnection) {
    let welcome = IpcWelcome {
        agent_version: env!("CARGO_PKG_VERSION").to_owned(),
        device_id: shared.identity.device_id().as_bytes().to_vec().into(),
        protocol: Some(PROTOCOL_VERSION),
    };
    let (conn, name) = match pending.authenticate(welcome).await {
        Ok(ok) => ok,
        Err(e) => {
            debug!(error = %e, "IPC client failed to authenticate");
            return;
        }
    };
    debug!(client = %name, "IPC client connected");
    let (mut reader, mut writer) = conn.split();
    let mut events = shared.events.subscribe();
    let (responses_tx, mut responses_rx) = mpsc::channel::<IpcResponse>(64);
    // Session traffic (video, cursor) for sessions this client opened.
    let (session_tx, mut session_rx) = mpsc::channel::<nexus_proto::v1::IpcEvent>(32);
    let ctx = ConnCtx::new(session_tx);
    let mut shutdown = shared.shutdown.subscribe();

    loop {
        tokio::select! {
            frame = reader.recv() => match frame {
                Ok(Some(IpcFrame { body: Some(F::Request(req)) })) => {
                    let (shared, tx, ctx) = (Arc::clone(&shared), responses_tx.clone(), Arc::clone(&ctx));
                    // Requests run concurrently: a long diagnostics run must not block others.
                    tokio::spawn(async move { let _ = tx.send(handle(&shared, &ctx, req).await).await; });
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break,
            },
            Some(ev) = session_rx.recv() => {
                if writer.send(&IpcFrame { body: Some(F::Event(ev)) }).await.is_err() { break; }
            }
            Some(response) = responses_rx.recv() => {
                if writer.send(&IpcFrame { body: Some(F::Response(response)) }).await.is_err() { break; }
            }
            event = events.recv() => match event {
                Ok(event) => {
                    if writer.send(&IpcFrame { body: Some(F::Event(event)) }).await.is_err() { break; }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // The client missed events: resynchronise it with the full device list.
                    for device in shared.device_views() {
                        let ev = nexus_proto::v1::IpcEvent {
                            body: Some(Ev::DeviceChanged(nexus_proto::v1::DeviceChanged { device: Some(device) })),
                        };
                        if writer.send(&IpcFrame { body: Some(F::Event(ev)) }).await.is_err() { return; }
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _ = shutdown.changed() => {
                // Flush responses that are already queued (notably the Ack for `Shutdown`
                // itself) before the connection goes away.
                while let Ok(response) = responses_rx.try_recv() {
                    let _ = writer.send(&IpcFrame { body: Some(F::Response(response)) }).await;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
                while let Ok(response) = responses_rx.try_recv() {
                    let _ = writer.send(&IpcFrame { body: Some(F::Response(response)) }).await;
                }
                break;
            }
        }
    }
    terminal_bridge::close_all(&shared, &ctx);
    viewer_bridge::close_all(&shared, &ctx).await;
    debug!(client = %name, "IPC client disconnected");
}

async fn handle(shared: &Arc<Shared>, ctx: &Arc<ConnCtx>, request: IpcRequest) -> IpcResponse {
    let id = request.id;
    let body = match dispatch(shared, ctx, request.body).await {
        Ok(body) => body,
        Err(e) => Resp::Error(e.to_ipc()),
    };
    IpcResponse {
        in_reply_to: id,
        body: Some(body),
    }
}

async fn dispatch(
    shared: &Arc<Shared>,
    ctx: &Arc<ConnCtx>,
    body: Option<Req>,
) -> Result<Resp, AgentError> {
    let ack = Resp::Ack(IpcAck {});
    match body.ok_or_else(|| AgentError::Invalid("empty request".into()))? {
        Req::Status(_) => Ok(Resp::Status(shared.status())),
        Req::ListDevices(_) => Ok(Resp::Devices(DeviceList {
            devices: shared.device_views(),
        })),
        Req::PairingOffer(o) => {
            let (pairing_id, code) = pairing::offer(shared, o.allow_peer_to).await?;
            Ok(Resp::PairingStarted(PairingStarted { pairing_id, code }))
        }
        Req::PairingJoin(j) => {
            let pairing_id =
                pairing::join(shared, &j.code, j.request_on_peer, j.grant_to_peer).await?;
            Ok(Resp::PairingStarted(PairingStarted {
                pairing_id,
                code: String::new(),
            }))
        }
        Req::PairingConfirm(c) => {
            pairing::confirm(shared, c.pairing_id, c.words_match).await?;
            Ok(ack)
        }
        Req::PairingCancel(c) => {
            pairing::cancel(shared, c.pairing_id)?;
            Ok(ack)
        }
        Req::Unpair(u) => {
            unpair(shared, &u.device_id).await?;
            Ok(ack)
        }
        Req::Ping(p) => ping(shared, p).await.map(Resp::PingReport),
        Req::SetLockdown(l) => {
            set_lockdown(shared, l.enabled)?;
            Ok(ack)
        }
        Req::OpenSession(o) => viewer_bridge::open(shared, ctx, o)
            .await
            .map(Resp::SessionOpened),
        Req::CloseSession(c) => {
            viewer_bridge::close(shared, ctx, c.session_id).await?;
            Ok(ack)
        }
        Req::SessionInput(i) => {
            viewer_bridge::input(shared, ctx, i.session_id, &i.packet)?;
            Ok(ack)
        }
        Req::SessionKeyframe(k) => {
            viewer_bridge::keyframe(shared, ctx, k.session_id)?;
            Ok(ack)
        }
        Req::SessionMonitor(m) => {
            viewer_bridge::switch_monitor(shared, ctx, &m)?;
            Ok(ack)
        }
        Req::SessionQuality(q) => {
            viewer_bridge::quality(shared, ctx, &q)?;
            Ok(ack)
        }
        Req::FileList(r) => files_bridge::list(shared, ctx, r)
            .await
            .map(Resp::FileListing),
        Req::FileStat(r) => files_bridge::stat(shared, ctx, r)
            .await
            .map(Resp::FileEntry),
        Req::FileMkdir(r) => {
            files_bridge::mkdir(shared, ctx, r).await?;
            Ok(ack)
        }
        Req::FileRemove(r) => {
            files_bridge::remove(shared, ctx, r).await?;
            Ok(ack)
        }
        Req::FileTransfer(r) => files_bridge::transfer(shared, ctx, r).map(Resp::TransferStarted),
        Req::ConsentDecision(d) => {
            consent::decide(shared, d)?;
            Ok(ack)
        }
        Req::SetUnattended(s) => {
            consent::set_unattended(shared, &s.device_id, s.unattended)?;
            Ok(ack)
        }
        Req::PowerCommand(c) => {
            power_bridge::power(shared, ctx, c).await?;
            Ok(ack)
        }
        Req::WakeDevice(w) => {
            power_bridge::wake(shared, &w.device_id, &w.via_device_id).await?;
            Ok(ack)
        }
        Req::OpenTerminal(t) => terminal_bridge::open(shared, ctx, t)
            .await
            .map(Resp::TerminalStarted),
        Req::TerminalInput(i) => {
            terminal_bridge::input(shared, ctx, i)?;
            Ok(ack)
        }
        Req::TerminalResize(r) => {
            terminal_bridge::resize(shared, ctx, &r)?;
            Ok(ack)
        }
        Req::CloseTerminal(c) => {
            terminal_bridge::close(shared, ctx, &c)?;
            Ok(ack)
        }
        Req::Shutdown(_) => {
            info!("shutdown requested over IPC");
            let _ = shared.shutdown.send(true);
            Ok(ack)
        }
    }
}

async fn unpair(shared: &Arc<Shared>, device_id: &[u8]) -> Result<(), AgentError> {
    let peer = shared
        .find_peer(device_id)
        .ok_or_else(|| AgentError::NotFound("no paired device with that id".into()))?;
    let key = peer.identity.ed25519_bytes();
    let now = Shared::now_ms();
    // Local effects first: they must hold even if the server is unreachable.
    shared.trust.lock().revoke(&key, now)?;
    shared.node.allow_list().revoke(&key); // closes live sessions with that device
    shared.devices.lock().remove(&key);
    shared.policy.lock().forget(&key);
    shared.wakebook.lock().forget(&key);
    shared.emit(Ev::DeviceRemoved(nexus_proto::v1::DeviceRemoved {
        device_id: device_id.to_vec().into(),
    }));
    // Then tell the server so the other device learns of it (re-sent on reconnect if this fails).
    if let Some(signal) = shared.signal() {
        let signed = revocation::revoke_key(&shared.identity, &key, now);
        if let Err(e) = signal.revoke_pairing(signed).await
            && e.server_code() != Some(nexus_proto::v1::SignalErrorCode::NotFound)
        {
            warn!(error = %e, "could not notify the server of the revocation yet; will retry on reconnect");
        }
    }
    info!(peer = %peer.identity.device_id(), "unpaired");
    Ok(())
}

fn set_lockdown(shared: &Shared, enabled: bool) -> Result<(), AgentError> {
    use std::sync::atomic::Ordering;
    let marker = shared.cfg.lockdown_file();
    if enabled {
        if let Some(dir) = marker.parent() {
            std::fs::create_dir_all(dir).map_err(|e| AgentError::Failed(e.to_string()))?;
        }
        std::fs::write(&marker, b"on\n").map_err(|e| AgentError::Failed(e.to_string()))?;
    } else {
        match std::fs::remove_file(&marker) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(AgentError::Failed(e.to_string())),
        }
    }
    shared.lockdown.store(enabled, Ordering::SeqCst);
    // Lockdown empties the allow-list, which terminates every live session at once.
    shared.refresh_allowlist();
    info!(enabled, "lockdown changed");
    Ok(())
}

fn latency(s: &LatencySummary) -> LatencyView {
    LatencyView {
        count: s.count,
        p50_us: s.p50_us,
        p95_us: s.p95_us,
        p99_us: s.p99_us,
        max_us: s.max_us,
    }
}

async fn ping(
    shared: &Arc<Shared>,
    request: nexus_proto::v1::PingDevice,
) -> Result<PingReport, AgentError> {
    let defaults = diag::Options::default();
    let opts = diag::Options {
        pings: if request.pings == 0 {
            defaults.pings
        } else {
            request.pings.min(MAX_PINGS) as usize
        },
        stream_echoes: if request.stream_echoes == 0 {
            defaults.stream_echoes
        } else {
            request.stream_echoes.min(MAX_PINGS) as usize
        },
        datagrams: if request.datagrams == 0 {
            defaults.datagrams
        } else {
            request.datagrams.min(MAX_PINGS) as usize
        },
        bulk_bytes: if request.bulk_bytes == 0 {
            defaults.bulk_bytes
        } else {
            usize::try_from(request.bulk_bytes.min(MAX_BULK_BYTES)).unwrap_or(defaults.bulk_bytes)
        },
    };
    let (session, mut established) = sessions::dial(shared, &request.device_id).await?;
    let report = diag::run(&session, &mut established.control, &opts)
        .await
        .map_err(|e| AgentError::Failed(format!("diagnostics failed: {e}")))?;
    let rtt = session.path().rtt;
    session.close_normal();
    Ok(PingReport {
        control: Some(latency(&report.control)),
        stream: Some(latency(&report.stream)),
        datagram: Some(latency(&report.datagram)),
        datagram_loss: report.datagram_loss,
        throughput_mbps: report.throughput_mbps,
        path_before: report.path_before.kind as i32,
        path_after: report.path_after.kind as i32,
        remote: report.path_after.remote,
        rtt_us: u64::try_from(rtt.as_micros()).unwrap_or(u64::MAX),
    })
}
