//! The `nexus-agent` daemon.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use nexus_agent::{Agent, AgentConfig, DiagnosticsHandler, RemoteHandler, SessionHandler};
use nexus_core::config::{ConfigLoader, LogConfig};
use nexus_core::{Paths, Scope};
use nexus_crypto::provision::load_or_create_identity;
use nexus_crypto::store::{DefaultStoreOptions, open_default};
use nexus_files::{FileRoots, SharedRoot};
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
    /// Do not offer this machine's screen or accept remote input (diagnostics only).
    #[arg(long)]
    no_screen: bool,
    /// Run as a Wake-on-LAN helper: an always-on LAN machine (a Raspberry Pi, the home server)
    /// that sends magic packets for paired devices holding the `wake` grant, and offers nothing
    /// else (no screen, terminal, files, clipboard, audio, power or port forwarding).
    #[arg(long)]
    wake_helper: bool,
    /// Windows only: run as the Windows service. The service starts a second copy of this agent
    /// as SYSTEM inside the active console session, which is what lets it capture and control the
    /// UAC prompt and the lock screen. Installed by the MSI; not for interactive use.
    #[arg(long)]
    windows_service: bool,
    /// Internal SYSTEM-only secure-attention broker event created by the SCM service.
    #[arg(long, hide = true)]
    windows_sas_event: Option<String>,
    /// Keep remote terminals in the SYSTEM service (they then run as SYSTEM: administrator-grade
    /// access for anyone holding the `terminal` grant).
    #[arg(long)]
    allow_system_terminal: bool,
    /// Do not offer remote terminals (set for the SYSTEM service: a shell there would be a
    /// SYSTEM shell).
    #[arg(long)]
    no_terminal: bool,
    /// Share this folder read-only with paired devices that hold the `files` grant (repeatable).
    #[arg(long = "share", value_name = "DIR")]
    share: Vec<PathBuf>,
    /// Share this folder read-write (repeatable).
    #[arg(long = "share-rw", value_name = "DIR")]
    share_rw: Vec<PathBuf>,
    /// Highest frame rate to stream when this device is being watched.
    #[arg(long, default_value_t = 60)]
    max_fps: u32,
    /// Log filter, e.g. `info,nexus_net=debug`.
    #[arg(long, env = "RUST_LOG")]
    log: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = Args::parse();
    #[cfg(windows)]
    {
        if let Some(name) = &args.windows_sas_event {
            nexus_input::windows_inject::configure_secure_attention(name.clone())
                .context("configuring the service secure-attention broker")?;
        }
    }
    if args.wake_helper {
        args.no_screen = true;
        args.share.clear();
        args.share_rw.clear();
    }
    if args.windows_service {
        let service_paths = match &args.home {
            Some(home) => Paths::under(home),
            None => Paths::resolve(Scope::System)?,
        };
        let _service_telemetry = nexus_core::telemetry::init_tracing(&LogConfig {
            file_dir: Some(service_paths.log_dir.join("service")),
            ansi: false,
            ..Default::default()
        });
        // The service runs a SYSTEM copy of this program in the console session; that copy gets
        // the same arguments plus `--system`, and no terminal unless the operator opted in.
        let mut child: Vec<String> = std::env::args()
            .skip(1)
            .filter(|a| a != "--windows-service")
            .collect();
        child.push("--system".into());
        if !child.iter().any(|a| a == "--allow-system-terminal") {
            child.push("--no-terminal".into());
        }
        return tokio::task::spawn_blocking(move || nexus_winservice::run(child))
            .await
            .context("the service thread panicked")?
            .map_err(Into::into);
    }
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
    if args.no_terminal {
        nexus.terminal.enabled = false;
    }
    if args.wake_helper {
        nexus.power.wake_relay = true;
        nexus.power.accept_requests = false;
        nexus.terminal.enabled = false;
        nexus.portfwd.enabled = false;
        nexus.audio.enabled = false;
        nexus.clipboard.send = false;
        nexus.clipboard.receive = false;
        nexus.files.shared.clear();
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
    #[allow(unused_mut)]
    let mut config = AgentConfig::from_nexus_config(&nexus, paths, name)?;
    config.auth_store = Some(Arc::from(store));
    // A SYSTEM agent in a user's session asks the person with a message box on their desktop.
    #[cfg(windows)]
    if args.system {
        config.consent_prompter = Some(Arc::new(nexus_agent::WtsPrompter::new(
            nexus_core::i18n::Locale::detect(),
        )));
    }
    let mut shared: Vec<SharedRoot> = nexus
        .files
        .shared
        .iter()
        .map(|f| SharedRoot {
            path: f.path.clone(),
            writable: f.writable,
        })
        .collect();
    shared.extend(args.share.iter().map(|p| SharedRoot {
        path: p.clone(),
        writable: false,
    }));
    shared.extend(args.share_rw.iter().map(|p| SharedRoot {
        path: p.clone(),
        writable: true,
    }));
    let roots = FileRoots::new(shared);
    if !roots.is_empty() {
        tracing::info!(
            folders = roots.roots().len(),
            "sharing folders with devices that hold the files grant"
        );
    }
    let max_upload = nexus.files.max_upload_mib.saturating_mul(1 << 20);
    let clipboard = (nexus.clipboard.send || nexus.clipboard.receive).then(|| {
        nexus_clipboard::ClipboardSetup {
            factory: nexus_clipboard::os_clipboard(),
            policy: nexus_clipboard::ClipboardPolicy {
                send: nexus.clipboard.send,
                receive: nexus.clipboard.receive,
                images: nexus.clipboard.images,
                ..Default::default()
            },
        }
    });
    let handler: Arc<dyn SessionHandler> =
        if args.no_screen && roots.is_empty() && clipboard.is_none() && !nexus.power.wake_relay {
            Arc::new(DiagnosticsHandler)
        } else {
            let base = if args.no_screen {
                RemoteHandler::new(None, None, args.max_fps)
            } else {
                RemoteHandler::detect_with_display(args.max_fps, &nexus.display)
            };
            let base = base
                .with_files(roots, max_upload)
                .with_wake_relay(nexus.power.wake_relay);
            let base = if nexus.terminal.enabled {
                let host = nexus_terminal::TerminalHost::new(nexus_terminal::HostConfig {
                    allowed_shells: nexus.terminal.shells.clone(),
                    default_shell: None,
                    detached_ttl: std::time::Duration::from_secs(
                        u64::from(nexus.terminal.detached_ttl_hours) * 3600,
                    ),
                    max_sessions: nexus.terminal.max_sessions as usize,
                });
                host.spawn_reaper();
                base.with_terminal(host)
            } else {
                base
            };
            let base = if nexus.portfwd.enabled {
                let mut policy = nexus_portfwd::ForwardPolicy {
                    loopback: nexus.portfwd.loopback,
                    allow: Vec::new(),
                    max_connections: nexus.portfwd.max_connections as usize,
                };
                for t in &nexus.portfwd.allow {
                    policy.allow.push(nexus_portfwd::AllowRule {
                        host: t.host.clone(),
                        ports: (t.port_min, t.port_max.unwrap_or(t.port_min)),
                    });
                }
                base.with_port_forwarding(nexus_portfwd::ForwardHost::new(policy))
            } else {
                base
            };
            let base = match audio_source(&nexus.audio) {
                Some(factory) => base.with_audio(factory),
                None => base,
            };
            let base = if nexus.power.accept_requests {
                base.with_power(Arc::new(nexus_power::SystemPower))
            } else {
                base
            };
            Arc::new(match clipboard {
                Some(c) => base.with_clipboard(c),
                None => base,
            })
        };
    let agent = Agent::start(config, Arc::new(identity), handler).await?;
    tracing::info!(device = %agent.device_id(), ipc = ?agent.ipc_paths().endpoint, "agent running; Ctrl-C to stop");

    tokio::select! {
        _ = tokio::signal::ctrl_c() => tracing::info!("interrupted"),
        () = agent.wait_for_shutdown() => {}
    }
    agent.shutdown().await;
    Ok(())
}

/// The capture source for sound sharing, if this build and configuration offer it.
#[cfg(feature = "audio-device")]
fn audio_source(
    cfg: &nexus_core::config::AudioConfig,
) -> Option<nexus_media::target::AudioSourceFactory> {
    if !cfg.enabled {
        return None;
    }
    let (device, loopback) = (cfg.device.clone(), !cfg.microphone);
    let bitrate = cfg.bitrate_kbps.saturating_mul(1000);
    Some(Arc::new(move |requested: &nexus_audio::StreamConfig| {
        let shaped = nexus_audio::StreamConfig {
            bitrate_bps: if requested.bitrate_bps
                == nexus_audio::StreamConfig::default().bitrate_bps
            {
                bitrate
            } else {
                requested.bitrate_bps
            },
            ..*requested
        };
        nexus_audio::device::DeviceSource::open(&shaped, device.as_deref(), loopback)
            .map(|s| Box::new(s) as Box<dyn nexus_audio::AudioSource>)
    }))
}

#[cfg(not(feature = "audio-device"))]
fn audio_source(
    _: &nexus_core::config::AudioConfig,
) -> Option<nexus_media::target::AudioSourceFactory> {
    None
}
