use std::collections::HashMap;

use async_trait::async_trait;
use parking_lot::Mutex;

use super::{
    Key, PairingStore, PresenceEntry, PresenceStore, PutOutcome, RevokeOutcome, StoreError,
    StoredPairing, StoredRevocation, canonical,
};
use nexus_proto::v1::PresenceState;

/// In-memory pairing registry. Loses everything on restart: for tests and single-node trials.
#[derive(Debug, Default)]
pub struct MemoryPairingStore {
    pairs: Mutex<HashMap<(Key, Key), StoredPairing>>,
}

impl MemoryPairingStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl PairingStore for MemoryPairingStore {
    async fn put(&self, pairing: StoredPairing) -> Result<PutOutcome, StoreError> {
        let key = canonical(pairing.a, pairing.b);
        let mut pairs = self.pairs.lock();
        match pairs.get(&key) {
            None => {
                pairs.insert(key, pairing);
                Ok(PutOutcome::Inserted)
            }
            Some(existing) => {
                // A revocation shadows every record created at or before it; a new pairing
                // must be strictly newer than whatever is stored.
                let shadow = existing.revoked.as_ref().map_or(0, |r| r.unix_ms);
                let newest = existing.created_unix_ms.max(shadow);
                if pairing.created_unix_ms > newest {
                    pairs.insert(key, pairing);
                    Ok(PutOutcome::Replaced)
                } else {
                    Ok(PutOutcome::Stale)
                }
            }
        }
    }

    async fn revoke(
        &self,
        a: Key,
        b: Key,
        revocation: StoredRevocation,
    ) -> Result<RevokeOutcome, StoreError> {
        let key = canonical(a, b);
        let mut pairs = self.pairs.lock();
        match pairs.get_mut(&key) {
            None => Ok(RevokeOutcome::NotFound),
            Some(p) if p.revoked.is_some() => Ok(RevokeOutcome::AlreadyRevoked),
            Some(p) => {
                p.revoked = Some(revocation);
                Ok(RevokeOutcome::Revoked)
            }
        }
    }

    async fn active_for(&self, device: Key) -> Result<Vec<StoredPairing>, StoreError> {
        Ok(self
            .pairs
            .lock()
            .values()
            .filter(|p| p.revoked.is_none() && (p.a == device || p.b == device))
            .cloned()
            .collect())
    }

    async fn is_active(&self, a: Key, b: Key) -> Result<bool, StoreError> {
        Ok(self
            .pairs
            .lock()
            .get(&canonical(a, b))
            .is_some_and(|p| p.revoked.is_none()))
    }
}

/// In-memory presence registry.
#[derive(Debug, Default)]
pub struct MemoryPresenceStore {
    entries: Mutex<HashMap<Key, PresenceEntry>>,
}

impl MemoryPresenceStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl PresenceStore for MemoryPresenceStore {
    async fn set(&self, device: Key, entry: PresenceEntry) -> Result<(), StoreError> {
        self.entries.lock().insert(device, entry);
        Ok(())
    }

    async fn get(&self, device: Key) -> Result<Option<PresenceEntry>, StoreError> {
        Ok(self.entries.lock().get(&device).cloned())
    }

    async fn touch(&self, device: Key, now_ms: u64, expires_ms: u64) -> Result<bool, StoreError> {
        let mut entries = self.entries.lock();
        match entries.get_mut(&device) {
            Some(e) if e.online => {
                e.last_seen_ms = now_ms;
                e.expires_ms = expires_ms;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn expire(&self, now_ms: u64) -> Result<Vec<(Key, PresenceEntry)>, StoreError> {
        let mut changed = Vec::new();
        for (key, entry) in self.entries.lock().iter_mut() {
            if entry.online && entry.expires_ms <= now_ms {
                entry.online = false;
                entry.info.state = PresenceState::Offline as i32;
                changed.push((*key, entry.clone()));
            }
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use nexus_proto::v1::{PresenceInfo, SignedPairingRecord, SignedRevocation};

    use super::*;

    fn pairing(a: u8, b: u8, at: u64) -> StoredPairing {
        let (a, b) = canonical([a; 32], [b; 32]);
        StoredPairing {
            a,
            b,
            created_unix_ms: at,
            signed: SignedPairingRecord::default(),
            revoked: None,
        }
    }

    fn revocation(by: u8, at: u64) -> StoredRevocation {
        StoredRevocation {
            revoker: [by; 32],
            unix_ms: at,
            signed: SignedRevocation::default(),
        }
    }

    #[tokio::test]
    async fn put_list_and_active() {
        let s = MemoryPairingStore::new();
        assert_eq!(
            s.put(pairing(1, 2, 100)).await.unwrap(),
            PutOutcome::Inserted
        );
        assert_eq!(
            s.put(pairing(1, 3, 100)).await.unwrap(),
            PutOutcome::Inserted
        );
        assert_eq!(s.active_for([1; 32]).await.unwrap().len(), 2);
        assert_eq!(s.active_for([2; 32]).await.unwrap().len(), 1);
        assert_eq!(s.active_for([9; 32]).await.unwrap().len(), 0);
        assert!(
            s.is_active([2; 32], [1; 32]).await.unwrap(),
            "order must not matter"
        );
        assert!(!s.is_active([2; 32], [3; 32]).await.unwrap());
    }

    #[tokio::test]
    async fn revoked_pairings_vanish_and_cannot_be_resurrected_by_old_records() {
        let s = MemoryPairingStore::new();
        s.put(pairing(1, 2, 100)).await.unwrap();
        assert_eq!(
            s.revoke([1; 32], [2; 32], revocation(1, 200))
                .await
                .unwrap(),
            RevokeOutcome::Revoked
        );
        assert_eq!(
            s.revoke([2; 32], [1; 32], revocation(2, 210))
                .await
                .unwrap(),
            RevokeOutcome::AlreadyRevoked
        );
        assert!(!s.is_active([1; 32], [2; 32]).await.unwrap());
        assert!(s.active_for([1; 32]).await.unwrap().is_empty());
        // Replaying the original (or any record created before the revocation) must not work.
        assert_eq!(s.put(pairing(1, 2, 100)).await.unwrap(), PutOutcome::Stale);
        assert_eq!(s.put(pairing(1, 2, 200)).await.unwrap(), PutOutcome::Stale);
        // A genuinely new pairing afterwards does.
        assert_eq!(
            s.put(pairing(1, 2, 201)).await.unwrap(),
            PutOutcome::Replaced
        );
        assert!(s.is_active([1; 32], [2; 32]).await.unwrap());
    }

    #[tokio::test]
    async fn older_records_do_not_replace_newer() {
        let s = MemoryPairingStore::new();
        s.put(pairing(1, 2, 100)).await.unwrap();
        assert_eq!(s.put(pairing(1, 2, 50)).await.unwrap(), PutOutcome::Stale);
        assert_eq!(s.put(pairing(1, 2, 100)).await.unwrap(), PutOutcome::Stale);
        assert_eq!(
            s.put(pairing(1, 2, 150)).await.unwrap(),
            PutOutcome::Replaced
        );
    }

    #[tokio::test]
    async fn revoking_nothing_reports_not_found() {
        let s = MemoryPairingStore::new();
        assert_eq!(
            s.revoke([1; 32], [2; 32], revocation(1, 1)).await.unwrap(),
            RevokeOutcome::NotFound
        );
    }

    fn entry(online: bool, expires: u64) -> PresenceEntry {
        PresenceEntry {
            info: PresenceInfo {
                state: PresenceState::Online as i32,
                ..Default::default()
            },
            online,
            last_seen_ms: 0,
            expires_ms: expires,
        }
    }

    #[tokio::test]
    async fn presence_expiry_marks_offline_once() {
        let s = MemoryPresenceStore::new();
        s.set([1; 32], entry(true, 1000)).await.unwrap();
        s.set([2; 32], entry(true, 5000)).await.unwrap();
        s.set([3; 32], entry(false, 0)).await.unwrap(); // already offline: untouched
        assert!(s.expire(999).await.unwrap().is_empty());
        let changed = s.expire(1000).await.unwrap();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].0, [1; 32]);
        assert!(!changed[0].1.online);
        assert_eq!(changed[0].1.info.state, PresenceState::Offline as i32);
        assert!(
            s.expire(1000).await.unwrap().is_empty(),
            "reported only once"
        );
        assert!(!s.get([1; 32]).await.unwrap().unwrap().online);
        assert!(s.get([2; 32]).await.unwrap().unwrap().online);
    }

    #[tokio::test]
    async fn touch_extends_only_online_entries() {
        let s = MemoryPresenceStore::new();
        s.set([1; 32], entry(true, 1000)).await.unwrap();
        s.set([2; 32], entry(false, 0)).await.unwrap();
        assert!(s.touch([1; 32], 900, 1900).await.unwrap());
        assert!(
            !s.touch([2; 32], 900, 1900).await.unwrap(),
            "offline entries are not revived by heartbeats"
        );
        assert!(!s.touch([3; 32], 900, 1900).await.unwrap());
        assert!(s.expire(1500).await.unwrap().is_empty());
    }
}
