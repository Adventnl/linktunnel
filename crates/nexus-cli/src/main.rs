//! `nexus`: command line interface for Nexus Remote.
//!
//! Runtime messages are localised (`--lang`, `NEXUS_LANG`, or the OS locale). Clap's own `--help`
//! text is English-only; that is a clap limitation, tracked in docs/USER_GUIDE.md.

// A CLI's job is to print.
#![allow(clippy::print_stdout, clippy::print_stderr)]

mod agent_cmds;
mod consent_cmds;
mod files_cmds;
mod forward_cmds;
mod identity_cmd;
mod power_cmds;
mod remote_cmds;
mod shell_cmds;

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
    /// Manage this device's cryptographic identity.
    #[command(subcommand)]
    Id(identity_cmd::IdCommand),
    /// Check that this machine is ready to run Nexus Remote.
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
    /// Reach a service on a paired device through a local port (`nexus forward pc 5432`).
    Forward {
        /// Device name or ID.
        device: String,
        /// `[LOCAL_PORT:]HOST:PORT`, `LOCAL_PORT:PORT` or just `PORT` (HOST defaults to
        /// localhost as seen from the device).
        spec: String,
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
    /// Answer "may this device use me?" prompts in this terminal.
    Consent,
    /// Choose which paired devices may use this one without asking.
    #[command(subcommand)]
    Trust(consent_cmds::TrustCommand),
    /// Turn the emergency lockdown switch on or off.
    #[command(subcommand)]
    Lockdown(agent_cmds::LockdownCommand),
    /// Ask the running agent to exit.
    Stop,
}

/// Everything a subcommand needs to know about its environment.
pub(crate) struct AppContext {
    pub locale: Locale,
    pub paths: Paths,
    pub store: Box<dyn SecretStore>,
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
    let paths = match &cli.home {
        Some(home) => Paths::under(home),
        None => Paths::resolve(Scope::User).context("locating state directories")?,
    };
    let store = open_default(&DefaultStoreOptions {
        service: KEYCHAIN_SERVICE.into(),
        file_dir: paths.data_dir.join("keys"),
        prefer_os_store: !cli.file_store && cli.home.is_none(),
        allow_file_fallback: true,
    })
    .context("opening the secret store")?;

    let ctx = AppContext {
        locale,
        paths,
        store,
    };
    match cli.command {
        Command::Id(cmd) => identity_cmd::run(&ctx, &cmd),
        Command::Doctor => doctor(&ctx),
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
        Command::Forward { device, spec } => forward_cmds::forward(&ctx, &device, &spec).await,
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
        Command::Consent => consent_cmds::watch(&ctx).await,
        Command::Trust(cmd) => consent_cmds::trust(&ctx, cmd).await,
        Command::Lockdown(cmd) => agent_cmds::lockdown(&ctx, &cmd).await,
        Command::Stop => agent_cmds::stop(&ctx).await,
    }
}

fn doctor(ctx: &AppContext) -> Result<ExitCode> {
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
    Ok(if healthy {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
