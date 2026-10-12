//! Incoming and outgoing sessions.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use nexus_net::{CLOSE_NOT_PAIRED, Established, NetError, PeerAddr, Session, SessionRole, diag};
use nexus_proto::v1::{Capabilities, Hello, PathKind, VideoCodec};
use nexus_proto::version::{MIN_PROTOCOL_VERSION, PROTOCOL_VERSION};
use nexus_trust::PairedPeer;
use tracing::{debug, info, warn};

use crate::error::AgentError;
use crate::state::Shared;
use crate::sysinfo;

/// Close code used when lockdown refuses a session.
pub const CLOSE_LOCKDOWN: u32 = 0x4E03;
/// Close code used when a device opens sessions faster than allowed.
pub const CLOSE_RATE_LIMITED: u32 = 0x4E04;
/// The person at this machine ended the session from the dashboard / tray / `nexus disconnect`.
pub const CLOSE_DISCONNECTED: u32 = 0x4E05;

/// Everything a handler needs about one established incoming session.
pub struct SessionContext {
    /// The authenticated connection.
    pub session: Session,
    /// The negotiated session (control stream, peer hello, agreed features).
    pub established: Established,
    /// The paired device on the other end.
    pub peer: PairedPeer,
    /// Agent state.
    pub shared: Arc<Shared>,
}

/// What the agent does with an accepted session. Phase 2 serves diagnostics; later phases add
/// the remote-desktop, terminal and file handlers.
#[async_trait]
pub trait SessionHandler: Send + Sync + 'static {
    /// What this build can do, announced in `Hello` and in presence.
    fn capabilities(&self) -> Capabilities;

    /// How this device syncs its clipboard when it controls another one (and, for a target,
    /// with whom). `None` disables clipboard sync.
    fn clipboard(&self) -> Option<nexus_clipboard::ClipboardSetup> {
        None
    }

    /// Runs the session until it ends.
    async fn handle(&self, ctx: SessionContext);
}

/// Answers `nexus ping` diagnostics.
#[derive(Debug, Default)]
pub struct DiagnosticsHandler;

#[async_trait]
impl SessionHandler for DiagnosticsHandler {
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }

    async fn handle(&self, ctx: SessionContext) {
        diag::serve(ctx.session, ctx.established.control).await;
    }
}

/// Builds this device's `Hello`.
pub(crate) fn local_hello(shared: &Shared) -> Hello {
    let mut nonce = [0u8; 16];
    let _ = getrandom::fill(&mut nonce);
    Hello {
        version: Some(PROTOCOL_VERSION),
        min_version: Some(MIN_PROTOCOL_VERSION),
        capabilities: Some(shared.handler.capabilities()),
        os: Some(sysinfo::os_info()),
        agent_version: env!("CARGO_PKG_VERSION").to_owned(),
        session_nonce: nonce.to_vec().into(),
    }
}

/// Decrements the session counter and clears path state when a session ends.
struct SessionGuard {
    shared: Arc<Shared>,
    peer: [u8; 32],
    id: u64,
}

impl SessionGuard {
    fn new(
        shared: &Arc<Shared>,
        peer: [u8; 32],
        path: PathKind,
        who: &nexus_trust::PairedPeer,
        session: &Session,
    ) -> Self {
        shared.sessions.fetch_add(1, Ordering::SeqCst);
        shared.devices.lock().set_path(peer, Some(path));
        let id = shared.next_session_id.fetch_add(1, Ordering::Relaxed);
        shared.active.lock().push(crate::state::ActiveEntry {
            id,
            peer,
            peer_id: who.identity.device_id().as_bytes().to_vec(),
            name: who.name.clone(),
            started_ms: Shared::now_ms(),
            path,
            grants: who.grants_from_peer.iter().cloned().collect(),
            session: session.clone(),
        });
        shared.emit_device_changed(&peer);
        shared.emit_sessions();
        Self {
            shared: Arc::clone(shared),
            peer,
            id,
        }
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.shared.sessions.fetch_sub(1, Ordering::SeqCst);
        self.shared.devices.lock().set_path(self.peer, None);
        self.shared.active.lock().retain(|a| a.id != self.id);
        self.shared.emit_device_changed(&self.peer);
        self.shared.emit_sessions();
    }
}

/// Ends the live incoming sessions of `device_id` (all of them if it is empty). The device is
/// not blocked and may connect again, but must be approved afresh: a person pulling the plug
/// also closes any resume window.
pub(crate) fn disconnect(shared: &Shared, device_id: &[u8]) -> usize {
    let targets: Vec<(u64, [u8; 32], String, nexus_net::Session)> = shared
        .active
        .lock()
        .iter()
        .filter(|a| device_id.is_empty() || a.peer_id == device_id)
        .map(|a| (a.id, a.peer, a.name.clone(), a.session.clone()))
        .collect();
    for (_, key, name, session) in &targets {
        session.close(
            CLOSE_DISCONNECTED,
            "disconnected by the person at this machine",
        );
        shared.limits.lock().ended_on_purpose(key);
        shared
            .audit
            .record("session.disconnected_locally", None, &[("name", name)]);
    }
    if !targets.is_empty() {
        info!(
            count = targets.len(),
            "sessions disconnected by the local user"
        );
    }
    targets.len()
}

/// Accepts incoming sessions for the life of the agent.
pub(crate) async fn accept_loop(shared: Arc<Shared>) {
    let mut shutdown = shared.shutdown.subscribe();
    loop {
        let session = tokio::select! {
            s = shared.node.accept() => match s { Some(s) => s, None => return },
            _ = shutdown.changed() => return,
        };
        let shared = Arc::clone(&shared);
        tokio::spawn(async move { serve_incoming(shared, session).await });
    }
}

async fn serve_incoming(shared: Arc<Shared>, session: Session) {
    let key = session.peer();
    if shared.is_locked_down() {
        info!("lockdown: refusing an incoming session");
        session.close(CLOSE_LOCKDOWN, "lockdown");
        return;
    }
    let Some(peer) = shared.trust.lock().get(&key).cloned() else {
        // The allow-list and trust store disagree (a revocation raced the handshake).
        warn!("incoming session from a device that is no longer paired");
        session.close(CLOSE_NOT_PAIRED, "not paired");
        return;
    };
    if !shared.limits.lock().allow_session(key, Instant::now()) {
        warn!(peer = %peer.identity.device_id(), "too many session attempts; refusing");
        shared.audit.record(
            "session.rate_limited",
            Some(&peer.identity.device_id().to_string()),
            &[("name", &peer.name)],
        );
        session.close(CLOSE_RATE_LIMITED, "too many attempts");
        return;
    }
    let hello = local_hello(&shared);
    let established = match session
        .establish(&hello, SessionRole::Target, &[VideoCodec::H264])
        .await
    {
        Ok(e) => e,
        Err(e) => {
            debug!(error = %e, "hello exchange failed");
            session.close(0, "hello failed");
            return;
        }
    };
    info!(peer = %peer.identity.device_id(), path = ?session.path().kind, "session established");
    let _guard = SessionGuard::new(&shared, key, session.path().kind, &peer, &session);
    let actor = peer.identity.device_id().to_string();
    shared.audit.record(
        "session.accepted",
        Some(&actor),
        &[
            ("name", &peer.name),
            ("path", &format!("{:?}", session.path().kind)),
        ],
    );

    // Keep the dashboard's path indicator current (relay → direct migration, etc.).
    let watcher = {
        let (shared, session) = (Arc::clone(&shared), session.clone());
        tokio::spawn(async move {
            let mut last = session.path().kind;
            while !session.is_closed() {
                tokio::time::sleep(Duration::from_secs(2)).await;
                // An approved session that is alive keeps its resume window open.
                shared.limits.lock().heartbeat(&key, Instant::now());
                let now = session.path().kind;
                if now != last {
                    last = now;
                    shared.devices.lock().set_path(key, Some(now));
                    shared.emit_device_changed(&key);
                }
            }
        })
    };
    shared
        .handler
        .handle(SessionContext {
            session,
            established,
            peer,
            shared: Arc::clone(&shared),
        })
        .await;
    watcher.abort();
    shared.audit.record("session.ended", Some(&actor), &[]);
}

/// Opens a session to the paired device `device_id` as its controller.
pub(crate) async fn dial(
    shared: &Arc<Shared>,
    device_id: &[u8],
) -> Result<(Session, Established), AgentError> {
    if shared.is_locked_down() {
        return Err(AgentError::Denied("lockdown is on".into()));
    }
    let peer = shared
        .find_peer(device_id)
        .ok_or_else(|| AgentError::NotFound("no paired device with that id".into()))?;
    let key = peer.identity.ed25519_bytes();
    let mut addr = PeerAddr::from_key(key);
    if let Some(live) = shared.devices.lock().presence(&key) {
        if !live.info.relay_url.is_empty() {
            addr = addr.with_relay(live.info.relay_url.clone());
        }
        for hint in &live.info.ip_hints {
            if let Ok(sock) = hint.parse() {
                addr = addr.with_direct(sock);
            }
        }
    }
    // Addresses heard on the local network need no server at all (and are the fastest path).
    if let Some(d) = &shared.discovery {
        for sock in d.addresses_for(peer.identity.device_id().as_bytes()) {
            addr = addr.with_direct(sock);
        }
    }
    let session = shared.node.connect(&addr).await.map_err(AgentError::from)?;
    let hello = local_hello(shared);
    let established = session
        .establish(&hello, SessionRole::Controller, &[VideoCodec::H264])
        .await
        .map_err(|e: NetError| AgentError::Failed(format!("session setup failed: {e}")))?;
    Ok((session, established))
}
