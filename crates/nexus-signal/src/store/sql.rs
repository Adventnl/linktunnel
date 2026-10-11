//! Durable pairing registry on SQLite (zero-setup, single node) or PostgreSQL.
//!
//! One table, one row per unordered pair of devices. The conflict rules are the ones the
//! in-memory store defines and the shared conformance suite checks: a record replaces an existing
//! one only if it is **strictly newer than both the stored record and any revocation**, so an old
//! record can never resurrect a revoked pairing.

use async_trait::async_trait;
use nexus_proto::v1::{SignedPairingRecord, SignedRevocation};
use prost::Message;
use sqlx::Row;

use super::{
    Key, PairingStore, PutOutcome, RevokeOutcome, StoreError, StoredPairing, StoredRevocation,
    canonical,
};

/// Connection to the pairing database.
#[derive(Debug, Clone)]
pub enum SqlPairingStore {
    /// SQLite file or in-memory database.
    #[cfg(feature = "sqlite")]
    Sqlite(sqlx::SqlitePool),
    /// PostgreSQL.
    #[cfg(feature = "postgres")]
    Postgres(sqlx::PgPool),
}

fn db_err(e: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(e.to_string())
}

/// Runs the same body against whichever pool this store holds. The SQL text is shared; the only
/// per-backend difference is the blob column type in the schema.
macro_rules! with_pool {
    ($self:expr, $pool:ident => $body:expr) => {
        match $self {
            #[cfg(feature = "sqlite")]
            SqlPairingStore::Sqlite($pool) => $body,
            #[cfg(feature = "postgres")]
            SqlPairingStore::Postgres($pool) => $body,
        }
    };
}

impl SqlPairingStore {
    /// Connects using a URL: `sqlite:///path/to/pairings.db`, `sqlite::memory:` or
    /// `postgres://user:pass@host/db`, creating the schema if needed.
    pub async fn connect(url: &str) -> Result<Self, StoreError> {
        let store = if url.starts_with("sqlite:") {
            #[cfg(feature = "sqlite")]
            {
                use std::str::FromStr;

                use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
                let options = SqliteConnectOptions::from_str(url)
                    .map_err(db_err)?
                    .create_if_missing(true)
                    .journal_mode(SqliteJournalMode::Wal)
                    .busy_timeout(std::time::Duration::from_secs(5));
                // A single connection keeps `sqlite::memory:` coherent and serialises writers,
                // which is all a single-node registry needs.
                let pool = SqlitePoolOptions::new()
                    .max_connections(1)
                    .connect_with(options)
                    .await
                    .map_err(db_err)?;
                Self::Sqlite(pool)
            }
            #[cfg(not(feature = "sqlite"))]
            return Err(StoreError::Backend(
                "built without the `sqlite` feature".into(),
            ));
        } else if url.starts_with("postgres:") || url.starts_with("postgresql:") {
            #[cfg(feature = "postgres")]
            {
                let pool = sqlx::postgres::PgPoolOptions::new()
                    .max_connections(8)
                    .connect(url)
                    .await
                    .map_err(db_err)?;
                Self::Postgres(pool)
            }
            #[cfg(not(feature = "postgres"))]
            return Err(StoreError::Backend(
                "built without the `postgres` feature".into(),
            ));
        } else {
            return Err(StoreError::Backend(format!(
                "unsupported database URL scheme in `{url}`"
            )));
        };
        store.migrate().await?;
        Ok(store)
    }

    async fn migrate(&self) -> Result<(), StoreError> {
        // Two literal statements (not one format!ed string): the schema is identical except for
        // the blob type name, and literal SQL keeps injection review trivial.
        #[cfg(feature = "sqlite")]
        const SQLITE_DDL: &str = "CREATE TABLE IF NOT EXISTS pairings (
            key_a BLOB NOT NULL, key_b BLOB NOT NULL,
            created_unix_ms BIGINT NOT NULL, signed BLOB NOT NULL,
            revoker BLOB, revoked_unix_ms BIGINT, revocation BLOB,
            PRIMARY KEY (key_a, key_b))";
        #[cfg(feature = "postgres")]
        const POSTGRES_DDL: &str = "CREATE TABLE IF NOT EXISTS pairings (
            key_a BYTEA NOT NULL, key_b BYTEA NOT NULL,
            created_unix_ms BIGINT NOT NULL, signed BYTEA NOT NULL,
            revoker BYTEA, revoked_unix_ms BIGINT, revocation BYTEA,
            PRIMARY KEY (key_a, key_b))";
        const INDEX_DDL: &str = "CREATE INDEX IF NOT EXISTS pairings_key_b ON pairings (key_b)";
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite(pool) => {
                sqlx::query(SQLITE_DDL)
                    .execute(pool)
                    .await
                    .map_err(db_err)?;
                sqlx::query(INDEX_DDL).execute(pool).await.map_err(db_err)?;
            }
            #[cfg(feature = "postgres")]
            Self::Postgres(pool) => {
                sqlx::query(POSTGRES_DDL)
                    .execute(pool)
                    .await
                    .map_err(db_err)?;
                sqlx::query(INDEX_DDL).execute(pool).await.map_err(db_err)?;
            }
        }
        Ok(())
    }
}

fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn to_key(raw: Vec<u8>) -> Result<Key, StoreError> {
    raw.try_into()
        .map_err(|_| StoreError::Backend("corrupt key length in database".into()))
}

#[async_trait]
impl PairingStore for SqlPairingStore {
    async fn put(&self, pairing: StoredPairing) -> Result<PutOutcome, StoreError> {
        let (a, b) = canonical(pairing.a, pairing.b);
        let signed = pairing.signed.encode_to_vec();
        let created = to_i64(pairing.created_unix_ms);
        // Two atomic statements instead of read-modify-write, so concurrent writers cannot race:
        //  1. insert if the pair is new (a conflict is not an error, it just inserts nothing);
        //  2. otherwise replace only if this record is strictly newer than both the stored
        //     record and any revocation, decided inside the database in one statement.
        with_pool!(self, pool => {
            let inserted = sqlx::query(
                "INSERT INTO pairings (key_a, key_b, created_unix_ms, signed) VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (key_a, key_b) DO NOTHING",
            )
            .bind(a.to_vec())
            .bind(b.to_vec())
            .bind(created)
            .bind(signed.clone())
            .execute(pool)
            .await
            .map_err(db_err)?
            .rows_affected();
            if inserted > 0 {
                return Ok(PutOutcome::Inserted);
            }
            let replaced = sqlx::query(
                "UPDATE pairings SET created_unix_ms = $3, signed = $4, revoker = NULL, \
                 revoked_unix_ms = NULL, revocation = NULL \
                 WHERE key_a = $1 AND key_b = $2 AND $3 > created_unix_ms \
                 AND $3 > COALESCE(revoked_unix_ms, 0)",
            )
            .bind(a.to_vec())
            .bind(b.to_vec())
            .bind(created)
            .bind(signed)
            .execute(pool)
            .await
            .map_err(db_err)?
            .rows_affected();
            Ok(if replaced > 0 { PutOutcome::Replaced } else { PutOutcome::Stale })
        })
    }

    async fn revoke(
        &self,
        a: Key,
        b: Key,
        revocation: StoredRevocation,
    ) -> Result<RevokeOutcome, StoreError> {
        let (a, b) = canonical(a, b);
        let encoded = revocation.signed.encode_to_vec();
        with_pool!(self, pool => {
            let updated = sqlx::query(
                "UPDATE pairings SET revoker = $3, revoked_unix_ms = $4, revocation = $5 \
                 WHERE key_a = $1 AND key_b = $2 AND revoked_unix_ms IS NULL",
            )
            .bind(a.to_vec())
            .bind(b.to_vec())
            .bind(revocation.revoker.to_vec())
            .bind(to_i64(revocation.unix_ms))
            .bind(encoded)
            .execute(pool)
            .await
            .map_err(db_err)?
            .rows_affected();
            if updated > 0 {
                return Ok(RevokeOutcome::Revoked);
            }
            let exists = sqlx::query("SELECT 1 FROM pairings WHERE key_a = $1 AND key_b = $2")
                .bind(a.to_vec())
                .bind(b.to_vec())
                .fetch_optional(pool)
                .await
                .map_err(db_err)?
                .is_some();
            Ok(if exists { RevokeOutcome::AlreadyRevoked } else { RevokeOutcome::NotFound })
        })
    }

    async fn active_for(&self, device: Key) -> Result<Vec<StoredPairing>, StoreError> {
        with_pool!(self, pool => {
            let rows = sqlx::query(
                "SELECT key_a, key_b, created_unix_ms, signed FROM pairings \
                 WHERE revoked_unix_ms IS NULL AND (key_a = $1 OR key_b = $1)",
            )
            .bind(device.to_vec())
            .fetch_all(pool)
            .await
            .map_err(db_err)?;
            rows.into_iter()
                .map(|row| {
                    let signed: Vec<u8> = row.try_get("signed").map_err(db_err)?;
                    let created: i64 = row.try_get("created_unix_ms").map_err(db_err)?;
                    Ok(StoredPairing {
                        a: to_key(row.try_get("key_a").map_err(db_err)?)?,
                        b: to_key(row.try_get("key_b").map_err(db_err)?)?,
                        created_unix_ms: u64::try_from(created).unwrap_or(0),
                        signed: SignedPairingRecord::decode(signed.as_slice()).map_err(db_err)?,
                        revoked: None,
                    })
                })
                .collect()
        })
    }

    async fn is_active(&self, a: Key, b: Key) -> Result<bool, StoreError> {
        let (a, b) = canonical(a, b);
        with_pool!(self, pool => {
            Ok(sqlx::query(
                "SELECT 1 FROM pairings WHERE key_a = $1 AND key_b = $2 AND revoked_unix_ms IS NULL",
            )
            .bind(a.to_vec())
            .bind(b.to_vec())
            .fetch_optional(pool)
            .await
            .map_err(db_err)?
            .is_some())
        })
    }
}

// Silence unused-import warnings when a build enables neither backend (the type is then empty).
#[allow(dead_code)]
type _Unused = SignedRevocation;
