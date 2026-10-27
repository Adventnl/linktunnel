//! Local credential enrollment and approval of one exact remote connection.
use crate::{AppContext, agent_cmds};
use anyhow::{Context, Result, bail};
use clap::Subcommand;
use nexus_core::i18n::{Locale, tr};
use nexus_proto::v1::AuthRequest;
use std::process::ExitCode;
#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Show enrolled factors and the unattended step-up policy.
    Status,
    /// Add a passkey using the system browser (Touch ID / Windows Hello / security key).
    EnrollPasskey,
    /// Enroll an authenticator app; activate it by entering a generated code.
    EnrollTotp,
    /// Verify a factor for the next connection to DEVICE (expires after 60 seconds).
    Unlock {
        /// Paired device name or ID.
        device: String,
        /// Use an authenticator code instead of the primary passkey.
        #[arg(long)]
        totp: bool,
    },
}
pub async fn run(ctx: &AppContext, command: AuthCommand) -> Result<ExitCode> {
    let client = agent_cmds::connect(ctx).await?;
    let chinese = ctx.locale == Locale::ZhCn;
    match command {
        AuthCommand::Status => {
            let result = client
                .auth(AuthRequest {
                    operation: "status".into(),
                    ..Default::default()
                })
                .await?;
            println!("{}", result.json);
        }
        AuthCommand::EnrollTotp | AuthCommand::EnrollPasskey => {
            let passkey = matches!(command, AuthCommand::EnrollPasskey);
            let begin = client
                .auth(AuthRequest {
                    operation: if passkey {
                        "enroll-passkey"
                    } else {
                        "enroll-totp"
                    }
                    .into(),
                    ..Default::default()
                })
                .await?;
            let options: serde_json::Value = serde_json::from_str(&begin.json)?;
            let mut finish = AuthRequest {
                operation: "enroll-finish".into(),
                handle: begin.handle,
                ..Default::default()
            };
            if passkey {
                let prompt = nexus_auth::browser::BrowserPrompt::start(
                    options,
                    true,
                    "linktunnel",
                    chinese,
                )
                .await?;
                println!("{} {}", tr(ctx.locale, "cli.auth.browser"), prompt.url());
                let _ = prompt.open();
                finish.response_json = prompt.wait().await?;
            } else {
                println!(
                    "{}\n{}",
                    tr(ctx.locale, "cli.auth.enroll"),
                    options["uri"].as_str().context("missing enrollment URI")?
                );
                finish.totp = read_code(ctx.locale).await?;
            }
            client.auth(finish).await?;
            println!("{}", tr(ctx.locale, "cli.auth.enrolled"));
        }
        AuthCommand::Unlock { device, totp } => {
            let peer =
                agent_cmds::resolve(ctx.locale, &client.list_devices().await?.devices, &device)?;
            let begin = client
                .auth(AuthRequest {
                    operation: "remote-begin".into(),
                    device_id: peer.device_id.clone(),
                    ..Default::default()
                })
                .await?;
            let options: serde_json::Value = serde_json::from_str(&begin.json)?;
            if !begin.handle.is_empty() {
                let mut finish = AuthRequest {
                    operation: "remote-finish".into(),
                    handle: begin.handle,
                    ..Default::default()
                };
                if totp {
                    if options["totp"] != true {
                        bail!("TOTP is not enrolled on that device");
                    }
                    finish.totp = read_code(ctx.locale).await?;
                } else {
                    let key = options["public_key"].clone();
                    if key.is_null() {
                        bail!("no passkey is enrolled; use --totp or enroll one on the target");
                    }
                    let prompt =
                        nexus_auth::browser::BrowserPrompt::start(key, false, &peer.name, chinese)
                            .await?;
                    println!("{} {}", tr(ctx.locale, "cli.auth.browser"), prompt.url());
                    let _ = prompt.open();
                    finish.response_json = prompt.wait().await?;
                }
                client.auth(finish).await?;
            }
            println!("{}", tr(ctx.locale, "cli.auth.unlocked"));
        }
    }
    Ok(ExitCode::SUCCESS)
}
async fn read_code(locale: Locale) -> Result<String> {
    tokio::task::spawn_blocking(move || rpassword::prompt_password(tr(locale, "cli.auth.code")))
        .await?
        .map_err(Into::into)
}
