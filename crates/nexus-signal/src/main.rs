//! The `nexus-signal` binary.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use nexus_signal::store::{MemoryPairingStore, MemoryPresenceStore};
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

    /// Log filter, e.g. `info,nexus_signal=debug`.
    #[arg(long, env = "RUST_LOG", default_value = "info")]
    log: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let _telemetry = nexus_core::telemetry::init_tracing(&nexus_core::config::LogConfig {
        level: args.log.clone(),
        ..Default::default()
    });

    let mut config = SignalConfig::new(args.audience);
    config.trust_forwarded_for = args.trust_forwarded_for;

    // The in-memory stores lose everything on restart; production deployments select the
    // Postgres/Redis backends (see docs/SELF_HOSTING.md).
    let hub = Hub::new(
        config,
        Arc::new(MemoryPairingStore::new()),
        Arc::new(MemoryPresenceStore::new()),
    );

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
