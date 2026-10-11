//! One behavioural suite that every storage backend must satisfy.
//!
//! Backends: in-memory and SQLite always; PostgreSQL and Redis when `NEXUS_TEST_POSTGRES_URL` /
//! `NEXUS_TEST_REDIS_URL` are set (`scripts/test-with-services.sh` starts containers for them).
//! With `NEXUS_REQUIRE_SERVICES=1` a missing URL is a failure, not a skip (CI sets it).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use nexus_proto::v1::{PresenceInfo, PresenceState, SignedPairingRecord, SignedRevocation};
use nexus_signal::store::{
    Key, MemoryPairingStore, MemoryPresenceStore, PairingStore, PresenceEntry, PresenceStore,
    PutOutcome, RevokeOutcome, StoredPairing, StoredRevocation, canonical,
};

/// A fresh random key so tests sharing one database never interfere.
fn key() -> Key {
    let mut k = [0u8; 32];
    getrandom::fill(&mut k).unwrap();
    k
}

fn pairing(x: Key, y: Key, at: u64) -> StoredPairing {
    let (a, b) = canonical(x, y);
    StoredPairing {
        a,
        b,
        created_unix_ms: at,
        signed: SignedPairingRecord {
            record: format!("record@{at}").into_bytes().into(),
            signature_a: vec![1; 64].into(),
            signature_b: vec![2; 64].into(),
        },
        revoked: None,
    }
}

fn revocation(by: Key, at: u64) -> StoredRevocation {
    StoredRevocation {
        revoker: by,
        unix_ms: at,
        signed: SignedRevocation {
            revocation: b"r".to_vec().into(),
            signature: vec![3; 64].into(),
        },
    }
}

async fn pairing_suite(s: &dyn PairingStore) {
    // put / list / is_active, order-insensitive
    let (a, b, c) = (key(), key(), key());
    assert_eq!(
        s.put(pairing(a, b, 100)).await.unwrap(),
        PutOutcome::Inserted
    );
    assert_eq!(
        s.put(pairing(a, c, 100)).await.unwrap(),
        PutOutcome::Inserted
    );
    assert_eq!(s.active_for(a).await.unwrap().len(), 2);
    assert_eq!(s.active_for(b).await.unwrap().len(), 1);
    assert!(s.is_active(b, a).await.unwrap() && s.is_active(a, b).await.unwrap());
    assert!(!s.is_active(b, c).await.unwrap());
    let stored = s.active_for(b).await.unwrap().remove(0);
    assert_eq!(
        stored.signed.record.as_ref(),
        b"record@100",
        "payload must round-trip byte for byte"
    );
    assert_eq!(stored.created_unix_ms, 100);
    assert_eq!(canonical(stored.a, stored.b), (stored.a, stored.b));

    // Older / equal records never replace newer ones; newer ones do.
    assert_eq!(s.put(pairing(a, b, 50)).await.unwrap(), PutOutcome::Stale);
    assert_eq!(s.put(pairing(b, a, 100)).await.unwrap(), PutOutcome::Stale);
    assert_eq!(
        s.put(pairing(a, b, 150)).await.unwrap(),
        PutOutcome::Replaced
    );
    let ab = canonical(a, b);
    let found = s
        .active_for(a)
        .await
        .unwrap()
        .into_iter()
        .find(|p| (p.a, p.b) == ab);
    assert_eq!(found.map(|p| p.created_unix_ms), Some(150));

    // Revocation: ends the pairing, is idempotent, and shadows every record made before it.
    assert_eq!(
        s.revoke(a, b, revocation(a, 200)).await.unwrap(),
        RevokeOutcome::Revoked
    );
    assert_eq!(
        s.revoke(b, a, revocation(b, 210)).await.unwrap(),
        RevokeOutcome::AlreadyRevoked
    );
    assert!(!s.is_active(a, b).await.unwrap());
    assert!(s.active_for(b).await.unwrap().is_empty());
    assert_eq!(
        s.active_for(a).await.unwrap().len(),
        1,
        "the other pairing is untouched"
    );
    assert_eq!(s.put(pairing(a, b, 150)).await.unwrap(), PutOutcome::Stale);
    assert_eq!(
        s.put(pairing(a, b, 200)).await.unwrap(),
        PutOutcome::Stale,
        "equal to the revocation time"
    );
    assert_eq!(
        s.put(pairing(a, b, 201)).await.unwrap(),
        PutOutcome::Replaced
    );
    assert!(
        s.is_active(a, b).await.unwrap(),
        "a genuinely newer pairing revives the pair"
    );

    // Revoking a pair that never existed.
    assert_eq!(
        s.revoke(key(), key(), revocation(a, 1)).await.unwrap(),
        RevokeOutcome::NotFound
    );
}

async fn concurrent_puts(s: Arc<dyn PairingStore>) {
    // Many racing writers for one pair: no errors, and the newest timestamp always wins.
    let (a, b) = (key(), key());
    let handles: Vec<_> = (1..=16u64)
        .map(|i| {
            let s = Arc::clone(&s);
            tokio::spawn(async move { s.put(pairing(a, b, i * 10)).await.unwrap() })
        })
        .collect();
    for h in handles {
        h.await.unwrap();
    }
    let list = s.active_for(a).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(
        list[0].created_unix_ms, 160,
        "the newest record must win regardless of arrival order"
    );
}

fn entry(online: bool, expires: u64, name: &str) -> PresenceEntry {
    PresenceEntry {
        info: PresenceInfo {
            state: PresenceState::Online as i32,
            name: name.into(),
            seq: 7,
            ..Default::default()
        },
        online,
        last_seen_ms: 5,
        expires_ms: expires,
    }
}

async fn presence_suite(s: &dyn PresenceStore) {
    let (d1, d2, d3, d4) = (key(), key(), key(), key());
    // Round trip, including the full info record.
    s.set(d1, entry(true, 1_000_000, "one")).await.unwrap();
    let got = s.get(d1).await.unwrap().unwrap();
    assert_eq!(got.info.name, "one");
    assert_eq!(got.info.seq, 7);
    assert!(got.online);
    assert_eq!((got.last_seen_ms, got.expires_ms), (5, 1_000_000));
    assert!(s.get(d4).await.unwrap().is_none());

    // Touch extends only online entries.
    s.set(d2, entry(true, 1000, "two")).await.unwrap();
    s.set(d3, entry(false, 0, "three")).await.unwrap();
    assert!(s.touch(d2, 900, 5000).await.unwrap());
    assert!(
        !s.touch(d3, 900, 5000).await.unwrap(),
        "offline entries are not revived by heartbeats"
    );
    assert!(!s.touch(d4, 900, 5000).await.unwrap());
    assert_eq!(s.get(d2).await.unwrap().unwrap().expires_ms, 5000);

    // Expiry: reported exactly once, marks the entry offline, leaves others alone. Other tests
    // may share this store, so look only at the devices of this test.
    let mine = [d1, d2, d3];
    let changed_at = |now: u64| async move { s.expire(now).await.unwrap() };
    let changed: Vec<Key> = changed_at(4999)
        .await
        .into_iter()
        .map(|(k, _)| k)
        .filter(|k| mine.contains(k))
        .collect();
    assert!(changed.is_empty(), "nothing is due at 4999");
    let changed: Vec<_> = changed_at(5000)
        .await
        .into_iter()
        .filter(|(k, _)| mine.contains(k))
        .collect();
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].0, d2);
    assert!(!changed[0].1.online);
    assert_eq!(changed[0].1.info.state, PresenceState::Offline as i32);
    assert!(
        changed_at(5000)
            .await
            .into_iter()
            .all(|(k, _)| !mine.contains(&k)),
        "reported only once"
    );
    assert!(!s.get(d2).await.unwrap().unwrap().online);
    assert!(s.get(d1).await.unwrap().unwrap().online);
}

async fn concurrent_expiry(s: Arc<dyn PresenceStore>) {
    // Several processes reaping at once must not double-report a device.
    let d = key();
    s.set(d, entry(true, 10, "racer")).await.unwrap();
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let s = Arc::clone(&s);
            tokio::spawn(async move {
                s.expire(1_000)
                    .await
                    .unwrap()
                    .into_iter()
                    .filter(|(k, _)| *k == d)
                    .count()
            })
        })
        .collect();
    let mut reports = 0;
    for h in handles {
        reports += h.await.unwrap();
    }
    assert_eq!(reports, 1, "exactly one reaper may claim the expiry");
}

#[allow(dead_code)] // used only when the postgres/redis test features are enabled
fn service_url(var: &str) -> Option<String> {
    match std::env::var(var) {
        Ok(url) if !url.is_empty() => Some(url),
        _ => {
            assert!(
                std::env::var("NEXUS_REQUIRE_SERVICES").is_err(),
                "{var} is required but not set (NEXUS_REQUIRE_SERVICES=1)"
            );
            eprintln!("SKIPPED: {var} not set; run scripts/test-with-services.sh to exercise it");
            None
        }
    }
}

#[tokio::test]
async fn memory_backend() {
    pairing_suite(&MemoryPairingStore::new()).await;
    concurrent_puts(Arc::new(MemoryPairingStore::new())).await;
    presence_suite(&MemoryPresenceStore::new()).await;
    concurrent_expiry(Arc::new(MemoryPresenceStore::new())).await;
}

#[cfg(feature = "sqlite")]
mod sqlite {
    use nexus_signal::store::SqlPairingStore;

    use super::*;

    #[tokio::test]
    async fn sqlite_backend() {
        let s = SqlPairingStore::connect("sqlite::memory:").await.unwrap();
        pairing_suite(&s).await;
        concurrent_puts(Arc::new(
            SqlPairingStore::connect("sqlite::memory:").await.unwrap(),
        ))
        .await;
    }

    #[tokio::test]
    async fn sqlite_data_survives_reopening_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("pairings.db").display()
        );
        let (a, b) = (key(), key());
        {
            let s = SqlPairingStore::connect(&url).await.unwrap();
            s.put(pairing(a, b, 100)).await.unwrap();
            s.put(pairing(a, key(), 100)).await.unwrap();
            s.revoke(a, b, revocation(a, 200)).await.unwrap();
        }
        let s = SqlPairingStore::connect(&url).await.unwrap();
        assert_eq!(
            s.active_for(a).await.unwrap().len(),
            1,
            "the revocation was persisted"
        );
        assert!(!s.is_active(a, b).await.unwrap());
        assert_eq!(
            s.put(pairing(a, b, 150)).await.unwrap(),
            PutOutcome::Stale,
            "and still shadows old records"
        );
    }

    #[tokio::test]
    async fn unknown_schemes_are_rejected() {
        assert!(SqlPairingStore::connect("mysql://nope").await.is_err());
        assert!(SqlPairingStore::connect("not a url").await.is_err());
    }
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn postgres_backend() {
    use nexus_signal::store::SqlPairingStore;
    let Some(url) = service_url("NEXUS_TEST_POSTGRES_URL") else {
        return;
    };
    let s = SqlPairingStore::connect(&url).await.unwrap();
    pairing_suite(&s).await;
    concurrent_puts(Arc::new(SqlPairingStore::connect(&url).await.unwrap())).await;
    // A second connection (a restarted server) sees the same data.
    let (a, b) = (key(), key());
    s.put(pairing(a, b, 100)).await.unwrap();
    let again = SqlPairingStore::connect(&url).await.unwrap();
    assert!(again.is_active(a, b).await.unwrap());
}

#[cfg(feature = "redis")]
#[tokio::test]
async fn redis_backend() {
    use nexus_signal::store::RedisPresenceStore;
    let Some(url) = service_url("NEXUS_TEST_REDIS_URL") else {
        return;
    };
    let s = RedisPresenceStore::connect(&url).await.unwrap();
    presence_suite(&s).await;
    concurrent_expiry(Arc::new(RedisPresenceStore::connect(&url).await.unwrap())).await;
}
