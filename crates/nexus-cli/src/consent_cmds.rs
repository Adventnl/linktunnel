//! Attended access from the terminal: `nexus consent` answers "may this device use me?" prompts,
//! and `nexus trust` decides which paired devices are pre-authorised.

use std::process::ExitCode;
use std::time::Duration;

use anyhow::Result;
use clap::Subcommand;
use nexus_core::i18n::{tr, tr_args};
use nexus_proto::v1::{ConsentRequest, ipc_event::Body as Ev};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::AppContext;
use crate::agent_cmds::{connect, connect_with_events, resolve};

#[derive(Debug, Subcommand)]
pub enum TrustCommand {
    /// Let a paired device use this one without asking each time (servers, your own PCs).
    Unattended {
        /// Device name or ID.
        device: String,
    },
    /// Ask before this paired device uses this one.
    Attended {
        /// Device name or ID.
        device: String,
    },
}

pub(crate) async fn trust(ctx: &AppContext, cmd: TrustCommand) -> Result<ExitCode> {
    let (device, on) = match cmd {
        TrustCommand::Unattended { device } => (device, true),
        TrustCommand::Attended { device } => (device, false),
    };
    let client = connect(ctx).await?;
    let devices = client.list_devices().await?.devices;
    let dev = resolve(ctx.locale, &devices, &device)?;
    client.set_unattended(dev.device_id.to_vec(), on).await?;
    println!(
        "{}",
        tr_args(
            ctx.locale,
            if on {
                "cli.trust.unattended_on"
            } else {
                "cli.trust.unattended_off"
            },
            &[("name", &dev.name)]
        )
    );
    Ok(ExitCode::SUCCESS)
}

/// Stays connected and asks on the terminal whenever a device requests access.
pub(crate) async fn watch(ctx: &AppContext) -> Result<ExitCode> {
    let l = ctx.locale;
    let (client, mut events) = connect_with_events(ctx).await?;
    println!("{}", tr(l, "cli.consent.waiting"));
    let mut stdin = BufReader::new(tokio::io::stdin()).lines();
    while let Some(ev) = events.next().await {
        let Some(Ev::ConsentRequest(req)) = ev.body else {
            continue;
        };
        let answer = ask(ctx, &req, &mut stdin, &mut events).await;
        let (allow, remember) = match answer.as_deref() {
            Some("y" | "yes") => (true, false),
            Some("a" | "always") => (true, true),
            _ => (false, false),
        };
        match client
            .consent_decision(req.request_id, allow, remember)
            .await
        {
            Ok(()) => println!(
                "{}",
                if remember {
                    tr_args(l, "cli.consent.remembered", &[("name", &req.peer_name)])
                } else if allow {
                    tr(l, "cli.consent.allowed")
                } else {
                    tr(l, "cli.consent.denied")
                }
            ),
            Err(_) => println!("{}", tr(l, "cli.consent.expired")),
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Prints the prompt and waits for a line, or for the request to be resolved elsewhere.
async fn ask(
    ctx: &AppContext,
    req: &ConsentRequest,
    stdin: &mut tokio::io::Lines<BufReader<tokio::io::Stdin>>,
    events: &mut nexus_ipc::EventReceiver,
) -> Option<String> {
    let grants = if req.grants.is_empty() {
        "-".to_owned()
    } else {
        req.grants.join(", ")
    };
    let fingerprint: String = req.peer_fingerprint.chars().take(16).collect();
    print!(
        "{}",
        tr_args(
            ctx.locale,
            "cli.consent.prompt",
            &[
                ("name", &req.peer_name),
                ("fingerprint", &fingerprint),
                ("grants", &grants),
                ("secs", &req.timeout_secs.to_string()),
            ]
        )
    );
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    let deadline = Duration::from_secs(u64::from(req.timeout_secs));
    tokio::select! {
        line = stdin.next_line() => line.ok().flatten().map(|l| l.trim().to_lowercase()),
        () = tokio::time::sleep(deadline) => None,
        () = async {
            // Another client answered, or it timed out: stop waiting for typing.
            while let Some(ev) = events.next().await {
                if matches!(ev.body, Some(Ev::ConsentResolved(ref r)) if r.request_id == req.request_id) {
                    return;
                }
            }
        } => { println!(); None }
    }
}
