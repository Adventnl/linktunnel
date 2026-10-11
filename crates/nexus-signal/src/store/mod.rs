//! Storage behind the signaling server.
//!
//! Two traits so the durable and the ephemeral halves can live in different systems:
//!
//! * [`PairingStore`]: durable. Dual-signed pairing records and revocations (Postgres in
//!   production, in-memory for tests and single-node trials).
//! * [`PresenceStore`]: ephemeral. Who is online and in what state (Redis in production).

mod memory;
#[cfg(feature = "redis")]
mod redis_store;
#[cfg(any(feature = "sqlite", feature = "postgres"))]
mod sql;

use async_trait::async_trait;
pub use memory::{MemoryPairingStore, MemoryPresenceStore};
use nexus_proto::v1::{PresenceInfo, SignedPairingRecord, SignedRevocation};
#[cfg(feature = "redis")]
pub use redis_store::RedisPresenceStore;
#[cfg(any(feature = "sqlite", feature = "postgres"))]
pub use sql::SqlPairingStore;

/// An Ed25519 public key: the primary key of a device everywhere on the server.
pub type Key = [u8; 32];

/// Storage failure.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The backend failed (connection lost, query error, …).
    #[error("storage backend: {0}")]
    Backend(String),
}

/// A stored pairing, keyed by the unordered pair `{a, b}` (`a < b`).
#[derive(Debug, Clone, PartialEq)]
pub struct StoredPairing {
    /// The key that sorts first.
    pub a: Key,
    /// The other key.
    pub b: Key,
    /// Creation time from the signed record.
    pub created_unix_ms: u64,
    /// The signed record exactly as submitted.
    pub signed: SignedPairingRecord,
    /// Set once either party revoked it.
    pub revoked: Option<StoredRevocation>,
}

/// A revocation applied to a stored pairing.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredRevocation {
    /// Who revoked.
    pub revoker: Key,
    /// When they signed it.
    pub unix_ms: u64,
    /// The signed revocation as submitted.
    pub signed: SignedRevocation,
}

/// What `put` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PutOutcome {
    /// New pair.
    Inserted,
    /// Replaced an older pairing (or a revoked one) with a newer record.
    Replaced,
    /// Not stored: an equal-or-newer record, or a later revocation, already exists.
    Stale,
}

/// What `revoke` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevokeOutcome {
    /// The pairing is now revoked.
    Revoked,
    /// No pairing exists between the two keys.
    NotFound,
    /// It was already revoked.
    AlreadyRevoked,
}

/// Durable pairing registry.
#[async_trait]
pub trait PairingStore: Send + Sync + 'static {
    /// Stores `pairing` unless a newer record or a later revocation exists for the pair.
    async fn put(&self, pairing: StoredPairing) -> Result<PutOutcome, StoreError>;

    /// Marks the `{a, b}` pairing revoked.
    async fn revoke(
        &self,
        a: Key,
        b: Key,
        revocation: StoredRevocation,
    ) -> Result<RevokeOutcome, StoreError>;

    /// All non-revoked pairings involving `device`.
    async fn active_for(&self, device: Key) -> Result<Vec<StoredPairing>, StoreError>;

    /// Whether an active (non-revoked) pairing exists between the two keys.
    async fn is_active(&self, a: Key, b: Key) -> Result<bool, StoreError>;
}

/// A device's last published presence.
#[derive(Debug, Clone, PartialEq)]
pub struct PresenceEntry {
    /// What the device published.
    pub info: PresenceInfo,
    /// Whether it is currently connected and heart-beating.
    pub online: bool,
    /// Last time we heard from it (server clock, ms since epoch).
    pub last_seen_ms: u64,
    /// When the entry goes offline if nothing refreshes it (server clock, ms since epoch).
    pub expires_ms: u64,
}

/// Ephemeral presence registry.
#[async_trait]
pub trait PresenceStore: Send + Sync + 'static {
    /// Creates or replaces a device's entry.
    async fn set(&self, device: Key, entry: PresenceEntry) -> Result<(), StoreError>;

    /// Reads a device's entry.
    async fn get(&self, device: Key) -> Result<Option<PresenceEntry>, StoreError>;

    /// Extends the lifetime of an online entry. Returns false if there is none.
    async fn touch(&self, device: Key, now_ms: u64, expires_ms: u64) -> Result<bool, StoreError>;

    /// Marks every online entry whose `expires_ms <= now_ms` as offline and returns them.
    async fn expire(&self, now_ms: u64) -> Result<Vec<(Key, PresenceEntry)>, StoreError>;
}

/// Orders two keys the way pairing records do.
#[must_use]
pub fn canonical(x: Key, y: Key) -> (Key, Key) {
    if x <= y { (x, y) } else { (y, x) }
}
