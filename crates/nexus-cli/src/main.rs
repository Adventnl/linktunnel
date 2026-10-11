//! `nexus`: command line interface for Nexus Remote.
//!
//! Runtime messages are localised (`--lang`, `NEXUS_LANG`, or the OS locale). Clap's own `--help`
//! text is English-only; that is a clap limitation, tracked in docs/USER_GUIDE.md.

// A CLI's job is to print.
#![allow(clippy::print_stdout, clippy::print_stderr)]

mod identity_cmd;

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
}

/// Everything a subcommand needs to know about its environment.
pub(crate) struct AppContext {
    pub locale: Locale,
    pub paths: Paths,
    pub store: Box<dyn SecretStore>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
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
