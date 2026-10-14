//! The `nexus-signal` binary.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
#[cfg(feature = "redis")]
use nexus_signal::store::RedisPresenceStore;
#[cfg(any(feature = "sqlite", feature = "postgres"))]
use nexus_signal::store::SqlPairingStore;
use nexus_signal::store::{MemoryPairingStore, MemoryPresenceStore, PairingStore, PresenceStore};
use nexus_signal::{Hub, SignalConfig, serve};
use tokio::net::TcpListener;

/// Signaling, presence and pairing-registry server for Nexus Remote.
#[derive(Debug, Parser)]
#[command(name = "nexus-signal", version)]
struct Args {
    /// Address to listen on.
    #[arg(long, env = "NEXUS_SIGNAL_LISTEN", default_value = "127.0.0.1:8443")]
    listen: SocketAddr,

    /// Name clients sign into their login proof: the public `host[:port]` of this server.
    #[arg(long, env = "NEXUS_SIGNAL_AUDIENCE")]
    audience: String,

    /// Believe `X-Forwarded-For` (only behind a reverse proxy you control).
    #[arg(long, env = "NEXUS_SIGNAL_TRUST_FORWARDED_FOR")]
    trust_forwarded_for: bool,

    /// Where pairings are stored: `sqlite:///var/lib/nexus/pairings.db` (default: in memory only,
    /// lost on restart) or `postgres://user:pass@host/db`.
    #[arg(long, env = "NEXUS_SIGNAL_DATABASE_URL")]
    database_url: Option<String>,

    /// Where presence is kept: `redis://host:6379` (default: in memory).
    #[arg(long, env = "NEXUS_SIGNAL_REDIS_URL")]
    redis_url: Option<String>,

    /// Log filter, e.g. `info,nexus_signal=debug`.
    #[arg(long, env = "RUST_LOG", default_value = "info")]
    log: String,

    /// Directory for daily-rotated server logs (useful for boot-time hosts).
    #[arg(long)]
    log_dir: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let _telemetry = nexus_core::telemetry::init_tracing(&nexus_core::config::LogConfig {
        level: args.log.clone(),
        file_dir: args.log_dir.clone(),
        ..Default::default()
    });

    let mut config = SignalConfig::new(args.audience);
    config.trust_forwarded_for = args.trust_forwarded_for;

    let pairings: Arc<dyn PairingStore> = match &args.database_url {
        #[cfg(any(feature = "sqlite", feature = "postgres"))]
        Some(url) => Arc::new(
            SqlPairingStore::connect(url)
                .await
                .with_context(|| "connecting to the pairing database")?,
        ),
        #[cfg(not(any(feature = "sqlite", feature = "postgres")))]
        Some(_) => anyhow::bail!(
            "this build has no SQL backend (enable the `sqlite` or `postgres` feature)"
        ),
        None => {
            tracing::warn!("no --database-url: pairings are kept in memory and lost on restart");
            Arc::new(MemoryPairingStore::new())
        }
    };
    let presence: Arc<dyn PresenceStore> = match &args.redis_url {
        #[cfg(feature = "redis")]
        Some(url) => Arc::new(
            RedisPresenceStore::connect(url)
                .await
                .with_context(|| "connecting to redis")?,
        ),
        #[cfg(not(feature = "redis"))]
        Some(_) => anyhow::bail!("this build has no Redis backend (enable the `redis` feature)"),
        None => Arc::new(MemoryPresenceStore::new()),
    };
    let hub = Hub::new(config, pairings, presence);

    let listener = TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("binding {}", args.listen))?;
    tracing::info!(addr = %listener.local_addr()?, "nexus-signal listening (TLS must be terminated in front of it)");
    serve(listener, hub, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
    .context("server error")
}
