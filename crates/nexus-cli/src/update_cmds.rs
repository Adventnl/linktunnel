//! `nexus update`: signed updates.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::Subcommand;
use nexus_core::config::ConfigLoader;
use nexus_core::i18n::{tr, tr_args};
use nexus_update::{Decision, Device, PublisherKeys, apply, decide, fetch};
use semver::Version;

use crate::AppContext;

#[derive(Debug, Subcommand)]
pub enum UpdateCommand {
    /// Is a newer version available?
    Check,
    /// Download, verify and install the newer version.
    Apply {
        /// For single-file releases: the program to replace (default: `nexus-agent` next to
        /// this program).
        #[arg(long)]
        target: Option<PathBuf>,
        /// Do not run installers; only download and verify.
        #[arg(long)]
        download_only: bool,
    },
    /// Put back the version that was replaced by the last `apply`.
    Rollback {
        /// The program to restore.
        #[arg(long)]
        target: PathBuf,
    },
}

struct Checked {
    decision: Decision,
    current: Version,
}

async fn check(ctx: &AppContext) -> Result<Option<Checked>> {
    let cfg = ConfigLoader::new()
        .with_file(ctx.paths.config_file())
        .with_env()
        .load()?;
    let keys = PublisherKeys::from_hex(&cfg.update.public_keys)?;
    let Some(url) = cfg
        .update
        .manifest_url
        .as_ref()
        .filter(|_| !keys.is_empty())
    else {
        println!("{}", tr(ctx.locale, "cli.update.disabled"));
        return Ok(None);
    };
    let signed = fetch::fetch_manifest(url.as_str()).await?;
    let manifest = keys.verify(&signed)?;
    let current = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let device_id = nexus_crypto::load_identity(ctx.store.as_ref())?
        .map(|i| i.device_id().as_bytes().to_vec())
        .unwrap_or_default();
    let decision = decide(
        &manifest,
        &Device {
            current: &current,
            os: nexus_update::current_os(),
            arch: nexus_update::current_arch(),
            device_id: &device_id,
            now_ms: nexus_core::clock::unix_ms(),
            channel: &cfg.update.channel,
        },
    )?;
    Ok(Some(Checked { decision, current }))
}

pub(crate) async fn run(ctx: &AppContext, cmd: UpdateCommand) -> Result<ExitCode> {
    let l = ctx.locale;
    match cmd {
        UpdateCommand::Check => {
            let Some(c) = check(ctx).await? else {
                return Ok(ExitCode::FAILURE);
            };
            match &c.decision {
                Decision::UpToDate => println!(
                    "{}",
                    tr_args(
                        l,
                        "cli.update.uptodate",
                        &[("version", &c.current.to_string())]
                    )
                ),
                Decision::Update { version, notes, .. } => {
                    println!(
                        "{}",
                        tr_args(
                            l,
                            "cli.update.available",
                            &[
                                ("version", &version.to_string()),
                                ("current", &c.current.to_string())
                            ]
                        )
                    );
                    if !notes.is_empty() {
                        println!("{notes}");
                    }
                }
                Decision::NotYet {
                    version,
                    rollout_percent,
                } => println!(
                    "{}",
                    tr_args(
                        l,
                        "cli.update.not_yet",
                        &[
                            ("version", &version.to_string()),
                            ("percent", &rollout_percent.to_string())
                        ]
                    )
                ),
                Decision::NoArtifact(v) => println!(
                    "{}",
                    tr_args(l, "cli.update.no_artifact", &[("version", &v.to_string())])
                ),
            }
            Ok(ExitCode::SUCCESS)
        }
        UpdateCommand::Apply {
            target,
            download_only,
        } => {
            let Some(c) = check(ctx).await? else {
                return Ok(ExitCode::FAILURE);
            };
            let Decision::Update { artifact, .. } = c.decision else {
                println!(
                    "{}",
                    tr_args(
                        l,
                        "cli.update.uptodate",
                        &[("version", &c.current.to_string())]
                    )
                );
                return Ok(ExitCode::SUCCESS);
            };
            let staged = fetch::download(&artifact, &ctx.paths.data_dir.join("updates")).await?;
            println!(
                "{}",
                tr_args(
                    l,
                    "cli.update.downloaded",
                    &[("path", &staged.display().to_string())]
                )
            );
            if download_only {
                return Ok(ExitCode::SUCCESS);
            }
            if artifact.kind == "binary" {
                return replace_program(ctx, &staged, target);
            }
            let Some(mut command) = apply::installer_command(&artifact.os, &artifact.kind, &staged)
            else {
                bail!(
                    "don't know how to install a {} on {}",
                    artifact.kind,
                    artifact.os
                );
            };
            let shown = format!("{command:?}");
            println!(
                "{}",
                tr_args(l, "cli.update.installer", &[("command", &shown)])
            );
            match command.status() {
                Ok(s) if s.success() => Ok(ExitCode::SUCCESS),
                _ => {
                    println!(
                        "{}",
                        tr_args(l, "cli.update.manual", &[("command", &shown)])
                    );
                    Ok(ExitCode::FAILURE)
                }
            }
        }
        UpdateCommand::Rollback { target } => roll_back(ctx, &target),
    }
}

#[cfg(unix)]
fn replace_program(
    ctx: &AppContext,
    staged: &std::path::Path,
    target: Option<PathBuf>,
) -> Result<ExitCode> {
    use anyhow::Context as _;
    let target = match target {
        Some(t) => t,
        None => std::env::current_exe()?.with_file_name("nexus-agent"),
    };
    let old = apply::replace_binary(staged, &target)
        .with_context(|| format!("replacing {}", target.display()))?;
    println!(
        "{}",
        tr_args(
            ctx.locale,
            "cli.update.replaced",
            &[
                ("path", &target.display().to_string()),
                ("old", &old.display().to_string())
            ]
        )
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(not(unix))]
fn replace_program(
    _ctx: &AppContext,
    _staged: &std::path::Path,
    _target: Option<PathBuf>,
) -> Result<ExitCode> {
    bail!("single-file updates are not supported on this platform; use the installer")
}

#[cfg(unix)]
fn roll_back(ctx: &AppContext, target: &std::path::Path) -> Result<ExitCode> {
    apply::roll_back(target)?;
    println!(
        "{}",
        tr_args(
            ctx.locale,
            "cli.update.rolled_back",
            &[("path", &target.display().to_string())]
        )
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(not(unix))]
fn roll_back(_ctx: &AppContext, _target: &std::path::Path) -> Result<ExitCode> {
    bail!("rollback of single files is not supported on this platform")
}
