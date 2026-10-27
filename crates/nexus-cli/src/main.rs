//! `nexus`: command line interface for linktunnel.
//!
//! Runtime messages are localised (`--lang`, `NEXUS_LANG`, or the OS locale). Clap's own `--help`
//! text is English-only; that is a clap limitation, tracked in docs/USER_GUIDE.md.

// A CLI's job is to print.
#![allow(clippy::print_stdout, clippy::print_stderr)]

mod agent_cmds;
mod audit_cmds;
mod auth_cmds;
mod consent_cmds;
mod dashboard;
mod files_cmds;
mod forward_cmds;
mod identity_cmd;
mod power_cmds;
mod remote_cmds;
mod shell_cmds;
mod update_cmds;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use nexus_core::config::ConfigLoader;
use nexus_core::i18n::{Locale, tr};
use nexus_core::{Paths, Scope};
use nexus_crypto::store::{DefaultStoreOptions, SecretStore, open_default};

/// Keychain service name under which all Nexus items are stored.
const KEYCHAIN_SERVICE: &str = "app.nexus-remote";

#[derive(Debug, Parser)]
#[command(
    name = "nexus",
    version,
    about = "Secure, real-time remote control for your own devices"
)]
struct Cli {
    /// Message language (`en`, `zh-CN`). Defaults to the OS locale.
    #[arg(long, global = true, env = "NEXUS_LANG")]
    lang: Option<String>,

    /// Keep all state under this directory instead of the OS defaults.
    #[arg(long, global = true, env = "NEXUS_HOME")]
    home: Option<PathBuf>,

    /// Talk to the machine-wide service agent (`nexus-agent --system`, as installed by the
    /// packages) instead of the per-user one. Usually needs `sudo` / an elevated terminal.
    #[arg(long, global = true)]
    system: bool,

    /// Store the identity in an owner-only file instead of the OS keychain.
    #[arg(long, global = true)]
    file_store: bool,

    /// More logging (repeat for more detail).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Enroll passkeys/TOTP and verify unattended access.
    #[command(subcommand)]
    Auth(auth_cmds::AuthCommand),
    /// Manage this device's cryptographic identity.
    #[command(subcommand)]
    Id(identity_cmd::IdCommand),
    /// Check that this machine is ready to run linktunnel.
    Doctor,
    /// Show the running agent's status.
    Status,
    /// List paired devices and their live state.
    Devices,
    /// Pair this device with another one.
    #[command(subcommand)]
    Pair(agent_cmds::PairCommand),
    /// Remove a pairing (by name or ID).
    Unpair {
        /// Device name or ID.
        device: String,
    },
    /// Measure latency and throughput to a paired device.
    Ping {
        /// Device name or ID.
        device: String,
        /// Megabytes to upload in the throughput test.
        #[arg(long, default_value_t = 32)]
        bulk_mb: u64,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Look at and control a paired device's screen (screenshot, type, key, click, monitors).
    #[command(flatten_help = true, subcommand)]
    Remote(remote_cmds::RemoteCommand),
    /// List a paired device's shared folders or a directory (`nexus ls DEVICE:` / `DEVICE:/path`).
    Ls {
        /// `DEVICE` or `DEVICE:PATH`.
        target: String,
    },
    /// Copy a file to or from a paired device (`nexus cp ./a.zip DEVICE:/share/`).
    ///
    /// Resumes an interrupted copy and verifies the whole file with BLAKE3.
    Cp {
        /// Source: a local path or `DEVICE:PATH`.
        src: String,
        /// Destination: a local path or `DEVICE:PATH` (a trailing `/` keeps the file name).
        dst: String,
        /// Replace the destination if it exists.
        #[arg(long)]
        overwrite: bool,
    },
    /// Create a directory on a paired device.
    Mkdir {
        /// `DEVICE:PATH`.
        target: String,
    },
    /// Delete a file or directory on a paired device.
    Rm {
        /// `DEVICE:PATH`.
        target: String,
        /// Also delete a directory's contents.
        #[arg(short, long)]
        recursive: bool,
    },
    /// Live dashboard of your devices: state at a glance, one key to connect, wake or lock.
    Dashboard {
        /// Print one text snapshot and exit (scripts, bug reports).
        #[arg(long)]
        snapshot: bool,
        /// Width of the snapshot in columns.
        #[arg(long, default_value_t = 110)]
        width: u16,
    },
    /// Reach a service on a paired device through a local port (`nexus forward pc 5432`).
    Forward {
        /// Device name or ID.
        device: String,
        /// `[LOCAL_PORT:]HOST:PORT`, `LOCAL_PORT:PORT` or just `PORT` (HOST defaults to
        /// localhost as seen from the device).
        spec: String,
    },
    /// Pipe stdin/stdout to a TCP service on a paired device, for use as an SSH `ProxyCommand`:
    /// `Host box` / `ProxyCommand nexus proxy box` in ~/.ssh/config, then `ssh box`.
    Proxy {
        /// Device name or ID.
        device: String,
        /// Host as seen from the device.
        #[arg(default_value = "localhost")]
        host: String,
        /// Port on that host.
        #[arg(default_value_t = 22)]
        port: u16,
    },
    /// Open a terminal on a paired device (`--name` keeps it running when you disconnect).
    Shell {
        /// Device name or ID.
        device: String,
        /// Persistent session name: detach with Ctrl-] . and come back with the same name.
        #[arg(short, long)]
        name: Option<String>,
        /// Program to run instead of the device's default shell.
        #[arg(long)]
        program: Option<String>,
        /// Only re-attach to an existing named session.
        #[arg(long)]
        attach_only: bool,
    },
    /// Lock, sleep, restart or shut down a paired device.
    Power {
        /// Device name or ID.
        device: String,
        /// What to do.
        #[arg(value_enum)]
        action: power_cmds::Action,
        /// Do not ask for confirmation.
        #[arg(long)]
        yes: bool,
    },
    /// Wake a sleeping paired device with Wake-on-LAN.
    Wake {
        /// Device name or ID.
        device: String,
        /// Send the packets from this other paired device's network (an always-on machine
        /// on the sleeping device's LAN) instead of from here.
        #[arg(long)]
        via: Option<String>,
    },
    /// Check for, download and apply signed updates.
    #[command(subcommand)]
    Update(update_cmds::UpdateCommand),
    /// Read and verify this device's tamper-evident audit log.
    #[command(subcommand)]
    Audit(audit_cmds::AuditCommand),
    /// Answer "may this device use me?" prompts in this terminal.
    Consent,
    /// Choose which paired devices may use this one without asking.
    #[command(subcommand)]
    Trust(consent_cmds::TrustCommand),
    /// Turn the emergency lockdown switch on or off.
    #[command(subcommand)]
    Lockdown(agent_cmds::LockdownCommand),
    /// Show who is connected to this machine right now.
    Sessions,
    /// Cut off whoever is connected to this machine (one device, or everyone).
    Disconnect {
        /// Device name or ID; omit to disconnect everyone.
        device: Option<String>,
    },
    /// Ask the running agent to exit.
    Stop,
}

/// Everything a subcommand needs to know about its environment.
pub(crate) struct AppContext {
    pub locale: Locale,
    pub paths: Paths,
    pub store: Box<dyn SecretStore>,
    /// Global flags to repeat when this process starts another `nexus` (the dashboard does).
    pub passthrough: Vec<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<ExitCode> {
    let level = match cli.verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let log = nexus_core::config::LogConfig {
        level: level.into(),
        ..Default::default()
    };
    let _telemetry = nexus_core::telemetry::init_tracing(&log);

    let locale = cli
        .lang
        .as_deref()
        .and_then(Locale::parse)
        .unwrap_or_else(Locale::detect);
    let paths = match (&cli.home, cli.system) {
        (Some(home), _) => Paths::under(home),
        (None, true) => Paths::resolve(Scope::System).context("locating state directories")?,
        (None, false) => Paths::resolve(Scope::User).context("locating state directories")?,
    };
    let store = open_default(&DefaultStoreOptions {
        service: KEYCHAIN_SERVICE.into(),
        file_dir: paths.data_dir.join("keys"),
        prefer_os_store: !cli.file_store && !cli.system && cli.home.is_none(),
        allow_file_fallback: true,
    })
    .context("opening the secret store")?;

    let mut passthrough = Vec::new();
    if let Some(home) = &cli.home {
        passthrough.extend(["--home".to_owned(), home.display().to_string()]);
    }
    if let Some(lang) = &cli.lang {
        passthrough.extend(["--lang".to_owned(), lang.clone()]);
    }
    if cli.file_store {
        passthrough.push("--file-store".into());
    }
    if cli.system {
        passthrough.push("--system".into());
    }
    let ctx = AppContext {
        locale,
        paths,
        store,
        passthrough,
    };
    match cli.command {
        Command::Auth(cmd) => auth_cmds::run(&ctx, cmd).await,
        Command::Id(cmd) => identity_cmd::run(&ctx, &cmd),
        Command::Doctor => doctor(&ctx).await,
        Command::Status => agent_cmds::status(&ctx).await,
        Command::Devices => agent_cmds::devices(&ctx).await,
        Command::Pair(cmd) => agent_cmds::pair(&ctx, &cmd).await,
        Command::Unpair { device } => agent_cmds::unpair(&ctx, &device).await,
        Command::Ping {
            device,
            bulk_mb,
            json,
        } => agent_cmds::ping(&ctx, &device, bulk_mb, json).await,
        Command::Remote(cmd) => remote_cmds::run(&ctx, cmd).await,
        Command::Ls { target } => files_cmds::ls(&ctx, &target).await,
        Command::Cp {
            src,
            dst,
            overwrite,
        } => files_cmds::cp(&ctx, &src, &dst, overwrite).await,
        Command::Mkdir { target } => files_cmds::mkdir(&ctx, &target).await,
        Command::Rm { target, recursive } => files_cmds::rm(&ctx, &target, recursive).await,
        Command::Dashboard { snapshot, width } => dashboard::run(&ctx, snapshot, width).await,
        Command::Forward { device, spec } => forward_cmds::forward(&ctx, &device, &spec).await,
        Command::Proxy { device, host, port } => {
            forward_cmds::proxy(&ctx, &device, &host, port).await
        }
        Command::Shell {
            device,
            name,
            program,
            attach_only,
        } => shell_cmds::shell(&ctx, &device, name, program, attach_only).await,
        Command::Power {
            device,
            action,
            yes,
        } => power_cmds::power(&ctx, &device, action, yes).await,
        Command::Wake { device, via } => power_cmds::wake(&ctx, &device, via.as_deref()).await,
        Command::Update(cmd) => update_cmds::run(&ctx, cmd).await,
        Command::Audit(cmd) => audit_cmds::run(&ctx, &cmd),
        Command::Consent => consent_cmds::watch(&ctx).await,
        Command::Trust(cmd) => consent_cmds::trust(&ctx, cmd).await,
        Command::Lockdown(cmd) => agent_cmds::lockdown(&ctx, &cmd).await,
        Command::Sessions => agent_cmds::sessions(&ctx).await,
        Command::Disconnect { device } => agent_cmds::disconnect(&ctx, device.as_deref()).await,
        Command::Stop => agent_cmds::stop(&ctx).await,
    }
}

async fn doctor(ctx: &AppContext) -> Result<ExitCode> {
    println!("{}\n", tr(ctx.locale, "cli.doctor.title"));
    let mut healthy = true;
    let mut report = |label: &str, result: Result<String>| {
        let (status, detail) = match result {
            Ok(detail) => (tr(ctx.locale, "cli.doctor.ok"), detail),
            Err(e) => {
                healthy = false;
                (tr(ctx.locale, "cli.doctor.fail"), format!("{e:#}"))
            }
        };
        println!("  [{status}] {label}: {detail}");
    };
    // Things worth knowing that are not faults (LAN-only use, headless machines).
    let note = |label: &str, detail: String| {
        println!(
            "  [{}] {label}: {detail}",
            tr(ctx.locale, "cli.doctor.note")
        );
    };

    report(
        "config",
        ConfigLoader::new()
            .with_file(ctx.paths.config_file())
            .with_env()
            .load()
            .map(|_| ctx.paths.config_file().display().to_string())
            .map_err(Into::into),
    );
    report(
        "state dir",
        ctx.paths
            .ensure()
            .map(|()| ctx.paths.data_dir.display().to_string())
            .map_err(Into::into),
    );
    report(
        "secret store",
        Ok(identity_cmd::describe_store(
            ctx.locale,
            ctx.store.as_ref(),
            &ctx.paths,
        )),
    );
    report(
        "identity",
        nexus_crypto::load_identity(ctx.store.as_ref())
            .map_err(Into::into)
            .and_then(|id| match id {
                Some(id) => Ok(id.device_id().to_string()),
                None => Err(anyhow::anyhow!(tr(ctx.locale, "cli.identity.missing"))),
            }),
    );

    // The running agent: is it up, can it reach the signaling server, who is connected.
    match agent_cmds::connect(ctx).await {
        Ok(client) => match client.status().await {
            Ok(st) => {
                report(
                    "agent",
                    Ok(format!(
                        "{} {} ({})",
                        st.device_name, st.agent_version, st.fingerprint
                    )),
                );
                if st.signal_connected {
                    report("signaling", Ok(st.signal_url.clone()));
                } else if st.signal_url.is_empty() {
                    note(
                        "signaling",
                        "not configured: LAN and direct connections only".into(),
                    );
                } else {
                    note(
                        "signaling",
                        format!(
                            "{} is unreachable: devices on other networks cannot find this one",
                            st.signal_url
                        ),
                    );
                }
                note("paired devices", st.paired_devices.to_string());
                if st.lockdown {
                    note(
                        "lockdown",
                        "ON: every remote access request is refused".into(),
                    );
                }
                match client.list_sessions().await {
                    Ok(list) if !list.sessions.is_empty() => {
                        let names: Vec<&str> =
                            list.sessions.iter().map(|s| s.peer_name.as_str()).collect();
                        note("connected now", names.join(", "));
                    }
                    _ => {}
                }
            }
            Err(e) => report("agent", Err(e.into())),
        },
        Err(_) => report(
            "agent",
            Err(anyhow::anyhow!(
                "not running (start `nexus-agent`, or enable the service; `--system` for the system service)"
            )),
        ),
    }

    // Screen capture needs a display; a headless server uses the virtual one.
    #[cfg(target_os = "linux")]
    {
        let has_display =
            std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some();
        let has_xvfb = std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("Xvfb").is_file()));
        match (has_display, has_xvfb) {
            (true, _) => note("display", "a desktop session is available".into()),
            (false, true) => note("display", "no desktop session: Xvfb is installed, so the agent can serve a virtual screen".into()),
            (false, false) => note("display", "no desktop session and no Xvfb: screen sharing is unavailable (terminal and files still work); install xvfb for a virtual screen".into()),
        }
    }

    Ok(if healthy {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
