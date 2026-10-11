//! The `nexus-agent` daemon.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use nexus_agent::{Agent, AgentConfig, DiagnosticsHandler};
use nexus_core::config::{ConfigLoader, LogConfig};
use nexus_core::{Paths, Scope};
use nexus_crypto::provision::load_or_create_identity;
use nexus_crypto::store::{DefaultStoreOptions, open_default};
use url::Url;

const KEYCHAIN_SERVICE: &str = "app.nexus-remote";

/// Nexus Remote agent: keeps this device reachable by your paired devices.
#[derive(Debug, Parser)]
#[command(name = "nexus-agent", version)]
struct Args {
    /// Keep all state under this directory instead of the OS defaults.
    #[arg(long, env = "NEXUS_HOME")]
    home: Option<PathBuf>,
    /// Signaling server (`wss://…/v1/ws`). Overrides the config file.
    #[arg(long, env = "NEXUS_SIGNAL_URL")]
    signal_url: Option<Url>,
    /// Allow a plain `ws://` signaling URL to a non-loopback host. Trials on a trusted LAN only.
    #[arg(long)]
    allow_insecure_signal: bool,
    /// Name shown on your other devices (defaults to the host name).
    #[arg(long)]
    name: Option<String>,
    /// Store the identity in an owner-only file instead of the OS keychain.
    #[arg(long)]
    file_store: bool,
    /// Run as the machine-wide service account (uses system state directories).
    #[arg(long)]
    system: bool,
    /// Log filter, e.g. `info,nexus_net=debug`.
    #[arg(long, env = "RUST_LOG")]
    log: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let paths = match (&args.home, args.system) {
        (Some(home), _) => Paths::under(home),
        (None, true) => Paths::resolve(Scope::System)?,
        (None, false) => Paths::resolve(Scope::User)?,
    };

    let mut nexus = ConfigLoader::new()
        .with_file(paths.config_file())
        .with_env()
        .load()
        .context("loading configuration")?;
    if let Some(url) = args.signal_url {
        nexus.net.signal_url = Some(url);
    }
    if args.allow_insecure_signal {
        nexus.net.allow_insecure_signal = true;
        tracing::warn!(
            "plain ws:// signaling allowed: metadata is visible to anyone on the network path"
        );
    }
    if let Some(level) = args.log {
        nexus.log.level = level;
    }
    nexus.validate().context("validating configuration")?;
    let _telemetry = nexus_core::telemetry::init_tracing(&LogConfig {
        ..nexus.log.clone()
    });

    let store = open_default(&DefaultStoreOptions {
        service: KEYCHAIN_SERVICE.into(),
        file_dir: paths.data_dir.join("keys"),
        prefer_os_store: !args.file_store && !args.system && args.home.is_none(),
        allow_file_fallback: true,
    })
    .context("opening the secret store")?;
    let (identity, how) =
        load_or_create_identity(store.as_ref()).context("loading the device identity")?;
    tracing::info!(?how, store = %store.describe(), device = %identity.device_id(), "identity ready");

    let name = args.name.unwrap_or_else(nexus_agent::sysinfo::hostname);
    let config = AgentConfig::from_nexus_config(&nexus, paths, name)?;
    let agent = Agent::start(config, Arc::new(identity), Arc::new(DiagnosticsHandler)).await?;
    tracing::info!(device = %agent.device_id(), ipc = ?agent.ipc_paths().endpoint, "agent running; Ctrl-C to stop");

    tokio::select! {
        _ = tokio::signal::ctrl_c() => tracing::info!("interrupted"),
        () = agent.wait_for_shutdown() => {}
    }
    agent.shutdown().await;
    Ok(())
}
