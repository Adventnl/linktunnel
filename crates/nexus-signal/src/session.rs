//! One authenticated WebSocket connection: login, then the request/response loop.
//!
//! Authorization rules enforced here (each has a test in `tests/`):
//!
//! 1. Until `ClientAuth` succeeds, nothing else is accepted.
//! 2. Pairing registration needs **both** signatures and the caller must be one of the parties.
//! 3. Revocation needs the revoker's signature and the caller must *be* the revoker.
//! 4. Presence snapshots/updates and envelopes only ever involve devices with an active pairing.
//! 5. Asking about an unpaired device is indistinguishable from asking about one that does not
//!    exist (`NOT_PAIRED` in both cases).

use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket};
use bytes::Bytes;
use nexus_crypto::noise::envelope::MAX_SEALED;
use nexus_crypto::{DeviceId, PublicIdentity, SignContext, Signature};
use nexus_proto::signal::{CLIENT_NONCE_LEN, ROOM_ID_LEN, SERVER_NONCE_LEN, auth_message};
use nexus_proto::v1::{
    Ack, AuthOk, ClientAuth, DevicePresence, IncomingEnvelope, PairingAdded, PairingList,
    PairingRevoked, PresenceInfo, PresenceSnapshot, PresenceState, RendezvousData, RendezvousPeer,
    ServerHello, SignalClientMessage, SignalError, SignalErrorCode, SignalServerMessage,
    signal_client_message::Body as C, signal_server_message::Body as S,
};
use nexus_proto::version::PROTOCOL_VERSION;
use nexus_trust::{record, revocation};
use prost::Message as _;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tracing::{debug, info, warn};

use crate::hub::{Delivery, Hub, JoinError, device_presence};
use crate::ratelimit::KeyedLimiter;
use crate::store::{
    Key, PresenceEntry, PutOutcome, RevokeOutcome, StoreError, StoredPairing, StoredRevocation,
    canonical,
};

/// Largest rendezvous payload relayed in one message.
const MAX_RENDEZVOUS_DATA: usize = 16 * 1024;
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// A request that failed, ready to turn into a `SignalError`.
#[derive(Debug)]
struct Failure {
    code: SignalErrorCode,
    message: &'static str,
    retry_after: Duration,
}

impl Failure {
    fn new(code: SignalErrorCode, message: &'static str) -> Self {
        Self {
            code,
            message,
            retry_after: Duration::ZERO,
        }
    }

    fn into_body(self) -> S {
        S::Error(SignalError {
            code: self.code as i32,
            message: self.message.to_owned(),
            retry_after_ms: u32::try_from(self.retry_after.as_millis()).unwrap_or(u32::MAX),
        })
    }
}

impl From<StoreError> for Failure {
    fn from(e: StoreError) -> Self {
        warn!(error = %e, "storage failure");
        Self::new(SignalErrorCode::Internal, "storage unavailable")
    }
}

fn invalid(message: &'static str) -> Failure {
    Failure::new(SignalErrorCode::InvalidRequest, message)
}

fn encode(msg: &SignalServerMessage) -> Message {
    Message::Binary(Bytes::from(msg.encode_to_vec()))
}

async fn send(socket: &mut WebSocket, msg: &SignalServerMessage) -> Result<(), ()> {
    match timeout(SEND_TIMEOUT, socket.send(encode(msg))).await {
        Ok(Ok(())) => Ok(()),
        _ => Err(()),
    }
}

fn server_msg(in_reply_to: u64, body: S) -> SignalServerMessage {
    SignalServerMessage {
        in_reply_to,
        body: Some(body),
    }
}

/// Everything wrong with a presence record, or `Ok`. Bounds all fields so a client cannot make
/// the server store or fan out unbounded data.
fn validate_presence(info: &PresenceInfo) -> Result<(), &'static str> {
    let text_ok = |s: &str, max: usize| s.len() <= max && !s.chars().any(char::is_control);
    if !text_ok(&info.name, 64)
        || !text_ok(&info.active_user, 64)
        || !text_ok(&info.agent_version, 32)
        || !text_ok(&info.relay_url, 256)
    {
        return Err("text field too long or contains control characters");
    }
    if let Some(os) = &info.os
        && (!text_ok(&os.hostname, 64) || !text_ok(&os.version, 64) || !text_ok(&os.arch, 16))
    {
        return Err("os field too long");
    }
    if info.ip_hints.len() > 8 || !info.ip_hints.iter().all(|h| text_ok(h, 64)) {
        return Err("too many or too long ip hints");
    }
    if info.wake_macs.len() > 4 || !info.wake_macs.iter().all(|m| m.len() == 6) {
        return Err("wake macs must be at most four 6-byte addresses");
    }
    if info.battery_percent > 100 && info.battery_percent != 255 {
        return Err("battery percent out of range");
    }
    if let Some(caps) = &info.capabilities
        && (caps.features.len() > 64 || !caps.features.iter().all(|f| text_ok(f, 64)))
    {
        return Err("too many capabilities");
    }
    if info.encoded_len() > 4096 {
        return Err("presence record too large");
    }
    Ok(())
}

fn publishable(state: i32) -> bool {
    matches!(
        PresenceState::try_from(state),
        Ok(PresenceState::Online
            | PresenceState::Idle
            | PresenceState::Locked
            | PresenceState::InUse)
    )
}

struct Session {
    hub: Arc<Hub>,
    key: Key,
    id: DeviceId,
    conn_id: u64,
    out: mpsc::Sender<SignalServerMessage>,
    last_presence_seq: u64,
    room: Option<[u8; 16]>,
    /// A clean "going to sleep / shutting down" was processed; do not overwrite it on disconnect.
    parked: bool,
    limiter: KeyedLimiter<()>,
}

/// Runs a connection from handshake to close.
pub(crate) async fn run(hub: Arc<Hub>, mut socket: WebSocket, ip: IpAddr) {
    let mut nonce = [0u8; SERVER_NONCE_LEN];
    if getrandom::fill(&mut nonce).is_err() {
        return;
    }
    let hello = server_msg(
        0,
        S::Hello(ServerHello {
            version: Some(PROTOCOL_VERSION),
            nonce: nonce.to_vec().into(),
            audience: hub.config.audience.clone(),
            server_time_ms: hub.now_ms(),
        }),
    );
    if send(&mut socket, &hello).await.is_err() {
        return;
    }

    // --- login ---------------------------------------------------------------------------
    if hub.auth_blocked(ip) {
        let f = Failure {
            retry_after: Duration::from_secs(60),
            ..Failure::new(SignalErrorCode::RateLimited, "too many failed logins")
        };
        let _ = send(&mut socket, &server_msg(0, f.into_body())).await;
        return;
    }
    let first = match timeout(
        hub.config.auth_timeout,
        next_client_message(&mut socket, hub.config.max_message_bytes),
    )
    .await
    {
        Ok(Some(m)) => m,
        _ => return,
    };
    let reply_id = first.id;
    let identity = match first.body {
        Some(C::Auth(auth)) => match verify_login(&hub, &nonce, &auth) {
            Ok(identity) => identity,
            Err(f) => {
                hub.note_auth_failure(ip);
                debug!(code = ?f.code, "login rejected");
                let _ = send(&mut socket, &server_msg(reply_id, f.into_body())).await;
                return;
            }
        },
        _ => {
            hub.note_auth_failure(ip);
            let f = Failure::new(SignalErrorCode::Unauthenticated, "authenticate first");
            let _ = send(&mut socket, &server_msg(reply_id, f.into_body())).await;
            return;
        }
    };

    let key = identity.ed25519_bytes();
    let (out, mut rx) = mpsc::channel::<SignalServerMessage>(hub.config.outbound_queue);
    let conn_id = hub.register(key, out.clone());
    hub.metrics.logins.fetch_add(1, Ordering::Relaxed);
    let id = identity.device_id();
    info!(device = %id, "device connected");

    let ok = server_msg(
        reply_id,
        S::AuthOk(AuthOk {
            device_id: id.as_bytes().to_vec().into(),
            heartbeat_interval_ms: ms(hub.config.heartbeat_interval),
            presence_ttl_ms: ms(hub.config.presence_ttl),
            server_time_ms: hub.now_ms(),
            max_message_bytes: u32::try_from(hub.config.max_message_bytes).unwrap_or(u32::MAX),
        }),
    );
    if send(&mut socket, &ok).await.is_err() {
        hub.unregister(&key, conn_id);
        return;
    }

    let mut session = Session {
        limiter: KeyedLimiter::new(hub.config.message_rate, 1),
        hub: Arc::clone(&hub),
        key,
        id,
        conn_id,
        out,
        last_presence_seq: 0,
        room: None,
        parked: false,
    };

    // --- request loop --------------------------------------------------------------------
    let idle = hub.config.idle_timeout;
    let mut keepalive = tokio::time::interval((idle / 3).max(Duration::from_secs(1)));
    keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_rx = Instant::now();
    loop {
        tokio::select! {
            biased;
            outbound = rx.recv() => match outbound {
                Some(msg) => {
                    let superseded = matches!(msg.body, Some(S::Superseded(_)));
                    if send(&mut socket, &msg).await.is_err() || superseded { break; }
                }
                None => break,
            },
            incoming = socket.recv() => match incoming {
                None | Some(Err(_) | Ok(Message::Close(_))) => break,
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => last_rx = Instant::now(),
                Some(Ok(Message::Text(_))) => break,
                Some(Ok(Message::Binary(bytes))) => {
                    last_rx = Instant::now();
                    if bytes.len() > hub.config.max_message_bytes { break; }
                    let Ok(msg) = SignalClientMessage::decode(bytes.as_ref()) else { break };
                    if !session.handle(msg).await { break; }
                }
            },
            _ = keepalive.tick() => {
                if last_rx.elapsed() > idle { debug!("idle timeout"); break; }
                if timeout(SEND_TIMEOUT, socket.send(Message::Ping(Bytes::new()))).await.is_err() { break; }
            }
        }
    }

    session.cleanup().await;
}

fn ms(d: Duration) -> u32 {
    u32::try_from(d.as_millis()).unwrap_or(u32::MAX)
}

/// Reads the next binary protocol message, skipping WebSocket control frames. `None` on close,
/// error or a protocol violation.
async fn next_client_message(socket: &mut WebSocket, max: usize) -> Option<SignalClientMessage> {
    loop {
        match socket.recv().await? {
            Ok(Message::Binary(b)) if b.len() <= max => {
                return SignalClientMessage::decode(b.as_ref()).ok();
            }
            Ok(Message::Ping(_) | Message::Pong(_)) => {}
            _ => return None,
        }
    }
}

/// Validates a login proof and returns the authenticated identity.
fn verify_login(
    hub: &Hub,
    server_nonce: &[u8],
    auth: &ClientAuth,
) -> Result<PublicIdentity, Failure> {
    let version = auth
        .version
        .as_ref()
        .ok_or_else(|| invalid("missing version"))?;
    if version.major != PROTOCOL_VERSION.major {
        return Err(Failure::new(
            SignalErrorCode::IncompatibleVersion,
            "unsupported protocol version",
        ));
    }
    if auth.client_nonce.len() != CLIENT_NONCE_LEN {
        return Err(invalid("bad client nonce"));
    }
    let identity =
        PublicIdentity::from_bytes(&auth.identity).map_err(|_| invalid("bad identity"))?;
    let sig: [u8; 64] = auth
        .signature
        .as_ref()
        .try_into()
        .map_err(|_| invalid("bad signature"))?;
    let message = auth_message(&hub.config.audience, server_nonce, &auth.client_nonce);
    identity
        .verify(
            SignContext::SignalAuth,
            &message,
            &Signature::from_bytes(&sig),
        )
        .map_err(|_| Failure::new(SignalErrorCode::Unauthenticated, "login proof rejected"))?;
    Ok(identity)
}

impl Session {
    fn reply(&self, id: u64, body: S) -> bool {
        match self.out.try_send(server_msg(id, body)) {
            Ok(()) => true,
            Err(_) => false,
        }
    }

    /// Handles one request; returns false if the connection should be closed.
    async fn handle(&mut self, msg: SignalClientMessage) -> bool {
        if let Err(wait) = self.limiter.check(&()) {
            self.hub
                .metrics
                .rate_limited
                .fetch_add(1, Ordering::Relaxed);
            let f = Failure {
                retry_after: wait,
                ..Failure::new(SignalErrorCode::RateLimited, "slow down")
            };
            return self.reply(msg.id, f.into_body());
        }
        let id = msg.id;
        let result = match msg.body {
            None => Err(invalid("empty message")),
            Some(C::Auth(_)) => Err(Failure::new(
                SignalErrorCode::AlreadyAuthenticated,
                "already logged in",
            )),
            Some(C::PublishPresence(p)) => self.publish_presence(p.info).await,
            Some(C::Heartbeat(_)) => self.heartbeat().await,
            Some(C::GoingOffline(g)) => self.going_offline(g.reason).await,
            Some(C::SubscribePresence(_)) => self.subscribe().await,
            Some(C::RegisterPairing(r)) => self.register_pairing(r.signed).await,
            Some(C::RevokePairing(r)) => self.revoke_pairing(r.signed).await,
            Some(C::ListPairings(_)) => self.list_pairings().await,
            Some(C::SendEnvelope(e)) => self.send_envelope(&e.to, e.sealed.to_vec()).await,
            Some(C::RendezvousJoin(j)) => self.rendezvous_join(&j.room_id),
            Some(C::RendezvousSend(s)) => self.rendezvous_send(s.data.to_vec()),
            Some(C::RendezvousLeave(_)) => self.rendezvous_leave(),
        };
        match result {
            Ok(Some(body)) => self.reply(id, body),
            Ok(None) => id == 0 || self.reply(id, S::Ack(Ack {})),
            Err(f) => self.reply(id, f.into_body()),
        }
    }

    // ---- presence ----------------------------------------------------------------------

    async fn publish_presence(&mut self, info: Option<PresenceInfo>) -> Result<Option<S>, Failure> {
        let info = info.ok_or_else(|| invalid("missing presence"))?;
        validate_presence(&info).map_err(invalid)?;
        if !publishable(info.state) {
            return Err(invalid("state must be online, idle, locked or in_use"));
        }
        if info.seq != 0 && info.seq <= self.last_presence_seq {
            return Ok(None); // stale update from a reordered/duplicated message: ignore
        }
        self.last_presence_seq = info.seq;
        let now = self.hub.now_ms();
        let entry = PresenceEntry {
            info,
            online: true,
            last_seen_ms: now,
            expires_ms: now.saturating_add(self.hub.ttl_ms()),
        };
        self.hub.presence.set(self.key, entry).await?;
        self.parked = false;
        self.hub.broadcast_presence(&self.key).await?;
        Ok(None)
    }

    async fn heartbeat(&mut self) -> Result<Option<S>, Failure> {
        let now = self.hub.now_ms();
        let alive = self
            .hub
            .presence
            .touch(self.key, now, now.saturating_add(self.hub.ttl_ms()))
            .await?;
        if alive {
            Ok(None)
        } else {
            // The entry was reaped (or never published): tell the client to publish again.
            Err(Failure::new(
                SignalErrorCode::NotFound,
                "no presence to refresh; publish first",
            ))
        }
    }

    async fn going_offline(&mut self, reason: i32) -> Result<Option<S>, Failure> {
        let (state, stay_online) = Hub::state_for(reason);
        if stay_online {
            return Ok(None);
        }
        let now = self.hub.now_ms();
        let mut entry = self
            .hub
            .presence
            .get(self.key)
            .await?
            .unwrap_or(PresenceEntry {
                info: PresenceInfo::default(),
                online: false,
                last_seen_ms: now,
                expires_ms: 0,
            });
        entry.info.state = state as i32;
        entry.online = false;
        entry.last_seen_ms = now;
        entry.expires_ms = 0;
        self.hub.presence.set(self.key, entry).await?;
        self.parked = true;
        self.hub.broadcast_presence(&self.key).await?;
        Ok(None)
    }

    async fn subscribe(&mut self) -> Result<Option<S>, Failure> {
        self.hub.set_subscribed(&self.key, self.conn_id);
        let mut devices: Vec<DevicePresence> = Vec::new();
        for pairing in self.hub.pairings.active_for(self.key).await? {
            let peer = if pairing.a == self.key {
                pairing.b
            } else {
                pairing.a
            };
            if let Some(entry) = self.hub.presence.get(peer).await? {
                devices.push(device_presence(&peer, &entry));
            }
        }
        Ok(Some(S::PresenceSnapshot(PresenceSnapshot { devices })))
    }

    // ---- pairing registry --------------------------------------------------------------

    async fn register_pairing(
        &mut self,
        signed: Option<nexus_proto::v1::SignedPairingRecord>,
    ) -> Result<Option<S>, Failure> {
        let signed = signed.ok_or_else(|| invalid("missing record"))?;
        let verified = record::verify(&signed).map_err(|_| invalid("pairing record rejected"))?;
        record::check_fresh(&verified, self.hub.now_ms())
            .map_err(|_| invalid("pairing record expired"))?;
        let (a, b) = (verified.a.ed25519_bytes(), verified.b.ed25519_bytes());
        if self.key != a && self.key != b {
            return Err(invalid("caller is not a party to this pairing"));
        }
        let outcome = self
            .hub
            .pairings
            .put(StoredPairing {
                a,
                b,
                created_unix_ms: verified.created_unix_ms,
                signed: signed.clone(),
                revoked: None,
            })
            .await?;
        if outcome == PutOutcome::Stale {
            return Err(Failure::new(
                SignalErrorCode::Conflict,
                "a newer pairing or revocation exists",
            ));
        }
        self.hub
            .metrics
            .pairings_registered
            .fetch_add(1, Ordering::Relaxed);
        let peer = if self.key == a { b } else { a };
        self.hub.send_to(
            &peer,
            S::PairingAdded(PairingAdded {
                signed: Some(signed),
            }),
        );
        // Each side can now see the other's presence.
        for x in [self.key, peer] {
            self.hub.broadcast_presence(&x).await?;
        }
        Ok(None)
    }

    async fn revoke_pairing(
        &mut self,
        signed: Option<nexus_proto::v1::SignedRevocation>,
    ) -> Result<Option<S>, Failure> {
        let signed = signed.ok_or_else(|| invalid("missing revocation"))?;
        let r = revocation::verify(&signed, self.hub.now_ms())
            .map_err(|_| invalid("revocation rejected"))?;
        if r.revoker != self.key {
            return Err(invalid(
                "only the revoking device may submit its revocation",
            ));
        }
        let (a, b) = canonical(r.revoker, r.peer);
        let outcome = self
            .hub
            .pairings
            .revoke(
                a,
                b,
                StoredRevocation {
                    revoker: r.revoker,
                    unix_ms: r.unix_ms,
                    signed: signed.clone(),
                },
            )
            .await?;
        match outcome {
            RevokeOutcome::NotFound => {
                Err(Failure::new(SignalErrorCode::NotFound, "no such pairing"))
            }
            RevokeOutcome::AlreadyRevoked => Ok(None),
            RevokeOutcome::Revoked => {
                self.hub
                    .metrics
                    .pairings_revoked
                    .fetch_add(1, Ordering::Relaxed);
                self.hub.send_to(
                    &r.peer,
                    S::PairingRevoked(PairingRevoked {
                        peer_device_id: self.id.as_bytes().to_vec().into(),
                        signed: Some(signed),
                    }),
                );
                Ok(None)
            }
        }
    }

    async fn list_pairings(&mut self) -> Result<Option<S>, Failure> {
        let pairings = self
            .hub
            .pairings
            .active_for(self.key)
            .await?
            .into_iter()
            .map(|p| p.signed)
            .collect();
        Ok(Some(S::PairingList(PairingList { pairings })))
    }

    // ---- sealed envelopes --------------------------------------------------------------

    async fn send_envelope(&mut self, to: &[u8], sealed: Vec<u8>) -> Result<Option<S>, Failure> {
        if sealed.len() > MAX_SEALED + 64 {
            return Err(Failure::new(
                SignalErrorCode::PayloadTooLarge,
                "envelope too large",
            ));
        }
        if let Err(wait) = self.hub.allow_envelope(&self.key) {
            self.hub
                .metrics
                .rate_limited
                .fetch_add(1, Ordering::Relaxed);
            return Err(Failure {
                retry_after: wait,
                ..Failure::new(SignalErrorCode::RateLimited, "too many envelopes")
            });
        }
        let to: [u8; DeviceId::LEN] = to.try_into().map_err(|_| invalid("bad recipient id"))?;
        let to = DeviceId::from_bytes(to);
        let mut recipient = None;
        for pairing in self.hub.pairings.active_for(self.key).await? {
            let peer = if pairing.a == self.key {
                pairing.b
            } else {
                pairing.a
            };
            if DeviceId::from_ed25519_bytes(&peer) == to {
                recipient = Some(peer);
                break;
            }
        }
        // Unpaired and nonexistent look the same on purpose.
        let peer = recipient.ok_or_else(|| {
            Failure::new(SignalErrorCode::NotPaired, "not paired with that device")
        })?;
        let envelope = IncomingEnvelope {
            from: self.id.as_bytes().to_vec().into(),
            sealed: sealed.into(),
            server_seq: self.hub.next_envelope_seq(),
        };
        match self.hub.send_to(&peer, S::Envelope(envelope)) {
            Delivery::Delivered => {
                self.hub.metrics.envelopes.fetch_add(1, Ordering::Relaxed);
                Ok(None)
            }
            Delivery::Offline | Delivery::Dropped => Err(Failure::new(
                SignalErrorCode::RecipientOffline,
                "recipient is not connected",
            )),
        }
    }

    // ---- rendezvous --------------------------------------------------------------------

    fn rendezvous_join(&mut self, room_id: &[u8]) -> Result<Option<S>, Failure> {
        let room: [u8; ROOM_ID_LEN] = room_id.try_into().map_err(|_| invalid("bad room id"))?;
        if let Err(wait) = self.hub.allow_join(&self.key) {
            self.hub
                .metrics
                .rate_limited
                .fetch_add(1, Ordering::Relaxed);
            return Err(Failure {
                retry_after: wait,
                ..Failure::new(SignalErrorCode::RateLimited, "too many pairing attempts")
            });
        }
        if let Some(old) = self.room.filter(|r| *r != room) {
            self.leave_current_room(Some(old));
        }
        match self.hub.join_room(room, self.key, self.conn_id) {
            Ok(other) => {
                self.room = Some(room);
                if let Some(other) = other {
                    let joined = |j| S::RendezvousPeer(RendezvousPeer { joined: j });
                    self.hub.send_to(&other, joined(true));
                    self.hub.send_to(&self.key, joined(true));
                }
                Ok(None)
            }
            Err(JoinError::Full) => Err(Failure::new(SignalErrorCode::RoomFull, "room is full")),
        }
    }

    fn rendezvous_send(&mut self, data: Vec<u8>) -> Result<Option<S>, Failure> {
        if data.len() > MAX_RENDEZVOUS_DATA {
            return Err(Failure::new(
                SignalErrorCode::PayloadTooLarge,
                "rendezvous message too large",
            ));
        }
        let room = self
            .room
            .ok_or_else(|| Failure::new(SignalErrorCode::NotFound, "not in a rendezvous room"))?;
        let peer = self.hub.room_peer(&room, &self.key).ok_or_else(|| {
            Failure::new(
                SignalErrorCode::PeerAbsent,
                "the other device has not joined",
            )
        })?;
        match self.hub.send_to(
            &peer,
            S::RendezvousData(RendezvousData { data: data.into() }),
        ) {
            Delivery::Delivered => Ok(None),
            _ => Err(Failure::new(
                SignalErrorCode::PeerAbsent,
                "the other device is gone",
            )),
        }
    }

    fn rendezvous_leave(&mut self) -> Result<Option<S>, Failure> {
        self.leave_current_room(self.room);
        Ok(None)
    }

    fn leave_current_room(&mut self, room: Option<[u8; ROOM_ID_LEN]>) {
        let Some(room) = room else { return };
        if let Some(remaining) = self.hub.leave_room(&room, &self.key) {
            self.hub.send_to(
                &remaining,
                S::RendezvousPeer(RendezvousPeer { joined: false }),
            );
        }
        if self.room == Some(room) {
            self.room = None;
        }
    }

    // ---- teardown ----------------------------------------------------------------------

    async fn cleanup(mut self) {
        self.leave_current_room(self.room);
        let current = self.hub.unregister(&self.key, self.conn_id);
        info!(device = %self.id, "device disconnected");
        if !current || self.parked {
            return;
        }
        // An abrupt disconnect: mark the device offline immediately instead of waiting for the TTL.
        match self.hub.presence.get(self.key).await {
            Ok(Some(mut entry)) if entry.online => {
                entry.online = false;
                entry.info.state = PresenceState::Offline as i32;
                entry.last_seen_ms = self.hub.now_ms();
                entry.expires_ms = 0;
                if self.hub.presence.set(self.key, entry).await.is_ok()
                    && let Err(e) = self.hub.broadcast_presence(&self.key).await
                {
                    warn!(error = %e, "broadcasting disconnect failed");
                }
            }
            Ok(_) => {}
            Err(e) => warn!(error = %e, "could not mark device offline"),
        }
    }
}
