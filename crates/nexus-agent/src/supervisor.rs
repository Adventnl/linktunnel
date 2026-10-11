//! Keeps the agent connected to the signaling server and reacts to what it pushes.

use std::sync::Arc;
use std::time::Duration;

use nexus_crypto::noise::envelope;
use nexus_crypto::noise::replay::ReplayGuard;
use nexus_proto::v1::{GoingOfflineReason, PresenceInfo, PresenceState};
use nexus_signal_client::{ClientConfig, ClientError, ServerEvent, SignalClient};
use nexus_trust::revocation;
use tracing::{debug, info, warn};

use crate::state::{RendezvousMsg, Shared};
use crate::sysinfo;

/// Exponential backoff with full jitter.
struct Backoff {
    base: Duration,
    cap: Duration,
    attempt: u32,
}

impl Backoff {
    fn new(cap: Duration) -> Self {
        Self {
            base: Duration::from_millis(500),
            cap,
            attempt: 0,
        }
    }

    fn reset(&mut self) {
        self.attempt = 0;
    }

    fn next(&mut self) -> Duration {
        let ceiling = self
            .base
            .saturating_mul(1u32.checked_shl(self.attempt.min(16)).unwrap_or(u32::MAX))
            .min(self.cap);
        self.attempt = self.attempt.saturating_add(1);
        let mut r = [0u8; 4];
        let _ = getrandom::fill(&mut r);
        let frac = f64::from(u32::from_le_bytes(r)) / f64::from(u32::MAX);
        // Never below 100 ms so a tight failure loop cannot spin.
        Duration::from_secs_f64((ceiling.as_secs_f64() * frac).max(0.1))
    }
}

/// Runs for the life of the agent: connect, sync, serve events, reconnect on failure.
pub(crate) async fn run(shared: Arc<Shared>) {
    let Some(url) = shared.cfg.signal_url.clone() else {
        info!("no signaling server configured: running without presence or pairing registry");
        return;
    };
    let mut shutdown = shared.shutdown.subscribe();
    let mut backoff = Backoff::new(shared.cfg.reconnect_max);
    loop {
        if *shutdown.borrow() {
            return;
        }
        let mut config = ClientConfig::new(url.clone());
        config.os = sysinfo::os_info();
        let outcome = tokio::select! {
            r = session(&shared, &config) => r,
            _ = shutdown.changed() => return,
        };
        shared.signal.lock().take();
        shared.devices.lock().mark_all_unknown();
        shared.emit(nexus_proto::v1::ipc_event::Body::SignalState(
            nexus_proto::v1::SignalState { connected: false },
        ));
        match outcome {
            Ok(Ended::Shutdown) => return,
            Ok(Ended::Dropped) => backoff.reset(),
            Ok(Ended::Superseded) => {
                warn!(
                    "another process is using this device identity on the signaling server; standing down"
                );
                return;
            }
            Err(e) => warn!(error = %e, "signaling connection failed"),
        }
        let wait = backoff.next();
        debug!(?wait, "reconnecting to signaling");
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            _ = shutdown.changed() => return,
        }
    }
}

enum Ended {
    Dropped,
    Superseded,
    Shutdown,
}

/// What the presence record says about the machine's activity.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Activity {
    state: PresenceState,
    battery_percent: u32,
    on_ac: bool,
    user: String,
}

fn activity(shared: &Shared) -> Activity {
    let snap = match &shared.cfg.presence_sensor {
        Some(s) => s.sample(),
        None => nexus_presence::Sensor::sample(&nexus_presence::SystemSensor),
    };
    let remote = shared.sessions.load(std::sync::atomic::Ordering::SeqCst) > 0;
    Activity {
        state: nexus_presence::classify(&snap, remote, nexus_presence::Thresholds::default()),
        battery_percent: snap.battery_percent.map_or(255, u32::from),
        on_ac: snap.on_ac,
        user: snap.user.unwrap_or_default(),
    }
}

fn presence_info(shared: &Shared, seq: u64) -> PresenceInfo {
    let act = activity(shared);
    let reach = shared.node.reachability();
    let hints: Vec<String> = reach
        .direct
        .iter()
        .filter(|a| shared.cfg.publish_loopback_hints || !a.ip().is_loopback())
        .take(8)
        .map(ToString::to_string)
        .collect();
    PresenceInfo {
        state: act.state as i32,
        name: shared.cfg.device_name.clone(),
        os: Some(sysinfo::os_info()),
        active_user: act.user,
        battery_percent: act.battery_percent,
        on_ac_power: act.on_ac,
        uptime_secs: shared.started.elapsed().as_secs(),
        ip_hints: hints,
        relay_url: reach.relay.unwrap_or_default(),
        agent_version: env!("CARGO_PKG_VERSION").to_owned(),
        capabilities: Some(shared.handler.capabilities()),
        seq,
        ..PresenceInfo::default()
    }
}

async fn session(shared: &Arc<Shared>, config: &ClientConfig) -> Result<Ended, ClientError> {
    let (client, mut events) =
        nexus_signal_client::connect(config, Arc::clone(&shared.identity)).await?;
    info!(server = %config.url, "connected to signaling");
    *shared.signal.lock() = Some(client.clone());
    shared.emit(nexus_proto::v1::ipc_event::Body::SignalState(
        nexus_proto::v1::SignalState { connected: true },
    ));

    sync_pairings(shared, &client).await?;
    let mut seq: u64 = 1;
    client.publish_presence(presence_info(shared, seq)).await?;
    for presence in client.subscribe_presence().await? {
        apply_presence(shared, &presence);
    }

    let mut guard = ReplayGuard::default();
    let mut shutdown = shared.shutdown.subscribe();
    let mut heartbeat = tokio::time::interval(
        client
            .params()
            .heartbeat_interval
            .max(Duration::from_secs(1)),
    );
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut refresh = tokio::time::interval(Duration::from_secs(60));
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    refresh.tick().await;
    // Activity changes (someone sits down, the screen locks) are published within seconds.
    let mut sense =
        tokio::time::interval(shared.cfg.presence_interval.max(Duration::from_millis(50)));
    sense.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    sense.tick().await;
    let mut last_activity = activity(shared);
    let mut last_sense = std::time::Instant::now();

    loop {
        tokio::select! {
            event = events.next() => match event {
                None | Some(ServerEvent::Disconnected(_)) => return Ok(Ended::Dropped),
                Some(ServerEvent::Superseded) => return Ok(Ended::Superseded),
                Some(event) => handle_event(shared, &client, &mut guard, event).await,
            },
            _ = heartbeat.tick() => match client.heartbeat(seq).await {
                Ok(()) => {}
                Err(e) if e.server_code() == Some(nexus_proto::v1::SignalErrorCode::NotFound) => {
                    seq += 1;
                    client.publish_presence(presence_info(shared, seq)).await?;
                }
                Err(ClientError::Closed) => return Ok(Ended::Dropped),
                Err(e) => debug!(error = %e, "heartbeat failed"),
            },
            _ = sense.tick() => {
                let now = activity(shared);
                // A long gap between ticks means the machine was asleep: tell everyone it is back.
                let woke = last_sense.elapsed() > shared.cfg.presence_interval * 4 + Duration::from_secs(5);
                last_sense = std::time::Instant::now();
                if now != last_activity || woke {
                    last_activity = now;
                    seq += 1;
                    client.publish_presence(presence_info(shared, seq)).await?;
                }
            }
            _ = refresh.tick() => {
                seq += 1;
                client.publish_presence(presence_info(shared, seq)).await?;
            }
            _ = shutdown.changed() => {
                let _ = client.going_offline(GoingOfflineReason::Shutdown).await;
                client.close().await;
                return Ok(Ended::Shutdown);
            }
        }
    }
}

pub(crate) fn apply_presence_pub(shared: &Shared, presence: &nexus_proto::v1::DevicePresence) {
    apply_presence(shared, presence);
}

fn apply_presence(shared: &Shared, presence: &nexus_proto::v1::DevicePresence) {
    let Some(id) = nexus_crypto::DeviceId::from_slice(&presence.device_id).ok() else {
        return;
    };
    let key = shared
        .trust
        .lock()
        .peers()
        .find(|p| p.identity.device_id() == id)
        .map(|p| p.identity.ed25519_bytes());
    if let Some(key) = key {
        shared.devices.lock().update_presence(key, presence);
        if let Some(os) = presence.info.as_ref().and_then(|i| i.os.as_ref()) {
            shared.wakebook.lock().learn(&key, &os.mac_addresses);
        }
        shared.emit_device_changed(&key);
    }
}

/// Reconciles local trust with the server after (re)connecting.
async fn sync_pairings(shared: &Arc<Shared>, client: &SignalClient) -> Result<(), ClientError> {
    // Pairings made while we were offline (e.g. by the other device).
    for signed in client.list_pairings().await? {
        let result = shared
            .trust
            .lock()
            .add(&signed)
            .map(|p| p.identity.ed25519_bytes());
        match result {
            Ok(key) => {
                shared.refresh_allowlist();
                shared.emit_device_changed(&key);
            }
            Err(e) => debug!(error = %e, "ignoring a server-listed pairing"),
        }
    }
    // Local pairings the server may have lost (e.g. it was restarted with in-memory stores), and
    // revocations that never reached it because we were offline when we made them.
    let (pairings, revoked) = {
        let trust = shared.trust.lock();
        (
            trust.peers().map(|p| p.signed.clone()).collect::<Vec<_>>(),
            trust.revoked().collect::<Vec<_>>(),
        )
    };
    for signed in pairings {
        if let Err(e) = client.register_pairing(signed).await
            && e.server_code() != Some(nexus_proto::v1::SignalErrorCode::Conflict)
        {
            debug!(error = %e, "re-registering a pairing failed");
        }
    }
    for (peer_key, at_ms) in revoked {
        let signed = revocation::revoke_key(&shared.identity, &peer_key, at_ms);
        if let Err(e) = client.revoke_pairing(signed).await
            && e.server_code() != Some(nexus_proto::v1::SignalErrorCode::NotFound)
        {
            debug!(error = %e, "re-sending a revocation failed");
        }
    }
    Ok(())
}

async fn handle_event(
    shared: &Arc<Shared>,
    client: &SignalClient,
    guard: &mut ReplayGuard,
    event: ServerEvent,
) {
    match event {
        ServerEvent::PresenceUpdate(p) => apply_presence(shared, &p),
        ServerEvent::PairingAdded(signed) => {
            let result = shared
                .trust
                .lock()
                .add(&signed)
                .map(|p| p.identity.ed25519_bytes());
            match result {
                Ok(key) => {
                    info!("pairing added by the other device");
                    shared.refresh_allowlist();
                    // Learn the new peer's presence right away.
                    if let Ok(snapshot) = client.subscribe_presence().await {
                        for p in snapshot {
                            apply_presence(shared, &p);
                        }
                    }
                    shared.emit_device_changed(&key);
                }
                Err(e) => warn!(error = %e, "rejected a pairing pushed by the server"),
            }
        }
        ServerEvent::PairingRevoked { signed, .. } => {
            let result = shared
                .trust
                .lock()
                .apply_remote_revocation(&signed, Shared::now_ms());
            match result {
                Ok(key) => {
                    info!("pairing revoked by the other device; dropping sessions");
                    shared.node.allow_list().revoke(&key);
                    shared.devices.lock().remove(&key);
                    shared.emit(nexus_proto::v1::ipc_event::Body::DeviceRemoved(
                        nexus_proto::v1::DeviceRemoved {
                            device_id: nexus_crypto::DeviceId::from_ed25519_bytes(&key)
                                .as_bytes()
                                .to_vec()
                                .into(),
                        },
                    ));
                }
                Err(e) => warn!(error = %e, "ignored an invalid revocation"),
            }
        }
        ServerEvent::Envelope { sealed, .. } => {
            let peers: Vec<_> = shared
                .trust
                .lock()
                .peers()
                .map(|p| p.identity.clone())
                .collect();
            match envelope::open_from_peers(
                &shared.identity,
                &peers,
                guard,
                &sealed,
                Shared::now_ms(),
            ) {
                Ok((peer, env)) => {
                    // Wake / session-request handlers are added in Phase 8.
                    debug!(peer = %peer.device_id(), kind = env.kind, "received a sealed envelope");
                }
                Err(e) => debug!(error = %e, "dropped an invalid envelope"),
            }
        }
        ServerEvent::RendezvousPeer { joined } => {
            forward_rendezvous(shared, RendezvousMsg::Peer(joined)).await
        }
        ServerEvent::RendezvousData(data) => {
            forward_rendezvous(shared, RendezvousMsg::Data(data)).await
        }
        ServerEvent::Superseded | ServerEvent::Disconnected(_) => {}
    }
}

async fn forward_rendezvous(shared: &Shared, msg: RendezvousMsg) {
    let tx = shared.pairing.lock().as_ref().map(|p| p.rendezvous.clone());
    if let Some(tx) = tx {
        let _ = tx.send(msg).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_is_capped_and_resets() {
        let mut b = Backoff::new(Duration::from_secs(8));
        let mut max_seen = Duration::ZERO;
        for _ in 0..50 {
            let d = b.next();
            assert!(
                d >= Duration::from_millis(100) && d <= Duration::from_secs(8),
                "{d:?}"
            );
            max_seen = max_seen.max(d);
        }
        assert!(
            max_seen > Duration::from_secs(1),
            "backoff never grew: {max_seen:?}"
        );
        b.reset();
        assert!(b.next() <= Duration::from_millis(500));
    }
}
