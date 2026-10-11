//! `nexus power` and `nexus wake`.

use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::ValueEnum;
use nexus_core::i18n::tr_args;
use nexus_proto::v1::{OpenSession, PowerActionKind};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::AppContext;
use crate::agent_cmds::{connect, resolve};

/// What to do to the device.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Action {
    /// Lock the screen.
    Lock,
    /// Suspend to RAM.
    Sleep,
    /// Suspend to disk.
    Hibernate,
    /// Reboot.
    Restart,
    /// Power off.
    Shutdown,
    /// End the user session.
    Logoff,
}

impl Action {
    fn kind(self) -> PowerActionKind {
        match self {
            Self::Lock => PowerActionKind::Lock,
            Self::Sleep => PowerActionKind::Sleep,
            Self::Hibernate => PowerActionKind::Hibernate,
            Self::Restart => PowerActionKind::Restart,
            Self::Shutdown => PowerActionKind::Shutdown,
            Self::Logoff => PowerActionKind::LogOff,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Lock => "lock",
            Self::Sleep => "sleep",
            Self::Hibernate => "hibernate",
            Self::Restart => "restart",
            Self::Shutdown => "shut down",
            Self::Logoff => "log off",
        }
    }

    /// Everything but locking cuts the connection and may lose unsaved work.
    fn needs_confirmation(self) -> bool {
        !matches!(self, Self::Lock)
    }
}

pub(crate) async fn power(
    ctx: &AppContext,
    device: &str,
    action: Action,
    yes: bool,
) -> Result<ExitCode> {
    let l = ctx.locale;
    let client = connect(ctx).await?;
    let devices = client.list_devices().await?.devices;
    let dev = resolve(l, &devices, device)?;
    if action.needs_confirmation() && !yes {
        print!(
            "{}",
            tr_args(
                l,
                "cli.power.confirm",
                &[("action", action.label()), ("name", &dev.name)]
            )
        );
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
        let answer = BufReader::new(tokio::io::stdin())
            .lines()
            .next_line()
            .await?
            .unwrap_or_default();
        if answer.trim() != dev.name {
            println!("{}", nexus_core::i18n::tr(l, "cli.power.cancelled"));
            return Ok(ExitCode::FAILURE);
        }
    }
    let opened = client
        .open_session(OpenSession {
            device_id: dev.device_id.clone(),
            want_video: false,
            ..Default::default()
        })
        .await?;
    let result = client.power(opened.session_id, action.kind()).await;
    let _ = client.close_session(opened.session_id).await;
    result?;
    println!(
        "{}",
        tr_args(
            l,
            "cli.power.done",
            &[("action", action.label()), ("name", &dev.name)]
        )
    );
    Ok(ExitCode::SUCCESS)
}

pub(crate) async fn wake(ctx: &AppContext, device: &str, via: Option<&str>) -> Result<ExitCode> {
    let l = ctx.locale;
    let client = connect(ctx).await?;
    let devices = client.list_devices().await?.devices;
    let dev = resolve(l, &devices, device)?;
    let via_dev = match via {
        Some(v) => {
            let d = resolve(l, &devices, v)?;
            if d.device_id == dev.device_id {
                bail!("a device cannot wake itself");
            }
            Some(d)
        }
        None => None,
    };
    client
        .wake_device(
            dev.device_id.to_vec(),
            via_dev
                .as_ref()
                .map(|d| d.device_id.to_vec())
                .unwrap_or_default(),
        )
        .await?;
    println!(
        "{}",
        match via_dev {
            Some(v) => tr_args(
                l,
                "cli.wake.relayed",
                &[("via", &v.name), ("name", &dev.name)]
            ),
            None => tr_args(l, "cli.wake.sent", &[("name", &dev.name)]),
        }
    );
    Ok(ExitCode::SUCCESS)
}
