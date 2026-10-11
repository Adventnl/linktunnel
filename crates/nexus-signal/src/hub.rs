//! Shared server state: authenticated connections, presence fan-out and rendezvous rooms.
//!
//! The hub holds the invariants that make the server safe to run even though it is untrusted for
//! content:
//!
//! * presence and envelopes flow only between devices with an **active, dual-signed pairing**;
//! * one live connection per device (a newer login supersedes the older);
//! * a connection that cannot keep up is dropped, never buffered without bound.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use nexus_crypto::DeviceId;
use nexus_proto::v1::{
    DevicePresence, PresenceState, PresenceUpdate, SignalServerMessage, signal_server_message::Body,
};
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::config::SignalConfig;
use crate::ratelimit::KeyedLimiter;
use crate::store::{Key, PairingStore, PresenceEntry, PresenceStore, StoreError};

/// Source of "now" in ms since the Unix epoch (replaceable in tests).
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// Counters exposed at `/metrics`.
#[derive(Debug, Default)]
pub struct Metrics {
    /// Successful logins.
    pub logins: AtomicU64,
    /// Failed logins.
    pub auth_failures: AtomicU64,
    /// Currently authenticated connections (gauge).
    pub connections: AtomicU64,
    /// Sealed envelopes routed.
    pub envelopes: AtomicU64,
    /// Pairings registered.
    pub pairings_registered: AtomicU64,
    /// Pairings revoked.
    pub pairings_revoked: AtomicU64,
    /// Requests refused by a rate limit.
    pub rate_limited: AtomicU64,
}

#[derive(Debug)]
struct Conn {
    id: u64,
    tx: mpsc::Sender<SignalServerMessage>,
    subscribed: bool,
}

#[derive(Debug)]
struct Room {
    created_ms: u64,
    members: Vec<(Key, u64)>,
}

/// Result of delivering a message to a connected device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Queued for the device.
    Delivered,
    /// The device is not connected.
    Offline,
    /// The device's queue was full, so it was disconnected.
    Dropped,
}

/// Why a rendezvous join was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinError {
    /// Two devices are already in the room.
    Full,
}

/// The shared server state.
pub struct Hub {
    pub(crate) config: SignalConfig,
    pub(crate) pairings: Arc<dyn PairingStore>,
    pub(crate) presence: Arc<dyn PresenceStore>,
    pub(crate) metrics: Metrics,
    conns: Mutex<HashMap<Key, Conn>>,
    rooms: Mutex<HashMap<[u8; 16], Room>>,
    auth_failures: Mutex<KeyedLimiter<IpAddr>>,
    joins: Mutex<KeyedLimiter<Key>>,
    envelopes: Mutex<KeyedLimiter<Key>>,
    next_conn: AtomicU64,
    envelope_seq: AtomicU64,
    clock: Clock,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hub")
            .field("audience", &self.config.audience)
            .finish_non_exhaustive()
    }
}

/// `DevicePresence` for the wire from a stored entry.
pub(crate) fn device_presence(key: &Key, entry: &PresenceEntry) -> DevicePresence {
    DevicePresence {
        device_id: DeviceId::from_ed25519_bytes(key).as_bytes().to_vec().into(),
        info: Some(entry.info.clone()),
        online: entry.online,
        last_seen_ms: entry.last_seen_ms,
    }
}

impl Hub {
    /// Creates a hub using the real clock.
    #[must_use]
    pub fn new(
        config: SignalConfig,
        pairings: Arc<dyn PairingStore>,
        presence: Arc<dyn PresenceStore>,
    ) -> Arc<Self> {
        Self::with_clock(
            config,
            pairings,
            presence,
            Arc::new(nexus_core::clock::unix_ms),
        )
    }

    /// Creates a hub with an injected clock.
    #[must_use]
    pub fn with_clock(
        config: SignalConfig,
        pairings: Arc<dyn PairingStore>,
        presence: Arc<dyn PresenceStore>,
        clock: Clock,
    ) -> Arc<Self> {
        Arc::new(Self {
            auth_failures: Mutex::new(KeyedLimiter::new(config.auth_failure_rate, 100_000)),
            joins: Mutex::new(KeyedLimiter::new(config.rendezvous_join_rate, 100_000)),
            envelopes: Mutex::new(KeyedLimiter::new(config.envelope_rate, 100_000)),
            config,
            pairings,
            presence,
            metrics: Metrics::default(),
            conns: Mutex::new(HashMap::new()),
            rooms: Mutex::new(HashMap::new()),
            next_conn: AtomicU64::new(1),
            envelope_seq: AtomicU64::new(1),
            clock,
        })
    }

    /// Current time in ms since the Unix epoch.
    #[must_use]
    pub fn now_ms(&self) -> u64 {
        (self.clock)()
    }

    pub(crate) fn ttl_ms(&self) -> u64 {
        u64::try_from(self.config.presence_ttl.as_millis()).unwrap_or(u64::MAX)
    }

    pub(crate) fn next_envelope_seq(&self) -> u64 {
        self.envelope_seq.fetch_add(1, Ordering::Relaxed)
    }

    // ---- rate limits -------------------------------------------------------------------

    /// True if `ip` has used up its failed-login budget.
    pub(crate) fn auth_blocked(&self, ip: IpAddr) -> bool {
        !self.auth_failures.lock().would_allow(&ip)
    }

    /// Records a failed login from `ip`.
    pub(crate) fn note_auth_failure(&self, ip: IpAddr) {
        self.metrics.auth_failures.fetch_add(1, Ordering::Relaxed);
        let _ = self.auth_failures.lock().check(&ip);
    }

    pub(crate) fn allow_join(&self, device: &Key) -> Result<(), Duration> {
        self.joins.lock().check(device)
    }

    pub(crate) fn allow_envelope(&self, device: &Key) -> Result<(), Duration> {
        self.envelopes.lock().check(device)
    }

    // ---- connections -------------------------------------------------------------------

    /// Registers the authenticated connection for `key`, returning its id. A previous
    /// connection for the same device is told it was superseded and then dropped.
    pub(crate) fn register(&self, key: Key, tx: mpsc::Sender<SignalServerMessage>) -> u64 {
        let id = self.next_conn.fetch_add(1, Ordering::Relaxed);
        let old = self.conns.lock().insert(
            key,
            Conn {
                id,
                tx,
                subscribed: false,
            },
        );
        if let Some(old) = old {
            let _ = old.tx.try_send(SignalServerMessage {
                in_reply_to: 0,
                body: Some(Body::Superseded(nexus_proto::v1::Superseded {})),
            });
            // Dropping `old.tx` ends the old session's outbound channel.
        } else {
            self.metrics.connections.fetch_add(1, Ordering::Relaxed);
        }
        id
    }

    /// Removes the connection if `conn_id` is still the current one for `key`.
    pub(crate) fn unregister(&self, key: &Key, conn_id: u64) -> bool {
        let mut conns = self.conns.lock();
        if conns.get(key).is_some_and(|c| c.id == conn_id) {
            conns.remove(key);
            self.metrics.connections.fetch_sub(1, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    pub(crate) fn set_subscribed(&self, key: &Key, conn_id: u64) {
        if let Some(c) = self.conns.lock().get_mut(key)
            && c.id == conn_id
        {
            c.subscribed = true;
        }
    }

    fn is_subscribed(&self, key: &Key) -> bool {
        self.conns.lock().get(key).is_some_and(|c| c.subscribed)
    }

    /// Whether `key` currently has a connection.
    #[must_use]
    pub fn is_connected(&self, key: &Key) -> bool {
        self.conns.lock().contains_key(key)
    }

    /// Queues `body` for `key`'s connection.
    pub(crate) fn send_to(&self, key: &Key, body: Body) -> Delivery {
        let mut conns = self.conns.lock();
        let Some(conn) = conns.get(key) else {
            return Delivery::Offline;
        };
        match conn.tx.try_send(SignalServerMessage {
            in_reply_to: 0,
            body: Some(body),
        }) {
            Ok(()) => Delivery::Delivered,
            Err(mpsc::error::TrySendError::Full(_)) => {
                warn!("outbound queue full; disconnecting slow client");
                conns.remove(key);
                self.metrics.connections.fetch_sub(1, Ordering::Relaxed);
                Delivery::Dropped
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Delivery::Offline,
        }
    }

    // ---- presence ----------------------------------------------------------------------

    /// Tells every subscribed, paired peer of `key` about its current presence.
    pub(crate) async fn broadcast_presence(&self, key: &Key) -> Result<(), StoreError> {
        let Some(entry) = self.presence.get(*key).await? else {
            return Ok(());
        };
        let device = device_presence(key, &entry);
        for pairing in self.pairings.active_for(*key).await? {
            let peer = if pairing.a == *key {
                pairing.b
            } else {
                pairing.a
            };
            if self.is_subscribed(&peer) {
                self.send_to(
                    &peer,
                    Body::PresenceUpdate(PresenceUpdate {
                        device: Some(device.clone()),
                    }),
                );
            }
        }
        Ok(())
    }

    /// Marks stale online devices offline and tells their peers. Run periodically.
    pub async fn reap_presence(&self) {
        match self.presence.expire(self.now_ms()).await {
            Ok(changed) => {
                for (key, _) in changed {
                    debug!("presence expired");
                    if let Err(e) = self.broadcast_presence(&key).await {
                        warn!(error = %e, "broadcasting expiry failed");
                    }
                }
            }
            Err(e) => warn!(error = %e, "presence expiry failed"),
        }
        self.reap_rooms();
    }

    /// Spawns the background reaper (presence TTL + rendezvous TTL).
    pub fn spawn_reaper(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let hub = Arc::clone(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                hub.reap_presence().await;
            }
        })
    }

    /// The presence state implied by a clean "going offline" notification.
    pub(crate) fn state_for(reason: i32) -> (PresenceState, bool) {
        use nexus_proto::v1::GoingOfflineReason as R;
        match R::try_from(reason).unwrap_or(R::Unspecified) {
            R::Sleep => (PresenceState::Sleeping, false),
            R::Hibernate => (PresenceState::Hibernating, false),
            R::Shutdown | R::Restart => (PresenceState::Offline, false),
            // Logging out keeps the machine reachable.
            R::Logout | R::Unspecified => (PresenceState::Online, true),
        }
    }

    // ---- rendezvous --------------------------------------------------------------------

    /// Adds `key` to `room`. Returns the other member (if present) so both sides can be told.
    pub(crate) fn join_room(
        &self,
        room_id: [u8; 16],
        key: Key,
        conn_id: u64,
    ) -> Result<Option<Key>, JoinError> {
        let now = self.now_ms();
        let mut rooms = self.rooms.lock();
        let room = rooms.entry(room_id).or_insert_with(|| Room {
            created_ms: now,
            members: Vec::new(),
        });
        // Re-joining (e.g. after a reconnect) just refreshes the connection.
        if let Some(pos) = room.members.iter().position(|(k, _)| *k == key) {
            room.members[pos].1 = conn_id;
            return Ok(room
                .members
                .iter()
                .find(|(k, _)| *k != key)
                .map(|(k, _)| *k));
        }
        if room.members.len() >= 2 {
            return Err(JoinError::Full);
        }
        let other = room.members.first().map(|(k, _)| *k);
        room.members.push((key, conn_id));
        Ok(other)
    }

    /// The other member of `room` as seen by `key`.
    pub(crate) fn room_peer(&self, room_id: &[u8; 16], key: &Key) -> Option<Key> {
        self.rooms
            .lock()
            .get(room_id)
            .and_then(|r| r.members.iter().find(|(k, _)| k != key).map(|(k, _)| *k))
    }

    /// Removes `key` from `room`, returning the remaining peer. Empty rooms are deleted.
    pub(crate) fn leave_room(&self, room_id: &[u8; 16], key: &Key) -> Option<Key> {
        let mut rooms = self.rooms.lock();
        let room = rooms.get_mut(room_id)?;
        room.members.retain(|(k, _)| k != key);
        let remaining = room.members.first().map(|(k, _)| *k);
        if room.members.is_empty() {
            rooms.remove(room_id);
        }
        remaining
    }

    fn reap_rooms(&self) {
        let now = self.now_ms();
        let ttl = u64::try_from(self.config.rendezvous_ttl.as_millis()).unwrap_or(u64::MAX);
        self.rooms
            .lock()
            .retain(|_, r| now.saturating_sub(r.created_ms) < ttl);
    }

    /// Number of live rooms (diagnostics and tests).
    #[must_use]
    pub fn room_count(&self) -> usize {
        self.rooms.lock().len()
    }

    /// The configured audience.
    #[must_use]
    pub fn audience(&self) -> &str {
        &self.config.audience
    }
}
