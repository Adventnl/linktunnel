//! The paired-device allow-list and the hooks that enforce it.
//!
//! Enforcement points (both run right after the TLS handshake, **before any application byte is
//! read or written**):
//!
//! * `after_handshake`: reject any connection, incoming or outgoing, whose authenticated remote
//!   key is not allow-listed;
//! * `before_connect`: refuse to even dial an unpaired key.
//!
//! Because iroh authenticates peers with TLS raw public keys, "remote key" here is
//! cryptographically proven, not claimed.

use std::collections::HashSet;
use std::sync::Arc;

use iroh::EndpointAddr;
use iroh::endpoint::{AfterHandshakeOutcome, BeforeConnectOutcome, Connection, EndpointHooks};
use parking_lot::RwLock;
use tokio::sync::broadcast;
use tracing::debug;

/// ALPN of the session protocol. Connections negotiating anything else are rejected outright.
pub const SESSION_ALPN: &[u8] = b"nexus/session/1";

/// QUIC application close code: the peer is not paired.
pub const CLOSE_NOT_PAIRED: u32 = 0x4E01;
/// QUIC application close code: the pairing was revoked while the connection was live.
pub const CLOSE_REVOKED: u32 = 0x4E02;
/// QUIC application close code: normal end of session.
pub const CLOSE_NORMAL: u32 = 0;

type Key = [u8; 32];

#[derive(Debug, Default)]
struct Inner {
    keys: RwLock<HashSet<Key>>,
}

/// A live, shareable set of trusted Ed25519 keys.
#[derive(Debug, Clone)]
pub struct AllowList {
    inner: Arc<Inner>,
    revocations: broadcast::Sender<Key>,
}

impl Default for AllowList {
    fn default() -> Self {
        Self::new()
    }
}

impl AllowList {
    /// An empty list: nobody may connect.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::default(),
            revocations: broadcast::channel(64).0,
        }
    }

    /// Builds a list from keys.
    #[must_use]
    pub fn from_keys(keys: impl IntoIterator<Item = Key>) -> Self {
        let list = Self::new();
        list.inner.keys.write().extend(keys);
        list
    }

    /// Replaces the whole list (e.g. after reloading the trust store). Keys that disappeared are
    /// treated as revoked.
    pub fn replace(&self, keys: impl IntoIterator<Item = Key>) {
        let new: HashSet<Key> = keys.into_iter().collect();
        let removed: Vec<Key> = {
            let mut guard = self.inner.keys.write();
            let removed = guard.difference(&new).copied().collect();
            *guard = new;
            removed
        };
        for key in removed {
            let _ = self.revocations.send(key);
        }
    }

    /// Allows `key`.
    pub fn allow(&self, key: Key) {
        self.inner.keys.write().insert(key);
    }

    /// Removes `key` and signals every live session with it to terminate.
    pub fn revoke(&self, key: &Key) -> bool {
        let existed = self.inner.keys.write().remove(key);
        let _ = self.revocations.send(*key);
        existed
    }

    /// Whether `key` is currently allowed.
    #[must_use]
    pub fn contains(&self, key: &Key) -> bool {
        self.inner.keys.read().contains(key)
    }

    /// Number of allowed keys.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.keys.read().len()
    }

    /// True when nobody is allowed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Notifications of keys removed from the list.
    #[must_use]
    pub fn subscribe_revocations(&self) -> broadcast::Receiver<Key> {
        self.revocations.subscribe()
    }

    pub(crate) fn hook(&self) -> AllowHook {
        AllowHook {
            allow: self.clone(),
        }
    }
}

/// The endpoint hook enforcing an [`AllowList`].
#[derive(Debug)]
pub(crate) struct AllowHook {
    allow: AllowList,
}

impl EndpointHooks for AllowHook {
    async fn before_connect<'a>(
        &'a self,
        remote_addr: &'a EndpointAddr,
        alpn: &'a [u8],
    ) -> BeforeConnectOutcome {
        if alpn == SESSION_ALPN && !self.allow.contains(remote_addr.id.as_bytes()) {
            debug!("refusing to dial an unpaired device");
            return BeforeConnectOutcome::Reject;
        }
        BeforeConnectOutcome::Accept
    }

    async fn after_handshake<'a>(&'a self, conn: &'a Connection) -> AfterHandshakeOutcome {
        if conn.alpn() != SESSION_ALPN {
            return AfterHandshakeOutcome::Reject {
                error_code: CLOSE_NOT_PAIRED.into(),
                reason: b"unsupported protocol".to_vec(),
            };
        }
        if !self.allow.contains(conn.remote_id().as_bytes()) {
            debug!("rejecting connection: peer is not in the active allow-list");
            return AfterHandshakeOutcome::Reject {
                error_code: CLOSE_NOT_PAIRED.into(),
                reason: b"remote access blocked: check Lockdown and pairing on the target".to_vec(),
            };
        }
        AfterHandshakeOutcome::Accept
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_membership() {
        let list = AllowList::new();
        assert!(list.is_empty());
        list.allow([1; 32]);
        assert!(list.contains(&[1; 32]));
        assert!(!list.contains(&[2; 32]));
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn revoke_removes_and_notifies() {
        let list = AllowList::from_keys([[1; 32], [2; 32]]);
        let mut rx = list.subscribe_revocations();
        assert!(list.revoke(&[1; 32]));
        assert!(!list.contains(&[1; 32]));
        assert!(list.contains(&[2; 32]));
        assert_eq!(rx.try_recv().unwrap(), [1; 32]);
        assert!(!list.revoke(&[9; 32]), "unknown key");
    }

    #[test]
    fn replace_revokes_what_disappears() {
        let list = AllowList::from_keys([[1; 32], [2; 32], [3; 32]]);
        let mut rx = list.subscribe_revocations();
        list.replace([[2; 32], [4; 32]]);
        let mut gone = vec![rx.try_recv().unwrap(), rx.try_recv().unwrap()];
        gone.sort_unstable();
        assert_eq!(gone, vec![[1; 32], [3; 32]]);
        assert!(list.contains(&[4; 32]) && list.contains(&[2; 32]));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn clones_share_state() {
        let a = AllowList::new();
        let b = a.clone();
        a.allow([5; 32]);
        assert!(b.contains(&[5; 32]));
    }
}
