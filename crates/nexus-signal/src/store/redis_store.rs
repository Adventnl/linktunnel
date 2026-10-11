//! Presence in Redis, for deployments that keep presence outside the server process.
//!
//! Layout:
//!
//! * `nx:presence:<hex key>`: hash with `info` (protobuf), `online`, `last_seen`, `expires`;
//! * `nx:presence:expiry`: sorted set of online devices scored by expiry time.
//!
//! Expiry uses `ZREM` as a claim: when several server processes reap at once, exactly one of them
//! gets `1` back for a given device and reports the change.

use async_trait::async_trait;
use nexus_proto::v1::{PresenceInfo, PresenceState};
use prost::Message;
use redis::AsyncCommands;
use redis::aio::ConnectionManager;

use super::{Key, PresenceEntry, PresenceStore, StoreError};

const EXPIRY_SET: &str = "nx:presence:expiry";

fn redis_err(e: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(e.to_string())
}

fn entry_key(device: &Key) -> String {
    format!("nx:presence:{}", data_encoding::HEXLOWER.encode(device))
}

/// Presence registry backed by Redis.
#[derive(Clone)]
pub struct RedisPresenceStore {
    conn: ConnectionManager,
}

impl std::fmt::Debug for RedisPresenceStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RedisPresenceStore")
    }
}

impl RedisPresenceStore {
    /// Connects to `redis://…` (or `rediss://…`).
    pub async fn connect(url: &str) -> Result<Self, StoreError> {
        let client = redis::Client::open(url).map_err(redis_err)?;
        let conn = ConnectionManager::new(client).await.map_err(redis_err)?;
        Ok(Self { conn })
    }

    fn decode(fields: &std::collections::HashMap<String, Vec<u8>>) -> Option<PresenceEntry> {
        let info = PresenceInfo::decode(fields.get("info")?.as_slice()).ok()?;
        let num = |name: &str| -> Option<u64> {
            std::str::from_utf8(fields.get(name)?).ok()?.parse().ok()
        };
        Some(PresenceEntry {
            info,
            online: fields.get("online").is_some_and(|v| v.as_slice() == b"1"),
            last_seen_ms: num("last_seen")?,
            expires_ms: num("expires")?,
        })
    }
}

#[async_trait]
impl PresenceStore for RedisPresenceStore {
    async fn set(&self, device: Key, entry: PresenceEntry) -> Result<(), StoreError> {
        let mut conn = self.conn.clone();
        let key = entry_key(&device);
        let member = data_encoding::HEXLOWER.encode(&device);
        let mut pipe = redis::pipe();
        pipe.atomic()
            .hset_multiple(
                &key,
                &[
                    ("info", entry.info.encode_to_vec()),
                    (
                        "online",
                        if entry.online {
                            b"1".to_vec()
                        } else {
                            b"0".to_vec()
                        },
                    ),
                    ("last_seen", entry.last_seen_ms.to_string().into_bytes()),
                    ("expires", entry.expires_ms.to_string().into_bytes()),
                ],
            )
            .ignore();
        if entry.online {
            pipe.zadd(EXPIRY_SET, &member, entry.expires_ms as f64)
                .ignore();
        } else {
            pipe.zrem(EXPIRY_SET, &member).ignore();
        }
        pipe.query_async::<()>(&mut conn).await.map_err(redis_err)
    }

    async fn get(&self, device: Key) -> Result<Option<PresenceEntry>, StoreError> {
        let mut conn = self.conn.clone();
        let fields: std::collections::HashMap<String, Vec<u8>> =
            conn.hgetall(entry_key(&device)).await.map_err(redis_err)?;
        Ok(Self::decode(&fields))
    }

    async fn touch(&self, device: Key, now_ms: u64, expires_ms: u64) -> Result<bool, StoreError> {
        // Atomic: only an *online* entry may be refreshed.
        let script = redis::Script::new(
            r"if redis.call('HGET', KEYS[1], 'online') == '1' then
                  redis.call('HSET', KEYS[1], 'last_seen', ARGV[1], 'expires', ARGV[2])
                  redis.call('ZADD', KEYS[2], ARGV[2], ARGV[3])
                  return 1
              end
              return 0",
        );
        let mut conn = self.conn.clone();
        let refreshed: i64 = script
            .key(entry_key(&device))
            .key(EXPIRY_SET)
            .arg(now_ms)
            .arg(expires_ms)
            .arg(data_encoding::HEXLOWER.encode(&device))
            .invoke_async(&mut conn)
            .await
            .map_err(redis_err)?;
        Ok(refreshed == 1)
    }

    async fn expire(&self, now_ms: u64) -> Result<Vec<(Key, PresenceEntry)>, StoreError> {
        let mut conn = self.conn.clone();
        let due: Vec<String> = conn
            .zrangebyscore(EXPIRY_SET, f64::NEG_INFINITY, now_ms as f64)
            .await
            .map_err(redis_err)?;
        let mut changed = Vec::new();
        for member in due {
            // Claim: only the process that removes the member reports (and applies) the change.
            let claimed: i64 = conn.zrem(EXPIRY_SET, &member).await.map_err(redis_err)?;
            if claimed != 1 {
                continue;
            }
            let Ok(raw) = data_encoding::HEXLOWER.decode(member.as_bytes()) else {
                continue;
            };
            let Ok(device) = <Key>::try_from(raw) else {
                continue;
            };
            let Some(mut entry) = self.get(device).await? else {
                continue;
            };
            if !entry.online || entry.expires_ms > now_ms {
                // Refreshed between the range query and the claim: put it back.
                if entry.online {
                    let _: () = conn
                        .zadd(EXPIRY_SET, &member, entry.expires_ms as f64)
                        .await
                        .map_err(redis_err)?;
                }
                continue;
            }
            entry.online = false;
            entry.info.state = PresenceState::Offline as i32;
            self.set(device, entry.clone()).await?;
            changed.push((device, entry));
        }
        Ok(changed)
    }
}
